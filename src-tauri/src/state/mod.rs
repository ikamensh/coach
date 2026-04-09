use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

#[cfg(feature = "pycoach")]
use crate::pycoach::Pycoach;
use crate::settings::{CoachRule, EngineMode, ModelConfig, Settings};

mod snapshot;
pub use snapshot::{
    away_message, CoachSnapshot, SessionSnapshot, TokenSource, TokenStatus,
};
use snapshot::{derive_display_name, SESSION_ACTIVITY_CAP};
#[cfg(test)]
use snapshot::{activity_bucket, SESSION_ACTIVE_WINDOW_SECS};

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
// Two data shapes cover all providers:
//   • ServerId: server retains conversation state, we just store an
//     opaque id (OpenAI Responses API `previous_response_id`).
//   • History: client-side turn list resent every call (Anthropic with
//     prompt caching, Google Gemini with full resend, mocks).
// `CoachChain` stays `Empty` until the first observer call.

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
    ServerId {
        id: String,
    },
    History {
        messages: Vec<CoachMessage>,
    },
}

impl CoachChain {
    /// Short tag suitable for the frontend.
    pub fn kind(&self) -> &'static str {
        match self {
            CoachChain::Empty => "empty",
            CoachChain::ServerId { .. } => "server_id",
            CoachChain::History { .. } => "history",
        }
    }
}

/// Token accounting for a single coach LLM call (or summed across calls).
/// Mirrors the three rig::completion::Usage fields we actually care about.
/// `cached_input_tokens` is non-zero only for providers with prompt caching
/// (Anthropic with `with_automatic_caching()`).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoachUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
}

impl std::ops::AddAssign for CoachUsage {
    fn add_assign(&mut self, other: Self) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
    }
}

/// Groups all coach-LLM telemetry fields for a session. Encapsulates
/// chain state, call counts, latency, usage, and the most recent
/// assessment / error / title. `record_success` and `record_error`
/// consolidate the repeated 4-field update blocks that used to be
/// scattered across server.rs.
pub struct CoachTelemetry {
    pub chain: CoachChain,
    pub calls: usize,
    pub errors: usize,
    pub last_called_at: Option<DateTime<Utc>>,
    pub last_latency_ms: Option<u64>,
    pub last_usage: Option<CoachUsage>,
    pub total_usage: CoachUsage,
    pub last_assessment: Option<String>,
    pub last_error: Option<String>,
    pub session_title: Option<String>,
    pub observer_tx: Option<tokio::sync::mpsc::UnboundedSender<ObserverQueueItem>>,
}

impl CoachTelemetry {
    pub fn new() -> Self {
        Self {
            chain: CoachChain::Empty,
            calls: 0,
            errors: 0,
            last_called_at: None,
            last_latency_ms: None,
            last_usage: None,
            total_usage: CoachUsage::default(),
            last_assessment: None,
            last_error: None,
            session_title: None,
            observer_tx: None,
        }
    }

    /// Record a successful LLM call: bump counter, update latency and
    /// usage. Optionally update the chain and assessment (observer calls
    /// set both; namer calls only set title separately).
    pub fn record_success(
        &mut self,
        latency_ms: u64,
        usage: CoachUsage,
        new_chain: Option<CoachChain>,
    ) {
        self.calls += 1;
        self.last_called_at = Some(Utc::now());
        self.last_latency_ms = Some(latency_ms);
        self.last_usage = Some(usage);
        self.total_usage += usage;
        if let Some(c) = new_chain {
            self.chain = c;
        }
    }

    /// Record a failed LLM call.
    pub fn record_error(&mut self, error: &str) {
        self.errors += 1;
        self.last_error = Some(error.to_string());
    }
}

/// Mock override for [`crate::llm::session_send`]. When set, all LLM calls
/// that go through `session_send` return this function's result instead of
/// calling a real provider. Receives `(system_prompt, user_message)`.
pub type MockSessionSend = Arc<
    dyn Fn(&str, &str) -> Result<(String, CoachUsage), String> + Send + Sync,
>;

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

/// Which agent CLI / IDE the session belongs to. Set once at session
/// creation and only switched by `mark_client`. The frontend uses this
/// to render a distinct icon per source so users can tell Claude Code
/// and Cursor sessions apart at a glance.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SessionClient {
    #[default]
    Claude,
    Cursor,
}

/// Per-window state. The owning `CoachState.sessions` map is keyed by
/// `pid` — `current_session_id` is just the label of the conversation
/// currently running in that window.
pub struct SessionState {
    pub pid: u32,
    pub current_session_id: String,
    pub mode: CoachMode,
    /// Launch directory for this window — set once on first observation
    /// (scanner or hook) and frozen. Claude Code may chdir during a
    /// session, but the launch dir is what users mean when they ask
    /// "which session is this?".
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
    /// Coach LLM telemetry: chain, call counts, usage, assessments.
    /// Reset on `/clear` since the new conversation has no shared context.
    pub telemetry: CoachTelemetry,
    pub activity: VecDeque<ActivityEntry>,
    /// Number of Agent tool calls currently in-flight. Incremented on
    /// PreToolUse(Agent), decremented on PostToolUse(Agent).
    pub active_agents: usize,
    /// Which agent CLI / IDE this session belongs to. Set once on
    /// creation (Claude by default) and only updated by `mark_client`,
    /// which the cursor handlers call after the shared `run_*` path
    /// creates the session.
    pub client: SessionClient,
    /// True when the session's cwd is a git linked worktree (not the
    /// main checkout). Detected once on first cwd observation via
    /// `git rev-parse --git-dir`.
    pub is_worktree: bool,
    /// Set to true after the scanner has bootstrapped this session from
    /// the JSONL conversation log. Prevents re-bootstrapping on every
    /// scan cycle.
    pub bootstrapped: bool,
}

/// Item enqueued for the per-session observer consumer.
pub struct ObserverQueueItem {
    pub priorities: Vec<String>,
    pub event: String,
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
    /// On clean exit, uninstall managed hooks. See `Settings`.
    pub auto_uninstall_hooks_on_exit: bool,
    /// Persistent record of "user opted in to Claude Code hooks". Survives
    /// auto-cleanup on exit so the next startup re-installs.
    pub hooks_user_enabled: bool,
    /// Same, for Cursor Agent hooks.
    pub cursor_hooks_user_enabled: bool,
    /// When set, `llm::session_send` returns this function's result instead
    /// of calling a real provider. Used by scenario replay tests.
    pub mock_session_send: Option<MockSessionSend>,
    /// Optional Python sidecar (`pycoach serve`). `None` until/unless the
    /// user opts in via `COACH_PYCOACH_BIN` / `COACH_PYCOACH_CMD`. The Arc
    /// owns a child process with `kill_on_drop`, so dropping `CoachState`
    /// at app exit also stops the sidecar.
    #[cfg(feature = "pycoach")]
    pub pycoach: Option<Arc<Pycoach>>,
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

/// Returns true when `cwd` is a git linked worktree (not the main checkout).
fn is_git_worktree(cwd: &str) -> bool {
    let Ok(out) = std::process::Command::new("git")
        .args(["-C", cwd, "rev-parse", "--git-dir"])
        .output()
    else {
        return false;
    };
    out.status.success() && String::from_utf8_lossy(&out.stdout).contains("/worktrees/")
}

/// Set the launch cwd on first observation. No-op if already set, so
/// later hooks (which may report a different cwd after the user `cd`s)
/// can't drift the window's identity.
fn adopt_cwd_if_unset(sess: &mut SessionState, cwd: Option<&str>) {
    if sess.cwd.is_some() {
        return;
    }
    if let Some(c) = cwd {
        sess.cwd = Some(c.to_string());
        sess.display_name = derive_display_name(sess.cwd.as_deref(), sess.pid);
        sess.is_worktree = is_git_worktree(c);
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
            auto_uninstall_hooks_on_exit: settings.auto_uninstall_hooks_on_exit,
            hooks_user_enabled: settings.hooks_user_enabled,
            cursor_hooks_user_enabled: settings.cursor_hooks_user_enabled,
            mock_session_send: None,
            #[cfg(feature = "pycoach")]
            pycoach: None,
        }
    }

    pub fn set_all_modes(&mut self, mode: CoachMode) {
        self.default_mode = mode;
        for session in self.sessions.values_mut() {
            session.mode = mode;
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
            auto_uninstall_hooks_on_exit: self.auto_uninstall_hooks_on_exit,
            hooks_user_enabled: self.hooks_user_enabled,
            cursor_hooks_user_enabled: self.cursor_hooks_user_enabled,
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
    ///    coach response chain reset. PID, mode, cwd, display_name
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

        let default_mode = self.default_mode;
        let now = Utc::now();

        match self.sessions.get_mut(&pid) {
            Some(sess) if sess.current_session_id == session_id => {
                sess.last_event = Instant::now();
                sess.last_event_time = now;
                sess.event_count += 1;
                adopt_cwd_if_unset(sess, cwd);
            }
            Some(sess) if sess.current_session_id.is_empty() => {
                // First hook for a scanner-discovered placeholder. Adopt
                // the conversation id without resetting started_at — the
                // scanner already populated it from the session file.
                sess.current_session_id = session_id.to_string();
                sess.last_event = Instant::now();
                sess.last_event_time = now;
                sess.event_count = 1;
                adopt_cwd_if_unset(sess, cwd);
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
                sess.active_agents = 0;
                sess.stop_count = 0;
                sess.stop_blocked_count = 0;
                sess.last_stop_blocked = None;
                sess.telemetry = CoachTelemetry::new();
                sess.activity.clear();
                adopt_cwd_if_unset(sess, cwd);
            }
            None => {
                let display_name = derive_display_name(cwd, pid);
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
                        telemetry: CoachTelemetry::new(),
                        activity: VecDeque::new(),
                        active_agents: 0,
                        client: SessionClient::default(),
                        is_worktree: cwd.map_or(false, is_git_worktree),
                        bootstrapped: false,
                    },
                );
            }
        }
        self.sessions.get_mut(&pid).expect("just inserted")
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
        let display_name = derive_display_name(cwd, pid);
        self.sessions.insert(
            pid,
            SessionState {
                pid,
                // No current conversation yet — first hook will fill this in.
                current_session_id: String::new(),
                mode: self.default_mode,
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
                telemetry: CoachTelemetry::new(),
                activity: VecDeque::new(),
                active_agents: 0,
                // The file scanner only walks `~/.claude/projects` so any
                // session it discovers is necessarily Claude Code.
                client: SessionClient::Claude,
                is_worktree: cwd.map_or(false, is_git_worktree),
                bootstrapped: false,
            },
        );
        true
    }

    /// Tag the session for `pid` with the given client. Used by the
    /// cursor hook handlers right after the shared `run_*` path creates
    /// the session, since those functions don't know which agent the
    /// hook came from.
    pub fn mark_client(&mut self, pid: u32, client: SessionClient) {
        if let Some(s) = self.sessions.get_mut(&pid) {
            s.client = client;
        }
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

pub type SharedState = Arc<RwLock<CoachState>>;

/// Build a `CoachState` with empty env_tokens so tests don't depend on
/// the machine's actual environment variables. Lives at module scope so
/// other modules' test trees (e.g. `replay::tests`) can share it.
#[cfg(test)]
pub(crate) fn test_state() -> CoachState {
    CoachState {
        sessions: HashMap::new(),
        session_id_to_pid: HashMap::new(),
        priorities: vec!["Simplicity".into()],
        port: 7700,
        theme: Theme::System,
        default_mode: CoachMode::Present,
        model: crate::settings::ModelConfig {
            provider: "google".into(),
            model: "gemini-2.5-flash".into(),
        },
        api_tokens: HashMap::new(),
        env_tokens: HashMap::new(),
        http_client: reqwest::Client::new(),
        coach_mode: crate::settings::EngineMode::Rules,
        rules: vec![],
        auto_uninstall_hooks_on_exit: true,
        hooks_user_enabled: false,
        cursor_hooks_user_enabled: false,
        mock_session_send: None,
        #[cfg(feature = "pycoach")]
        pycoach: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{EngineMode, ModelConfig, Settings};
    use std::collections::HashMap;

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
    /// preserves user-set fields like mode. The launch cwd is frozen on
    /// first observation — a later hook from a different cwd (e.g. after
    /// a `cd` in a Bash tool) must NOT drift the window's identity.
    #[test]
    fn apply_hook_event_increments_existing_session() {
        let mut state = test_state();
        state.apply_hook_event(42, "conv-1", Some("/a"));
        // Flip mode to Away to verify it's preserved.
        state.sessions.get_mut(&42).unwrap().mode = CoachMode::Away;

        state.apply_hook_event(42, "conv-1", Some("/b"));

        let sess = state.sessions.get(&42).unwrap();
        assert_eq!(sess.event_count, 2);
        assert_eq!(sess.cwd, Some("/a".into()), "launch cwd is frozen");
        assert_eq!(sess.mode, CoachMode::Away, "mode survives hook updates");
    }

    /// /clear: same PID, new session_id. Counters reset, started_at moves
    /// forward, activity is wiped, but pid/mode/cwd persist.
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
            s.telemetry.chain = CoachChain::ServerId { id: "resp_old".into() };
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
        assert_eq!(sess.telemetry.chain, CoachChain::Empty, "/clear must reset chain");
        assert!(sess.activity.is_empty());
        assert!(sess.started_at > original_started);
        // Window-scoped: preserved
        assert_eq!(sess.pid, 42);
        assert_eq!(sess.mode, CoachMode::Away, "mode is window-scoped");
        assert_eq!(sess.cwd, Some("/projects/coach".into()), "cwd preserved");
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

    /// Property: every coach_* field on SessionState is mirrored onto the
    /// snapshot, and coach_chain_messages is derived correctly from the chain
    /// (Anthropic: literal history length; OpenAI: calls * 2 since each call
    /// appends a user + assistant pair server-side).
    #[test]
    fn snapshot_mirrors_coach_telemetry_fields() {
        let mut state = test_state();
        state.apply_hook_event(7, "s", Some("/p"));
        let usage = CoachUsage { input_tokens: 100, output_tokens: 20, cached_input_tokens: 10 };
        let now = Utc::now();
        {
            let s = state.sessions.get_mut(&7).unwrap();
            s.telemetry.chain = CoachChain::History {
                messages: vec![
                    CoachMessage { role: CoachRole::User, content: "u1".into() },
                    CoachMessage { role: CoachRole::Assistant, content: "a1".into() },
                    CoachMessage { role: CoachRole::User, content: "u2".into() },
                    CoachMessage { role: CoachRole::Assistant, content: "a2".into() },
                ],
            };
            s.telemetry.last_assessment = Some("looks fine".into());
            s.telemetry.session_title = Some("auth refactor".into());
            s.telemetry.calls = 2;
            s.telemetry.errors = 1;
            s.telemetry.last_called_at = Some(now);
            s.telemetry.last_latency_ms = Some(420);
            s.telemetry.last_usage = Some(usage);
            s.telemetry.total_usage = CoachUsage {
                input_tokens: 200,
                output_tokens: 40,
                cached_input_tokens: 20,
            };
        }
        let snap = state.snapshot();
        let sess = &snap.sessions[0];
        assert_eq!(sess.coach_chain_kind, "history");
        assert_eq!(sess.coach_chain_messages, 4, "history count == messages.len()");
        assert_eq!(sess.coach_calls, 2);
        assert_eq!(sess.coach_errors, 1);
        assert_eq!(sess.coach_last_called_at, Some(now));
        assert_eq!(sess.coach_last_latency_ms, Some(420));
        assert_eq!(sess.coach_last_usage, Some(usage));
        assert_eq!(sess.coach_total_usage.input_tokens, 200);
        assert_eq!(sess.coach_last_assessment.as_deref(), Some("looks fine"));
        assert_eq!(sess.coach_session_title.as_deref(), Some("auth refactor"));

        // Round-trip through JSON: catches any serde-incompatible field
        // shapes we might introduce later.
        let json = serde_json::to_string(&snap).expect("snapshot must serialize");
        assert!(json.contains("\"coach_chain_kind\":\"history\""));
        assert!(json.contains("\"coach_chain_messages\":4"));
    }

    /// ServerId chains have no client-side message list, so we approximate
    /// the held-message count as `calls * 2` (one user + one assistant per call).
    #[test]
    fn snapshot_server_id_chain_messages_derived_from_calls() {
        let mut state = test_state();
        state.apply_hook_event(8, "s", Some("/p"));
        {
            let s = state.sessions.get_mut(&8).unwrap();
            s.telemetry.chain = CoachChain::ServerId { id: "resp_xyz".into() };
            s.telemetry.calls = 5;
        }
        let snap = state.snapshot();
        assert_eq!(snap.sessions[0].coach_chain_kind, "server_id");
        assert_eq!(snap.sessions[0].coach_chain_messages, 10);
    }

    /// Regression: /clear must wipe coach telemetry along with the chain.
    /// Otherwise the panel would show stale call counts and "12s ago" for a
    /// brand-new conversation that has yet to call the LLM.
    #[test]
    fn clear_resets_coach_telemetry() {
        let mut state = test_state();
        state.apply_hook_event(9, "old", Some("/p"));
        {
            let s = state.sessions.get_mut(&9).unwrap();
            s.telemetry.chain = CoachChain::ServerId { id: "resp_old".into() };
            s.telemetry.session_title = Some("old topic".into());
            s.telemetry.calls = 7;
            s.telemetry.errors = 2;
            s.telemetry.last_called_at = Some(Utc::now());
            s.telemetry.last_latency_ms = Some(300);
            s.telemetry.last_usage = Some(CoachUsage {
                input_tokens: 50,
                output_tokens: 5,
                cached_input_tokens: 0,
            });
            s.telemetry.total_usage = CoachUsage {
                input_tokens: 500,
                output_tokens: 50,
                cached_input_tokens: 0,
            };
        }
        state.apply_hook_event(9, "new", Some("/p"));
        let s = state.sessions.get(&9).unwrap();
        assert_eq!(s.telemetry.calls, 0);
        assert_eq!(s.telemetry.errors, 0);
        assert!(s.telemetry.last_called_at.is_none());
        assert!(s.telemetry.last_latency_ms.is_none());
        assert!(s.telemetry.last_usage.is_none());
        assert_eq!(s.telemetry.total_usage, CoachUsage::default());
        assert!(
            s.telemetry.session_title.is_none(),
            "/clear must drop the previous conversation's LLM title"
        );
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
            auto_uninstall_hooks_on_exit: false,
            hooks_user_enabled: true,
            cursor_hooks_user_enabled: true,
        };

        // Round-trip via from_settings/to_settings — exercises the full
        // pair instead of constructing CoachState by hand and silently
        // forgetting new fields.
        let restored = CoachState::from_settings(original.clone()).to_settings();

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
        assert_eq!(
            derive_display_name(Some("/Users/foo/projects/coach"), 12345),
            "coach",
        );
    }

    /// "src", "lib", "target" etc. are generic — the parent disambiguates.
    /// Without this, `~/projects/coach/src` and `~/projects/foo/src` would
    /// both display as "src".
    #[test]
    fn display_name_generic_last_segment() {
        assert_eq!(
            derive_display_name(Some("/Users/foo/projects/coach/src"), 12345),
            "coach/src",
        );
    }

    /// With no cwd, fall back to a `pid:N` label so the user can still
    /// distinguish multiple unconfigured windows.
    #[test]
    fn display_name_fallback_to_pid() {
        assert_eq!(derive_display_name(None, 12345), "pid:12345");
        assert_eq!(derive_display_name(Some(""), 12345), "pid:12345");
    }

    // ── launch cwd is frozen on first observation ──────────────────────

    /// Regression: a window's title used to drift to whatever subdirectory
    /// the most recent hook reported (e.g. `dynamic-fluttering-sprout` →
    /// `dynamic-fluttering-sprout/src-tauri` after a `cd` in Bash). The
    /// launch dir is the only stable label.
    #[test]
    fn launch_cwd_is_frozen_after_first_observation() {
        let mut state = test_state();
        state.apply_hook_event(1, "s", Some("/Users/foo/projects/coach"));
        // Subsequent hooks from a deeper cwd must NOT drift the title.
        state.apply_hook_event(1, "s", Some("/Users/foo/projects/coach/src-tauri"));
        state.apply_hook_event(1, "s", Some("/tmp/elsewhere"));

        let sess = state.sessions.get(&1).unwrap();
        assert_eq!(sess.cwd, Some("/Users/foo/projects/coach".into()));
        assert_eq!(sess.display_name, "coach");
    }

    /// If the first hook lacked a cwd (defensive — Claude Code always
    /// sends one in practice), the next hook with a cwd should adopt it.
    #[test]
    fn launch_cwd_adopted_when_first_hook_had_none() {
        let mut state = test_state();
        state.apply_hook_event(1, "s", None);
        state.apply_hook_event(1, "s", Some("/Users/foo/projects/coach"));

        let sess = state.sessions.get(&1).unwrap();
        assert_eq!(sess.cwd, Some("/Users/foo/projects/coach".into()));
        assert_eq!(sess.display_name, "coach");
    }

    /// `/clear` keeps the launch cwd — it's window-scoped, not
    /// conversation-scoped.
    #[test]
    fn launch_cwd_persists_across_clear() {
        let mut state = test_state();
        state.apply_hook_event(1, "old", Some("/Users/foo/projects/coach"));
        state.apply_hook_event(1, "new", Some("/Users/foo/projects/coach/src"));
        let sess = state.sessions.get(&1).unwrap();
        assert_eq!(sess.cwd, Some("/Users/foo/projects/coach".into()));
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
