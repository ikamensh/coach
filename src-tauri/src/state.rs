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

// ── Coach LLM chain (per-session conversation handle) ─────────────────
//
// Different providers preserve conversation state in different ways:
//   • OpenAI Responses API: server-side, indexed by response_id. We just
//     store the latest id and pass it as previous_response_id next call.
//   • Anthropic: no server state. We keep the message history client-side
//     (cached cheap via prompt caching).
// `CoachChain` lets one SessionState field cover both — and stays
// `Empty` until the first observer call.

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CoachRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CoachMessage {
    pub role: CoachRole,
    pub content: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CoachChain {
    #[default]
    Empty,
    OpenAi {
        response_id: String,
    },
    Anthropic {
        history: Vec<CoachMessage>,
    },
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

/// Snapshot of one Claude Code window — keyed by PID, surfacing the
/// **current** conversation in that window. See docs/SESSION_TRACKING.md.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// OS PID of the Claude Code process. Stable across `/clear`.
    pub pid: u32,
    /// Current conversation id. Changes when the user runs `/clear`,
    /// `/resume`, or `/compact`. Empty string if the scanner saw the
    /// process before any hook fired.
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
    /// Recent activity for the current conversation, oldest-first.
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

/// Per-window state. The owning `CoachState.sessions` map is keyed by
/// `pid` — `current_session_id` is just the label of the conversation
/// currently running in that window.
pub struct SessionState {
    pub pid: u32,
    pub current_session_id: String,
    pub mode: CoachMode,
    pub cwd: Option<String>,
    pub last_event: Instant,
    pub last_event_time: DateTime<Utc>,
    pub event_count: usize,
    pub last_stop_blocked: Option<Instant>,
    /// When the **current conversation** started. Resets on `/clear`.
    pub started_at: DateTime<Utc>,
    pub display_name: String,
    pub tool_counts: HashMap<String, usize>,
    pub stop_count: usize,
    pub stop_blocked_count: usize,
    /// All cwds this **window** has been in. Persists across `/clear`
    /// because it describes the process, not the conversation.
    pub cwd_history: Vec<String>,
    /// Coach LLM chain handle (provider-specific). Reset to `Empty` on
    /// `/clear` since the new conversation has no shared context with
    /// the previous one.
    pub coach_chain: CoachChain,
    pub coach_last_assessment: Option<String>,
    pub activity: VecDeque<ActivityEntry>,
}

pub struct CoachState {
    /// Keyed by PID — one entry per Claude Code window.
    pub sessions: HashMap<u32, SessionState>,
    /// Cache: hook session_id → PID. Populated on the first hook of a
    /// new conversation; lets later hooks for the same conversation skip
    /// the lsof lookup. Cleared for any PID that dies.
    pub session_id_to_pid: HashMap<String, u32>,
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

/// Derive a human-readable display name from a window's working directories.
///
/// Picks the deepest non-home path from `cwd_history`, extracts its last segment,
/// and includes the parent if that segment is a generic name like "src" or "lib".
/// Falls back to `pid:<n>` if no cwd is available.
fn derive_display_name(cwd_history: &[String], pid: u32) -> String {
    let best = cwd_history
        .iter()
        .filter(|p| !p.is_empty())
        .max_by_key(|p| p.matches('/').count());

    let path = match best {
        Some(p) => p.as_str(),
        None => return format!("pid:{pid}"),
    };

    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return format!("pid:{pid}");
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
/// Sessions that have had any activity within this window count as "active"
/// for ordering purposes. Crossing this threshold is the only thing that
/// can cause a session to change rank, so the list stays stable while you
/// work and only occasionally demotes a session that has gone idle.
const SESSION_ACTIVE_WINDOW_SECS: i64 = 15 * 60;

/// Two-bucket activity classifier used by the snapshot sort.
/// 0 = active (recent activity), 1 = idle.
fn activity_bucket(last_event: DateTime<Utc>, now: DateTime<Utc>) -> u8 {
    if (now - last_event).num_seconds() < SESSION_ACTIVE_WINDOW_SECS {
        0
    } else {
        1
    }
}

impl CoachState {
    pub fn from_settings(settings: Settings) -> Self {
        Self {
            sessions: HashMap::new(),
            session_id_to_pid: HashMap::new(),
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

    /// Process a hook event for a known PID. This is the canonical mutation
    /// for hook handlers — it handles three cases:
    ///
    /// 1. **No session for this PID yet** → create one.
    /// 2. **PID has a session and `session_id` matches** → bump counters.
    /// 3. **PID has a session under a different `session_id`** → this is a
    ///    `/clear` (or `/resume` / `/compact`). Replace the conversation:
    ///    new id, fresh `started_at`, counters reset to 0, activity cleared,
    ///    coach response chain reset. PID, mode, cwd_history, display_name
    ///    are preserved because they describe the **window**.
    ///
    /// Returns a mutable reference to the session for callers that want to
    /// poke at it further (e.g. set tool_counts).
    pub fn apply_hook_event(
        &mut self,
        pid: u32,
        session_id: &str,
        cwd: Option<&str>,
    ) -> &mut SessionState {
        // Index the session_id → pid mapping so subsequent hooks skip the
        // lsof lookup. Safe to overwrite — same session_id never moves
        // between PIDs.
        self.session_id_to_pid.insert(session_id.to_string(), pid);

        let default_mode = self.default_mode.clone();
        let now = Utc::now();

        match self.sessions.get_mut(&pid) {
            Some(sess) if sess.current_session_id == session_id => {
                sess.last_event = Instant::now();
                sess.last_event_time = now;
                sess.event_count += 1;
                if let Some(cwd) = cwd {
                    sess.cwd = Some(cwd.to_string());
                    if !sess.cwd_history.iter().any(|c| c == cwd) {
                        sess.cwd_history.push(cwd.to_string());
                        sess.display_name = derive_display_name(&sess.cwd_history, pid);
                    }
                }
            }
            Some(sess) if sess.current_session_id.is_empty() => {
                // First hook for a scanner-discovered placeholder. Adopt
                // the conversation id without resetting started_at — the
                // scanner already populated it from the session file.
                sess.current_session_id = session_id.to_string();
                sess.last_event = Instant::now();
                sess.last_event_time = now;
                sess.event_count = 1;
                if let Some(cwd) = cwd {
                    sess.cwd = Some(cwd.to_string());
                    if !sess.cwd_history.iter().any(|c| c == cwd) {
                        sess.cwd_history.push(cwd.to_string());
                        sess.display_name = derive_display_name(&sess.cwd_history, pid);
                    }
                }
            }
            Some(sess) => {
                // /clear: new conversation in the same window. Reset
                // conversation-scoped state, keep window-scoped state.
                sess.current_session_id = session_id.to_string();
                sess.last_event = Instant::now();
                sess.last_event_time = now;
                sess.event_count = 1;
                sess.started_at = now;
                sess.tool_counts.clear();
                sess.stop_count = 0;
                sess.stop_blocked_count = 0;
                sess.last_stop_blocked = None;
                sess.coach_chain = CoachChain::Empty;
                sess.coach_last_assessment = None;
                sess.activity.clear();
                if let Some(cwd) = cwd {
                    sess.cwd = Some(cwd.to_string());
                    if !sess.cwd_history.iter().any(|c| c == cwd) {
                        sess.cwd_history.push(cwd.to_string());
                        sess.display_name = derive_display_name(&sess.cwd_history, pid);
                    }
                }
            }
            None => {
                let cwd_history: Vec<String> = cwd.iter().map(|c| c.to_string()).collect();
                let display_name = derive_display_name(&cwd_history, pid);
                self.sessions.insert(
                    pid,
                    SessionState {
                        pid,
                        current_session_id: session_id.to_string(),
                        mode: default_mode,
                        cwd: cwd.map(String::from),
                        last_event: Instant::now(),
                        last_event_time: now,
                        event_count: 1,
                        last_stop_blocked: None,
                        started_at: now,
                        display_name,
                        tool_counts: HashMap::new(),
                        stop_count: 0,
                        stop_blocked_count: 0,
                        cwd_history,
                        coach_chain: CoachChain::Empty,
                        coach_last_assessment: None,
                        activity: VecDeque::new(),
                    },
                );
            }
        }
        self.sessions.get_mut(&pid).expect("just inserted")
    }

    /// Snapshot for the frontend.
    pub fn snapshot(&self) -> CoachSnapshot {
        let now = Utc::now();
        let mut sessions: Vec<SessionSnapshot> = self
            .sessions
            .values()
            .map(|s| SessionSnapshot {
                pid: s.pid,
                session_id: s.current_session_id.clone(),
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
        // Stable two-bucket sort: active sessions on top, idle below.
        // Within a bucket, newest-started first. The only event that
        // reorders the list is a session crossing the active/idle
        // boundary — so the order stays stable while you're working.
        sessions.sort_by(|a, b| {
            let bucket_a = activity_bucket(a.last_event, now);
            let bucket_b = activity_bucket(b.last_event, now);
            bucket_a
                .cmp(&bucket_b)
                .then_with(|| b.started_at.cmp(&a.started_at))
        });

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

    /// Append an activity entry to the session for `pid`. Silent no-op
    /// if the PID has no session — log calls are best-effort and must
    /// never crash the hook server.
    pub fn log(
        &mut self,
        pid: u32,
        hook_event: &str,
        action: &str,
        detail: Option<String>,
    ) {
        let Some(session) = self.sessions.get_mut(&pid) else {
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

    /// Register a PID discovered by the file scanner. Creates a placeholder
    /// session entry if no hook has populated one yet, so a freshly-launched
    /// Claude Code window appears in the UI before the user types anything.
    ///
    /// If a session for this PID already exists (because a hook beat the
    /// scanner), this is a no-op and returns false.
    pub fn register_discovered_pid(
        &mut self,
        pid: u32,
        cwd: Option<&str>,
        started_at: DateTime<Utc>,
    ) -> bool {
        if self.sessions.contains_key(&pid) {
            return false;
        }
        let cwd_history: Vec<String> = cwd.iter().map(|c| c.to_string()).collect();
        let display_name = derive_display_name(&cwd_history, pid);
        self.sessions.insert(
            pid,
            SessionState {
                pid,
                // No current conversation yet — first hook will fill this in.
                current_session_id: String::new(),
                mode: self.default_mode.clone(),
                cwd: cwd.map(String::from),
                last_event: Instant::now(),
                last_event_time: started_at,
                event_count: 0,
                last_stop_blocked: None,
                started_at,
                display_name,
                tool_counts: HashMap::new(),
                stop_count: 0,
                stop_blocked_count: 0,
                cwd_history,
                coach_chain: CoachChain::Empty,
                coach_last_assessment: None,
                activity: VecDeque::new(),
            },
        );
        true
    }

    /// Remove sessions whose PID is not in the live set. Returns the
    /// removed PIDs. Also drops any cached `session_id → pid` entries
    /// pointing at the dead PIDs so the cache doesn't grow unbounded.
    pub fn remove_dead_pids(&mut self, live_pids: &HashSet<u32>) -> Vec<u32> {
        let dead: Vec<u32> = self
            .sessions
            .keys()
            .copied()
            .filter(|pid| !live_pids.contains(pid))
            .collect();
        for pid in &dead {
            self.sessions.remove(pid);
        }
        self.session_id_to_pid
            .retain(|_, pid| live_pids.contains(pid));
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
            session_id_to_pid: HashMap::new(),
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

    #[test]
    fn effective_token_user_overrides_env() {
        let mut state = test_state();
        state.api_tokens.insert("google".into(), "user-key".into());
        state.env_tokens.insert("google".into(), "env-key".into());
        assert_eq!(state.effective_token("google"), Some("user-key"));
    }

    #[test]
    fn effective_token_empty_user_falls_back_to_env() {
        let mut state = test_state();
        state.api_tokens.insert("google".into(), "".into());
        state.env_tokens.insert("google".into(), "env-key".into());
        assert_eq!(state.effective_token("google"), Some("env-key"));
    }

    #[test]
    fn effective_token_returns_none_when_absent() {
        let state = test_state();
        assert_eq!(state.effective_token("google"), None);
    }

    #[test]
    fn effective_token_providers_are_independent() {
        let mut state = test_state();
        state.api_tokens.insert("google".into(), "gk".into());
        assert_eq!(state.effective_token("google"), Some("gk"));
        assert_eq!(state.effective_token("anthropic"), None);
    }

    // ── apply_hook_event lifecycle ──────────────────────────────────────

    /// First hook for a PID creates a session under that PID with
    /// event_count=1 and the right mode.
    #[test]
    fn apply_hook_event_creates_session_for_new_pid() {
        let mut state = test_state();
        state.default_mode = CoachMode::Away;

        state.apply_hook_event(42, "conv-1", Some("/tmp"));

        let sess = state.sessions.get(&42).unwrap();
        assert_eq!(sess.pid, 42);
        assert_eq!(sess.current_session_id, "conv-1");
        assert_eq!(sess.event_count, 1);
        assert_eq!(sess.mode, CoachMode::Away);
        assert_eq!(sess.cwd, Some("/tmp".into()));
        // Cache should be primed.
        assert_eq!(state.session_id_to_pid.get("conv-1"), Some(&42));
    }

    /// Same (pid, session_id) on a second hook bumps the counter and
    /// preserves user-set fields like mode.
    #[test]
    fn apply_hook_event_increments_existing_session() {
        let mut state = test_state();
        state.apply_hook_event(42, "conv-1", Some("/a"));
        // Flip mode to Away to verify it's preserved.
        state.sessions.get_mut(&42).unwrap().mode = CoachMode::Away;

        state.apply_hook_event(42, "conv-1", Some("/b"));

        let sess = state.sessions.get(&42).unwrap();
        assert_eq!(sess.event_count, 2);
        assert_eq!(sess.cwd, Some("/b".into()));
        assert_eq!(sess.mode, CoachMode::Away, "mode survives hook updates");
    }

    /// /clear: same PID, new session_id. Counters reset, started_at moves
    /// forward, activity is wiped, but pid/mode/cwd_history persist.
    /// This is the core regression test for the original bug.
    #[test]
    fn apply_hook_event_resets_on_clear() {
        let mut state = test_state();
        state.apply_hook_event(42, "old", Some("/projects/coach"));
        {
            let s = state.sessions.get_mut(&42).unwrap();
            s.mode = CoachMode::Away;
            s.event_count = 17;
            s.tool_counts.insert("Bash".into(), 9);
            s.stop_count = 3;
            s.stop_blocked_count = 2;
            s.coach_chain = CoachChain::OpenAi { response_id: "resp_old".into() };
            s.activity.push_back(ActivityEntry {
                timestamp: Utc::now(),
                hook_event: "x".into(),
                action: "y".into(),
                detail: None,
            });
        }
        let original_started = state.sessions.get(&42).unwrap().started_at;

        // Sleep a touch so started_at differs measurably.
        std::thread::sleep(std::time::Duration::from_millis(2));

        state.apply_hook_event(42, "new", Some("/projects/coach"));

        let sess = state.sessions.get(&42).unwrap();
        // Conversation-scoped: reset
        assert_eq!(sess.current_session_id, "new");
        assert_eq!(sess.event_count, 1);
        assert!(sess.tool_counts.is_empty());
        assert_eq!(sess.stop_count, 0);
        assert_eq!(sess.stop_blocked_count, 0);
        assert_eq!(sess.coach_chain, CoachChain::Empty, "/clear must reset chain");
        assert!(sess.activity.is_empty());
        assert!(sess.started_at > original_started);
        // Window-scoped: preserved
        assert_eq!(sess.pid, 42);
        assert_eq!(sess.mode, CoachMode::Away, "mode is window-scoped");
        assert_eq!(sess.cwd_history.len(), 1, "cwd preserved");
        // Cache: BOTH ids point at the same PID (lookup safety)
        assert_eq!(state.session_id_to_pid.get("old"), Some(&42));
        assert_eq!(state.session_id_to_pid.get("new"), Some(&42));
    }

    /// First hook for a scanner-discovered placeholder adopts the
    /// conversation id without treating it as a /clear. started_at is
    /// preserved (the scanner read it from the session file).
    #[test]
    fn apply_hook_event_adopts_scanner_placeholder() {
        use chrono::Duration;
        let mut state = test_state();
        let scanner_started = Utc::now() - Duration::hours(2);
        state.register_discovered_pid(42, Some("/p"), scanner_started);

        state.apply_hook_event(42, "conv-X", Some("/p"));

        let sess = state.sessions.get(&42).unwrap();
        assert_eq!(sess.current_session_id, "conv-X");
        assert_eq!(sess.event_count, 1);
        assert_eq!(
            sess.started_at, scanner_started,
            "scanner started_at must survive the first hook"
        );
    }

    /// Two distinct PIDs are tracked as two distinct sessions even when
    /// they share a cwd. This is the multi-window-same-cwd case that
    /// every other heuristic fails on.
    #[test]
    fn distinct_pids_in_same_cwd_are_separate_sessions() {
        let mut state = test_state();
        state.apply_hook_event(100, "conv-a", Some("/projects/coach"));
        state.apply_hook_event(200, "conv-b", Some("/projects/coach"));

        assert_eq!(state.sessions.len(), 2);
        assert_eq!(state.sessions.get(&100).unwrap().current_session_id, "conv-a");
        assert_eq!(state.sessions.get(&200).unwrap().current_session_id, "conv-b");
    }

    // ── log() ───────────────────────────────────────────────────────────

    #[test]
    fn log_adds_entries_to_session() {
        let mut state = test_state();
        state.apply_hook_event(1, "s", None);
        state.log(1, "PostToolUse", "observed", None);
        state.log(1, "Stop", "blocked", Some("priorities".into()));

        let activity = &state.sessions.get(&1).unwrap().activity;
        assert_eq!(activity.len(), 2);
        assert_eq!(activity[0].action, "observed");
        assert_eq!(activity[1].detail, Some("priorities".into()));
    }

    #[test]
    fn log_for_unknown_pid_is_silent_noop() {
        let mut state = test_state();
        state.log(9999, "PostToolUse", "observed", None);
        assert!(state.sessions.is_empty());
    }

    #[test]
    fn log_is_capped_per_session() {
        let mut state = test_state();
        state.apply_hook_event(1, "s", None);
        for i in 0..SESSION_ACTIVITY_CAP + 10 {
            state.log(1, "PostToolUse", &format!("entry-{i}"), None);
        }
        let activity = &state.sessions.get(&1).unwrap().activity;
        assert_eq!(activity.len(), SESSION_ACTIVITY_CAP);
        assert_eq!(activity[0].action, "entry-10");
        assert_eq!(
            activity[SESSION_ACTIVITY_CAP - 1].action,
            format!("entry-{}", SESSION_ACTIVITY_CAP + 9),
        );
    }

    /// Property: chatty session never evicts a quiet session's history.
    #[test]
    fn busy_session_does_not_evict_quiet_session() {
        let mut state = test_state();
        state.apply_hook_event(1, "quiet", None);
        state.apply_hook_event(2, "busy", None);
        state.log(1, "PostToolUse", "first", Some("Read".into()));

        for i in 0..SESSION_ACTIVITY_CAP * 3 {
            state.log(2, "PostToolUse", &format!("noise-{i}"), None);
        }

        let quiet = &state.sessions.get(&1).unwrap().activity;
        assert_eq!(quiet.len(), 1, "quiet session keeps its only entry");
        assert_eq!(quiet[0].action, "first");
    }

    // ── snapshot ────────────────────────────────────────────────────────

    /// Within the active bucket the order is by started_at descending
    /// (newest session on top), and it must be stable as last_event ticks.
    /// This is the regression test for "sessions keep swapping place".
    #[test]
    fn snapshot_sort_is_stable_within_active_bucket() {
        use chrono::Duration;

        let mut state = test_state();
        state.apply_hook_event(1, "s1", None);
        state.apply_hook_event(2, "s2", None);
        state.apply_hook_event(3, "s3", None);

        let now = Utc::now();
        // All three are active. started_at: pid 1 oldest, pid 2 middle, pid 3 newest.
        state.sessions.get_mut(&1).unwrap().started_at = now - Duration::seconds(300);
        state.sessions.get_mut(&2).unwrap().started_at = now - Duration::seconds(200);
        state.sessions.get_mut(&3).unwrap().started_at = now - Duration::seconds(100);
        // last_event jitters: pid 1 most recent, pid 3 oldest. Old sort would
        // have produced [1, 2, 3]; new sort must ignore last_event
        // within a bucket and use started_at desc.
        state.sessions.get_mut(&1).unwrap().last_event_time = now;
        state.sessions.get_mut(&2).unwrap().last_event_time = now - Duration::seconds(5);
        state.sessions.get_mut(&3).unwrap().last_event_time = now - Duration::seconds(10);

        let snap = state.snapshot();
        let pids: Vec<u32> = snap.sessions.iter().map(|s| s.pid).collect();
        assert_eq!(pids, vec![3, 2, 1], "newest-started first, stable");
    }

    /// Idle sessions sit below active sessions regardless of when they started.
    #[test]
    fn snapshot_sort_demotes_idle_sessions() {
        use chrono::Duration;

        let mut state = test_state();
        state.apply_hook_event(10, "active_old", None);
        state.apply_hook_event(20, "idle_new", None);

        let now = Utc::now();
        // idle started recently but hasn't seen events for an hour.
        state.sessions.get_mut(&20).unwrap().started_at = now - Duration::seconds(60);
        state.sessions.get_mut(&20).unwrap().last_event_time = now - Duration::seconds(60 * 60);
        // active started long ago but is still active.
        state.sessions.get_mut(&10).unwrap().started_at = now - Duration::seconds(3000);
        state.sessions.get_mut(&10).unwrap().last_event_time = now;

        let snap = state.snapshot();
        let pids: Vec<u32> = snap.sessions.iter().map(|s| s.pid).collect();
        assert_eq!(pids, vec![10, 20], "active outranks idle");
    }

    /// activity_bucket is the only thing that can change a session's rank,
    /// so it must be a clean step at exactly SESSION_ACTIVE_WINDOW_SECS.
    #[test]
    fn activity_bucket_step_at_threshold() {
        use chrono::Duration;
        let now = Utc::now();
        let just_inside = now - Duration::seconds(SESSION_ACTIVE_WINDOW_SECS - 1);
        let just_outside = now - Duration::seconds(SESSION_ACTIVE_WINDOW_SECS);
        assert_eq!(activity_bucket(just_inside, now), 0);
        assert_eq!(activity_bucket(just_outside, now), 1);
    }

    #[test]
    fn snapshot_token_status_reflects_user_token() {
        let mut state = test_state();
        state.api_tokens.insert("google".into(), "gk-user".into());
        let snap = state.snapshot();
        let google_status = snap.token_status.get("google").unwrap();
        assert_eq!(google_status.source, TokenSource::User);
        assert!(google_status.env_var.is_none());
    }

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

    #[test]
    fn snapshot_exposes_pid_and_current_session_id() {
        let mut state = test_state();
        state.apply_hook_event(77, "conv-X", Some("/Users/foo/projects/coach"));
        {
            let s = state.sessions.get_mut(&77).unwrap();
            s.tool_counts.insert("Read".into(), 3);
        }
        let snap = state.snapshot();
        assert_eq!(snap.sessions[0].pid, 77);
        assert_eq!(snap.sessions[0].session_id, "conv-X");
        assert_eq!(snap.sessions[0].display_name, "coach");
        assert_eq!(snap.sessions[0].tool_counts.get("Read"), Some(&3));
    }

    // ── from_settings / to_settings roundtrip ───────────────────────────

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

        let state = CoachState {
            sessions: HashMap::new(),
            session_id_to_pid: HashMap::new(),
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

    #[test]
    fn to_settings_excludes_transient_state() {
        let mut state = test_state();
        state.apply_hook_event(1, "s", Some("/tmp"));
        state.log(1, "PostToolUse", "observed", None);

        let settings = state.to_settings();
        let json = serde_json::to_value(&settings).unwrap();

        assert!(json.get("sessions").is_none());
        assert!(json.get("activity").is_none());
    }

    // ── derive_display_name ────────────────────────────────────────────

    #[test]
    fn display_name_normal_path() {
        let history = vec!["/Users/foo/projects/coach".into()];
        assert_eq!(derive_display_name(&history, 12345), "coach");
    }

    #[test]
    fn display_name_generic_last_segment() {
        let history = vec!["/Users/foo/projects/coach/src".into()];
        assert_eq!(derive_display_name(&history, 12345), "coach/src");
    }

    /// With no cwd, fall back to a `pid:N` label so the user can still
    /// distinguish multiple unconfigured windows.
    #[test]
    fn display_name_fallback_to_pid() {
        let history: Vec<String> = vec![];
        assert_eq!(derive_display_name(&history, 12345), "pid:12345");
    }

    #[test]
    fn display_name_picks_deepest_cwd() {
        let history = vec![
            "/Users/foo/projects".into(),
            "/Users/foo/projects/coach/src".into(),
            "/Users/foo".into(),
        ];
        assert_eq!(derive_display_name(&history, 12345), "coach/src");
    }

    // ── cwd_history through apply_hook_event ───────────────────────────

    #[test]
    fn apply_hook_event_appends_cwd_history_on_change() {
        let mut state = test_state();
        state.apply_hook_event(1, "s", Some("/Users/foo/projects/coach"));
        state.apply_hook_event(1, "s", Some("/Users/foo/projects/coach/src"));

        let sess = state.sessions.get(&1).unwrap();
        assert_eq!(
            sess.cwd_history,
            vec![
                "/Users/foo/projects/coach",
                "/Users/foo/projects/coach/src",
            ]
        );
        assert_eq!(sess.display_name, "coach/src");
    }

    #[test]
    fn apply_hook_event_does_not_duplicate_cwd_history() {
        let mut state = test_state();
        state.apply_hook_event(1, "s", Some("/Users/foo/projects/coach"));
        state.apply_hook_event(1, "s", Some("/Users/foo/projects/coach/src"));
        state.apply_hook_event(1, "s", Some("/Users/foo/projects/coach"));

        let sess = state.sessions.get(&1).unwrap();
        assert_eq!(sess.cwd_history.len(), 2);
    }

    /// cwd_history persists across /clear because it describes the
    /// window, not the conversation.
    #[test]
    fn cwd_history_persists_across_clear() {
        let mut state = test_state();
        state.apply_hook_event(1, "old", Some("/Users/foo/projects/coach"));
        state.apply_hook_event(1, "new", Some("/Users/foo/projects/coach"));
        let sess = state.sessions.get(&1).unwrap();
        assert_eq!(sess.cwd_history, vec!["/Users/foo/projects/coach"]);
    }

    // ── register_discovered_pid ─────────────────────────────────────────

    #[test]
    fn register_discovered_pid_creates_placeholder() {
        use chrono::Duration;
        let mut state = test_state();
        let started = Utc::now() - Duration::hours(1);
        let created = state.register_discovered_pid(12345, Some("/projects/foo"), started);

        assert!(created);
        let sess = state.sessions.get(&12345).unwrap();
        assert_eq!(sess.event_count, 0);
        assert_eq!(sess.pid, 12345);
        assert_eq!(sess.started_at, started);
        assert_eq!(sess.cwd, Some("/projects/foo".into()));
        // No conversation yet — first hook will set this.
        assert_eq!(sess.current_session_id, "");
    }

    /// If a hook beat the scanner, the discovered PID is already in
    /// state and register_discovered_pid is a no-op (returns false).
    #[test]
    fn register_discovered_pid_is_noop_when_pid_known() {
        let mut state = test_state();
        state.apply_hook_event(42, "from-hook", None);
        let event_count_before = state.sessions.get(&42).unwrap().event_count;

        let created = state.register_discovered_pid(42, Some("/anywhere"), Utc::now());

        assert!(!created);
        assert_eq!(
            state.sessions.get(&42).unwrap().event_count,
            event_count_before,
            "discovered should not stomp on hook-populated state"
        );
    }

    // ── remove_dead_pids ────────────────────────────────────────────────

    #[test]
    fn remove_dead_pids_removes_only_unknown_pids() {
        let mut state = test_state();
        state.apply_hook_event(1, "alive-1", None);
        state.apply_hook_event(2, "dead-2", None);
        state.apply_hook_event(3, "dead-3", None);

        let live: HashSet<u32> = [1].into_iter().collect();
        let dead = state.remove_dead_pids(&live);

        let mut dead_sorted = dead.clone();
        dead_sorted.sort();
        assert_eq!(dead_sorted, vec![2, 3]);
        assert!(state.sessions.contains_key(&1));
        assert!(!state.sessions.contains_key(&2));
        assert!(!state.sessions.contains_key(&3));
    }

    /// Dead PIDs should also be evicted from the session_id cache to
    /// prevent unbounded growth.
    #[test]
    fn remove_dead_pids_clears_cache_entries_for_dead_pids() {
        let mut state = test_state();
        state.apply_hook_event(1, "alive", None);
        state.apply_hook_event(2, "dead", None);
        assert!(state.session_id_to_pid.contains_key("dead"));

        let live: HashSet<u32> = [1].into_iter().collect();
        state.remove_dead_pids(&live);

        assert!(state.session_id_to_pid.contains_key("alive"));
        assert!(!state.session_id_to_pid.contains_key("dead"));
    }

    #[test]
    fn remove_dead_pids_keeps_all_when_all_alive() {
        let mut state = test_state();
        state.apply_hook_event(1, "a", None);
        state.apply_hook_event(2, "b", None);

        let live: HashSet<u32> = [1, 2].into_iter().collect();
        let dead = state.remove_dead_pids(&live);

        assert!(dead.is_empty());
        assert_eq!(state.sessions.len(), 2);
    }
}
