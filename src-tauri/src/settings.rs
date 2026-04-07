use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::state::Theme;

// ── Claude Code hook detection / installation ──────────────────────────

fn claude_settings_path() -> PathBuf {
    dirs::home_dir()
        .expect("no home directory")
        .join(".claude")
        .join("settings.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookEntryStatus {
    pub event: String,
    pub url: String,
    pub installed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookStatus {
    pub installed: bool,
    pub path: String,
    pub hooks: Vec<HookEntryStatus>,
}

fn expected_hook_urls(port: u16) -> Vec<(&'static str, String)> {
    let base = format!("http://localhost:{}", port);
    vec![
        ("PermissionRequest", format!("{}/hook/permission-request", base)),
        ("Stop", format!("{}/hook/stop", base)),
        ("PostToolUse", format!("{}/hook/post-tool-use", base)),
        ("UserPromptSubmit", format!("{}/hook/user-prompt-submit", base)),
    ]
}

fn has_http_hook(entries: &[serde_json::Value], url: &str) -> bool {
    entries.iter().any(|entry| is_coach_hook_entry(entry, url))
}

/// Returns true if this hook entry contains an HTTP hook matching the given URL.
fn is_coach_hook_entry(entry: &serde_json::Value, url: &str) -> bool {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .is_some_and(|hooks| {
            hooks.iter().any(|hook| {
                hook.get("type").and_then(|t| t.as_str()) == Some("http")
                    && hook.get("url").and_then(|u| u.as_str()) == Some(url)
            })
        })
}

pub fn check_hook_status(port: u16) -> HookStatus {
    check_hook_status_at(port, &claude_settings_path())
}

pub fn check_hook_status_at(port: u16, path: &std::path::Path) -> HookStatus {
    let expected = expected_hook_urls(port);

    let settings: serde_json::Value = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);

    let hooks_obj = settings.get("hooks");

    let entries: Vec<HookEntryStatus> = expected
        .iter()
        .map(|(event, url)| {
            let installed = hooks_obj
                .and_then(|h| h.get(*event))
                .and_then(|arr| arr.as_array())
                .is_some_and(|entries| has_http_hook(entries, url));

            HookEntryStatus {
                event: event.to_string(),
                url: url.clone(),
                installed,
            }
        })
        .collect();

    let all_installed = entries.iter().all(|e| e.installed);

    HookStatus {
        installed: all_installed,
        path: path.display().to_string(),
        hooks: entries,
    }
}

/// Merge coach hooks into ~/.claude/settings.json, preserving existing hooks.
pub fn install_hooks(port: u16) -> Result<(), String> {
    install_hooks_at(port, &claude_settings_path())
}

pub fn install_hooks_at(port: u16, path: &std::path::Path) -> Result<(), String> {
    let expected = expected_hook_urls(port);

    let mut settings: serde_json::Value = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::json!({}));

    let settings_obj = settings
        .as_object_mut()
        .ok_or("Claude settings is not a JSON object")?;

    if !settings_obj.contains_key("hooks") {
        settings_obj.insert("hooks".into(), serde_json::json!({}));
    }

    let hooks_obj = settings_obj
        .get_mut("hooks")
        .and_then(|v| v.as_object_mut())
        .ok_or("hooks is not an object")?;

    for (event, url) in &expected {
        let hook_entry = serde_json::json!({
            "hooks": [{"type": "http", "url": url}]
        });

        if let Some(existing) = hooks_obj.get_mut(*event) {
            if let Some(arr) = existing.as_array_mut() {
                if !has_http_hook(arr, url) {
                    arr.push(hook_entry);
                }
            }
        } else {
            hooks_obj.insert(event.to_string(), serde_json::json!([hook_entry]));
        }
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())?;

    Ok(())
}

/// Remove coach hooks from ~/.claude/settings.json, preserving everything else.
pub fn uninstall_hooks(port: u16) -> Result<(), String> {
    uninstall_hooks_at(port, &claude_settings_path())
}

pub fn uninstall_hooks_at(port: u16, path: &std::path::Path) -> Result<(), String> {
    let expected = expected_hook_urls(port);

    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut settings: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| e.to_string())?;

    let hooks_obj = settings
        .get_mut("hooks")
        .and_then(|v| v.as_object_mut())
        .ok_or("No hooks object in settings")?;

    for (event, url) in &expected {
        if let Some(arr) = hooks_obj.get_mut(*event).and_then(|v| v.as_array_mut()) {
            arr.retain(|entry| !is_coach_hook_entry(entry, url));
        }
    }

    // Clean up empty event arrays
    let empty_events: Vec<String> = hooks_obj
        .iter()
        .filter(|(_, v)| v.as_array().is_some_and(|a| a.is_empty()))
        .map(|(k, _)| k.clone())
        .collect();
    for key in empty_events {
        hooks_obj.remove(&key);
    }

    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())?;

    Ok(())
}

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
}

fn default_model() -> ModelConfig {
    ModelConfig {
        provider: "openai".into(),
        model: "gpt-5.4-mini".into(),
    }
}

/// Providers that support stateful coach sessions (response-id chaining
/// or equivalent server-side conversation state). Currently only OpenAI's
/// Responses API in rig 0.34. Other providers can still serve the rules
/// engine and one-shot stop evaluation.
pub const OBSERVER_CAPABLE_PROVIDERS: &[&str] = &["openai"];

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
        }
    }
}

fn settings_path() -> PathBuf {
    dirs::home_dir()
        .expect("no home directory")
        .join(".coach")
        .join("settings.json")
}

impl Settings {
    pub fn load() -> Self {
        let path = settings_path();
        match std::fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|e| {
                eprintln!("Warning: failed to parse {}: {}", path.display(), e);
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let path = settings_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
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
    }

    // ── has_http_hook helper ────────────────────────────────────────────

    /// has_http_hook should find a matching entry when the URL and type
    /// are present in the nested hooks array.
    #[test]
    fn has_http_hook_finds_matching_entry() {
        let entries: Vec<serde_json::Value> = vec![serde_json::json!({
            "hooks": [{"type": "http", "url": "http://localhost:7700/hook/stop"}]
        })];
        assert!(has_http_hook(&entries, "http://localhost:7700/hook/stop"));
    }

    /// has_http_hook should return false when the URL doesn't match,
    /// even if the structure is correct.
    #[test]
    fn has_http_hook_rejects_non_matching_url() {
        let entries: Vec<serde_json::Value> = vec![serde_json::json!({
            "hooks": [{"type": "http", "url": "http://localhost:7700/hook/stop"}]
        })];
        assert!(!has_http_hook(&entries, "http://localhost:9999/hook/stop"));
    }

    /// has_http_hook on an empty array should return false.
    #[test]
    fn has_http_hook_returns_false_for_empty_array() {
        let entries: Vec<serde_json::Value> = vec![];
        assert!(!has_http_hook(&entries, "http://localhost:7700/hook/stop"));
    }

    /// has_http_hook should ignore entries with a non-http type.
    #[test]
    fn has_http_hook_ignores_non_http_types() {
        let entries: Vec<serde_json::Value> = vec![serde_json::json!({
            "hooks": [{"type": "command", "url": "http://localhost:7700/hook/stop"}]
        })];
        assert!(!has_http_hook(&entries, "http://localhost:7700/hook/stop"));
    }

    // ── check_hook_status with path ─────────────────────────────────────

    /// When the settings file doesn't exist, all hooks should report
    /// as not installed.
    #[test]
    fn check_hook_status_reports_not_installed_for_missing_file() {
        let status = check_hook_status_at(7700, std::path::Path::new("/nonexistent/path.json"));
        assert!(!status.installed);
        assert!(status.hooks.iter().all(|h| !h.installed));
        assert_eq!(status.hooks.len(), 4); // PermissionRequest, Stop, PostToolUse, UserPromptSubmit
    }

    /// After install_hooks_at writes hooks to a temp file, check_hook_status_at
    /// should report all hooks as installed. This tests the install/check roundtrip.
    #[test]
    fn install_then_check_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        install_hooks_at(7700, &path).unwrap();
        let status = check_hook_status_at(7700, &path);

        assert!(status.installed);
        assert!(status.hooks.iter().all(|h| h.installed));
    }

    /// Installing hooks twice should be idempotent — the second call
    /// should not duplicate hook entries.
    #[test]
    fn install_hooks_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        install_hooks_at(7700, &path).unwrap();
        install_hooks_at(7700, &path).unwrap();

        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        // Each event should have exactly one entry.
        for event in ["PermissionRequest", "Stop", "PostToolUse", "UserPromptSubmit"] {
            let arr = content["hooks"][event].as_array().unwrap();
            assert_eq!(arr.len(), 1, "event {event} should have exactly 1 entry after double install");
        }
    }

    /// Installing hooks should preserve existing content in the settings file.
    #[test]
    fn install_hooks_preserves_existing_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        // Write a settings file with some existing config.
        let existing = serde_json::json!({
            "permissions": {"allow": ["Bash"]},
            "hooks": {
                "Stop": [{"hooks": [{"type": "command", "command": "echo stopped"}]}]
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&existing).unwrap()).unwrap();

        install_hooks_at(7700, &path).unwrap();

        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        // Existing permissions should still be there.
        assert_eq!(content["permissions"]["allow"][0], "Bash");
        // Existing command hook should still be there alongside the new http hook.
        let stop_entries = content["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop_entries.len(), 2, "existing command hook + new http hook");
    }

    // ── uninstall_hooks ────────────────────────────────────────────────

    /// install then uninstall should leave no coach hooks, and
    /// check_hook_status should report not installed.
    #[test]
    fn uninstall_reverses_install() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        install_hooks_at(7700, &path).unwrap();
        uninstall_hooks_at(7700, &path).unwrap();

        let status = check_hook_status_at(7700, &path);
        assert!(!status.installed);
        assert!(status.hooks.iter().all(|h| !h.installed));
    }

    /// Uninstall should preserve other hooks and settings.
    #[test]
    fn uninstall_preserves_other_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let existing = serde_json::json!({
            "permissions": {"allow": ["Bash"]},
            "hooks": {
                "Stop": [{"hooks": [{"type": "command", "command": "echo stopped"}]}]
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&existing).unwrap()).unwrap();

        install_hooks_at(7700, &path).unwrap();
        uninstall_hooks_at(7700, &path).unwrap();

        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        // Permissions untouched.
        assert_eq!(content["permissions"]["allow"][0], "Bash");
        // The user's command hook on Stop should survive.
        let stop_entries = content["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop_entries.len(), 1);
        assert_eq!(stop_entries[0]["hooks"][0]["type"], "command");
    }

    /// Uninstall cleans up empty event arrays (coach-only events get removed).
    #[test]
    fn uninstall_removes_empty_event_arrays() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        install_hooks_at(7700, &path).unwrap();
        uninstall_hooks_at(7700, &path).unwrap();

        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        let hooks = content["hooks"].as_object().unwrap();
        assert!(hooks.is_empty(), "all coach-only events should be cleaned up");
    }
}
