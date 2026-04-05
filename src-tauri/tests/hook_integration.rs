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
            "sessionId": "sess-1",
            "hookEventName": "PostToolUse",
            "toolName": "Bash",
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
                "sessionId": id,
                "hookEventName": "PostToolUse",
                "toolName": "Read",
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
            "sessionId": "alpha",
            "hookEventName": "PostToolUse",
            "toolName": "Edit"
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
            "sessionId": "away-sess",
            "hookEventName": "PostToolUse",
            "toolName": "Read"
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
            "sessionId": "away-sess",
            "hookEventName": "PermissionRequest",
            "toolName": "Bash"
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
            "sessionId": "stop-sess",
            "hookEventName": "PostToolUse",
            "toolName": "Read"
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
            "sessionId": "stop-sess",
            "hookEventName": "Stop"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp["hookSpecificOutput"]["decision"], "block");
    assert!(resp["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("priorities"));

    // Second Stop (within cooldown) → should pass through.
    let resp: serde_json::Value = client
        .post(format!("{base}/hook/stop"))
        .json(&serde_json::json!({
            "sessionId": "stop-sess",
            "hookEventName": "Stop"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(resp.get("hookSpecificOutput").is_none());
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
