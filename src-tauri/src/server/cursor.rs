//! Cursor Agent lifecycle hooks (`~/.cursor/hooks.json`) send JSON to `/cursor/hook/*`.
//! Cursor invokes subprocess commands (e.g. `curl`) that POST stdin to Coach; the TCP peer is
//! not the agent process, so we key sessions by `session_id` from the payload and use the same
//! synthetic PID scheme as integration tests (`fake_pid_for_sid`).

use axum::extract::State as AxumState;
use axum::Json;
use serde_json::{json, Value};

use super::{
    fake_pid_for_sid, run_permission_request, run_post_tool_use, run_session_start, run_stop,
    run_user_prompt_submit, AppState, HookPayload, HookResponse,
};
use crate::state::SessionClient;

/// Tag the session as belonging to Cursor right after the shared `run_*`
/// path created/updated it. Idempotent — safe to call after every cursor
/// hook even though only the first one transitions the client field.
async fn mark_cursor(state: &AppState, pid: u32) {
    state
        .coach
        .write()
        .await
        .mark_client(pid, SessionClient::Cursor);
}

fn cursor_session_key(v: &Value) -> String {
    for key in [
        "sessionId",
        "session_id",
        "conversationId",
        "conversation_id",
        "id",
    ] {
        if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    "unknown".to_string()
}

fn cursor_pid(v: &Value) -> u32 {
    fake_pid_for_sid(&cursor_session_key(v))
}

fn cursor_cwd(v: &Value) -> Option<String> {
    for key in ["cwd", "workspaceRoot", "workspace_root", "rootPath", "root"] {
        if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
            return Some(s.to_string());
        }
    }
    // `cursor-agent` (and the IDE) actually send `workspace_roots: [path,
    // ...]` — first entry is the active workspace root.
    if let Some(arr) = v.get("workspace_roots").and_then(|x| x.as_array()) {
        if let Some(s) = arr.first().and_then(|x| x.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

fn payload_session_start(v: &Value) -> HookPayload {
    let sid = cursor_session_key(v);
    let source = v
        .get("source")
        .or_else(|| v.get("type"))
        .and_then(|x| x.as_str())
        .unwrap_or("cursor")
        .to_string();
    HookPayload {
        session_id: Some(sid),
        hook_event_name: Some("sessionStart".into()),
        tool_name: None,
        tool_input: None,
        stop_reason: None,
        prompt: None,
        source: Some(source),
        cwd: cursor_cwd(v),
    }
}

fn payload_before_submit(v: &Value) -> HookPayload {
    let sid = cursor_session_key(v);
    let prompt = v
        .get("prompt")
        .or_else(|| v.get("text"))
        .or_else(|| v.get("message"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    HookPayload {
        session_id: Some(sid),
        hook_event_name: Some("beforeSubmitPrompt".into()),
        tool_name: None,
        tool_input: None,
        stop_reason: None,
        prompt,
        source: None,
        cwd: cursor_cwd(v),
    }
}

fn payload_before_shell(v: &Value) -> HookPayload {
    let sid = cursor_session_key(v);
    let cmd = v
        .get("command")
        .or_else(|| v.get("commandLine"))
        .or_else(|| v.get("script"))
        .and_then(|x| x.as_str())
        .unwrap_or("");
    HookPayload {
        session_id: Some(sid),
        hook_event_name: Some("beforeShellExecution".into()),
        tool_name: Some("Bash".into()),
        tool_input: Some(json!({ "command": cmd })),
        stop_reason: None,
        prompt: None,
        source: None,
        cwd: cursor_cwd(v),
    }
}

fn payload_before_mcp(v: &Value) -> HookPayload {
    let sid = cursor_session_key(v);
    HookPayload {
        session_id: Some(sid),
        hook_event_name: Some("beforeMCPExecution".into()),
        tool_name: Some("mcp".into()),
        tool_input: Some(v.clone()),
        stop_reason: None,
        prompt: None,
        source: None,
        cwd: cursor_cwd(v),
    }
}

fn payload_after_shell(v: &Value) -> HookPayload {
    let sid = cursor_session_key(v);
    let cmd = v
        .get("command")
        .or_else(|| v.get("commandLine"))
        .and_then(|x| x.as_str())
        .unwrap_or("");
    HookPayload {
        session_id: Some(sid),
        hook_event_name: Some("afterShellExecution".into()),
        tool_name: Some("Bash".into()),
        tool_input: Some(json!({ "command": cmd })),
        stop_reason: None,
        prompt: None,
        source: None,
        cwd: cursor_cwd(v),
    }
}

fn payload_after_mcp(v: &Value) -> HookPayload {
    let sid = cursor_session_key(v);
    HookPayload {
        session_id: Some(sid),
        hook_event_name: Some("afterMCPExecution".into()),
        tool_name: Some("mcp".into()),
        tool_input: Some(v.clone()),
        stop_reason: None,
        prompt: None,
        source: None,
        cwd: cursor_cwd(v),
    }
}

fn payload_after_file_edit(v: &Value) -> HookPayload {
    let sid = cursor_session_key(v);
    let new_string = v
        .get("newContent")
        .or_else(|| v.get("new_string"))
        .or_else(|| v.get("newString"))
        .and_then(|x| x.as_str())
        .unwrap_or("");
    HookPayload {
        session_id: Some(sid),
        hook_event_name: Some("afterFileEdit".into()),
        tool_name: Some("Edit".into()),
        tool_input: Some(json!({ "new_string": new_string })),
        stop_reason: None,
        prompt: None,
        source: None,
        cwd: cursor_cwd(v),
    }
}

fn payload_stop(v: &Value) -> HookPayload {
    let sid = cursor_session_key(v);
    let stop_reason = v
        .get("reason")
        .or_else(|| v.get("stopReason"))
        .or_else(|| v.get("stop_reason"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    HookPayload {
        session_id: Some(sid),
        hook_event_name: Some("stop".into()),
        tool_name: None,
        tool_input: None,
        stop_reason,
        prompt: None,
        source: None,
        cwd: cursor_cwd(v),
    }
}

pub async fn session_start(
    AxumState(state): AxumState<AppState>,
    Json(v): Json<Value>,
) -> Json<HookResponse> {
    let pid = cursor_pid(&v);
    let resp = run_session_start(&state, pid, payload_session_start(&v)).await;
    mark_cursor(&state, pid).await;
    resp
}

pub async fn before_submit_prompt(
    AxumState(state): AxumState<AppState>,
    Json(v): Json<Value>,
) -> Json<HookResponse> {
    let pid = cursor_pid(&v);
    let resp = run_user_prompt_submit(&state, pid, payload_before_submit(&v)).await;
    mark_cursor(&state, pid).await;
    resp
}

pub async fn before_shell(
    AxumState(state): AxumState<AppState>,
    Json(v): Json<Value>,
) -> Json<HookResponse> {
    let pid = cursor_pid(&v);
    let resp = run_permission_request(&state, pid, payload_before_shell(&v)).await;
    mark_cursor(&state, pid).await;
    resp
}

pub async fn before_mcp(
    AxumState(state): AxumState<AppState>,
    Json(v): Json<Value>,
) -> Json<HookResponse> {
    let pid = cursor_pid(&v);
    let resp = run_permission_request(&state, pid, payload_before_mcp(&v)).await;
    mark_cursor(&state, pid).await;
    resp
}

pub async fn after_shell(
    AxumState(state): AxumState<AppState>,
    Json(v): Json<Value>,
) -> Json<HookResponse> {
    let pid = cursor_pid(&v);
    let resp = run_post_tool_use(&state, pid, payload_after_shell(&v)).await;
    mark_cursor(&state, pid).await;
    resp
}

pub async fn after_mcp(
    AxumState(state): AxumState<AppState>,
    Json(v): Json<Value>,
) -> Json<HookResponse> {
    let pid = cursor_pid(&v);
    let resp = run_post_tool_use(&state, pid, payload_after_mcp(&v)).await;
    mark_cursor(&state, pid).await;
    resp
}

pub async fn after_file_edit(
    AxumState(state): AxumState<AppState>,
    Json(v): Json<Value>,
) -> Json<HookResponse> {
    let pid = cursor_pid(&v);
    let resp = run_post_tool_use(&state, pid, payload_after_file_edit(&v)).await;
    mark_cursor(&state, pid).await;
    resp
}

pub async fn stop(
    AxumState(state): AxumState<AppState>,
    Json(v): Json<Value>,
) -> Json<serde_json::Value> {
    let pid = cursor_pid(&v);
    let resp = run_stop(&state, pid, payload_stop(&v)).await;
    mark_cursor(&state, pid).await;
    resp
}
