//! Session discovery and replay for the dev tools UI.
//!
//! Scans `~/.claude/projects/` for JSONL session files, extracts metadata,
//! and replays hook events through Coach's intervention logic. The replay
//! mode parameter mirrors live coach behavior:
//!   • `"present"` — passthrough; no intervention
//!   • `"away"`    — rules-mode: block the first Stop with the static
//!                   `away_message`, pass subsequent ones (a stand-in for
//!                   the live 15s cooldown)
//!   • `"llm"`     — call `llm::evaluate_stop` per Stop event, exactly
//!                   the stateless one-shot path the live coach falls
//!                   back to when no provider chain is available

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::state::SharedState;

fn claude_projects_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("projects"))
}

// ── Types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedSession {
    pub id: String,
    pub project: String,
    pub mtime: f64,
    pub size: u64,
    pub topic: String,
    pub message_count: usize,
    pub user_message_count: usize,
    pub assistant_message_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayEvent {
    pub index: usize,
    pub kind: String,
    pub tool_name: String,
    pub timestamp: String,
    pub summary: String,
    /// null = passthrough, "blocked", "auto-approved"
    pub action: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayResult {
    pub session_id: String,
    pub topic: String,
    pub cwd: String,
    pub message_count: usize,
    pub user_message_count: usize,
    pub assistant_message_count: usize,
    pub event_count: usize,
    pub events: Vec<ReplayEvent>,
    pub first_intervention_index: Option<usize>,
}

// ── Session discovery ──────────────────────────────────────────────────

pub fn list_sessions(limit: usize) -> Vec<SavedSession> {
    let projects_dir = match claude_projects_dir() {
        Some(d) if d.exists() => d,
        _ => return vec![],
    };

    let mut sessions: Vec<SavedSession> = Vec::new();

    let entries = match std::fs::read_dir(&projects_dir) {
        Ok(e) => e,
        Err(_) => return vec![],
    };

    for project_entry in entries.flatten() {
        let project_path = project_entry.path();
        if !project_path.is_dir() {
            continue;
        }
        let project_name = project_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let files = match std::fs::read_dir(&project_path) {
            Ok(f) => f,
            Err(_) => continue,
        };

        for file_entry in files.flatten() {
            let path = file_entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }

            let meta = match path.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };

            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);

            let id = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            let (topic, msg_count, user_count, asst_count) = scan_session_file(&path);

            sessions.push(SavedSession {
                id,
                project: project_name.clone(),
                mtime,
                size: meta.len(),
                topic,
                message_count: msg_count,
                user_message_count: user_count,
                assistant_message_count: asst_count,
            });
        }
    }

    sessions.sort_by(|a, b| b.mtime.partial_cmp(&a.mtime).unwrap_or(std::cmp::Ordering::Equal));
    sessions.truncate(limit);
    sessions
}

/// Quick scan: extract topic (first user text) and message counts.
fn scan_session_file(path: &std::path::Path) -> (String, usize, usize, usize) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return (String::new(), 0, 0, 0),
    };

    let mut topic = String::new();
    let mut total = 0usize;
    let mut user = 0usize;
    let mut assistant = 0usize;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        total += 1;

        let msg_type = entry.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match msg_type {
            "user" => {
                user += 1;
                if topic.is_empty() {
                    topic = extract_topic_from_entry(&entry);
                }
            }
            "assistant" => {
                assistant += 1;
            }
            _ => {}
        }
    }

    (topic, total, user, assistant)
}

fn extract_topic_from_entry(entry: &serde_json::Value) -> String {
    let content = entry
        .pointer("/message/content")
        .unwrap_or(&serde_json::Value::Null);

    if let Some(text) = content.as_str() {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return truncate(trimmed, 100);
        }
    }

    if let Some(arr) = content.as_array() {
        for block in arr {
            if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        return truncate(trimmed, 100);
                    }
                }
            }
        }
    }

    String::new()
}

/// Truncate `s` to at most `max` *bytes*, appending `...` if shortened.
/// Walks back to the nearest UTF-8 char boundary at or below `max` so we
/// never panic by slicing inside a multi-byte codepoint — a real bug
/// found by `cargo test` on a VPS whose `~/.claude/projects/` contained
/// a transcript with `’` (U+2019, 3 bytes) straddling byte 100.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

// ── Replay ─────────────────────────────────────────────────────────────

pub fn find_session(session_id: &str) -> Option<PathBuf> {
    let projects_dir = claude_projects_dir()?;
    if !projects_dir.exists() {
        return None;
    }

    for project_entry in std::fs::read_dir(&projects_dir).ok()?.flatten() {
        let project_path = project_entry.path();
        if !project_path.is_dir() {
            continue;
        }
        // Exact match
        let exact = project_path.join(format!("{}.jsonl", session_id));
        if exact.exists() {
            return Some(exact);
        }
        // Prefix match
        for f in std::fs::read_dir(&project_path).ok()?.flatten() {
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                let stem = p.file_stem().unwrap_or_default().to_string_lossy();
                if stem.starts_with(session_id) {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// Internal raw event extracted from the JSONL transcript before
/// per-mode evaluation. `stop_reason` is `Some` only on Stop events;
/// it's the field passed through to the LLM coach, matching what the
/// live `run_stop` hook handler builds in `StopContext`.
struct RawEvent {
    kind: String,
    tool_name: String,
    timestamp: String,
    summary: String,
    stop_reason: Option<String>,
}

pub async fn replay_session(
    session_id: &str,
    mode: &str,
    state: &SharedState,
) -> Result<ReplayResult, String> {
    let path = find_session(session_id)
        .ok_or_else(|| format!("Session not found: {}", session_id))?;

    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read session: {}", e))?;

    let mut messages: Vec<serde_json::Value> = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            messages.push(v);
        }
    }

    let topic = messages.iter()
        .find(|m| m.get("type").and_then(|v| v.as_str()) == Some("user"))
        .map(extract_topic_from_entry)
        .unwrap_or_default();

    let cwd = messages.iter()
        .find_map(|m| m.get("cwd").and_then(|v| v.as_str()).map(String::from))
        .unwrap_or_default();

    let user_count = messages.iter()
        .filter(|m| m.get("type").and_then(|v| v.as_str()) == Some("user"))
        .count();
    let asst_count = messages.iter()
        .filter(|m| m.get("type").and_then(|v| v.as_str()) == Some("assistant"))
        .count();

    // Snapshot priorities now and drop the read guard before any awaits.
    // `evaluate_stop` reacquires its own short-lived read lock, so we
    // must not be holding one when we call it.
    let priorities: Vec<String> = state.read().await.priorities.clone();

    // Extract hook events from the transcript.
    let mut events: Vec<RawEvent> = Vec::new();

    for msg in &messages {
        if msg.get("type").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }

        let ts = msg.get("timestamp").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let content = msg.pointer("/message/content");
        let stop_reason = msg.pointer("/message/stop_reason").and_then(|v| v.as_str()).unwrap_or("");

        if let Some(arr) = content.and_then(|c| c.as_array()) {
            for block in arr {
                if block.get("type").and_then(|v| v.as_str()) != Some("tool_use") {
                    continue;
                }
                let tool = block.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
                let summary = tool_summary(block);
                events.push(RawEvent {
                    kind: "PostToolUse".to_string(),
                    tool_name: tool.to_string(),
                    timestamp: ts.clone(),
                    summary: format!("{}: {}", tool, summary),
                    stop_reason: None,
                });
            }
        }

        // Live coach observes the Stop hook regardless of stop_reason,
        // but in the JSONL transcript the only assistant frames that
        // would have triggered Stop are the `end_turn` ones. Filtering
        // here keeps replay aligned with what the user actually saw.
        if stop_reason == "end_turn" {
            events.push(RawEvent {
                kind: "Stop".to_string(),
                tool_name: String::new(),
                timestamp: ts,
                summary: "end_turn".to_string(),
                stop_reason: Some(stop_reason.to_string()),
            });
        }
    }

    // Evaluate events. Running counters give the LLM the same
    // `StopContext` shape the live coach builds — tool_counts,
    // stop_count, stop_blocked_count — at each point in the timeline.
    let mut replay_events = Vec::new();
    let mut first_intervention: Option<usize> = None;
    let mut tool_counts: HashMap<String, usize> = HashMap::new();
    let mut stop_count: usize = 0;
    let mut stop_blocked_count: usize = 0;
    let mut rules_stop_blocked = false; // rules-mode "block first Stop only"

    for (i, ev) in events.iter().enumerate() {
        if ev.kind == "PostToolUse" && !ev.tool_name.is_empty() {
            *tool_counts.entry(ev.tool_name.clone()).or_insert(0) += 1;
        }
        if ev.kind == "Stop" {
            stop_count += 1;
        }

        let (action, message) = if mode == "present" || ev.kind != "Stop" {
            (None, None)
        } else if mode == "llm" {
            let ctx = crate::llm::StopContext {
                priorities: priorities.clone(),
                cwd: if cwd.is_empty() { None } else { Some(cwd.clone()) },
                tool_counts: tool_counts.clone(),
                stop_count,
                stop_blocked_count,
                stop_reason: ev.stop_reason.clone(),
            };
            match crate::llm::evaluate_stop(state, &ctx).await {
                Ok(decision) if decision.allow => (None, None),
                Ok(decision) => {
                    stop_blocked_count += 1;
                    let msg = decision
                        .message
                        .filter(|m| !m.trim().is_empty())
                        .unwrap_or_else(|| crate::state::away_message(&priorities));
                    (Some("blocked".to_string()), Some(msg))
                }
                Err(e) => (
                    Some("error".to_string()),
                    Some(format!("[LLM error] {e}")),
                ),
            }
        } else if rules_stop_blocked {
            (None, None)
        } else {
            rules_stop_blocked = true;
            stop_blocked_count += 1;
            (
                Some("blocked".to_string()),
                Some(crate::state::away_message(&priorities)),
            )
        };

        let is_intervention = action.is_some();

        replay_events.push(ReplayEvent {
            index: i,
            kind: ev.kind.clone(),
            tool_name: ev.tool_name.clone(),
            timestamp: ev.timestamp.clone(),
            summary: ev.summary.clone(),
            action,
            message,
        });

        if is_intervention && first_intervention.is_none() {
            first_intervention = Some(i);
        }
    }

    Ok(ReplayResult {
        session_id: session_id.to_string(),
        topic,
        cwd,
        message_count: messages.len(),
        user_message_count: user_count,
        assistant_message_count: asst_count,
        event_count: replay_events.len(),
        events: replay_events,
        first_intervention_index: first_intervention,
    })
}

fn tool_summary(block: &serde_json::Value) -> String {
    let input = block.get("input").unwrap_or(&serde_json::Value::Null);
    if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
        return truncate(cmd, 80);
    }
    if let Some(fp) = input.get("file_path").and_then(|v| v.as_str()) {
        return fp.to_string();
    }
    if let Some(pat) = input.get("pattern").and_then(|v| v.as_str()) {
        return format!("pattern=\"{}\"", pat);
    }
    truncate(&input.to_string(), 80)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_sessions_returns_vec() {
        // Should not panic even if ~/.claude/projects doesn't exist
        let sessions = list_sessions(10);
        // Just verify it returns without error — actual content depends on machine state
        assert!(sessions.len() <= 10);
    }

    #[test]
    fn truncate_works() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hello...");
    }

    /// Property: `truncate(s, max)` never panics, regardless of where
    /// `max` lands in `s`'s UTF-8 byte sequence. Regression for the
    /// char-boundary slice bug — running `cargo test` on a VPS with a
    /// real claude transcript containing `’` (U+2019, 3 bytes) across
    /// byte 100 panicked at `&s[..max]`. We exhaustively try every
    /// `max` from 0 through `s.len() + 2` against strings packed with
    /// multi-byte codepoints (2-, 3-, and 4-byte sequences) so any
    /// future regression that re-introduces a raw byte slice gets
    /// caught at unit-test time, not in production.
    #[test]
    fn truncate_never_panics_inside_a_multibyte_char() {
        let inputs = [
            "ascii only",
            "café",                      // 2-byte: é
            "naïveté",                   // 2-byte: ï, é
            "what’s up",                 // 3-byte: ’ (U+2019)
            "日本語テスト",              // 3-byte each
            "🚀 rockets ⛵ boats 🦀 crabs", // 4-byte emoji + 3-byte
            // Long string built so the panic-prone byte 100 would land
            // exactly inside a multi-byte codepoint, mirroring the
            // real bug we hit.
            &("a".repeat(98) + "’ trailing text"),
        ];
        for s in inputs {
            for max in 0..=s.len() + 2 {
                let out = truncate(s, max);
                // Output must always be valid UTF-8 (the type system
                // enforces this — `String` can't be otherwise — so
                // reaching this line is what we're really proving).
                assert!(
                    out.len() <= s.len() + 3,
                    "truncate({s:?}, {max}) returned longer than input + '...'"
                );
            }
        }
    }

    #[tokio::test]
    async fn replay_nonexistent_session_returns_error() {
        use std::sync::Arc;
        use tokio::sync::RwLock;
        let state: SharedState = Arc::new(RwLock::new(crate::state::test_state()));
        let result = replay_session("nonexistent-id-12345", "away", &state).await;
        assert!(result.is_err());
    }

    /// Live integration test for `mode = "llm"`: pick the smallest real
    /// session in `~/.claude/projects/`, replay it through Gemini, and
    /// assert that every Stop event got a real verdict from the model
    /// (allowed or blocked — never `"error"`, which would mean the LLM
    /// call itself failed).
    ///
    /// Ignored by default because it costs a few API calls. Run with
    /// `cargo test --lib replay::tests::replay_llm_mode_calls_real_llm -- --ignored --nocapture`
    /// (requires `GOOGLE_API_KEY` and at least one saved Claude Code
    /// session containing assistant messages).
    #[tokio::test]
    #[ignore = "live LLM call — run with --ignored"]
    async fn replay_llm_mode_calls_real_llm() {
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let google_key = std::env::var("GOOGLE_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .expect("GOOGLE_API_KEY must be set for the live LLM replay test");

        // Pick the smallest session with at least one assistant message.
        // Smaller = fewer Stop events = fewer LLM calls.
        let mut sessions = list_sessions(50);
        sessions.sort_by_key(|s| s.message_count);
        let session = sessions
            .into_iter()
            .find(|s| s.assistant_message_count > 0)
            .expect("no saved sessions with assistant messages in ~/.claude/projects/");
        eprintln!(
            "[live] replaying session {} ({} msgs, {} assistant)",
            session.id, session.message_count, session.assistant_message_count
        );

        // Use the shared `test_state()` builder so we don't depend on
        // the user's `~/.coach/settings.json` (which might point at a
        // model the test environment can't reach). Inject only the
        // Google token so `pick_verifier` finds no second provider and
        // we make exactly one LLM call per Stop.
        let mut coach = crate::state::test_state();
        coach.env_tokens.insert("google".into(), google_key);
        let state: SharedState = Arc::new(RwLock::new(coach));

        let result = replay_session(&session.id, "llm", &state)
            .await
            .expect("replay_session returned an error");

        let stops: Vec<&ReplayEvent> = result
            .events
            .iter()
            .filter(|e| e.kind == "Stop")
            .collect();
        assert!(
            !stops.is_empty(),
            "selected session {} has no Stop events; cannot exercise the LLM path",
            session.id
        );

        let errors: Vec<&&ReplayEvent> = stops
            .iter()
            .filter(|e| e.action.as_deref() == Some("error"))
            .collect();
        assert!(
            errors.is_empty(),
            "{}/{} Stop events errored from the LLM call. First error: {:?}",
            errors.len(),
            stops.len(),
            errors.first().and_then(|e| e.message.as_ref())
        );

        eprintln!("[live] {} Stop events evaluated by Gemini:", stops.len());
        for ev in &stops {
            let verdict = ev.action.as_deref().unwrap_or("allowed");
            let preview = ev
                .message
                .as_ref()
                .and_then(|m| m.lines().next())
                .unwrap_or("");
            eprintln!("  event {:>3}: {:<8} {}", ev.index + 1, verdict, preview);
        }
    }
}
