use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::state::Theme;

mod hooks;
pub use hooks::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum EngineMode {
    /// Rule-based: pattern-matching only, no LLM calls.
    Rules,
    /// LLM-powered: sends context to the configured model for evaluation.
    Llm,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CoachRule {
    pub id: String,
    pub enabled: bool,
}

/// Env var name for each provider.
/// Env var names per provider. Multiple names are checked in order (first match wins).
pub const PROVIDER_ENV_VARS: &[(&str, &[&str])] = &[
    ("google", &["GEMINI_API_KEY", "GOOGLE_API_KEY"]),
    ("anthropic", &["ANTHROPIC_API_KEY"]),
    ("openai", &["OPENAI_API_KEY"]),
    ("openrouter", &["OPENROUTER_API_KEY"]),
];

/// Read all available API tokens from environment variables.
pub fn env_tokens() -> HashMap<String, String> {
    PROVIDER_ENV_VARS
        .iter()
        .filter_map(|(provider, vars)| {
            vars.iter()
                .find_map(|var| std::env::var(var).ok().filter(|v| !v.is_empty()))
                .map(|v| (provider.to_string(), v))
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub api_tokens: HashMap<String, String>,
    #[serde(default = "default_model")]
    pub model: ModelConfig,
    #[serde(default = "default_priorities")]
    pub priorities: Vec<String>,
    #[serde(default = "default_theme")]
    pub theme: Theme,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_coach_mode")]
    pub coach_mode: EngineMode,
    #[serde(default = "default_rules")]
    pub rules: Vec<CoachRule>,
    /// On clean exit, uninstall Coach's hooks from `~/.claude/settings.json`
    /// and `~/.cursor/hooks.json` so that other live Claude/Cursor sessions
    /// don't fail with "HTTP undefined" when they try to call the now-stopped
    /// hook server. Default true; users can opt out if they'd rather see
    /// the failures as a signal that Coach isn't running. Re-installation
    /// happens automatically on next startup based on `hooks_user_enabled` /
    /// `cursor_hooks_user_enabled`.
    #[serde(default = "default_true")]
    pub auto_uninstall_hooks_on_exit: bool,
    /// Persistent record of the user's intent to use Claude Code hooks. Set
    /// when the user clicks Install, cleared when they click Uninstall. Survives
    /// auto-cleanup-on-exit so the next startup knows to reinstall. Migrated
    /// to `true` on first run if hooks are already on disk.
    #[serde(default)]
    pub hooks_user_enabled: bool,
    /// Same idea, for Cursor Agent hooks.
    #[serde(default)]
    pub cursor_hooks_user_enabled: bool,
}

fn default_model() -> ModelConfig {
    ModelConfig {
        provider: "openai".into(),
        model: "gpt-5.4-mini".into(),
    }
}

/// Providers that support stateful coach sessions via `session_send`.
/// Three mechanisms, in decreasing cost efficiency:
///   * OpenAI: server-side state via Responses API + previous_response_id
///     (native, O(1) per call).
///   * Anthropic: client-side message history with prompt caching
///     (emulated, ~10% of full input cost on the cached prefix).
///   * Google Gemini: client-side message history, no usable prefix
///     cache — full input charged every call (emulated, O(N) per call).
///     Pair with a cheap Flash model to keep observer cost tolerable.
/// Other providers (openrouter, ...) can still serve the rules engine
/// and one-shot stop evaluation; they just can't accumulate observer
/// context at all.
pub const OBSERVER_CAPABLE_PROVIDERS: &[&str] = &["openai", "anthropic", "google"];

fn default_priorities() -> Vec<String> {
    vec![
        "Code simplicity".into(),
        "Performance".into(),
        "Feature completeness".into(),
    ]
}

fn default_theme() -> Theme {
    Theme::System
}

fn default_port() -> u16 {
    7700
}

fn default_coach_mode() -> EngineMode {
    EngineMode::Rules
}

fn default_rules() -> Vec<CoachRule> {
    vec![CoachRule {
        id: "outdated_models".into(),
        enabled: true,
    }]
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            api_tokens: HashMap::new(),
            model: default_model(),
            priorities: default_priorities(),
            theme: default_theme(),
            port: default_port(),
            coach_mode: default_coach_mode(),
            rules: default_rules(),
            auto_uninstall_hooks_on_exit: default_true(),
            hooks_user_enabled: false,
            cursor_hooks_user_enabled: false,
        }
    }
}

/// Default location of `~/.coach/settings.json`. Exposed so the CLI
/// can show users where it's reading from / writing to.
pub fn settings_path() -> PathBuf {
    dirs::home_dir()
        .expect("no home directory")
        .join(".coach")
        .join("settings.json")
}

impl Settings {
    pub fn load() -> Self {
        Self::load_from(&settings_path())
    }

    /// Path-injectable load. Used by `Settings::load` in production and
    /// directly by the CLI's `--config-file` override and unit tests.
    pub fn load_from(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|e| {
                eprintln!("Warning: failed to parse {}: {}", path.display(), e);
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        self.save_to(&settings_path());
    }

    /// Path-injectable save. Errors here are eprintln!'d rather than
    /// returned so the GUI's hot path stays infallible — the CLI uses
    /// the same code path and gets the same warning on stderr.
    pub fn save_to(&self, path: &std::path::Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = std::fs::write(path, json) {
                    eprintln!("Warning: failed to write {}: {}", path.display(), e);
                }
            }
            Err(e) => eprintln!("Warning: failed to serialize settings: {}", e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Default values ──────────────────────────────────────────────────

    /// Default model should be OpenAI gpt-5.4-mini — observer requires
    /// OpenAI's Responses API for stateful coach sessions.
    #[test]
    fn default_model_is_openai_mini() {
        let s = Settings::default();
        assert_eq!(s.model.provider, "openai");
        assert_eq!(s.model.model, "gpt-5.4-mini");
    }

    /// OpenAI must be in the observer-capable list (it's the one provider
    /// rig 0.34 lets us chain via previous_response_id).
    #[test]
    fn openai_is_observer_capable() {
        assert!(OBSERVER_CAPABLE_PROVIDERS.contains(&"openai"));
    }

    /// Anthropic is observer-capable via client-side history + prompt
    /// caching. rig 0.34 exposes `with_automatic_caching()` first-class.
    #[test]
    fn anthropic_is_observer_capable() {
        assert!(OBSERVER_CAPABLE_PROVIDERS.contains(&"anthropic"));
    }

    /// Google is observer-capable via the emulated path: client-side
    /// history, no prefix caching. Cost scales with conversation length,
    /// so users should pair it with a cheap Flash model.
    #[test]
    fn google_is_observer_capable() {
        assert!(OBSERVER_CAPABLE_PROVIDERS.contains(&"google"));
    }

    /// openrouter remains unsupported: it's a chat-completions proxy
    /// with no session primitive and no caching primitive we can drive.
    #[test]
    fn openrouter_is_not_observer_capable() {
        assert!(!OBSERVER_CAPABLE_PROVIDERS.contains(&"openrouter"));
    }

    /// Priorities should ship with sensible non-empty defaults so the
    /// coach has something to say on first launch.
    #[test]
    fn default_priorities_are_non_empty() {
        let s = Settings::default();
        assert!(!s.priorities.is_empty());
    }

    /// Default port should be 7700, matching the hardcoded hook URLs
    /// and frontend expectations.
    #[test]
    fn default_port_is_7700() {
        let s = Settings::default();
        assert_eq!(s.port, 7700);
    }

    /// Default theme should be System so the app matches OS appearance.
    #[test]
    fn default_theme_is_system() {
        let s = Settings::default();
        assert_eq!(s.theme, Theme::System);
    }

    // ── Serde roundtrip ─────────────────────────────────────────────────

    /// Serializing Settings to JSON and deserializing back should
    /// preserve all fields exactly. This guards against accidentally
    /// breaking persistence when adding new fields.
    #[test]
    fn settings_serde_roundtrip_preserves_all_fields() {
        let original = Settings {
            api_tokens: HashMap::from([
                ("google".into(), "gk-123".into()),
                ("openai".into(), "sk-abc".into()),
            ]),
            model: ModelConfig {
                provider: "anthropic".into(),
                model: "claude-sonnet-4-20250514".into(),
            },
            priorities: vec!["Speed".into(), "Safety".into()],
            theme: Theme::Dark,
            port: 9999,
            coach_mode: EngineMode::Llm,
            rules: vec![
                CoachRule { id: "outdated_models".into(), enabled: false },
            ],
            auto_uninstall_hooks_on_exit: false,
            hooks_user_enabled: true,
            cursor_hooks_user_enabled: true,
        };

        let json = serde_json::to_string(&original).unwrap();
        let restored: Settings = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.api_tokens, original.api_tokens);
        assert_eq!(restored.model.provider, original.model.provider);
        assert_eq!(restored.model.model, original.model.model);
        assert_eq!(restored.priorities, original.priorities);
        assert_eq!(restored.theme, original.theme);
        assert_eq!(restored.port, original.port);
        assert_eq!(restored.coach_mode, original.coach_mode);
        assert_eq!(restored.rules, original.rules);
        assert_eq!(
            restored.auto_uninstall_hooks_on_exit,
            original.auto_uninstall_hooks_on_exit
        );
        assert_eq!(restored.hooks_user_enabled, original.hooks_user_enabled);
        assert_eq!(
            restored.cursor_hooks_user_enabled,
            original.cursor_hooks_user_enabled
        );
    }

    /// Deserializing an empty JSON object `{}` should produce the same
    /// defaults as `Settings::default()`. This ensures serde(default)
    /// attributes are correctly set on every field.
    #[test]
    fn empty_json_deserializes_to_defaults() {
        let from_json: Settings = serde_json::from_str("{}").unwrap();
        let defaults = Settings::default();

        assert_eq!(from_json.model.provider, defaults.model.provider);
        assert_eq!(from_json.model.model, defaults.model.model);
        assert_eq!(from_json.priorities, defaults.priorities);
        assert_eq!(from_json.theme, defaults.theme);
        assert_eq!(from_json.port, defaults.port);
        assert!(from_json.api_tokens.is_empty());
        // New hook-cleanup fields default sensibly: opt-in to cleanup,
        // opt-out of any auto-install. Users upgrading from older Coach
        // versions get these defaults silently.
        assert!(from_json.auto_uninstall_hooks_on_exit);
        assert!(!from_json.hooks_user_enabled);
        assert!(!from_json.cursor_hooks_user_enabled);
    }
}
