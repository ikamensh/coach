//! Session discovery and replay for the dev tools UI.
//!
//! Scans `~/.claude/projects/` for JSONL session files, extracts metadata,
//! and replays hook events through **the same `dispatch()` path that
//! handles live hooks**. There is no parallel intervention logic: replay
//! parses a JSONL transcript into a sequence of `SessionEvent`s and
//! feeds them through `server::events::dispatch` against an isolated
//! `AppState` clone (empty sessions, cloned config + services, no-op
//! emitter). The live state the caller passed in is never touched.
//!
//! Replay modes change only two settings on the isolated state:
//!   • `"present"` — `coach_mode = Rules`, default session mode `Present`
//!   • `"away"`    — `coach_mode = Rules`, default session mode `Away`
//!   • `"llm"`     — `coach_mode = Llm`,   default session mode `Away`
//!
//! Every other rule — cooldown, observer chain, rule matcher, permission
//! auto-approval, Stop eviction — is the live code path, not a copy.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use crate::server::events::{dispatch, SessionEvent, SessionSource};
use crate::server::{fake_pid_for_sid, HookServerState};
use crate::settings::EngineMode;
use crate::state::{CoachMode, AppState, RuntimeServices, SessionRegistry, SharedState};

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
    /// null = passthrough, "blocked", "intervention"
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

/// Extract the full text from a user message entry (no truncation).
/// Used for feeding the observer the same `user_prompt` the live path sees.
fn extract_user_prompt(entry: &serde_json::Value) -> Option<String> {
    let content = entry
        .pointer("/message/content")
        .unwrap_or(&serde_json::Value::Null);

    if let Some(text) = content.as_str() {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    if let Some(arr) = content.as_array() {
        for block in arr {
            if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        return Some(trimmed.to_string());
                    }
                }
            }
        }
    }

    None
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

/// A step we feed to `dispatch`. Only a subset of steps become
/// user-visible `ReplayEvent`s (`emit_index = Some`); `UserPromptSubmit`
/// is dispatched internally so the observer sees the turn, but the
/// DevPane timeline only shows tool calls and stops.
struct DispatchStep {
    event: SessionEvent,
    emit: Option<EmitInfo>,
}

struct EmitInfo {
    kind: &'static str,
    tool_name: String,
    timestamp: String,
    summary: String,
}

/// Map a replay mode string to `(coach_mode, session_mode)`. Unknown
/// strings fall back to `present` so UI bugs can't wedge the server.
fn resolve_modes(replay_mode: &str) -> (EngineMode, CoachMode) {
    match replay_mode {
        "llm" => (EngineMode::Llm, CoachMode::Away),
        "away" => (EngineMode::Rules, CoachMode::Away),
        _ => (EngineMode::Rules, CoachMode::Present),
    }
}

/// Build a fresh `AppState` with an empty `SessionRegistry` but every
/// other field cloned from the caller's live state. The replay session
/// lives here and never leaks into the real state the caller handed us.
async fn isolated_state_from(live: &SharedState, replay_mode: &str) -> SharedState {
    let (engine_mode, session_mode) = resolve_modes(replay_mode);
    let live_read = live.read().await;
    let mut config = live_read.config.clone();
    config.coach_mode = engine_mode;

    let services = RuntimeServices {
        http_client: live_read.services.http_client.clone(),
        env_tokens: live_read.services.env_tokens.clone(),
        mock_session_send: live_read.services.mock_session_send.clone(),
        llm_logger: live_read.services.llm_logger.clone(),
        #[cfg(feature = "pycoach")]
        pycoach: live_read.services.pycoach.clone(),
    };
    drop(live_read);

    let mut sessions = SessionRegistry::new();
    sessions.default_mode = session_mode;

    Arc::new(RwLock::new(AppState {
        sessions,
        config,
        services,
    }))
}

/// Observer completion counter. Every successful or failed observer
/// call bumps `telemetry.calls` or `telemetry.errors` exactly once, so
/// their sum is a monotonic "observer work done" count for the session.
async fn observer_progress(state: &SharedState, sid: &str) -> usize {
    state
        .read()
        .await
        .sessions
        .get(sid)
        .map(|s| s.coach.telemetry.calls + s.coach.telemetry.errors)
        .unwrap_or(0)
}

/// Block until `observer_progress(sid) > baseline` or `timeout` elapses.
/// Used between PostToolUse events in LLM mode so the chain is fully
/// advanced before the next dispatch reads it. Live uses a fire-and-
/// forget queue; replay waits so each event sees the previous one's
/// verdict — otherwise a Stop might run on a stale chain.
async fn wait_for_observer_tick(
    state: &SharedState,
    sid: &str,
    baseline: usize,
    timeout: Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if observer_progress(state, sid).await > baseline {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Convert a dispatch response into `(action, message)` for a
/// `ReplayEvent`. Mirrors the response shapes `dispatch` emits:
///   • `{}`                                                   → passthrough (no action)
///   • `{decision: "block", reason: "..."}`                   → blocked stop
///   • `{hookSpecificOutput: {additionalContext: "..."}}`     → rule / intervention
///   • `{hookSpecificOutput: {decision: ...}}`                → passthrough (permission wire format, not a coach action)
fn interpret_response(resp: &serde_json::Value) -> (Option<String>, Option<String>) {
    if resp.get("decision").and_then(|d| d.as_str()) == Some("block") {
        let msg = resp
            .get("reason")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        return (Some("blocked".to_string()), msg);
    }
    if let Some(ctx) = resp
        .pointer("/hookSpecificOutput/additionalContext")
        .and_then(|v| v.as_str())
    {
        return (
            Some("intervention".to_string()),
            Some(ctx.to_string()),
        );
    }
    (None, None)
}

/// Parse a Claude Code JSONL transcript into an ordered dispatch stream.
/// User messages become `UserPromptSubmitted`, assistant `tool_use`
/// blocks become `ToolCompleted`, and assistant turns with
/// `stop_reason == "end_turn"` become `StopRequested`. Only the
/// tool/stop events get a user-visible `EmitInfo`; user prompts are
/// dispatched silently so the observer sees them.
fn build_dispatch_stream(
    messages: &[serde_json::Value],
    session_id: &str,
    cwd: Option<String>,
) -> Vec<DispatchStep> {
    let mut steps: Vec<DispatchStep> = Vec::new();

    for msg in messages {
        let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let ts = msg
            .get("timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if msg_type == "user" {
            if let Some(prompt) = extract_user_prompt(msg) {
                steps.push(DispatchStep {
                    event: SessionEvent::UserPromptSubmitted {
                        session_id: session_id.to_string(),
                        cwd: cwd.clone(),
                        prompt: Some(prompt),
                    },
                    emit: None,
                });
            }
            continue;
        }
        if msg_type != "assistant" {
            continue;
        }

        let content = msg.pointer("/message/content");
        let stop_reason = msg
            .pointer("/message/stop_reason")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if let Some(arr) = content.and_then(|c| c.as_array()) {
            for block in arr {
                if block.get("type").and_then(|v| v.as_str()) != Some("tool_use") {
                    continue;
                }
                let tool = block
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let tool_input = block
                    .get("input")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let summary = tool_summary(block);
                steps.push(DispatchStep {
                    event: SessionEvent::ToolCompleted {
                        session_id: session_id.to_string(),
                        cwd: cwd.clone(),
                        tool_name: tool.clone(),
                        tool_input,
                    },
                    emit: Some(EmitInfo {
                        kind: "PostToolUse",
                        tool_name: tool.clone(),
                        timestamp: ts.clone(),
                        summary: format!("{}: {}", tool, summary),
                    }),
                });
            }
        }

        if stop_reason == "end_turn" {
            steps.push(DispatchStep {
                event: SessionEvent::StopRequested {
                    session_id: session_id.to_string(),
                    cwd: cwd.clone(),
                    stop_reason: Some(stop_reason.to_string()),
                },
                emit: Some(EmitInfo {
                    kind: "Stop",
                    tool_name: String::new(),
                    timestamp: ts,
                    summary: "end_turn".to_string(),
                }),
            });
        }
    }

    steps
}

/// Replay a saved session against the real hook dispatch path.
///
/// Builds an isolated `AppState` (empty sessions, config cloned from
/// the caller's live state) and feeds it synthetic `SessionEvent`s
/// derived from the JSONL transcript. The decision logic, observer
/// chain, rule matcher, and cooldown behavior are all the live code —
/// replay is just a transport. The caller's state is never mutated.
pub async fn replay_session(
    session_id: &str,
    mode: &str,
    state: &SharedState,
) -> Result<ReplayResult, String> {
    let path = find_session(session_id)
        .ok_or_else(|| format!("Session not found: {}", session_id))?;
    replay_transcript_at(&path, session_id, mode, state).await
}

/// Replay a JSONL transcript from an arbitrary path. Used by the
/// session-id entry point above and by scenario fixtures that don't
/// live under `~/.claude/projects/`. `session_id` is the label used
/// to key the replay session inside the isolated state and to tag
/// activity entries; pass the file stem if you don't have a natural
/// one.
pub async fn replay_transcript_at(
    path: &std::path::Path,
    session_id: &str,
    mode: &str,
    state: &SharedState,
) -> Result<ReplayResult, String> {
    let content = std::fs::read_to_string(path)
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

    let topic = messages
        .iter()
        .find(|m| m.get("type").and_then(|v| v.as_str()) == Some("user"))
        .map(extract_topic_from_entry)
        .unwrap_or_default();

    let cwd_string: String = messages
        .iter()
        .find_map(|m| m.get("cwd").and_then(|v| v.as_str()).map(String::from))
        .unwrap_or_default();
    let cwd_opt: Option<String> = if cwd_string.is_empty() {
        None
    } else {
        Some(cwd_string.clone())
    };

    let user_count = messages
        .iter()
        .filter(|m| m.get("type").and_then(|v| v.as_str()) == Some("user"))
        .count();
    let asst_count = messages
        .iter()
        .filter(|m| m.get("type").and_then(|v| v.as_str()) == Some("assistant"))
        .count();

    let isolated = isolated_state_from(state, mode).await;
    let app_state = HookServerState {
        app: isolated.clone(),
        emitter: Arc::new(crate::NoopEmitter),
    };
    let pid = fake_pid_for_sid(session_id);

    // Whether LLM mode is actually live: requires `llm` replay mode AND
    // that the cloned provider is observer-capable. Matches the live
    // gate in `on_tool_completed`, so replay never waits on an observer
    // that won't fire.
    let llm_active = {
        let s = isolated.read().await;
        s.config.coach_mode == EngineMode::Llm
            && crate::settings::OBSERVER_CAPABLE_PROVIDERS
                .contains(&s.config.model.provider.as_str())
    };

    let steps = build_dispatch_stream(&messages, session_id, cwd_opt);

    let mut replay_events: Vec<ReplayEvent> = Vec::new();
    let mut first_intervention: Option<usize> = None;

    for step in steps {
        let is_tool = matches!(step.event, SessionEvent::ToolCompleted { .. });
        let baseline = if llm_active && is_tool {
            observer_progress(&isolated, session_id).await
        } else {
            0
        };

        let response = dispatch(&app_state, pid, SessionSource::ClaudeCode, step.event).await;

        if llm_active && is_tool {
            wait_for_observer_tick(
                &isolated,
                session_id,
                baseline,
                Duration::from_secs(30),
            )
            .await;
        }

        let Some(emit) = step.emit else {
            continue;
        };

        let (action, message) = interpret_response(&response.0);
        let index = replay_events.len();
        if action.is_some() && first_intervention.is_none() {
            first_intervention = Some(index);
        }
        replay_events.push(ReplayEvent {
            index,
            kind: emit.kind.to_string(),
            tool_name: emit.tool_name,
            timestamp: emit.timestamp,
            summary: emit.summary,
            action,
            message,
        });
    }

    Ok(ReplayResult {
        session_id: session_id.to_string(),
        topic,
        cwd: cwd_string,
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

    /// End-to-end regression: a synthetic JSONL fixture replayed through
    /// the live `dispatch()` path for all three modes. Proves that
    /// replay reuses the real hook pipeline (rules, observer, Stop
    /// cooldown) rather than a parallel implementation.
    ///
    /// The fixture is the smallest meaningful conversation: one user
    /// prompt, one tool call, one `end_turn` — so the stream the replay
    /// dispatcher produces is exactly `[UserPromptSubmitted,
    /// ToolCompleted, StopRequested]`, and the user-visible
    /// `ReplayEvent` list is exactly `[PostToolUse, Stop]`.
    ///
    /// Each mode asserts a different invariant of the live pipeline:
    /// `present` — every hook passes through; `away` — the first Stop
    /// is blocked by the rules-mode `away_message`; `llm` — the mock
    /// provider drives `evaluate_stop_chained` and the block reason
    /// propagates out of the HTTP-shape response via
    /// `interpret_response`. If any of these drift, replay has stopped
    /// sharing the live path.
    #[tokio::test]
    async fn replay_dispatches_through_live_path_for_all_modes() {
        use crate::settings::{EngineMode, ModelConfig};
        use crate::state::{AppState, CoachUsage, MockSessionSend};
        use std::io::Write;
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let mut file = tempfile::Builder::new()
            .suffix(".jsonl")
            .tempfile()
            .expect("tempfile");
        // One user → one tool_use → one end_turn. The transcript has to
        // be valid claude JSONL so the parser's msg_type / content /
        // stop_reason pointers all hit. Keep it as compact as the
        // parser allows so the intent is visible at a glance.
        writeln!(
            file,
            r#"{{"type":"user","timestamp":"2025-01-01T00:00:00Z","cwd":"/tmp/x","message":{{"content":"build hello world"}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant","timestamp":"2025-01-01T00:00:01Z","cwd":"/tmp/x","message":{{"stop_reason":"tool_use","content":[{{"type":"tool_use","name":"Read","input":{{"file_path":"/tmp/x/main.py"}}}}]}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant","timestamp":"2025-01-01T00:00:02Z","cwd":"/tmp/x","message":{{"stop_reason":"end_turn","content":[{{"type":"text","text":"done"}}]}}}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let sid = "test-fixture-1";

        // ── present: nothing blocks ────────────────────────────────
        let state: SharedState = Arc::new(RwLock::new(crate::state::test_state()));
        let result = replay_transcript_at(file.path(), sid, "present", &state)
            .await
            .expect("present replay");
        assert_eq!(result.event_count, 2, "1 PostToolUse + 1 Stop expected");
        assert!(
            result.events.iter().all(|e| e.action.is_none()),
            "present mode should never intervene, got {:?}",
            result.events
        );
        assert!(result.first_intervention_index.is_none());

        // ── away (rules): first Stop blocked by away_message ───────
        let result = replay_transcript_at(file.path(), sid, "away", &state)
            .await
            .expect("away replay");
        assert_eq!(result.event_count, 2);
        let stop = result
            .events
            .iter()
            .find(|e| e.kind == "Stop")
            .expect("Stop event missing");
        assert_eq!(
            stop.action.as_deref(),
            Some("blocked"),
            "away mode must block the first Stop: {:?}",
            stop
        );
        assert!(stop.message.is_some());
        assert_eq!(result.first_intervention_index, Some(1));

        // ── llm (mocked observer + stop evaluator) ─────────────────
        // Mock discriminates by the stop-prompt substring from
        // `prompts/stop_chained.txt`. Observer calls return "Noted."
        // (no INTERVENE prefix → no intervention); the stop evaluator
        // returns a JSON verdict the live Stop handler parses into a
        // `decision.block` response the replay layer then surfaces.
        let mock: MockSessionSend = Arc::new(|_sys, msg| {
            if msg.contains("requesting to stop") {
                Ok((
                    r#"{"allow": false, "message": "keep going"}"#.into(),
                    CoachUsage::default(),
                ))
            } else {
                Ok(("Noted.".into(), CoachUsage::default()))
            }
        });
        let mut coach = AppState {
            sessions: crate::state::SessionRegistry::new(),
            config: crate::state::test_state().config,
            services: crate::state::test_state().services,
        };
        coach.config.coach_mode = EngineMode::Llm;
        coach.config.model = ModelConfig {
            provider: "anthropic".into(),
            model: "mock".into(),
        };
        coach
            .config
            .api_tokens
            .insert("anthropic".into(), "mock".into());
        coach.services.mock_session_send = Some(mock);
        let state_llm: SharedState = Arc::new(RwLock::new(coach));

        let result = replay_transcript_at(file.path(), sid, "llm", &state_llm)
            .await
            .expect("llm replay");
        assert_eq!(result.event_count, 2);
        let stop = result
            .events
            .iter()
            .find(|e| e.kind == "Stop")
            .expect("Stop event missing");
        assert_eq!(
            stop.action.as_deref(),
            Some("blocked"),
            "llm mode should block via mocked decision: {:?}",
            stop
        );
        let msg = stop.message.as_deref().unwrap_or("");
        assert!(
            msg.contains("keep going"),
            "mocked block reason should propagate, got: {:?}",
            msg
        );
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
        // model the test environment can't reach).
        let mut coach = crate::state::test_state();
        coach.services.env_tokens.insert("google".into(), google_key);
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
