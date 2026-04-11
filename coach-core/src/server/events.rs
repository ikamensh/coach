//! Domain `SessionEvent` model and shared handlers.
//!
//! Each of Claude Code, Codex, and Cursor hits Coach over its own set of
//! URL paths with its own payload shape, but the domain concepts are the
//! same: "a session started", "the user submitted a prompt", "a tool
//! finished". This module defines that domain vocabulary (`SessionEvent`)
//! and one entry point, `dispatch`, that handles every variant. The
//! three transport modules (`claude`, `codex`, `cursor`) translate raw
//! payloads into `SessionEvent`s and call `dispatch`.

use std::time::Duration;

use axum::Json;
use serde_json::{json, Value};

use super::{observer, rules, HookServerState};
use crate::coach::{ChainedStopInput, LlmCoach, NameSessionInput, StopContext};
use crate::settings::EngineMode;
use crate::state::{CoachMode, AppState, SessionClient, SessionState};

/// Which coding-agent CLI / IDE produced a hook. Domain handlers use
/// this to tag sessions so the frontend renders the right icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionSource {
    ClaudeCode,
    Codex,
    Cursor,
}

impl SessionSource {
    fn client(self) -> SessionClient {
        match self {
            SessionSource::ClaudeCode => SessionClient::Claude,
            SessionSource::Codex => SessionClient::Codex,
            SessionSource::Cursor => SessionClient::Cursor,
        }
    }
}



/// Domain event: transport-agnostic description of what just happened
/// in a coding session. Variants hold only what the current handlers
/// actually consume.
pub(crate) enum SessionEvent {
    SessionStarted {
        session_id: String,
        cwd: Option<String>,
        /// `"startup"`/`"resume"`/`"clear"`/`"compact"` from Claude Code,
        /// `"cursor"`/`"codex"` (or whatever the payload says) from the
        /// IDE adapters. Logged verbatim as the session's first activity.
        source_label: String,
    },
    UserPromptSubmitted {
        session_id: String,
        cwd: Option<String>,
        prompt: Option<String>,
    },
    PermissionRequested {
        session_id: String,
        cwd: Option<String>,
        tool_name: String,
    },
    /// Claude Code / Codex `PreToolUse`. Today we only act when
    /// `tool_name == "Agent"`, but the variant itself stays
    /// tool-agnostic so Cursor can grow into it if it ever gains a
    /// pre-tool hook.
    ToolStarting {
        session_id: String,
        cwd: Option<String>,
        tool_name: String,
    },
    ToolCompleted {
        session_id: String,
        cwd: Option<String>,
        tool_name: String,
        tool_input: Value,
    },
    StopRequested {
        session_id: String,
        cwd: Option<String>,
        stop_reason: Option<String>,
    },
}

/// One entry point for every hook. Parses nothing — the transport has
/// already built a `SessionEvent` — and routes to the matching handler.
/// Returns the raw JSON the agent will see on the wire: `{}` for
/// passthrough, `{hookSpecificOutput: ...}` for permission / post-tool-use
/// responses, `{decision: "block", reason: ...}` for blocked stops.
pub(crate) async fn dispatch(
    state: &HookServerState,
    pid: u32,
    source: SessionSource,
    event: SessionEvent,
) -> Json<Value> {
    match event {
        SessionEvent::SessionStarted {
            session_id,
            cwd,
            source_label,
        } => on_session_started(state, pid, source, session_id, cwd, source_label).await,
        SessionEvent::UserPromptSubmitted {
            session_id,
            cwd,
            prompt,
        } => on_user_prompt_submitted(state, pid, source, session_id, cwd, prompt).await,
        SessionEvent::PermissionRequested {
            session_id,
            cwd,
            tool_name,
        } => on_permission_requested(state, pid, source, session_id, cwd, tool_name).await,
        SessionEvent::ToolStarting {
            session_id,
            cwd,
            tool_name,
        } => on_tool_starting(state, pid, source, session_id, cwd, tool_name).await,
        SessionEvent::ToolCompleted {
            session_id,
            cwd,
            tool_name,
            tool_input,
        } => on_tool_completed(state, pid, source, session_id, cwd, tool_name, tool_input).await,
        SessionEvent::StopRequested {
            session_id,
            cwd,
            stop_reason,
        } => on_stop_requested(state, pid, source, session_id, cwd, stop_reason).await,
    }
}

fn passthrough() -> Json<Value> {
    Json(json!({}))
}

/// Session-title cadence: one call early so a useful title shows up
/// quickly, then every `TITLE_INTERVAL_EVENTS` after that. Pure function
/// so the rule is testable without spinning up the server.
const TITLE_FIRST_EVENT: usize = 5;
const TITLE_INTERVAL_EVENTS: usize = 15;

fn should_request_title(event_count: usize) -> bool {
    event_count == TITLE_FIRST_EVENT
        || (event_count > TITLE_FIRST_EVENT && event_count.is_multiple_of(TITLE_INTERVAL_EVENTS))
}

/// Adopt the session for `session_id` and tag it with the source's
/// client. Runs inside an existing `state::mutate` closure so the
/// snapshot goes out once with the right icon — no flicker.
fn adopt<'a>(
    coach: &'a mut AppState,
    pid: u32,
    sid: &str,
    cwd: Option<&str>,
    source: SessionSource,
) -> &'a mut SessionState {
    let sess = coach.sessions.apply_hook_event(pid, sid, cwd);
    sess.client = source.client();
    sess
}

/// `SessionStart` — log it and move on. `apply_hook_event` takes care
/// of evicting the previous conversation's entry if `/clear` happened
/// in the same window.
async fn on_session_started(
    state: &HookServerState,
    pid: u32,
    source: SessionSource,
    session_id: String,
    cwd: Option<String>,
    source_label: String,
) -> Json<Value> {
    crate::state::mutate(&state.app, &state.emitter, |coach| {
        adopt(coach, pid, &session_id, cwd.as_deref(), source);
        coach
            .sessions
            .log(&session_id, "SessionStart", &source_label, None);
    })
    .await;
    passthrough()
}

/// `UserPromptSubmit` — remember the prompt and record a "user spoke"
/// entry. Truncated for the activity chip tooltip; full text stays in
/// coach memory so the observer sees the whole turn.
async fn on_user_prompt_submitted(
    state: &HookServerState,
    pid: u32,
    source: SessionSource,
    session_id: String,
    cwd: Option<String>,
    prompt: Option<String>,
) -> Json<Value> {
    let detail = prompt.as_ref().map(|p| {
        const MAX: usize = 200;
        if p.chars().count() > MAX {
            let truncated: String = p.chars().take(MAX).collect();
            format!("{truncated}…")
        } else {
            p.clone()
        }
    });
    crate::state::mutate(&state.app, &state.emitter, |coach| {
        let sess = adopt(coach, pid, &session_id, cwd.as_deref(), source);
        sess.coach.memory.last_user_prompt = prompt;
        coach
            .sessions
            .log(&session_id, "UserPromptSubmit", "user spoke", detail);
    })
    .await;
    passthrough()
}

/// `PermissionRequest` — in Away mode we auto-approve everything so the
/// user can walk away; in Present mode we pass through and let the
/// agent's normal UI prompt.
async fn on_permission_requested(
    state: &HookServerState,
    pid: u32,
    source: SessionSource,
    session_id: String,
    cwd: Option<String>,
    tool_name: String,
) -> Json<Value> {
    let mode = crate::state::mutate(&state.app, &state.emitter, |coach| {
        let sess = adopt(coach, pid, &session_id, cwd.as_deref(), source);
        let mode = sess.mode;
        let action = if mode == CoachMode::Away {
            "auto-approved"
        } else {
            "passed through"
        };
        coach
            .sessions
            .log(&session_id, "PermissionRequest", action, Some(tool_name));
        mode
    })
    .await;

    if mode == CoachMode::Away {
        Json(json!({
            "hookSpecificOutput": { "decision": { "behavior": "allow" } }
        }))
    } else {
        passthrough()
    }
}

/// `PreToolUse` — today the only tool we special-case is `Agent`, where
/// we track how many Agent calls are in flight so the snapshot can show
/// "is the sub-agent still running?".
async fn on_tool_starting(
    state: &HookServerState,
    pid: u32,
    source: SessionSource,
    session_id: String,
    cwd: Option<String>,
    tool_name: String,
) -> Json<Value> {
    if tool_name != "Agent" {
        return passthrough();
    }
    crate::state::mutate(&state.app, &state.emitter, |coach| {
        let sess = adopt(coach, pid, &session_id, cwd.as_deref(), source);
        sess.record_agent_start();
        coach
            .sessions
            .log(&session_id, "PreToolUse", "agent starting", None);
    })
    .await;
    passthrough()
}

/// `PostToolUse` — record the tool in session counters, check rules,
/// fire the observer queue if LLM mode is active, schedule session
/// naming at the right cadence. The busiest hook by far.
async fn on_tool_completed(
    state: &HookServerState,
    pid: u32,
    source: SessionSource,
    session_id: String,
    cwd: Option<String>,
    tool_name: String,
    tool_input: Value,
) -> Json<Value> {
    let (rule_message, intervention_to_deliver, namer_input) =
        crate::state::mutate(&state.app, &state.emitter, |coach| {
            let event_count = {
                let sess = adopt(coach, pid, &session_id, cwd.as_deref(), source);
                sess.record_tool(&tool_name);
                if tool_name == "Agent" {
                    sess.record_agent_end();
                }
                sess.event_count
            };

            let rule_message =
                rules::check_rules(&coach.config.rules, &tool_name, &tool_input);

            if let Some(ref msg) = rule_message {
                coach.sessions.log(
                    &session_id,
                    "PostToolUse",
                    "rule triggered",
                    Some(format!("{}: {}", tool_name, msg)),
                );
            } else {
                coach.sessions.log(
                    &session_id,
                    "PostToolUse",
                    "observed",
                    Some(tool_name.clone()),
                );
            }

            let (pending, muted) = {
                let sess = coach
                    .sessions
                    .get_mut(&session_id)
                    .expect("apply_hook_event populated");
                (
                    sess.coach.memory.pending_intervention.take(),
                    sess.coach.intervention_muted,
                )
            };
            let intervention_to_deliver = match pending {
                Some(msg) if !muted => {
                    coach.sessions.log(
                        &session_id,
                        "Intervention",
                        "delivered",
                        Some(msg.clone()),
                    );
                    Some(msg)
                }
                Some(msg) => {
                    coach
                        .sessions
                        .log(&session_id, "Intervention", "muted", Some(msg));
                    None
                }
                None => None,
            };

            let llm_active = coach.config.coach_mode == EngineMode::Llm
                && crate::settings::OBSERVER_CAPABLE_PROVIDERS
                    .contains(&coach.config.model.provider.as_str());

            if llm_active {
                let priorities = coach.config.priorities.clone();
                let sess = coach
                    .sessions
                    .get_mut(&session_id)
                    .expect("apply_hook_event populated");
                if sess.coach.observer_tx.is_none() {
                    let (tx, rx) = tokio::sync::mpsc::channel(
                        crate::state::OBSERVER_QUEUE_CAPACITY,
                    );
                    sess.coach.observer_tx = Some(tx);
                    let coach_state = state.app.clone();
                    let emitter = state.emitter.clone();
                    let sid_for_task = session_id.clone();
                    sess.coach.observer_task = Some(tokio::spawn(async move {
                        observer::observer_consumer(coach_state, emitter, sid_for_task, rx).await;
                    }));
                }
                let item = crate::state::ObserverQueueItem {
                    priorities,
                    tool_name: tool_name.clone(),
                    tool_input: tool_input.clone(),
                    user_prompt: sess.coach.memory.last_user_prompt.clone(),
                };
                if let Err(tokio::sync::mpsc::error::TrySendError::Full(_)) =
                    sess.coach.observer_tx.as_ref().unwrap().try_send(item)
                {
                    sess.coach.observer_dropped += 1;
                    eprintln!(
                        "[coach] observer queue full (sid={session_id}, dropped={})",
                        sess.coach.observer_dropped
                    );
                }
            }

            let namer_input = if llm_active && should_request_title(event_count) {
                let sess = coach
                    .sessions
                    .get(&session_id)
                    .expect("apply_hook_event populated");
                Some(NameSessionInput {
                    priorities: coach.config.priorities.clone(),
                    cwd: sess.cwd.clone(),
                    tool_counts: sess.tool_counts.clone(),
                    last_assessment: sess.coach.memory.last_assessment.clone(),
                    session_id: Some(session_id.clone()),
                })
            } else {
                None
            };

            (rule_message, intervention_to_deliver, namer_input)
        })
        .await;

    if let Some(input) = namer_input {
        let coach_state = state.app.clone();
        let emitter = state.emitter.clone();
        let sid = session_id.clone();
        tokio::spawn(async move {
            observer::run_session_namer(coach_state, emitter, sid, input).await;
        });
    }

    let context = match (rule_message, intervention_to_deliver) {
        (Some(rule), Some(intervention)) => Some(format!("{rule}\n\n[Coach]: {intervention}")),
        (Some(rule), None) => Some(rule),
        (None, Some(intervention)) => Some(format!("[Coach]: {intervention}")),
        (None, None) => None,
    };
    match context {
        Some(msg) => Json(json!({
            "hookSpecificOutput": { "additionalContext": msg }
        })),
        None => passthrough(),
    }
}

const STOP_COOLDOWN: Duration = Duration::from_secs(15);

/// `Stop` — the biggest handler. In Present mode: pass. In Away mode,
/// three phases: (1) read session snapshot, decide whether to evaluate;
/// (2) run the LLM coach if configured; (3) fall back to the fixed
/// rules/cooldown behavior on LLM errors or in Rules mode.
///
/// Note: Stop uses top-level `decision`/`reason` fields, NOT
/// `hookSpecificOutput` — Claude Code rejects the latter on stop hooks.
async fn on_stop_requested(
    state: &HookServerState,
    pid: u32,
    source: SessionSource,
    session_id: String,
    cwd: Option<String>,
    stop_reason: Option<String>,
) -> Json<Value> {
    enum Phase1 {
        PassThrough,
        Evaluate {
            coach_mode: EngineMode,
            provider_capable: bool,
            prev_chain: crate::state::CoachChain,
            ctx: StopContext,
        },
    }
    let phase1 = crate::state::mutate(&state.app, &state.emitter, |coach| {
        let priorities = coach.config.priorities.clone();
        let provider_capable = crate::settings::OBSERVER_CAPABLE_PROVIDERS
            .contains(&coach.config.model.provider.as_str());
        let coach_mode = coach.config.coach_mode.clone();
        let sess = adopt(coach, pid, &session_id, cwd.as_deref(), source);
        sess.stop_count += 1;

        if sess.mode != CoachMode::Away {
            coach
                .sessions
                .log(&session_id, "Stop", "passed through", None);
            return Phase1::PassThrough;
        }

        let prev_chain = sess.coach.memory.chain.clone();
        let ctx = StopContext {
            priorities,
            cwd: sess.cwd.clone(),
            tool_counts: sess.tool_counts.clone(),
            stop_count: sess.stop_count,
            stop_blocked_count: sess.stop_blocked_count,
            stop_reason,
            session_id: Some(session_id.clone()),
        };
        Phase1::Evaluate {
            coach_mode,
            provider_capable,
            prev_chain,
            ctx,
        }
    })
    .await;

    let (coach_mode, provider_capable, prev_chain, ctx) = match phase1 {
        Phase1::PassThrough => return passthrough(),
        Phase1::Evaluate {
            coach_mode,
            provider_capable,
            prev_chain,
            ctx,
        } => (coach_mode, provider_capable, prev_chain, ctx),
    };

    if coach_mode == EngineMode::Llm {
        let llm_coach = LlmCoach::new(state.app.clone());
        let started = std::time::Instant::now();
        let chained = if provider_capable {
            match llm_coach
                .evaluate_stop_chained(ChainedStopInput {
                    priorities: ctx.priorities.clone(),
                    chain: prev_chain,
                    stop_reason: ctx.stop_reason.clone(),
                    session_id: ctx.session_id.clone(),
                })
                .await
            {
                Ok(result) => Some(Ok((result.decision, Some(result.chain), Some(result.usage)))),
                Err(e) => Some(Err(e)),
            }
        } else {
            None
        };

        let result = match chained {
            Some(r) => r,
            None => llm_coach.evaluate_stop(ctx).await.map(|d| (d, None, None)),
        };

        match result {
            Ok((decision, new_chain, usage)) if decision.allow => {
                let latency_ms = started.elapsed().as_millis() as u64;
                crate::state::mutate(&state.app, &state.emitter, |coach| {
                    if let Some(s) = coach.sessions.get_mut(&session_id) {
                        let u = usage.unwrap_or_default();
                        s.coach.record_success(latency_ms, u, new_chain);
                    }
                    coach
                        .sessions
                        .log(&session_id, "Stop", "allowed (LLM)", None);
                })
                .await;
                return passthrough();
            }
            Ok((decision, new_chain, usage)) => {
                let latency_ms = started.elapsed().as_millis() as u64;
                let message = crate::state::mutate(&state.app, &state.emitter, |coach| {
                    let message = decision
                        .message
                        .filter(|m| !m.trim().is_empty())
                        .unwrap_or_else(|| {
                            crate::state::away_message(&coach.config.priorities)
                        });
                    if let Some(s) = coach.sessions.get_mut(&session_id) {
                        s.last_stop_blocked = Some(std::time::Instant::now());
                        s.stop_blocked_count += 1;
                        let u = usage.unwrap_or_default();
                        s.coach.record_success(latency_ms, u, new_chain);
                    }
                    coach.sessions.log(
                        &session_id,
                        "Stop",
                        "blocked (LLM)",
                        Some(message.clone()),
                    );
                    message
                })
                .await;
                return Json(json!({
                    "decision": "block",
                    "reason": message
                }));
            }
            Err(e) => {
                eprintln!("[coach] LLM evaluate_stop failed, falling back: {e}");
                crate::state::mutate(&state.app, &state.emitter, |coach| {
                    if let Some(s) = coach.sessions.get_mut(&session_id) {
                        s.coach.record_error(&e);
                    }
                })
                .await;
            }
        }
    }

    enum Phase3 {
        Cooldown,
        Blocked(String),
    }
    let phase3 = crate::state::mutate(&state.app, &state.emitter, |coach| {
        let on_cooldown = coach
            .sessions
            .get(&session_id)
            .and_then(|s| s.last_stop_blocked)
            .is_some_and(|last| last.elapsed() < STOP_COOLDOWN);

        if on_cooldown {
            coach
                .sessions
                .log(&session_id, "Stop", "allowed (cooldown)", None);
            return Phase3::Cooldown;
        }

        if let Some(s) = coach.sessions.get_mut(&session_id) {
            s.last_stop_blocked = Some(std::time::Instant::now());
            s.stop_blocked_count += 1;
        }
        let message = crate::state::away_message(&coach.config.priorities);
        coach.sessions.log(
            &session_id,
            "Stop",
            "blocked — user away",
            Some(message.clone()),
        );
        Phase3::Blocked(message)
    })
    .await;

    match phase3 {
        Phase3::Cooldown => passthrough(),
        Phase3::Blocked(message) => Json(json!({
            "decision": "block",
            "reason": message
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cadence rule has three contracts: never fire on the first few
    /// hooks (the session has nothing to summarize), fire exactly once at
    /// the early-trigger anchor, then fire on a steady interval.
    #[test]
    fn should_request_title_cadence() {
        for n in 0..TITLE_FIRST_EVENT {
            assert!(!should_request_title(n), "fired too early at n={n}");
        }
        assert!(should_request_title(TITLE_FIRST_EVENT));
        for n in (TITLE_FIRST_EVENT + 1)..TITLE_INTERVAL_EVENTS {
            assert!(!should_request_title(n), "spurious fire at n={n}");
        }
        for k in 1..6 {
            let n = TITLE_INTERVAL_EVENTS * k;
            assert!(should_request_title(n), "missed interval at n={n}");
        }
        assert!(!should_request_title(TITLE_INTERVAL_EVENTS - 1));
        assert!(!should_request_title(TITLE_INTERVAL_EVENTS + 1));
    }
}
