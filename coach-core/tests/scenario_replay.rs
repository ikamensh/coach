/// Pipeline smoke test: runs the real observer + stop pipeline against
/// a mocked LLM provider so we can verify the wiring (observer queue,
/// chain building, stop_chained fallback, response shape) without
/// spending API calls.
///
/// The actual "did the coach make the right call?" scenarios live in
/// `benchmark/` and run via `cargo test -p coach-core --test
/// benchmark_suite`. Those are the ones to edit when you want to
/// pin down a coaching behavior.
///
///     cargo test -p coach --test scenario_replay pipeline
use coach_core::settings::{EngineMode, ModelConfig, Settings};
use coach_core::state::{AppState, CoachMode, CoachUsage, MockSessionSend};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;

// ── Harness ────────────────────────────────────────────────────────────

struct Harness {
    base: String,
    state: Arc<RwLock<AppState>>,
    client: reqwest::Client,
}

impl Harness {
    async fn mock(mock: MockSessionSend) -> Self {
        let mut coach = AppState::from_settings(Settings::default());
        coach.config.coach_mode = EngineMode::Llm;
        coach.config.model = ModelConfig {
            provider: "anthropic".into(),
            model: "mock".into(),
        };
        coach.config.api_tokens.insert("anthropic".into(), "mock".into());
        coach.services.mock_session_send = Some(mock);
        Self::boot(Arc::new(RwLock::new(coach))).await
    }

    async fn boot(state: Arc<RwLock<AppState>>) -> Self {
        let router = coach_core::server::create_router_headless(state.clone());
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
        Self {
            base: format!("http://127.0.0.1:{port}"),
            state,
            client: reqwest::Client::new(),
        }
    }

    async fn tool(&self, sid: &str, tool: &str, input: Value, cwd: &str) {
        self.post(
            "hook/post-tool-use",
            json!({
                "session_id": sid,
                "tool_name": tool,
                "tool_input": input,
                "cwd": cwd,
            }),
        )
        .await;
    }

    async fn stop(&self, sid: &str) -> Value {
        self.post(
            "hook/stop",
            json!({ "session_id": sid, "stop_reason": "end_turn" }),
        )
        .await
    }

    async fn set_away(&self, sid: &str) {
        let mut s = self.state.write().await;
        if let Some(sess) = s.sessions.get_mut(sid) {
            sess.mode = CoachMode::Away;
        }
    }

    /// Wait for N observer entries, return how many actually appeared.
    async fn wait_observers(&self, sid: &str, n: usize, timeout_ms: u64) -> usize {
        let deadline = tokio::time::Instant::now()
            + tokio::time::Duration::from_millis(timeout_ms);
        loop {
            let count = {
                let s = self.state.read().await;
                s.sessions
                    .get(sid)
                    .map(|sess| {
                        sess.activity
                            .iter()
                            .filter(|a| a.hook_event == "Observer")
                            .count()
                    })
                    .unwrap_or(0)
            };
            if count >= n {
                return count;
            }
            if tokio::time::Instant::now() > deadline {
                return count;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }
    }

    async fn post(&self, path: &str, payload: Value) -> Value {
        let resp = self
            .client
            .post(format!("{}/{path}", self.base))
            .json(&payload)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "{path} returned {}", resp.status());
        resp.json().await.unwrap()
    }
}

// ── Pipeline wiring (mock, no key) ────────────────────────────────────

fn canned() -> MockSessionSend {
    Arc::new(|_sys, msg| {
        if msg.contains("requesting to stop") {
            Ok((
                r#"{"allow": false, "message": "Keep going."}"#.into(),
                CoachUsage::default(),
            ))
        } else {
            Ok(("Noted.".into(), CoachUsage::default()))
        }
    })
}

#[tokio::test]
async fn pipeline_observer_and_stop() {
    let h = Harness::mock(canned()).await;
    let sid = "pipe";
    h.tool(sid, "Read", json!({ "file_path": "/app/main.py" }), "/app")
        .await;
    h.tool(
        sid,
        "Edit",
        json!({
            "file_path": "/app/main.py",
            "old_string": "pass",
            "new_string": "print('hi')"
        }),
        "/app",
    )
    .await;
    h.wait_observers(sid, 2, 2000).await;

    h.set_away(sid).await;
    let resp = h.stop(sid).await;
    assert_eq!(resp["decision"], "block");
    assert!(resp["reason"].as_str().unwrap().contains("Keep going"));
}
