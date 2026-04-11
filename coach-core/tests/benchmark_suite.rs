//! Benchmark suite runner.
//!
//! Walks `benchmark/*.json` relative to the workspace root, boots a
//! headless hook server per scenario, POSTs each event through the
//! real HTTP router, and checks the response against declarative
//! `expect` keys defined in the scenario. Adding a new scenario means
//! dropping a pair of `.md` + `.json` files into `benchmark/` — no
//! code changes here.
//!
//! See `benchmark/README.md` for the file format and the list of
//! supported `expect` keys.
//!
//! Each scenario runs against a fresh `AppState`; nothing leaks
//! between them. Failures collect into a single report so one run
//! tells you everything that's wrong at once instead of fail-fast
//! hiding later problems.

use coach_core::server::create_router_headless;
use coach_core::settings::{EngineMode, Settings};
use coach_core::state::{AppState, CoachMode, SharedState};
use serde::Deserialize;
use serde_json::Value;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

// ── Scenario schema ─────────────────────────────────────────────────

/// One scenario file, exactly the shape documented in
/// `benchmark/README.md`. `source` and `description` are captured so
/// they round-trip through `serde` — that way a typo in a scenario
/// file surfaces as a deserialization error at load time instead of a
/// confusing runtime miss.
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
    /// Top-level `decision` string. The only meaningful value today is
    /// `"block"` (Stop blocked) but the schema doesn't hardcode that —
    /// if a future hook returns `decision: "allow"` somewhere at the
    /// top level we can express that too.
    decision: Option<String>,
    /// Substring match on the top-level `reason` field. Paired with
    /// `decision: "block"` for Stop scenarios.
    reason_contains: Option<String>,
    /// Substring match on `hookSpecificOutput.additionalContext`.
    /// That's where rule messages and `[Coach]: …` interventions land
    /// on PostToolUse.
    context_contains: Option<String>,
    /// `hookSpecificOutput.decision.behavior` — the permission
    /// auto-approve shape. Only expected value is `"allow"`.
    permission: Option<String>,
    /// `true` asserts the response body is exactly `{}`. `false`
    /// asserts it isn't. Leave unset if you don't care.
    passthrough: Option<bool>,
}

// ── Loading ─────────────────────────────────────────────────────────

fn benchmark_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("benchmark")
}

fn load_scenarios() -> Vec<(PathBuf, Scenario)> {
    let dir = benchmark_dir();
    let mut out: Vec<(PathBuf, Scenario)> = Vec::new();
    for entry in std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("benchmark dir {dir:?} not found: {e}"))
    {
        let path = entry.expect("read benchmark entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {path:?}: {e}"));
        let scenario: Scenario = serde_json::from_str(&content)
            .unwrap_or_else(|e| panic!("parse {path:?}: {e}"));
        out.push((path, scenario));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

// ── Per-scenario headless server ────────────────────────────────────

/// Build an `AppState` pre-configured for this scenario's mode and
/// priorities. Setting `sessions.default_mode` *before* any hook fires
/// means the first `apply_hook_event` call creates the session
/// already in Away/Present as the scenario needs — no need for a
/// separate mode-flip event.
fn build_state(scenario: &Scenario) -> AppState {
    let mut app = AppState::from_settings(Settings::default());
    match scenario.mode.as_str() {
        "llm" => {
            app.config.coach_mode = EngineMode::Llm;
            app.sessions.default_mode = CoachMode::Away;
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
    // Inject session_id into every body so scenarios don't have to
    // repeat it per event. Claude Code puts it at the top level of
    // every hook payload, so that's where we merge.
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

// ── Assertion engine ────────────────────────────────────────────────

/// Check every non-None field of `expect` against `response`. Collects
/// *all* mismatches into one string so a scenario that has three
/// unmet assertions shows all three in the failure report instead of
/// one-at-a-time stop-on-first.
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
                "expected permission={want:?}, got hookSpecificOutput.decision.behavior={got:?}"
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

async fn run_scenario(
    client: &reqwest::Client,
    path: &Path,
    scenario: &Scenario,
) -> Vec<String> {
    let (base, _state) = boot_server(scenario).await;
    let mut failures: Vec<String> = Vec::new();

    for (idx, ev) in scenario.events.iter().enumerate() {
        let response = match post_hook(client, &base, &ev.hook, &scenario.session_id, &ev.body)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                failures.push(format!(
                    "{}\n  scenario: {}\n  event[{idx}] ({} {}): transport error: {e}",
                    path.display(),
                    scenario.name,
                    ev.hook,
                    summarize_body(&ev.body)
                ));
                continue;
            }
        };

        if let Some(expect) = &ev.expect {
            if let Err(err) = check_expect(expect, &response) {
                failures.push(format!(
                    "{}\n  scenario: {}\n  event[{idx}] ({} {}):\n      {err}",
                    path.display(),
                    scenario.name,
                    ev.hook,
                    summarize_body(&ev.body)
                ));
            }
        }
    }

    failures
}

/// One-line body preview for failure messages. Keeps the error
/// report compact while still identifying which event failed when a
/// scenario has multiple of the same hook type (e.g. two Stops in
/// the cooldown scenario). Walks back to a UTF-8 char boundary
/// before truncating so a multi-byte codepoint that straddles byte
/// 80 doesn't panic the runner.
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

// ── Test entry point ────────────────────────────────────────────────

/// Load every benchmark scenario, run each against a fresh headless
/// server, report all failures at once. The test fails iff any
/// `expect` key anywhere doesn't match — a clean run means all
/// documented canonical interventions still fire as intended.
#[tokio::test]
async fn benchmark_scenarios_match_expected_interventions() {
    let scenarios = load_scenarios();
    assert!(
        !scenarios.is_empty(),
        "no benchmark scenarios found in {:?}",
        benchmark_dir()
    );

    let client = reqwest::Client::new();
    let mut all_failures: Vec<String> = Vec::new();
    let mut passed = 0usize;

    for (path, scenario) in &scenarios {
        eprintln!("[benchmark] running {}", scenario.name);
        let failures = run_scenario(&client, path, scenario).await;
        if failures.is_empty() {
            passed += 1;
        } else {
            all_failures.extend(failures);
        }
    }

    eprintln!(
        "[benchmark] {passed}/{} scenarios passed",
        scenarios.len()
    );

    assert!(
        all_failures.is_empty(),
        "\n\n{} benchmark assertion(s) failed:\n\n{}\n",
        all_failures.len(),
        all_failures.join("\n\n")
    );
}
