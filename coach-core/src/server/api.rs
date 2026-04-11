use axum::{
    extract::{Path, State as AxumState},
    http::StatusCode,
    Json,
};
use serde::Deserialize;

use super::AppState;
use crate::settings::{CoachRule, EngineMode, ModelConfig};
use crate::state::CoachMode;

// ── /api/* endpoints used by the CLI when Coach is running ──────────────
//
// These mirror the Tauri commands in commands.rs so the CLI never has to
// touch ~/.coach/settings.json directly while the GUI is up. Each handler
// mutates the in-memory state, persists to disk, and emits the same
// `coach-state-updated` event the Tauri commands emit so the GUI refreshes.

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

pub(crate) async fn set_session_mode(
    AxumState(state): AxumState<AppState>,
    Path(session_id): Path<String>,
    Json(payload): Json<ModePayload>,
) -> Result<Json<crate::state::CoachSnapshot>, (StatusCode, String)> {
    {
        let s = state.coach.read().await;
        if !s.sessions.contains_key(&session_id) {
            return Err((
                StatusCode::NOT_FOUND,
                format!("no session for id {session_id}"),
            ));
        }
    }
    let snap = crate::state::mutate(&state.coach, &state.emitter, |s| {
        s.sessions.set_session_mode(&session_id, payload.mode);
        s.snapshot()
    })
    .await;
    Ok(Json(snap))
}

pub(crate) async fn set_all_modes(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<ModePayload>,
) -> Json<crate::state::CoachSnapshot> {
    let snap = crate::state::mutate(&state.coach, &state.emitter, |s| {
        s.sessions.set_all_modes(payload.mode);
        s.snapshot()
    })
    .await;
    Json(snap)
}

pub(crate) async fn set_priorities(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<PrioritiesPayload>,
) -> Json<crate::state::CoachSnapshot> {
    let snap = crate::state::mutate(&state.coach, &state.emitter, |s| {
        s.config.update_priorities(payload.priorities);
        s.snapshot()
    })
    .await;
    Json(snap)
}

pub(crate) async fn set_model(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<ModelConfig>,
) -> Json<crate::state::CoachSnapshot> {
    let snap = crate::state::mutate(&state.coach, &state.emitter, |s| {
        s.config.update_model(payload);
        s.snapshot()
    })
    .await;
    Json(snap)
}

pub(crate) async fn set_api_token(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<ApiTokenPayload>,
) -> Json<crate::state::CoachSnapshot> {
    let snap = crate::state::mutate(&state.coach, &state.emitter, |s| {
        s.config.update_api_token(&payload.provider, &payload.token);
        s.snapshot()
    })
    .await;
    Json(snap)
}

pub(crate) async fn set_coach_mode(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<CoachModePayload>,
) -> Json<crate::state::CoachSnapshot> {
    let snap = crate::state::mutate(&state.coach, &state.emitter, |s| {
        s.config.update_coach_mode(payload.coach_mode);
        s.snapshot()
    })
    .await;
    Json(snap)
}

pub(crate) async fn set_rules(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<RulesPayload>,
) -> Json<crate::state::CoachSnapshot> {
    let snap = crate::state::mutate(&state.coach, &state.emitter, |s| {
        s.config.update_rules(payload.rules);
        s.snapshot()
    })
    .await;
    Json(snap)
}
