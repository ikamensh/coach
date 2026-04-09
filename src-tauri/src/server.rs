use axum::{
    extract::{ConnectInfo, State as AxumState},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use crate::settings::EngineMode;
use crate::state::{CoachMode, SharedState};

mod api;
mod cursor;

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

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) coach: SharedState,
    pub(crate) emitter: Option<tauri::AppHandle>,
    resolver: PidResolver,
    parent_pid_fn: ParentPidFn,
}

pub(crate) fn session_id(payload: &HookPayload) -> String {
    payload
        .session_id
        .clone()
        .unwrap_or_else(|| "unknown".into())
}

pub(crate) fn emit_update(emitter: &Option<tauri::AppHandle>, coach: &crate::state::CoachState) {
    if let Some(handle) = emitter {
        use tauri::Emitter;
        let _ = handle.emit(crate::state::EVENT_STATE_UPDATED, coach.snapshot());
    }
}

/// Resolve a hook to its owning PID. Cache lookup first, then the
/// configured resolver (lsof in production, hash-of-sid in tests).
/// Returns None if the resolver fails — the caller should drop the
/// event from session-list bookkeeping rather than create a phantom row.
///
/// When the raw PID isn't a known session, walks up the parent chain.
/// This handles command-type hooks where the TCP peer is the shim's
/// curl process, not Claude Code. The parent walk finds the real
/// Claude Code PID that the scanner already discovered.
async fn resolve_pid(state: &AppState, sid: &str, peer_port: u16) -> Option<u32> {
    {
        let coach = state.coach.read().await;
        if let Some(&pid) = coach.session_id_to_pid.get(sid) {
            return Some(pid);
        }
    }
    let raw_pid = (state.resolver)(peer_port, sid)?;

    // Collect known session PIDs so we can check without holding the lock
    // during the parent walk (which may do I/O).
    let known: std::collections::HashSet<u32> = {
        let coach = state.coach.read().await;
        coach.sessions.keys().copied().collect()
    };

    if known.contains(&raw_pid) {
        eprintln!("[coach] resolved sid {sid} → pid {raw_pid} (peer port {peer_port})");
        return Some(raw_pid);
    }

    // Walk parent chain: curl → sh → Claude Code.
    let mut candidate = raw_pid;
    for _ in 0..5 {
        match (state.parent_pid_fn)(candidate) {
            Some(ppid) if known.contains(&ppid) => {
                eprintln!(
                    "[coach] resolved sid {sid} → pid {ppid} (parent of {raw_pid}, peer port {peer_port})"
                );
                return Some(ppid);
            }
            Some(ppid) => candidate = ppid,
            None => break,
        }
    }

    // No known ancestor — use raw PID (first hook before scanner runs).
    eprintln!("[coach] resolved sid {sid} → pid {raw_pid} (peer port {peer_port})");
    Some(raw_pid)
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
    *session.tool_counts.entry(tool.clone()).or_insert(0) += 1;
    let mode = session.mode;

    if mode == CoachMode::Away {
        coach.log(pid, "PermissionRequest", "auto-approved", Some(tool));
        emit_update(&state.emitter, &coach);
        Json(HookResponse {
            hook_specific_output: Some(serde_json::json!({
                "decision": { "behavior": "allow" }
            })),
        })
    } else {
        coach.log(pid, "PermissionRequest", "passed through", Some(tool));
        emit_update(&state.emitter, &coach);
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
    emit_update(&state.emitter, &coach);

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
    coach.apply_hook_event(pid, &sid, payload.cwd.as_deref());

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
    emit_update(&state.emitter, &coach);

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
            emit_update(&state.emitter, &coach);
            return Json(serde_json::json!({}));
        }

        let prev_chain = session.telemetry.chain.clone();
        let ctx = crate::llm::StopContext {
            priorities,
            cwd: session.cwd.clone(),
            tool_counts: session.tool_counts.clone(),
            stop_count: session.stop_count,
            stop_blocked_count: session.stop_blocked_count,
            stop_reason: payload.stop_reason.clone(),
        };
        (coach_mode, provider_capable, prev_chain, ctx)
    };

    // Phase 2: LLM mode. Two paths:
    //   • Chained (OpenAI Responses or Anthropic+caching): continues the
    //     observer's chain so the model uses everything observed so far.
    //   • One-shot fallback: any other provider — sends only the digest.
    if coach_mode == EngineMode::Llm {
        let started = std::time::Instant::now();
        let chained = if provider_capable {
            match crate::llm::evaluate_stop_chained(
                &state.coach,
                &ctx.priorities,
                &prev_chain,
                ctx.stop_reason.as_deref(),
            )
            .await
            {
                Ok((decision, new_chain, usage)) => {
                    Some(Ok((decision, Some(new_chain), Some(usage))))
                }
                Err(e) => Some(Err(e)),
            }
        } else {
            None
        };

        let result = match chained {
            Some(r) => r,
            None => crate::llm::evaluate_stop(&state.coach, &ctx)
                .await
                .map(|d| (d, None, None)),
        };

        match result {
            Ok((decision, new_chain, usage)) if decision.allow => {
                let latency_ms = started.elapsed().as_millis() as u64;
                let mut coach = state.coach.write().await;
                if let Some(s) = coach.sessions.get_mut(&pid) {
                    let u = usage.unwrap_or_default();
                    s.telemetry.record_success(latency_ms, u, new_chain);
                }
                coach.log(pid, "Stop", "allowed (LLM)", None);
                emit_update(&state.emitter, &coach);
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
                    s.telemetry.record_success(latency_ms, u, new_chain);
                }
                coach.log(pid, "Stop", "blocked (LLM)", Some(message.clone()));
                emit_update(&state.emitter, &coach);
                return Json(serde_json::json!({
                    "decision": "block",
                    "reason": message
                }));
            }
            Err(e) => {
                eprintln!("[coach] LLM evaluate_stop failed, falling back: {e}");
                let mut coach = state.coach.write().await;
                if let Some(s) = coach.sessions.get_mut(&pid) {
                    s.telemetry.record_error(&e);
                }
                emit_update(&state.emitter, &coach);
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
        emit_update(&state.emitter, &coach);
        return Json(serde_json::json!({}));
    }

    if let Some(s) = coach.sessions.get_mut(&pid) {
        s.last_stop_blocked = Some(std::time::Instant::now());
        s.stop_blocked_count += 1;
    }
    let message = crate::state::away_message(&coach.priorities);
    coach.log(pid, "Stop", "blocked — user away", Some(message.clone()));
    emit_update(&state.emitter, &coach);

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
        session.active_agents += 1;
        coach.log(pid, "PreToolUse", "agent starting", None);
        emit_update(&state.emitter, &coach);
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
    {
        let mut coach = state.coach.write().await;
        let event_count = {
            let session = coach.apply_hook_event(pid, &sid, payload.cwd.as_deref());
            *session.tool_counts.entry(tool.clone()).or_insert(0) += 1;
            if tool == "Agent" {
                session.active_agents = session.active_agents.saturating_sub(1);
            }
            session.event_count
        };

        rule_message = check_rules(&coach.rules, &tool, &tool_input);

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

        let llm_active = coach.coach_mode == EngineMode::Llm
            && crate::settings::OBSERVER_CAPABLE_PROVIDERS
                .contains(&coach.model.provider.as_str());

        if llm_active {
            match crate::llm::build_observer_event(&tool, &tool_input) {
                Ok(event) => {
                    let priorities = coach.priorities.clone();
                    let session = coach.sessions.get_mut(&pid).expect("apply_hook_event populated");
                    if session.telemetry.observer_tx.is_none() {
                        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                        session.telemetry.observer_tx = Some(tx);
                        consumer_rx = Some(rx);
                    }
                    let _ = session.telemetry.observer_tx.as_ref().unwrap().send(
                        crate::state::ObserverQueueItem { priorities, event },
                    );
                }
                Err(e) => {
                    eprintln!("[coach] observer event prompt failed: {e}");
                    if let Some(s) = coach.sessions.get_mut(&pid) {
                        s.telemetry.record_error(&e);
                    }
                }
            }
        }

        namer_input = if llm_active && should_request_title(event_count) {
            let session = coach.sessions.get(&pid).expect("apply_hook_event populated");
            Some(crate::llm::NameSessionInput {
                priorities: coach.priorities.clone(),
                cwd: session.cwd.clone(),
                tool_counts: session.tool_counts.clone(),
                last_assessment: session.telemetry.last_assessment.clone(),
            })
        } else {
            None
        };

        emit_update(&state.emitter, &coach);
    } // lock released

    // Spawn the sequential observer consumer if we just created the queue.
    if let Some(rx) = consumer_rx {
        let coach_state = state.coach.clone();
        let emitter = state.emitter.clone();
        tokio::spawn(async move {
            observer_consumer(coach_state, emitter, pid, rx).await;
        });
    }

    if let Some(input) = namer_input {
        let coach_state = state.coach.clone();
        let emitter = state.emitter.clone();
        tokio::spawn(async move {
            run_session_namer(coach_state, emitter, pid, input).await;
        });
    }

    match rule_message {
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

/// Sequential observer consumer for one session. Reads chain from
/// session state before each LLM call, so each observation builds on
/// the previous one. Exits when the sender is dropped (session end or
/// `/clear`).
async fn observer_consumer(
    coach: SharedState,
    emitter: Option<tauri::AppHandle>,
    pid: u32,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<crate::state::ObserverQueueItem>,
) {
    while let Some(item) = rx.recv().await {
        // Read the current chain — includes all previous observations.
        let chain = {
            let s = coach.read().await;
            s.sessions.get(&pid)
                .map(|sess| sess.telemetry.chain.clone())
                .unwrap_or_default()
        };

        let started = std::time::Instant::now();
        match crate::llm::observe_event(
            &coach,
            &item.priorities,
            &chain,
            &item.event,
        )
        .await
        {
            Ok((text, new_chain, usage)) => {
                let latency_ms = started.elapsed().as_millis() as u64;
                let mut s = coach.write().await;
                if let Some(sess) = s.sessions.get_mut(&pid) {
                    sess.telemetry.record_success(latency_ms, usage, Some(new_chain));
                    sess.telemetry.last_assessment = Some(text.clone());
                }
                s.log(pid, "Observer", "noted", Some(text));
                emit_update(&emitter, &s);
            }
            Err(e) => {
                eprintln!("[coach] observer call failed: {e}");
                let mut s = coach.write().await;
                if let Some(sess) = s.sessions.get_mut(&pid) {
                    sess.telemetry.record_error(&e);
                }
                s.log(pid, "Observer", "error", Some(e));
                emit_update(&emitter, &s);
            }
        }
    }
}

/// Periodic session-title generation. Stateless LLM call (fresh chain),
/// fire-and-forget like the observer. On success, writes the cleaned
/// title to `telemetry.session_title`. On failure, surfaces the error in
/// `telemetry.last_error` and increments `telemetry.errors` so the
/// telemetry panel reflects it — same shape as `run_observer`.
async fn run_session_namer(
    coach: SharedState,
    emitter: Option<tauri::AppHandle>,
    pid: u32,
    input: crate::llm::NameSessionInput,
) {
    match crate::llm::name_session(&coach, &input).await {
        Ok((title, usage)) => {
            let mut s = coach.write().await;
            if let Some(sess) = s.sessions.get_mut(&pid) {
                // Namer doesn't update the chain — pass 0 latency since
                // it's a stateless call and latency isn't worth tracking.
                sess.telemetry.record_success(0, usage, None);
                sess.telemetry.session_title = Some(title.clone());
            }
            s.log(pid, "Namer", "renamed", Some(title));
            emit_update(&emitter, &s);
        }
        Err(e) => {
            eprintln!("[coach] name_session failed: {e}");
            let mut s = coach.write().await;
            if let Some(sess) = s.sessions.get_mut(&pid) {
                sess.telemetry.record_error(&e);
            }
            s.log(pid, "Namer", "error", Some(e));
            emit_update(&emitter, &s);
        }
    }
}

fn check_rules(
    rules: &[crate::settings::CoachRule],
    tool_name: &str,
    tool_input: &serde_json::Value,
) -> Option<String> {
    let outdated_enabled = rules.iter().any(|r| r.id == "outdated_models" && r.enabled);
    if !outdated_enabled {
        return None;
    }

    let text = extract_checkable_text(tool_name, tool_input)?;
    check_outdated_models(&text)
}

// ── Rule helpers (inlined from rules.rs) ──────────────────────────────

struct ModelMapping {
    outdated: &'static str,
    suggestion: &'static str,
}

const OUTDATED_MODELS: &[ModelMapping] = &[
    ModelMapping { outdated: "gemini-2.0-flash",   suggestion: "gemini-2.5-flash (stable) or gemini-3.0-flash" },
    ModelMapping { outdated: "gemini-2-flash",      suggestion: "gemini-2.5-flash (stable) or gemini-3.0-flash" },
    ModelMapping { outdated: "gemini-1.5-flash",    suggestion: "gemini-2.5-flash" },
    ModelMapping { outdated: "gemini-1.5-pro",      suggestion: "gemini-2.5-pro" },
    ModelMapping { outdated: "gemini-1.0",          suggestion: "gemini-2.5-flash" },
    ModelMapping { outdated: "claude-3-5-sonnet",   suggestion: "claude-sonnet-4-6" },
    ModelMapping { outdated: "claude-3.5-sonnet",   suggestion: "claude-sonnet-4-6" },
    ModelMapping { outdated: "claude-3-opus",       suggestion: "claude-opus-4-6" },
    ModelMapping { outdated: "claude-3-sonnet",     suggestion: "claude-sonnet-4-6" },
    ModelMapping { outdated: "claude-3-haiku",      suggestion: "claude-haiku-4-5-20251001" },
    ModelMapping { outdated: "gpt-4o",              suggestion: "gpt-4.1 or gpt-5.4" },
    ModelMapping { outdated: "gpt-4-turbo",         suggestion: "gpt-4.1" },
    ModelMapping { outdated: "gpt-3.5",             suggestion: "gpt-4.1-mini" },
];

fn check_outdated_models(content: &str) -> Option<String> {
    let mut found: Vec<(&str, &str)> = Vec::new();
    for m in OUTDATED_MODELS {
        if content.contains(m.outdated) {
            if !found.iter().any(|(_, s)| *s == m.suggestion) {
                found.push((m.outdated, m.suggestion));
            }
        }
    }
    if found.is_empty() {
        return None;
    }
    let details: Vec<String> = found
        .iter()
        .map(|(old, new)| format!("'{}' -> {}", old, new))
        .collect();
    Some(format!(
        "Outdated model identifier detected: {}. Update to current versions.",
        details.join("; ")
    ))
}

fn extract_checkable_text(tool_name: &str, tool_input: &serde_json::Value) -> Option<String> {
    match tool_name {
        "Write" | "write" => tool_input.get("content").and_then(|v| v.as_str()).map(String::from),
        "Edit" | "edit" => tool_input.get("new_string").and_then(|v| v.as_str()).map(String::from),
        "Bash" | "bash" => tool_input.get("command").and_then(|v| v.as_str()).map(String::from),
        _ => None,
    }
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
    emitter: Option<tauri::AppHandle>,
    resolver: PidResolver,
    parent_pid_fn: ParentPidFn,
) -> Router {
    let state = AppState {
        coach,
        emitter,
        resolver,
        parent_pid_fn,
    };
    Router::new()
        .route("/hook/permission-request", post(handle_permission_request))
        .route("/hook/stop", post(handle_stop))
        .route("/hook/pre-tool-use", post(handle_pre_tool_use))
        .route("/hook/post-tool-use", post(handle_post_tool_use))
        .route("/hook/user-prompt-submit", post(handle_user_prompt_submit))
        .route("/hook/session-start", post(handle_session_start))
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

/// Router without Tauri emitter — for integration tests.
/// Tests inject a fake resolver via `fake_resolver_from_sid()` so the
/// in-process client gets distinct fake PIDs per session_id.
pub fn create_router_headless(coach: SharedState, resolver: PidResolver) -> Router {
    build_router(coach, None, resolver, no_parent())
}

/// Router with a custom parent-PID function — for tests that exercise
/// the parent walk (e.g. command-hook ghost session fix).
pub fn create_router_headless_with_parent(
    coach: SharedState,
    resolver: PidResolver,
    parent_pid_fn: ParentPidFn,
) -> Router {
    build_router(coach, None, resolver, parent_pid_fn)
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
    app_handle: Option<tauri::AppHandle>,
    port: u16,
) {
    let addr = format!("127.0.0.1:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("Failed to bind to {}: {}", addr, e));
    eprintln!("Coach hook server listening on {}", addr);
    serve_on_listener(listener, coach, app_handle, port).await;
}

/// Serve hook traffic on an already-bound listener. Used by the
/// headless `serve()` path so it can pre-bind, fail fast on port
/// collisions, and *then* announce success.
pub async fn serve_on_listener(
    listener: tokio::net::TcpListener,
    coach: SharedState,
    app_handle: Option<tauri::AppHandle>,
    port: u16,
) {
    let real_parent: ParentPidFn = Arc::new(crate::pid_resolver::parent_pid);
    let app = build_router(coach, app_handle, lsof_resolver(port), real_parent);
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

    // ── Rule engine tests (moved from rules.rs) ──────────────────────

    #[test]
    fn detects_outdated_gemini_model() {
        let code = r#"client = genai.Client(model="gemini-2.0-flash")"#;
        let msg = check_outdated_models(code);
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("gemini-2.0-flash"));
    }

    #[test]
    fn detects_outdated_claude_model() {
        let code = r#"model: "claude-3-5-sonnet-20241022""#;
        let msg = check_outdated_models(code);
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("claude-3-5-sonnet"));
    }

    #[test]
    fn detects_outdated_gpt_model() {
        let code = r#"model="gpt-4o""#;
        let msg = check_outdated_models(code);
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("gpt-4o"));
    }

    #[test]
    fn passes_current_models() {
        let code = r#"
            model = "gemini-2.5-flash"
            other = "claude-sonnet-4-6"
            gpt = "gpt-4.1"
        "#;
        assert!(check_outdated_models(code).is_none());
    }

    #[test]
    fn multiple_outdated_in_one_file() {
        let code = r#"
            primary = "gemini-2.0-flash"
            fallback = "gpt-4o"
        "#;
        let msg = check_outdated_models(code).unwrap();
        assert!(msg.contains("gemini-2.0-flash"));
        assert!(msg.contains("gpt-4o"));
    }

    #[test]
    fn extract_write_content() {
        let input = serde_json::json!({"file_path": "/a.py", "content": "x = 1"});
        assert_eq!(extract_checkable_text("Write", &input), Some("x = 1".into()));
    }

    #[test]
    fn extract_edit_new_string() {
        let input = serde_json::json!({"old_string": "a", "new_string": "b"});
        assert_eq!(extract_checkable_text("Edit", &input), Some("b".into()));
    }

    #[test]
    fn extract_bash_command() {
        let input = serde_json::json!({"command": "echo hi"});
        assert_eq!(extract_checkable_text("Bash", &input), Some("echo hi".into()));
    }

    #[test]
    fn extract_unknown_tool_returns_none() {
        let input = serde_json::json!({"query": "test"});
        assert!(extract_checkable_text("Grep", &input).is_none());
    }
}
