use axum::{
    extract::{ConnectInfo, Path, State as AxumState},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use crate::settings::{CoachRule, EngineMode, ModelConfig};
use crate::state::{CoachMode, SharedState};

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

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) coach: SharedState,
    pub(crate) emitter: Option<tauri::AppHandle>,
    resolver: PidResolver,
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
async fn resolve_pid(state: &AppState, sid: &str, peer_port: u16) -> Option<u32> {
    {
        let coach = state.coach.read().await;
        if let Some(&pid) = coach.session_id_to_pid.get(sid) {
            return Some(pid);
        }
    }
    (state.resolver)(peer_port, sid)
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
    let mode = session.mode.clone();

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

        let prev_chain = session.coach_chain.clone();
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
        let chained = if provider_capable {
            match crate::llm::evaluate_stop_chained(
                &state.coach,
                &ctx.priorities,
                &prev_chain,
                ctx.stop_reason.as_deref(),
            )
            .await
            {
                Ok((decision, new_chain)) => Some(Ok((decision, Some(new_chain)))),
                Err(e) => Some(Err(e)),
            }
        } else {
            None
        };

        let result = match chained {
            Some(r) => r,
            None => crate::llm::evaluate_stop(&state.coach, &ctx)
                .await
                .map(|d| (d, None)),
        };

        match result {
            Ok((decision, new_chain)) if decision.allow => {
                let mut coach = state.coach.write().await;
                if let (Some(s), Some(c)) = (coach.sessions.get_mut(&pid), new_chain) {
                    s.coach_chain = c;
                }
                coach.log(pid, "Stop", "allowed (LLM)", None);
                emit_update(&state.emitter, &coach);
                return Json(serde_json::json!({}));
            }
            Ok((decision, new_chain)) => {
                let mut coach = state.coach.write().await;
                let message = decision
                    .message
                    .filter(|m| !m.trim().is_empty())
                    .unwrap_or_else(|| crate::state::away_message(&coach.priorities));
                if let Some(s) = coach.sessions.get_mut(&pid) {
                    s.last_stop_blocked = Some(std::time::Instant::now());
                    s.stop_blocked_count += 1;
                    if let Some(c) = new_chain {
                        s.coach_chain = c;
                    }
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

pub(crate) async fn run_post_tool_use(
    state: &AppState,
    pid: u32,
    payload: HookPayload,
) -> Json<HookResponse> {
    let sid = session_id(&payload);
    let tool = payload.tool_name.unwrap_or_default();
    let tool_input = payload.tool_input.unwrap_or(serde_json::Value::Null);

    let observer_input;
    let rule_message;
    {
        let mut coach = state.coach.write().await;
        let prev_chain = {
            let session = coach.apply_hook_event(pid, &sid, payload.cwd.as_deref());
            *session.tool_counts.entry(tool.clone()).or_insert(0) += 1;
            session.coach_chain.clone()
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

        // Decide if we should fire the observer. Requires LLM mode + a
        // provider that supports stateful coach chains (OpenAI Responses
        // or Anthropic with prompt caching, currently).
        observer_input = if coach.coach_mode == EngineMode::Llm
            && crate::settings::OBSERVER_CAPABLE_PROVIDERS
                .contains(&coach.model.provider.as_str())
        {
            Some(ObserverInput {
                pid,
                priorities: coach.priorities.clone(),
                prev_chain,
                event: crate::llm::build_observer_event(&tool, &tool_input),
            })
        } else {
            None
        };

        emit_update(&state.emitter, &coach);
    } // lock released

    // Fire-and-forget: the observer call may take seconds, but the agent
    // shouldn't wait. PostToolUse always returns immediately.
    if let Some(input) = observer_input {
        let coach_state = state.coach.clone();
        let emitter = state.emitter.clone();
        tokio::spawn(async move {
            run_observer(coach_state, emitter, input).await;
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

struct ObserverInput {
    pid: u32,
    priorities: Vec<String>,
    prev_chain: crate::state::CoachChain,
    event: String,
}

async fn run_observer(
    coach: SharedState,
    emitter: Option<tauri::AppHandle>,
    input: ObserverInput,
) {
    match crate::llm::observe_event(
        &coach,
        &input.priorities,
        &input.prev_chain,
        &input.event,
    )
    .await
    {
        Ok((text, new_chain)) => {
            let mut s = coach.write().await;
            if let Some(sess) = s.sessions.get_mut(&input.pid) {
                sess.coach_chain = new_chain;
                sess.coach_last_assessment = Some(text.clone());
            }
            s.log(input.pid, "Observer", "noted", Some(text));
            emit_update(&emitter, &s);
        }
        Err(e) => {
            eprintln!("[coach] observer call failed: {e}");
            let mut s = coach.write().await;
            s.log(input.pid, "Observer", "error", Some(e));
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

    let text = crate::rules::extract_checkable_text(tool_name, tool_input)?;
    crate::rules::check_outdated_models(&text)
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

// ── /api/* endpoints used by the CLI when Coach is running ──────────────
//
// These mirror the Tauri commands in commands.rs so the CLI never has to
// touch ~/.coach/settings.json directly while the GUI is up. Each handler
// mutates the in-memory state, persists to disk, and emits the same
// `coach-state-updated` event the Tauri commands emit so the GUI refreshes.

#[derive(Deserialize)]
struct ModePayload {
    mode: CoachMode,
}

#[derive(Deserialize)]
struct PrioritiesPayload {
    priorities: Vec<String>,
}

#[derive(Deserialize)]
struct ApiTokenPayload {
    provider: String,
    token: String,
}

#[derive(Deserialize)]
struct CoachModePayload {
    coach_mode: EngineMode,
}

#[derive(Deserialize)]
struct RulesPayload {
    rules: Vec<CoachRule>,
}

async fn api_set_session_mode(
    AxumState(state): AxumState<AppState>,
    Path(pid): Path<u32>,
    Json(payload): Json<ModePayload>,
) -> Result<Json<crate::state::CoachSnapshot>, (StatusCode, String)> {
    let mut s = state.coach.write().await;
    if !s.sessions.contains_key(&pid) {
        return Err((StatusCode::NOT_FOUND, format!("no session for pid {pid}")));
    }
    if let Some(sess) = s.sessions.get_mut(&pid) {
        sess.mode = payload.mode;
    }
    let snap = s.snapshot();
    emit_update(&state.emitter, &s);
    Ok(Json(snap))
}

async fn api_set_all_modes(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<ModePayload>,
) -> Json<crate::state::CoachSnapshot> {
    let mut s = state.coach.write().await;
    s.set_all_modes(payload.mode);
    let snap = s.snapshot();
    emit_update(&state.emitter, &s);
    Json(snap)
}

async fn api_set_priorities(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<PrioritiesPayload>,
) -> Json<crate::state::CoachSnapshot> {
    let mut s = state.coach.write().await;
    s.priorities = payload.priorities;
    s.save();
    let snap = s.snapshot();
    emit_update(&state.emitter, &s);
    Json(snap)
}

async fn api_set_model(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<ModelConfig>,
) -> Json<crate::state::CoachSnapshot> {
    let mut s = state.coach.write().await;
    s.model = payload;
    s.save();
    let snap = s.snapshot();
    emit_update(&state.emitter, &s);
    Json(snap)
}

async fn api_set_api_token(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<ApiTokenPayload>,
) -> Json<crate::state::CoachSnapshot> {
    let mut s = state.coach.write().await;
    if payload.token.is_empty() {
        s.api_tokens.remove(&payload.provider);
    } else {
        s.api_tokens.insert(payload.provider, payload.token);
    }
    s.save();
    let snap = s.snapshot();
    emit_update(&state.emitter, &s);
    Json(snap)
}

async fn api_set_coach_mode(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<CoachModePayload>,
) -> Json<crate::state::CoachSnapshot> {
    let mut s = state.coach.write().await;
    s.coach_mode = payload.coach_mode;
    s.save();
    let snap = s.snapshot();
    emit_update(&state.emitter, &s);
    Json(snap)
}

async fn api_set_rules(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<RulesPayload>,
) -> Json<crate::state::CoachSnapshot> {
    let mut s = state.coach.write().await;
    s.rules = payload.rules;
    s.save();
    let snap = s.snapshot();
    emit_update(&state.emitter, &s);
    Json(snap)
}

fn build_router(
    coach: SharedState,
    emitter: Option<tauri::AppHandle>,
    resolver: PidResolver,
) -> Router {
    let state = AppState {
        coach,
        emitter,
        resolver,
    };
    Router::new()
        .route("/hook/permission-request", post(handle_permission_request))
        .route("/hook/stop", post(handle_stop))
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
        .route("/api/sessions/mode", post(api_set_all_modes))
        .route("/api/sessions/{pid}/mode", post(api_set_session_mode))
        .route("/api/config/priorities", post(api_set_priorities))
        .route("/api/config/model", post(api_set_model))
        .route("/api/config/api-token", post(api_set_api_token))
        .route("/api/config/coach-mode", post(api_set_coach_mode))
        .route("/api/config/rules", post(api_set_rules))
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

/// Router without Tauri emitter — for integration tests.
/// Tests inject a fake resolver via `fake_resolver_from_sid()` so the
/// in-process client gets distinct fake PIDs per session_id.
pub fn create_router_headless(coach: SharedState, resolver: PidResolver) -> Router {
    build_router(coach, None, resolver)
}

/// Bind the production hook server. Pass `Some(app_handle)` from the
/// Tauri GUI path to get state-update events emitted to the frontend;
/// pass `None` for headless `coach serve` mode (CLI / VM tests / CI).
pub async fn start_server(
    coach: SharedState,
    app_handle: Option<tauri::AppHandle>,
    port: u16,
) {
    let app = build_router(coach, app_handle, lsof_resolver(port));
    let addr = format!("127.0.0.1:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("Failed to bind to {}: {}", addr, e));
    eprintln!("Coach hook server listening on {}", addr);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .expect("Hook server crashed");
}
