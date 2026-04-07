use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

use crate::settings::{CoachRule, EngineMode, ModelConfig, Settings};

pub const EVENT_STATE_UPDATED: &str = "coach-state-updated";
pub const EVENT_THEME_CHANGED: &str = "coach-theme-changed";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
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
    pub started_at: DateTime<Utc>,
    pub duration_secs: u64,
    pub display_name: String,
    pub tool_counts: HashMap<String, usize>,
    pub stop_count: usize,
    pub stop_blocked_count: usize,
    pub cwd_history: Vec<String>,
    pub coach_last_assessment: Option<String>,
    /// Recent activity for this session, oldest-first.
    pub activity: Vec<ActivityEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TokenSource {
    User,
    Env,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenStatus {
    pub source: TokenSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_var: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoachSnapshot {
    pub sessions: Vec<SessionSnapshot>,
    pub priorities: Vec<String>,
    pub port: u16,
    pub theme: Theme,
    pub model: ModelConfig,
    pub token_status: HashMap<String, TokenStatus>,
    pub coach_mode: EngineMode,
    pub rules: Vec<CoachRule>,
    /// Providers that support stateful coach sessions. Frontend uses this
    /// to mark unsupported choices in the model picker.
    pub observer_capable_providers: Vec<String>,
}

pub struct SessionState {
    pub mode: CoachMode,
    pub cwd: Option<String>,
    pub last_event: Instant,
    pub last_event_time: DateTime<Utc>,
    pub event_count: usize,
    pub last_stop_blocked: Option<Instant>,
    pub started_at: DateTime<Utc>,
    pub display_name: String,
    pub tool_counts: HashMap<String, usize>,
    pub stop_count: usize,
    pub stop_blocked_count: usize,
    pub cwd_history: Vec<String>,
    /// PID of the Claude Code process, set by the session scanner.
    pub pid: Option<u32>,
    /// OpenAI Responses API chain handle. Set by the first observer call,
    /// passed back as `previous_response_id` on subsequent calls so the
    /// model accumulates context server-side without us resending history.
    pub coach_response_id: Option<String>,
    /// Latest free-text observation produced by the coach LLM. Surfaces in
    /// the session detail UI.
    pub coach_last_assessment: Option<String>,
    /// Recent activity entries (hook events, coach decisions, scanner notes).
    /// Oldest-first; capped at SESSION_ACTIVITY_CAP.
    pub activity: VecDeque<ActivityEntry>,
}

pub struct CoachState {
    pub sessions: HashMap<String, SessionState>,
    pub priorities: Vec<String>,
    pub port: u16,
    pub theme: Theme,
    pub default_mode: CoachMode,
    pub model: ModelConfig,
    pub api_tokens: HashMap<String, String>,
    pub env_tokens: HashMap<String, String>,
    pub http_client: reqwest::Client,
    pub coach_mode: EngineMode,
    pub rules: Vec<CoachRule>,
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

const GENERIC_DIR_NAMES: &[&str] = &[
    "src", "lib", "app", "test", "tests", "dist", "build",
    "node_modules", "packages", ".git", "target",
];

/// Derive a human-readable display name from session working directories.
///
/// Picks the deepest non-home path from `cwd_history`, extracts its last segment,
/// and includes the parent if that segment is a generic name like "src" or "lib".
/// Falls back to the first 8 characters of `session_id` if no cwd is available.
fn derive_display_name(cwd_history: &[String], session_id: &str) -> String {
    let best = cwd_history
        .iter()
        .filter(|p| !p.is_empty())
        .max_by_key(|p| p.matches('/').count());

    let path = match best {
        Some(p) => p.as_str(),
        None => return session_id.chars().take(8).collect(),
    };

    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return session_id.chars().take(8).collect();
    }

    let last = segments[segments.len() - 1];
    if GENERIC_DIR_NAMES.contains(&last) && segments.len() >= 2 {
        let parent = segments[segments.len() - 2];
        format!("{}/{}", parent, last)
    } else {
        last.to_string()
    }
}

const SESSION_ACTIVITY_CAP: usize = 200;
const SESSION_TTL_SECS: u64 = 3600;

impl CoachState {
    pub fn from_settings(settings: Settings) -> Self {
        Self {
            sessions: HashMap::new(),
            priorities: settings.priorities,
            port: settings.port,
            theme: settings.theme,
            default_mode: CoachMode::Present,
            model: settings.model,
            api_tokens: settings.api_tokens,
            env_tokens: crate::settings::env_tokens(),
            http_client: reqwest::Client::new(),
            coach_mode: settings.coach_mode,
            rules: settings.rules,
        }
    }

    pub fn set_all_modes(&mut self, mode: CoachMode) {
        self.default_mode = mode.clone();
        for session in self.sessions.values_mut() {
            session.mode = mode.clone();
        }
    }

    pub fn to_settings(&self) -> Settings {
        Settings {
            api_tokens: self.api_tokens.clone(),
            model: self.model.clone(),
            priorities: self.priorities.clone(),
            theme: self.theme.clone(),
            port: self.port,
            coach_mode: self.coach_mode.clone(),
            rules: self.rules.clone(),
        }
    }

    pub fn save(&self) {
        self.to_settings().save();
    }

    /// Get or create a session, returning a mutable reference.
    pub fn session(&mut self, session_id: &str, cwd: Option<&str>) -> &mut SessionState {
        self.prune_stale();
        let default_mode = self.default_mode.clone();
        let sid = session_id.to_string();
        self.sessions
            .entry(sid.clone())
            .and_modify(|s| {
                s.last_event = Instant::now();
                s.last_event_time = Utc::now();
                s.event_count += 1;
                if let Some(cwd) = cwd {
                    s.cwd = Some(cwd.to_string());
                    if !s.cwd_history.iter().any(|c| c == cwd) {
                        s.cwd_history.push(cwd.to_string());
                        s.display_name = derive_display_name(&s.cwd_history, &sid);
                    }
                }
            })
            .or_insert_with(|| {
                let cwd_history: Vec<String> = cwd.iter().map(|c| c.to_string()).collect();
                let display_name = derive_display_name(&cwd_history, &sid);
                SessionState {
                    mode: default_mode,
                    cwd: cwd.map(String::from),
                    last_event: Instant::now(),
                    last_event_time: Utc::now(),
                    event_count: 1,
                    last_stop_blocked: None,
                    started_at: Utc::now(),
                    display_name,
                    tool_counts: HashMap::new(),
                    stop_count: 0,
                    stop_blocked_count: 0,
                    cwd_history,
                    pid: None,
                    coach_response_id: None,
                    coach_last_assessment: None,
                    activity: VecDeque::new(),
                }
            })
    }

    /// Snapshot for the frontend. Tokens are masked (true = set, false = empty).
    pub fn snapshot(&self) -> CoachSnapshot {
        let now = Utc::now();
        let mut sessions: Vec<SessionSnapshot> = self
            .sessions
            .iter()
            .map(|(id, s)| SessionSnapshot {
                session_id: id.clone(),
                mode: s.mode.clone(),
                cwd: s.cwd.clone(),
                last_event: s.last_event_time,
                event_count: s.event_count,
                started_at: s.started_at,
                duration_secs: (now - s.started_at).num_seconds().max(0) as u64,
                display_name: s.display_name.clone(),
                tool_counts: s.tool_counts.clone(),
                stop_count: s.stop_count,
                stop_blocked_count: s.stop_blocked_count,
                cwd_history: s.cwd_history.clone(),
                coach_last_assessment: s.coach_last_assessment.clone(),
                activity: s.activity.iter().cloned().collect(),
            })
            .collect();
        sessions.sort_by(|a, b| b.last_event.cmp(&a.last_event));

        CoachSnapshot {
            sessions,
            priorities: self.priorities.clone(),
            port: self.port,
            theme: self.theme.clone(),
            model: self.model.clone(),
            token_status: {
                let mut status = HashMap::new();
                for (provider, vars) in crate::settings::PROVIDER_ENV_VARS {
                    let has_user = self.api_tokens.get(*provider).is_some_and(|v| !v.is_empty());
                    let has_env = self.env_tokens.contains_key(*provider);
                    let env_var_name = if !has_user && has_env {
                        vars.iter().find(|_| {
                            self.env_tokens.contains_key(*provider)
                        }).map(|v| v.to_string())
                    } else {
                        None
                    };
                    let (source, env_var) = if has_user {
                        (TokenSource::User, None)
                    } else if let Some(var) = env_var_name {
                        (TokenSource::Env, Some(var))
                    } else {
                        (TokenSource::None, None)
                    };
                    status.insert(provider.to_string(), TokenStatus { source, env_var });
                }
                status
            },
            coach_mode: self.coach_mode.clone(),
            rules: self.rules.clone(),
            observer_capable_providers: crate::settings::OBSERVER_CAPABLE_PROVIDERS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }

    /// Append an activity entry to the given session's queue. Silently
    /// drops the entry if the session no longer exists — log calls are
    /// best-effort and should never crash the hook server.
    pub fn log(
        &mut self,
        session_id: &str,
        hook_event: &str,
        action: &str,
        detail: Option<String>,
    ) {
        let Some(session) = self.sessions.get_mut(session_id) else {
            return;
        };
        session.activity.push_back(ActivityEntry {
            timestamp: Utc::now(),
            hook_event: hook_event.to_string(),
            action: action.to_string(),
            detail,
        });
        while session.activity.len() > SESSION_ACTIVITY_CAP {
            session.activity.pop_front();
        }
    }

    fn prune_stale(&mut self) {
        self.sessions
            .retain(|_, s| s.last_event.elapsed().as_secs() < SESSION_TTL_SECS);
    }

    /// Register a session discovered by the file scanner.
    /// If the session already exists (created by a hook), just assigns the PID.
    pub fn register_discovered(
        &mut self,
        session_id: &str,
        cwd: Option<&str>,
        started_at: DateTime<Utc>,
        pid: u32,
    ) {
        if let Some(sess) = self.sessions.get_mut(session_id) {
            if sess.pid.is_none() {
                sess.pid = Some(pid);
            }
            return;
        }

        let cwd_history: Vec<String> = cwd.iter().map(|c| c.to_string()).collect();
        let display_name = derive_display_name(&cwd_history, session_id);
        self.sessions.insert(
            session_id.to_string(),
            SessionState {
                mode: self.default_mode.clone(),
                cwd: cwd.map(String::from),
                last_event: Instant::now(),
                last_event_time: Utc::now(),
                event_count: 0,
                last_stop_blocked: None,
                started_at,
                display_name,
                tool_counts: HashMap::new(),
                stop_count: 0,
                stop_blocked_count: 0,
                cwd_history,
                pid: Some(pid),
                coach_response_id: None,
                coach_last_assessment: None,
                activity: VecDeque::new(),
            },
        );
    }

    /// Remove sessions whose PID was set by the scanner but is no longer
    /// in the live set. Returns the IDs of removed sessions.
    pub fn remove_dead_sessions(&mut self, live_session_ids: &HashSet<String>) -> Vec<String> {
        let dead: Vec<String> = self
            .sessions
            .iter()
            .filter(|(id, s)| s.pid.is_some() && !live_session_ids.contains(id.as_str()))
            .map(|(id, _)| id.clone())
            .collect();

        for id in &dead {
            self.sessions.remove(id);
        }
        dead
    }
}

/// Build the "away mode" intervention message from the current priorities.
pub fn away_message(priorities: &[String]) -> String {
    let ptext = priorities
        .iter()
        .enumerate()
        .map(|(i, p)| format!("{}. {}", i + 1, p))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "User is away. Continue working autonomously. \
         If you need to make a decision, use these priorities (highest first): {}. \
         If you were asking whether to proceed — yes, proceed.",
        ptext
    )
}

pub type SharedState = Arc<RwLock<CoachState>>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{EngineMode, ModelConfig, Settings};
    use std::collections::HashMap;

    /// Build a CoachState with empty env_tokens so tests don't depend
    /// on the machine's actual environment variables.
    fn test_state() -> CoachState {
        CoachState {
            sessions: HashMap::new(),
            priorities: vec!["Simplicity".into()],
            port: 7700,
            theme: Theme::System,
            default_mode: CoachMode::Present,
            model: ModelConfig {
                provider: "google".into(),
                model: "gemini-2.5-flash".into(),
            },
            api_tokens: HashMap::new(),
            env_tokens: HashMap::new(),
            http_client: reqwest::Client::new(),
            coach_mode: EngineMode::Rules,
            rules: vec![],
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

    /// log() should append entries to the targeted session's activity queue.
    #[test]
    fn log_adds_entries_to_session() {
        let mut state = test_state();
        state.session("s1", None);
        state.log("s1", "PostToolUse", "observed", None);
        state.log("s1", "Stop", "blocked", Some("priorities".into()));

        let activity = &state.sessions.get("s1").unwrap().activity;
        assert_eq!(activity.len(), 2);
        assert_eq!(activity[0].action, "observed");
        assert_eq!(activity[1].detail, Some("priorities".into()));
    }

    /// log() for a non-existent session must be a silent no-op
    /// (best-effort logging — never crash the hook server).
    #[test]
    fn log_for_unknown_session_is_silent_noop() {
        let mut state = test_state();
        state.log("ghost", "PostToolUse", "observed", None);
        assert!(state.sessions.is_empty());
    }

    /// Each session's queue is capped independently at SESSION_ACTIVITY_CAP.
    #[test]
    fn log_is_capped_per_session() {
        let mut state = test_state();
        state.session("s1", None);
        for i in 0..SESSION_ACTIVITY_CAP + 10 {
            state.log("s1", "PostToolUse", &format!("entry-{i}"), None);
        }
        let activity = &state.sessions.get("s1").unwrap().activity;
        assert_eq!(activity.len(), SESSION_ACTIVITY_CAP);
        // The oldest 10 should have been pruned.
        assert_eq!(activity[0].action, "entry-10");
        assert_eq!(
            activity[SESSION_ACTIVITY_CAP - 1].action,
            format!("entry-{}", SESSION_ACTIVITY_CAP + 9),
        );
    }

    /// Property: a chatty session must not evict another session's history.
    /// This is the regression test for the original "shared global log" bug.
    #[test]
    fn busy_session_does_not_evict_quiet_session() {
        let mut state = test_state();
        state.session("quiet", None);
        state.session("busy", None);

        state.log("quiet", "PostToolUse", "first", Some("Read".into()));

        // Hammer the busy session well past the per-session cap.
        for i in 0..SESSION_ACTIVITY_CAP * 3 {
            state.log("busy", "PostToolUse", &format!("noise-{i}"), None);
        }

        let quiet = &state.sessions.get("quiet").unwrap().activity;
        assert_eq!(quiet.len(), 1, "quiet session must keep its only entry");
        assert_eq!(quiet[0].action, "first");
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
        assert_eq!(google_status.source, TokenSource::User);
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
            coach_mode: EngineMode::Rules,
            rules: vec![],
        };

        // Note: from_settings calls env_tokens() which reads real env.
        // We construct the state manually to avoid that dependency.
        let state = CoachState {
            sessions: HashMap::new(),
            priorities: original.priorities.clone(),
            port: original.port,
            theme: original.theme.clone(),
            default_mode: CoachMode::Present,
            model: original.model.clone(),
            api_tokens: original.api_tokens.clone(),
            env_tokens: HashMap::new(),
            http_client: reqwest::Client::new(),
            coach_mode: original.coach_mode.clone(),
            rules: original.rules.clone(),
        };

        let restored = state.to_settings();

        assert_eq!(restored.api_tokens, original.api_tokens);
        assert_eq!(restored.model.provider, original.model.provider);
        assert_eq!(restored.model.model, original.model.model);
        assert_eq!(restored.priorities, original.priorities);
        assert_eq!(restored.theme, original.theme);
        assert_eq!(restored.port, original.port);
        assert_eq!(restored.coach_mode, original.coach_mode);
        assert_eq!(restored.rules, original.rules);
    }

    /// to_settings should not include transient state like sessions or
    /// activity — those are runtime-only.
    #[test]
    fn to_settings_excludes_transient_state() {
        let mut state = test_state();
        state.session("s1", Some("/tmp"));
        state.log("s1", "PostToolUse", "observed", None);

        let settings = state.to_settings();
        let json = serde_json::to_value(&settings).unwrap();

        // Settings JSON should not contain sessions or per-session activity.
        assert!(json.get("sessions").is_none());
        assert!(json.get("activity").is_none());
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

    // ── derive_display_name ────────────────────────────────────────────

    /// A normal project path should yield just the last segment.
    #[test]
    fn display_name_normal_path() {
        let history = vec!["/Users/foo/projects/coach".into()];
        assert_eq!(derive_display_name(&history, "abc12345"), "coach");
    }

    /// When the last segment is generic (e.g. "src"), include parent/child.
    #[test]
    fn display_name_generic_last_segment() {
        let history = vec!["/Users/foo/projects/coach/src".into()];
        assert_eq!(derive_display_name(&history, "abc12345"), "coach/src");
    }

    /// With no cwd history, fall back to first 8 chars of session_id.
    #[test]
    fn display_name_fallback_to_session_id() {
        let history: Vec<String> = vec![];
        assert_eq!(derive_display_name(&history, "abcdef1234567890"), "abcdef12");
    }

    /// With multiple cwd entries, pick the deepest (most path segments).
    #[test]
    fn display_name_picks_deepest_cwd() {
        let history = vec![
            "/Users/foo/projects".into(),
            "/Users/foo/projects/coach/src".into(),
            "/Users/foo".into(),
        ];
        // Deepest is coach/src — "src" is generic, so "coach/src"
        assert_eq!(derive_display_name(&history, "abc12345"), "coach/src");
    }

    // ── New session fields ─────────────────────────────────────────────

    /// New sessions should have started_at set, empty tool_counts, zero counters,
    /// and cwd_history populated from the initial cwd.
    #[test]
    fn session_initializes_new_fields() {
        let mut state = test_state();
        let before = Utc::now();
        let sess = state.session("s1", Some("/Users/foo/projects/coach"));

        assert!(sess.started_at >= before);
        assert!(sess.tool_counts.is_empty());
        assert_eq!(sess.stop_count, 0);
        assert_eq!(sess.stop_blocked_count, 0);
        assert_eq!(sess.cwd_history, vec!["/Users/foo/projects/coach"]);
        assert_eq!(sess.display_name, "coach");
    }

    /// Updating a session with a new cwd should append to cwd_history
    /// and recompute display_name.
    #[test]
    fn session_updates_cwd_history_on_change() {
        let mut state = test_state();
        state.session("s1", Some("/Users/foo/projects/coach"));
        let sess = state.session("s1", Some("/Users/foo/projects/coach/src"));

        assert_eq!(sess.cwd_history, vec![
            "/Users/foo/projects/coach",
            "/Users/foo/projects/coach/src",
        ]);
        assert_eq!(sess.display_name, "coach/src");
    }

    /// Re-visiting an already-seen cwd should not duplicate it in history.
    #[test]
    fn session_does_not_duplicate_cwd_history() {
        let mut state = test_state();
        state.session("s1", Some("/Users/foo/projects/coach"));
        state.session("s1", Some("/Users/foo/projects/coach/src"));
        let sess = state.session("s1", Some("/Users/foo/projects/coach"));

        assert_eq!(sess.cwd_history.len(), 2);
    }

    // ── register_discovered / remove_dead_sessions ──────────────────────

    /// register_discovered should create a session with event_count=0
    /// and the provided started_at timestamp (not Utc::now()).
    #[test]
    fn register_discovered_creates_session() {
        use chrono::Duration;
        let mut state = test_state();
        let started = Utc::now() - Duration::hours(1);
        state.register_discovered("scan-1", Some("/projects/foo"), started, 12345);

        let sess = state.sessions.get("scan-1").unwrap();
        assert_eq!(sess.event_count, 0);
        assert_eq!(sess.pid, Some(12345));
        assert_eq!(sess.started_at, started);
        assert_eq!(sess.cwd, Some("/projects/foo".into()));
    }

    /// When a hook already created the session, register_discovered
    /// should only assign the PID without resetting other fields.
    #[test]
    fn register_discovered_assigns_pid_to_existing() {
        let mut state = test_state();
        // Hook creates session first
        state.session("s1", Some("/a"));
        assert_eq!(state.sessions.get("s1").unwrap().event_count, 1);
        assert!(state.sessions.get("s1").unwrap().pid.is_none());

        // Scanner discovers it
        state.register_discovered("s1", Some("/a"), Utc::now(), 999);

        let sess = state.sessions.get("s1").unwrap();
        assert_eq!(sess.pid, Some(999));
        assert_eq!(sess.event_count, 1, "event_count should be preserved");
    }

    /// remove_dead_sessions should remove sessions with a PID that is
    /// not in the live set, and leave sessions without a PID untouched.
    #[test]
    fn remove_dead_sessions_only_removes_pid_sessions() {
        let mut state = test_state();
        // hook-only session (no pid)
        state.session("hook-only", None);
        // scanner-discovered session
        state.register_discovered("scanned", Some("/a"), Utc::now(), 111);

        let live = HashSet::new(); // nothing is live
        let dead = state.remove_dead_sessions(&live);

        assert_eq!(dead, vec!["scanned"]);
        assert!(state.sessions.contains_key("hook-only"), "hook session should survive");
        assert!(!state.sessions.contains_key("scanned"));
    }

    /// remove_dead_sessions should keep sessions whose ID is in the live set.
    #[test]
    fn remove_dead_sessions_keeps_live() {
        let mut state = test_state();
        state.register_discovered("alive", Some("/a"), Utc::now(), 222);

        let live: HashSet<String> = ["alive".to_string()].into();
        let dead = state.remove_dead_sessions(&live);

        assert!(dead.is_empty());
        assert!(state.sessions.contains_key("alive"));
    }

    /// Snapshot should include the new session fields.
    #[test]
    fn snapshot_includes_new_session_fields() {
        let mut state = test_state();
        state.session("s1", Some("/Users/foo/projects/coach"));
        {
            let sess = state.sessions.get_mut("s1").unwrap();
            sess.tool_counts.insert("Read".into(), 3);
            sess.stop_count = 2;
            sess.stop_blocked_count = 1;
        }

        let snap = state.snapshot();
        let s = &snap.sessions[0];
        assert_eq!(s.display_name, "coach");
        assert_eq!(s.tool_counts.get("Read"), Some(&3));
        assert_eq!(s.stop_count, 2);
        assert_eq!(s.stop_blocked_count, 1);
        assert_eq!(s.cwd_history, vec!["/Users/foo/projects/coach"]);
        assert!(s.duration_secs < 5, "duration should be near zero for a just-created session");
    }
}
