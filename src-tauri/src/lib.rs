pub mod cli;
mod commands;
pub mod llm;
pub mod logging;
pub mod path_install;
pub mod pid_resolver;
pub mod prompts;
#[cfg(feature = "pycoach")]
pub mod pycoach;
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
    // Redirect stderr+stdout to a log file before any other output. After
    // this returns, every existing eprintln!/println! and any panic message
    // lands in `~/Library/Logs/Coach/coach.log` (or platform equivalent).
    let log_path = logging::init_for_app();
    eprintln!("[coach] starting up");
    if let Some(p) = &log_path {
        eprintln!("[coach] logging to {}", p.display());
    }
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
            let mut settings = Settings::load();
            let port = settings.port;
            eprintln!("[coach] setup: port={port}, priorities={:?}", settings.priorities);

            // Reconcile managed hooks with the user's recorded intent.
            // - First-time users: no-op (intent flag false, nothing on disk).
            // - Returning users (clean exit auto-cleanup): re-installs.
            // - Legacy users (hooks on disk, no flag yet): one-shot migration
            //   that flips the flag and tops up any newly-managed hooks.
            // Persist any flag changes back to disk before handing settings
            // off to CoachState — otherwise the migration would be lost on
            // the next restart.
            match settings::sync_managed_hooks(port, &mut settings.hooks_user_enabled) {
                Ok(added) if !added.is_empty() => {
                    eprintln!("[coach] setup: synced Claude hooks: {added:?}");
                }
                Ok(_) => {}
                Err(e) => eprintln!("[coach] setup: Claude hook sync failed: {e}"),
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

            spawn_pycoach_if_configured(state.clone());

            let server_state = state.clone();
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                server::start_server(server_state, Some(app_handle), port).await;
            });

            let scanner_state = state.clone();
            let scanner_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                scanner::run_session_scanner(scanner_state, Some(scanner_handle)).await;
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
            commands::set_auto_uninstall_hooks_on_exit,
            commands::list_saved_sessions,
            commands::replay_session,
            commands::set_coach_mode,
            commands::set_rules,
            commands::get_path_status,
            commands::install_path,
            commands::uninstall_path,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Coach")
        .run(|_app_handle, event| {
            // Run hook cleanup once when the event loop is exiting. We
            // re-read settings from disk rather than locking the in-memory
            // state from this sync callback — every command that mutates
            // the relevant flags also calls `Settings::save()` immediately,
            // so disk and memory agree.
            if matches!(event, tauri::RunEvent::Exit) {
                eprintln!("[coach] exiting, running hook cleanup");
                settings::cleanup_hooks_on_exit();
            }
        });
}

/// Headless daemon mode: start the HTTP hook server and the session
/// scanner without going through `tauri::Builder`. Reachable via the
/// `coach serve` CLI subcommand. Skipping the Tauri runtime is the
/// whole point — `tauri-plugin-single-instance` uses a global Unix
/// socket on macOS, so two GUI coach processes cannot coexist on the
/// same user account, which makes integration testing impossible.
/// Headless mode bypasses this and is what VM tests / CI / users who
/// just want the daemon should run.
///
/// Blocks until either the server task exits, the scanner task exits,
/// or the process receives Ctrl-C. Returns `Err` so the CLI exits
/// non-zero on bind failure or worker panic — that was the regression
/// the A5 user story caught.
pub async fn serve(port_override: Option<u16>) -> Result<(), String> {
    let mut settings = Settings::load();
    if let Some(p) = port_override {
        settings.port = p;
    }
    let port = settings.port;

    // Pre-bind the listener BEFORE printing the banner or spawning any
    // worker tasks. A port collision now becomes a clean Err propagated
    // out through `cmd_serve` to a non-zero exit code with a readable
    // message, instead of the previous "spawn → panic in worker → log
    // it → exit 0" path.
    let addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("failed to bind {addr}: {e}"))?;

    eprintln!("[coach serve] listening on {addr}, priorities={:?}", settings.priorities);

    // Reconcile managed hooks the same way the GUI path does so headless
    // and GUI behave identically. No-op on a fresh tempdir HOME.
    if let Ok(added) = settings::sync_managed_hooks(port, &mut settings.hooks_user_enabled) {
        if !added.is_empty() {
            eprintln!("[coach serve] synced Claude hooks: {added:?}");
        }
    }
    if let Ok(added) =
        settings::sync_managed_cursor_hooks(port, &mut settings.cursor_hooks_user_enabled)
    {
        if !added.is_empty() {
            eprintln!("[coach serve] synced Cursor hooks: {added:?}");
        }
    }
    settings.save();

    let state: SharedState = Arc::new(RwLock::new(CoachState::from_settings(settings)));

    spawn_pycoach_if_configured(state.clone());

    let server_task = tokio::spawn({
        let s = state.clone();
        async move { server::serve_on_listener(listener, s, None, port).await }
    });
    let scanner_task = tokio::spawn({
        let s = state.clone();
        async move { scanner::run_session_scanner(s, None).await }
    });

    let result = tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            eprintln!("[coach serve] received Ctrl-C, shutting down");
            Ok(())
        }
        r = server_task => {
            Err(format!("hook server task exited unexpectedly: {r:?}"))
        }
        r = scanner_task => {
            Err(format!("scanner task exited unexpectedly: {r:?}"))
        }
    };

    // Run hook cleanup on every shutdown path, not just clean Ctrl-C —
    // a worker panic on the way out is still a "Coach is gone" event for
    // the other live agent windows, and we want their hooks gone too.
    settings::cleanup_hooks_on_exit();

    result
}

/// Try to spawn the pycoach Python sidecar in the background.
///
/// No-op when the `pycoach` Cargo feature is disabled, or when no
/// `COACH_PYCOACH_*` env var is set. The sidecar is opt-in while its HTTP
/// contract is still moving. Failures are logged and swallowed: a missing
/// or broken sidecar must never block Coach startup, since the Rust LLM
/// backend keeps working without it.
#[cfg(feature = "pycoach")]
fn spawn_pycoach_if_configured(state: SharedState) {
    let Some(launcher) = pycoach::Pycoach::launcher_from_env() else {
        return;
    };
    tauri::async_runtime::spawn(async move {
        match pycoach::Pycoach::launch(launcher).await {
            Ok(py) => {
                eprintln!("[coach] pycoach sidecar ready at {}", py.base_url);
                state.write().await.pycoach = Some(Arc::new(py));
            }
            Err(e) => {
                eprintln!("[coach] pycoach sidecar failed to start: {e}");
            }
        }
    });
}

#[cfg(not(feature = "pycoach"))]
fn spawn_pycoach_if_configured(_state: SharedState) {}
