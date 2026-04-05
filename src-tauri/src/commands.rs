use crate::settings::{self, HookStatus, ModelConfig};
use crate::state::{CoachMode, CoachSnapshot, SharedState, Theme};
use serde_json::json;
use tauri::Emitter;

#[tauri::command]
pub async fn get_state(state: tauri::State<'_, SharedState>) -> Result<CoachSnapshot, String> {
    let s = state.read().await;
    Ok(s.snapshot())
}

#[tauri::command]
pub async fn set_session_mode(
    state: tauri::State<'_, SharedState>,
    app: tauri::AppHandle,
    session_id: String,
    mode: CoachMode,
) -> Result<(), String> {
    let mut s = state.write().await;
    if let Some(session) = s.sessions.get_mut(&session_id) {
        session.mode = mode;
    }
    let snapshot = s.snapshot();
    app.emit("coach-state-updated", &snapshot)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn set_all_sessions_mode(
    state: tauri::State<'_, SharedState>,
    app: tauri::AppHandle,
    mode: CoachMode,
) -> Result<(), String> {
    let mut s = state.write().await;
    s.default_mode = mode.clone();
    for session in s.sessions.values_mut() {
        session.mode = mode.clone();
    }
    let snapshot = s.snapshot();
    app.emit("coach-state-updated", &snapshot)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn set_priorities(
    state: tauri::State<'_, SharedState>,
    priorities: Vec<String>,
) -> Result<(), String> {
    let mut s = state.write().await;
    s.priorities = priorities;
    s.save();
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
    app.emit("coach-theme-changed", &theme)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn set_api_token(
    state: tauri::State<'_, SharedState>,
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
    Ok(())
}

#[tauri::command]
pub async fn set_model(
    state: tauri::State<'_, SharedState>,
    model: ModelConfig,
) -> Result<(), String> {
    let mut s = state.write().await;
    s.model = model;
    s.save();
    Ok(())
}

/// Ping the provider to verify the model + key combo works.
/// Returns Ok(()) on success, Err(message) on failure.
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
    drop(s);

    let client = reqwest::Client::new();

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
            let resp = client
                .post("https://api.anthropic.com/v1/messages")
                .header("x-api-key", &token)
                .header("anthropic-version", "2023-06-01")
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
        }
        "openrouter" => {
            let resp = client
                .post("https://openrouter.ai/api/v1/chat/completions")
                .header("Authorization", format!("Bearer {}", token))
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
