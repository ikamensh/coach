use axum::{
    extract::State as AxumState,
    routing::{get, post},
    Json, Router,
};
use std::net::SocketAddr;
use std::sync::Arc;

use crate::state::SharedState;
use crate::EventEmitter;

mod api;
mod claude;
mod codex;
mod cursor;
mod events;
mod observer;
mod rules;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) coach: SharedState,
    pub(crate) emitter: Arc<dyn EventEmitter>,
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

fn build_router(coach: SharedState, emitter: Arc<dyn EventEmitter>) -> Router {
    let state = AppState { coach, emitter };
    Router::new()
        .merge(claude::routes())
        .merge(codex::routes())
        .merge(cursor::routes())
        .route("/state", get(handle_get_state))
        .route("/version", get(handle_version))
        // CLI-facing API. Mirrors Tauri commands; same in-memory state.
        .route("/api/state", get(handle_get_state))
        .route("/api/sessions/mode", post(api::set_all_modes))
        .route("/api/sessions/{session_id}/mode", post(api::set_session_mode))
        .route("/api/config/priorities", post(api::set_priorities))
        .route("/api/config/model", post(api::set_model))
        .route("/api/config/api-token", post(api::set_api_token))
        .route("/api/config/coach-mode", post(api::set_coach_mode))
        .route("/api/config/rules", post(api::set_rules))
        .with_state(state)
}

/// Router for integration tests — no Tauri emitter.
pub fn create_router_headless(coach: SharedState) -> Router {
    build_router(coach, Arc::new(crate::NoopEmitter))
}

/// Hash a hook session_id to a stable, non-zero u32. Used as the
/// synthetic PID for agents whose hooks arrive via a shim subprocess
/// (Cursor, Codex), where the TCP peer isn't the agent itself. Also
/// exposed so integration tests can compute the same PID from the
/// session_id they posted.
pub fn fake_pid_for_sid(sid: &str) -> u32 {
    let mut h: u32 = 1;
    for b in sid.bytes() {
        h = h.wrapping_mul(31).wrapping_add(b as u32);
    }
    h | 1
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
    emitter: Arc<dyn EventEmitter>,
    port: u16,
) {
    let addr = format!("127.0.0.1:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("Failed to bind to {}: {}", addr, e));
    eprintln!("Coach hook server listening on {}", addr);
    serve_on_listener(listener, coach, emitter).await;
}

/// Serve hook traffic on an already-bound listener. Used by the
/// headless `serve()` path so it can pre-bind, fail fast on port
/// collisions, and *then* announce success.
pub async fn serve_on_listener(
    listener: tokio::net::TcpListener,
    coach: SharedState,
    emitter: Arc<dyn EventEmitter>,
) {
    let app = build_router(coach, emitter);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .expect("Hook server crashed");
}
