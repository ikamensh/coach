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
        // NOTE: SessionStart used to be in this list — fast `/clear`
        // detection. Removed because Claude Code 2.1.92+ silently drops
        // HTTP hooks for SessionStart (its debug log says
        // "HTTP hooks are not supported for SessionStart"). Installing
        // it produced a misleading ✓ in `coach hooks status` for a hook
        // that would never fire. `/clear` is now detected lazily on the
        // next tool call via the session_id-mismatch path in
        // `state::apply_hook_event`. The HTTP route + handler in
        // server.rs are still wired up because Cursor honours its own
        // `sessionStart` hook — see `cursor_hook_events`.
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

/// Top up Coach's managed hooks if at least one is already installed —
/// i.e. the user has previously opted in, and Coach has since gained a
/// new managed hook (like SessionStart) that needs to be added.
///
/// Returns the names of any hooks that were newly added so the caller
/// can log them.
///
/// **Does nothing** if no Coach hooks are installed yet — first-time
/// install stays explicit, gated on the user clicking "Install Hooks".
pub fn topup_managed_hooks(port: u16) -> Result<Vec<String>, String> {
    topup_managed_hooks_at(port, &claude_settings_path())
}

pub fn topup_managed_hooks_at(
    port: u16,
    path: &std::path::Path,
) -> Result<Vec<String>, String> {
    let status = check_hook_status_at(port, path);
    let any_installed = status.hooks.iter().any(|h| h.installed);
    if !any_installed {
        return Ok(vec![]);
    }
    let missing: Vec<String> = status
        .hooks
        .iter()
        .filter(|h| !h.installed)
        .map(|h| h.event.clone())
        .collect();
    if missing.is_empty() {
        return Ok(vec![]);
    }
    install_hooks_at(port, path)?;
    Ok(missing)
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

// ── Cursor Agent hooks (`~/.cursor/hooks.json`, `command` + stdin JSON) ──
//
// Cursor's hook runner silently drops any hook command that mentions `curl`
// directly (verified against cursor-agent 2.4.28 — even prefixing with an
// unrelated `touch` doesn't slip past). We work around it by installing a
// shim script (`coach-cursor-hook.sh`, sibling of hooks.json) and pointing
// every hook entry at it: the shim is what calls curl. Hook command entries
// become `<shim path> <event-slug>`.

fn cursor_hooks_path() -> PathBuf {
    dirs::home_dir()
        .expect("no home directory")
        .join(".cursor")
        .join("hooks.json")
}

/// Shim script Coach drops next to hooks.json so hook entries can avoid
/// mentioning `curl` directly (Cursor's hook runner blocks that).
pub fn cursor_shim_path() -> PathBuf {
    dirs::home_dir()
        .expect("no home directory")
        .join(".cursor")
        .join("coach-cursor-hook.sh")
}

const CURSOR_SHIM_FILENAME: &str = "coach-cursor-hook.sh";

/// (cursor event name, URL slug under `/cursor/hook/`).
const CURSOR_HOOK_EVENTS: &[(&str, &str)] = &[
    ("sessionStart", "session-start"),
    ("beforeSubmitPrompt", "before-submit-prompt"),
    ("beforeShellExecution", "before-shell"),
    ("beforeMCPExecution", "before-mcp"),
    ("afterShellExecution", "after-shell"),
    ("afterMCPExecution", "after-mcp"),
    ("afterFileEdit", "after-file-edit"),
    ("stop", "stop"),
];

/// Body of the shim script for the given Coach port. `--data-binary @-`
/// forwards stdin verbatim and `exec` makes curl's exit code the shim's
/// (cursor treats non-zero as a hook error).
pub fn cursor_shim_script(port: u16) -> String {
    format!(
        "#!/bin/sh\n\
         # Auto-generated by Coach.\n\
         exec curl -sS -X POST \"http://127.0.0.1:{port}/cursor/hook/$1\" \\\n\
              -H \"Content-Type: application/json\" \\\n\
              --data-binary @-\n",
    )
}

pub fn expected_cursor_hook_commands(shim_path: &std::path::Path) -> Vec<(&'static str, String)> {
    let shim = shim_path.display().to_string();
    CURSOR_HOOK_EVENTS
        .iter()
        .map(|(event, slug)| (*event, format!("{shim} {slug}")))
        .collect()
}

fn cursor_command_matches(entry: &serde_json::Value, expected: &str) -> bool {
    entry
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(|cmd| cmd == expected)
}

/// A hook entry is "managed by Coach" if its command references our shim
/// script by filename, OR if it's a legacy `curl ... 127.0.0.1:.../cursor/hook/...`
/// entry from a pre-shim install. The legacy match requires both the route
/// path AND a localhost host so an unrelated user hook with `/cursor/hook/`
/// in some other context can't be swept up by reinstall.
fn is_managed_cursor_command(cmd: &str) -> bool {
    if cmd.contains(CURSOR_SHIM_FILENAME) {
        return true;
    }
    cmd.contains("/cursor/hook/")
        && (cmd.contains("127.0.0.1:") || cmd.contains("localhost:"))
}

pub fn check_cursor_hook_status() -> HookStatus {
    check_cursor_hook_status_at(&cursor_hooks_path(), &cursor_shim_path())
}

pub fn check_cursor_hook_status_at(
    hooks_path: &std::path::Path,
    shim_path: &std::path::Path,
) -> HookStatus {
    let expected = expected_cursor_hook_commands(shim_path);

    let settings: serde_json::Value = std::fs::read_to_string(hooks_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);

    let hooks_obj = settings.get("hooks");

    let entries: Vec<HookEntryStatus> = expected
        .iter()
        .map(|(event, cmd)| {
            let installed = hooks_obj
                .and_then(|h| h.get(*event))
                .and_then(|arr| arr.as_array())
                .is_some_and(|entries| entries.iter().any(|e| cursor_command_matches(e, cmd)));

            HookEntryStatus {
                event: event.to_string(),
                url: cmd.clone(),
                installed,
            }
        })
        .collect();

    let all_installed = entries.iter().all(|e| e.installed);

    HookStatus {
        installed: all_installed,
        path: hooks_path.display().to_string(),
        hooks: entries,
    }
}

pub fn install_cursor_hooks(port: u16) -> Result<(), String> {
    install_cursor_hooks_at(port, &cursor_hooks_path(), &cursor_shim_path())
}

pub fn install_cursor_hooks_at(
    port: u16,
    hooks_path: &std::path::Path,
    shim_path: &std::path::Path,
) -> Result<(), String> {
    // 1. Write the shim script first so the executable referenced by
    //    hooks.json is on disk by the time the JSON is written.
    if let Some(parent) = shim_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(shim_path, cursor_shim_script(port)).map_err(|e| e.to_string())?;
    crate::path_install::make_executable(shim_path)?;

    // 2. Merge our hook entries into hooks.json, preserving any existing
    //    user entries (e.g. gleaner-cursor-upload).
    let expected = expected_cursor_hook_commands(shim_path);

    let mut settings: serde_json::Value = std::fs::read_to_string(hooks_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({ "version": 1, "hooks": {} }));

    let settings_obj = settings
        .as_object_mut()
        .ok_or("Cursor hooks.json must be a JSON object")?;

    if !settings_obj.contains_key("version") {
        settings_obj.insert("version".into(), serde_json::json!(1));
    }
    if !settings_obj.contains_key("hooks") {
        settings_obj.insert("hooks".into(), serde_json::json!({}));
    }

    let hooks_obj = settings_obj
        .get_mut("hooks")
        .and_then(|v| v.as_object_mut())
        .ok_or("hooks must be an object")?;

    for (event, cmd) in &expected {
        // Drop any stale Coach-managed entry first (different shim path
        // from a previous install) so we don't accumulate dead commands.
        if let Some(existing) = hooks_obj.get_mut(*event) {
            if let Some(arr) = existing.as_array_mut() {
                arr.retain(|entry| {
                    entry
                        .get("command")
                        .and_then(|c| c.as_str())
                        .map(|c| !is_managed_cursor_command(c))
                        .unwrap_or(true)
                });
            }
        }
        let entry = serde_json::json!({ "command": cmd });
        if let Some(existing) = hooks_obj.get_mut(*event) {
            if let Some(arr) = existing.as_array_mut() {
                if !arr.iter().any(|e| cursor_command_matches(e, cmd)) {
                    arr.push(entry);
                }
            }
        } else {
            hooks_obj.insert((*event).to_string(), serde_json::json!([entry]));
        }
    }

    if let Some(parent) = hooks_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(hooks_path, json).map_err(|e| e.to_string())?;

    Ok(())
}

pub fn uninstall_cursor_hooks() -> Result<(), String> {
    uninstall_cursor_hooks_at(&cursor_hooks_path(), &cursor_shim_path())
}

pub fn uninstall_cursor_hooks_at(
    hooks_path: &std::path::Path,
    shim_path: &std::path::Path,
) -> Result<(), String> {
    let content = std::fs::read_to_string(hooks_path).map_err(|e| e.to_string())?;
    let mut settings: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| e.to_string())?;

    let hooks_obj = settings
        .get_mut("hooks")
        .and_then(|v| v.as_object_mut())
        .ok_or("No hooks object in Cursor hooks.json")?;

    for (_event, arr_val) in hooks_obj.iter_mut() {
        if let Some(arr) = arr_val.as_array_mut() {
            arr.retain(|entry| {
                entry
                    .get("command")
                    .and_then(|c| c.as_str())
                    .map(|cmd| !is_managed_cursor_command(cmd))
                    .unwrap_or(true)
            });
        }
    }

    let empty_events: Vec<String> = hooks_obj
        .iter()
        .filter(|(_, v)| v.as_array().is_some_and(|a| a.is_empty()))
        .map(|(k, _)| k.clone())
        .collect();
    for key in empty_events {
        hooks_obj.remove(&key);
    }

    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(hooks_path, json).map_err(|e| e.to_string())?;

    // Best-effort: remove the shim script. If it's already gone (or
    // shared with something else), don't error.
    let _ = std::fs::remove_file(shim_path);

    Ok(())
}

pub fn topup_managed_cursor_hooks(port: u16) -> Result<Vec<String>, String> {
    topup_managed_cursor_hooks_at(port, &cursor_hooks_path(), &cursor_shim_path())
}

pub fn topup_managed_cursor_hooks_at(
    port: u16,
    hooks_path: &std::path::Path,
    shim_path: &std::path::Path,
) -> Result<Vec<String>, String> {
    let status = check_cursor_hook_status_at(hooks_path, shim_path);
    let any_installed = status.hooks.iter().any(|h| h.installed);
    if !any_installed {
        return Ok(vec![]);
    }
    let missing: Vec<String> = status
        .hooks
        .iter()
        .filter(|h| !h.installed)
        .map(|h| h.event.clone())
        .collect();
    if missing.is_empty() {
        return Ok(vec![]);
    }
    install_cursor_hooks_at(port, hooks_path, shim_path)?;
    Ok(missing)
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

/// Providers that support stateful coach sessions via `session_send`.
/// Three mechanisms, in decreasing cost efficiency:
///   • OpenAI: server-side state via Responses API + previous_response_id
///     (native, O(1) per call).
///   • Anthropic: client-side message history with prompt caching
///     (emulated, ~10% of full input cost on the cached prefix).
///   • Google Gemini: client-side message history, no usable prefix
///     cache — full input charged every call (emulated, O(N) per call).
///     Pair with a cheap Flash model to keep observer cost tolerable.
/// Other providers (openrouter, …) can still serve the rules engine
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
        // PermissionRequest, Stop, PostToolUse, UserPromptSubmit
        // (SessionStart was removed — see comment on expected_hook_urls)
        assert_eq!(status.hooks.len(), 4);
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

    // ── topup_managed_hooks ─────────────────────────────────────────────

    /// On a clean settings file with no Coach hooks, top-up does nothing —
    /// first install must be explicit.
    #[test]
    fn topup_does_nothing_when_no_hooks_installed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{}").unwrap();

        let added = topup_managed_hooks_at(7700, &path).unwrap();
        assert!(added.is_empty());

        let status = check_hook_status_at(7700, &path);
        assert!(!status.installed);
        assert!(status.hooks.iter().all(|h| !h.installed));
    }

    /// When Coach hooks are partially installed (e.g. user upgraded and
    /// Coach gained a new managed hook), top-up adds only the missing
    /// ones and reports them.
    #[test]
    fn topup_fills_in_missing_hooks_when_some_already_installed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        // Pre-install one Coach hook (simulating an older Coach version
        // that only managed PostToolUse).
        let partial = serde_json::json!({
            "hooks": {
                "PostToolUse": [{
                    "hooks": [{"type": "http", "url": "http://localhost:7700/hook/post-tool-use"}]
                }]
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&partial).unwrap()).unwrap();

        let added = topup_managed_hooks_at(7700, &path).unwrap();

        // The other three managed hooks should be reported as added.
        // (Was four including SessionStart — see expected_hook_urls.)
        let mut sorted = added.clone();
        sorted.sort();
        assert_eq!(
            sorted,
            vec![
                "PermissionRequest".to_string(),
                "Stop".to_string(),
                "UserPromptSubmit".to_string(),
            ]
        );

        let status = check_hook_status_at(7700, &path);
        assert!(status.installed, "all four managed hooks should now be installed");
    }

    /// Idempotent: running top-up again right after returns no additions.
    #[test]
    fn topup_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        install_hooks_at(7700, &path).unwrap();
        let added = topup_managed_hooks_at(7700, &path).unwrap();
        assert!(added.is_empty(), "fully-installed state requires no top-up");
    }

    /// User's pre-existing non-Coach hooks must survive a top-up.
    #[test]
    fn topup_preserves_non_coach_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        // Pre-install one Coach hook + one user command hook.
        let mixed = serde_json::json!({
            "hooks": {
                "Stop": [
                    {"hooks": [{"type": "command", "command": "echo user-hook"}]},
                    {"hooks": [{"type": "http", "url": "http://localhost:7700/hook/stop"}]}
                ]
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&mixed).unwrap()).unwrap();

        topup_managed_hooks_at(7700, &path).unwrap();

        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let stop_entries = content["hooks"]["Stop"].as_array().unwrap();
        // Both the user's command hook and Coach's http hook should still be present.
        assert_eq!(stop_entries.len(), 2);
        let kinds: Vec<&str> = stop_entries
            .iter()
            .map(|e| e["hooks"][0]["type"].as_str().unwrap())
            .collect();
        assert!(kinds.contains(&"command"));
        assert!(kinds.contains(&"http"));
    }
}
