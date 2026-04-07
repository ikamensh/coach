use crate::path_install::{self, PathStatus};
use crate::replay;
use crate::settings::{self, CoachRule, EngineMode, HookStatus, ModelConfig};
use crate::state::{CoachMode, CoachSnapshot, CoachState, SharedState, Theme, EVENT_STATE_UPDATED, EVENT_THEME_CHANGED};
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
    if let Some(session) = s.sessions.get_mut(&pid) {
        session.mode = mode;
    }
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
    s.priorities = priorities;
    s.save();
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
    s.theme = theme.clone();
    s.save();
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
    if token.is_empty() {
        s.api_tokens.remove(&provider);
    } else {
        s.api_tokens.insert(provider, token);
    }
    s.save();
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
    s.model = model;
    s.save();
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

#[tauri::command]
pub async fn get_hook_status(state: tauri::State<'_, SharedState>) -> Result<HookStatus, String> {
    let s = state.read().await;
    Ok(settings::check_hook_status(s.port))
}

#[tauri::command]
pub async fn install_hooks(state: tauri::State<'_, SharedState>) -> Result<HookStatus, String> {
    let port = state.read().await.port;
    settings::install_hooks(port)?;
    Ok(settings::check_hook_status(port))
}

#[tauri::command]
pub async fn uninstall_hooks(state: tauri::State<'_, SharedState>) -> Result<HookStatus, String> {
    let port = state.read().await.port;
    settings::uninstall_hooks(port)?;
    Ok(settings::check_hook_status(port))
}

#[tauri::command]
pub async fn get_cursor_hook_status(
    _state: tauri::State<'_, SharedState>,
) -> Result<HookStatus, String> {
    Ok(settings::check_cursor_hook_status())
}

#[tauri::command]
pub async fn install_cursor_hooks(
    state: tauri::State<'_, SharedState>,
) -> Result<HookStatus, String> {
    let port = state.read().await.port;
    settings::install_cursor_hooks(port)?;
    Ok(settings::check_cursor_hook_status())
}

#[tauri::command]
pub async fn uninstall_cursor_hooks(
    _state: tauri::State<'_, SharedState>,
) -> Result<HookStatus, String> {
    settings::uninstall_cursor_hooks()?;
    Ok(settings::check_cursor_hook_status())
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
    s.coach_mode = coach_mode;
    s.save();
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
    s.rules = rules;
    s.save();
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
