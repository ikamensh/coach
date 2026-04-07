pub mod cli;
mod commands;
pub mod llm;
pub mod path_install;
pub mod pid_resolver;
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
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // Another instance tried to launch — show our existing window.
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .on_window_event(|window, event| {
            // Close-to-tray: hide window instead of quitting.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(|app| {
            eprintln!("[coach] setup: loading settings");
            let settings = Settings::load();
            let port = settings.port;
            eprintln!("[coach] setup: port={port}, priorities={:?}", settings.priorities);
            let state: SharedState = Arc::new(RwLock::new(CoachState::from_settings(settings)));

            // Top up Coach's managed hooks if the user has previously
            // opted in. Adds anything we've added to the managed set
            // since they last installed (e.g. SessionStart) without
            // making them click "Install Hooks" again. No-op on
            // unconfigured machines — first install stays explicit.
            match settings::topup_managed_hooks(port) {
                Ok(added) if !added.is_empty() => {
                    eprintln!("[coach] setup: topped up managed hooks: {added:?}");
                }
                Ok(_) => {}
                Err(e) => eprintln!("[coach] setup: hook top-up failed: {e}"),
            }
            match settings::topup_managed_cursor_hooks(port) {
                Ok(added) if !added.is_empty() => {
                    eprintln!("[coach] setup: topped up Cursor managed hooks: {added:?}");
                }
                Ok(_) => {}
                Err(e) => eprintln!("[coach] setup: Cursor hook top-up failed: {e}"),
            }

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
            commands::get_cursor_hook_status,
            commands::install_cursor_hooks,
            commands::uninstall_cursor_hooks,
            commands::list_saved_sessions,
            commands::replay_session,
            commands::set_coach_mode,
            commands::set_rules,
            commands::get_path_status,
            commands::install_path,
            commands::uninstall_path,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Coach");
}
