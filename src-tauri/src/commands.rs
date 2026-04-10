use coach_core::path_install::{self, PathStatus};
use coach_core::replay;
use coach_core::settings::{CoachRule, EngineMode, HookStatus, HookTarget, ModelConfig};
use coach_core::state::{CoachMode, CoachSnapshot, CoachState, SharedState, Theme, EVENT_STATE_UPDATED, EVENT_THEME_CHANGED};
use serde_json::json;
use tauri::Emitter;

fn emit_snapshot(app: &tauri::AppHandle, state: &CoachState) -> Result<(), String> {
    app.emit(EVENT_STATE_UPDATED, &state.snapshot())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_state(state: tauri::State<'_, SharedState>) -> Result<CoachSnapshot, String> {
    let s = state.read().await;
    Ok(s.snapshot())
}

#[tauri::command]
pub async fn set_session_mode(
    state: tauri::State<'_, SharedState>,
    app: tauri::AppHandle,
    pid: u32,
    mode: CoachMode,
) -> Result<(), String> {
    let mut s = state.write().await;
    s.set_session_mode(pid, mode);
    emit_snapshot(&app, &s)?;
    Ok(())
}

#[tauri::command]
pub async fn set_all_sessions_mode(
    state: tauri::State<'_, SharedState>,
    app: tauri::AppHandle,
    mode: CoachMode,
) -> Result<(), String> {
    let mut s = state.write().await;
    s.set_all_modes(mode);
    crate::tray::update_icon(&app, &mode);
    emit_snapshot(&app, &s)?;
    Ok(())
}

#[tauri::command]
pub async fn set_priorities(
    state: tauri::State<'_, SharedState>,
    app: tauri::AppHandle,
    priorities: Vec<String>,
) -> Result<(), String> {
    let mut s = state.write().await;
    s.update_priorities(priorities);
    emit_snapshot(&app, &s)?;
    Ok(())
}

#[tauri::command]
pub async fn set_theme(
    state: tauri::State<'_, SharedState>,
    app: tauri::AppHandle,
    theme: Theme,
) -> Result<(), String> {
    let mut s = state.write().await;
    s.update_theme(theme.clone());
    app.emit(EVENT_THEME_CHANGED, &theme)
        .map_err(|e| e.to_string())?;
    emit_snapshot(&app, &s)?;
    Ok(())
}

#[tauri::command]
pub async fn set_api_token(
    state: tauri::State<'_, SharedState>,
    app: tauri::AppHandle,
    provider: String,
    token: String,
) -> Result<(), String> {
    let mut s = state.write().await;
    s.update_api_token(&provider, &token);
    emit_snapshot(&app, &s)?;
    Ok(())
}

#[tauri::command]
pub async fn set_model(
    state: tauri::State<'_, SharedState>,
    app: tauri::AppHandle,
    model: ModelConfig,
) -> Result<(), String> {
    let mut s = state.write().await;
    s.update_model(model);
    emit_snapshot(&app, &s)?;
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
    let client = s.http_client.clone();
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
    Ok(target.check_status(s.port))
}

async fn do_install_hooks(
    state: &SharedState,
    app: &tauri::AppHandle,
    target: HookTarget,
) -> Result<HookStatus, String> {
    let mut s = state.write().await;
    s.set_hook_enabled(target, true);
    target.install(s.port)?;
    emit_snapshot(app, &s)?;
    Ok(target.check_status(s.port))
}

async fn do_uninstall_hooks(
    state: &SharedState,
    app: &tauri::AppHandle,
    target: HookTarget,
) -> Result<HookStatus, String> {
    let mut s = state.write().await;
    s.set_hook_enabled(target, false);
    target.uninstall(s.port)?;
    emit_snapshot(app, &s)?;
    Ok(target.check_status(s.port))
}

#[tauri::command]
pub async fn get_hook_status(state: tauri::State<'_, SharedState>) -> Result<HookStatus, String> {
    do_get_hook_status(&state, HookTarget::Claude).await
}

#[tauri::command]
pub async fn install_hooks(state: tauri::State<'_, SharedState>, app: tauri::AppHandle) -> Result<HookStatus, String> {
    do_install_hooks(&state, &app, HookTarget::Claude).await
}

#[tauri::command]
pub async fn uninstall_hooks(state: tauri::State<'_, SharedState>, app: tauri::AppHandle) -> Result<HookStatus, String> {
    do_uninstall_hooks(&state, &app, HookTarget::Claude).await
}

#[tauri::command]
pub async fn get_codex_hook_status(state: tauri::State<'_, SharedState>) -> Result<HookStatus, String> {
    do_get_hook_status(&state, HookTarget::Codex).await
}

#[tauri::command]
pub async fn install_codex_hooks(state: tauri::State<'_, SharedState>, app: tauri::AppHandle) -> Result<HookStatus, String> {
    do_install_hooks(&state, &app, HookTarget::Codex).await
}

#[tauri::command]
pub async fn uninstall_codex_hooks(state: tauri::State<'_, SharedState>, app: tauri::AppHandle) -> Result<HookStatus, String> {
    do_uninstall_hooks(&state, &app, HookTarget::Codex).await
}

#[tauri::command]
pub async fn get_cursor_hook_status(state: tauri::State<'_, SharedState>) -> Result<HookStatus, String> {
    do_get_hook_status(&state, HookTarget::Cursor).await
}

#[tauri::command]
pub async fn install_cursor_hooks(state: tauri::State<'_, SharedState>, app: tauri::AppHandle) -> Result<HookStatus, String> {
    do_install_hooks(&state, &app, HookTarget::Cursor).await
}

#[tauri::command]
pub async fn uninstall_cursor_hooks(state: tauri::State<'_, SharedState>, app: tauri::AppHandle) -> Result<HookStatus, String> {
    do_uninstall_hooks(&state, &app, HookTarget::Cursor).await
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
    app: tauri::AppHandle,
    coach_mode: EngineMode,
) -> Result<(), String> {
    let mut s = state.write().await;
    s.update_coach_mode(coach_mode);
    emit_snapshot(&app, &s)?;
    Ok(())
}

#[tauri::command]
pub async fn set_rules(
    state: tauri::State<'_, SharedState>,
    app: tauri::AppHandle,
    rules: Vec<CoachRule>,
) -> Result<(), String> {
    let mut s = state.write().await;
    s.update_rules(rules);
    emit_snapshot(&app, &s)?;
    Ok(())
}

#[tauri::command]
pub async fn set_auto_uninstall_hooks_on_exit(
    state: tauri::State<'_, SharedState>,
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<(), String> {
    let mut s = state.write().await;
    s.update_auto_uninstall(enabled);
    emit_snapshot(&app, &s)?;
    Ok(())
}

#[tauri::command]
pub async fn set_intervention_muted(
    state: tauri::State<'_, SharedState>,
    app: tauri::AppHandle,
    pid: u32,
    muted: bool,
) -> Result<(), String> {
    let mut s = state.write().await;
    s.set_intervention_muted(pid, muted);
    emit_snapshot(&app, &s)?;
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
