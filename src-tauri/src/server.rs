use axum::{extract::State as AxumState, routing::{get, post}, Json, Router};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::state::{CoachMode, SharedState};

#[derive(Deserialize)]
struct HookPayload {
    session_id: Option<String>,
    #[allow(dead_code)]
    hook_event_name: Option<String>,
    tool_name: Option<String>,
    tool_input: Option<serde_json::Value>,
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
    let mut coach = state.coach.write().await;

    // Extract session state we need for decisions, then release the borrow.
    let session = coach.session(&sid, payload.cwd.as_deref());
    let mode = session.mode.clone();
    let on_cooldown = session
        .last_stop_blocked
        .map_or(false, |last| last.elapsed() < STOP_COOLDOWN);

    if mode != CoachMode::Away {
        coach.sessions.get_mut(&sid).unwrap().stop_count += 1;
        coach.log(&sid, "Stop", "passed through", None);
        emit_update(&state.emitter, &coach);
        return Json(serde_json::json!({}));
    }

    if on_cooldown {
        coach.sessions.get_mut(&sid).unwrap().stop_count += 1;
        coach.log(&sid, "Stop", "allowed (cooldown)", None);
        emit_update(&state.emitter, &coach);
        return Json(serde_json::json!({}));
    }

    // Block the stop — update session fields
    {
        let session = coach.sessions.get_mut(&sid).unwrap();
        session.last_stop_blocked = Some(std::time::Instant::now());
        session.stop_count += 1;
        session.stop_blocked_count += 1;
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
    let mut coach = state.coach.write().await;
    let session = coach.session(&sid, payload.cwd.as_deref());
    *session.tool_counts.entry(tool.clone()).or_insert(0) += 1;

    // Run enabled rules against tool input
    let rule_message = check_rules(&coach.rules, &tool, &tool_input);

    if let Some(ref msg) = rule_message {
        coach.log(&sid, "PostToolUse", "rule triggered", Some(format!("{}: {}", tool, msg)));
    } else {
        coach.log(&sid, "PostToolUse", "observed", Some(tool));
    }

    emit_update(&state.emitter, &coach);

    match rule_message {
        Some(msg) => Json(HookResponse {
            hook_specific_output: Some(serde_json::json!({
                "additionalContext": msg
            })),
        }),
        None => Json(HookResponse::passthrough()),
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
