//! Claude Code hooks (`~/.claude/settings.json` → `/hook/*`).
//!
//! Claude Code POSTs directly from its own process, so the TCP peer
//! really is the agent — we look up the PID via lsof against the
//! listener port. The resolution is best-effort metadata now: session
//! identity comes from the hook payload's `session_id`, and failing to
//! resolve the PID just means the session displays pid 0 until a
//! future hook lands with a resolvable peer.
//!
//! The `HookPayload` shape below is Claude Code's native format; it
//! also happens to be what Codex emits, which is why the Codex adapter
//! re-uses it rather than defining its own struct.

use axum::{
    extract::{ConnectInfo, State as AxumState},
    routing::post,
    Json, Router,
};
use serde::Deserialize;
use serde_json::Value;
use std::net::SocketAddr;

use super::events::{dispatch, SessionEvent, SessionSource};
use super::AppState;

const SOURCE: SessionSource = SessionSource::ClaudeCode;

#[derive(Deserialize)]
pub(crate) struct HookPayload {
    pub(crate) session_id: Option<String>,
    #[allow(dead_code)]
    pub(crate) hook_event_name: Option<String>,
    pub(crate) tool_name: Option<String>,
    pub(crate) tool_input: Option<Value>,
    pub(crate) stop_reason: Option<String>,
    pub(crate) prompt: Option<String>,
    pub(crate) source: Option<String>,
    pub(crate) cwd: Option<String>,
}

impl HookPayload {
    pub(crate) fn sid(&self) -> String {
        self.session_id.clone().unwrap_or_else(|| "unknown".into())
    }
}

/// Resolve the TCP peer port to a PID for display. Returns 0 when the
/// resolver can't find a match — sessions still work, they just show
/// pid 0 until a hook arrives with a resolvable peer.
fn peer_pid(addr: SocketAddr) -> u32 {
    crate::pid_resolver::resolve_peer_pid(addr.port(), 0).unwrap_or(0)
}

async fn handle_permission_request(
    AxumState(state): AxumState<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(payload): Json<HookPayload>,
) -> Json<Value> {
    let sid = payload.sid();
    dispatch(
        &state,
        peer_pid(addr),
        SOURCE,
        SessionEvent::PermissionRequested {
            session_id: sid,
            cwd: payload.cwd,
            tool_name: payload.tool_name.unwrap_or_default(),
        },
    )
    .await
}

async fn handle_session_start(
    AxumState(state): AxumState<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(payload): Json<HookPayload>,
) -> Json<Value> {
    let sid = payload.sid();
    dispatch(
        &state,
        peer_pid(addr),
        SOURCE,
        SessionEvent::SessionStarted {
            session_id: sid,
            cwd: payload.cwd,
            source_label: payload.source.unwrap_or_else(|| "unknown".into()),
        },
    )
    .await
}

async fn handle_user_prompt_submit(
    AxumState(state): AxumState<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(payload): Json<HookPayload>,
) -> Json<Value> {
    let sid = payload.sid();
    dispatch(
        &state,
        peer_pid(addr),
        SOURCE,
        SessionEvent::UserPromptSubmitted {
            session_id: sid,
            cwd: payload.cwd,
            prompt: payload.prompt,
        },
    )
    .await
}

async fn handle_stop(
    AxumState(state): AxumState<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(payload): Json<HookPayload>,
) -> Json<Value> {
    let sid = payload.sid();
    dispatch(
        &state,
        peer_pid(addr),
        SOURCE,
        SessionEvent::StopRequested {
            session_id: sid,
            cwd: payload.cwd,
            stop_reason: payload.stop_reason,
        },
    )
    .await
}

async fn handle_pre_tool_use(
    AxumState(state): AxumState<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(payload): Json<HookPayload>,
) -> Json<Value> {
    let sid = payload.sid();
    dispatch(
        &state,
        peer_pid(addr),
        SOURCE,
        SessionEvent::ToolStarting {
            session_id: sid,
            cwd: payload.cwd,
            tool_name: payload.tool_name.unwrap_or_default(),
        },
    )
    .await
}

async fn handle_post_tool_use(
    AxumState(state): AxumState<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(payload): Json<HookPayload>,
) -> Json<Value> {
    let sid = payload.sid();
    dispatch(
        &state,
        peer_pid(addr),
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

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/hook/permission-request", post(handle_permission_request))
        .route("/hook/stop", post(handle_stop))
        .route("/hook/pre-tool-use", post(handle_pre_tool_use))
        .route("/hook/post-tool-use", post(handle_post_tool_use))
        .route("/hook/user-prompt-submit", post(handle_user_prompt_submit))
        .route("/hook/session-start", post(handle_session_start))
}
