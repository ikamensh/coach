mod commands;
pub mod llm;
pub mod replay;
pub mod rules;
pub mod scanner;
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
    eprintln!("[coach] starting up");
    tauri::Builder::default()
        .setup(|app| {
            eprintln!("[coach] setup: loading settings");
            let settings = Settings::load();
            let port = settings.port;
            eprintln!("[coach] setup: port={port}, priorities={:?}", settings.priorities);
            let state: SharedState = Arc::new(RwLock::new(CoachState::from_settings(settings)));

            app.manage(state.clone());

            let server_state = state.clone();
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                server::start_server(server_state, app_handle, port).await;
            });

            let scanner_state = state.clone();
            let scanner_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                scanner::run_session_scanner(scanner_state, scanner_handle).await;
            });

            tray::setup(app, state)?;

            if std::env::args().any(|a| a == "--devtools") {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.open_devtools();
                }
            }

            eprintln!("[coach] setup: complete");
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
            commands::uninstall_hooks,
            commands::list_saved_sessions,
            commands::replay_session,
            commands::set_coach_mode,
            commands::set_rules,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Coach");
}
