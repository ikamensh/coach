//! Codex CLI hooks use the same event names and payload format as Claude Code.
//! Hooks arrive via a shim script (curl subprocess), so we use synthetic PIDs
//! from session_id rather than lsof — same approach as Cursor.

use axum::extract::State as AxumState;
use axum::Json;
use serde_json::Value;

use super::{
    fake_pid_for_sid, run_permission_request, run_post_tool_use, run_pre_tool_use,
    run_session_start, run_stop, run_user_prompt_submit, AppState, HookPayload, HookResponse,
};
use crate::state::SessionClient;

async fn mark_codex(state: &AppState, pid: u32) {
    crate::state::mutate(&state.coach, &state.emitter, |coach| {
        coach.mark_client(pid, SessionClient::Codex);
    })
    .await;
}

fn codex_pid(payload: &HookPayload) -> u32 {
    let sid = payload.session_id.as_deref().unwrap_or("unknown");
    fake_pid_for_sid(sid)
}

macro_rules! codex_handler {
    ($name:ident, $run:ident, $ret:ty) => {
        pub async fn $name(
            AxumState(state): AxumState<AppState>,
            Json(payload): Json<HookPayload>,
        ) -> Json<$ret> {
            let pid = codex_pid(&payload);
            let resp = $run(&state, pid, payload).await;
            mark_codex(&state, pid).await;
            resp
        }
    };
}

codex_handler!(permission_request, run_permission_request, HookResponse);
codex_handler!(pre_tool_use, run_pre_tool_use, HookResponse);
codex_handler!(post_tool_use, run_post_tool_use, HookResponse);
codex_handler!(user_prompt_submit, run_user_prompt_submit, HookResponse);
codex_handler!(session_start, run_session_start, HookResponse);
codex_handler!(stop, run_stop, Value);
