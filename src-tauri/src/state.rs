use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

use crate::settings::{ModelConfig, Settings};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CoachMode {
    Present,
    Away,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Light,
    Dark,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEntry {
    pub timestamp: DateTime<Utc>,
    pub session_id: String,
    pub hook_event: String,
    pub action: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub session_id: String,
    pub mode: CoachMode,
    pub cwd: Option<String>,
    pub last_event: DateTime<Utc>,
    pub event_count: usize,
}

/// Per-provider token status sent to the frontend.
/// "source" is "user", "env", or "none".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenStatus {
    pub source: String,
    /// Which env var was matched (only set when source == "env").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_var: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoachSnapshot {
    pub sessions: Vec<SessionSnapshot>,
    pub priorities: Vec<String>,
    pub activity_log: Vec<ActivityEntry>,
    pub port: u16,
    pub theme: Theme,
    pub model: ModelConfig,
    pub token_status: HashMap<String, TokenStatus>,
}

pub struct SessionState {
    pub mode: CoachMode,
    pub cwd: Option<String>,
    pub last_event: Instant,
    pub last_event_time: DateTime<Utc>,
    pub event_count: usize,
    pub last_stop_blocked: Option<Instant>,
}

pub struct CoachState {
    pub sessions: HashMap<String, SessionState>,
    pub priorities: Vec<String>,
    pub activity_log: Vec<ActivityEntry>,
    pub port: u16,
    pub theme: Theme,
    pub default_mode: CoachMode,
    pub model: ModelConfig,
    /// User-configured tokens (from settings file).
    pub api_tokens: HashMap<String, String>,
    /// Tokens detected from environment at startup (read-only).
    pub env_tokens: HashMap<String, String>,
}

impl CoachState {
    /// Resolve the effective token for a provider: user override wins, then env.
    pub fn effective_token(&self, provider: &str) -> Option<&str> {
        self.api_tokens
            .get(provider)
            .filter(|v| !v.is_empty())
            .or_else(|| self.env_tokens.get(provider))
            .map(|s| s.as_str())
    }
}

const MAX_LOG_ENTRIES: usize = 200;
const SESSION_TTL_SECS: u64 = 3600;

impl CoachState {
    pub fn from_settings(settings: Settings) -> Self {
        Self {
            sessions: HashMap::new(),
            priorities: settings.priorities,
            activity_log: Vec::new(),
            port: settings.port,
            theme: settings.theme,
            default_mode: CoachMode::Present,
            model: settings.model,
            api_tokens: settings.api_tokens,
            env_tokens: crate::settings::env_tokens(),
        }
    }

    pub fn to_settings(&self) -> Settings {
        Settings {
            api_tokens: self.api_tokens.clone(),
            model: self.model.clone(),
            priorities: self.priorities.clone(),
            theme: self.theme.clone(),
            port: self.port,
        }
    }

    pub fn save(&self) {
        self.to_settings().save();
    }

    /// Get or create a session, returning a mutable reference.
    pub fn session(&mut self, session_id: &str, cwd: Option<&str>) -> &mut SessionState {
        self.prune_stale();
        let default_mode = self.default_mode.clone();
        self.sessions
            .entry(session_id.to_string())
            .and_modify(|s| {
                s.last_event = Instant::now();
                s.last_event_time = Utc::now();
                s.event_count += 1;
                if let Some(cwd) = cwd {
                    s.cwd = Some(cwd.to_string());
                }
            })
            .or_insert_with(|| SessionState {
                mode: default_mode,
                cwd: cwd.map(String::from),
                last_event: Instant::now(),
                last_event_time: Utc::now(),
                event_count: 1,
                last_stop_blocked: None,
            })
    }

    /// Snapshot for the frontend. Tokens are masked (true = set, false = empty).
    pub fn snapshot(&self) -> CoachSnapshot {
        let mut sessions: Vec<SessionSnapshot> = self
            .sessions
            .iter()
            .map(|(id, s)| SessionSnapshot {
                session_id: id.clone(),
                mode: s.mode.clone(),
                cwd: s.cwd.clone(),
                last_event: s.last_event_time,
                event_count: s.event_count,
            })
            .collect();
        sessions.sort_by(|a, b| b.last_event.cmp(&a.last_event));

        CoachSnapshot {
            sessions,
            priorities: self.priorities.clone(),
            activity_log: self.activity_log.clone(),
            port: self.port,
            theme: self.theme.clone(),
            model: self.model.clone(),
            token_status: {
                let mut status = HashMap::new();
                for (provider, vars) in crate::settings::PROVIDER_ENV_VARS {
                    let has_user = self.api_tokens.get(*provider).is_some_and(|v| !v.is_empty());
                    let env_var_found = if !has_user {
                        vars.iter().find(|var| {
                            std::env::var(var).ok().is_some_and(|v| !v.is_empty())
                        }).map(|v| v.to_string())
                    } else {
                        None
                    };
                    let (source, env_var) = if has_user {
                        ("user", None)
                    } else if let Some(var) = env_var_found {
                        ("env", Some(var))
                    } else {
                        ("none", None)
                    };
                    status.insert(provider.to_string(), TokenStatus {
                        source: source.into(),
                        env_var,
                    });
                }
                status
            },
        }
    }

    pub fn log(
        &mut self,
        session_id: &str,
        hook_event: &str,
        action: &str,
        detail: Option<String>,
    ) {
        self.activity_log.push(ActivityEntry {
            timestamp: Utc::now(),
            session_id: session_id.to_string(),
            hook_event: hook_event.to_string(),
            action: action.to_string(),
            detail,
        });
        if self.activity_log.len() > MAX_LOG_ENTRIES {
            self.activity_log
                .drain(..self.activity_log.len() - MAX_LOG_ENTRIES);
        }
    }

    fn prune_stale(&mut self) {
        self.sessions
            .retain(|_, s| s.last_event.elapsed().as_secs() < SESSION_TTL_SECS);
    }
}

pub type SharedState = Arc<RwLock<CoachState>>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{ModelConfig, Settings};
    use std::collections::HashMap;

    /// Build a CoachState with empty env_tokens so tests don't depend
    /// on the machine's actual environment variables.
    fn test_state() -> CoachState {
        CoachState {
            sessions: HashMap::new(),
            priorities: vec!["Simplicity".into()],
            activity_log: Vec::new(),
            port: 7700,
            theme: Theme::System,
            default_mode: CoachMode::Present,
            model: ModelConfig {
                provider: "google".into(),
                model: "gemini-2.5-flash".into(),
            },
            api_tokens: HashMap::new(),
            env_tokens: HashMap::new(),
        }
    }

    // ── effective_token resolution chain ─────────────────────────────────

    /// User-provided token should take precedence over an env token
    /// for the same provider.
    #[test]
    fn effective_token_user_overrides_env() {
        let mut state = test_state();
        state.api_tokens.insert("google".into(), "user-key".into());
        state.env_tokens.insert("google".into(), "env-key".into());

        assert_eq!(state.effective_token("google"), Some("user-key"));
    }

    /// When the user token is empty, it should fall back to the env token.
    #[test]
    fn effective_token_empty_user_falls_back_to_env() {
        let mut state = test_state();
        state.api_tokens.insert("google".into(), "".into());
        state.env_tokens.insert("google".into(), "env-key".into());

        assert_eq!(state.effective_token("google"), Some("env-key"));
    }

    /// When neither user nor env token exists, effective_token returns None.
    #[test]
    fn effective_token_returns_none_when_absent() {
        let state = test_state();
        assert_eq!(state.effective_token("google"), None);
    }

    /// Token resolution is per-provider — setting a token for "google"
    /// should not affect "anthropic".
    #[test]
    fn effective_token_providers_are_independent() {
        let mut state = test_state();
        state.api_tokens.insert("google".into(), "gk".into());

        assert_eq!(state.effective_token("google"), Some("gk"));
        assert_eq!(state.effective_token("anthropic"), None);
    }

    // ── Session lifecycle ───────────────────────────────────────────────

    /// Calling session() for a new ID should create a session with
    /// event_count=1 and the state's default mode.
    #[test]
    fn session_creates_with_correct_defaults() {
        let mut state = test_state();
        state.default_mode = CoachMode::Away;

        let sess = state.session("s1", Some("/tmp"));
        assert_eq!(sess.event_count, 1);
        assert_eq!(sess.mode, CoachMode::Away);
        assert_eq!(sess.cwd, Some("/tmp".into()));
    }

    /// Calling session() again on the same ID should increment
    /// event_count and update cwd, but not reset mode.
    #[test]
    fn session_increments_event_count_and_updates_cwd() {
        let mut state = test_state();
        state.session("s1", Some("/a"));

        // Change mode to Away to verify it's preserved.
        state.sessions.get_mut("s1").unwrap().mode = CoachMode::Away;

        let sess = state.session("s1", Some("/b"));
        assert_eq!(sess.event_count, 2);
        assert_eq!(sess.cwd, Some("/b".into()));
        assert_eq!(sess.mode, CoachMode::Away, "mode should be preserved across events");
    }

    /// When cwd is None on a subsequent call, the existing cwd should
    /// be preserved (not overwritten to None).
    #[test]
    fn session_preserves_cwd_when_none() {
        let mut state = test_state();
        state.session("s1", Some("/original"));
        let sess = state.session("s1", None);
        assert_eq!(sess.cwd, Some("/original".into()));
    }

    // ── Activity log ────────────────────────────────────────────────────

    /// log() should append entries to the activity log.
    #[test]
    fn log_adds_entries() {
        let mut state = test_state();
        state.log("s1", "PostToolUse", "observed", None);
        state.log("s1", "Stop", "blocked", Some("priorities".into()));

        assert_eq!(state.activity_log.len(), 2);
        assert_eq!(state.activity_log[0].action, "observed");
        assert_eq!(state.activity_log[1].detail, Some("priorities".into()));
    }

    /// The activity log should be capped at MAX_LOG_ENTRIES (200).
    /// Adding more entries should drop the oldest ones.
    #[test]
    fn log_is_capped_at_200_entries() {
        let mut state = test_state();
        for i in 0..210 {
            state.log("s1", "PostToolUse", &format!("entry-{i}"), None);
        }
        assert_eq!(state.activity_log.len(), MAX_LOG_ENTRIES);
        // The oldest entries (0..9) should have been pruned.
        assert_eq!(state.activity_log[0].action, "entry-10");
        assert_eq!(state.activity_log[199].action, "entry-209");
    }

    // ── Snapshot properties ─────────────────────────────────────────────

    /// Snapshot sessions should be sorted by last_event descending
    /// (most recent first). We set timestamps manually to avoid
    /// depending on wall-clock timing between calls.
    #[test]
    fn snapshot_sessions_sorted_by_last_event_descending() {
        use chrono::Duration;

        let mut state = test_state();
        state.session("s1", None);
        state.session("s2", None);
        state.session("s3", None);

        // Manually assign distinct timestamps: s2 oldest, s3 middle, s1 newest.
        let now = Utc::now();
        state.sessions.get_mut("s2").unwrap().last_event_time = now - Duration::seconds(20);
        state.sessions.get_mut("s3").unwrap().last_event_time = now - Duration::seconds(10);
        state.sessions.get_mut("s1").unwrap().last_event_time = now;

        let snap = state.snapshot();
        let ids: Vec<&str> = snap.sessions.iter().map(|s| s.session_id.as_str()).collect();
        assert_eq!(ids, vec!["s1", "s3", "s2"], "should be sorted newest-first");
    }

    /// token_status should reflect "user" source when a user token is set.
    #[test]
    fn snapshot_token_status_reflects_user_token() {
        let mut state = test_state();
        state.api_tokens.insert("google".into(), "gk-user".into());

        let snap = state.snapshot();
        let google_status = snap.token_status.get("google").unwrap();
        assert_eq!(google_status.source, "user");
        assert!(google_status.env_var.is_none());
    }

    /// Snapshot should roundtrip model config without loss.
    #[test]
    fn snapshot_contains_model_config() {
        let mut state = test_state();
        state.model = ModelConfig {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-20250514".into(),
        };

        let snap = state.snapshot();
        assert_eq!(snap.model.provider, "anthropic");
        assert_eq!(snap.model.model, "claude-sonnet-4-20250514");
    }

    // ── from_settings / to_settings roundtrip ───────────────────────────

    /// Converting Settings -> CoachState -> Settings should preserve
    /// all persisted fields. This is the core save/load invariant.
    #[test]
    fn from_settings_to_settings_roundtrip() {
        let original = Settings {
            api_tokens: HashMap::from([("openai".into(), "sk-test".into())]),
            model: ModelConfig {
                provider: "openai".into(),
                model: "gpt-4o".into(),
            },
            priorities: vec!["Speed".into(), "Correctness".into()],
            theme: Theme::Dark,
            port: 8080,
        };

        // Note: from_settings calls env_tokens() which reads real env.
        // We construct the state manually to avoid that dependency.
        let state = CoachState {
            sessions: HashMap::new(),
            priorities: original.priorities.clone(),
            activity_log: Vec::new(),
            port: original.port,
            theme: original.theme.clone(),
            default_mode: CoachMode::Present,
            model: original.model.clone(),
            api_tokens: original.api_tokens.clone(),
            env_tokens: HashMap::new(),
        };

        let restored = state.to_settings();

        assert_eq!(restored.api_tokens, original.api_tokens);
        assert_eq!(restored.model.provider, original.model.provider);
        assert_eq!(restored.model.model, original.model.model);
        assert_eq!(restored.priorities, original.priorities);
        assert_eq!(restored.theme, original.theme);
        assert_eq!(restored.port, original.port);
    }

    /// to_settings should not include transient state like sessions or
    /// activity log — those are runtime-only.
    #[test]
    fn to_settings_excludes_transient_state() {
        let mut state = test_state();
        state.session("s1", Some("/tmp"));
        state.log("s1", "PostToolUse", "observed", None);

        let settings = state.to_settings();
        let json = serde_json::to_value(&settings).unwrap();

        // Settings JSON should not contain sessions or activity_log keys.
        assert!(json.get("sessions").is_none());
        assert!(json.get("activity_log").is_none());
    }

    // ── prune_stale ─────────────────────────────────────────────────────

    /// Fresh sessions (just created) should survive pruning.
    #[test]
    fn prune_stale_keeps_fresh_sessions() {
        let mut state = test_state();
        state.session("s1", None);
        state.session("s2", None);

        // prune_stale is called internally by session(), but let's call it
        // explicitly via a new session() to trigger it.
        state.session("s3", None);

        assert_eq!(state.sessions.len(), 3);
    }
}
