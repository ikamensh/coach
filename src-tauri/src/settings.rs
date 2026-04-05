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
    ]
}

fn has_http_hook(entries: &[serde_json::Value], url: &str) -> bool {
    entries.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .is_some_and(|hooks| {
                hooks.iter().any(|hook| {
                    hook.get("type").and_then(|t| t.as_str()) == Some("http")
                        && hook.get("url").and_then(|u| u.as_str()) == Some(url)
                })
            })
    })
}

pub fn check_hook_status(port: u16) -> HookStatus {
    let path = claude_settings_path();
    let expected = expected_hook_urls(port);

    let settings: serde_json::Value = std::fs::read_to_string(&path)
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
    let path = claude_settings_path();
    let expected = expected_hook_urls(port);

    let mut settings: serde_json::Value = std::fs::read_to_string(&path)
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
    std::fs::write(&path, json).map_err(|e| e.to_string())?;

    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub provider: String,
    pub model: String,
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
}

fn default_model() -> ModelConfig {
    ModelConfig {
        provider: "google".into(),
        model: "gemini-2.5-flash".into(),
    }
}

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

impl Default for Settings {
    fn default() -> Self {
        Self {
            api_tokens: HashMap::new(),
            model: default_model(),
            priorities: default_priorities(),
            theme: default_theme(),
            port: default_port(),
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
