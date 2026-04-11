//! Codex CLI hooks. Payload shape is identical to Claude Code's (same
//! `HookPayload` struct), but hooks arrive via a shim curl subprocess,
//! so the TCP peer isn't the agent. We key sessions by `session_id` →
//! synthetic PID, like the Cursor adapter does.

use axum::extract::State as AxumState;
use axum::{routing::post, Json, Router};
use serde_json::Value;

use super::claude::HookPayload;
use super::events::{dispatch, SessionEvent, SessionSource};
use super::{fake_pid_for_sid, HookServerState};

const SOURCE: SessionSource = SessionSource::Codex;

fn pid_and_sid(payload: &HookPayload) -> (u32, String) {
    let sid = payload.sid();
    (fake_pid_for_sid(&sid), sid)
}

async fn permission_request(
    AxumState(state): AxumState<HookServerState>,
    Json(payload): Json<HookPayload>,
) -> Json<Value> {
    let (pid, sid) = pid_and_sid(&payload);
    dispatch(
        &state,
        pid,
        SOURCE,
        SessionEvent::PermissionRequested {
            session_id: sid,
            cwd: payload.cwd,
            tool_name: payload.tool_name.unwrap_or_default(),
        },
    )
    .await
}

async fn session_start(
    AxumState(state): AxumState<HookServerState>,
    Json(payload): Json<HookPayload>,
) -> Json<Value> {
    let (pid, sid) = pid_and_sid(&payload);
    dispatch(
        &state,
        pid,
        SOURCE,
        SessionEvent::SessionStarted {
            session_id: sid,
            cwd: payload.cwd,
            source_label: payload.source.unwrap_or_else(|| "codex".into()),
        },
    )
    .await
}

async fn user_prompt_submit(
    AxumState(state): AxumState<HookServerState>,
    Json(payload): Json<HookPayload>,
) -> Json<Value> {
    let (pid, sid) = pid_and_sid(&payload);
    dispatch(
        &state,
        pid,
        SOURCE,
        SessionEvent::UserPromptSubmitted {
            session_id: sid,
            cwd: payload.cwd,
            prompt: payload.prompt,
        },
    )
    .await
}

async fn stop(
    AxumState(state): AxumState<HookServerState>,
    Json(payload): Json<HookPayload>,
) -> Json<Value> {
    let (pid, sid) = pid_and_sid(&payload);
    dispatch(
        &state,
        pid,
        SOURCE,
        SessionEvent::StopRequested {
            session_id: sid,
            cwd: payload.cwd,
            stop_reason: payload.stop_reason,
        },
    )
    .await
}

async fn pre_tool_use(
    AxumState(state): AxumState<HookServerState>,
    Json(payload): Json<HookPayload>,
) -> Json<Value> {
    let (pid, sid) = pid_and_sid(&payload);
    dispatch(
        &state,
        pid,
        SOURCE,
        SessionEvent::ToolStarting {
            session_id: sid,
            cwd: payload.cwd,
            tool_name: payload.tool_name.unwrap_or_default(),
        },
    )
    .await
}

async fn post_tool_use(
    AxumState(state): AxumState<HookServerState>,
    Json(payload): Json<HookPayload>,
) -> Json<Value> {
    let (pid, sid) = pid_and_sid(&payload);
    dispatch(
        &state,
        pid,
        SOURCE,
        SessionEvent::ToolCompleted {
            session_id: sid,
            cwd: payload.cwd,
            tool_name: payload.tool_name.unwrap_or_default(),
            tool_input: payload.tool_input.unwrap_or(Value::Null),
        },
    )
    .await
}

pub(crate) fn routes() -> Router<HookServerState> {
    Router::new()
        .route("/codex/hook/permission-request", post(permission_request))
        .route("/codex/hook/stop", post(stop))
        .route("/codex/hook/pre-tool-use", post(pre_tool_use))
        .route("/codex/hook/post-tool-use", post(post_tool_use))
        .route("/codex/hook/user-prompt-submit", post(user_prompt_submit))
        .route("/codex/hook/session-start", post(session_start))
}
