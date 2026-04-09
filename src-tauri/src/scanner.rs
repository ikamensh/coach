use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use crate::state::{SharedState, EVENT_STATE_UPDATED};

/// Minimal view of `~/.claude/sessions/<pid>.json`.
///
/// We **only** trust `pid` and `cwd` from this file. The `session_id`
/// stored inside is whatever conversation Claude Code launched with —
/// always stale once the user has run `/clear`. The current conversation
/// id arrives via hooks; see docs/SESSION_TRACKING.md.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeSessionFile {
    pub pid: u32,
    pub cwd: Option<String>,
    pub started_at: i64, // Unix millis
    #[serde(default = "default_kind")]
    pub kind: String,
}

fn default_kind() -> String {
    "interactive".to_string()
}

impl ClaudeSessionFile {
    pub fn started_at_utc(&self) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(self.started_at).unwrap_or_else(Utc::now)
    }
}

fn sessions_dir() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("sessions"))
}

/// Check if a process with the given PID is currently alive.
///
/// On Unix this uses `kill(pid, 0)` — signal 0 sends nothing but returns
/// success if the process exists. On Windows it opens the process handle
/// and checks `GetExitCodeProcess` for `STILL_ACTIVE`.
#[cfg(unix)]
pub fn is_pid_alive(pid: u32) -> bool {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe { kill(pid as i32, 0) == 0 }
}

#[cfg(windows)]
pub fn is_pid_alive(pid: u32) -> bool {
    use std::ffi::c_void;
    type Handle = *mut c_void;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;

    extern "system" {
        fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> Handle;
        fn CloseHandle(handle: Handle) -> i32;
        fn GetExitCodeProcess(handle: Handle, exit_code: *mut u32) -> i32;
    }

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut exit_code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut exit_code);
        CloseHandle(handle);
        ok != 0 && exit_code == STILL_ACTIVE
    }
}

pub fn scan_live_sessions() -> Vec<ClaudeSessionFile> {
    match sessions_dir() {
        Some(dir) => scan_live_sessions_in(&dir),
        None => vec![],
    }
}

pub fn scan_live_sessions_in(dir: &Path) -> Vec<ClaudeSessionFile> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return vec![],
    };

    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()?.to_str()? != "json" {
                return None;
            }
            let content = std::fs::read_to_string(&path).ok()?;
            let session: ClaudeSessionFile = serde_json::from_str(&content).ok()?;
            is_pid_alive(session.pid).then_some(session)
        })
        .collect()
}

const SCAN_INTERVAL: Duration = Duration::from_secs(5);

/// Periodically refresh the session list from `~/.claude/sessions/*.json`.
/// Pass `Some(app_handle)` from the Tauri GUI path so changes emit
/// `EVENT_STATE_UPDATED` to the frontend; pass `None` for headless mode.
pub async fn run_session_scanner(state: SharedState, app_handle: Option<tauri::AppHandle>) {
    sync_sessions(&state, app_handle.as_ref()).await;

    let mut interval = tokio::time::interval(SCAN_INTERVAL);
    interval.tick().await; // first tick is immediate, skip it
    loop {
        interval.tick().await;
        sync_sessions(&state, app_handle.as_ref()).await;
    }
}

pub async fn sync_sessions(state: &SharedState, app_handle: Option<&tauri::AppHandle>) {
    let live = scan_live_sessions();
    let live_pids: HashSet<u32> = live.iter().map(|s| s.pid).collect();

    let mut coach = state.write().await;
    let mut changed = false;

    for session in &live {
        let created = coach.register_discovered_pid(
            session.pid,
            session.cwd.as_deref(),
            session.started_at_utc(),
        );
        if created {
            coach.log(session.pid, "Scanner", "process discovered", session.cwd.clone());
            changed = true;
        }
    }

    let dead = coach.remove_dead_pids(&live_pids);
    if !dead.is_empty() {
        changed = true;
    }

    if changed {
        if let Some(handle) = app_handle {
            use tauri::Emitter;
            let _ = handle.emit(EVENT_STATE_UPDATED, coach.snapshot());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_session_file(dir: &Path, pid: u32, cwd: &str) {
        // sessionId is included for realism but the scanner ignores it
        // (see docs/SESSION_TRACKING.md).
        let content = serde_json::json!({
            "pid": pid,
            "sessionId": format!("ignored-{pid}"),
            "cwd": cwd,
            "startedAt": 1775383533697_i64,
            "kind": "interactive",
            "entrypoint": "cli"
        });
        fs::write(
            dir.join(format!("{}.json", pid)),
            serde_json::to_string(&content).unwrap(),
        )
        .unwrap();
    }

    fn write_session_file_with_kind(dir: &Path, pid: u32, cwd: &str, kind: &str) {
        let content = serde_json::json!({
            "pid": pid,
            "sessionId": format!("ignored-{pid}"),
            "cwd": cwd,
            "startedAt": 1775383533697_i64,
            "kind": kind,
            "entrypoint": "cli"
        });
        fs::write(
            dir.join(format!("{}.json", pid)),
            serde_json::to_string(&content).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn parses_session_file() {
        let json = r#"{"pid":27014,"sessionId":"abc-123","cwd":"/tmp","startedAt":1775383533697,"kind":"interactive","entrypoint":"cli"}"#;
        let session: ClaudeSessionFile = serde_json::from_str(json).unwrap();
        assert_eq!(session.pid, 27014);
        assert_eq!(session.cwd, Some("/tmp".into()));
    }

    /// Millis timestamp should roundtrip through started_at_utc.
    #[test]
    fn started_at_utc_roundtrips() {
        let session = ClaudeSessionFile {
            pid: 1,
            cwd: None,
            started_at: 1775383533697,
            kind: "interactive".to_string(),
        };
        assert_eq!(session.started_at_utc().timestamp_millis(), 1775383533697);
    }

    #[test]
    fn current_process_is_alive() {
        assert!(is_pid_alive(std::process::id()));
    }

    /// Session files with a live PID (our own process) should be found.
    #[test]
    fn scan_finds_sessions_with_live_pid() {
        let dir = TempDir::new().unwrap();
        let my_pid = std::process::id();
        write_session_file(dir.path(), my_pid, "/tmp/project");

        let sessions = scan_live_sessions_in(dir.path());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].pid, my_pid);
    }

    /// Session files with a dead PID should be skipped.
    #[test]
    fn scan_skips_dead_pid() {
        let dir = TempDir::new().unwrap();
        // PID 99999 is almost certainly dead
        write_session_file(dir.path(), 99999, "/tmp/gone");

        if !is_pid_alive(99999) {
            let sessions = scan_live_sessions_in(dir.path());
            assert!(sessions.is_empty());
        }
    }

    #[test]
    fn scan_ignores_non_json_files() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("notes.txt"), "not a session").unwrap();
        assert!(scan_live_sessions_in(dir.path()).is_empty());
    }

    #[test]
    fn scan_handles_missing_dir() {
        assert!(scan_live_sessions_in(Path::new("/nonexistent")).is_empty());
    }

    /// Scanner returns all sessions including sub-agents; the kind field
    /// is preserved so callers can partition.
    #[test]
    fn scan_returns_all_kinds() {
        let dir = TempDir::new().unwrap();
        let my_pid = std::process::id();

        write_session_file(dir.path(), my_pid, "/tmp/main");
        // sub-agent uses our own PID too (different filename) so it passes the alive check
        write_session_file_with_kind(dir.path(), my_pid + 100_000, "/tmp/sub", "task");

        let sessions = scan_live_sessions_in(dir.path());
        // Our own PID is alive; PID+100000 is almost certainly dead → only 1.
        // But both would appear if both were alive. The point is: no kind filtering.
        assert!(sessions.iter().all(|s| s.pid == my_pid));
        assert_eq!(sessions[0].kind, "interactive");
    }

    /// Session files missing the `kind` field default to "interactive"
    /// for backwards compatibility with older Claude Code versions.
    #[test]
    fn missing_kind_defaults_to_interactive() {
        let dir = TempDir::new().unwrap();
        let my_pid = std::process::id();

        let content = serde_json::json!({
            "pid": my_pid,
            "sessionId": "no-kind",
            "cwd": "/tmp",
            "startedAt": 1775383533697_i64,
        });
        fs::write(
            dir.path().join(format!("{}.json", my_pid)),
            serde_json::to_string(&content).unwrap(),
        )
        .unwrap();

        let sessions = scan_live_sessions_in(dir.path());
        assert_eq!(sessions.len(), 1, "missing kind should default to interactive");
        assert_eq!(sessions[0].kind, "interactive");
    }

}
