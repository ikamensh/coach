//! Cursor Agent lifecycle hooks (`~/.cursor/hooks.json`) send JSON to `/cursor/hook/*`.
//! Cursor invokes subprocess commands (e.g. `curl`) that POST stdin to Coach; the TCP peer is
//! not the agent process, so we key sessions by `session_id` from the payload and use the same
//! synthetic PID scheme as integration tests (`fake_pid_for_sid`).

use axum::extract::State as AxumState;
use axum::Json;
use serde_json::{json, Value};

use super::{
    emit_update, fake_pid_for_sid, run_permission_request, run_post_tool_use, run_session_start,
    run_stop, run_user_prompt_submit, AppState, HookPayload, HookResponse,
};
use crate::state::SessionClient;

/// Tag the session as belonging to Cursor and re-emit so the frontend
/// gets the updated `client` field. The shared `run_*` path the cursor
/// handler just called already emitted a snapshot with `client=Claude`
/// (the default on creation), so without this re-emit the icon would
/// flicker on the first event of every fresh cursor session.
async fn mark_cursor(state: &AppState, pid: u32) {
    let mut coach = state.coach.write().await;
    coach.mark_client(pid, SessionClient::Cursor);
    emit_update(&state.emitter, &coach);
}

/// First non-empty string under any of `keys`. Used to probe Cursor's
/// JSON payloads, which have shifted between snake_case and camelCase
/// across versions.
fn first_string_field(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| {
        v.get(k)
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

fn cursor_session_key(v: &Value) -> String {
    first_string_field(
        v,
        &[
            "sessionId",
            "session_id",
            "conversationId",
            "conversation_id",
            "id",
        ],
    )
    .unwrap_or_else(|| "unknown".to_string())
}

fn cursor_pid(v: &Value) -> u32 {
    fake_pid_for_sid(&cursor_session_key(v))
}

fn cursor_cwd(v: &Value) -> Option<String> {
    first_string_field(
        v,
        &["cwd", "workspaceRoot", "workspace_root", "rootPath", "root"],
    )
    .or_else(|| {
        // `cursor-agent` and the IDE actually send `workspace_roots: [path, ...]`.
        v.get("workspace_roots")
            .and_then(|x| x.as_array())
            .and_then(|arr| arr.first())
            .and_then(|x| x.as_str())
            .map(str::to_string)
    })
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

/// Each cursor handler is identical: extract the synthetic PID from the
/// payload, run the shared `run_*` for the matching Claude lifecycle
/// event, then re-tag the session as Cursor (and re-emit). The macro
/// keeps the function names stable so the axum routes in `server.rs`
/// don't need to change when handlers are added or removed.
macro_rules! cursor_handler {
    ($name:ident, $run:ident, $payload:ident, $ret:ty) => {
        pub async fn $name(
            AxumState(state): AxumState<AppState>,
            Json(v): Json<Value>,
        ) -> Json<$ret> {
            let pid = cursor_pid(&v);
            let resp = $run(&state, pid, $payload(&v)).await;
            mark_cursor(&state, pid).await;
            resp
        }
    };
}

cursor_handler!(session_start, run_session_start, payload_session_start, HookResponse);
cursor_handler!(before_submit_prompt, run_user_prompt_submit, payload_before_submit, HookResponse);
cursor_handler!(before_shell, run_permission_request, payload_before_shell, HookResponse);
cursor_handler!(before_mcp, run_permission_request, payload_before_mcp, HookResponse);
cursor_handler!(after_shell, run_post_tool_use, payload_after_shell, HookResponse);
cursor_handler!(after_mcp, run_post_tool_use, payload_after_mcp, HookResponse);
cursor_handler!(after_file_edit, run_post_tool_use, payload_after_file_edit, HookResponse);
cursor_handler!(stop, run_stop, payload_stop, serde_json::Value);
