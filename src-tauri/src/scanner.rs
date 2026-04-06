use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use crate::state::{SharedState, EVENT_STATE_UPDATED};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeSessionFile {
    pub pid: u32,
    pub session_id: String,
    pub cwd: Option<String>,
    pub started_at: i64, // Unix millis
}

impl ClaudeSessionFile {
    pub fn started_at_utc(&self) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(self.started_at).unwrap_or_else(Utc::now)
    }
}

fn sessions_dir() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("sessions"))
}

/// Check if a process exists via POSIX kill(pid, 0).
/// Signal 0 sends nothing but returns success if the process exists.
pub fn is_pid_alive(pid: u32) -> bool {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe { kill(pid as i32, 0) == 0 }
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

pub async fn run_session_scanner(state: SharedState, app_handle: tauri::AppHandle) {
    sync_sessions(&state, Some(&app_handle)).await;

    let mut interval = tokio::time::interval(SCAN_INTERVAL);
    interval.tick().await; // first tick is immediate, skip it
    loop {
        interval.tick().await;
        sync_sessions(&state, Some(&app_handle)).await;
    }
}

pub async fn sync_sessions(state: &SharedState, app_handle: Option<&tauri::AppHandle>) {
    let live = scan_live_sessions();
    let live_ids: HashSet<String> = live.iter().map(|s| s.session_id.clone()).collect();

    let mut coach = state.write().await;
    let mut changed = false;

    for session in &live {
        let is_new = !coach.sessions.contains_key(&session.session_id);
        coach.register_discovered(
            &session.session_id,
            session.cwd.as_deref(),
            session.started_at_utc(),
            session.pid,
        );
        if is_new {
            coach.log(
                &session.session_id,
                "Scanner",
                "session discovered",
                session.cwd.clone(),
            );
            changed = true;
        }
    }

    let dead = coach.remove_dead_sessions(&live_ids);
    if !dead.is_empty() {
        for id in &dead {
            coach.log(id, "Scanner", "session ended", None);
        }
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

    fn write_session_file(dir: &Path, pid: u32, session_id: &str, cwd: &str) {
        let content = serde_json::json!({
            "pid": pid,
            "sessionId": session_id,
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

    #[test]
    fn parses_session_file() {
        let json = r#"{"pid":27014,"sessionId":"abc-123","cwd":"/tmp","startedAt":1775383533697,"kind":"interactive","entrypoint":"cli"}"#;
        let session: ClaudeSessionFile = serde_json::from_str(json).unwrap();
        assert_eq!(session.pid, 27014);
        assert_eq!(session.session_id, "abc-123");
        assert_eq!(session.cwd, Some("/tmp".into()));
    }

    /// Millis timestamp should roundtrip through started_at_utc.
    #[test]
    fn started_at_utc_roundtrips() {
        let session = ClaudeSessionFile {
            pid: 1,
            session_id: "test".into(),
            cwd: None,
            started_at: 1775383533697,
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
        write_session_file(dir.path(), my_pid, "live-session", "/tmp/project");

        let sessions = scan_live_sessions_in(dir.path());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "live-session");
    }

    /// Session files with a dead PID should be skipped.
    #[test]
    fn scan_skips_dead_pid() {
        let dir = TempDir::new().unwrap();
        // PID 99999 is almost certainly dead
        write_session_file(dir.path(), 99999, "dead-session", "/tmp/gone");

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
}
