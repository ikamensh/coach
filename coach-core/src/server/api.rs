use axum::{
    extract::{Path, State as AxumState},
    http::StatusCode,
    Json,
};
use serde::Deserialize;

use super::HookServerState;
use crate::services::{self, ServiceError};
use crate::settings::{CoachRule, EngineMode, ModelConfig};
use crate::state::CoachMode;

// ── /api/* endpoints used by the CLI when Coach is running ──────────────
//
// These mirror the Tauri commands in commands.rs so the CLI never has to
// touch ~/.coach/settings.json directly while the GUI is up. Each handler
// parses its JSON body, calls a service function (which mutates the
// in-memory state, persists to disk, and emits the same
// `coach-state-updated` event the Tauri commands emit), then returns a
// fresh snapshot.

#[derive(Deserialize)]
pub(crate) struct ModePayload {
    mode: CoachMode,
}

#[derive(Deserialize)]
pub(crate) struct PrioritiesPayload {
    priorities: Vec<String>,
}

#[derive(Deserialize)]
pub(crate) struct ApiTokenPayload {
    provider: String,
    token: String,
}

#[derive(Deserialize)]
pub(crate) struct CoachModePayload {
    coach_mode: EngineMode,
}

#[derive(Deserialize)]
pub(crate) struct RulesPayload {
    rules: Vec<CoachRule>,
}

/// After a service mutation, take a quick read lock and return the
/// snapshot. The mutation already emitted; the snapshot we send back is
/// what the CLI consumes.
async fn snapshot(state: &HookServerState) -> crate::state::CoachSnapshot {
    state.app.read().await.snapshot()
}

pub(crate) async fn set_session_mode(
    AxumState(state): AxumState<HookServerState>,
    Path(session_id): Path<String>,
    Json(payload): Json<ModePayload>,
) -> Result<Json<crate::state::CoachSnapshot>, (StatusCode, String)> {
    match services::set_session_mode(&state.app, &state.emitter, session_id, payload.mode).await
    {
        Ok(()) => Ok(Json(snapshot(&state).await)),
        Err(ServiceError::SessionNotFound { .. }) => {
            Err((StatusCode::NOT_FOUND, "no session for that id".to_string()))
        }
    }
}

pub(crate) async fn set_all_modes(
    AxumState(state): AxumState<HookServerState>,
    Json(payload): Json<ModePayload>,
) -> Json<crate::state::CoachSnapshot> {
    services::set_all_modes(&state.app, &state.emitter, payload.mode).await;
    Json(snapshot(&state).await)
}

pub(crate) async fn set_priorities(
    AxumState(state): AxumState<HookServerState>,
    Json(payload): Json<PrioritiesPayload>,
) -> Json<crate::state::CoachSnapshot> {
    services::set_priorities(&state.app, &state.emitter, payload.priorities).await;
    Json(snapshot(&state).await)
}

pub(crate) async fn set_model(
    AxumState(state): AxumState<HookServerState>,
    Json(payload): Json<ModelConfig>,
) -> Json<crate::state::CoachSnapshot> {
    services::set_model(&state.app, &state.emitter, payload).await;
    Json(snapshot(&state).await)
}

pub(crate) async fn set_api_token(
    AxumState(state): AxumState<HookServerState>,
    Json(payload): Json<ApiTokenPayload>,
) -> Json<crate::state::CoachSnapshot> {
    services::set_api_token(&state.app, &state.emitter, payload.provider, payload.token).await;
    Json(snapshot(&state).await)
}

pub(crate) async fn set_coach_mode(
    AxumState(state): AxumState<HookServerState>,
    Json(payload): Json<CoachModePayload>,
) -> Json<crate::state::CoachSnapshot> {
    services::set_coach_mode(&state.app, &state.emitter, payload.coach_mode).await;
    Json(snapshot(&state).await)
}

pub(crate) async fn set_rules(
    AxumState(state): AxumState<HookServerState>,
    Json(payload): Json<RulesPayload>,
) -> Json<crate::state::CoachSnapshot> {
    services::set_rules(&state.app, &state.emitter, payload.rules).await;
    Json(snapshot(&state).await)
}
