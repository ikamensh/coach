mod commands;
pub mod server;
pub mod settings;
pub mod state;
mod tray;

use settings::Settings;
use state::{CoachState, SharedState};
use std::sync::Arc;
use tauri::Manager;
use tokio::sync::RwLock;

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let settings = Settings::load();
            let port = settings.port;
            let state: SharedState = Arc::new(RwLock::new(CoachState::from_settings(settings)));

            app.manage(state.clone());

            let server_state = state.clone();
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                server::start_server(server_state, app_handle, port).await;
            });

            tray::setup(app, state)?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::set_session_mode,
            commands::set_all_sessions_mode,
            commands::set_priorities,
            commands::set_theme,
            commands::set_api_token,
            commands::set_model,
            commands::validate_model,
            commands::get_hook_status,
            commands::install_hooks,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Coach");
}
