use axum::{
    extract::{Path, State as AxumState},
    http::StatusCode,
    Json,
};
use serde::Deserialize;

use super::{emit_update, AppState};
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
    Path(pid): Path<u32>,
    Json(payload): Json<ModePayload>,
) -> Result<Json<crate::state::CoachSnapshot>, (StatusCode, String)> {
    let mut s = state.coach.write().await;
    if !s.sessions.contains_key(&pid) {
        return Err((StatusCode::NOT_FOUND, format!("no session for pid {pid}")));
    }
    if let Some(sess) = s.sessions.get_mut(&pid) {
        sess.mode = payload.mode;
    }
    let snap = s.snapshot();
    emit_update(&state.emitter, &s);
    Ok(Json(snap))
}

pub(crate) async fn set_all_modes(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<ModePayload>,
) -> Json<crate::state::CoachSnapshot> {
    let mut s = state.coach.write().await;
    s.set_all_modes(payload.mode);
    let snap = s.snapshot();
    emit_update(&state.emitter, &s);
    Json(snap)
}

pub(crate) async fn set_priorities(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<PrioritiesPayload>,
) -> Json<crate::state::CoachSnapshot> {
    let mut s = state.coach.write().await;
    s.priorities = payload.priorities;
    s.save();
    let snap = s.snapshot();
    emit_update(&state.emitter, &s);
    Json(snap)
}

pub(crate) async fn set_model(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<ModelConfig>,
) -> Json<crate::state::CoachSnapshot> {
    let mut s = state.coach.write().await;
    s.model = payload;
    s.save();
    let snap = s.snapshot();
    emit_update(&state.emitter, &s);
    Json(snap)
}

pub(crate) async fn set_api_token(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<ApiTokenPayload>,
) -> Json<crate::state::CoachSnapshot> {
    let mut s = state.coach.write().await;
    if payload.token.is_empty() {
        s.api_tokens.remove(&payload.provider);
    } else {
        s.api_tokens.insert(payload.provider, payload.token);
    }
    s.save();
    let snap = s.snapshot();
    emit_update(&state.emitter, &s);
    Json(snap)
}

pub(crate) async fn set_coach_mode(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<CoachModePayload>,
) -> Json<crate::state::CoachSnapshot> {
    let mut s = state.coach.write().await;
    s.coach_mode = payload.coach_mode;
    s.save();
    let snap = s.snapshot();
    emit_update(&state.emitter, &s);
    Json(snap)
}

pub(crate) async fn set_rules(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<RulesPayload>,
) -> Json<crate::state::CoachSnapshot> {
    let mut s = state.coach.write().await;
    s.rules = payload.rules;
    s.save();
    let snap = s.snapshot();
    emit_update(&state.emitter, &s);
    Json(snap)
}
