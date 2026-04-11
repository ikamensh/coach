use coach_core::path_install::{self, PathStatus};
use coach_core::replay;
use coach_core::settings::{CoachRule, EngineMode, HookStatus, HookTarget, ModelConfig};
use coach_core::state::{self, CoachMode, CoachSnapshot, SharedState, Theme};
use coach_core::EventEmitter;
use serde_json::json;
use std::sync::Arc;

type Emitter<'a> = tauri::State<'a, Arc<dyn EventEmitter>>;

#[tauri::command]
pub async fn get_state(state: tauri::State<'_, SharedState>) -> Result<CoachSnapshot, String> {
    let s = state.read().await;
    Ok(s.snapshot())
}

#[tauri::command]
pub async fn set_session_mode(
    state: tauri::State<'_, SharedState>,
    emitter: Emitter<'_>,
    session_id: String,
    mode: CoachMode,
) -> Result<(), String> {
    state::mutate(&state, emitter.inner(), |s| {
        s.sessions.set_session_mode(&session_id, mode)
    })
    .await;
    Ok(())
}

#[tauri::command]
pub async fn set_all_sessions_mode(
    state: tauri::State<'_, SharedState>,
    emitter: Emitter<'_>,
    app: tauri::AppHandle,
    mode: CoachMode,
) -> Result<(), String> {
    state::mutate(&state, emitter.inner(), |s| s.sessions.set_all_modes(mode)).await;
    crate::tray::update_icon(&app, &mode);
    Ok(())
}

#[tauri::command]
pub async fn set_priorities(
    state: tauri::State<'_, SharedState>,
    emitter: Emitter<'_>,
    priorities: Vec<String>,
) -> Result<(), String> {
    state::mutate(&state, emitter.inner(), |s| {
        s.config.update_priorities(priorities)
    })
    .await;
    Ok(())
}

#[tauri::command]
pub async fn set_theme(
    state: tauri::State<'_, SharedState>,
    emitter: Emitter<'_>,
    theme: Theme,
) -> Result<(), String> {
    state::mutate(&state, emitter.inner(), |s| s.config.update_theme(theme.clone())).await;
    emitter.emit_theme_changed(&theme);
    Ok(())
}

#[tauri::command]
pub async fn set_api_token(
    state: tauri::State<'_, SharedState>,
    emitter: Emitter<'_>,
    provider: String,
    token: String,
) -> Result<(), String> {
    state::mutate(&state, emitter.inner(), |s| {
        s.config.update_api_token(&provider, &token)
    })
    .await;
    Ok(())
}

#[tauri::command]
pub async fn set_model(
    state: tauri::State<'_, SharedState>,
    emitter: Emitter<'_>,
    model: ModelConfig,
) -> Result<(), String> {
    state::mutate(&state, emitter.inner(), |s| s.config.update_model(model)).await;
    Ok(())
}

async fn validate_chat_endpoint(
    client: &reqwest::Client,
    url: &str,
    auth_header: (&str, &str),
    model: &str,
) -> Result<(), String> {
    let resp = client
        .post(url)
        .header(auth_header.0, auth_header.1)
        .json(&json!({
            "model": model,
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        if body.contains("model") {
            return Err(format!("Model '{}' not found", model));
        }
        return Err("API key invalid or request failed".into());
    }
    Ok(())
}

#[tauri::command]
pub async fn validate_model(
    state: tauri::State<'_, SharedState>,
    provider: String,
    model: String,
) -> Result<(), String> {
    let s = state.read().await;
    let token = s
        .effective_token(&provider)
        .ok_or("No API key configured")?
        .to_string();
    let client = s.services.http_client.clone();
    drop(s);

    match provider.as_str() {
        "google" => {
            let url = format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{}?key={}",
                model, token
            );
            let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("Model '{}' not found", model));
            }
        }
        "openai" => {
            let url = format!("https://api.openai.com/v1/models/{}", model);
            let resp = client
                .get(&url)
                .header("Authorization", format!("Bearer {}", token))
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("Model '{}' not found or key invalid", model));
            }
        }
        "anthropic" => {
            validate_chat_endpoint(
                &client,
                "https://api.anthropic.com/v1/messages",
                ("x-api-key", &token),
                &model,
            )
            .await?;
        }
        "openrouter" => {
            let bearer = format!("Bearer {}", token);
            validate_chat_endpoint(
                &client,
                "https://openrouter.ai/api/v1/chat/completions",
                ("Authorization", &bearer),
                &model,
            )
            .await?;
        }
        _ => return Err(format!("Unknown provider '{}'", provider)),
    }

    Ok(())
}

// ── Hook management (shared helpers + per-client Tauri commands) ─────

async fn do_get_hook_status(state: &SharedState, target: HookTarget) -> Result<HookStatus, String> {
    let s = state.read().await;
    Ok(target.check_status(s.config.port))
}

async fn do_install_hooks(
    state: &SharedState,
    emitter: &Arc<dyn EventEmitter>,
    target: HookTarget,
) -> Result<HookStatus, String> {
    let port = state::mutate(state, emitter, |s| {
        s.config.set_hook_enabled(target, true);
        s.config.port
    })
    .await;
    target.install(port)?;
    Ok(target.check_status(port))
}

async fn do_uninstall_hooks(
    state: &SharedState,
    emitter: &Arc<dyn EventEmitter>,
    target: HookTarget,
) -> Result<HookStatus, String> {
    let port = state::mutate(state, emitter, |s| {
        s.config.set_hook_enabled(target, false);
        s.config.port
    })
    .await;
    target.uninstall(port)?;
    Ok(target.check_status(port))
}

#[tauri::command]
pub async fn get_hook_status(state: tauri::State<'_, SharedState>) -> Result<HookStatus, String> {
    do_get_hook_status(&state, HookTarget::Claude).await
}

#[tauri::command]
pub async fn install_hooks(state: tauri::State<'_, SharedState>, emitter: Emitter<'_>) -> Result<HookStatus, String> {
    do_install_hooks(&state, emitter.inner(), HookTarget::Claude).await
}

#[tauri::command]
pub async fn uninstall_hooks(state: tauri::State<'_, SharedState>, emitter: Emitter<'_>) -> Result<HookStatus, String> {
    do_uninstall_hooks(&state, emitter.inner(), HookTarget::Claude).await
}

#[tauri::command]
pub async fn get_codex_hook_status(state: tauri::State<'_, SharedState>) -> Result<HookStatus, String> {
    do_get_hook_status(&state, HookTarget::Codex).await
}

#[tauri::command]
pub async fn install_codex_hooks(state: tauri::State<'_, SharedState>, emitter: Emitter<'_>) -> Result<HookStatus, String> {
    do_install_hooks(&state, emitter.inner(), HookTarget::Codex).await
}

#[tauri::command]
pub async fn uninstall_codex_hooks(state: tauri::State<'_, SharedState>, emitter: Emitter<'_>) -> Result<HookStatus, String> {
    do_uninstall_hooks(&state, emitter.inner(), HookTarget::Codex).await
}

#[tauri::command]
pub async fn get_cursor_hook_status(state: tauri::State<'_, SharedState>) -> Result<HookStatus, String> {
    do_get_hook_status(&state, HookTarget::Cursor).await
}

#[tauri::command]
pub async fn install_cursor_hooks(state: tauri::State<'_, SharedState>, emitter: Emitter<'_>) -> Result<HookStatus, String> {
    do_install_hooks(&state, emitter.inner(), HookTarget::Cursor).await
}

#[tauri::command]
pub async fn uninstall_cursor_hooks(state: tauri::State<'_, SharedState>, emitter: Emitter<'_>) -> Result<HookStatus, String> {
    do_uninstall_hooks(&state, emitter.inner(), HookTarget::Cursor).await
}

#[tauri::command]
pub async fn list_saved_sessions(limit: Option<usize>) -> Result<Vec<replay::SavedSession>, String> {
    let limit = limit.unwrap_or(50);
    Ok(replay::list_sessions(limit))
}

#[tauri::command]
pub async fn replay_session(
    state: tauri::State<'_, SharedState>,
    session_id: String,
    mode: Option<String>,
) -> Result<replay::ReplayResult, String> {
    let mode = mode.unwrap_or_else(|| "away".to_string());
    replay::replay_session(&session_id, &mode, state.inner()).await
}

#[tauri::command]
pub async fn set_coach_mode(
    state: tauri::State<'_, SharedState>,
    emitter: Emitter<'_>,
    coach_mode: EngineMode,
) -> Result<(), String> {
    state::mutate(&state, emitter.inner(), |s| {
        s.config.update_coach_mode(coach_mode)
    })
    .await;
    Ok(())
}

#[tauri::command]
pub async fn set_rules(
    state: tauri::State<'_, SharedState>,
    emitter: Emitter<'_>,
    rules: Vec<CoachRule>,
) -> Result<(), String> {
    state::mutate(&state, emitter.inner(), |s| s.config.update_rules(rules)).await;
    Ok(())
}

#[tauri::command]
pub async fn set_auto_uninstall_hooks_on_exit(
    state: tauri::State<'_, SharedState>,
    emitter: Emitter<'_>,
    enabled: bool,
) -> Result<(), String> {
    state::mutate(&state, emitter.inner(), |s| {
        s.config.update_auto_uninstall(enabled)
    })
    .await;
    Ok(())
}

#[tauri::command]
pub async fn set_intervention_muted(
    state: tauri::State<'_, SharedState>,
    emitter: Emitter<'_>,
    session_id: String,
    muted: bool,
) -> Result<(), String> {
    state::mutate(&state, emitter.inner(), |s| {
        s.sessions.set_intervention_muted(&session_id, muted)
    })
    .await;
    Ok(())
}

// ── PATH shim management ────────────────────────────────────────────────

#[tauri::command]
pub async fn get_path_status() -> Result<PathStatus, String> {
    path_install::status()
}

#[tauri::command]
pub async fn install_path() -> Result<PathStatus, String> {
    path_install::install()
}

#[tauri::command]
pub async fn uninstall_path() -> Result<PathStatus, String> {
    path_install::uninstall()
}
