//! Cursor Agent lifecycle hooks (`~/.cursor/hooks.json`).
//!
//! Cursor invokes a shim subprocess that POSTs the hook JSON to Coach;
//! the TCP peer is the shim (curl), not the agent process, so we key
//! sessions by `session_id` → synthetic PID just like the Codex adapter.
//! Each hook has its own raw JSON shape, so there's no single payload
//! struct — each route pulls what it needs out of the `serde_json::Value`.

use axum::extract::State as AxumState;
use axum::{routing::post, Json, Router};
use serde_json::{json, Value};

use super::events::{dispatch, SessionEvent, SessionSource};
use super::{fake_pid_for_sid, HookServerState};

const SOURCE: SessionSource = SessionSource::Cursor;

/// First non-empty string under any of `keys`. Cursor's payloads have
/// shifted between snake_case and camelCase across versions.
fn first_string(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| {
        v.get(k)
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

fn session_key(v: &Value) -> String {
    first_string(
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

fn cwd(v: &Value) -> Option<String> {
    first_string(
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

fn pid_and_sid(v: &Value) -> (u32, String) {
    let sid = session_key(v);
    (fake_pid_for_sid(&sid), sid)
}

async fn session_start(
    AxumState(state): AxumState<HookServerState>,
    Json(v): Json<Value>,
) -> Json<Value> {
    let (pid, sid) = pid_and_sid(&v);
    let label = v
        .get("source")
        .or_else(|| v.get("type"))
        .and_then(|x| x.as_str())
        .unwrap_or("cursor")
        .to_string();
    dispatch(
        &state,
        pid,
        SOURCE,
        SessionEvent::SessionStarted {
            session_id: sid,
            cwd: cwd(&v),
            source_label: label,
        },
    )
    .await
}

async fn before_submit_prompt(
    AxumState(state): AxumState<HookServerState>,
    Json(v): Json<Value>,
) -> Json<Value> {
    let (pid, sid) = pid_and_sid(&v);
    let prompt = v
        .get("prompt")
        .or_else(|| v.get("text"))
        .or_else(|| v.get("message"))
        .and_then(|x| x.as_str())
        .map(str::to_string);
    dispatch(
        &state,
        pid,
        SOURCE,
        SessionEvent::UserPromptSubmitted {
            session_id: sid,
            cwd: cwd(&v),
            prompt,
        },
    )
    .await
}

/// `beforeShellExecution` — Cursor asks Coach whether to allow a shell
/// command. Maps to the domain `PermissionRequested` event (same away-
/// mode auto-approve semantics as Claude's permission-request hook).
async fn before_shell(
    AxumState(state): AxumState<HookServerState>,
    Json(v): Json<Value>,
) -> Json<Value> {
    let (pid, sid) = pid_and_sid(&v);
    dispatch(
        &state,
        pid,
        SOURCE,
        SessionEvent::PermissionRequested {
            session_id: sid,
            cwd: cwd(&v),
            tool_name: "Bash".into(),
        },
    )
    .await
}

/// `beforeMCPExecution` — same story as `beforeShellExecution`, but for
/// an MCP tool call.
async fn before_mcp(
    AxumState(state): AxumState<HookServerState>,
    Json(v): Json<Value>,
) -> Json<Value> {
    let (pid, sid) = pid_and_sid(&v);
    dispatch(
        &state,
        pid,
        SOURCE,
        SessionEvent::PermissionRequested {
            session_id: sid,
            cwd: cwd(&v),
            tool_name: "mcp".into(),
        },
    )
    .await
}

async fn after_shell(
    AxumState(state): AxumState<HookServerState>,
    Json(v): Json<Value>,
) -> Json<Value> {
    let (pid, sid) = pid_and_sid(&v);
    let cmd = v
        .get("command")
        .or_else(|| v.get("commandLine"))
        .and_then(|x| x.as_str())
        .unwrap_or("");
    dispatch(
        &state,
        pid,
        SOURCE,
        SessionEvent::ToolCompleted {
            session_id: sid,
            cwd: cwd(&v),
            tool_name: "Bash".into(),
            tool_input: json!({ "command": cmd }),
            tool_output: None,
        },
    )
    .await
}

async fn after_mcp(
    AxumState(state): AxumState<HookServerState>,
    Json(v): Json<Value>,
) -> Json<Value> {
    let (pid, sid) = pid_and_sid(&v);
    let cwd_val = cwd(&v);
    dispatch(
        &state,
        pid,
        SOURCE,
        SessionEvent::ToolCompleted {
            session_id: sid,
            cwd: cwd_val,
            tool_name: "mcp".into(),
            tool_input: v,
            tool_output: None,
        },
    )
    .await
}

/// `afterFileEdit` — Cursor's only file-edit signal. We collapse it into
/// `ToolCompleted { tool_name: "Edit" }` so the rules engine and observer
/// see the same event they'd see from Claude Code's PostToolUse(Edit) hook.
async fn after_file_edit(
    AxumState(state): AxumState<HookServerState>,
    Json(v): Json<Value>,
) -> Json<Value> {
    let (pid, sid) = pid_and_sid(&v);
    let new_string = v
        .get("newContent")
        .or_else(|| v.get("new_string"))
        .or_else(|| v.get("newString"))
        .and_then(|x| x.as_str())
        .unwrap_or("");
    dispatch(
        &state,
        pid,
        SOURCE,
        SessionEvent::ToolCompleted {
            session_id: sid,
            cwd: cwd(&v),
            tool_name: "Edit".into(),
            tool_input: json!({ "new_string": new_string }),
            tool_output: None,
        },
    )
    .await
}

async fn stop(
    AxumState(state): AxumState<HookServerState>,
    Json(v): Json<Value>,
) -> Json<Value> {
    let (pid, sid) = pid_and_sid(&v);
    let stop_reason = v
        .get("reason")
        .or_else(|| v.get("stopReason"))
        .or_else(|| v.get("stop_reason"))
        .and_then(|x| x.as_str())
        .map(str::to_string);
    dispatch(
        &state,
        pid,
        SOURCE,
        SessionEvent::StopRequested {
            session_id: sid,
            cwd: cwd(&v),
            stop_reason,
        },
    )
    .await
}

pub(crate) fn routes() -> Router<HookServerState> {
    Router::new()
        .route("/cursor/hook/session-start", post(session_start))
        .route(
            "/cursor/hook/before-submit-prompt",
            post(before_submit_prompt),
        )
        .route("/cursor/hook/before-shell", post(before_shell))
        .route("/cursor/hook/before-mcp", post(before_mcp))
        .route("/cursor/hook/after-shell", post(after_shell))
        .route("/cursor/hook/after-mcp", post(after_mcp))
        .route("/cursor/hook/after-file-edit", post(after_file_edit))
        .route("/cursor/hook/stop", post(stop))
}
