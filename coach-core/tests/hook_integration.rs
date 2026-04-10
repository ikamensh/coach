/// Integration tests for the Coach hook server.
///
/// The main tests verify session tracking via direct HTTP calls to the hook
/// endpoints — this covers the contract between Claude Code and Coach without
/// requiring Claude Code to be installed.
///
/// PID resolution is normally handled by `lsof` against the request's TCP
/// peer port. Tests run client and server in the same process, so we inject
/// a fake resolver (`fake_resolver_from_sid`) that hashes the session_id to
/// a deterministic non-zero u32. Tests can compute the same fake PID via
/// `coach_core::server::fake_pid_for_sid` to look up state.
///
/// The `test_with_real_claude_code` test (ignored by default) launches the
/// actual `claude` CLI against a temporary project and checks that its
/// session appears in Coach state. Run it with:
///     cargo test -p coach -- --ignored
use coach_core::server::fake_pid_for_sid;
use coach_core::settings::Settings;
use coach_core::state::CoachState;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Start the hook server on an OS-assigned port and return its base URL.
async fn start_test_server() -> (String, Arc<RwLock<CoachState>>) {
    let state = Arc::new(RwLock::new(CoachState::from_settings(Settings::default())));
    let router = coach_core::server::create_router_headless(
        state.clone(),
        coach_core::server::fake_resolver_from_sid(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    (format!("http://127.0.0.1:{}", port), state)
}

#[tokio::test]
async fn post_tool_use_creates_session() {
    let (base, _state) = start_test_server().await;
    let client = reqwest::Client::new();

    // No sessions yet.
    let snap: serde_json::Value = client
        .get(format!("{base}/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(snap["sessions"].as_array().unwrap().len(), 0);

    // Simulate a PostToolUse event from Claude Code.
    let resp = client
        .post(format!("{base}/hook/post-tool-use"))
        .json(&serde_json::json!({
            "session_id": "sess-1",
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "cwd": "/tmp/my-project"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Session should now exist.
    let snap: serde_json::Value = client
        .get(format!("{base}/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sessions = snap["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["session_id"], "sess-1");
    assert_eq!(sessions[0]["pid"], fake_pid_for_sid("sess-1"));
    assert_eq!(sessions[0]["cwd"], "/tmp/my-project");
    assert_eq!(sessions[0]["event_count"], 1);
}

/// Cursor routes use session id → synthetic PID (subprocess curl is not the agent).
#[tokio::test]
async fn cursor_after_shell_tracks_session() {
    let (base, _state) = start_test_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/cursor/hook/after-shell"))
        .json(&serde_json::json!({
            "sessionId": "cursor-sess-1",
            "command": "echo hi",
            "cwd": "/tmp/cursor-proj"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let snap: serde_json::Value = client
        .get(format!("{base}/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sessions = snap["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["session_id"], "cursor-sess-1");
    assert_eq!(sessions[0]["pid"], fake_pid_for_sid("cursor-sess-1"));
    assert_eq!(sessions[0]["cwd"], "/tmp/cursor-proj");
    // Property: a session created by ANY cursor hook is tagged as
    // belonging to Cursor, so the frontend renders the cursor icon.
    assert_eq!(sessions[0]["client"], "cursor");
}

/// Property: sessions created via the Claude Code hook routes are
/// tagged as Claude (the default), regardless of whether the cursor
/// `mark_client` path also runs.
#[tokio::test]
async fn claude_post_tool_use_marks_session_as_claude() {
    let (base, _state) = start_test_server().await;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/hook/post-tool-use"))
        .json(&serde_json::json!({
            "session_id": "claude-1",
            "hook_event_name": "PostToolUse",
            "tool_name": "Read"
        }))
        .send()
        .await
        .unwrap();

    let snap: serde_json::Value = client
        .get(format!("{base}/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sessions = snap["sessions"].as_array().unwrap();
    let session = sessions
        .iter()
        .find(|s| s["session_id"] == "claude-1")
        .expect("session should exist");
    assert_eq!(session["client"], "claude");
}

#[tokio::test]
async fn multiple_sessions_tracked_independently() {
    let (base, _state) = start_test_server().await;
    let client = reqwest::Client::new();

    for id in ["alpha", "beta"] {
        client
            .post(format!("{base}/hook/post-tool-use"))
            .json(&serde_json::json!({
                "session_id": id,
                "hook_event_name": "PostToolUse",
                "tool_name": "Read",
                "cwd": format!("/projects/{id}")
            }))
            .send()
            .await
            .unwrap();
    }

    // Send a second event for alpha.
    client
        .post(format!("{base}/hook/post-tool-use"))
        .json(&serde_json::json!({
            "session_id": "alpha",
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit"
        }))
        .send()
        .await
        .unwrap();

    let snap: serde_json::Value = client
        .get(format!("{base}/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sessions = snap["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);

    let alpha = sessions
        .iter()
        .find(|s| s["session_id"] == "alpha")
        .unwrap();
    assert_eq!(alpha["event_count"], 2);
    assert_eq!(alpha["cwd"], "/projects/alpha");

    let beta = sessions
        .iter()
        .find(|s| s["session_id"] == "beta")
        .unwrap();
    assert_eq!(beta["event_count"], 1);
}

/// Regression: a window's title used to drift to whichever subdirectory
/// the latest hook reported (e.g. `coach/src-tauri` after a `cd` in a
/// Bash tool, even though the user launched Claude in `coach`). The
/// launch directory must be frozen on first observation. End-to-end via
/// HTTP so this would catch a regression in the wiring between the
/// hook handler and the state mutator, not just the state unit test.
#[tokio::test]
async fn launch_cwd_frozen_across_subsequent_hooks() {
    let (base, _state) = start_test_server().await;
    let client = reqwest::Client::new();

    // First hook: claude was launched in /Users/foo/projects/coach.
    client
        .post(format!("{base}/hook/post-tool-use"))
        .json(&serde_json::json!({
            "session_id": "drift-me",
            "hook_event_name": "PostToolUse",
            "tool_name": "Read",
            "cwd": "/Users/foo/projects/coach",
        }))
        .send()
        .await
        .unwrap();

    // Subsequent hooks report a deeper cwd (Claude `cd`'d into
    // src-tauri at some point).
    for _ in 0..3 {
        client
            .post(format!("{base}/hook/post-tool-use"))
            .json(&serde_json::json!({
                "session_id": "drift-me",
                "hook_event_name": "PostToolUse",
                "tool_name": "Bash",
                "cwd": "/Users/foo/projects/coach/src-tauri",
            }))
            .send()
            .await
            .unwrap();
    }

    let snap: serde_json::Value = client
        .get(format!("{base}/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sess = snap["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["session_id"] == "drift-me")
        .expect("session should exist");

    assert_eq!(
        sess["cwd"], "/Users/foo/projects/coach",
        "launch cwd must NOT drift to a deeper subdirectory",
    );
    assert_eq!(
        sess["display_name"], "coach",
        "title must reflect the launch dir, not the deepest cwd",
    );
}

#[tokio::test]
async fn permission_request_auto_approves_in_away_mode() {
    let (base, state) = start_test_server().await;
    let client = reqwest::Client::new();

    // First create a session via any hook, then switch it to away.
    client
        .post(format!("{base}/hook/post-tool-use"))
        .json(&serde_json::json!({
            "session_id": "away-sess",
            "hook_event_name": "PostToolUse",
            "tool_name": "Read"
        }))
        .send()
        .await
        .unwrap();

    {
        let mut s = state.write().await;
        s.default_mode = coach_core::state::CoachMode::Away;
        let pid = fake_pid_for_sid("away-sess");
        if let Some(sess) = s.sessions.get_mut(&pid) {
            sess.mode = coach_core::state::CoachMode::Away;
        }
    }

    // PermissionRequest should auto-approve.
    let resp: serde_json::Value = client
        .post(format!("{base}/hook/permission-request"))
        .json(&serde_json::json!({
            "session_id": "away-sess",
            "hook_event_name": "PermissionRequest",
            "tool_name": "Bash"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let decision = &resp["hookSpecificOutput"]["decision"]["behavior"];
    assert_eq!(decision, "allow");
}

#[tokio::test]
async fn stop_blocks_then_allows_on_cooldown() {
    let (base, state) = start_test_server().await;
    let client = reqwest::Client::new();

    // Create session in away mode.
    client
        .post(format!("{base}/hook/post-tool-use"))
        .json(&serde_json::json!({
            "session_id": "stop-sess",
            "hook_event_name": "PostToolUse",
            "tool_name": "Read"
        }))
        .send()
        .await
        .unwrap();

    {
        let mut s = state.write().await;
        // This test exercises the rules/cooldown stop path, so force Rules mode.
        s.coach_mode = coach_core::settings::EngineMode::Rules;
        let pid = fake_pid_for_sid("stop-sess");
        if let Some(sess) = s.sessions.get_mut(&pid) {
            sess.mode = coach_core::state::CoachMode::Away;
        }
    }

    // First Stop → should block.
    let resp: serde_json::Value = client
        .post(format!("{base}/hook/stop"))
        .json(&serde_json::json!({
            "session_id": "stop-sess",
            "hook_event_name": "Stop"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // Stop hooks use top-level fields, NOT hookSpecificOutput.
    assert!(
        resp.get("hookSpecificOutput").is_none(),
        "stop hook must NOT use hookSpecificOutput — Claude Code rejects it"
    );
    assert_eq!(resp["decision"], "block");
    assert!(resp["reason"].as_str().unwrap().contains("priorities"));

    // Second Stop (within cooldown) → should pass through (empty object).
    let resp: serde_json::Value = client
        .post(format!("{base}/hook/stop"))
        .json(&serde_json::json!({
            "session_id": "stop-sess",
            "hook_event_name": "Stop"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(resp.as_object().unwrap().is_empty(), "cooldown passthrough should be {{}}");
}

// ── /clear handling: same window, new conversation ────────────────────

/// The big regression test: a /clear in a Claude Code window must NOT
/// produce a duplicate row. Same fake PID, new session_id → existing
/// session is replaced (counters reset, conversation id swapped) instead
/// of a second session being created.
#[tokio::test]
async fn clear_replaces_session_in_same_window() {
    let (base, state) = start_test_server().await;
    let client = reqwest::Client::new();

    // Two PostToolUse events for the original conversation.
    for tool in ["Read", "Bash"] {
        client
            .post(format!("{base}/hook/post-tool-use"))
            .json(&serde_json::json!({
                "session_id": "before-clear",
                "hook_event_name": "PostToolUse",
                "tool_name": tool,
                "cwd": "/projects/coach"
            }))
            .send()
            .await
            .unwrap();
    }

    // SessionStart fires immediately after /clear with the new conv id.
    // Critical: in real Claude Code the source TCP port stays inside the
    // same Claude Code process, so the resolver returns the same PID.
    // Our fake resolver hashes session_id, so we need to spoof: instead
    // of a new session_id giving a new fake PID (which would fail to
    // simulate /clear), we directly inject the cache entry.
    {
        let mut s = state.write().await;
        let pid = fake_pid_for_sid("before-clear");
        s.session_id_to_pid.insert("after-clear".to_string(), pid);
    }

    client
        .post(format!("{base}/hook/session-start"))
        .json(&serde_json::json!({
            "session_id": "after-clear",
            "hook_event_name": "SessionStart",
            "source": "clear",
            "cwd": "/projects/coach"
        }))
        .send()
        .await
        .unwrap();

    // Session list still has exactly ONE entry, but it's now the new
    // conversation with reset counters.
    let snap: serde_json::Value = client
        .get(format!("{base}/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sessions = snap["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "no duplicate row after /clear");
    assert_eq!(sessions[0]["session_id"], "after-clear");
    // apply_hook_event resets counters; event_count stays 0 until record_tool.
    assert_eq!(sessions[0]["event_count"], 0);
    let tool_counts = sessions[0]["tool_counts"].as_object().unwrap();
    assert!(tool_counts.is_empty(), "tool counts reset on /clear");
    // PID is still the same window.
    assert_eq!(sessions[0]["pid"], fake_pid_for_sid("before-clear"));
}

// ── Scanner / hook integration ────────────────────────────────────────

/// The scanner should populate one entry per live PID found in
/// ~/.claude/sessions/. The launch-time sessionId in the file is
/// ignored — sessions start with empty current_session_id until a hook
/// fills it in.
#[tokio::test]
async fn scanner_discovers_real_sessions() {
    let state = Arc::new(RwLock::new(CoachState::from_settings(Settings::default())));

    coach_core::scanner::sync_sessions(&state, &coach_core::NoopEmitter).await;

    let coach = state.read().await;
    let live_files = coach_core::scanner::scan_live_sessions();

    for file in &live_files {
        let sess = coach
            .sessions
            .get(&file.pid)
            .unwrap_or_else(|| panic!("PID {} from session file should be in state", file.pid));
        assert_eq!(sess.pid, file.pid);
        // Sessions are bootstrapped from their JSONL, so event_count may be > 0.
        assert!(sess.bootstrapped, "discovered sessions should be bootstrapped");
    }
}

/// A hook event arriving for a scanner-discovered PID should adopt the
/// conversation id without resetting started_at — the scanner already
/// populated it from the file.
#[tokio::test]
async fn hook_adopts_scanner_discovered_pid() {
    let (base, state) = start_test_server().await;

    // Simulate scanner discovering a process. Use the hash for the
    // hook session_id we're about to send so the resolver lines up.
    let sid = "adopt-me";
    let scanner_pid = fake_pid_for_sid(sid);
    let scanner_started = chrono::Utc::now() - chrono::Duration::hours(1);
    {
        let mut coach = state.write().await;
        coach.register_discovered_pid(scanner_pid, Some("/tmp/project"), scanner_started);
    }

    // Hook event arrives.
    let client = reqwest::Client::new();
    client
        .post(format!("{base}/hook/post-tool-use"))
        .json(&serde_json::json!({
            "session_id": sid,
            "hook_event_name": "PostToolUse",
            "tool_name": "Read",
            "cwd": "/tmp/project"
        }))
        .send()
        .await
        .unwrap();

    let coach = state.read().await;
    let sess = coach.sessions.get(&scanner_pid).unwrap();
    assert_eq!(sess.current_session_id, sid);
    assert_eq!(sess.event_count, 1);
    assert_eq!(
        sess.started_at, scanner_started,
        "scanner started_at must survive the first hook"
    );
}

// ── Outdated models rule ────────────────────────────────────────────────

#[tokio::test]
async fn post_tool_use_triggers_outdated_models_rule() {
    let (base, _state) = start_test_server().await;
    let client = reqwest::Client::new();

    let resp: serde_json::Value = client
        .post(format!("{base}/hook/post-tool-use"))
        .json(&serde_json::json!({
            "session_id": "rule-sess",
            "hook_event_name": "PostToolUse",
            "tool_name": "Write",
            "tool_input": {
                "file_path": "/tmp/app.py",
                "content": "model = genai.GenerativeModel('gemini-2.0-flash')\nresult = model.generate(prompt)"
            },
            "cwd": "/tmp"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let ctx = resp["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("should have additionalContext");
    assert!(ctx.contains("gemini-2.0-flash"), "should mention the outdated model");
    assert!(ctx.contains("gemini-2.5-flash"), "should suggest the replacement");
}

#[tokio::test]
async fn post_tool_use_passes_through_current_models() {
    let (base, _state) = start_test_server().await;
    let client = reqwest::Client::new();

    let resp: serde_json::Value = client
        .post(format!("{base}/hook/post-tool-use"))
        .json(&serde_json::json!({
            "session_id": "rule-sess-2",
            "hook_event_name": "PostToolUse",
            "tool_name": "Write",
            "tool_input": {
                "file_path": "/tmp/app.py",
                "content": "model = genai.GenerativeModel('gemini-2.5-flash')"
            },
            "cwd": "/tmp"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(
        resp.get("hookSpecificOutput").is_none(),
        "current models should not trigger the rule"
    );
}

// ── Hook response schema validation ───────────────────────────────────
//
// Claude Code validates hook responses against a strict schema per event type.
// Each hook type has a different response format:
//
//   PermissionRequest: { hookSpecificOutput: { decision: { behavior }, additionalContext? } }
//   PostToolUse:       { hookSpecificOutput: { additionalContext } }
//   Stop:              { decision: "block", reason: "..." }   (top-level, NO hookSpecificOutput)
//
// Passthrough (no intervention) is always `{}` for all hook types.

/// Validates that a hook response conforms to Claude Code's expected schema.
fn validate_hook_response(resp: &serde_json::Value, hook_event: &str) {
    let obj = resp.as_object().unwrap_or_else(|| panic!("{hook_event}: response must be object"));

    if obj.is_empty() {
        return;
    }

    match hook_event {
        "Stop" => {
            assert!(
                resp.get("hookSpecificOutput").is_none(),
                "Stop: must NOT use hookSpecificOutput"
            );
            let allowed: &[&str] = &["decision", "reason"];
            for key in obj.keys() {
                assert!(
                    allowed.contains(&key.as_str()),
                    "Stop: unexpected top-level key '{key}' (allowed: {allowed:?})"
                );
            }
            if let Some(d) = resp.get("decision") {
                assert_eq!(d, "block", "Stop: decision must be \"block\"");
            }
        }
        "PermissionRequest" | "PostToolUse" => {
            for key in obj.keys() {
                assert!(
                    key == "hookSpecificOutput",
                    "{hook_event}: unexpected top-level key '{key}'"
                );
            }
            let hso = resp["hookSpecificOutput"]
                .as_object()
                .unwrap_or_else(|| panic!("{hook_event}: hookSpecificOutput must be an object"));

            let allowed: &[&str] = match hook_event {
                "PermissionRequest" => &["decision", "additionalContext"],
                "PostToolUse" => &["additionalContext"],
                _ => unreachable!(),
            };
            for key in hso.keys() {
                assert!(
                    allowed.contains(&key.as_str()),
                    "{hook_event}: unexpected field '{key}' in hookSpecificOutput (allowed: {allowed:?})"
                );
            }

            if hook_event == "PermissionRequest" {
                if let Some(decision) = hso.get("decision") {
                    assert!(decision.is_object(), "PermissionRequest: decision must be an object");
                    let behavior = decision.get("behavior").and_then(|b| b.as_str())
                        .unwrap_or_else(|| panic!("PermissionRequest: decision needs 'behavior'"));
                    assert!(
                        behavior == "allow" || behavior == "deny",
                        "PermissionRequest: decision.behavior must be 'allow' or 'deny', got '{behavior}'"
                    );
                }
            }
        }
        other => panic!("unknown hook event: {other}"),
    }
}

#[tokio::test]
async fn all_hook_responses_conform_to_claude_code_schema() {
    let (base, state) = start_test_server().await;
    let client = reqwest::Client::new();

    let payload = |sid: &str, event: &str| -> serde_json::Value {
        serde_json::json!({
            "session_id": sid,
            "hook_event_name": event,
            "tool_name": "Bash",
            "tool_input": { "command": "echo hello" },
            "cwd": "/tmp/schema-test"
        })
    };

    // ── Present mode (default) ──────────────────────────────────────

    for (endpoint, event) in [
        ("hook/permission-request", "PermissionRequest"),
        ("hook/stop", "Stop"),
        ("hook/post-tool-use", "PostToolUse"),
    ] {
        let resp: serde_json::Value = client
            .post(format!("{base}/{endpoint}"))
            .json(&payload("schema-present", event))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

        validate_hook_response(&resp, event);
    }

    // ── Away mode ───────────────────────────────────────────────────

    {
        let mut s = state.write().await;
        s.default_mode = coach_core::state::CoachMode::Away;
        let pid = fake_pid_for_sid("schema-present");
        if let Some(sess) = s.sessions.get_mut(&pid) {
            sess.mode = coach_core::state::CoachMode::Away;
        }
    }

    // Create fresh session that will inherit away mode.
    let resp: serde_json::Value = client
        .post(format!("{base}/hook/permission-request"))
        .json(&payload("schema-away", "PermissionRequest"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    validate_hook_response(&resp, "PermissionRequest");

    // Stop in away mode (first call — should block).
    let resp: serde_json::Value = client
        .post(format!("{base}/hook/stop"))
        .json(&payload("schema-away", "Stop"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    validate_hook_response(&resp, "Stop");

    // Stop in away mode (second call — cooldown passthrough).
    let resp: serde_json::Value = client
        .post(format!("{base}/hook/stop"))
        .json(&payload("schema-away", "Stop"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    validate_hook_response(&resp, "Stop");

    // PostToolUse in away mode.
    let resp: serde_json::Value = client
        .post(format!("{base}/hook/post-tool-use"))
        .json(&payload("schema-away", "PostToolUse"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    validate_hook_response(&resp, "PostToolUse");
}

#[tokio::test]
async fn post_tool_use_rule_response_schema() {
    let (base, _state) = start_test_server().await;
    let client = reqwest::Client::new();

    let resp: serde_json::Value = client
        .post(format!("{base}/hook/post-tool-use"))
        .json(&serde_json::json!({
            "session_id": "rule-schema",
            "hook_event_name": "PostToolUse",
            "tool_name": "Write",
            "tool_input": {
                "content": "model = 'gpt-3.5-turbo'"
            }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    validate_hook_response(&resp, "PostToolUse");
    assert!(
        resp["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .is_some(),
        "expected rule to produce additionalContext"
    );
}

// ── LLM mode fallback + observer wiring ────────────────────────────────
//
// These tests cover the LLM code paths (chained evaluator, fire-and-forget
// observer) without needing a real LLM key. `chain_openai` fails fast at the
// "no token for primary provider" check before making any HTTP call, so the
// failure paths run in milliseconds.

async fn put_in_llm_mode_no_key(state: &Arc<RwLock<CoachState>>) {
    let mut s = state.write().await;
    s.coach_mode = coach_core::settings::EngineMode::Llm;
    s.model = coach_core::settings::ModelConfig {
        provider: "openai".into(),
        model: "gpt-5.4-mini".into(),
    };
    s.api_tokens.clear();
    s.env_tokens.clear();
}

#[tokio::test]
async fn stop_in_llm_mode_falls_back_to_fixed_when_no_key() {
    let (base, state) = start_test_server().await;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/hook/post-tool-use"))
        .json(&serde_json::json!({
            "session_id": "llm-fallback",
            "hook_event_name": "PostToolUse",
            "tool_name": "Read"
        }))
        .send()
        .await
        .unwrap();

    put_in_llm_mode_no_key(&state).await;
    {
        let mut s = state.write().await;
        let pid = fake_pid_for_sid("llm-fallback");
        if let Some(sess) = s.sessions.get_mut(&pid) {
            sess.mode = coach_core::state::CoachMode::Away;
        }
    }

    let resp: serde_json::Value = client
        .post(format!("{base}/hook/stop"))
        .json(&serde_json::json!({
            "session_id": "llm-fallback",
            "hook_event_name": "Stop"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    validate_hook_response(&resp, "Stop");
    assert_eq!(resp["decision"], "block");
    let reason = resp["reason"].as_str().unwrap();
    assert!(
        reason.contains("priorities") || reason.contains("away"),
        "fallback should look like the fixed away_message; got: {reason}"
    );
}

#[tokio::test]
async fn observer_fires_in_llm_mode_and_records_failure() {
    let (base, state) = start_test_server().await;
    let client = reqwest::Client::new();

    put_in_llm_mode_no_key(&state).await;

    let resp = client
        .post(format!("{base}/hook/post-tool-use"))
        .json(&serde_json::json!({
            "session_id": "obs-fires",
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "tool_input": {"file_path": "/x.py", "new_string": "ok"}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "hook must respond immediately");

    let pid = fake_pid_for_sid("obs-fires");
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        let s = state.read().await;
        if let Some(sess) = s.sessions.get(&pid) {
            if sess.activity.iter().any(|a| a.hook_event == "Observer") {
                let observer_entry = sess
                    .activity
                    .iter()
                    .find(|a| a.hook_event == "Observer")
                    .unwrap();
                assert_eq!(
                    observer_entry.action, "error",
                    "no-token failure should be logged as Observer/error"
                );
                // Counters: error path must tick coach_errors and leave
                // coach_calls / usage at zero (no successful round-trip).
                assert_eq!(sess.coach.telemetry.errors, 1, "error counter should tick");
                assert_eq!(sess.coach.telemetry.calls, 0, "no successful call yet");
                assert!(sess.coach.telemetry.last_called_at.is_none());
                assert_eq!(sess.coach.telemetry.total_usage.input_tokens, 0);
                // The error message must be captured so the panel can show
                // it. Should match the activity log detail exactly.
                assert!(sess.coach.memory.last_error.is_some());
                assert_eq!(
                    sess.coach.memory.last_error.as_deref(),
                    observer_entry.detail.as_deref(),
                );
                return;
            }
        }
    }
    panic!("observer task never recorded an Observer entry within 500ms");
}

#[tokio::test]
async fn observer_does_not_fire_in_rules_mode() {
    let (base, state) = start_test_server().await;
    {
        let mut s = state.write().await;
        s.coach_mode = coach_core::settings::EngineMode::Rules;
    }
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/hook/post-tool-use"))
        .json(&serde_json::json!({
            "session_id": "rules-only",
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "tool_input": {"file_path": "/x.py", "new_string": "ok"}
        }))
        .send()
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let s = state.read().await;
    let pid = fake_pid_for_sid("rules-only");
    let sess = s.sessions.get(&pid).expect("session should exist");
    let observer_entries: Vec<_> = sess
        .activity
        .iter()
        .filter(|a| a.hook_event == "Observer")
        .collect();
    assert!(
        observer_entries.is_empty(),
        "Rules mode must not spawn observer; found {} Observer entries",
        observer_entries.len()
    );
}

#[tokio::test]
async fn observer_does_not_fire_for_non_capable_provider() {
    let (base, state) = start_test_server().await;
    let client = reqwest::Client::new();

    // openrouter is the one provider still NOT in OBSERVER_CAPABLE_PROVIDERS
    // — google joined the list once the emulated chain_gemini path landed,
    // so we pick openrouter to keep this gate test meaningful.
    {
        let mut s = state.write().await;
        s.coach_mode = coach_core::settings::EngineMode::Llm;
        s.model = coach_core::settings::ModelConfig {
            provider: "openrouter".into(),
            model: "openrouter/auto".into(),
        };
        s.api_tokens.clear();
        s.env_tokens.clear();
    }

    client
        .post(format!("{base}/hook/post-tool-use"))
        .json(&serde_json::json!({
            "session_id": "openrouter-llm",
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "tool_input": {"file_path": "/x.py", "new_string": "ok"}
        }))
        .send()
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let s = state.read().await;
    let pid = fake_pid_for_sid("openrouter-llm");
    let sess = s.sessions.get(&pid).expect("session should exist");
    assert!(
        sess.activity.iter().all(|a| a.hook_event != "Observer"),
        "non-capable provider must not spawn observer"
    );
}

// ── Real Claude Code integration ───────────────────────────────────────

#[tokio::test]
#[ignore] // Requires `claude` CLI — run with: cargo test -p coach -- --ignored
async fn test_with_real_claude_code() {
    let which = tokio::process::Command::new("which")
        .arg("claude")
        .output()
        .await
        .unwrap();
    if !which.status.success() {
        eprintln!("claude CLI not found, skipping");
        return;
    }

    let (base, _state) = start_test_server().await;
    let port: u16 = base.rsplit(':').next().unwrap().parse().unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let claude_dir = tmp.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();

    let hooks_config = serde_json::json!({
        "hooks": {
            "Stop": [{"hooks": [{"type": "http", "url": format!("http://localhost:{port}/hook/stop")}]}],
            "PostToolUse": [{"hooks": [{"type": "http", "url": format!("http://localhost:{port}/hook/post-tool-use")}]}],
            "PermissionRequest": [{"hooks": [{"type": "http", "url": format!("http://localhost:{port}/hook/permission-request")}]}]
        }
    });
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string_pretty(&hooks_config).unwrap(),
    )
    .unwrap();

    tokio::process::Command::new("git")
        .args(["init"])
        .current_dir(tmp.path())
        .output()
        .await
        .unwrap();

    let output = tokio::process::Command::new("claude")
        .args(["-p", "respond with just the word hello", "--output-format", "text"])
        .current_dir(tmp.path())
        .output()
        .await
        .expect("failed to run claude CLI");

    eprintln!("claude stdout: {}", String::from_utf8_lossy(&output.stdout));
    eprintln!("claude stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(
        output.status.success(),
        "claude exited with {:?}",
        output.status
    );

    let client = reqwest::Client::new();
    let snap: serde_json::Value = client
        .get(format!("{base}/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sessions = snap["sessions"].as_array().unwrap();
    assert!(
        !sessions.is_empty(),
        "expected claude to register a session via hooks, got none"
    );
    eprintln!(
        "tracked {} session(s), first id: {}",
        sessions.len(),
        sessions[0]["session_id"]
    );
}

/// Live integration check against the real `cursor-agent` CLI (the same
/// binary `kodo`'s cursor orchestrator drives). Empirically, headless
/// `cursor-agent -p` fires four of Coach's eight Cursor hooks:
/// `sessionStart`, `beforeShellExecution`, `afterShellExecution`, and
/// `afterFileEdit`. The other four (`beforeSubmitPrompt`, `afterMCPExecution`,
/// `afterAgentResponse`, `stop`) only fire from the IDE / from agent flows
/// the CLI doesn't exercise — that's a Cursor-side limitation, not ours.
///
/// This test exists because of a real bug we hit: Cursor's hook runner
/// silently rejects any hook command that mentions `curl` directly, so
/// the previous "curl in hooks.json" install path was inert. The fix is
/// the shim script approach in `install_cursor_hooks_at`. This test would
/// have caught the original bug — and will catch any future regression
/// to a curl-in-hooks-json shape.
///
/// The test writes a project-level `<tmp>/.cursor/hooks.json` plus shim
/// script (Cursor reads project + user + /etc, all merged) using the same
/// `install_cursor_hooks_at` helper production uses, then runs `cursor-
/// agent -p` against a prompt that forces a shell call and a file edit,
/// and asserts the session is registered with activity from both the
/// shell and edit hooks.
#[tokio::test]
#[ignore] // Requires `cursor-agent` CLI logged in — run with: cargo test -p coach -- --ignored
async fn test_with_real_cursor_agent() {
    let which = tokio::process::Command::new("which")
        .arg("cursor-agent")
        .output()
        .await
        .unwrap();
    if !which.status.success() {
        eprintln!("cursor-agent CLI not found, skipping");
        return;
    }

    let (base, _state) = start_test_server().await;
    let port: u16 = base.rsplit(':').next().unwrap().parse().unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let hooks_path = tmp.path().join(".cursor").join("hooks.json");
    let shim_path = tmp.path().join(".cursor").join("coach-cursor-hook.sh");
    coach_core::settings::install_cursor_hooks_at(port, &hooks_path, &shim_path)
        .expect("install cursor hooks");

    // git-init so cursor-agent treats the tempdir as a real workspace.
    tokio::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(tmp.path())
        .output()
        .await
        .unwrap();

    // A prompt that forces (a) a shell call and (b) a file edit, so we
    // exercise beforeShellExecution / afterShellExecution / afterFileEdit
    // in addition to sessionStart.
    let prompt = "Run the shell command 'echo hello-from-coach-test' and then \
                  create a file note.txt containing the word 'hi'. Then say done.";

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(180),
        tokio::process::Command::new("cursor-agent")
            .args([
                "-p",
                "-f",
                "--trust",
                "--output-format",
                "text",
                "--workspace",
                tmp.path().to_str().unwrap(),
                prompt,
            ])
            .current_dir(tmp.path())
            .output(),
    )
    .await
    .expect("cursor-agent timed out")
    .expect("failed to spawn cursor-agent");

    eprintln!(
        "cursor-agent stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    eprintln!(
        "cursor-agent stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "cursor-agent exited with {:?}",
        output.status
    );

    let client = reqwest::Client::new();
    let snap: serde_json::Value = client
        .get(format!("{base}/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sessions = snap["sessions"].as_array().unwrap();
    assert!(
        !sessions.is_empty(),
        "expected cursor-agent to register a session via hooks, got none"
    );
    eprintln!(
        "tracked {} cursor session(s), first id: {}",
        sessions.len(),
        sessions[0]["session_id"]
    );

    // Find the session whose activity matches what cursor-agent should have
    // generated (a shell call + a file edit). With multiple cursor-agent
    // runs in a row this could legitimately be split across sessions, so we
    // look across all of them.
    let saw_session_start = sessions.iter().any(|s| {
        s["activity"]
            .as_array()
            .map(|acts| acts.iter().any(|a| a["hook_event"] == "SessionStart"))
            .unwrap_or(false)
    });
    let saw_shell = sessions.iter().any(|s| {
        s["activity"]
            .as_array()
            .map(|acts| {
                acts.iter().any(|a| {
                    a["hook_event"] == "PermissionRequest"
                        || a["hook_event"] == "PostToolUse"
                })
            })
            .unwrap_or(false)
    });
    assert!(
        saw_session_start,
        "expected at least one SessionStart activity entry from cursor-agent's sessionStart hook"
    );
    assert!(
        saw_shell,
        "expected at least one tool-use activity (PermissionRequest from beforeShellExecution \
         or PostToolUse from after-shell/after-edit) — none of the cursor tool hooks fired"
    );

    // Cursor sends `workspace_roots: [...]` (not `cwd`) — this asserts
    // `cursor::cursor_cwd` actually picks it up. macOS resolves /tmp via
    // /private/tmp, so accept either form.
    let tmp_str = tmp.path().display().to_string();
    let private_tmp = format!("/private{tmp_str}");
    let saw_cwd = sessions.iter().any(|s| {
        s["cwd"]
            .as_str()
            .map(|c| c == tmp_str || c == private_tmp)
            .unwrap_or(false)
    });
    assert!(
        saw_cwd,
        "expected at least one session whose cwd matches the workspace ({tmp_str} or \
         {private_tmp}) — workspace_roots payload field not being read by cursor::cursor_cwd"
    );

    // Every session created via a cursor route must be tagged as Cursor
    // so the frontend renders the cursor icon, not the owl.
    assert!(
        sessions.iter().all(|s| s["client"] == "cursor"),
        "all live cursor-agent sessions should have client=\"cursor\", got: {sessions:?}"
    );
}

/// UserPromptSubmit must create the session if it doesn't exist, record a
/// "user spoke" entry in the session's activity log with the (truncated)
/// prompt text, and pass through with no decision payload.
#[tokio::test]
async fn user_prompt_submit_records_activity() {
    let (base, state) = start_test_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/hook/user-prompt-submit"))
        .json(&serde_json::json!({
            "session_id": "talk-1",
            "hook_event_name": "UserPromptSubmit",
            "prompt": "make the sessions stop jumping around",
            "cwd": "/projects/coach"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body.get("hookSpecificOutput").is_none()
            || body["hookSpecificOutput"].is_null(),
        "user prompt submit should pass through, got: {body}",
    );

    let snap: serde_json::Value = client
        .get(format!("{base}/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session = snap["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["session_id"] == "talk-1")
        .expect("session should exist after user prompt");
    let activity = session["activity"].as_array().unwrap();
    assert_eq!(activity.len(), 1);
    assert_eq!(activity[0]["hook_event"], "UserPromptSubmit");
    assert_eq!(activity[0]["action"], "user spoke");
    assert_eq!(activity[0]["detail"], "make the sessions stop jumping around");

    let pid = fake_pid_for_sid("talk-1");
    let guard = state.read().await;
    let session = guard.sessions.get(&pid).expect("session should exist in state");
    assert_eq!(
        session.coach.memory.last_user_prompt.as_deref(),
        Some("make the sessions stop jumping around")
    );
}

// ── /api/* CLI-facing endpoints ────────────────────────────────────────
//
// These mirror the Tauri commands. The CLI uses them when Coach is
// running so the GUI's in-memory state stays consistent with the file
// on disk. Property checked: each POST changes both the snapshot the
// next GET sees AND the underlying CoachState (so a save() call would
// reflect the change).

#[tokio::test]
async fn api_set_priorities_updates_state_and_snapshot() {
    let (base, state) = start_test_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/config/priorities"))
        .json(&serde_json::json!({ "priorities": ["X", "Y", "Z"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // The POST response itself should reflect the new value.
    let snap: serde_json::Value = resp.json().await.unwrap();
    let priorities: Vec<String> = snap["priorities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(priorities, vec!["X", "Y", "Z"]);

    // And the in-memory state should be updated, so a subsequent GET sees it.
    let snap2: serde_json::Value = client
        .get(format!("{base}/api/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(snap2["priorities"][0], "X");

    // And the underlying CoachState (the same one a save() would persist).
    let s = state.read().await;
    assert_eq!(s.priorities, vec!["X", "Y", "Z"]);
}

#[tokio::test]
async fn api_set_all_sessions_mode_flips_every_session() {
    let (base, state) = start_test_server().await;
    let client = reqwest::Client::new();

    // Create two sessions in present mode (the default).
    for sid in ["one", "two"] {
        client
            .post(format!("{base}/hook/post-tool-use"))
            .json(&serde_json::json!({
                "session_id": sid,
                "hook_event_name": "PostToolUse",
                "tool_name": "Read"
            }))
            .send()
            .await
            .unwrap();
    }

    let resp = client
        .post(format!("{base}/api/sessions/mode"))
        .json(&serde_json::json!({ "mode": "away" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let s = state.read().await;
    assert!(
        s.sessions.values().all(|sess| sess.mode == coach_core::state::CoachMode::Away),
        "every session must be in away mode after the bulk POST"
    );
    assert_eq!(s.default_mode, coach_core::state::CoachMode::Away);
}

#[tokio::test]
async fn api_set_session_mode_targets_one_pid() {
    let (base, state) = start_test_server().await;
    let client = reqwest::Client::new();

    for sid in ["alpha", "beta"] {
        client
            .post(format!("{base}/hook/post-tool-use"))
            .json(&serde_json::json!({
                "session_id": sid,
                "hook_event_name": "PostToolUse",
                "tool_name": "Read"
            }))
            .send()
            .await
            .unwrap();
    }

    let alpha_pid = fake_pid_for_sid("alpha");
    let beta_pid = fake_pid_for_sid("beta");

    let resp = client
        .post(format!("{base}/api/sessions/{alpha_pid}/mode"))
        .json(&serde_json::json!({ "mode": "away" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let s = state.read().await;
    assert_eq!(
        s.sessions.get(&alpha_pid).unwrap().mode,
        coach_core::state::CoachMode::Away
    );
    assert_eq!(
        s.sessions.get(&beta_pid).unwrap().mode,
        coach_core::state::CoachMode::Present,
        "beta must NOT be touched by a per-pid POST to alpha"
    );
}

#[tokio::test]
async fn api_set_session_mode_404_for_unknown_pid() {
    let (base, _state) = start_test_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/sessions/999999/mode"))
        .json(&serde_json::json!({ "mode": "away" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "unknown pid must yield 404, not silent no-op");
}

#[tokio::test]
async fn api_set_model_updates_state() {
    let (base, state) = start_test_server().await;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/api/config/model"))
        .json(&serde_json::json!({ "provider": "anthropic", "model": "claude-sonnet-4-6" }))
        .send()
        .await
        .unwrap();

    let s = state.read().await;
    assert_eq!(s.model.provider, "anthropic");
    assert_eq!(s.model.model, "claude-sonnet-4-6");
}

#[tokio::test]
async fn api_set_api_token_inserts_and_clears() {
    let (base, state) = start_test_server().await;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/api/config/api-token"))
        .json(&serde_json::json!({ "provider": "openai", "token": "sk-test" }))
        .send()
        .await
        .unwrap();
    {
        let s = state.read().await;
        assert_eq!(s.api_tokens.get("openai").map(String::as_str), Some("sk-test"));
    }

    // Empty token deletes the entry — matches the Tauri command behavior.
    client
        .post(format!("{base}/api/config/api-token"))
        .json(&serde_json::json!({ "provider": "openai", "token": "" }))
        .send()
        .await
        .unwrap();
    let s = state.read().await;
    assert!(!s.api_tokens.contains_key("openai"));
}

#[tokio::test]
async fn api_set_coach_mode_round_trip() {
    let (base, state) = start_test_server().await;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/api/config/coach-mode"))
        .json(&serde_json::json!({ "coach_mode": "llm" }))
        .send()
        .await
        .unwrap();
    {
        let s = state.read().await;
        assert_eq!(s.coach_mode, coach_core::settings::EngineMode::Llm);
    }

    client
        .post(format!("{base}/api/config/coach-mode"))
        .json(&serde_json::json!({ "coach_mode": "rules" }))
        .send()
        .await
        .unwrap();
    let s = state.read().await;
    assert_eq!(s.coach_mode, coach_core::settings::EngineMode::Rules);
}

#[tokio::test]
async fn api_set_rules_replaces_rule_list() {
    let (base, state) = start_test_server().await;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/api/config/rules"))
        .json(&serde_json::json!({
            "rules": [
                { "id": "outdated_models", "enabled": false },
                { "id": "custom_one", "enabled": true }
            ]
        }))
        .send()
        .await
        .unwrap();

    let s = state.read().await;
    assert_eq!(s.rules.len(), 2);
    assert!(s.rules.iter().any(|r| r.id == "outdated_models" && !r.enabled));
    assert!(s.rules.iter().any(|r| r.id == "custom_one" && r.enabled));
}

#[tokio::test]
async fn user_prompt_submit_truncates_long_prompts() {
    let (base, state) = start_test_server().await;
    let client = reqwest::Client::new();

    let long_prompt = "x".repeat(1000);
    client
        .post(format!("{base}/hook/user-prompt-submit"))
        .json(&serde_json::json!({
            "session_id": "longwinded",
            "hook_event_name": "UserPromptSubmit",
            "prompt": long_prompt.clone(),
        }))
        .send()
        .await
        .unwrap();

    let snap: serde_json::Value = client
        .get(format!("{base}/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session = snap["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["session_id"] == "longwinded")
        .unwrap();
    let detail = session["activity"][0]["detail"].as_str().unwrap();
    assert!(detail.ends_with("…"), "truncated prompts should end with ellipsis");
    // 200 x's plus the ellipsis character.
    assert_eq!(detail.chars().count(), 201);

    let pid = fake_pid_for_sid("longwinded");
    let guard = state.read().await;
    let session = guard.sessions.get(&pid).expect("session should exist in state");
    assert_eq!(
        session.coach.memory.last_user_prompt.as_deref(),
        Some(long_prompt.as_str())
    );
}

// ── Command-hook ghost session bug ─────────────────────────────────────
//
// With command-type hooks the TCP peer is curl/sh, not Claude Code.
// resolve_peer_pid returns curl's PID, which differs from the scanner-
// discovered Claude Code PID. This created a ghost session for curl
// while the real session got no activity.

/// Reproduce the ghost-session bug: scanner discovers Claude Code PID,
/// then a hook arrives from a different PID (simulating curl in the
/// command-hook shim). Activity must land on the scanner session.
#[tokio::test]
async fn command_hook_updates_scanner_session_not_ghost() {
    let state = Arc::new(RwLock::new(CoachState::from_settings(Settings::default())));
    let claude_pid: u32 = 100;
    let curl_pid: u32 = 200;

    // Scanner discovers Claude Code before any hooks arrive.
    state
        .write()
        .await
        .register_discovered_pid(claude_pid, Some("/projects"), chrono::Utc::now());

    // Resolver returns curl's PID; parent walk maps curl → Claude Code.
    let resolver: coach_core::server::PidResolver =
        Arc::new(move |_peer_port, _sid| Some(curl_pid));
    let parent_fn: coach_core::server::ParentPidFn = Arc::new(move |pid| {
        if pid == curl_pid { Some(claude_pid) } else { None }
    });
    let router = coach_core::server::create_router_headless_with_parent(
        state.clone(),
        resolver,
        parent_fn,
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // Send a UserPromptSubmit hook (the path the user reported).
    client
        .post(format!("{base}/hook/user-prompt-submit"))
        .json(&serde_json::json!({
            "session_id": "conv-1",
            "prompt": "hello",
            "cwd": "/projects"
        }))
        .send()
        .await
        .unwrap();

    let s = state.read().await;
    assert_eq!(
        s.sessions[&claude_pid].current_session_id, "conv-1",
        "scanner session should receive the hook event"
    );
    assert!(
        !s.sessions.contains_key(&curl_pid),
        "no ghost session should be created for curl's PID"
    );
}
