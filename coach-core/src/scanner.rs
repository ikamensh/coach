use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::server::fake_pid_for_sid;
use crate::state::{ActivityEntry, SessionClient, SharedState};
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

// ── Codex CLI session scanning ──────────────────────────────────────────

/// Active Codex thread discovered from the local SQLite databases.
#[derive(Debug, Clone)]
pub struct CodexSessionInfo {
    pub thread_id: String,
    pub real_pid: u32,
    pub cwd: Option<String>,
    pub thread_name: Option<String>,
    pub created_at: i64, // Unix seconds
}

impl CodexSessionInfo {
    pub fn started_at_utc(&self) -> DateTime<Utc> {
        DateTime::from_timestamp(self.created_at, 0).unwrap_or_else(Utc::now)
    }
}

/// Extract the real OS PID from a Codex `process_uuid` field.
/// Format: `pid:{real_pid}:{uuid}` → Some(real_pid).
fn parse_codex_pid(process_uuid: &str) -> Option<u32> {
    let rest = process_uuid.strip_prefix("pid:")?;
    let pid_str = rest.split(':').next()?;
    pid_str.parse().ok()
}

pub fn scan_codex_sessions() -> Vec<CodexSessionInfo> {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return vec![],
    };
    let codex_dir = home.join(".codex");
    scan_codex_sessions_in(
        &codex_dir.join("logs_2.sqlite"),
        &codex_dir.join("state_5.sqlite"),
        &codex_dir.join("session_index.jsonl"),
    )
}

/// Query Codex's SQLite databases for active sessions. Uses the `sqlite3`
/// CLI for read-only access — no Rust SQLite dependency needed. Returns
/// empty vec if `sqlite3` isn't available or the files don't exist.
pub fn scan_codex_sessions_in(
    logs_db: &Path,
    state_db: &Path,
    index_file: &Path,
) -> Vec<CodexSessionInfo> {
    if !logs_db.exists() || !state_db.exists() {
        return vec![];
    }

    // Read nice thread names from session_index.jsonl (optional).
    let thread_names = read_thread_names(index_file);

    // One sqlite3 call with ATTACH to join logs (PID) and state (metadata).
    let query = format!(
        "ATTACH '{}' AS state; \
         SELECT DISTINCT l.thread_id, l.process_uuid, s.cwd, s.created_at \
         FROM (SELECT DISTINCT thread_id, process_uuid FROM logs \
               WHERE ts > (strftime('%s','now') - 86400) AND thread_id IS NOT NULL) l \
         JOIN state.threads s ON l.thread_id = s.id \
         WHERE s.updated_at > (strftime('%s','now') - 600);",
        state_db.display()
    );

    let output = match std::process::Command::new("sqlite3")
        .arg("-separator")
        .arg("|")
        .arg(logs_db)
        .arg(&query)
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return vec![],
    };

    output
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.splitn(4, '|').collect();
            if parts.len() < 4 {
                return None;
            }
            let thread_id = parts[0].to_string();
            let real_pid = parse_codex_pid(parts[1])?;
            if !is_pid_alive(real_pid) {
                return None;
            }
            let cwd = if parts[2].is_empty() {
                None
            } else {
                Some(parts[2].to_string())
            };
            let created_at: i64 = parts[3].parse().ok()?;
            let thread_name = thread_names.get(&thread_id).cloned();
            Some(CodexSessionInfo {
                thread_id,
                real_pid,
                cwd,
                thread_name,
                created_at,
            })
        })
        .collect()
}

/// Read `session_index.jsonl` for human-friendly thread names.
fn read_thread_names(path: &Path) -> HashMap<String, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };
    content
        .lines()
        .filter_map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).ok()?;
            let id = v.get("id")?.as_str()?.to_string();
            let name = v.get("thread_name")?.as_str()?.to_string();
            Some((id, name))
        })
        .collect()
}

// ── Scanner loop ────────────────────────────────────────────────────────

const SCAN_INTERVAL: Duration = Duration::from_secs(5);

/// Periodically refresh sessions from Claude Code files and Codex SQLite.
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

    // Claude Code
    let sessions_dir = home.join(".claude").join("sessions");
    let projects_dir = home.join(".claude").join("projects");
    let claude_live = scan_live_sessions_in(&sessions_dir);

    // Codex CLI
    let codex_live = scan_codex_sessions();

    sync_all_sessions_with(state, emitter, &claude_live, &projects_dir, &codex_live).await;
}

/// Unified sync: handles Claude and Codex discovery + cleanup.
pub async fn sync_all_sessions_with(
    state: &SharedState,
    emitter: &dyn EventEmitter,
    claude_live: &[ClaudeSessionFile],
    projects_dir: &Path,
    codex_live: &[CodexSessionInfo],
) {
    // Build unified live PID set.
    let mut live_pids: HashSet<u32> = claude_live.iter().map(|s| s.pid).collect();
    for cs in codex_live {
        live_pids.insert(fake_pid_for_sid(&cs.thread_id));
    }

    let mut coach = state.write().await;
    let mut changed = false;

    // Keep hook-created sessions (Cursor, etc.) that are still active.
    // Without this, remove_dead_pids would evict them between scan cycles.
    let recent_cutoff = std::time::Instant::now() - Duration::from_secs(30);
    for sess in coach.sessions.values() {
        if sess.last_event > recent_cutoff {
            live_pids.insert(sess.pid);
        }
    }

    // ── Claude Code sessions ────────────────────────────────────────
    for session in claude_live {
        let created = coach.sessions.register_discovered_pid(
            session.pid,
            Some(&session.session_id),
            session.cwd.as_deref(),
            session.started_at_utc(),
        );
        if created {
            coach.sessions.log(
                &session.session_id,
                "Scanner",
                "process discovered",
                session.cwd.clone(),
            );
            changed = true;
        }
        bootstrap_claude_session(&mut coach, session, projects_dir, &mut changed);
    }

    // ── Codex CLI sessions ──────────────────────────────────────────
    for session in codex_live {
        let pid = fake_pid_for_sid(&session.thread_id);
        let created = coach.sessions.register_discovered_pid(
            pid,
            None,
            session.cwd.as_deref(),
            session.started_at_utc(),
        );
        // Placeholders live under a sentinel key until a hook fills in
        // the real session_id, so we look them up by pid.
        let Some(current_key) = coach.sessions.session_key_for_pid(pid) else {
            continue;
        };
        if created {
            coach.sessions.log(
                &current_key,
                "Scanner",
                "codex session discovered",
                session.cwd.clone(),
            );
            changed = true;
        }
        coach.sessions.mark_client(&current_key, SessionClient::Codex);
        // Set thread name as session title if we have one and coach hasn't set its own.
        if let (Some(name), Some(sess)) =
            (&session.thread_name, coach.sessions.get_mut(&current_key))
        {
            if sess.coach.memory.session_title.is_none() {
                sess.coach.memory.session_title = Some(name.clone());
            }
        }
    }

    // ── Cleanup ─────────────────────────────────────────────────────
    let dead = coach.sessions.remove_dead_pids(&live_pids);
    if !dead.is_empty() {
        changed = true;
    }

    if changed {
        emitter.emit_state_update(&coach.snapshot());
    }
}

/// Testable core: Claude-only sync. Delegates to `sync_all_sessions_with`
/// with an empty Codex list so existing tests keep working.
pub async fn sync_sessions_with(
    state: &SharedState,
    emitter: &dyn EventEmitter,
    live: &[ClaudeSessionFile],
    projects_dir: &Path,
) {
    sync_all_sessions_with(state, emitter, live, projects_dir, &[]).await;
}

/// Bootstrap a Claude Code session from its JSONL conversation log.
fn bootstrap_claude_session(
    coach: &mut crate::state::AppState,
    session: &ClaudeSessionFile,
    projects_dir: &Path,
    changed: &mut bool,
) {
    let Some(key) = coach.sessions.session_key_for_pid(session.pid) else {
        return;
    };
    let sess_state_sid = {
        let sess = coach.sessions.get(&key).unwrap();
        if sess.bootstrapped {
            return;
        }
        sess.session_id.clone()
    };
    // Prefer the hook's session_id (current conversation) over
    // the session file's (may be stale after /clear).
    let effective_sid = if sess_state_sid.is_empty() {
        session.session_id.clone()
    } else {
        sess_state_sid
    };
    let effective_session = ClaudeSessionFile {
        session_id: effective_sid.clone(),
        ..session.clone()
    };
    let Some(jsonl_path) = jsonl_path_for(&effective_session, projects_dir) else {
        if let Some(sess) = coach.sessions.get_mut(&key) {
            sess.bootstrapped = true;
        }
        return;
    };
    let sess = coach.sessions.get_mut(&key).unwrap();
    match replay_jsonl(&jsonl_path, sess) {
        Ok(total) => {
            sess.bootstrapped_session_id = Some(effective_sid);
            sess.bootstrapped = true;
            let agents = sess.active_agents;
            coach.sessions.log(
                &key,
                "Scanner",
                "bootstrapped from JSONL",
                Some(format!("{total} tools, {agents} active agents")),
            );
            *changed = true;
        }
        Err(e) => {
            eprintln!("[coach] JSONL bootstrap failed for pid {}: {e}", session.pid);
        }
    }
}

// ── JSONL bootstrapping ─────────────────────────────────────────────────

/// Derive the JSONL path: `{projects_dir}/{mangled-cwd}/{sessionId}.jsonl`.
/// Claude Code slugifies the cwd by replacing every non-alphanumeric byte
/// with `-`, so `/tmp/coach_llm_demo` lands under `-tmp-coach-llm-demo`
/// (not `-tmp-coach_llm_demo`). `/` alone was not enough.
fn jsonl_path_for(session: &ClaudeSessionFile, projects_dir: &Path) -> Option<PathBuf> {
    let cwd = session.cwd.as_deref()?;
    let mangled = mangle_cwd(cwd);
    Some(projects_dir.join(mangled).join(format!("{}.jsonl", session.session_id)))
}

/// Slugify a cwd the same way Claude Code does when naming its project
/// directory under `~/.claude/projects/`: every non-alphanumeric byte
/// becomes `-`.
fn mangle_cwd(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// State bootstrapped from a JSONL conversation log.
/// Replay a JSONL conversation log into a session, using the same
/// `record_tool` / `record_agent_start` / `record_agent_end` methods
/// that live hooks use. Also pushes ActivityBar entries at their real
/// historical timestamps (from each JSONL line's `timestamp` field), so
/// the activity bar reflects past work — faded by age, like live events
/// are. Returns the number of tool events replayed.
pub fn replay_jsonl(
    path: &Path,
    session: &mut crate::state::SessionState,
) -> Result<usize, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("read {}: {e}", path.display()))?;

    let mut agent_tool_ids: HashSet<String> = HashSet::new();

    for line in content.lines() {
        let entry: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let ts = entry
            .get("timestamp")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc));

        match entry.get("type").and_then(|t| t.as_str()) {
            Some("assistant") => {
                let blocks = entry
                    .pointer("/message/content")
                    .and_then(|c| c.as_array());
                if let Some(blocks) = blocks {
                    for block in blocks {
                        if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                            if let Some(name) = block.get("name").and_then(|n| n.as_str()) {
                                session.record_tool(name);
                                if let Some(timestamp) = ts {
                                    session.push_activity(ActivityEntry {
                                        timestamp,
                                        hook_event: "PostToolUse".to_string(),
                                        action: "observed".to_string(),
                                        detail: Some(name.to_string()),
                                    });
                                }
                                if name == "Agent" {
                                    session.record_agent_start();
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
                                if agent_tool_ids.remove(id) {
                                    session.record_agent_end();
                                }
                            }
                        }
                    }
                }
                if let Some(timestamp) = ts {
                    if is_user_prompt(&entry) {
                        session.push_activity(ActivityEntry {
                            timestamp,
                            hook_event: "UserPromptSubmit".to_string(),
                            action: "submitted".to_string(),
                            detail: None,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    Ok(session.event_count)
}

/// True when a `type: "user"` JSONL entry is a real user prompt
/// (text content), not a tool_result delivery. Used by the bootstrap
/// replayer to decide whether to log a UserPromptSubmit activity chip.
fn is_user_prompt(entry: &serde_json::Value) -> bool {
    let Some(content) = entry.pointer("/message/content") else {
        return false;
    };
    if let Some(text) = content.as_str() {
        return !text.trim().is_empty();
    }
    if let Some(arr) = content.as_array() {
        return arr.iter().any(|block| {
            block.get("type").and_then(|t| t.as_str()) == Some("text")
                && block
                    .get("text")
                    .and_then(|t| t.as_str())
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false)
        });
    }
    false
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

    /// Helper: create a blank SessionState for replay tests.
    fn blank_session() -> crate::state::SessionState {
        use std::collections::{HashMap, VecDeque};
        use std::time::Instant;
        crate::state::SessionState {
            session_id: String::new(),
            pid: 0,
            mode: crate::state::CoachMode::Present,
            cwd: None,
            last_event: Instant::now(),
            last_event_time: chrono::Utc::now(),
            event_count: 0,
            last_stop_blocked: None,
            started_at: chrono::Utc::now(),
            display_name: String::new(),
            tool_counts: HashMap::new(),
            stop_count: 0,
            stop_blocked_count: 0,
            coach: crate::state::SessionCoachState::new(),
            activity: VecDeque::new(),
            active_agents: 0,
            client: crate::state::SessionClient::Claude,
            is_worktree: false,
            bootstrapped: false,
            bootstrapped_session_id: None,
        }
    }

    /// replay_jsonl extracts tool counts and active agent count.
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

        let mut sess = blank_session();
        let total = replay_jsonl(&path, &mut sess).unwrap();
        assert_eq!(sess.tool_counts.get("Bash"), Some(&2));
        assert_eq!(sess.tool_counts.get("Agent"), Some(&1));
        assert_eq!(sess.active_agents, 1);
        assert_eq!(total, 3);
    }

    /// Regression: bootstrapped sessions used to render an empty
    /// ActivityBar because `replay_jsonl` populated `tool_counts` but
    /// never pushed to `session.activity`. The activity entries fed by
    /// bootstrap must carry the real JSONL timestamps (not `Utc::now()`)
    /// so the ActivityBar fade reflects when work actually happened.
    #[test]
    fn bootstrap_populates_activity_with_jsonl_timestamps() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.jsonl");

        // One real user prompt (text), one tool_use, one tool_result
        // (must NOT be logged as UserPromptSubmit).
        let lines = vec![
            r#"{"type":"user","timestamp":"2024-01-01T10:00:00Z","message":{"content":"how do I build this?"}}"#,
            r#"{"type":"assistant","timestamp":"2024-01-01T10:00:05Z","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#,
            r#"{"type":"user","timestamp":"2024-01-01T10:00:06Z","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#,
        ];
        fs::write(&path, lines.join("\n")).unwrap();

        let mut sess = blank_session();
        replay_jsonl(&path, &mut sess).unwrap();

        assert_eq!(
            sess.activity.len(),
            2,
            "expected 1 UserPromptSubmit + 1 PostToolUse, got: {:?}",
            sess.activity.iter().collect::<Vec<_>>()
        );

        let user = &sess.activity[0];
        assert_eq!(user.hook_event, "UserPromptSubmit");
        assert_eq!(
            user.timestamp.to_rfc3339(),
            "2024-01-01T10:00:00+00:00",
            "user prompt must use the JSONL timestamp, not Utc::now()"
        );

        let tool = &sess.activity[1];
        assert_eq!(tool.hook_event, "PostToolUse");
        assert_eq!(tool.detail.as_deref(), Some("Bash"));
        assert_eq!(
            tool.timestamp.to_rfc3339(),
            "2024-01-01T10:00:05+00:00",
            "tool use must use the JSONL timestamp, not Utc::now()"
        );
    }

    /// A user message containing only a `tool_result` block is a tool
    /// delivery, not a user prompt — it must not emit a
    /// UserPromptSubmit chip (those render as yellow spikes in the bar
    /// and would be deeply misleading if faked).
    #[test]
    fn bootstrap_does_not_log_tool_result_as_user_prompt() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.jsonl");

        let lines = vec![
            r#"{"type":"assistant","timestamp":"2024-01-01T10:00:00Z","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{}}]}}"#,
            r#"{"type":"user","timestamp":"2024-01-01T10:00:01Z","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#,
        ];
        fs::write(&path, lines.join("\n")).unwrap();

        let mut sess = blank_session();
        replay_jsonl(&path, &mut sess).unwrap();

        assert!(
            sess.activity
                .iter()
                .all(|e| e.hook_event != "UserPromptSubmit"),
            "tool_result user message must not produce UserPromptSubmit chips"
        );
        assert_eq!(sess.activity.len(), 1);
        assert_eq!(sess.activity[0].hook_event, "PostToolUse");
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

        let mut sess = blank_session();
        replay_jsonl(&path, &mut sess).unwrap();
        assert_eq!(sess.active_agents, 0);
        assert_eq!(sess.tool_counts.get("Agent"), Some(&1));
    }

    /// Replay against a real JSONL from the current session, if available.
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
                    let mut sess = blank_session();
                    let result = replay_jsonl(&path, &mut sess);
                    assert!(result.is_ok(), "failed to parse {}: {:?}", path.display(), result.err());
                }
            }
        }
    }

    /// Helper: create a JSONL file at the path sync_sessions_with expects.
    fn write_jsonl(projects_dir: &Path, cwd: &str, session_id: &str, lines: &[&str]) {
        let dir = projects_dir.join(mangle_cwd(cwd));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{session_id}.jsonl")), lines.join("\n")).unwrap();
    }

    /// Property: mangle_cwd matches Claude Code's slugification so any
    /// non-alphanumeric byte collapses to `-`. Regression for the
    /// `/tmp/coach_llm_demo` → `-tmp-coach-llm-demo` case: the underscore
    /// used to survive under the old `/`-only rule and the lookup would
    /// miss the file Claude had actually written.
    #[test]
    fn mangle_cwd_collapses_all_non_alnum_to_dash() {
        assert_eq!(mangle_cwd("/tmp/my-project"), "-tmp-my-project");
        assert_eq!(
            mangle_cwd("/tmp/coach_llm_demo"),
            "-tmp-coach-llm-demo",
            "underscores must become dashes (regression)"
        );
        assert_eq!(
            mangle_cwd("/Users/ikamen/.coach/x.y"),
            "-Users-ikamen--coach-x-y",
            "dots, hidden dirs, and any other punctuation collapse too"
        );
        assert_eq!(mangle_cwd("/private/tmp/a_b"), "-private-tmp-a-b");
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
    /// The entry is keyed by the session_id from the session file.
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
            crate::state::AppState::from_settings(crate::settings::Settings::default()),
        ));
        let emitter = crate::NoopEmitter;

        let live = scan_live_sessions_in(sessions_dir.path());
        assert_eq!(live.len(), 1);

        sync_sessions_with(&state, &emitter, &live, projects_dir.path()).await;

        let coach = state.read().await;
        let sess = coach.sessions.session_for_pid(my_pid).expect("session should exist");
        assert!(sess.bootstrapped);
        assert_eq!(sess.tool_counts.get("Read"), Some(&1));
        assert_eq!(sess.tool_counts.get("Bash"), Some(&1));
        assert_eq!(sess.tool_counts.get("Agent"), Some(&1));
        assert_eq!(sess.active_agents, 1, "Agent t3 has no result yet");
        assert_eq!(sess.event_count, 3);
        assert_eq!(sess.session_id, sid,
            "scanner registers under the session_id from the session file");
        assert_eq!(sess.bootstrapped_session_id, Some(sid.to_string()));
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
            crate::state::AppState::from_settings(crate::settings::Settings::default()),
        ));
        let emitter = crate::NoopEmitter;

        // Hook creates session first — empty tool_counts.
        {
            let mut coach = state.write().await;
            coach.sessions.apply_hook_event(my_pid, sid, Some(cwd));
            let sess = coach.sessions.get(sid).unwrap();
            assert!(sess.tool_counts.is_empty(), "hook-created session starts empty");
            assert!(!sess.bootstrapped);
        }

        // Scanner runs and bootstraps.
        let live = scan_live_sessions_in(sessions_dir.path());
        sync_sessions_with(&state, &emitter, &live, projects_dir.path()).await;

        let coach = state.read().await;
        let sess = coach.sessions.get(sid).unwrap();
        assert!(sess.bootstrapped);
        assert_eq!(sess.tool_counts.get("Edit"), Some(&2));
        assert_eq!(sess.event_count, 2);
        assert_eq!(sess.active_agents, 0);
    }

    /// Regression: when the hook's session_id differs from the session
    /// file's (stale after /clear), bootstrap must prefer the hook's
    /// JSONL so the session shows what the user is actually working on.
    #[tokio::test]
    async fn bootstrap_prefers_hook_session_over_stale_file() {
        let sessions_dir = TempDir::new().unwrap();
        let projects_dir = TempDir::new().unwrap();
        let my_pid = std::process::id();
        let hook_sid = "current-conversation";
        let file_sid = "stale-from-session-file";
        let cwd = "/tmp/stale-test";

        // Session file has the STALE id.
        write_session_file_full(sessions_dir.path(), my_pid, file_sid, cwd);
        // JSONL for the stale id (old conversation — should be ignored).
        write_jsonl(projects_dir.path(), cwd, file_sid, &[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"old1","name":"Bash","input":{}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"old1","content":"ok"}]}}"#,
        ]);
        // JSONL for the current conversation (should be used).
        write_jsonl(projects_dir.path(), cwd, hook_sid, &[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Edit","input":{}},{"type":"tool_use","id":"t3","name":"Edit","input":{}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"ok"},{"type":"tool_result","tool_use_id":"t3","content":"ok"}]}}"#,
        ]);

        let state: SharedState = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::state::AppState::from_settings(crate::settings::Settings::default()),
        ));
        let emitter = crate::NoopEmitter;

        // Hook arrives first with the CURRENT conversation id.
        {
            let mut coach = state.write().await;
            coach.sessions.apply_hook_event(my_pid, hook_sid, Some(cwd));
        }

        // Scanner bootstraps — should load the hook_sid's JSONL.
        let live = scan_live_sessions_in(sessions_dir.path());
        sync_sessions_with(&state, &emitter, &live, projects_dir.path()).await;

        let coach = state.read().await;
        let sess = coach.sessions.get(hook_sid).unwrap();
        assert_eq!(sess.session_id, hook_sid);
        assert!(sess.bootstrapped);
        // Should have loaded the CURRENT conversation's tools (Read+Edit+Edit),
        // not the stale one (Bash).
        assert_eq!(sess.tool_counts.get("Read"), Some(&1));
        assert_eq!(sess.tool_counts.get("Edit"), Some(&2));
        assert_eq!(sess.tool_counts.get("Bash"), None,
            "stale conversation's tools should not appear");
        assert_eq!(sess.event_count, 3);

        // Next hook with same session_id should increment, not reset.
        drop(coach);
        {
            let mut coach = state.write().await;
            let sess = coach.sessions.apply_hook_event(my_pid, hook_sid, Some(cwd));
            assert!(sess.event_count > 1,
                "next hook should increment, not reset; got event_count={}",
                sess.event_count);
        }
    }

    /// Scanner-first with stale session file (after /clear). The hook
    /// carries the REAL session_id. Rekey creates a fresh entry under
    /// the real session_id; the stale placeholder is left behind.
    #[tokio::test]
    async fn scanner_first_stale_sid_then_hook_creates_fresh_entry() {
        let sessions_dir = TempDir::new().unwrap();
        let projects_dir = TempDir::new().unwrap();
        let my_pid = std::process::id();
        let stale_sid = "stale-old-conversation";
        let real_sid = "real-current-conversation";
        let cwd = "/tmp/stale-scanner-test";

        write_session_file_full(sessions_dir.path(), my_pid, stale_sid, cwd);
        write_jsonl(projects_dir.path(), cwd, stale_sid, &[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#,
        ]);

        let state: SharedState = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::state::AppState::from_settings(crate::settings::Settings::default()),
        ));
        let emitter = crate::NoopEmitter;

        // Scanner runs first — registers under the stale session_id
        // from the session file and bootstraps from the stale JSONL.
        let live = scan_live_sessions_in(sessions_dir.path());
        sync_sessions_with(&state, &emitter, &live, projects_dir.path()).await;

        {
            let coach = state.read().await;
            let sess = coach.sessions.session_for_pid(my_pid).unwrap();
            assert!(sess.bootstrapped);
            assert_eq!(sess.session_id, stale_sid);
            assert_eq!(sess.tool_counts.get("Bash"), Some(&1));
        }

        // Hook arrives with the REAL session_id — stale entry is
        // evicted (same PID, different session_id = /clear) and a
        // fresh entry is created under real_sid.
        {
            let mut coach = state.write().await;
            let sess = coach.sessions.apply_hook_event(my_pid, real_sid, Some(cwd));
            assert_eq!(sess.session_id, real_sid);
            assert_eq!(sess.event_count, 0, "stale data discarded, no tools yet");
            assert!(sess.tool_counts.is_empty(),
                "stale bootstrap tool_counts must be cleared");
        }
    }

    /// End-to-end /clear → Coach restart scenario.
    ///
    /// Reproduces the production bug: session file has stale sessionId
    /// (from before /clear). Scanner bootstraps from the stale JSONL,
    /// then the first hook arrives with the real sessionId. The stale
    /// data gets discarded — but the scanner must RE-BOOTSTRAP from
    /// the correct JSONL on the next cycle so the session shows full
    /// history, not event_count=1.
    #[tokio::test]
    async fn clear_then_reload_rebootstraps_from_correct_jsonl() {
        let sessions_dir = TempDir::new().unwrap();
        let projects_dir = TempDir::new().unwrap();
        let my_pid = std::process::id();
        let stale_sid = "pre-clear-conversation";
        let real_sid = "post-clear-conversation";
        let cwd = "/tmp/clear-reload-test";

        write_session_file_full(sessions_dir.path(), my_pid, stale_sid, cwd);

        write_jsonl(projects_dir.path(), cwd, stale_sid, &[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"old1","name":"Bash","input":{}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"old1","content":"ok"}]}}"#,
        ]);
        write_jsonl(projects_dir.path(), cwd, real_sid, &[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Edit","input":{}},{"type":"tool_use","id":"t3","name":"Bash","input":{}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"ok"},{"type":"tool_result","tool_use_id":"t3","content":"ok"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t4","name":"Write","input":{}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t4","content":"ok"}]}}"#,
        ]);

        let state: SharedState = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::state::AppState::from_settings(crate::settings::Settings::default()),
        ));
        let emitter = crate::NoopEmitter;
        let live = scan_live_sessions_in(sessions_dir.path());

        // ── Step 1: Scanner bootstraps from stale JSONL into the placeholder ──
        sync_sessions_with(&state, &emitter, &live, projects_dir.path()).await;
        {
            let coach = state.read().await;
            let sess = coach.sessions.session_for_pid(my_pid).unwrap();
            assert_eq!(sess.tool_counts.get("Bash"), Some(&1), "stale bootstrap loaded");
            assert_eq!(sess.event_count, 1);
        }

        // ── Step 2: First hook with real session_id → discard stale ──
        {
            let mut coach = state.write().await;
            let sess = coach.sessions.apply_hook_event(my_pid, real_sid, Some(cwd));
            assert_eq!(sess.event_count, 0, "stale data discarded");
            assert!(sess.tool_counts.is_empty());
        }

        // ── Step 3: Scanner runs again → should re-bootstrap ──
        sync_sessions_with(&state, &emitter, &live, projects_dir.path()).await;
        {
            let coach = state.read().await;
            let sess = coach.sessions.get(real_sid).unwrap();
            assert_eq!(sess.event_count, 4,
                "re-bootstrap from correct JSONL: Read + Edit + Bash + Write = 4 events");
            assert_eq!(sess.tool_counts.get("Read"), Some(&1));
            assert_eq!(sess.tool_counts.get("Edit"), Some(&1));
            assert_eq!(sess.tool_counts.get("Bash"), Some(&1));
            assert_eq!(sess.tool_counts.get("Write"), Some(&1));
        }

        // ── Step 4: Subsequent hooks don't change event_count (record_tool does) ──
        {
            let mut coach = state.write().await;
            let sess = coach.sessions.apply_hook_event(my_pid, real_sid, Some(cwd));
            assert_eq!(sess.event_count, 4, "apply_hook_event doesn't touch event_count");
            sess.record_tool("Bash");
            assert_eq!(sess.event_count, 5);
        }
    }

    /// Scanner-first, session_id matches the hook. Bootstrap data is
    /// preserved and the first hook increments event_count.
    #[tokio::test]
    async fn scanner_first_matching_sid_then_hook_keeps_bootstrap() {
        let sessions_dir = TempDir::new().unwrap();
        let projects_dir = TempDir::new().unwrap();
        let my_pid = std::process::id();
        let sid = "same-conversation";
        let cwd = "/tmp/matching-sid-test";

        write_session_file_full(sessions_dir.path(), my_pid, sid, cwd);
        write_jsonl(projects_dir.path(), cwd, sid, &[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Bash","input":{}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"ok"}]}}"#,
        ]);

        let state: SharedState = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::state::AppState::from_settings(crate::settings::Settings::default()),
        ));
        let emitter = crate::NoopEmitter;

        let live = scan_live_sessions_in(sessions_dir.path());
        sync_sessions_with(&state, &emitter, &live, projects_dir.path()).await;

        {
            let coach = state.read().await;
            let sess = coach.sessions.session_for_pid(my_pid).unwrap();
            assert_eq!(sess.event_count, 2);
        }

        // Hook with same session_id — placeholder is rekeyed to sid;
        // bootstrap data survives because bootstrapped_session_id
        // matches.
        {
            let mut coach = state.write().await;
            let sess = coach.sessions.apply_hook_event(my_pid, sid, Some(cwd));
            assert_eq!(sess.event_count, 2,
                "bootstrap counts preserved, hook doesn't increment");
            assert_eq!(sess.tool_counts.get("Read"), Some(&1));
            assert_eq!(sess.tool_counts.get("Bash"), Some(&1));
            sess.record_tool("Edit");
            assert_eq!(sess.event_count, 3);
        }
    }

    // ── pid=0 regression ─────────────────────────────────────────────

    /// End-to-end: scanner discovers a session, then hook arrives with
    /// pid=0 (PID resolution failure). Must produce exactly one session
    /// and not overwrite the real PID with 0.
    ///
    /// This test goes through sync_sessions_with (the production path)
    /// so it survives internal refactoring of how sessions are registered.
    #[tokio::test]
    async fn hook_with_pid_zero_finds_scanner_session_no_duplicate() {
        let sessions_dir = TempDir::new().unwrap();
        let projects_dir = TempDir::new().unwrap();
        let my_pid = std::process::id();
        let sid = "pid-zero-regression";
        let cwd = "/tmp/pid-zero-test";

        write_session_file_full(sessions_dir.path(), my_pid, sid, cwd);

        let state: SharedState = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::state::AppState::from_settings(crate::settings::Settings::default()),
        ));
        let emitter = crate::NoopEmitter;

        // Scanner discovers the process.
        let live = scan_live_sessions_in(sessions_dir.path());
        sync_sessions_with(&state, &emitter, &live, projects_dir.path()).await;

        {
            let coach = state.read().await;
            assert_eq!(coach.sessions.len(), 1, "scanner created one entry");
        }

        // Hook arrives with pid=0 (lsof failed).
        {
            let mut coach = state.write().await;
            coach.sessions.apply_hook_event(0, sid, Some(cwd));
        }

        let coach = state.read().await;
        assert_eq!(
            coach.sessions.len(),
            1,
            "scanner + pid=0 hook must not create a duplicate"
        );
        let sess = coach.sessions.get(sid).unwrap();
        assert_eq!(sess.pid, my_pid, "pid=0 must not overwrite the real PID");
    }

    // ── Codex scanner tests ─────────────────────────────────────────

    #[test]
    fn parse_codex_pid_extracts_real_pid() {
        assert_eq!(parse_codex_pid("pid:54433:2ccb4866-b48f-4a09-a0bd-3f5669c49816"), Some(54433));
        assert_eq!(parse_codex_pid("pid:1:abc"), Some(1));
    }

    #[test]
    fn parse_codex_pid_rejects_malformed() {
        assert_eq!(parse_codex_pid(""), None);
        assert_eq!(parse_codex_pid("not-a-pid"), None);
        assert_eq!(parse_codex_pid("pid:"), None);
        assert_eq!(parse_codex_pid("pid:abc:def"), None);
    }

    #[test]
    fn read_thread_names_parses_jsonl() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session_index.jsonl");
        let lines = [
            r#"{"id":"aaa-111","thread_name":"Fix auth bug","updated_at":"2026-04-10T06:13:22Z"}"#,
            r#"{"id":"bbb-222","thread_name":"Add tests","updated_at":"2026-04-10T06:14:00Z"}"#,
        ];
        fs::write(&path, lines.join("\n")).unwrap();

        let names = read_thread_names(&path);
        assert_eq!(names.get("aaa-111").unwrap(), "Fix auth bug");
        assert_eq!(names.get("bbb-222").unwrap(), "Add tests");
    }

    #[test]
    fn read_thread_names_returns_empty_on_missing_file() {
        assert!(read_thread_names(Path::new("/nonexistent/index.jsonl")).is_empty());
    }

    /// Codex sessions discovered by the scanner get registered with fake
    /// PIDs (matching what hooks produce) and marked as Codex clients.
    #[tokio::test]
    async fn sync_registers_codex_sessions() {
        let sessions_dir = TempDir::new().unwrap();
        let projects_dir = TempDir::new().unwrap();

        let codex_sessions = vec![CodexSessionInfo {
            thread_id: "codex-thread-001".to_string(),
            real_pid: std::process::id(), // use our own PID so it's "alive"
            cwd: Some("/tmp/codex-project".to_string()),
            thread_name: Some("Fix auth".to_string()),
            created_at: 1775801533,
        }];

        let state: SharedState = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::state::AppState::from_settings(crate::settings::Settings::default()),
        ));
        let emitter = crate::NoopEmitter;

        sync_all_sessions_with(
            &state,
            &emitter,
            &scan_live_sessions_in(sessions_dir.path()),
            projects_dir.path(),
            &codex_sessions,
        )
        .await;

        let coach = state.read().await;
        let pid = fake_pid_for_sid("codex-thread-001");
        let sess = coach.sessions.session_for_pid(pid).expect("codex session should exist");
        assert_eq!(sess.client, SessionClient::Codex);
        assert_eq!(sess.cwd, Some("/tmp/codex-project".into()));
        assert_eq!(
            sess.coach.memory.session_title.as_deref(),
            Some("Fix auth"),
            "thread_name should seed the session title"
        );
    }

    /// When a hook creates a Codex session first, the scanner should
    /// mark it as Codex and set the title without resetting state.
    #[tokio::test]
    async fn sync_codex_adopts_hook_created_session() {
        let sessions_dir = TempDir::new().unwrap();
        let projects_dir = TempDir::new().unwrap();
        let thread_id = "hook-then-scan";
        let pid = fake_pid_for_sid(thread_id);

        let state: SharedState = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::state::AppState::from_settings(crate::settings::Settings::default()),
        ));
        let emitter = crate::NoopEmitter;

        // Hook creates session first.
        {
            let mut coach = state.write().await;
            coach.sessions.apply_hook_event(pid, thread_id, Some("/tmp/hook-cwd"));
            let sess = coach.sessions.get_mut(thread_id).unwrap();
            sess.record_tool("Bash");
        }

        let codex_sessions = vec![CodexSessionInfo {
            thread_id: thread_id.to_string(),
            real_pid: std::process::id(),
            cwd: Some("/tmp/hook-cwd".to_string()),
            thread_name: Some("Hook task".to_string()),
            created_at: 1775801533,
        }];

        sync_all_sessions_with(
            &state,
            &emitter,
            &scan_live_sessions_in(sessions_dir.path()),
            projects_dir.path(),
            &codex_sessions,
        )
        .await;

        let coach = state.read().await;
        let sess = coach.sessions.get(thread_id).unwrap();
        assert_eq!(sess.client, SessionClient::Codex);
        assert_eq!(sess.event_count, 1, "hook tool count preserved");
        assert_eq!(sess.tool_counts.get("Bash"), Some(&1));
        assert_eq!(
            sess.coach.memory.session_title.as_deref(),
            Some("Hook task"),
        );
    }

    /// Hook-created sessions survive scanner cleanup if they had recent activity.
    #[tokio::test]
    async fn recent_hook_sessions_survive_cleanup() {
        let sessions_dir = TempDir::new().unwrap();
        let projects_dir = TempDir::new().unwrap();

        let state: SharedState = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::state::AppState::from_settings(crate::settings::Settings::default()),
        ));
        let emitter = crate::NoopEmitter;

        // Create a hook session (Cursor-style fake PID, no scanner file).
        let cursor_pid = fake_pid_for_sid("cursor-session-xyz");
        {
            let mut coach = state.write().await;
            coach.sessions.apply_hook_event(cursor_pid, "cursor-session-xyz", Some("/tmp/cursor"));
        }

        // Run scanner with no Claude or Codex sessions — the hook session
        // should survive because it was just active.
        sync_all_sessions_with(
            &state,
            &emitter,
            &scan_live_sessions_in(sessions_dir.path()),
            projects_dir.path(),
            &[],
        )
        .await;

        let coach = state.read().await;
        assert!(
            coach.sessions.session_for_pid(cursor_pid).is_some(),
            "recently active hook session should not be evicted"
        );
    }
}
