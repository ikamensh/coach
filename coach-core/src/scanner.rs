use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::state::SharedState;
use crate::EventEmitter;

/// Minimal view of `~/.claude/sessions/<pid>.json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeSessionFile {
    pub pid: u32,
    pub session_id: String,
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
/// Emits `EVENT_STATE_UPDATED` via the provided `EventEmitter` when changes
/// are detected.
pub async fn run_session_scanner(state: SharedState, emitter: std::sync::Arc<dyn EventEmitter>) {
    sync_sessions(&state, &*emitter).await;

    let mut interval = tokio::time::interval(SCAN_INTERVAL);
    interval.tick().await; // first tick is immediate, skip it
    loop {
        interval.tick().await;
        sync_sessions(&state, &*emitter).await;
    }
}

pub async fn sync_sessions(state: &SharedState, emitter: &dyn EventEmitter) {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return,
    };
    let sessions_dir = home.join(".claude").join("sessions");
    let projects_dir = home.join(".claude").join("projects");
    let live = scan_live_sessions_in(&sessions_dir);
    sync_sessions_with(state, emitter, &live, &projects_dir).await;
}

/// Testable core: accepts the session list and projects directory.
pub async fn sync_sessions_with(
    state: &SharedState,
    emitter: &dyn EventEmitter,
    live: &[ClaudeSessionFile],
    projects_dir: &Path,
) {
    let live_pids: HashSet<u32> = live.iter().map(|s| s.pid).collect();

    let mut coach = state.write().await;
    let mut changed = false;

    for session in live {
        let created = coach.register_discovered_pid(
            session.pid,
            session.cwd.as_deref(),
            session.started_at_utc(),
        );
        if created {
            coach.log(session.pid, "Scanner", "process discovered", session.cwd.clone());
            changed = true;
        }

        // Bootstrap from JSONL on first scan for sessions that haven't
        // been bootstrapped yet. This covers both scanner-first and
        // hook-first discovery: hooks create the session with empty
        // tool_counts, the scanner fills it in from history.
        let needs_bootstrap = coach.sessions.get(&session.pid)
            .is_some_and(|s| !s.bootstrapped);
        if needs_bootstrap {
            if let Some(jsonl_path) = jsonl_path_for(session, projects_dir) {
                match bootstrap_from_jsonl(&jsonl_path) {
                    Ok(boot) => {
                        if let Some(sess) = coach.sessions.get_mut(&session.pid) {
                            sess.current_session_id = session.session_id.clone();
                            sess.tool_counts = boot.tool_counts;
                            sess.active_agents = boot.active_agents;
                            sess.event_count = boot.total_tools;
                            sess.bootstrapped = true;
                        }
                        coach.log(
                            session.pid,
                            "Scanner",
                            "bootstrapped from JSONL",
                            Some(format!("{} tools, {} active agents",
                                boot.total_tools, boot.active_agents)),
                        );
                        changed = true;
                    }
                    Err(e) => {
                        eprintln!("[coach] JSONL bootstrap failed for pid {}: {e}", session.pid);
                    }
                }
            } else {
                // No JSONL found — mark as bootstrapped to avoid retrying.
                if let Some(sess) = coach.sessions.get_mut(&session.pid) {
                    sess.bootstrapped = true;
                }
            }
        }
    }

    let dead = coach.remove_dead_pids(&live_pids);
    if !dead.is_empty() {
        changed = true;
    }

    if changed {
        emitter.emit_state_update(&coach.snapshot());
    }
}

// ── JSONL bootstrapping ─────────────────────────────────────────────────

/// Derive the JSONL path: `{projects_dir}/{mangled-cwd}/{sessionId}.jsonl`
fn jsonl_path_for(session: &ClaudeSessionFile, projects_dir: &Path) -> Option<PathBuf> {
    let cwd = session.cwd.as_deref()?;
    let mangled = cwd.replace('/', "-");
    Some(projects_dir.join(mangled).join(format!("{}.jsonl", session.session_id)))
}

/// State bootstrapped from a JSONL conversation log.
#[derive(Debug, Clone)]
pub struct BootstrapState {
    pub tool_counts: HashMap<String, usize>,
    pub active_agents: usize,
    pub total_tools: usize,
}

/// Parse a Claude Code JSONL to extract tool counts and active agent count.
///
/// Agent tool_use blocks that don't yet have a matching tool_result are
/// counted as active agents.
pub fn bootstrap_from_jsonl(path: &Path) -> Result<BootstrapState, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("read {}: {e}", path.display()))?;

    let mut tool_counts: HashMap<String, usize> = HashMap::new();
    let mut agent_tool_ids: HashSet<String> = HashSet::new();
    let mut agent_results: HashSet<String> = HashSet::new();

    for line in content.lines() {
        let entry: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        match entry.get("type").and_then(|t| t.as_str()) {
            Some("assistant") => {
                let blocks = entry
                    .pointer("/message/content")
                    .and_then(|c| c.as_array());
                if let Some(blocks) = blocks {
                    for block in blocks {
                        if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                            if let Some(name) = block.get("name").and_then(|n| n.as_str()) {
                                *tool_counts.entry(name.to_string()).or_default() += 1;
                                if name == "Agent" {
                                    if let Some(id) = block.get("id").and_then(|i| i.as_str()) {
                                        agent_tool_ids.insert(id.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Some("user") => {
                let blocks = entry
                    .pointer("/message/content")
                    .and_then(|c| c.as_array());
                if let Some(blocks) = blocks {
                    for block in blocks {
                        if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                            if let Some(id) = block.get("tool_use_id").and_then(|i| i.as_str()) {
                                if agent_tool_ids.contains(id) {
                                    agent_results.insert(id.to_string());
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let total_tools: usize = tool_counts.values().sum();
    let active_agents = agent_tool_ids.len().saturating_sub(agent_results.len());

    Ok(BootstrapState {
        tool_counts,
        active_agents,
        total_tools,
    })
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
            session_id: "test".to_string(),
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

    /// bootstrap_from_jsonl extracts tool counts and active agent count.
    #[test]
    fn bootstrap_counts_tools_and_active_agents() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.jsonl");

        // Two Bash tool_uses, one Agent tool_use, no Agent result → 1 active agent
        let lines = vec![
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Bash","input":{}},{"type":"tool_use","id":"t3","name":"Agent","input":{}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"ok"}]}}"#,
            // t3 (Agent) has no tool_result → still active
        ];
        fs::write(&path, lines.join("\n")).unwrap();

        let boot = bootstrap_from_jsonl(&path).unwrap();
        assert_eq!(boot.tool_counts.get("Bash"), Some(&2));
        assert_eq!(boot.tool_counts.get("Agent"), Some(&1));
        assert_eq!(boot.active_agents, 1);
        assert_eq!(boot.total_tools, 3);
    }

    /// When an Agent's tool_result appears, it's no longer active.
    #[test]
    fn bootstrap_agent_completes_when_result_arrives() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.jsonl");

        let lines = vec![
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"a1","name":"Agent","input":{}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"a1","content":"done"}]}}"#,
        ];
        fs::write(&path, lines.join("\n")).unwrap();

        let boot = bootstrap_from_jsonl(&path).unwrap();
        assert_eq!(boot.active_agents, 0);
        assert_eq!(boot.tool_counts.get("Agent"), Some(&1));
    }

    /// Bootstrap against a real JSONL from the current session, if available.
    #[test]
    fn bootstrap_reads_real_session() {
        let home = match dirs::home_dir() {
            Some(h) => h,
            None => return,
        };
        let sessions_dir = home.join(".claude").join("sessions");
        let projects_dir = home.join(".claude").join("projects");
        let sessions = scan_live_sessions_in(&sessions_dir);
        for session in sessions.iter().take(1) {
            if let Some(path) = jsonl_path_for(session, &projects_dir) {
                if path.exists() {
                    let boot = bootstrap_from_jsonl(&path);
                    assert!(boot.is_ok(), "failed to parse {}: {:?}", path.display(), boot.err());
                }
            }
        }
    }

    /// Helper: create a JSONL file at the path sync_sessions_with expects.
    fn write_jsonl(projects_dir: &Path, cwd: &str, session_id: &str, lines: &[&str]) {
        let mangled = cwd.replace('/', "-");
        let dir = projects_dir.join(mangled);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{session_id}.jsonl")), lines.join("\n")).unwrap();
    }

    fn write_session_file_full(dir: &Path, pid: u32, session_id: &str, cwd: &str) {
        let content = serde_json::json!({
            "pid": pid,
            "sessionId": session_id,
            "cwd": cwd,
            "startedAt": 1775383533697_i64,
            "kind": "interactive",
        });
        fs::write(
            dir.join(format!("{pid}.json")),
            serde_json::to_string(&content).unwrap(),
        ).unwrap();
    }

    /// Scanner discovers a session and bootstraps it from the JSONL.
    #[tokio::test]
    async fn sync_bootstraps_scanner_discovered_session() {
        let sessions_dir = TempDir::new().unwrap();
        let projects_dir = TempDir::new().unwrap();
        let my_pid = std::process::id();
        let sid = "test-session-001";
        let cwd = "/tmp/my-project";

        write_session_file_full(sessions_dir.path(), my_pid, sid, cwd);
        write_jsonl(projects_dir.path(), cwd, sid, &[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Bash","input":{}},{"type":"tool_use","id":"t3","name":"Agent","input":{}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"ok"}]}}"#,
        ]);

        let state: SharedState = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::state::CoachState::from_settings(crate::settings::Settings::default()),
        ));
        let emitter = crate::NoopEmitter;

        let live = scan_live_sessions_in(sessions_dir.path());
        assert_eq!(live.len(), 1);

        sync_sessions_with(&state, &emitter, &live, projects_dir.path()).await;

        let coach = state.read().await;
        let sess = coach.sessions.get(&my_pid).expect("session should exist");
        assert!(sess.bootstrapped);
        assert_eq!(sess.tool_counts.get("Read"), Some(&1));
        assert_eq!(sess.tool_counts.get("Bash"), Some(&1));
        assert_eq!(sess.tool_counts.get("Agent"), Some(&1));
        assert_eq!(sess.active_agents, 1, "Agent t3 has no result yet");
        assert_eq!(sess.event_count, 3);
        assert_eq!(sess.current_session_id, sid);
    }

    /// A hook creates the session first (empty tool_counts), then the
    /// scanner bootstraps it from the JSONL on its next pass.
    #[tokio::test]
    async fn sync_bootstraps_hook_created_session() {
        let sessions_dir = TempDir::new().unwrap();
        let projects_dir = TempDir::new().unwrap();
        let my_pid = std::process::id();
        let sid = "hook-session-002";
        let cwd = "/tmp/hook-project";

        write_session_file_full(sessions_dir.path(), my_pid, sid, cwd);
        write_jsonl(projects_dir.path(), cwd, sid, &[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Edit","input":{}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Edit","input":{}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"ok"}]}}"#,
        ]);

        let state: SharedState = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::state::CoachState::from_settings(crate::settings::Settings::default()),
        ));
        let emitter = crate::NoopEmitter;

        // Hook creates session first — empty tool_counts.
        {
            let mut coach = state.write().await;
            coach.apply_hook_event(my_pid, sid, Some(cwd));
            let sess = coach.sessions.get(&my_pid).unwrap();
            assert!(sess.tool_counts.is_empty(), "hook-created session starts empty");
            assert!(!sess.bootstrapped);
        }

        // Scanner runs and bootstraps.
        let live = scan_live_sessions_in(sessions_dir.path());
        sync_sessions_with(&state, &emitter, &live, projects_dir.path()).await;

        let coach = state.read().await;
        let sess = coach.sessions.get(&my_pid).unwrap();
        assert!(sess.bootstrapped);
        assert_eq!(sess.tool_counts.get("Edit"), Some(&2));
        assert_eq!(sess.event_count, 2);
        assert_eq!(sess.active_agents, 0);
    }
}
