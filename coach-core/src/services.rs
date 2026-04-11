//! Application service layer.
//!
//! One async function per config/session mutation operation. Tauri
//! commands and HTTP `/api/config/*` handlers both call into here so
//! the actual mutation lives in exactly one place. Each service wraps
//! `state::mutate`, which takes the write lock, runs the closure, and
//! emits a snapshot — so callers don't need to think about emit-after-
//! write at all.
//!
//! Hook event handling and observer/namer flows do **not** go through
//! services — they live on the hook path and have their own dispatcher
//! in `server::events`.

use std::sync::Arc;

use crate::settings::{CoachRule, EngineMode, HookTarget, ModelConfig};
use crate::state::{self, CoachMode, SharedState, Theme};
use crate::EventEmitter;

/// Errors a service can return when the request can't be honored.
/// Tauri and HTTP adapters map these into their transport-specific
/// error responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceError {
    SessionNotFound { session_id: String },
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceError::SessionNotFound { session_id } => {
                write!(f, "no session for id {session_id}")
            }
        }
    }
}

impl std::error::Error for ServiceError {}

// ── Session-scoped operations ─────────────────────────────────────────

pub async fn set_session_mode(
    state: &SharedState,
    emitter: &Arc<dyn EventEmitter>,
    session_id: String,
    mode: CoachMode,
) -> Result<(), ServiceError> {
    {
        let s = state.read().await;
        if !s.sessions.contains_key(&session_id) {
            return Err(ServiceError::SessionNotFound { session_id });
        }
    }
    state::mutate(state, emitter, |s| {
        s.sessions.set_session_mode(&session_id, mode);
    })
    .await;
    Ok(())
}

pub async fn set_intervention_muted(
    state: &SharedState,
    emitter: &Arc<dyn EventEmitter>,
    session_id: String,
    muted: bool,
) {
    state::mutate(state, emitter, |s| {
        s.sessions.set_intervention_muted(&session_id, muted);
    })
    .await;
}

pub async fn set_all_modes(
    state: &SharedState,
    emitter: &Arc<dyn EventEmitter>,
    mode: CoachMode,
) {
    state::mutate(state, emitter, |s| s.sessions.set_all_modes(mode)).await;
}

/// Flip the default session mode and apply it to all sessions. Returns
/// the new mode so callers (e.g. the tray) can refresh their UI.
pub async fn toggle_default_mode(
    state: &SharedState,
    emitter: &Arc<dyn EventEmitter>,
) -> CoachMode {
    state::mutate(state, emitter, |s| {
        let new_mode = match s.sessions.default_mode {
            CoachMode::Present => CoachMode::Away,
            CoachMode::Away => CoachMode::Present,
        };
        s.sessions.set_all_modes(new_mode);
        new_mode
    })
    .await
}

// ── Config-scoped operations ──────────────────────────────────────────

pub async fn set_priorities(
    state: &SharedState,
    emitter: &Arc<dyn EventEmitter>,
    priorities: Vec<String>,
) {
    state::mutate(state, emitter, |s| s.config.update_priorities(priorities)).await;
}

pub async fn set_model(
    state: &SharedState,
    emitter: &Arc<dyn EventEmitter>,
    model: ModelConfig,
) {
    state::mutate(state, emitter, |s| s.config.update_model(model)).await;
}

pub async fn set_api_token(
    state: &SharedState,
    emitter: &Arc<dyn EventEmitter>,
    provider: String,
    token: String,
) {
    state::mutate(state, emitter, |s| {
        s.config.update_api_token(&provider, &token);
    })
    .await;
}

/// Update the persisted theme **and** emit the dedicated
/// `coach-theme-changed` event so the frontend can swap stylesheets
/// without re-rendering the whole snapshot.
pub async fn set_theme(state: &SharedState, emitter: &Arc<dyn EventEmitter>, theme: Theme) {
    state::mutate(state, emitter, |s| s.config.update_theme(theme.clone())).await;
    emitter.emit_theme_changed(&theme);
}

pub async fn set_coach_mode(
    state: &SharedState,
    emitter: &Arc<dyn EventEmitter>,
    coach_mode: EngineMode,
) {
    state::mutate(state, emitter, |s| s.config.update_coach_mode(coach_mode)).await;
}

pub async fn set_rules(
    state: &SharedState,
    emitter: &Arc<dyn EventEmitter>,
    rules: Vec<CoachRule>,
) {
    state::mutate(state, emitter, |s| s.config.update_rules(rules)).await;
}

pub async fn set_auto_uninstall(
    state: &SharedState,
    emitter: &Arc<dyn EventEmitter>,
    enabled: bool,
) {
    state::mutate(state, emitter, |s| s.config.update_auto_uninstall(enabled)).await;
}

/// Persist the user's intent to use a given hook integration. Does
/// **not** touch the underlying `~/.claude/settings.json` etc. — the
/// caller invokes `target.install(port)` / `target.uninstall(port)`
/// after this. Returns the configured port so the caller doesn't have
/// to take a second read lock.
pub async fn set_hook_enabled(
    state: &SharedState,
    emitter: &Arc<dyn EventEmitter>,
    target: HookTarget,
    enabled: bool,
) -> u16 {
    state::mutate(state, emitter, |s| {
        s.config.set_hook_enabled(target, enabled);
        s.config.port
    })
    .await
}
