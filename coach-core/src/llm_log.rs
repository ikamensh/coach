//! Per-call JSONL logger for everything that crosses `llm::session_send`.
//!
//! Enabled by `COACH_LLM_LOG_DIR`. Each coach process creates a fresh run
//! directory `<root>/<timestamp>-<pid>/` and writes one file per tracked
//! coding session (`<sanitized-session-id>.jsonl`), one line per LLM call.
//! The file is flushed on every write so a crash still leaves complete
//! lines on disk.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::state::{CoachChain, CoachUsage};

/// One serialized LLM call. `build_pre` fills every field that is known
/// before the provider is called; `build_post` fills the response-side
/// fields once the call returns (or fails).
#[derive(Debug, Clone, Serialize)]
pub struct LlmCallRecord {
    pub ts: DateTime<Utc>,
    pub caller: String,
    pub session_id: Option<String>,
    pub provider: String,
    pub model: String,
    pub system_prompt: String,
    pub user_message: String,
    pub chain_in: CoachChain,
    pub require_json: bool,
    pub max_output_tokens: Option<u32>,
    pub response_text: Option<String>,
    pub error: Option<String>,
    pub latency_ms: u64,
    pub usage: Option<CoachUsage>,
    pub chain_out: Option<CoachChain>,
}

/// Borrowing handle for the caller identity. Wrappers in `llm.rs` fill in
/// the `caller` tag; callers higher up supply the `session_id`.
#[derive(Debug, Clone, Copy)]
pub struct LogContext<'a> {
    pub caller: &'a str,
    pub session_id: Option<&'a str>,
}

impl<'a> LogContext<'a> {
    pub fn new(caller: &'a str, session_id: Option<&'a str>) -> Self {
        Self { caller, session_id }
    }
}

/// A running logger. Holds one open file per coding session; files are
/// created lazily on the first call with that session_id.
pub struct LlmLogger {
    run_dir: PathBuf,
    files: Mutex<HashMap<String, File>>,
}

impl LlmLogger {
    /// Build a logger rooted at `path` (interpreted as the run dir, not
    /// the root dir). Creates the directory if missing. Exposed for tests
    /// that want a deterministic run dir.
    pub fn at(run_dir: PathBuf) -> std::io::Result<Arc<Self>> {
        std::fs::create_dir_all(&run_dir)?;
        Ok(Arc::new(Self {
            run_dir,
            files: Mutex::new(HashMap::new()),
        }))
    }

    /// Read `COACH_LLM_LOG_DIR`. When set, create a fresh run directory
    /// under it and return a logger. When unset or creation fails, return
    /// None — the caller treats that as "logging disabled."
    pub fn from_env() -> Option<Arc<Self>> {
        let root = std::env::var("COACH_LLM_LOG_DIR").ok()?;
        let root = PathBuf::from(root);
        let stamp = Utc::now().format("%Y%m%dT%H%M%S");
        let run_dir = root.join(format!("{stamp}-{}", std::process::id()));
        match Self::at(run_dir.clone()) {
            Ok(logger) => {
                eprintln!("[coach] llm_log: writing JSONL to {}", run_dir.display());
                Some(logger)
            }
            Err(e) => {
                eprintln!(
                    "[coach] llm_log: failed to create {}: {e}; logging disabled",
                    run_dir.display()
                );
                None
            }
        }
    }

    /// The directory that holds the per-session jsonl files.
    pub fn run_dir(&self) -> &std::path::Path {
        &self.run_dir
    }

    /// Serialize `record` and append it to the right session file.
    /// Best-effort: any I/O or serialization error is printed to stderr
    /// and swallowed so logging can never crash an observer call.
    pub fn append(&self, record: &LlmCallRecord) {
        let raw_id = record.session_id.as_deref().unwrap_or("unknown");
        let file_key = sanitize_session_id(raw_id);
        let json = match serde_json::to_string(record) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[coach] llm_log: serialize failed: {e}");
                return;
            }
        };

        let mut guard = match self.files.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let file = match guard.get_mut(&file_key) {
            Some(f) => f,
            None => {
                let path = self.run_dir.join(format!("{file_key}.jsonl"));
                match OpenOptions::new().create(true).append(true).open(&path) {
                    Ok(f) => guard.entry(file_key.clone()).or_insert(f),
                    Err(e) => {
                        eprintln!(
                            "[coach] llm_log: cannot open {}: {e}",
                            path.display()
                        );
                        return;
                    }
                }
            }
        };
        if let Err(e) = writeln!(file, "{json}") {
            eprintln!("[coach] llm_log: write failed: {e}");
            return;
        }
        let _ = file.flush();
    }
}

/// Keep only characters that are safe in a filename on every platform.
/// Empty or all-invalid ids collapse to "session".
fn sanitize_session_id(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() || cleaned.chars().all(|c| c == '_') {
        "session".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn fake_record(session_id: Option<&str>, caller: &str) -> LlmCallRecord {
        LlmCallRecord {
            ts: Utc::now(),
            caller: caller.to_string(),
            session_id: session_id.map(str::to_string),
            provider: "openai".into(),
            model: "gpt-test".into(),
            system_prompt: "sys".into(),
            user_message: "msg".into(),
            chain_in: CoachChain::Empty,
            require_json: false,
            max_output_tokens: Some(80),
            response_text: Some("ok".into()),
            error: None,
            latency_ms: 12,
            usage: Some(CoachUsage {
                input_tokens: 1,
                output_tokens: 2,
                cached_input_tokens: 0,
            }),
            chain_out: Some(CoachChain::ServerId { id: "resp_x".into() }),
        }
    }

    #[test]
    fn append_creates_file_and_writes_one_line_per_call() {
        let tmp = tempdir().unwrap();
        let logger = LlmLogger::at(tmp.path().to_path_buf()).unwrap();

        logger.append(&fake_record(Some("abc-123"), "observer"));
        logger.append(&fake_record(Some("abc-123"), "observer"));

        let path = tmp.path().join("abc-123.jsonl");
        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2, "one line per call");
        for line in &lines {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["caller"], "observer");
            assert_eq!(v["session_id"], "abc-123");
            assert_eq!(v["provider"], "openai");
            assert!(v["ts"].is_string());
        }
    }

    /// Property: distinct coding sessions go into distinct files under
    /// the same run dir, and neither file leaks lines into the other.
    #[test]
    fn distinct_sessions_go_to_distinct_files() {
        let tmp = tempdir().unwrap();
        let logger = LlmLogger::at(tmp.path().to_path_buf()).unwrap();

        logger.append(&fake_record(Some("sess-a"), "observer"));
        logger.append(&fake_record(Some("sess-b"), "namer"));
        logger.append(&fake_record(Some("sess-a"), "stop_chained"));

        let a = std::fs::read_to_string(tmp.path().join("sess-a.jsonl")).unwrap();
        let b = std::fs::read_to_string(tmp.path().join("sess-b.jsonl")).unwrap();
        assert_eq!(a.lines().count(), 2);
        assert_eq!(b.lines().count(), 1);
        assert!(a.contains("\"caller\":\"observer\""));
        assert!(a.contains("\"caller\":\"stop_chained\""));
        assert!(b.contains("\"caller\":\"namer\""));
        assert!(!b.contains("observer"));
    }

    /// Calls with no session_id still produce a line — collected into a
    /// stable "unknown" file — so we never silently drop observations.
    #[test]
    fn missing_session_id_writes_to_unknown_file() {
        let tmp = tempdir().unwrap();
        let logger = LlmLogger::at(tmp.path().to_path_buf()).unwrap();

        logger.append(&fake_record(None, "observer"));

        let contents = std::fs::read_to_string(tmp.path().join("unknown.jsonl")).unwrap();
        assert_eq!(contents.lines().count(), 1);
        let v: serde_json::Value = serde_json::from_str(contents.trim()).unwrap();
        assert!(v["session_id"].is_null());
    }

    /// Property: sanitize_session_id never lets a session_id escape the
    /// run dir — slashes, dots, null bytes, and tildes all collapse to
    /// underscores, so the filename stays in the intended directory.
    #[test]
    fn sanitize_rejects_path_traversal_characters() {
        for raw in [
            "../../etc/passwd",
            "foo/bar",
            "foo\\bar",
            "..",
            ".",
            "~/root",
            "a\0b",
        ] {
            let cleaned = sanitize_session_id(raw);
            assert!(!cleaned.contains('/'), "slash leaked from {raw:?}: {cleaned}");
            assert!(!cleaned.contains('\\'), "backslash leaked from {raw:?}: {cleaned}");
            assert!(!cleaned.contains('\0'), "null leaked from {raw:?}: {cleaned}");
        }
        assert_eq!(sanitize_session_id(""), "session");
        assert_eq!(sanitize_session_id("///"), "session");
        // Normal-looking ids come through untouched.
        assert_eq!(
            sanitize_session_id("e2f8a4b0-1234-4abc-bdef-0123456789ab"),
            "e2f8a4b0-1234-4abc-bdef-0123456789ab"
        );
    }

    /// Property: `from_env` is a pure function of the env var — no var,
    /// no logger. Kept as a sanity check so we don't accidentally leave
    /// logging on by default.
    #[test]
    fn from_env_returns_none_when_var_unset() {
        // Clear for this test; other tests in the process can still set it.
        // SAFETY: tests run single-threaded within a module — and we
        // restore the var immediately after the assertion.
        let prev = std::env::var("COACH_LLM_LOG_DIR").ok();
        unsafe {
            std::env::remove_var("COACH_LLM_LOG_DIR");
        }
        let logger = LlmLogger::from_env();
        assert!(logger.is_none());
        if let Some(v) = prev {
            unsafe {
                std::env::set_var("COACH_LLM_LOG_DIR", v);
            }
        }
    }
}
