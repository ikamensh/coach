/// Integration tests for the Coach hook server.
///
/// The main tests verify session tracking via direct HTTP calls to the hook
/// endpoints — this covers the contract between Claude Code and Coach without
/// requiring Claude Code to be installed.
///
/// The `test_with_real_claude_code` test (ignored by default) launches the
/// actual `claude` CLI against a temporary project and checks that its session
/// appears in Coach state.  Run it with:
///     cargo test -p coach -- --ignored
use coach_lib::settings::Settings;
use coach_lib::state::CoachState;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Start the hook server on an OS-assigned port and return its base URL.
async fn start_test_server() -> (String, Arc<RwLock<CoachState>>) {
    let state = Arc::new(RwLock::new(CoachState::from_settings(Settings::default())));
    let router = coach_lib::server::create_router_headless(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
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
    assert_eq!(sessions[0]["cwd"], "/tmp/my-project");
    assert_eq!(sessions[0]["event_count"], 1);
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
        s.default_mode = coach_lib::state::CoachMode::Away;
        if let Some(sess) = s.sessions.get_mut("away-sess") {
            sess.mode = coach_lib::state::CoachMode::Away;
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
        if let Some(sess) = s.sessions.get_mut("stop-sess") {
            sess.mode = coach_lib::state::CoachMode::Away;
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

// ── Scanner discovers live sessions from ~/.claude/sessions/ ──────────

/// The scanner should pick up real Claude Code sessions running on this
/// machine.  We verify this by reading the session files ourselves and
/// checking that sync_sessions registers them in CoachState.
#[tokio::test]
async fn scanner_discovers_real_sessions() {
    let state = Arc::new(RwLock::new(CoachState::from_settings(Settings::default())));

    // Run a sync (reads real ~/.claude/sessions/).
    coach_lib::scanner::sync_sessions(&state, None).await;

    let coach = state.read().await;
    let live_files = coach_lib::scanner::scan_live_sessions();

    // Every live session file should have a matching entry in state.
    for file in &live_files {
        let sess = coach.sessions.get(&file.session_id);
        assert!(
            sess.is_some(),
            "session {} from file should be in state",
            file.session_id
        );
        let sess = sess.unwrap();
        assert_eq!(sess.pid, Some(file.pid));
        assert_eq!(sess.event_count, 0, "discovered sessions start with 0 events");
    }
}

/// A hook event arriving for a scanner-discovered session should merge
/// cleanly: increment event_count while preserving the PID.
#[tokio::test]
async fn hook_merges_with_scanner_discovered_session() {
    let (_base, state) = start_test_server().await;

    // Simulate scanner discovering a session.
    {
        let mut coach = state.write().await;
        coach.register_discovered("scan-merge", Some("/tmp/project"), chrono::Utc::now(), 42);
    }

    // Now a hook event arrives for the same session.
    let client = reqwest::Client::new();
    client
        .post(format!("{_base}/hook/post-tool-use"))
        .json(&serde_json::json!({
            "session_id": "scan-merge",
            "hook_event_name": "PostToolUse",
            "tool_name": "Read",
            "cwd": "/tmp/project"
        }))
        .send()
        .await
        .unwrap();

    let coach = state.read().await;
    let sess = coach.sessions.get("scan-merge").unwrap();
    assert_eq!(sess.event_count, 1, "hook should increment from 0 to 1");
    assert_eq!(sess.pid, Some(42), "PID should be preserved after hook update");
}

// ── Outdated models rule ────────────────────────────────────────────────

#[tokio::test]
async fn post_tool_use_triggers_outdated_models_rule() {
    let (base, _state) = start_test_server().await;
    let client = reqwest::Client::new();

    // Write tool with outdated model in content → should get additionalContext.
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

    // Write tool with current model → no intervention.
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

    // Empty object = passthrough, always valid.
    if obj.is_empty() {
        return;
    }

    match hook_event {
        "Stop" => {
            // Stop hooks use top-level fields, never hookSpecificOutput.
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
            // These use hookSpecificOutput.
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

            // PermissionRequest decision must be { behavior: "allow"|"deny" }.
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
    // Exercises every hook endpoint in every mode and validates the response schema.
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
        s.default_mode = coach_lib::state::CoachMode::Away;
        if let Some(sess) = s.sessions.get_mut("schema-present") {
            sess.mode = coach_lib::state::CoachMode::Away;
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
    // When a rule fires, the PostToolUse response must still conform to schema.
    let (base, _state) = start_test_server().await;
    let client = reqwest::Client::new();

    // Write tool input that triggers the outdated_models rule.
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
    // Rule should have fired — verify additionalContext is present.
    assert!(
        resp["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .is_some(),
        "expected rule to produce additionalContext"
    );
}

// ── Real Claude Code integration ───────────────────────────────────────

#[tokio::test]
#[ignore] // Requires `claude` CLI — run with: cargo test -p coach -- --ignored
async fn test_with_real_claude_code() {
    // Verify claude is installed.
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

    // Create a temp project with hooks pointing to our test server.
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

    // Claude Code needs a git repo to find project root.
    tokio::process::Command::new("git")
        .args(["init"])
        .current_dir(tmp.path())
        .output()
        .await
        .unwrap();

    // Run claude in print mode — minimal API usage.
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

    // Check that at least one session was tracked.
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
