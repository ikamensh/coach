use axum::{extract::State as AxumState, routing::{get, post}, Json, Router};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::settings::EngineMode;
use crate::state::{CoachMode, SharedState};

#[derive(Deserialize)]
struct HookPayload {
    session_id: Option<String>,
    #[allow(dead_code)]
    hook_event_name: Option<String>,
    tool_name: Option<String>,
    tool_input: Option<serde_json::Value>,
    /// Set by Claude Code on Stop hooks when available.
    stop_reason: Option<String>,
    cwd: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HookResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    hook_specific_output: Option<serde_json::Value>,
}

impl HookResponse {
    fn passthrough() -> Self {
        Self {
            hook_specific_output: None,
        }
    }
}

#[derive(Clone)]
struct AppState {
    coach: SharedState,
    emitter: Option<tauri::AppHandle>,
}

fn session_id(payload: &HookPayload) -> String {
    payload
        .session_id
        .clone()
        .unwrap_or_else(|| "unknown".into())
}

fn emit_update(emitter: &Option<tauri::AppHandle>, coach: &crate::state::CoachState) {
    if let Some(handle) = emitter {
        use tauri::Emitter;
        let _ = handle.emit(crate::state::EVENT_STATE_UPDATED, coach.snapshot());
    }
}

async fn handle_permission_request(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<HookPayload>,
) -> Json<HookResponse> {
    let sid = session_id(&payload);
    let tool = payload.tool_name.clone().unwrap_or_default();
    let mut coach = state.coach.write().await;
    let session = coach.session(&sid, payload.cwd.as_deref());
    *session.tool_counts.entry(tool.clone()).or_insert(0) += 1;
    let mode = session.mode.clone();

    if mode == CoachMode::Away {
        coach.log(&sid, "PermissionRequest", "auto-approved", Some(tool));
        emit_update(&state.emitter, &coach);
        Json(HookResponse {
            hook_specific_output: Some(serde_json::json!({
                "decision": { "behavior": "allow" }
            })),
        })
    } else {
        coach.log(&sid, "PermissionRequest", "passed through", Some(tool));
        emit_update(&state.emitter, &coach);
        Json(HookResponse::passthrough())
    }
}

const STOP_COOLDOWN: Duration = Duration::from_secs(15);

async fn handle_stop(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<HookPayload>,
) -> Json<serde_json::Value> {
    let sid = session_id(&payload);

    // Phase 1: read context, increment stop_count, release the lock
    // before we make any (potentially slow) LLM call.
    let (coach_mode, provider_capable, prev_response_id, ctx) = {
        let mut coach = state.coach.write().await;
        let priorities = coach.priorities.clone();
        let provider_capable = crate::settings::OBSERVER_CAPABLE_PROVIDERS
            .contains(&coach.model.provider.as_str());
        let coach_mode = coach.coach_mode.clone();
        let session = coach.session(&sid, payload.cwd.as_deref());
        session.stop_count += 1;

        if session.mode != CoachMode::Away {
            coach.log(&sid, "Stop", "passed through", None);
            emit_update(&state.emitter, &coach);
            return Json(serde_json::json!({}));
        }

        let prev = session.coach_response_id.clone();
        let ctx = crate::llm::StopContext {
            priorities,
            cwd: session.cwd.clone(),
            tool_counts: session.tool_counts.clone(),
            stop_count: session.stop_count,
            stop_blocked_count: session.stop_blocked_count,
            stop_reason: payload.stop_reason.clone(),
        };
        (coach_mode, provider_capable, prev, ctx)
    };

    // Phase 2: LLM mode. Two paths:
    //   • Chained (OpenAI Responses API): continues the observer's chain
    //     so the model uses everything it's already seen this session.
    //   • One-shot fallback: any other provider — sends only the digest.
    if coach_mode == EngineMode::Llm {
        let chained = if provider_capable {
            match crate::llm::evaluate_stop_chained(
                &state.coach,
                &ctx.priorities,
                prev_response_id.as_deref(),
                ctx.stop_reason.as_deref(),
            )
            .await
            {
                Ok((decision, new_id)) => Some(Ok((decision, Some(new_id)))),
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
            Ok((decision, new_response_id)) if decision.allow => {
                let mut coach = state.coach.write().await;
                if let (Some(s), Some(id)) = (coach.sessions.get_mut(&sid), new_response_id) {
                    s.coach_response_id = Some(id);
                }
                coach.log(&sid, "Stop", "allowed (LLM)", None);
                emit_update(&state.emitter, &coach);
                return Json(serde_json::json!({}));
            }
            Ok((decision, new_response_id)) => {
                let mut coach = state.coach.write().await;
                let message = decision
                    .message
                    .filter(|m| !m.trim().is_empty())
                    .unwrap_or_else(|| crate::state::away_message(&coach.priorities));
                if let Some(s) = coach.sessions.get_mut(&sid) {
                    s.last_stop_blocked = Some(std::time::Instant::now());
                    s.stop_blocked_count += 1;
                    if let Some(id) = new_response_id {
                        s.coach_response_id = Some(id);
                    }
                }
                coach.log(&sid, "Stop", "blocked (LLM)", Some(message.clone()));
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
        .get(&sid)
        .and_then(|s| s.last_stop_blocked)
        .is_some_and(|last| last.elapsed() < STOP_COOLDOWN);

    if on_cooldown {
        coach.log(&sid, "Stop", "allowed (cooldown)", None);
        emit_update(&state.emitter, &coach);
        return Json(serde_json::json!({}));
    }

    if let Some(s) = coach.sessions.get_mut(&sid) {
        s.last_stop_blocked = Some(std::time::Instant::now());
        s.stop_blocked_count += 1;
    }
    let message = crate::state::away_message(&coach.priorities);
    coach.log(&sid, "Stop", "blocked — user away", Some(message.clone()));
    emit_update(&state.emitter, &coach);

    // Stop hooks use top-level fields, NOT hookSpecificOutput.
    Json(serde_json::json!({
        "decision": "block",
        "reason": message
    }))
}

async fn handle_post_tool_use(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<HookPayload>,
) -> Json<HookResponse> {
    let sid = session_id(&payload);
    let tool = payload.tool_name.unwrap_or_default();
    let tool_input = payload.tool_input.unwrap_or(serde_json::Value::Null);

    let observer_input;
    let rule_message;
    {
        let mut coach = state.coach.write().await;
        let prev_response_id = {
            let session = coach.session(&sid, payload.cwd.as_deref());
            *session.tool_counts.entry(tool.clone()).or_insert(0) += 1;
            session.coach_response_id.clone()
        };

        rule_message = check_rules(&coach.rules, &tool, &tool_input);

        if let Some(ref msg) = rule_message {
            coach.log(
                &sid,
                "PostToolUse",
                "rule triggered",
                Some(format!("{}: {}", tool, msg)),
            );
        } else {
            coach.log(&sid, "PostToolUse", "observed", Some(tool.clone()));
        }

        // Decide if we should fire the observer. Requires LLM mode + a
        // provider that can chain response_ids (rig only does this for OpenAI).
        observer_input = if coach.coach_mode == EngineMode::Llm
            && crate::settings::OBSERVER_CAPABLE_PROVIDERS
                .contains(&coach.model.provider.as_str())
        {
            Some(ObserverInput {
                sid: sid.clone(),
                priorities: coach.priorities.clone(),
                previous_response_id: prev_response_id,
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

struct ObserverInput {
    sid: String,
    priorities: Vec<String>,
    previous_response_id: Option<String>,
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
        input.previous_response_id.as_deref(),
        &input.event,
    )
    .await
    {
        Ok(call) => {
            let mut s = coach.write().await;
            if let Some(sess) = s.sessions.get_mut(&input.sid) {
                sess.coach_response_id = Some(call.response_id);
                sess.coach_last_assessment = Some(call.text.clone());
            }
            s.log(&input.sid, "Observer", "noted", Some(call.text));
            emit_update(&emitter, &s);
        }
        Err(e) => {
            eprintln!("[coach] observer call failed: {e}");
            let mut s = coach.write().await;
            s.log(&input.sid, "Observer", "error", Some(e));
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

fn build_router(coach: SharedState, emitter: Option<tauri::AppHandle>) -> Router {
    let state = AppState { coach, emitter };
    Router::new()
        .route("/hook/permission-request", post(handle_permission_request))
        .route("/hook/stop", post(handle_stop))
        .route("/hook/post-tool-use", post(handle_post_tool_use))
        .route("/state", get(handle_get_state))
        .route("/version", get(handle_version))
        .with_state(state)
}

pub fn create_router(coach: SharedState, app_handle: tauri::AppHandle) -> Router {
    build_router(coach, Some(app_handle))
}

/// Router without Tauri emitter — for integration tests.
pub fn create_router_headless(coach: SharedState) -> Router {
    build_router(coach, None)
}

pub async fn start_server(coach: SharedState, app_handle: tauri::AppHandle, port: u16) {
    let app = create_router(coach, app_handle);
    let addr = format!("127.0.0.1:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("Failed to bind to {}: {}", addr, e));
    eprintln!("Coach hook server listening on {}", addr);
    axum::serve(listener, app)
        .await
        .expect("Hook server crashed");
}
