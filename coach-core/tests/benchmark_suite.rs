//! Benchmark suite runner.
//!
//! Each scenario lives in `benchmark/<name>.{md,json}`. The `.md` is
//! human documentation; the `.json` is the input event stream plus
//! declarative `expect` assertions. See `benchmark/README.md` for the
//! schema.
//!
//! One `#[tokio::test]` per scenario, declared via the `scenario!` /
//! `llm_scenario!` macros at the bottom of the file. That gives us:
//!
//! - IDE test navigation per scenario
//! - `cargo test away_stop` to run one scenario (prefix match)
//! - clean test-result lines in CI output
//! - LLM scenarios isolated under `#[ignore]` so a default `cargo
//!   test` run stays deterministic and costs no API calls
//!
//! The price is one extra macro line per scenario when adding a new
//! file. `benchmark/README.md` has the "add a scenario" checklist.
//!
//! Scenarios run against fresh `AppState`s — nothing leaks between
//! runs. LLM scenarios wait for the observer queue to drain between
//! `PostToolUse` events so the chain is fully advanced before `Stop`
//! reads it.

use coach_core::server::create_router_headless;
use coach_core::settings::{EngineMode, ModelConfig, Settings};
use coach_core::state::{AppState, CoachMode, SharedState};
use serde::Deserialize;
use serde_json::Value;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

// ── Scenario schema ─────────────────────────────────────────────────

/// One scenario file, exactly the shape documented in
/// `benchmark/README.md`. `source` and `description` are captured so
/// they round-trip through `serde` — a typo surfaces as a
/// deserialization error at load time instead of a confusing runtime
/// miss.
#[derive(Debug, Deserialize)]
struct Scenario {
    name: String,
    #[allow(dead_code)]
    source: Option<String>,
    #[allow(dead_code)]
    description: Option<String>,
    mode: String,
    priorities: Option<Vec<String>>,
    session_id: String,
    events: Vec<ScenarioEvent>,
}

#[derive(Debug, Deserialize)]
struct ScenarioEvent {
    hook: String,
    body: Value,
    #[serde(default)]
    expect: Option<Expect>,
}

/// Declarative assertions against one hook response. Every field is
/// optional — a missing field means "don't check this key". Keep this
/// list aligned with the table in `benchmark/README.md`.
#[derive(Debug, Deserialize, Default)]
struct Expect {
    decision: Option<String>,
    reason_contains: Option<String>,
    context_contains: Option<String>,
    permission: Option<String>,
    passthrough: Option<bool>,
}

// ── Loading ─────────────────────────────────────────────────────────

fn benchmark_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("benchmark")
}

fn load_scenario(name: &str) -> Scenario {
    let path = benchmark_dir().join(format!("{name}.json"));
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    serde_json::from_str(&content).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

// ── State builder ───────────────────────────────────────────────────

/// Configure an `AppState` for this scenario's mode + priorities.
/// Rules scenarios (`present` / `away`) don't touch API tokens; LLM
/// scenarios pull a key out of the environment and install a
/// capable provider+model. Called from the sync test path so it
/// panics with a clear message if an LLM scenario runs without a
/// key.
fn build_state(scenario: &Scenario) -> AppState {
    let mut app = AppState::from_settings(Settings::default());

    match scenario.mode.as_str() {
        "llm" => {
            app.config.coach_mode = EngineMode::Llm;
            app.sessions.default_mode = CoachMode::Away;
            install_env_llm_key(&mut app);
        }
        "away" => {
            app.config.coach_mode = EngineMode::Rules;
            app.sessions.default_mode = CoachMode::Away;
        }
        "present" => {
            app.config.coach_mode = EngineMode::Rules;
            app.sessions.default_mode = CoachMode::Present;
        }
        other => panic!(
            "scenario {:?}: unknown mode {other:?} (expected present/away/llm)",
            scenario.name
        ),
    }

    if let Some(ref prios) = scenario.priorities {
        app.config.priorities = prios.clone();
    }

    app
}

/// Configure the LLM provider and model on `app`.
///
/// If `COACH_BENCHMARK_MODEL` is set (format: `provider/model`, e.g.
/// `openai/gpt-5.4-nano`), use that exact model. Otherwise fall back
/// to auto-detection from API key env vars.
///
/// Either way, the matching API key must be set in the environment.
fn install_env_llm_key(app: &mut AppState) {
    fn env(name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }

    // Explicit model override — used by the multi-model eval script.
    if let Some(spec) = env("COACH_BENCHMARK_MODEL") {
        let (provider, model) = spec
            .split_once('/')
            .unwrap_or_else(|| panic!(
                "COACH_BENCHMARK_MODEL must be provider/model, got {spec:?}"
            ));
        let key_var = match provider {
            "openai" => "OPENAI_API_KEY",
            "anthropic" => "ANTHROPIC_API_KEY",
            "google" => "GOOGLE_API_KEY",
            other => panic!("unknown provider {other:?} in COACH_BENCHMARK_MODEL"),
        };
        let key = env(key_var).unwrap_or_else(|| {
            panic!("COACH_BENCHMARK_MODEL={spec} but {key_var} is not set")
        });
        app.config.model = ModelConfig {
            provider: provider.into(),
            model: model.into(),
        };
        app.config.api_tokens.insert(provider.into(), key);
        eprintln!("[benchmark] model override: {provider}/{model}");
        return;
    }

    if let Some(key) = env("ANTHROPIC_API_KEY") {
        app.config.model = ModelConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5-20251001".into(),
        };
        app.config.api_tokens.insert("anthropic".into(), key);
    } else if let Some(key) = env("OPENAI_API_KEY") {
        app.config.model = ModelConfig {
            provider: "openai".into(),
            model: "gpt-4.1-mini".into(),
        };
        app.config.api_tokens.insert("openai".into(), key);
    } else if let Some(key) = env("GOOGLE_API_KEY").or_else(|| env("GEMINI_API_KEY")) {
        app.config.model = ModelConfig {
            provider: "google".into(),
            model: "gemini-2.5-flash".into(),
        };
        app.config.api_tokens.insert("google".into(), key);
    } else {
        panic!(
            "LLM benchmark scenario needs ANTHROPIC_API_KEY, OPENAI_API_KEY, \
             or GOOGLE_API_KEY in the environment"
        );
    }
    eprintln!(
        "[benchmark] using provider={} model={}",
        app.config.model.provider, app.config.model.model
    );
}

// ── Headless server ─────────────────────────────────────────────────

async fn boot_server(scenario: &Scenario) -> (String, SharedState) {
    let state: SharedState = Arc::new(RwLock::new(build_state(scenario)));
    let router = create_router_headless(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("server");
    });
    (format!("http://127.0.0.1:{port}"), state)
}

async fn post_hook(
    client: &reqwest::Client,
    base: &str,
    hook: &str,
    session_id: &str,
    body: &Value,
) -> Result<Value, String> {
    let mut merged = body.clone();
    if let Some(obj) = merged.as_object_mut() {
        obj.insert(
            "session_id".to_string(),
            Value::String(session_id.to_string()),
        );
    }
    let url = format!("{base}/hook/{hook}");
    let resp = client
        .post(&url)
        .json(&merged)
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("POST {url} returned {status}"));
    }
    resp.json()
        .await
        .map_err(|e| format!("parse response from {url}: {e}"))
}

// ── Observer wait (LLM mode) ────────────────────────────────────────

/// Combined success+error counter for this session's observer. One
/// bump per completed observer call, same signal `replay.rs` polls.
async fn observer_progress(state: &SharedState, sid: &str) -> usize {
    state
        .read()
        .await
        .sessions
        .get(sid)
        .map(|s| s.coach.telemetry.calls + s.coach.telemetry.errors)
        .unwrap_or(0)
}

/// Block until the session's observer counter advances past
/// `baseline`, or `timeout` elapses (treated as success with a
/// warning — the runner still moves on, and the downstream Stop
/// will fail its assertion if the chain is stale, which gives a
/// clearer error than silently hanging).
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
            eprintln!(
                "[benchmark] observer wait timed out after {timeout:?} for sid {sid} \
                 — chain may be stale"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ── Assertion engine ────────────────────────────────────────────────

/// Check every non-None field of `expect` against `response`. Collects
/// all mismatches into one string so a scenario with three unmet
/// assertions shows all three in the failure report instead of
/// one-at-a-time.
fn check_expect(expect: &Expect, response: &Value) -> Result<(), String> {
    let mut errors: Vec<String> = Vec::new();

    if let Some(passthrough) = expect.passthrough {
        let is_empty = response
            .as_object()
            .map(|o| o.is_empty())
            .unwrap_or(false);
        if passthrough && !is_empty {
            errors.push(format!(
                "expected passthrough (body `{{}}`), got {response}"
            ));
        }
        if !passthrough && is_empty {
            errors.push("expected a non-passthrough response, got `{}`".to_string());
        }
    }

    if let Some(ref want) = expect.decision {
        let got = response.get("decision").and_then(|v| v.as_str());
        if got != Some(want.as_str()) {
            errors.push(format!(
                "expected decision={want:?}, got decision={got:?} in {response}"
            ));
        }
    }

    if let Some(ref needle) = expect.reason_contains {
        let got = response
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !got.contains(needle.as_str()) {
            errors.push(format!(
                "expected reason_contains={needle:?}, got reason={got:?}"
            ));
        }
    }

    if let Some(ref needle) = expect.context_contains {
        let got = response
            .pointer("/hookSpecificOutput/additionalContext")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !got.contains(needle.as_str()) {
            errors.push(format!(
                "expected context_contains={needle:?}, got additionalContext={got:?}"
            ));
        }
    }

    if let Some(ref want) = expect.permission {
        let got = response
            .pointer("/hookSpecificOutput/decision/behavior")
            .and_then(|v| v.as_str());
        if got != Some(want.as_str()) {
            errors.push(format!(
                "expected permission={want:?}, \
                 got hookSpecificOutput.decision.behavior={got:?}"
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n      "))
    }
}

// ── Runner ──────────────────────────────────────────────────────────

/// Load one scenario by name, boot a fresh server for it, dispatch
/// its events, and panic on any unmet assertion with a detailed
/// report. One scenario per test. Used by both `scenario!` and
/// `llm_scenario!` macros — the only difference between rules and
/// llm scenarios is what `build_state` configures and whether the
/// runner waits for the observer between tool events.
async fn run_one(name: &str) {
    let scenario = load_scenario(name);
    assert_eq!(
        scenario.name, name,
        "scenario file name {name:?} doesn't match `name` field inside: {:?}",
        scenario.name
    );
    eprintln!("[benchmark] running {} (mode={})", scenario.name, scenario.mode);

    let (base, state) = boot_server(&scenario).await;
    let client = reqwest::Client::new();
    let is_llm = scenario.mode == "llm";
    let observer_timeout = Duration::from_secs(60);

    let mut failures: Vec<String> = Vec::new();

    for (idx, ev) in scenario.events.iter().enumerate() {
        let baseline = if is_llm && ev.hook == "post-tool-use" {
            observer_progress(&state, &scenario.session_id).await
        } else {
            0
        };

        let response = match post_hook(
            &client,
            &base,
            &ev.hook,
            &scenario.session_id,
            &ev.body,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => {
                failures.push(format!(
                    "event[{idx}] ({} {}): transport error: {e}",
                    ev.hook,
                    summarize_body(&ev.body)
                ));
                continue;
            }
        };

        if is_llm && ev.hook == "post-tool-use" {
            wait_for_observer_tick(
                &state,
                &scenario.session_id,
                baseline,
                observer_timeout,
            )
            .await;
        }

        if let Some(expect) = &ev.expect {
            if let Err(err) = check_expect(expect, &response) {
                failures.push(format!(
                    "event[{idx}] ({} {}):\n      {err}",
                    ev.hook,
                    summarize_body(&ev.body)
                ));
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "scenario {} failed:\n\n{}\n",
            scenario.name,
            failures.join("\n\n")
        );
    }
}

/// One-line body preview for failure messages. Walks back to a
/// UTF-8 char boundary before truncating so a multi-byte codepoint
/// straddling byte 80 doesn't panic the runner.
fn summarize_body(body: &Value) -> String {
    let compact = serde_json::to_string(body).unwrap_or_default();
    const MAX: usize = 80;
    if compact.len() <= MAX {
        return compact;
    }
    let mut end = MAX;
    while !compact.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &compact[..end])
}

// ── Test registry ───────────────────────────────────────────────────
//
// One macro per class of scenario. The scenario name here *must*
// match the filename stem under `benchmark/` and the `name` field
// inside the JSON (`run_one` asserts this). Adding a new scenario =
// new .md/.json pair + one line below.

macro_rules! scenario {
    ($fn_name:ident, $name:literal) => {
        #[tokio::test]
        async fn $fn_name() {
            run_one($name).await;
        }
    };
}

macro_rules! llm_scenario {
    ($fn_name:ident, $name:literal) => {
        #[tokio::test]
        #[ignore = "live LLM call — run with --ignored and an API key \
                    (ANTHROPIC_API_KEY, OPENAI_API_KEY, or GOOGLE_API_KEY)"]
        async fn $fn_name() {
            run_one($name).await;
        }
    };
}

// Rules-mode scenarios — always run.
scenario!(
    away_permission_auto_approved,
    "away_permission_auto_approved"
);
scenario!(
    away_stop_blocks_with_priorities,
    "away_stop_blocks_with_priorities"
);
scenario!(
    away_stop_cooldown_passes_second,
    "away_stop_cooldown_passes_second"
);

// LLM-mode scenarios — ignored unless opted in via `-- --ignored`.
llm_scenario!(
    llm_agent_asks_instead_of_acting,
    "llm_agent_asks_instead_of_acting"
);
llm_scenario!(llm_agent_completes_task, "llm_agent_completes_task");
llm_scenario!(llm_agent_gives_up_early, "llm_agent_gives_up_early");
llm_scenario!(llm_agent_scaffolds_project, "llm_agent_scaffolds_project");
