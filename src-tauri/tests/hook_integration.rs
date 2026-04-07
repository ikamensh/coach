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
/// `coach_lib::server::fake_pid_for_sid` to look up state.
///
/// The `test_with_real_claude_code` test (ignored by default) launches the
/// actual `claude` CLI against a temporary project and checks that its
/// session appears in Coach state. Run it with:
///     cargo test -p coach -- --ignored
use coach_lib::server::fake_pid_for_sid;
use coach_lib::settings::Settings;
use coach_lib::state::CoachState;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Start the hook server on an OS-assigned port and return its base URL.
async fn start_test_server() -> (String, Arc<RwLock<CoachState>>) {
    let state = Arc::new(RwLock::new(CoachState::from_settings(Settings::default())));
    let router = coach_lib::server::create_router_headless(
        state.clone(),
        coach_lib::server::fake_resolver_from_sid(),
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
        let pid = fake_pid_for_sid("away-sess");
        if let Some(sess) = s.sessions.get_mut(&pid) {
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
        let pid = fake_pid_for_sid("stop-sess");
        if let Some(sess) = s.sessions.get_mut(&pid) {
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
    // SessionStart counts as one event in the new conversation.
    assert_eq!(sessions[0]["event_count"], 1);
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

    coach_lib::scanner::sync_sessions(&state, None).await;

    let coach = state.read().await;
    let live_files = coach_lib::scanner::scan_live_sessions();

    for file in &live_files {
        let sess = coach
            .sessions
            .get(&file.pid)
            .unwrap_or_else(|| panic!("PID {} from session file should be in state", file.pid));
        assert_eq!(sess.pid, file.pid);
        assert_eq!(sess.event_count, 0, "discovered sessions start with 0 events");
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
        s.default_mode = coach_lib::state::CoachMode::Away;
        let pid = fake_pid_for_sid("schema-present");
        if let Some(sess) = s.sessions.get_mut(&pid) {
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
    s.coach_mode = coach_lib::settings::EngineMode::Llm;
    s.model = coach_lib::settings::ModelConfig {
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
            sess.mode = coach_lib::state::CoachMode::Away;
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
                return;
            }
        }
    }
    panic!("observer task never recorded an Observer entry within 500ms");
}

#[tokio::test]
async fn observer_does_not_fire_in_rules_mode() {
    let (base, state) = start_test_server().await;
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

    {
        let mut s = state.write().await;
        s.coach_mode = coach_lib::settings::EngineMode::Llm;
        s.model = coach_lib::settings::ModelConfig {
            provider: "google".into(),
            model: "gemini-2.5-flash".into(),
        };
        s.api_tokens.clear();
        s.env_tokens.clear();
    }

    client
        .post(format!("{base}/hook/post-tool-use"))
        .json(&serde_json::json!({
            "session_id": "google-llm",
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "tool_input": {"file_path": "/x.py", "new_string": "ok"}
        }))
        .send()
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let s = state.read().await;
    let pid = fake_pid_for_sid("google-llm");
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

/// UserPromptSubmit must create the session if it doesn't exist, record a
/// "user spoke" entry in the session's activity log with the (truncated)
/// prompt text, and pass through with no decision payload.
#[tokio::test]
async fn user_prompt_submit_records_activity() {
    let (base, _state) = start_test_server().await;
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
}

#[tokio::test]
async fn user_prompt_submit_truncates_long_prompts() {
    let (base, _state) = start_test_server().await;
    let client = reqwest::Client::new();

    let long_prompt = "x".repeat(1000);
    client
        .post(format!("{base}/hook/user-prompt-submit"))
        .json(&serde_json::json!({
            "session_id": "longwinded",
            "hook_event_name": "UserPromptSubmit",
            "prompt": long_prompt,
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
}
