mod commands;
mod tray;

// Re-export coach_core publicly so integration tests can use `coach_lib::*`
// to reach types like `server::`, `settings::`, `state::`, etc.
pub use coach_core::*;

use coach_core::settings::Settings;
use coach_core::state::{CoachSnapshot, CoachState, SharedState, Theme};
use std::sync::Arc;
use tauri::Manager;
use tokio::sync::RwLock;

// ── TauriEmitter ──────────────────────────────────────────────────────

/// Bridges `coach_core::EventEmitter` to Tauri's `AppHandle::emit()`.
struct TauriEmitter {
    handle: tauri::AppHandle,
}

impl EventEmitter for TauriEmitter {
    fn emit_state_update(&self, snapshot: &CoachSnapshot) {
        use tauri::Emitter;
        let _ = self.handle.emit(coach_core::state::EVENT_STATE_UPDATED, snapshot);
    }

    fn emit_theme_changed(&self, theme: &Theme) {
        use tauri::Emitter;
        let _ = self.handle.emit(coach_core::state::EVENT_THEME_CHANGED, theme);
    }
}

// ── GUI entry point ───────────────────────────────────────────────────

pub fn run() {
    let log_path = coach_core::logging::init_for_app();
    eprintln!("[coach] starting up");
    if let Some(p) = &log_path {
        eprintln!("[coach] logging to {}", p.display());
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(|app| {
            eprintln!("[coach] setup: loading settings");
            let mut settings = Settings::load();
            let port = settings.port;
            eprintln!("[coach] setup: port={port}, priorities={:?}", settings.priorities);

            match settings::sync_managed_hooks(port, &mut settings.hooks_user_enabled) {
                Ok(added) if !added.is_empty() => {
                    eprintln!("[coach] setup: synced Claude hooks: {added:?}");
                }
                Ok(_) => {}
                Err(e) => eprintln!("[coach] setup: Claude hook sync failed: {e}"),
            }
            match settings::sync_managed_codex_hooks(port, &mut settings.codex_hooks_user_enabled) {
                Ok(added) if !added.is_empty() => {
                    eprintln!("[coach] setup: synced Codex hooks: {added:?}");
                }
                Ok(_) => {}
                Err(e) => eprintln!("[coach] setup: Codex hook sync failed: {e}"),
            }
            match settings::sync_managed_cursor_hooks(port, &mut settings.cursor_hooks_user_enabled)
            {
                Ok(added) if !added.is_empty() => {
                    eprintln!("[coach] setup: synced Cursor hooks: {added:?}");
                }
                Ok(_) => {}
                Err(e) => eprintln!("[coach] setup: Cursor hook sync failed: {e}"),
            }
            settings.save();

            let state: SharedState = Arc::new(RwLock::new(CoachState::from_settings(settings)));

            app.manage(state.clone());

            let emitter: Arc<dyn EventEmitter> = Arc::new(TauriEmitter {
                handle: app.handle().clone(),
            });

            let server_state = state.clone();
            let server_emitter = emitter.clone();
            tauri::async_runtime::spawn(async move {
                coach_core::server::start_server(server_state, server_emitter, port).await;
            });

            let scanner_state = state.clone();
            let scanner_emitter = emitter.clone();
            tauri::async_runtime::spawn(async move {
                coach_core::scanner::run_session_scanner(scanner_state, scanner_emitter).await;
            });

            tray::setup(app, state)?;

            if std::env::args().any(|a| a == "--devtools") {
                if let Some(w) = app.get_webview_window("main") {
                    w.open_devtools();
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
            commands::get_codex_hook_status,
            commands::install_codex_hooks,
            commands::uninstall_codex_hooks,
            commands::get_cursor_hook_status,
            commands::install_cursor_hooks,
            commands::uninstall_cursor_hooks,
            commands::set_auto_uninstall_hooks_on_exit,
            commands::list_saved_sessions,
            commands::replay_session,
            commands::set_coach_mode,
            commands::set_rules,
            commands::set_intervention_muted,
            commands::get_path_status,
            commands::install_path,
            commands::uninstall_path,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Coach")
        .run(|app_handle, event| match event {
            tauri::RunEvent::Reopen { .. } => {
                if let Some(w) = app_handle.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.unminimize();
                    let _ = w.set_focus();
                }
            }
            tauri::RunEvent::Exit => {
                eprintln!("[coach] exiting, running hook cleanup");
                settings::cleanup_hooks_on_exit();
            }
            _ => {}
        });
}
