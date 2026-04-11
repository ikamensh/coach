use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

use crate::llm_log::LlmLogger;
#[cfg(feature = "pycoach")]
use crate::pycoach::Pycoach;
use crate::settings::{CoachRule, EngineMode, HookTarget, ModelConfig, Settings};

mod snapshot;
pub use snapshot::{
    away_message, CoachSnapshot, SessionSnapshot, TokenSource, TokenStatus,
};
use snapshot::{derive_display_name, SESSION_ACTIVITY_CAP};
#[cfg(test)]
use snapshot::{activity_bucket, SESSION_ACTIVE_WINDOW_SECS};

/// Stable conversation identifier emitted by the coding agent (Claude
/// Code, Cursor, Codex). A session lives for the duration of one
/// conversation — `/clear` mints a new one. This is the key the
/// `CoachState.sessions` map is indexed by; the OS PID is metadata.
pub type SessionId = String;

/// Key scanner-discovered placeholders under a sentinel prefix + pid
/// so multiple windows can live side-by-side before any hook fires.
/// The first hook rekeys the placeholder to the real session_id.
pub(crate) fn placeholder_key(pid: u32) -> String {
    format!("<pending:{pid}>")
}

fn is_placeholder_key(k: &str) -> bool {
    k.starts_with("<pending:")
}

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

/// Coach-specific conversation memory for one session. This is the
/// context we want to forget on `/clear`: the running chain, latest
/// interpretation, pending intervention, and the user's most recent
/// prompt.
pub struct CoachMemory {
    pub chain: CoachChain,
    pub last_assessment: Option<String>,
    pub last_error: Option<String>,
    pub session_title: Option<String>,
    pub pending_intervention: Option<String>,
    pub last_user_prompt: Option<String>,
    /// The system prompt sent to the LLM on the last observer call.
    pub last_system_prompt: Option<String>,
    /// The user message sent to the LLM on the last observer call.
    pub last_user_message: Option<String>,
}

impl CoachMemory {
    pub fn new() -> Self {
        Self {
            chain: CoachChain::Empty,
            last_assessment: None,
            last_error: None,
            session_title: None,
            pending_intervention: None,
            last_user_prompt: None,
            last_system_prompt: None,
            last_user_message: None,
        }
    }
}

/// Counters and timings for coach LLM calls in one conversation.
pub struct CoachTelemetry {
    pub calls: usize,
    pub errors: usize,
    pub last_called_at: Option<DateTime<Utc>>,
    pub last_latency_ms: Option<u64>,
    pub last_usage: Option<CoachUsage>,
    pub total_usage: CoachUsage,
    pub intervention_count: usize,
}

impl CoachTelemetry {
    pub fn new() -> Self {
        Self {
            calls: 0,
            errors: 0,
            last_called_at: None,
            last_latency_ms: None,
            last_usage: None,
            total_usage: CoachUsage::default(),
            intervention_count: 0,
        }
    }

    /// Record a successful LLM call: bump counter, update latency and usage.
    pub fn record_success(&mut self, latency_ms: u64, usage: CoachUsage) {
        self.calls += 1;
        self.last_called_at = Some(Utc::now());
        self.last_latency_ms = Some(latency_ms);
        self.last_usage = Some(usage);
        self.total_usage += usage;
    }

    /// Record a failed LLM call.
    pub fn record_error(&mut self) {
        self.errors += 1;
    }
}

pub const OBSERVER_QUEUE_CAPACITY: usize = 64;

/// All coach-specific state hanging off one live session.
pub struct SessionCoachState {
    pub memory: CoachMemory,
    pub telemetry: CoachTelemetry,
    pub observer_tx: Option<tokio::sync::mpsc::Sender<ObserverQueueItem>>,
    pub observer_task: Option<JoinHandle<()>>,
    pub observer_dropped: u64,
    /// When true, observer interventions are shown in the UI only —
    /// not sent to the coding agent via hook responses.
    pub intervention_muted: bool,
}

impl SessionCoachState {
    pub fn new() -> Self {
        Self {
            memory: CoachMemory::new(),
            telemetry: CoachTelemetry::new(),
            observer_tx: None,
            observer_task: None,
            observer_dropped: 0,
            intervention_muted: true,
        }
    }

    pub fn reset_conversation(&mut self) {
        self.memory = CoachMemory::new();
        self.telemetry = CoachTelemetry::new();
        self.observer_tx = None;
        if let Some(handle) = self.observer_task.take() {
            handle.abort();
        }
        self.observer_dropped = 0;
    }

    pub fn record_success(
        &mut self,
        latency_ms: u64,
        usage: CoachUsage,
        new_chain: Option<CoachChain>,
    ) {
        self.telemetry.record_success(latency_ms, usage);
        if let Some(chain) = new_chain {
            self.memory.chain = chain;
        }
    }

    pub fn record_error(&mut self, error: &str) {
        self.telemetry.record_error();
        self.memory.last_error = Some(error.to_string());
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
    Codex,
}

/// Per-conversation state. The owning `CoachState.sessions` map is
/// keyed by `session_id`; `pid` is metadata used for display and
/// scanner liveness checks. `/clear` mints a new `session_id`, so a
/// window that has been cleared shows up as a fresh `SessionState`.
pub struct SessionState {
    pub session_id: SessionId,
    pub pid: u32,
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
    /// Coach-specific state for the current conversation in this session.
    /// Reset on `/clear` since the new conversation has no shared context.
    pub coach: SessionCoachState,
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
    /// The session_id whose JSONL was used for bootstrap. Lets
    /// `apply_hook_event` decide whether bootstrapped tool_counts
    /// belong to the current conversation (keep) or a stale one (discard).
    pub bootstrapped_session_id: Option<String>,
}

impl SessionState {
    /// Record a completed tool invocation. Used by both PostToolUse hooks
    /// and JSONL bootstrap replay so counts are identical either way.
    pub fn record_tool(&mut self, name: &str) {
        *self.tool_counts.entry(name.to_string()).or_insert(0) += 1;
        self.event_count += 1;
    }

    pub fn record_agent_start(&mut self) {
        self.active_agents += 1;
    }

    pub fn record_agent_end(&mut self) {
        self.active_agents = self.active_agents.saturating_sub(1);
    }

    /// Discard the scanner bootstrap for this session — used when the
    /// bootstrap loaded tools from a stale JSONL that doesn't match the
    /// conversation this session actually represents.
    pub fn discard_bootstrap(&mut self) {
        self.event_count = 0;
        self.tool_counts.clear();
        self.active_agents = 0;
        self.bootstrapped = false;
        self.bootstrapped_session_id = None;
    }
}

impl Drop for SessionState {
    fn drop(&mut self) {
        if let Some(handle) = self.coach.observer_task.take() {
            handle.abort();
        }
    }
}

/// Item enqueued for the per-session observer consumer.
pub struct ObserverQueueItem {
    pub priorities: Vec<String>,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub user_prompt: Option<String>,
}

pub struct CoachState {
    /// Keyed by session_id — one entry per live conversation. `/clear`
    /// mints a new conversation and therefore a new entry; the old one
    /// is evicted when the next hook arrives for the same PID under the
    /// new session_id, or when the scanner notices the owning PID died.
    pub sessions: HashMap<SessionId, SessionState>,
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
    /// Same, for Codex CLI hooks.
    pub codex_hooks_user_enabled: bool,
    /// When set, `llm::session_send` returns this function's result instead
    /// of calling a real provider. Used by scenario replay tests.
    pub mock_session_send: Option<MockSessionSend>,
    /// When set, every call through `llm::session_send` appends a JSONL
    /// record to a per-coding-session file in the logger's run dir.
    /// Populated from `COACH_LLM_LOG_DIR` at startup; `None` disables
    /// logging entirely with no overhead.
    pub llm_logger: Option<Arc<LlmLogger>>,
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
            codex_hooks_user_enabled: settings.codex_hooks_user_enabled,
            mock_session_send: None,
            llm_logger: LlmLogger::from_env(),
            #[cfg(feature = "pycoach")]
            pycoach: None,
        }
    }

    /// Reverse lookup: find any session owned by `pid`. The map is
    /// small (single-digit sessions in practice), so a linear scan is
    /// fine.
    pub fn session_for_pid(&self, pid: u32) -> Option<&SessionState> {
        self.sessions.values().find(|s| s.pid == pid)
    }

    pub fn session_for_pid_mut(&mut self, pid: u32) -> Option<&mut SessionState> {
        self.sessions.values_mut().find(|s| s.pid == pid)
    }

    /// Map key for the session owned by `pid`, if any. Scanner
    /// placeholders live under a sentinel key, so `session_id` alone
    /// can't find them.
    pub fn session_key_for_pid(&self, pid: u32) -> Option<SessionId> {
        self.sessions
            .iter()
            .find(|(_, s)| s.pid == pid)
            .map(|(k, _)| k.clone())
    }

    pub fn set_all_modes(&mut self, mode: CoachMode) {
        self.default_mode = mode;
        for session in self.sessions.values_mut() {
            session.mode = mode;
        }
    }

    // ── Config mutation methods ────────────────────────────────────────
    //
    // Centralise "mutate field + persist" so callers (Tauri commands,
    // HTTP API handlers) don't each reimplement the same logic.

    pub fn update_priorities(&mut self, priorities: Vec<String>) {
        self.priorities = priorities;
        self.save();
    }

    pub fn update_model(&mut self, model: ModelConfig) {
        self.model = model;
        self.save();
    }

    pub fn update_api_token(&mut self, provider: &str, token: &str) {
        if token.is_empty() {
            self.api_tokens.remove(provider);
        } else {
            self.api_tokens.insert(provider.to_string(), token.to_string());
        }
        self.save();
    }

    pub fn update_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.save();
    }

    pub fn update_coach_mode(&mut self, mode: EngineMode) {
        self.coach_mode = mode;
        self.save();
    }

    pub fn update_rules(&mut self, rules: Vec<CoachRule>) {
        self.rules = rules;
        self.save();
    }

    pub fn update_auto_uninstall(&mut self, enabled: bool) {
        self.auto_uninstall_hooks_on_exit = enabled;
        self.save();
    }

    pub fn set_session_mode(&mut self, session_id: &str, mode: CoachMode) {
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.mode = mode;
        }
    }

    pub fn set_intervention_muted(&mut self, session_id: &str, muted: bool) {
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.coach.intervention_muted = muted;
        }
    }

    pub fn set_hook_enabled(&mut self, target: HookTarget, enabled: bool) {
        match target {
            HookTarget::Claude => self.hooks_user_enabled = enabled,
            HookTarget::Cursor => self.cursor_hooks_user_enabled = enabled,
            HookTarget::Codex => self.codex_hooks_user_enabled = enabled,
        }
        self.save();
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
            codex_hooks_user_enabled: self.codex_hooks_user_enabled,
        }
    }

    pub fn save(&self) {
        self.to_settings().save();
    }

    /// Session lifecycle: create, adopt scanner placeholder, or `/clear`.
    ///
    /// 1. **Entry exists under `session_id`** → touch timestamps.
    /// 2. **Placeholder exists under the PID** (scanner beat the hook)
    ///    → rekey it to `session_id`, discard bootstrap if it doesn't
    ///    match.
    /// 3. **Another entry exists under the PID with a different
    ///    session_id** (`/clear`) → evict it and create fresh.
    /// 4. **Nothing for this PID** → create a new entry.
    ///
    /// Does NOT increment event_count — callers record tool activity via
    /// `record_tool` / `record_agent_start` / `record_agent_end`, keeping
    /// counts identical whether events arrive live or via JSONL replay.
    pub fn apply_hook_event(
        &mut self,
        pid: u32,
        session_id: &str,
        cwd: Option<&str>,
    ) -> &mut SessionState {
        let now = Utc::now();

        if let Some(sess) = self.sessions.get_mut(session_id) {
            sess.pid = pid;
            sess.last_event = Instant::now();
            sess.last_event_time = now;
            adopt_cwd_if_unset(sess, cwd);
            return self.sessions.get_mut(session_id).expect("just touched");
        }

        // Any prior entry for this PID belongs to either a scanner
        // placeholder (rekey into the real session_id) or a previous
        // conversation in the same window (`/clear` → evict).
        //
        // PID 0 means the kernel resolver failed; treating it as a
        // match would let unrelated sessions stomp each other.
        let old_key: Option<SessionId> = (pid != 0)
            .then(|| self.session_key_for_pid(pid))
            .flatten();

        if let Some(old_key) = old_key {
            if is_placeholder_key(&old_key) {
                let mut sess = self.sessions.remove(&old_key).expect("just found");
                sess.session_id = session_id.to_string();
                sess.last_event = Instant::now();
                sess.last_event_time = now;
                if sess.bootstrapped_session_id.as_deref() != Some(session_id) {
                    sess.discard_bootstrap();
                }
                adopt_cwd_if_unset(&mut sess, cwd);
                self.sessions.insert(session_id.to_string(), sess);
                return self.sessions.get_mut(session_id).expect("just inserted");
            }

            // `/clear`: evict the old conversation entry.
            let old = self.sessions.remove(&old_key).expect("just found");
            if old.coach.memory.chain.kind() != "empty" || old.event_count > 0 {
                eprintln!(
                    "[coach] apply_hook_event: evicting pid {pid} \
                     (old_sid={old_key}, new_sid={session_id}, \
                     chain_kind={}, events={})",
                    old.coach.memory.chain.kind(),
                    old.event_count
                );
            }
            drop(old);
        }

        let display_name = derive_display_name(cwd, pid);
        self.sessions.insert(
            session_id.to_string(),
            SessionState {
                session_id: session_id.to_string(),
                pid,
                mode: self.default_mode,
                cwd: cwd.map(String::from),
                last_event: Instant::now(),
                last_event_time: now,
                event_count: 0,
                last_stop_blocked: None,
                started_at: now,
                display_name,
                tool_counts: HashMap::new(),
                stop_count: 0,
                stop_blocked_count: 0,
                coach: SessionCoachState::new(),
                activity: VecDeque::new(),
                active_agents: 0,
                client: SessionClient::default(),
                is_worktree: cwd.map_or(false, is_git_worktree),
                bootstrapped: false,
                bootstrapped_session_id: None,
            },
        );
        self.sessions.get_mut(session_id).expect("just inserted")
    }

    /// Append an activity entry to the session for `session_id`. Silent
    /// no-op if the session is unknown — log calls are best-effort and
    /// must never crash the hook server.
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

    /// Register a PID discovered by the file scanner. Creates a
    /// placeholder session entry (keyed under a `<pending:pid>`
    /// sentinel) if no hook has populated one yet, so a freshly-
    /// launched window appears in the UI before the user types
    /// anything. The `session_id` field stays empty until the first
    /// hook lands and rekeys the entry. Returns false if a
    /// placeholder or real session already exists for this PID.
    pub fn register_discovered_pid(
        &mut self,
        pid: u32,
        cwd: Option<&str>,
        started_at: DateTime<Utc>,
    ) -> bool {
        if self.sessions.values().any(|s| s.pid == pid) {
            return false;
        }
        let display_name = derive_display_name(cwd, pid);
        self.sessions.insert(
            placeholder_key(pid),
            SessionState {
                session_id: String::new(),
                pid,
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
                coach: SessionCoachState::new(),
                activity: VecDeque::new(),
                active_agents: 0,
                // The file scanner only walks `~/.claude/projects` so any
                // session it discovers is necessarily Claude Code.
                client: SessionClient::Claude,
                is_worktree: cwd.map_or(false, is_git_worktree),
                bootstrapped: false,
                bootstrapped_session_id: None,
            },
        );
        true
    }

    /// Tag the session for `session_id` with the given client. Used by
    /// the cursor hook handlers right after the shared `run_*` path
    /// creates the session, since those functions don't know which
    /// agent the hook came from.
    pub fn mark_client(&mut self, session_id: &str, client: SessionClient) {
        if let Some(s) = self.sessions.get_mut(session_id) {
            s.client = client;
        }
    }

    /// Remove sessions whose owning PID is not in the live set. Returns
    /// the removed session_ids.
    pub fn remove_dead_pids(&mut self, live_pids: &HashSet<u32>) -> Vec<SessionId> {
        let dead: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, s)| !live_pids.contains(&s.pid))
            .map(|(k, _)| k.clone())
            .collect();
        for key in &dead {
            self.sessions.remove(key);
        }
        dead
    }
}

pub type SharedState = Arc<RwLock<CoachState>>;

pub async fn mutate<F, R>(
    state: &SharedState,
    emitter: &Arc<dyn crate::EventEmitter>,
    f: F,
) -> R
where
    F: FnOnce(&mut CoachState) -> R,
{
    let mut s = state.write().await;
    let out = f(&mut *s);
    let snapshot = s.snapshot();
    drop(s);
    emitter.emit_state_update(&snapshot);
    out
}

/// Build a `CoachState` with empty env_tokens so tests don't depend on
/// the machine's actual environment variables. Lives at module scope so
/// other modules' test trees (e.g. `replay::tests`) can share it.
#[cfg(test)]
pub(crate) fn test_state() -> CoachState {
    CoachState {
        sessions: HashMap::new(),
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
        codex_hooks_user_enabled: false,
        mock_session_send: None,
        llm_logger: None,
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

    /// First hook mints a session keyed by session_id and records the
    /// PID as metadata. event_count starts at 0 and only record_tool
    /// moves it.
    #[test]
    fn apply_hook_event_creates_session_for_new_sid() {
        let mut state = test_state();
        state.default_mode = CoachMode::Away;

        state.apply_hook_event(42, "conv-1", Some("/tmp"));

        let sess = state.sessions.get("conv-1").unwrap();
        assert_eq!(sess.pid, 42);
        assert_eq!(sess.session_id, "conv-1");
        assert_eq!(sess.event_count, 0);
        assert_eq!(sess.mode, CoachMode::Away);
        assert_eq!(sess.cwd, Some("/tmp".into()));

        // record_tool is how event_count grows — same path as bootstrap.
        let sess = state.sessions.get_mut("conv-1").unwrap();
        sess.record_tool("Bash");
        assert_eq!(sess.event_count, 1);
        assert_eq!(sess.tool_counts.get("Bash"), Some(&1));
    }

    /// Second hook with the same session_id touches timestamps but does
    /// not change event_count. Only record_tool does that.
    #[test]
    fn apply_hook_event_touches_existing_session() {
        let mut state = test_state();
        state.apply_hook_event(42, "conv-1", Some("/a"));
        state.sessions.get_mut("conv-1").unwrap().mode = CoachMode::Away;

        state.apply_hook_event(42, "conv-1", Some("/b"));

        let sess = state.sessions.get("conv-1").unwrap();
        assert_eq!(sess.event_count, 0, "apply_hook_event doesn't touch event_count");
        assert_eq!(sess.cwd, Some("/a".into()), "launch cwd is frozen");
        assert_eq!(sess.mode, CoachMode::Away, "mode survives hook updates");
    }

    /// /clear: same PID, new session_id. Old entry is evicted, new one
    /// is created fresh — no shared state between conversations.
    #[test]
    fn apply_hook_event_evicts_old_entry_on_clear() {
        let mut state = test_state();
        state.apply_hook_event(42, "old", Some("/projects/coach"));
        {
            let s = state.sessions.get_mut("old").unwrap();
            s.mode = CoachMode::Away;
            s.record_tool("Bash");
            s.record_tool("Bash");
            s.stop_count = 3;
            s.stop_blocked_count = 2;
            s.coach.memory.chain = CoachChain::ServerId { id: "resp_old".into() };
            s.activity.push_back(ActivityEntry {
                timestamp: Utc::now(),
                hook_event: "x".into(),
                action: "y".into(),
                detail: None,
            });
        }

        state.apply_hook_event(42, "new", Some("/projects/coach"));

        assert!(!state.sessions.contains_key("old"), "old entry evicted");
        let sess = state.sessions.get("new").unwrap();
        assert_eq!(sess.session_id, "new");
        assert_eq!(sess.event_count, 0);
        assert!(sess.tool_counts.is_empty());
        assert_eq!(sess.stop_count, 0);
        assert_eq!(sess.stop_blocked_count, 0);
        assert_eq!(sess.coach.memory.chain, CoachChain::Empty);
        assert!(sess.activity.is_empty());
        assert_eq!(sess.pid, 42, "pid carries over (same window)");
        assert_eq!(sess.cwd, Some("/projects/coach".into()));
        // Default mode is Present — `/clear` shouldn't preserve the old mode.
    }

    /// First hook for a scanner-discovered placeholder rekeys it under
    /// the real session_id. started_at is preserved (the scanner read
    /// it from the session file).
    #[test]
    fn apply_hook_event_adopts_scanner_placeholder() {
        use chrono::Duration;
        let mut state = test_state();
        let scanner_started = Utc::now() - Duration::hours(2);
        state.register_discovered_pid(42, Some("/p"), scanner_started);
        assert!(state.sessions.contains_key(&placeholder_key(42)));

        state.apply_hook_event(42, "conv-X", Some("/p"));

        assert!(
            !state.sessions.contains_key(&placeholder_key(42)),
            "placeholder rekeyed"
        );
        let sess = state.sessions.get("conv-X").unwrap();
        assert_eq!(sess.session_id, "conv-X");
        assert_eq!(sess.pid, 42);
        assert_eq!(sess.event_count, 0, "no tools recorded yet");
        assert_eq!(
            sess.started_at, scanner_started,
            "scanner started_at must survive the first hook"
        );
    }

    /// Two distinct PIDs are tracked as two distinct sessions even when
    /// they share a cwd.
    #[test]
    fn distinct_pids_in_same_cwd_are_separate_sessions() {
        let mut state = test_state();
        state.apply_hook_event(100, "conv-a", Some("/projects/coach"));
        state.apply_hook_event(200, "conv-b", Some("/projects/coach"));

        assert_eq!(state.sessions.len(), 2);
        assert_eq!(state.sessions.get("conv-a").unwrap().pid, 100);
        assert_eq!(state.sessions.get("conv-b").unwrap().pid, 200);
    }

    // ── log() ───────────────────────────────────────────────────────────

    #[test]
    fn log_adds_entries_to_session() {
        let mut state = test_state();
        state.apply_hook_event(1, "s", None);
        state.log("s", "PostToolUse", "observed", None);
        state.log("s", "Stop", "blocked", Some("priorities".into()));

        let activity = &state.sessions.get("s").unwrap().activity;
        assert_eq!(activity.len(), 2);
        assert_eq!(activity[0].action, "observed");
        assert_eq!(activity[1].detail, Some("priorities".into()));
    }

    #[test]
    fn log_for_unknown_session_id_is_silent_noop() {
        let mut state = test_state();
        state.log("ghost", "PostToolUse", "observed", None);
        assert!(state.sessions.is_empty());
    }

    #[test]
    fn log_is_capped_per_session() {
        let mut state = test_state();
        state.apply_hook_event(1, "s", None);
        for i in 0..SESSION_ACTIVITY_CAP + 10 {
            state.log("s", "PostToolUse", &format!("entry-{i}"), None);
        }
        let activity = &state.sessions.get("s").unwrap().activity;
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
        state.log("quiet", "PostToolUse", "first", Some("Read".into()));

        for i in 0..SESSION_ACTIVITY_CAP * 3 {
            state.log("busy", "PostToolUse", &format!("noise-{i}"), None);
        }

        let quiet = &state.sessions.get("quiet").unwrap().activity;
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
        // All three are active. started_at: s1 oldest, s2 middle, s3 newest.
        state.sessions.get_mut("s1").unwrap().started_at = now - Duration::seconds(300);
        state.sessions.get_mut("s2").unwrap().started_at = now - Duration::seconds(200);
        state.sessions.get_mut("s3").unwrap().started_at = now - Duration::seconds(100);
        // last_event jitters: s1 most recent, s3 oldest. Old sort would
        // have produced [1, 2, 3]; new sort must ignore last_event
        // within a bucket and use started_at desc.
        state.sessions.get_mut("s1").unwrap().last_event_time = now;
        state.sessions.get_mut("s2").unwrap().last_event_time = now - Duration::seconds(5);
        state.sessions.get_mut("s3").unwrap().last_event_time = now - Duration::seconds(10);

        let snap = state.snapshot();
        let sids: Vec<&str> = snap.sessions.iter().map(|s| s.session_id.as_str()).collect();
        assert_eq!(sids, vec!["s3", "s2", "s1"], "newest-started first, stable");
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
        state.sessions.get_mut("idle_new").unwrap().started_at = now - Duration::seconds(60);
        state.sessions.get_mut("idle_new").unwrap().last_event_time =
            now - Duration::seconds(60 * 60);
        // active started long ago but is still active.
        state.sessions.get_mut("active_old").unwrap().started_at = now - Duration::seconds(3000);
        state.sessions.get_mut("active_old").unwrap().last_event_time = now;

        let snap = state.snapshot();
        let sids: Vec<&str> = snap.sessions.iter().map(|s| s.session_id.as_str()).collect();
        assert_eq!(sids, vec!["active_old", "idle_new"], "active outranks idle");
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
    fn snapshot_exposes_pid_and_session_id() {
        let mut state = test_state();
        state.apply_hook_event(77, "conv-X", Some("/Users/foo/projects/coach"));
        {
            let s = state.sessions.get_mut("conv-X").unwrap();
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
            let s = state.sessions.get_mut("s").unwrap();
            s.coach.memory.chain = CoachChain::History {
                messages: vec![
                    CoachMessage { role: CoachRole::User, content: "u1".into() },
                    CoachMessage { role: CoachRole::Assistant, content: "a1".into() },
                    CoachMessage { role: CoachRole::User, content: "u2".into() },
                    CoachMessage { role: CoachRole::Assistant, content: "a2".into() },
                ],
            };
            s.coach.memory.last_assessment = Some("looks fine".into());
            s.coach.memory.session_title = Some("auth refactor".into());
            s.coach.telemetry.calls = 2;
            s.coach.telemetry.errors = 1;
            s.coach.telemetry.last_called_at = Some(now);
            s.coach.telemetry.last_latency_ms = Some(420);
            s.coach.telemetry.last_usage = Some(usage);
            s.coach.telemetry.total_usage = CoachUsage {
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
            let s = state.sessions.get_mut("s").unwrap();
            s.coach.memory.chain = CoachChain::ServerId { id: "resp_xyz".into() };
            s.coach.telemetry.calls = 5;
        }
        let snap = state.snapshot();
        assert_eq!(snap.sessions[0].coach_chain_kind, "server_id");
        assert_eq!(snap.sessions[0].coach_chain_messages, 10);
    }

    /// Regression: /clear must not carry coach telemetry over from the
    /// previous conversation. Since `/clear` evicts the old entry and
    /// creates a fresh one, this falls out of the rekey automatically —
    /// the new entry's SessionCoachState starts empty.
    #[test]
    fn clear_starts_with_empty_coach_telemetry() {
        let mut state = test_state();
        state.apply_hook_event(9, "old", Some("/p"));
        {
            let s = state.sessions.get_mut("old").unwrap();
            s.coach.memory.chain = CoachChain::ServerId { id: "resp_old".into() };
            s.coach.memory.session_title = Some("old topic".into());
            s.coach.telemetry.calls = 7;
            s.coach.telemetry.errors = 2;
            s.coach.telemetry.last_called_at = Some(Utc::now());
            s.coach.telemetry.last_latency_ms = Some(300);
            s.coach.telemetry.last_usage = Some(CoachUsage {
                input_tokens: 50,
                output_tokens: 5,
                cached_input_tokens: 0,
            });
            s.coach.telemetry.total_usage = CoachUsage {
                input_tokens: 500,
                output_tokens: 50,
                cached_input_tokens: 0,
            };
        }
        state.apply_hook_event(9, "new", Some("/p"));
        assert!(!state.sessions.contains_key("old"), "old entry evicted");
        let s = state.sessions.get("new").unwrap();
        assert_eq!(s.coach.telemetry.calls, 0);
        assert_eq!(s.coach.telemetry.errors, 0);
        assert!(s.coach.telemetry.last_called_at.is_none());
        assert!(s.coach.telemetry.last_latency_ms.is_none());
        assert!(s.coach.telemetry.last_usage.is_none());
        assert_eq!(s.coach.telemetry.total_usage, CoachUsage::default());
        assert!(s.coach.memory.session_title.is_none());
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
            codex_hooks_user_enabled: true,
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
        assert_eq!(
            restored.codex_hooks_user_enabled,
            original.codex_hooks_user_enabled
        );
    }

    #[test]
    fn to_settings_excludes_transient_state() {
        let mut state = test_state();
        state.apply_hook_event(1, "s", Some("/tmp"));
        state.log("s", "PostToolUse", "observed", None);

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

        let sess = state.sessions.get("s").unwrap();
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

        let sess = state.sessions.get("s").unwrap();
        assert_eq!(sess.cwd, Some("/Users/foo/projects/coach".into()));
        assert_eq!(sess.display_name, "coach");
    }

    /// `/clear` mints a fresh session_id; the new conversation is a new
    /// entry with its own cwd inherited from the first hook.
    #[test]
    fn launch_cwd_captured_per_conversation() {
        let mut state = test_state();
        state.apply_hook_event(1, "old", Some("/Users/foo/projects/coach"));
        state.apply_hook_event(1, "new", Some("/Users/foo/projects/coach/src"));
        let sess = state.sessions.get("new").unwrap();
        assert_eq!(sess.cwd, Some("/Users/foo/projects/coach/src".into()));
    }

    // ── register_discovered_pid ─────────────────────────────────────────

    #[test]
    fn register_discovered_pid_creates_placeholder() {
        use chrono::Duration;
        let mut state = test_state();
        let started = Utc::now() - Duration::hours(1);
        let created = state.register_discovered_pid(12345, Some("/projects/foo"), started);

        assert!(created);
        let sess = state.session_for_pid(12345).unwrap();
        assert_eq!(sess.event_count, 0);
        assert_eq!(sess.pid, 12345);
        assert_eq!(sess.started_at, started);
        assert_eq!(sess.cwd, Some("/projects/foo".into()));
        // No conversation yet — first hook will set this.
        assert_eq!(sess.session_id, "");
    }

    /// If a hook beat the scanner, the discovered PID is already in
    /// state and register_discovered_pid is a no-op (returns false).
    #[test]
    fn register_discovered_pid_is_noop_when_pid_known() {
        let mut state = test_state();
        state.apply_hook_event(42, "from-hook", None);
        let event_count_before = state.session_for_pid(42).unwrap().event_count;

        let created = state.register_discovered_pid(42, Some("/anywhere"), Utc::now());

        assert!(!created);
        assert_eq!(
            state.session_for_pid(42).unwrap().event_count,
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
        let mut dead = state.remove_dead_pids(&live);
        dead.sort();

        assert_eq!(dead, vec!["dead-2".to_string(), "dead-3".to_string()]);
        assert!(state.sessions.contains_key("alive-1"));
        assert!(!state.sessions.contains_key("dead-2"));
        assert!(!state.sessions.contains_key("dead-3"));
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

    // ── observer queue backpressure ─────────────────────────────────────

    /// With capacity 64 and no consumer, pushing 100 items must drop the
    /// 36 that overflow and match the counter we expose in the snapshot.
    #[tokio::test]
    async fn observer_queue_drops_overflow_and_counts() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<ObserverQueueItem>(
            OBSERVER_QUEUE_CAPACITY,
        );
        let mut dropped: u64 = 0;
        for _ in 0..100 {
            let item = ObserverQueueItem {
                priorities: vec![],
                tool_name: "Bash".into(),
                tool_input: serde_json::Value::Null,
                user_prompt: None,
            };
            if let Err(tokio::sync::mpsc::error::TrySendError::Full(_)) = tx.try_send(item) {
                dropped += 1;
            }
        }
        assert_eq!(dropped, 100 - OBSERVER_QUEUE_CAPACITY as u64);
    }
}
