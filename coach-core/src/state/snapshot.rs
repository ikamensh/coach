use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::{ActivityEntry, CoachChain, CoachMode, CoachUsage, SessionClient, SessionState};
use crate::settings::{CoachRule, EngineMode, ModelConfig};

// ── Display-name derivation ─────────────────────────────────────────────

pub(super) const GENERIC_DIR_NAMES: &[&str] = &[
    "src", "lib", "app", "test", "tests", "dist", "build",
    "node_modules", "packages", ".git", "target",
];

/// Derive a human-readable display name from the window's launch cwd.
///
/// Returns the last path segment, or `parent/last` if the last segment is a
/// generic name like "src" or "lib". Falls back to `pid:<n>` if no cwd.
pub(super) fn derive_display_name(cwd: Option<&str>, pid: u32) -> String {
    let path = match cwd {
        Some(p) if !p.is_empty() => p,
        _ => return format!("pid:{pid}"),
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

// ── Snapshot types (serialized to the frontend) ─────────────────────────

pub(super) const SESSION_ACTIVITY_CAP: usize = 200;
/// Sessions that have had any activity within this window count as "active"
/// for ordering purposes. Crossing this threshold is the only thing that
/// can cause a session to change rank, so the list stays stable while you
/// work and only occasionally demotes a session that has gone idle.
pub(super) const SESSION_ACTIVE_WINDOW_SECS: i64 = 15 * 60;

/// Two-bucket activity classifier used by the snapshot sort.
/// 0 = active (recent activity), 1 = idle.
pub(super) fn activity_bucket(last_event: DateTime<Utc>, now: DateTime<Utc>) -> u8 {
    if (now - last_event).num_seconds() < SESSION_ACTIVE_WINDOW_SECS {
        0
    } else {
        1
    }
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
    /// The directory the window was launched in. Set once on first
    /// observation (scanner or hook) and never overwritten — Claude Code
    /// can chdir mid-session, but the launch directory is the only stable
    /// label for "what is this window for".
    pub cwd: Option<String>,
    pub last_event: DateTime<Utc>,
    pub event_count: usize,
    pub started_at: DateTime<Utc>,
    pub duration_secs: u64,
    pub display_name: String,
    pub tool_counts: HashMap<String, usize>,
    pub stop_count: usize,
    pub stop_blocked_count: usize,
    pub coach_last_assessment: Option<String>,
    pub coach_last_error: Option<String>,
    /// Periodic 4-words-or-fewer topic produced by the coach LLM.
    /// `None` until the first successful title call.
    pub coach_session_title: Option<String>,
    /// Tag for the active chain backend ("empty" / "openai" / "anthropic").
    pub coach_chain_kind: String,
    /// Number of messages the coach holds in its conversation. For Anthropic
    /// this is the literal client-side history length; for OpenAI it's a
    /// counter we maintain because the Responses API only hands back an id.
    pub coach_chain_messages: usize,
    /// Successful coach LLM calls (observer + chained stop).
    pub coach_calls: usize,
    /// Failed coach LLM calls.
    pub coach_errors: usize,
    /// When the most recent successful coach call completed.
    pub coach_last_called_at: Option<DateTime<Utc>>,
    /// Wall-clock latency of the most recent successful coach call.
    pub coach_last_latency_ms: Option<u64>,
    pub coach_last_usage: Option<CoachUsage>,
    pub coach_total_usage: CoachUsage,
    /// Recent activity for the current conversation, oldest-first.
    pub activity: Vec<ActivityEntry>,
    /// Number of Agent tool calls currently in-flight.
    #[serde(default)]
    pub active_agents: usize,
    /// Which agent CLI / IDE this session belongs to. Drives the icon
    /// rendered in the frontend session list.
    #[serde(default)]
    pub client: SessionClient,
    /// True when the session's cwd is a git linked worktree.
    #[serde(default)]
    pub is_worktree: bool,
    /// Whether observer interventions are muted (display-only, not sent).
    #[serde(default)]
    pub intervention_muted: bool,
    /// Pending intervention message from the observer, not yet delivered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_intervention: Option<String>,
    /// Total interventions detected by the observer this conversation.
    #[serde(default)]
    pub intervention_count: usize,
    /// Observer queue items dropped because the bounded channel was full.
    #[serde(default)]
    pub observer_dropped: u64,
    /// The system prompt sent to the LLM on the last observer call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coach_last_system_prompt: Option<String>,
    /// The user message sent to the LLM on the last observer call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coach_last_user_message: Option<String>,
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
    pub theme: super::Theme,
    pub model: ModelConfig,
    pub token_status: HashMap<String, TokenStatus>,
    pub coach_mode: EngineMode,
    pub rules: Vec<CoachRule>,
    /// Providers that support stateful coach sessions. Frontend uses this
    /// to mark unsupported choices in the model picker.
    pub observer_capable_providers: Vec<String>,
    /// User toggle: when Coach exits cleanly, remove its hooks so other
    /// live Claude/Cursor sessions don't fail with "HTTP undefined".
    pub auto_uninstall_hooks_on_exit: bool,
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

// ── snapshot() implementation ───────────────────────────────────────────

impl super::CoachState {
    /// Snapshot for the frontend.
    pub fn snapshot(&self) -> CoachSnapshot {
        let now = Utc::now();
        let mut sessions: Vec<SessionSnapshot> = self
            .sessions
            .values()
            .map(|s| snapshot_session(s, now))
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
            auto_uninstall_hooks_on_exit: self.auto_uninstall_hooks_on_exit,
        }
    }
}

fn snapshot_session(s: &SessionState, now: DateTime<Utc>) -> SessionSnapshot {
    SessionSnapshot {
        pid: s.pid,
        session_id: s.current_session_id.clone(),
        mode: s.mode,
        cwd: s.cwd.clone(),
        last_event: s.last_event_time,
        event_count: s.event_count,
        started_at: s.started_at,
        duration_secs: (now - s.started_at).num_seconds().max(0) as u64,
        display_name: s.display_name.clone(),
        tool_counts: s.tool_counts.clone(),
        stop_count: s.stop_count,
        stop_blocked_count: s.stop_blocked_count,
        coach_last_assessment: s.coach.memory.last_assessment.clone(),
        coach_last_error: s.coach.memory.last_error.clone(),
        coach_session_title: s.coach.memory.session_title.clone(),
        coach_chain_kind: s.coach.memory.chain.kind().to_string(),
        coach_chain_messages: match &s.coach.memory.chain {
            CoachChain::History { messages } => messages.len(),
            CoachChain::ServerId { .. } => s.coach.telemetry.calls * 2,
            CoachChain::Empty => 0,
        },
        coach_calls: s.coach.telemetry.calls,
        coach_errors: s.coach.telemetry.errors,
        coach_last_called_at: s.coach.telemetry.last_called_at,
        coach_last_latency_ms: s.coach.telemetry.last_latency_ms,
        coach_last_usage: s.coach.telemetry.last_usage,
        coach_total_usage: s.coach.telemetry.total_usage,
        activity: s.activity.iter().cloned().collect(),
        active_agents: s.active_agents,
        client: s.client,
        is_worktree: s.is_worktree,
        intervention_muted: s.coach.intervention_muted,
        pending_intervention: s.coach.memory.pending_intervention.clone(),
        intervention_count: s.coach.telemetry.intervention_count,
        observer_dropped: s.coach.observer_dropped,
        coach_last_system_prompt: s.coach.memory.last_system_prompt.clone(),
        coach_last_user_message: s.coach.memory.last_user_message.clone(),
    }
}
