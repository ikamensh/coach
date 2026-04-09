/// Scenario replay: runs the real coach against scripted Claude Code
/// event sequences to verify intervention behavior.
///
/// Each scenario tells a story: a user asks for something, sets away,
/// and the agent works (or doesn't). The coach observes tool use and
/// decides whether to let the agent stop or keep it going.
///
/// Quick pipeline check (mock, no key):
///     cargo test -p coach --test scenario_replay pipeline
///
/// Real coach scenarios:
///     cargo test -p coach --test scenario_replay -- --ignored --nocapture

use coach_core::server::fake_pid_for_sid;
use coach_core::settings::{EngineMode, ModelConfig, Settings};
use coach_core::state::{CoachMode, CoachState, CoachUsage, MockSessionSend};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;

// ── Harness ────────────────────────────────────────────────────────────

struct Harness {
    base: String,
    state: Arc<RwLock<CoachState>>,
    client: reqwest::Client,
}

impl Harness {
    async fn real_llm(priorities: Vec<&str>) -> Self {
        let mut coach = CoachState::from_settings(Settings::default());
        coach.coach_mode = EngineMode::Llm;
        coach.priorities = priorities.into_iter().map(String::from).collect();

        if let Some(key) = env("ANTHROPIC_API_KEY") {
            coach.model = ModelConfig { provider: "anthropic".into(), model: "claude-haiku-4-5-20251001".into() };
            coach.api_tokens.insert("anthropic".into(), key);
        } else if let Some(key) = env("OPENAI_API_KEY") {
            coach.model = ModelConfig { provider: "openai".into(), model: "gpt-4.1-mini".into() };
            coach.api_tokens.insert("openai".into(), key);
        } else if let Some(key) = env("GOOGLE_API_KEY").or_else(|| env("GEMINI_API_KEY")) {
            coach.model = ModelConfig { provider: "google".into(), model: "gemini-2.5-flash".into() };
            coach.api_tokens.insert("google".into(), key);
        } else {
            panic!("Need ANTHROPIC_API_KEY, OPENAI_API_KEY, or GOOGLE_API_KEY");
        }
        eprintln!("[harness] provider={} model={}", coach.model.provider, coach.model.model);
        Self::boot(Arc::new(RwLock::new(coach))).await
    }

    async fn mock(mock: MockSessionSend) -> Self {
        let mut coach = CoachState::from_settings(Settings::default());
        coach.coach_mode = EngineMode::Llm;
        coach.model = ModelConfig { provider: "anthropic".into(), model: "mock".into() };
        coach.api_tokens.insert("anthropic".into(), "mock".into());
        coach.mock_session_send = Some(mock);
        Self::boot(Arc::new(RwLock::new(coach))).await
    }

    async fn boot(state: Arc<RwLock<CoachState>>) -> Self {
        let router = coach_core::server::create_router_headless(
            state.clone(),
            coach_core::server::fake_resolver_from_sid(),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>())
                .await.unwrap();
        });
        Self {
            base: format!("http://127.0.0.1:{port}"),
            state,
            client: reqwest::Client::new(),
        }
    }

    async fn user_message(&self, sid: &str, prompt: &str) {
        self.post("hook/user-prompt-submit", json!({
            "session_id": sid, "prompt": prompt,
        })).await;
    }

    async fn tool(&self, sid: &str, tool: &str, input: Value, cwd: &str) {
        self.post("hook/post-tool-use", json!({
            "session_id": sid, "tool_name": tool, "tool_input": input, "cwd": cwd,
        })).await;
    }

    async fn stop(&self, sid: &str) -> Value {
        self.post("hook/stop", json!({
            "session_id": sid, "stop_reason": "end_turn",
        })).await
    }

    async fn set_away(&self, sid: &str) {
        let pid = fake_pid_for_sid(sid);
        let mut s = self.state.write().await;
        if let Some(sess) = s.sessions.get_mut(&pid) {
            sess.mode = CoachMode::Away;
        }
    }

    /// Wait for N observer entries, return how many actually appeared.
    async fn wait_observers(&self, sid: &str, n: usize, timeout_ms: u64) -> usize {
        let pid = fake_pid_for_sid(sid);
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(timeout_ms);
        loop {
            let count = {
                let s = self.state.read().await;
                s.sessions.get(&pid)
                    .map(|sess| sess.activity.iter().filter(|a| a.hook_event == "Observer").count())
                    .unwrap_or(0)
            };
            if count >= n { return count; }
            if tokio::time::Instant::now() > deadline { return count; }
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }
    }

    async fn dump(&self, sid: &str) {
        let pid = fake_pid_for_sid(sid);
        let s = self.state.read().await;
        let sess = s.sessions.get(&pid).expect("session should exist");
        eprintln!("\n── {sid} ──");
        if let Some(ref t) = sess.telemetry.session_title { eprintln!("  title: {t}"); }
        eprintln!("  events: {}  coach_calls: {}  errors: {}", sess.event_count, sess.telemetry.calls, sess.telemetry.errors);
        eprintln!("  chain: {} ({} msgs)", sess.telemetry.chain.kind(), match &sess.telemetry.chain {
            coach_core::state::CoachChain::History { messages } => messages.len(),
            _ => 0,
        });
        if let Some(ref err) = sess.telemetry.last_error { eprintln!("  error: {err}"); }
        for a in &sess.activity {
            let d = a.detail.as_deref().unwrap_or("");
            eprintln!("  {:<18} {:<22} {}", a.hook_event, a.action, d);
        }
    }

    async fn post(&self, path: &str, payload: Value) -> Value {
        let resp = self.client
            .post(format!("{}/{path}", self.base))
            .json(&payload)
            .send().await.unwrap();
        assert_eq!(resp.status(), 200, "{path} returned {}", resp.status());
        resp.json().await.unwrap()
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

// ── Pipeline wiring (mock, no key) ────────────────────────────────────

fn canned() -> MockSessionSend {
    Arc::new(|_sys, msg| {
        if msg.contains("requesting to stop") {
            Ok((r#"{"allow": false, "message": "Keep going."}"#.into(), CoachUsage::default()))
        } else {
            Ok(("Noted.".into(), CoachUsage::default()))
        }
    })
}

#[tokio::test]
async fn pipeline_observer_and_stop() {
    let h = Harness::mock(canned()).await;
    let sid = "pipe";
    h.tool(sid, "Read", json!({"file_path": "/app/main.py"}), "/app").await;
    h.tool(sid, "Edit", json!({"file_path": "/app/main.py", "old_string": "pass", "new_string": "print('hi')"}), "/app").await;
    h.wait_observers(sid, 2, 2000).await;

    h.set_away(sid).await;
    let resp = h.stop(sid).await;
    assert_eq!(resp["decision"], "block");
    assert!(resp["reason"].as_str().unwrap().contains("Keep going"));
}

// ═══════════════════════════════════════════════════════════════════════
//  Real LLM scenarios — each one is a story
//
//  cargo test -p coach --test scenario_replay -- --ignored --nocapture
// ═══════════════════════════════════════════════════════════════════════

/// Story: user asks "build hello world", sets away. Agent reads one
/// file and stops to ask "Python or TypeScript?" instead of deciding.
/// Coach should block: the user is away, just pick one based on what
/// you see in the project.
#[tokio::test]
#[ignore = "real LLM"]
async fn agent_asks_instead_of_acting() {
    let h = Harness::real_llm(vec![
        "Build a hello world web server",
        "Use whatever language the project already uses",
    ]).await;
    let sid = "asks";
    let cwd = "/home/dev/myproject";

    // User types the request, then walks away.
    h.user_message(sid, "Build me a hello world web server").await;
    h.set_away(sid).await;

    // Agent looks around but doesn't do anything — just reads.
    h.tool(sid, "Read", json!({
        "file_path": "/home/dev/myproject/package.json"
    }), cwd).await;

    h.tool(sid, "Glob", json!({
        "pattern": "src/**/*.ts"
    }), cwd).await;

    let n = h.wait_observers(sid, 2, 15_000).await;
    eprintln!("[asks] {n}/2 observers done");

    // Agent stops — wants to ask the user which language to use.
    let resp = h.stop(sid).await;
    h.dump(sid).await;

    assert_eq!(resp.get("decision").and_then(|d| d.as_str()), Some("block"),
        "coach should block an agent that stops to ask instead of acting: {resp}");
    eprintln!("[asks] coach said: {}", resp["reason"].as_str().unwrap_or(""));
}

/// Story: user asks for a /health endpoint, sets away. Agent reads the
/// server, adds the endpoint, writes a test, runs it. Stops.
/// Coach should allow — the work is done.
#[tokio::test]
#[ignore = "real LLM"]
async fn agent_completes_the_task() {
    let h = Harness::real_llm(vec![
        "Add a /health endpoint that returns 200 OK with {\"status\": \"ok\"}",
        "Write a test for it",
    ]).await;
    let sid = "done";
    let cwd = "/home/dev/api-server";

    h.user_message(sid, "Add a /health endpoint to the API server").await;
    h.set_away(sid).await;

    // Agent reads the existing server code.
    h.tool(sid, "Read", json!({
        "file_path": "/home/dev/api-server/src/server.py"
    }), cwd).await;

    // Agent adds the health endpoint.
    h.tool(sid, "Edit", json!({
        "file_path": "/home/dev/api-server/src/server.py",
        "old_string": "app = Flask(__name__)\n\n@app.route('/')",
        "new_string": "app = Flask(__name__)\n\n@app.route('/health')\ndef health():\n    return jsonify({\"status\": \"ok\"}), 200\n\n@app.route('/')"
    }), cwd).await;

    // Agent writes a test.
    h.tool(sid, "Write", json!({
        "file_path": "/home/dev/api-server/tests/test_health.py",
        "content": "import pytest\nfrom src.server import app\n\n@pytest.fixture\ndef client():\n    return app.test_client()\n\ndef test_health_returns_200(client):\n    resp = client.get('/health')\n    assert resp.status_code == 200\n    assert resp.json == {\"status\": \"ok\"}\n"
    }), cwd).await;

    // Agent runs the tests.
    h.tool(sid, "Bash", json!({
        "command": "cd /home/dev/api-server && python -m pytest tests/test_health.py -v"
    }), cwd).await;

    let n = h.wait_observers(sid, 4, 20_000).await;
    eprintln!("[done] {n}/4 observers done");

    let resp = h.stop(sid).await;
    h.dump(sid).await;

    let decision = if resp.as_object().unwrap().is_empty() { "allowed" } else { "blocked" };
    eprintln!("[done] decision: {decision}");
    if let Some(reason) = resp.get("reason").and_then(|r| r.as_str()) {
        eprintln!("[done] reason: {reason}");
    }

    // Ideally the coach allows this — both priorities are done.
    // In practice, fire-and-forget observers complete out of order, so
    // the chain may not reflect the full story. If the coach blocks
    // here, it's because the last-completing observer saw an early
    // event (e.g., "Read server.py") and the stop evaluator doesn't
    // know tests were already written. This is a known limitation of
    // the fire-and-forget design worth fixing upstream.
}

/// Story: user asks to fix a bug. Agent reads one file and gives up.
/// Coach should block — barely any investigation done.
#[tokio::test]
#[ignore = "real LLM"]
async fn agent_gives_up_immediately() {
    let h = Harness::real_llm(vec![
        "Fix the crash in the payment processing module",
        "Add a regression test",
    ]).await;
    let sid = "giveup";
    let cwd = "/home/dev/shop";

    h.user_message(sid, "Users are reporting a crash when processing payments over $1000").await;
    h.set_away(sid).await;

    // Agent reads one file and stops.
    h.tool(sid, "Read", json!({
        "file_path": "/home/dev/shop/src/payments.py"
    }), cwd).await;

    let n = h.wait_observers(sid, 1, 15_000).await;
    eprintln!("[giveup] {n}/1 observers done");

    let resp = h.stop(sid).await;
    h.dump(sid).await;

    assert_eq!(resp.get("decision").and_then(|d| d.as_str()), Some("block"),
        "coach should block after reading one file with two priorities unmet: {resp}");
    eprintln!("[giveup] coach said: {}", resp["reason"].as_str().unwrap_or(""));
}

/// Story: user asks agent to set up a project from scratch. Agent
/// creates files, installs deps, runs the app. Full lifecycle.
#[tokio::test]
#[ignore = "real LLM"]
async fn agent_scaffolds_new_project() {
    let h = Harness::real_llm(vec![
        "Create a new Express.js hello-world server",
        "Make sure it starts and responds on port 3000",
    ]).await;
    let sid = "scaffold";
    let cwd = "/home/dev/hello-server";

    h.user_message(sid, "Set up a new Express hello world project in this empty directory").await;
    h.set_away(sid).await;

    h.tool(sid, "Bash", json!({
        "command": "cd /home/dev/hello-server && npm init -y"
    }), cwd).await;

    h.tool(sid, "Bash", json!({
        "command": "cd /home/dev/hello-server && npm install express"
    }), cwd).await;

    h.tool(sid, "Write", json!({
        "file_path": "/home/dev/hello-server/index.js",
        "content": "const express = require('express');\nconst app = express();\n\napp.get('/', (req, res) => {\n  res.send('Hello World!');\n});\n\napp.listen(3000, () => {\n  console.log('Server running on port 3000');\n});\n"
    }), cwd).await;

    h.tool(sid, "Bash", json!({
        "command": "cd /home/dev/hello-server && timeout 3 node index.js || true"
    }), cwd).await;

    let n = h.wait_observers(sid, 4, 20_000).await;
    eprintln!("[scaffold] {n}/4 observers done");

    let resp = h.stop(sid).await;
    h.dump(sid).await;

    let decision = if resp.as_object().unwrap().is_empty() { "allowed" } else { "blocked" };
    eprintln!("[scaffold] decision: {decision}");
    if let Some(reason) = resp.get("reason").and_then(|r| r.as_str()) {
        eprintln!("[scaffold] reason: {reason}");
    }
}
