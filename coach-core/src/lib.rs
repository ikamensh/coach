pub mod cli;
pub mod coach;
pub mod llm;
pub mod logging;
pub mod path_install;
pub mod pid_resolver;
pub mod prompts;
#[cfg(feature = "pycoach")]
pub mod pycoach;
pub mod replay;
pub mod scanner;
pub mod server;
pub mod settings;
pub mod state;

use settings::Settings;
use state::{CoachState, SharedState};
use std::sync::Arc;
use tokio::sync::RwLock;

// ── EventEmitter trait (the Tauri decoupling boundary) ────────────────

use state::CoachSnapshot;
use state::Theme;

/// Abstraction over the frontend event channel. The Tauri app provides
/// `TauriEmitter`; headless mode and tests use `NoopEmitter`.
pub trait EventEmitter: Send + Sync {
    fn emit_state_update(&self, snapshot: &CoachSnapshot);
    fn emit_theme_changed(&self, theme: &Theme);
}

/// No-op emitter for headless / test contexts.
pub struct NoopEmitter;

impl EventEmitter for NoopEmitter {
    fn emit_state_update(&self, _: &CoachSnapshot) {}
    fn emit_theme_changed(&self, _: &Theme) {}
}

// ── Headless serve() ──────────────────────────────────────────────────

/// Headless daemon mode: start the HTTP hook server and the session
/// scanner without a GUI runtime. Reachable via the `coach serve` CLI
/// subcommand. Blocks until Ctrl-C or a worker exits.
pub async fn serve(port_override: Option<u16>) -> Result<(), String> {
    let mut settings = Settings::load();
    if let Some(p) = port_override {
        settings.port = p;
    }
    let port = settings.port;

    let addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("failed to bind {addr}: {e}"))?;

    eprintln!("[coach serve] listening on {addr}, priorities={:?}", settings.priorities);

    if let Ok(added) = settings::sync_managed_hooks(port, &mut settings.hooks_user_enabled) {
        if !added.is_empty() {
            eprintln!("[coach serve] synced Claude hooks: {added:?}");
        }
    }
    if let Ok(added) =
        settings::sync_managed_codex_hooks(port, &mut settings.codex_hooks_user_enabled)
    {
        if !added.is_empty() {
            eprintln!("[coach serve] synced Codex hooks: {added:?}");
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

    let emitter: Arc<dyn EventEmitter> = Arc::new(NoopEmitter);

    let server_task = tokio::spawn({
        let s = state.clone();
        let e = emitter.clone();
        async move { server::serve_on_listener(listener, s, e, port).await }
    });
    let scanner_task = tokio::spawn({
        let s = state.clone();
        let e = emitter.clone();
        async move { scanner::run_session_scanner(s, e).await }
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

    settings::cleanup_hooks_on_exit();

    result
}

#[cfg(feature = "pycoach")]
fn spawn_pycoach_if_configured(state: SharedState) {
    let Some(launcher) = pycoach::Pycoach::launcher_from_env() else {
        return;
    };
    tokio::spawn(async move {
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
