use axum::{
    extract::{ConnectInfo, State as AxumState},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use crate::coach::{ChainedStopInput, LlmCoach, NameSessionInput, StopContext};
use crate::settings::EngineMode;
use crate::state::{CoachMode, SharedState};
use crate::EventEmitter;

mod api;
mod codex;
mod cursor;
mod observer;
mod rules;

#[derive(Deserialize)]
pub(crate) struct HookPayload {
    session_id: Option<String>,
    #[allow(dead_code)]
    hook_event_name: Option<String>,
    tool_name: Option<String>,
    tool_input: Option<serde_json::Value>,
    /// Set by Claude Code on Stop hooks when available.
    stop_reason: Option<String>,
    /// Set by Claude Code on UserPromptSubmit hooks — the literal text
    /// the user typed.
    prompt: Option<String>,
    /// Set by Claude Code on SessionStart: "startup" | "resume" | "clear" | "compact".
    /// We treat all four the same: a new conversation in the same window.
    source: Option<String>,
    cwd: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HookResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    hook_specific_output: Option<serde_json::Value>,
}

impl HookResponse {
    pub(crate) fn passthrough() -> Self {
        Self {
            hook_specific_output: None,
        }
    }
}

/// Maps a request's TCP peer port (and session_id, used by the test
/// fake) to the owning Claude Code PID. Production wraps
/// `crate::pid_resolver::resolve_peer_pid`; tests inject a deterministic
/// hash so distinct session_ids resolve to distinct fake PIDs.
pub type PidResolver = Arc<dyn Fn(u16, &str) -> Option<u32> + Send + Sync>;

/// Walk one level up the process tree. Injected into `AppState` so tests
/// can supply a fake; production uses `pid_resolver::parent_pid`.
pub type ParentPidFn = Arc<dyn Fn(u32) -> Option<u32> + Send + Sync>;

/// True if a pid looks like a Claude Code main process. Injected so
/// tests can stage nested-Claude scenarios without spawning real
/// processes. Production wraps `pid_resolver::is_claude_process`.
pub type IsClaudeFn = Arc<dyn Fn(u32) -> bool + Send + Sync>;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) coach: SharedState,
    pub(crate) emitter: Arc<dyn EventEmitter>,
    resolver: PidResolver,
    parent_pid_fn: ParentPidFn,
    is_claude_fn: IsClaudeFn,
}

pub(crate) fn session_id(payload: &HookPayload) -> String {
    payload
        .session_id
        .clone()
        .unwrap_or_else(|| "unknown".into())
}

pub(crate) fn emit_update(emitter: &dyn EventEmitter, coach: &crate::state::CoachState) {
    emitter.emit_state_update(&coach.snapshot());
}

/// Resolve a hook to its owning PID. Cache lookup first, then the
/// configured resolver (lsof in production, hash-of-sid in tests).
/// Returns None if the resolver fails — the caller should drop the
/// event from session-list bookkeeping rather than create a phantom row.
///
/// When the raw PID isn't a known session, walks up the parent chain.
/// This handles command-type hooks where the TCP peer is the shim's
/// curl process, not Claude Code.
///
/// Two compatibility rules make the walk safe when a Claude Code runs
/// under another Claude Code (`claude -p` from a Bash tool call):
///
/// 1. A known ancestor is only returned if its current session_id is
///    empty or matches `sid`. Otherwise the walk would stomp an
///    unrelated conversation via `apply_hook_event`'s /clear branch.
/// 2. An unknown ancestor that looks like a Claude Code main process
///    (detected via `is_claude_fn`) short-circuits the walk — it's
///    almost certainly the spawned session that the scanner hasn't
///    registered yet.
async fn resolve_pid(state: &AppState, sid: &str, peer_port: u16) -> Option<u32> {
    {
        let coach = state.coach.read().await;
        if let Some(&pid) = coach.session_id_to_pid.get(sid) {
            return Some(pid);
        }
    }
    let raw_pid = (state.resolver)(peer_port, sid)?;

    // Snapshot {pid → current_session_id} so we can classify ancestors
    // without holding the lock during the walk's I/O.
    let known: std::collections::HashMap<u32, String> = {
        let coach = state.coach.read().await;
        coach
            .sessions
            .iter()
            .map(|(pid, sess)| (*pid, sess.current_session_id.clone()))
            .collect()
    };

    if compatible(&known, raw_pid, sid) {
        eprintln!("[coach] resolved sid {sid} → pid {raw_pid} (peer port {peer_port})");
        return Some(raw_pid);
    }

    let mut candidate = raw_pid;
    for _ in 0..5 {
        let Some(ppid) = (state.parent_pid_fn)(candidate) else {
            break;
        };
        if compatible(&known, ppid, sid) {
            eprintln!(
                "[coach] resolved sid {sid} → pid {ppid} (parent of {raw_pid}, peer port {peer_port})"
            );
            return Some(ppid);
        }
        if known.contains_key(&ppid) {
            // Known ancestor with a different session — don't cross.
            // Register against the deepest non-crossing candidate;
            // better than stomping an unrelated conversation.
            eprintln!(
                "[coach] resolved sid {sid} → pid {candidate} (stopped at mismatched ancestor {ppid}, peer port {peer_port})"
            );
            return Some(candidate);
        }
        if (state.is_claude_fn)(ppid) {
            // Unknown Claude-looking ancestor: the spawned session.
            eprintln!(
                "[coach] resolved sid {sid} → pid {ppid} (claude ancestor of {raw_pid}, peer port {peer_port})"
            );
            return Some(ppid);
        }
        candidate = ppid;
    }

    // Walk exhausted without a match. Register against the deepest
    // candidate we reached — preferred over stomping a known session.
    eprintln!(
        "[coach] resolved sid {sid} → pid {candidate} (walk exhausted, peer port {peer_port})"
    );
    Some(candidate)
}

/// A known pid is "compatible" with `sid` when it has no current
/// session yet (placeholder from the scanner) or its current session
/// matches. Anything else means attributing this hook there would wipe
/// an unrelated conversation.
fn compatible(known: &std::collections::HashMap<u32, String>, pid: u32, sid: &str) -> bool {
    match known.get(&pid) {
        Some(existing) => existing.is_empty() || existing == sid,
        None => false,
    }
}

/// Shared by Claude `/hook/permission-request` and Cursor `/cursor/hook/*` permission analogues.
pub(crate) async fn run_permission_request(
    state: &AppState,
    pid: u32,
    payload: HookPayload,
) -> Json<HookResponse> {
    let sid = session_id(&payload);
    let tool = payload.tool_name.clone().unwrap_or_default();

    let mut coach = state.coach.write().await;
    let session = coach.apply_hook_event(pid, &sid, payload.cwd.as_deref());
    let mode = session.mode;

    if mode == CoachMode::Away {
        coach.log(pid, "PermissionRequest", "auto-approved", Some(tool));
        emit_update(&*state.emitter, &coach);
        Json(HookResponse {
            hook_specific_output: Some(serde_json::json!({
                "decision": { "behavior": "allow" }
            })),
        })
    } else {
        coach.log(pid, "PermissionRequest", "passed through", Some(tool));
        emit_update(&*state.emitter, &coach);
        Json(HookResponse::passthrough())
    }
}

async fn handle_permission_request(
    AxumState(state): AxumState<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(payload): Json<HookPayload>,
) -> Json<HookResponse> {
    let sid = session_id(&payload);
    let Some(pid) = resolve_pid(&state, &sid, addr.port()).await else {
        eprintln!("[coach] PermissionRequest: PID resolution failed for {sid}");
        return Json(HookResponse::passthrough());
    };
    run_permission_request(&state, pid, payload).await
}

/// SessionStart fires immediately when a new conversation begins:
/// `startup` (Claude Code launched), `resume` (`/resume`), `clear`
/// (`/clear`), or `compact` (`/compact`). All four mean the same thing
/// to us: this PID has a fresh conversation. apply_hook_event handles
/// the rest — same PID + new session_id triggers the reset path.
pub(crate) async fn run_session_start(state: &AppState, pid: u32, payload: HookPayload) -> Json<HookResponse> {
    let sid = session_id(&payload);
    let mut coach = state.coach.write().await;
    coach.apply_hook_event(pid, &sid, payload.cwd.as_deref());
    let source = payload.source.unwrap_or_else(|| "unknown".into());
    coach.log(pid, "SessionStart", &source, None);
    emit_update(&*state.emitter, &coach);

    Json(HookResponse::passthrough())
}

async fn handle_session_start(
    AxumState(state): AxumState<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(payload): Json<HookPayload>,
) -> Json<HookResponse> {
    let sid = session_id(&payload);
    let Some(pid) = resolve_pid(&state, &sid, addr.port()).await else {
        eprintln!("[coach] SessionStart: PID resolution failed for {sid}");
        return Json(HookResponse::passthrough());
    };
    run_session_start(&state, pid, payload).await
}

/// UserPromptSubmit fires whenever the user sends a turn to Claude Code.
/// Cheap, always passes through — we just record it as a major event in
/// the session timeline so the activity bar shows when the user spoke.
pub(crate) async fn run_user_prompt_submit(
    state: &AppState,
    pid: u32,
    payload: HookPayload,
) -> Json<HookResponse> {
    let sid = session_id(&payload);
    let mut coach = state.coach.write().await;
    {
        let session = coach.apply_hook_event(pid, &sid, payload.cwd.as_deref());
        session.coach.memory.last_user_prompt = payload.prompt.clone();
    }

    // Truncate the prompt for the activity log — full text is overkill for
    // a chip tooltip, and very long pastes would bloat the queue.
    let detail = payload.prompt.as_ref().map(|p| {
        const MAX: usize = 200;
        if p.chars().count() > MAX {
            let truncated: String = p.chars().take(MAX).collect();
            format!("{truncated}…")
        } else {
            p.clone()
        }
    });
    coach.log(pid, "UserPromptSubmit", "user spoke", detail);
    emit_update(&*state.emitter, &coach);

    Json(HookResponse::passthrough())
}

async fn handle_user_prompt_submit(
    AxumState(state): AxumState<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(payload): Json<HookPayload>,
) -> Json<HookResponse> {
    let sid = session_id(&payload);
    let Some(pid) = resolve_pid(&state, &sid, addr.port()).await else {
        eprintln!("[coach] UserPromptSubmit: PID resolution failed for {sid}");
        return Json(HookResponse::passthrough());
    };
    run_user_prompt_submit(&state, pid, payload).await
}

const STOP_COOLDOWN: Duration = Duration::from_secs(15);

pub(crate) async fn run_stop(state: &AppState, pid: u32, payload: HookPayload) -> Json<serde_json::Value> {
    let sid = session_id(&payload);

    // Phase 1: read context, increment stop_count, release the lock
    // before we make any (potentially slow) LLM call.
    let (coach_mode, provider_capable, prev_chain, ctx) = {
        let mut coach = state.coach.write().await;
        let priorities = coach.priorities.clone();
        let provider_capable = crate::settings::OBSERVER_CAPABLE_PROVIDERS
            .contains(&coach.model.provider.as_str());
        let coach_mode = coach.coach_mode.clone();
        let session = coach.apply_hook_event(pid, &sid, payload.cwd.as_deref());
        session.stop_count += 1;

        if session.mode != CoachMode::Away {
            coach.log(pid, "Stop", "passed through", None);
            emit_update(&*state.emitter, &coach);
            return Json(serde_json::json!({}));
        }

        let prev_chain = session.coach.memory.chain.clone();
        let session_id_owned = session.current_session_id.clone();
        let ctx = StopContext {
            priorities,
            cwd: session.cwd.clone(),
            tool_counts: session.tool_counts.clone(),
            stop_count: session.stop_count,
            stop_blocked_count: session.stop_blocked_count,
            stop_reason: payload.stop_reason.clone(),
            session_id: if session_id_owned.is_empty() {
                None
            } else {
                Some(session_id_owned)
            },
        };
        (coach_mode, provider_capable, prev_chain, ctx)
    };

    // Phase 2: LLM mode. Two paths:
    //   • Chained (OpenAI Responses or Anthropic+caching): continues the
    //     observer's chain so the model uses everything observed so far.
    //   • One-shot fallback: any other provider — sends only the digest.
    if coach_mode == EngineMode::Llm {
        let llm_coach = LlmCoach::new(state.coach.clone());
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
                let mut coach = state.coach.write().await;
                if let Some(s) = coach.sessions.get_mut(&pid) {
                    let u = usage.unwrap_or_default();
                    s.coach.record_success(latency_ms, u, new_chain);
                }
                coach.log(pid, "Stop", "allowed (LLM)", None);
                emit_update(&*state.emitter, &coach);
                return Json(serde_json::json!({}));
            }
            Ok((decision, new_chain, usage)) => {
                let latency_ms = started.elapsed().as_millis() as u64;
                let mut coach = state.coach.write().await;
                let message = decision
                    .message
                    .filter(|m| !m.trim().is_empty())
                    .unwrap_or_else(|| crate::state::away_message(&coach.priorities));
                if let Some(s) = coach.sessions.get_mut(&pid) {
                    s.last_stop_blocked = Some(std::time::Instant::now());
                    s.stop_blocked_count += 1;
                    let u = usage.unwrap_or_default();
                    s.coach.record_success(latency_ms, u, new_chain);
                }
                coach.log(pid, "Stop", "blocked (LLM)", Some(message.clone()));
                emit_update(&*state.emitter, &coach);
                return Json(serde_json::json!({
                    "decision": "block",
                    "reason": message
                }));
            }
            Err(e) => {
                eprintln!("[coach] LLM evaluate_stop failed, falling back: {e}");
                let mut coach = state.coach.write().await;
                if let Some(s) = coach.sessions.get_mut(&pid) {
                    s.coach.record_error(&e);
                }
                emit_update(&*state.emitter, &coach);
                drop(coach);
                // Fall through to rules/cooldown behavior.
            }
        }
    }

    // Phase 3: rules mode (or LLM fallback) — fixed message with cooldown escape.
    let mut coach = state.coach.write().await;
    let on_cooldown = coach
        .sessions
        .get(&pid)
        .and_then(|s| s.last_stop_blocked)
        .is_some_and(|last| last.elapsed() < STOP_COOLDOWN);

    if on_cooldown {
        coach.log(pid, "Stop", "allowed (cooldown)", None);
        emit_update(&*state.emitter, &coach);
        return Json(serde_json::json!({}));
    }

    if let Some(s) = coach.sessions.get_mut(&pid) {
        s.last_stop_blocked = Some(std::time::Instant::now());
        s.stop_blocked_count += 1;
    }
    let message = crate::state::away_message(&coach.priorities);
    coach.log(pid, "Stop", "blocked — user away", Some(message.clone()));
    emit_update(&*state.emitter, &coach);

    // Stop hooks use top-level fields, NOT hookSpecificOutput.
    Json(serde_json::json!({
        "decision": "block",
        "reason": message
    }))
}

async fn handle_stop(
    AxumState(state): AxumState<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(payload): Json<HookPayload>,
) -> Json<serde_json::Value> {
    let sid = session_id(&payload);
    let Some(pid) = resolve_pid(&state, &sid, addr.port()).await else {
        eprintln!("[coach] Stop: PID resolution failed for {sid}");
        return Json(serde_json::json!({}));
    };
    run_stop(&state, pid, payload).await
}

/// Session-title cadence: one call early so a useful title shows up
/// quickly, then every `TITLE_INTERVAL_EVENTS` after that. Pure function
/// so the rule is testable without spinning up the server.
pub(crate) const TITLE_FIRST_EVENT: usize = 5;
pub(crate) const TITLE_INTERVAL_EVENTS: usize = 15;

pub(crate) fn should_request_title(event_count: usize) -> bool {
    event_count == TITLE_FIRST_EVENT
        || (event_count > TITLE_FIRST_EVENT && event_count.is_multiple_of(TITLE_INTERVAL_EVENTS))
}

pub(crate) async fn run_pre_tool_use(
    state: &AppState,
    pid: u32,
    payload: HookPayload,
) -> Json<HookResponse> {
    let sid = session_id(&payload);
    let tool = payload.tool_name.unwrap_or_default();

    if tool == "Agent" {
        let mut coach = state.coach.write().await;
        let session = coach.apply_hook_event(pid, &sid, payload.cwd.as_deref());
        session.record_agent_start();
        coach.log(pid, "PreToolUse", "agent starting", None);
        emit_update(&*state.emitter, &coach);
    }

    Json(HookResponse::passthrough())
}

async fn handle_pre_tool_use(
    AxumState(state): AxumState<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(payload): Json<HookPayload>,
) -> Json<HookResponse> {
    let sid = session_id(&payload);
    let Some(pid) = resolve_pid(&state, &sid, addr.port()).await else {
        return Json(HookResponse::passthrough());
    };
    run_pre_tool_use(&state, pid, payload).await
}

pub(crate) async fn run_post_tool_use(
    state: &AppState,
    pid: u32,
    payload: HookPayload,
) -> Json<HookResponse> {
    let sid = session_id(&payload);
    let tool = payload.tool_name.unwrap_or_default();
    let tool_input = payload.tool_input.unwrap_or(serde_json::Value::Null);

    let namer_input;
    let rule_message;
    let mut consumer_rx = None;
    let intervention_to_deliver;
    {
        let mut coach = state.coach.write().await;
        let event_count = {
            let session = coach.apply_hook_event(pid, &sid, payload.cwd.as_deref());
            session.record_tool(&tool);
            if tool == "Agent" {
                session.record_agent_end();
            }
            session.event_count
        };

        rule_message = rules::check_rules(&coach.rules, &tool, &tool_input);

        if let Some(ref msg) = rule_message {
            coach.log(
                pid,
                "PostToolUse",
                "rule triggered",
                Some(format!("{}: {}", tool, msg)),
            );
        } else {
            coach.log(pid, "PostToolUse", "observed", Some(tool.clone()));
        }

        // Consume any pending intervention from a previous observer run.
        // Always consumed (cleared); only included in the response when unmuted.
        let (pending, muted) = {
            let session = coach.sessions.get_mut(&pid).expect("apply_hook_event populated");
            (
                session.coach.memory.pending_intervention.take(),
                session.coach.intervention_muted,
            )
        };
        intervention_to_deliver = match pending {
            Some(msg) if !muted => {
                coach.log(pid, "Intervention", "delivered", Some(msg.clone()));
                Some(msg)
            }
            Some(msg) => {
                coach.log(pid, "Intervention", "muted", Some(msg));
                None
            }
            None => None,
        };

        let llm_active = coach.coach_mode == EngineMode::Llm
            && crate::settings::OBSERVER_CAPABLE_PROVIDERS
                .contains(&coach.model.provider.as_str());

        if llm_active {
            let priorities = coach.priorities.clone();
            let session = coach.sessions.get_mut(&pid).expect("apply_hook_event populated");
            if session.coach.observer_tx.is_none() {
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                session.coach.observer_tx = Some(tx);
                consumer_rx = Some(rx);
            }
            let _ = session.coach.observer_tx.as_ref().unwrap().send(
                crate::state::ObserverQueueItem {
                    priorities,
                    tool_name: tool.clone(),
                    tool_input: tool_input.clone(),
                    user_prompt: session.coach.memory.last_user_prompt.clone(),
                },
            );
        }

        namer_input = if llm_active && should_request_title(event_count) {
            let session = coach.sessions.get(&pid).expect("apply_hook_event populated");
            let sid = session.current_session_id.clone();
            Some(NameSessionInput {
                priorities: coach.priorities.clone(),
                cwd: session.cwd.clone(),
                tool_counts: session.tool_counts.clone(),
                last_assessment: session.coach.memory.last_assessment.clone(),
                session_id: if sid.is_empty() { None } else { Some(sid) },
            })
        } else {
            None
        };

        emit_update(&*state.emitter, &coach);
    } // lock released

    // Spawn the sequential observer consumer if we just created the queue.
    if let Some(rx) = consumer_rx {
        let coach_state = state.coach.clone();
        let emitter = state.emitter.clone();
        tokio::spawn(async move {
            observer::observer_consumer(coach_state, emitter, pid, rx).await;
        });
    }

    if let Some(input) = namer_input {
        let coach_state = state.coach.clone();
        let emitter = state.emitter.clone();
        tokio::spawn(async move {
            observer::run_session_namer(coach_state, emitter, pid, input).await;
        });
    }

    // Combine rule messages and observer interventions into a single response.
    let context = match (rule_message, intervention_to_deliver) {
        (Some(rule), Some(intervention)) => Some(format!("{rule}\n\n[Coach]: {intervention}")),
        (Some(rule), None) => Some(rule),
        (None, Some(intervention)) => Some(format!("[Coach]: {intervention}")),
        (None, None) => None,
    };
    match context {
        Some(msg) => Json(HookResponse {
            hook_specific_output: Some(serde_json::json!({
                "additionalContext": msg
            })),
        }),
        None => Json(HookResponse::passthrough()),
    }
}

async fn handle_post_tool_use(
    AxumState(state): AxumState<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(payload): Json<HookPayload>,
) -> Json<HookResponse> {
    let sid = session_id(&payload);
    let Some(pid) = resolve_pid(&state, &sid, addr.port()).await else {
        eprintln!("[coach] PostToolUse: PID resolution failed for {sid}");
        return Json(HookResponse::passthrough());
    };
    run_post_tool_use(&state, pid, payload).await
}



async fn handle_get_state(
    AxumState(state): AxumState<AppState>,
) -> Json<crate::state::CoachSnapshot> {
    let coach = state.coach.read().await;
    Json(coach.snapshot())
}

async fn handle_version() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "version": env!("CARGO_PKG_VERSION") }))
}

fn build_router(
    coach: SharedState,
    emitter: Arc<dyn EventEmitter>,
    resolver: PidResolver,
    parent_pid_fn: ParentPidFn,
    is_claude_fn: IsClaudeFn,
) -> Router {
    let state = AppState {
        coach,
        emitter,
        resolver,
        parent_pid_fn,
        is_claude_fn,
    };
    Router::new()
        .route("/hook/permission-request", post(handle_permission_request))
        .route("/hook/stop", post(handle_stop))
        .route("/hook/pre-tool-use", post(handle_pre_tool_use))
        .route("/hook/post-tool-use", post(handle_post_tool_use))
        .route("/hook/user-prompt-submit", post(handle_user_prompt_submit))
        .route("/hook/session-start", post(handle_session_start))
        .route("/codex/hook/permission-request", post(codex::permission_request))
        .route("/codex/hook/stop", post(codex::stop))
        .route("/codex/hook/pre-tool-use", post(codex::pre_tool_use))
        .route("/codex/hook/post-tool-use", post(codex::post_tool_use))
        .route("/codex/hook/user-prompt-submit", post(codex::user_prompt_submit))
        .route("/codex/hook/session-start", post(codex::session_start))
        .route("/cursor/hook/session-start", post(cursor::session_start))
        .route(
            "/cursor/hook/before-submit-prompt",
            post(cursor::before_submit_prompt),
        )
        .route("/cursor/hook/before-shell", post(cursor::before_shell))
        .route("/cursor/hook/before-mcp", post(cursor::before_mcp))
        .route("/cursor/hook/after-shell", post(cursor::after_shell))
        .route("/cursor/hook/after-mcp", post(cursor::after_mcp))
        .route("/cursor/hook/after-file-edit", post(cursor::after_file_edit))
        .route("/cursor/hook/stop", post(cursor::stop))
        .route("/state", get(handle_get_state))
        .route("/version", get(handle_version))
        // CLI-facing API. Mirrors Tauri commands; same in-memory state.
        .route("/api/state", get(handle_get_state))
        .route("/api/sessions/mode", post(api::set_all_modes))
        .route("/api/sessions/{pid}/mode", post(api::set_session_mode))
        .route("/api/config/priorities", post(api::set_priorities))
        .route("/api/config/model", post(api::set_model))
        .route("/api/config/api-token", post(api::set_api_token))
        .route("/api/config/coach-mode", post(api::set_coach_mode))
        .route("/api/config/rules", post(api::set_rules))
        .with_state(state)
}

/// Build a resolver that delegates to lsof on `listen_port`. This is the
/// production resolver — it's accurate even with multiple Claude Code
/// windows in the same cwd.
pub fn lsof_resolver(listen_port: u16) -> PidResolver {
    Arc::new(move |peer_port, _sid| {
        crate::pid_resolver::resolve_peer_pid(peer_port, listen_port)
    })
}

/// Hash a hook session_id to a stable, non-zero u32. Used by the test
/// resolver and exposed so integration tests can compute the same fake
/// PID from the session_id they posted.
pub fn fake_pid_for_sid(sid: &str) -> u32 {
    let mut h: u32 = 1;
    for b in sid.bytes() {
        h = h.wrapping_mul(31).wrapping_add(b as u32);
    }
    h | 1
}

/// Test resolver: distinct session_ids resolve to distinct fake PIDs
/// without touching the OS. Used by integration tests where the client
/// and server live in the same process.
pub fn fake_resolver_from_sid() -> PidResolver {
    Arc::new(|_peer_port, sid| Some(fake_pid_for_sid(sid)))
}

/// No-op parent PID function for tests where fake PIDs have no real
/// process tree. The parent walk in `resolve_pid` simply skips.
pub fn no_parent() -> ParentPidFn {
    Arc::new(|_| None)
}

/// Default "nothing looks like Claude" stub for tests that don't care
/// about nested-Claude detection. Production uses
/// `pid_resolver::is_claude_process`.
pub fn no_claude_detect() -> IsClaudeFn {
    Arc::new(|_| false)
}

/// Router without Tauri emitter — for integration tests.
/// Tests inject a fake resolver via `fake_resolver_from_sid()` so the
/// in-process client gets distinct fake PIDs per session_id.
pub fn create_router_headless(coach: SharedState, resolver: PidResolver) -> Router {
    build_router(
        coach,
        Arc::new(crate::NoopEmitter),
        resolver,
        no_parent(),
        no_claude_detect(),
    )
}

/// Router with a custom parent-PID function — for tests that exercise
/// the parent walk (e.g. command-hook ghost session fix).
pub fn create_router_headless_with_parent(
    coach: SharedState,
    resolver: PidResolver,
    parent_pid_fn: ParentPidFn,
) -> Router {
    build_router(
        coach,
        Arc::new(crate::NoopEmitter),
        resolver,
        parent_pid_fn,
        no_claude_detect(),
    )
}

/// Router with custom parent-PID *and* Claude-detection functions — for
/// tests that exercise the nested-Claude parent walk.
pub fn create_router_headless_with_ancestry(
    coach: SharedState,
    resolver: PidResolver,
    parent_pid_fn: ParentPidFn,
    is_claude_fn: IsClaudeFn,
) -> Router {
    build_router(
        coach,
        Arc::new(crate::NoopEmitter),
        resolver,
        parent_pid_fn,
        is_claude_fn,
    )
}

/// Bind the production hook server. Pass `Some(app_handle)` from the
/// Tauri GUI path to get state-update events emitted to the frontend;
/// pass `None` for headless `coach serve` mode (CLI / VM tests / CI).
///
/// The Tauri GUI calls this from `lib.rs::run()` and panics on bind
/// failure (the GUI has no clean way to surface the error). The
/// headless `serve()` path pre-binds the listener itself via
/// `serve_on_listener` so port collisions become a non-zero CLI exit
/// with a clear error, not a panic-then-exit-0.
pub async fn start_server(
    coach: SharedState,
    emitter: Arc<dyn EventEmitter>,
    port: u16,
) {
    let addr = format!("127.0.0.1:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("Failed to bind to {}: {}", addr, e));
    eprintln!("Coach hook server listening on {}", addr);
    serve_on_listener(listener, coach, emitter, port).await;
}

/// Serve hook traffic on an already-bound listener. Used by the
/// headless `serve()` path so it can pre-bind, fail fast on port
/// collisions, and *then* announce success.
pub async fn serve_on_listener(
    listener: tokio::net::TcpListener,
    coach: SharedState,
    emitter: Arc<dyn EventEmitter>,
    port: u16,
) {
    let real_parent: ParentPidFn = Arc::new(crate::pid_resolver::parent_pid);
    let real_is_claude: IsClaudeFn = Arc::new(crate::pid_resolver::is_claude_process);
    let app = build_router(
        coach,
        emitter,
        lsof_resolver(port),
        real_parent,
        real_is_claude,
    );
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .expect("Hook server crashed");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cadence rule has three contracts: never fire on the first few
    /// hooks (the session has nothing to summarize), fire exactly once at
    /// the early-trigger anchor, then fire on a steady interval.
    #[test]
    fn should_request_title_cadence() {
        // No early firing — too little context to summarize.
        for n in 0..TITLE_FIRST_EVENT {
            assert!(!should_request_title(n), "fired too early at n={n}");
        }
        // First anchor.
        assert!(should_request_title(TITLE_FIRST_EVENT));
        // Quiet between the anchor and the next interval boundary.
        for n in (TITLE_FIRST_EVENT + 1)..TITLE_INTERVAL_EVENTS {
            assert!(!should_request_title(n), "spurious fire at n={n}");
        }
        // Interval boundaries fire.
        for k in 1..6 {
            let n = TITLE_INTERVAL_EVENTS * k;
            assert!(should_request_title(n), "missed interval at n={n}");
        }
        // Off-by-one around an interval boundary stays quiet.
        assert!(!should_request_title(TITLE_INTERVAL_EVENTS - 1));
        assert!(!should_request_title(TITLE_INTERVAL_EVENTS + 1));
    }
}
