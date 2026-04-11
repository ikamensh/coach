use axum::{
    extract::State as AxumState,
    routing::{get, post},
    Json, Router,
};
use std::net::SocketAddr;
use std::sync::Arc;

use crate::state::SharedState;
use crate::EventEmitter;

mod api;
mod claude;
mod codex;
mod cursor;
mod events;
mod observer;
mod rules;

/// Maps a request's TCP peer port (and session_id, used by the test
/// fake) to the owning Claude Code PID. Production wraps
/// `crate::pid_resolver::resolve_peer_pid`; tests inject a deterministic
/// hash so distinct session_ids resolve to distinct fake PIDs.
pub type PidResolver = Arc<dyn Fn(u16, &str) -> Option<u32> + Send + Sync>;

/// Walk one level up the process tree. Injected into `AppState` so tests
/// can supply a fake; production uses `pid_resolver::parent_pid`.
pub type ParentPidFn = Arc<dyn Fn(u32) -> Option<u32> + Send + Sync>;

/// True if a pid looks like a Claude Code main process. Injected so
/// tests can stage nested-Claude scenarios without spawning real
/// processes. Production wraps `pid_resolver::is_claude_process`.
pub type IsClaudeFn = Arc<dyn Fn(u32) -> bool + Send + Sync>;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) coach: SharedState,
    pub(crate) emitter: Arc<dyn EventEmitter>,
    resolver: PidResolver,
    parent_pid_fn: ParentPidFn,
    is_claude_fn: IsClaudeFn,
}

/// Resolve a hook to its owning PID. Cache lookup first, then the
/// configured resolver (lsof in production, hash-of-sid in tests).
/// Returns None if the resolver fails — the caller should drop the
/// event from session-list bookkeeping rather than create a phantom row.
///
/// When the raw PID isn't a known session, walks up the parent chain.
/// This handles command-type hooks where the TCP peer is the shim's
/// curl process, not Claude Code.
///
/// Two compatibility rules make the walk safe when a Claude Code runs
/// under another Claude Code (`claude -p` from a Bash tool call):
///
/// 1. A known ancestor is only returned if its current session_id is
///    empty or matches `sid`. Otherwise the walk would stomp an
///    unrelated conversation via `apply_hook_event`'s /clear branch.
/// 2. An unknown ancestor that looks like a Claude Code main process
///    (detected via `is_claude_fn`) short-circuits the walk — it's
///    almost certainly the spawned session that the scanner hasn't
///    registered yet.
async fn resolve_pid(state: &AppState, sid: &str, peer_port: u16) -> Option<u32> {
    {
        let coach = state.coach.read().await;
        if let Some(&pid) = coach.session_id_to_pid.get(sid) {
            return Some(pid);
        }
    }
    let raw_pid = (state.resolver)(peer_port, sid)?;

    // Snapshot {pid → current_session_id} so we can classify ancestors
    // without holding the lock during the walk's I/O.
    let known: std::collections::HashMap<u32, String> = {
        let coach = state.coach.read().await;
        coach
            .sessions
            .iter()
            .map(|(pid, sess)| (*pid, sess.current_session_id.clone()))
            .collect()
    };

    if compatible(&known, raw_pid, sid) {
        eprintln!("[coach] resolved sid {sid} → pid {raw_pid} (peer port {peer_port})");
        return Some(raw_pid);
    }

    let mut candidate = raw_pid;
    for _ in 0..5 {
        let Some(ppid) = (state.parent_pid_fn)(candidate) else {
            break;
        };
        if compatible(&known, ppid, sid) {
            eprintln!(
                "[coach] resolved sid {sid} → pid {ppid} (parent of {raw_pid}, peer port {peer_port})"
            );
            return Some(ppid);
        }
        if known.contains_key(&ppid) {
            // Known ancestor with a different session — don't cross.
            // Register against the deepest non-crossing candidate;
            // better than stomping an unrelated conversation.
            eprintln!(
                "[coach] resolved sid {sid} → pid {candidate} (stopped at mismatched ancestor {ppid}, peer port {peer_port})"
            );
            return Some(candidate);
        }
        if (state.is_claude_fn)(ppid) {
            // Unknown Claude-looking ancestor: the spawned session.
            eprintln!(
                "[coach] resolved sid {sid} → pid {ppid} (claude ancestor of {raw_pid}, peer port {peer_port})"
            );
            return Some(ppid);
        }
        candidate = ppid;
    }

    // Walk exhausted without a match. Register against the deepest
    // candidate we reached — preferred over stomping a known session.
    eprintln!(
        "[coach] resolved sid {sid} → pid {candidate} (walk exhausted, peer port {peer_port})"
    );
    Some(candidate)
}

/// A known pid is "compatible" with `sid` when it has no current
/// session yet (placeholder from the scanner) or its current session
/// matches. Anything else means attributing this hook there would wipe
/// an unrelated conversation.
fn compatible(known: &std::collections::HashMap<u32, String>, pid: u32, sid: &str) -> bool {
    match known.get(&pid) {
        Some(existing) => existing.is_empty() || existing == sid,
        None => false,
    }
}

async fn handle_get_state(
    AxumState(state): AxumState<AppState>,
) -> Json<crate::state::CoachSnapshot> {
    let coach = state.coach.read().await;
    Json(coach.snapshot())
}

async fn handle_version() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "version": env!("CARGO_PKG_VERSION") }))
}

fn build_router(
    coach: SharedState,
    emitter: Arc<dyn EventEmitter>,
    resolver: PidResolver,
    parent_pid_fn: ParentPidFn,
    is_claude_fn: IsClaudeFn,
) -> Router {
    let state = AppState {
        coach,
        emitter,
        resolver,
        parent_pid_fn,
        is_claude_fn,
    };
    Router::new()
        .merge(claude::routes())
        .merge(codex::routes())
        .merge(cursor::routes())
        .route("/state", get(handle_get_state))
        .route("/version", get(handle_version))
        // CLI-facing API. Mirrors Tauri commands; same in-memory state.
        .route("/api/state", get(handle_get_state))
        .route("/api/sessions/mode", post(api::set_all_modes))
        .route("/api/sessions/{pid}/mode", post(api::set_session_mode))
        .route("/api/config/priorities", post(api::set_priorities))
        .route("/api/config/model", post(api::set_model))
        .route("/api/config/api-token", post(api::set_api_token))
        .route("/api/config/coach-mode", post(api::set_coach_mode))
        .route("/api/config/rules", post(api::set_rules))
        .with_state(state)
}

/// Build a resolver that delegates to lsof on `listen_port`. This is the
/// production resolver — it's accurate even with multiple Claude Code
/// windows in the same cwd.
pub fn lsof_resolver(listen_port: u16) -> PidResolver {
    Arc::new(move |peer_port, _sid| {
        crate::pid_resolver::resolve_peer_pid(peer_port, listen_port)
    })
}

/// Hash a hook session_id to a stable, non-zero u32. Used by the test
/// resolver and exposed so integration tests can compute the same fake
/// PID from the session_id they posted.
pub fn fake_pid_for_sid(sid: &str) -> u32 {
    let mut h: u32 = 1;
    for b in sid.bytes() {
        h = h.wrapping_mul(31).wrapping_add(b as u32);
    }
    h | 1
}

/// Test resolver: distinct session_ids resolve to distinct fake PIDs
/// without touching the OS. Used by integration tests where the client
/// and server live in the same process.
pub fn fake_resolver_from_sid() -> PidResolver {
    Arc::new(|_peer_port, sid| Some(fake_pid_for_sid(sid)))
}

/// No-op parent PID function for tests where fake PIDs have no real
/// process tree. The parent walk in `resolve_pid` simply skips.
pub fn no_parent() -> ParentPidFn {
    Arc::new(|_| None)
}

/// Default "nothing looks like Claude" stub for tests that don't care
/// about nested-Claude detection. Production uses
/// `pid_resolver::is_claude_process`.
pub fn no_claude_detect() -> IsClaudeFn {
    Arc::new(|_| false)
}

/// Router without Tauri emitter — for integration tests.
/// Tests inject a fake resolver via `fake_resolver_from_sid()` so the
/// in-process client gets distinct fake PIDs per session_id.
pub fn create_router_headless(coach: SharedState, resolver: PidResolver) -> Router {
    build_router(
        coach,
        Arc::new(crate::NoopEmitter),
        resolver,
        no_parent(),
        no_claude_detect(),
    )
}

/// Router with a custom parent-PID function — for tests that exercise
/// the parent walk (e.g. command-hook ghost session fix).
pub fn create_router_headless_with_parent(
    coach: SharedState,
    resolver: PidResolver,
    parent_pid_fn: ParentPidFn,
) -> Router {
    build_router(
        coach,
        Arc::new(crate::NoopEmitter),
        resolver,
        parent_pid_fn,
        no_claude_detect(),
    )
}

/// Router with custom parent-PID *and* Claude-detection functions — for
/// tests that exercise the nested-Claude parent walk.
pub fn create_router_headless_with_ancestry(
    coach: SharedState,
    resolver: PidResolver,
    parent_pid_fn: ParentPidFn,
    is_claude_fn: IsClaudeFn,
) -> Router {
    build_router(
        coach,
        Arc::new(crate::NoopEmitter),
        resolver,
        parent_pid_fn,
        is_claude_fn,
    )
}

/// Bind the production hook server. Pass `Some(app_handle)` from the
/// Tauri GUI path to get state-update events emitted to the frontend;
/// pass `None` for headless `coach serve` mode (CLI / VM tests / CI).
///
/// The Tauri GUI calls this from `lib.rs::run()` and panics on bind
/// failure (the GUI has no clean way to surface the error). The
/// headless `serve()` path pre-binds the listener itself via
/// `serve_on_listener` so port collisions become a non-zero CLI exit
/// with a clear error, not a panic-then-exit-0.
pub async fn start_server(
    coach: SharedState,
    emitter: Arc<dyn EventEmitter>,
    port: u16,
) {
    let addr = format!("127.0.0.1:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("Failed to bind to {}: {}", addr, e));
    eprintln!("Coach hook server listening on {}", addr);
    serve_on_listener(listener, coach, emitter, port).await;
}

/// Serve hook traffic on an already-bound listener. Used by the
/// headless `serve()` path so it can pre-bind, fail fast on port
/// collisions, and *then* announce success.
pub async fn serve_on_listener(
    listener: tokio::net::TcpListener,
    coach: SharedState,
    emitter: Arc<dyn EventEmitter>,
    port: u16,
) {
    let real_parent: ParentPidFn = Arc::new(crate::pid_resolver::parent_pid);
    let real_is_claude: IsClaudeFn = Arc::new(crate::pid_resolver::is_claude_process);
    let app = build_router(
        coach,
        emitter,
        lsof_resolver(port),
        real_parent,
        real_is_claude,
    );
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .expect("Hook server crashed");
}

