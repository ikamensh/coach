//! End-to-end CLI tests: spawn the actual `coach` binary with a tempdir
//! standing in for `$HOME`, and assert on its stdout/stderr/exit-code
//! and on the files it leaves behind.
//!
//! Why spawn the binary instead of unit-testing `cli::dispatch_with_args`
//! directly? Because the *most likely* regression in this stack is the
//! dispatch wiring in `main.rs` — accidentally calling `coach_lib::run()`
//! before `dispatch()` would silently start Tauri on every CLI invocation.
//! These tests would catch that immediately by detecting the
//! `[coach] starting up` line in stderr.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Path to the freshly-built `coach` binary. Cargo sets this env var
/// for integration tests so we don't have to guess.
fn coach_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_coach"))
}

/// Run the binary with `args`, an isolated `$HOME`, and an empty `$PATH`
/// to keep tests deterministic. Returns (exit_code, stdout, stderr).
fn run_coach(args: &[&str], home: &Path) -> (i32, String, String) {
    let output = Command::new(coach_bin())
        .args(args)
        .env("HOME", home)
        // Force the dir-on-PATH probe to be deterministic.
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("failed to spawn coach");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Pre-seed `~/.coach/settings.json` with a port that's guaranteed not
/// to have a running Coach. Without this, the CLI's `server_running`
/// probe would discover the developer's actual Coach instance on 7700
/// and try to POST to `/api/...` against it — leaking test mutations
/// into the user's real settings file (and getting 404s from older
/// production builds that don't have those routes).
///
/// Port 1 (tcpmux) is reliably refused on every modern system, so the
/// probe fails immediately and the CLI falls back to file mode.
fn isolate_from_running_coach(home: &Path) {
    let dir = home.join(".coach");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("settings.json"),
        r#"{"port": 1}"#,
    )
    .unwrap();
}

// ── Property: CLI dispatch never starts Tauri ──────────────────────────

/// `coach version` should print and exit cleanly without ever touching
/// Tauri. The cheap signal: the `lib::run` setup hook prints
/// `[coach] starting up` to stderr — so its absence proves CLI dispatch
/// short-circuited before reaching `tauri::Builder`.
#[test]
fn version_subcommand_does_not_start_tauri() {
    let tmp = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = run_coach(&["version"], tmp.path());
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.starts_with("coach "), "got stdout: {stdout}");
    assert!(
        !stderr.contains("starting up"),
        "Tauri must NOT start for CLI subcommands. stderr: {stderr}"
    );
}

#[test]
fn help_subcommand_does_not_start_tauri() {
    let tmp = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = run_coach(&["help"], tmp.path());
    assert_eq!(code, 0);
    assert!(stdout.contains("USAGE"));
    assert!(!stderr.contains("starting up"));
}

#[test]
fn unknown_command_exits_two() {
    let tmp = tempfile::tempdir().unwrap();
    let (code, _stdout, stderr) = run_coach(&["nope"], tmp.path());
    assert_eq!(code, 2);
    assert!(stderr.contains("unknown command"));
}

// ── hooks install via CLI matches the file-mode helper byte-for-byte ───

/// The CLI's `coach hooks install` and the existing `install_hooks_at`
/// helper must produce the same `~/.claude/settings.json`. This is the
/// regression test that would catch accidental drift between the two
/// code paths — for example if the CLI started using a different default
/// port or hook event list than the Tauri command.
#[test]
fn cli_hooks_install_matches_install_hooks_at() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    // Run the CLI installer.
    let (code, _stdout, stderr) = run_coach(&["hooks", "install"], home);
    assert_eq!(code, 0, "stderr: {stderr}");

    let cli_path = home.join(".claude").join("settings.json");
    let cli_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&cli_path).unwrap()).unwrap();

    // Build the expected settings.json by calling the helper directly
    // against a separate temp file.
    let other = tempfile::tempdir().unwrap();
    let helper_path = other.path().join("settings.json");
    coach_lib::settings::install_hooks_at(7700, &helper_path).unwrap();
    let helper_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&helper_path).unwrap()).unwrap();

    assert_eq!(
        cli_json, helper_json,
        "CLI install must produce identical settings.json to install_hooks_at"
    );
}

#[test]
fn cli_hooks_cursor_install_matches_install_cursor_hooks_at() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    let (code, _stdout, stderr) = run_coach(&["hooks", "cursor", "install"], home);
    assert_eq!(code, 0, "stderr: {stderr}");

    let cli_hooks = home.join(".cursor").join("hooks.json");
    let cli_shim = home.join(".cursor").join("coach-cursor-hook.sh");
    let cli_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&cli_hooks).unwrap()).unwrap();

    let other = tempfile::tempdir().unwrap();
    let helper_hooks = other.path().join(".cursor").join("hooks.json");
    let helper_shim = other.path().join(".cursor").join("coach-cursor-hook.sh");
    coach_lib::settings::install_cursor_hooks_at(7700, &helper_hooks, &helper_shim).unwrap();
    let helper_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&helper_hooks).unwrap()).unwrap();

    // Each `command` entry embeds the absolute shim path, which differs
    // by tempdir. Normalize both to a placeholder before comparing so the
    // assertion still proves "CLI and helper produce structurally
    // identical hooks.json content".
    let normalize = |val: serde_json::Value, shim: &Path| {
        let raw = serde_json::to_string(&val).unwrap();
        let placeholder = "<SHIM>";
        let cleaned = raw.replace(&shim.display().to_string(), placeholder);
        serde_json::from_str::<serde_json::Value>(&cleaned).unwrap()
    };
    assert_eq!(
        normalize(cli_json, &cli_shim),
        normalize(helper_json, &helper_shim),
        "CLI cursor install must match install_cursor_hooks_at (modulo absolute shim path)"
    );

    // Both shims must be on disk and executable, since cursor's hook
    // runner spawns them directly.
    for (label, path) in [("cli", &cli_shim), ("helper", &helper_shim)] {
        let meta = std::fs::metadata(path)
            .unwrap_or_else(|e| panic!("{label} shim missing at {}: {e}", path.display()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert!(
                meta.permissions().mode() & 0o111 != 0,
                "{label} shim at {} must be executable",
                path.display()
            );
        }
        let body = std::fs::read_to_string(path).unwrap();
        assert!(
            body.starts_with("#!/bin/sh"),
            "{label} shim must start with shebang, got: {body}"
        );
        assert!(
            body.contains("curl"),
            "{label} shim must call curl internally"
        );
    }
}

#[test]
fn cli_hooks_install_then_uninstall_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    let (code, _, _) = run_coach(&["hooks", "install"], home);
    assert_eq!(code, 0);

    let (code, _, _) = run_coach(&["hooks", "uninstall"], home);
    assert_eq!(code, 0);

    let (_, stdout, _) = run_coach(&["hooks", "status"], home);
    assert!(stdout.contains("all installed: false"));
    // No coach hook lines should be installed.
    assert_eq!(stdout.matches("✓").count(), 0, "stdout: {stdout}");
}

// ── config get/set via CLI persists to settings.json ───────────────────

/// `config set priorities a,b,c` (in file mode, no server running) should
/// produce a settings.json whose `priorities` array equals what the CLI
/// passed in.
#[test]
fn cli_config_set_priorities_persists_to_file() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    isolate_from_running_coach(home);

    let (code, _stdout, stderr) = run_coach(
        &["config", "set", "priorities", "Speed,Safety,Simplicity"],
        home,
    );
    assert_eq!(code, 0, "stderr: {stderr}");

    let path = home.join(".coach").join("settings.json");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let priorities: Vec<&str> = json["priorities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(priorities, vec!["Speed", "Safety", "Simplicity"]);
}

/// Property: `config get` reads back what `config set` wrote.
#[test]
fn cli_config_get_reads_what_set_wrote() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    isolate_from_running_coach(home);

    run_coach(
        &["config", "set", "model", "anthropic", "claude-sonnet-4-6"],
        home,
    );
    let (code, stdout, _) = run_coach(&["config", "get", "model"], home);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("anthropic") && stdout.contains("claude-sonnet-4-6"),
        "stdout: {stdout}"
    );
}

/// Property: invariance under save/load — running `config set` twice in a
/// row leaves the same final state as running it once. Catches bugs where
/// load_from drops fields the CLI later tries to set.
#[test]
fn cli_config_set_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    isolate_from_running_coach(home);

    run_coach(&["config", "set", "priorities", "A,B"], home);
    let path = home.join(".coach").join("settings.json");
    let first = std::fs::read_to_string(&path).unwrap();

    run_coach(&["config", "set", "priorities", "A,B"], home);
    let second = std::fs::read_to_string(&path).unwrap();

    assert_eq!(first, second, "double-set must be idempotent");
}

/// Setting one config key must not clobber other keys. This is the
/// regression test for "load → mutate one field → save → other fields
/// gone" bugs.
#[test]
fn cli_config_set_preserves_unrelated_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    isolate_from_running_coach(home);

    run_coach(&["config", "set", "priorities", "X,Y"], home);
    run_coach(
        &["config", "set", "model", "openai", "gpt-test"],
        home,
    );

    let path = home.join(".coach").join("settings.json");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

    assert_eq!(json["priorities"][0], "X", "priorities lost after model set");
    assert_eq!(json["model"]["provider"], "openai");
    assert_eq!(json["model"]["model"], "gpt-test");
}

/// Setting a rule preserves other rules in the list — the CLI does
/// merge-not-replace for individual rules.
#[test]
fn cli_config_set_rule_merges_into_existing_list() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    isolate_from_running_coach(home);

    // Default settings has the outdated_models rule enabled.
    run_coach(&["config", "set", "rule", "custom_check", "on"], home);

    let path = home.join(".coach").join("settings.json");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let rules = json["rules"].as_array().unwrap();

    let ids: Vec<&str> = rules.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert!(
        ids.contains(&"outdated_models"),
        "merge must keep the existing default rule"
    );
    assert!(ids.contains(&"custom_check"), "new rule must be added");
}

// ── path install via CLI ───────────────────────────────────────────────

#[test]
fn cli_path_install_creates_shim_in_chosen_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let bin_dir = home.join("custom-bin");

    let (code, stdout, stderr) = run_coach(
        &["path", "install", "--dir", bin_dir.to_str().unwrap()],
        home,
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let shim = bin_dir.join("coach");
    assert!(shim.exists(), "shim missing; stdout: {stdout}");

    // Symlink should resolve to the test binary itself.
    #[cfg(unix)]
    {
        let target = std::fs::read_link(&shim).unwrap();
        // Read-link returns whatever was passed to symlink() — verify it
        // canonicalizes to our binary.
        let canon_target = std::fs::canonicalize(&target).unwrap();
        let canon_self = std::fs::canonicalize(coach_bin()).unwrap();
        assert_eq!(canon_target, canon_self);
    }
}

// ── sessions list via CLI ──────────────────────────────────────────────

/// Smoke test: `coach sessions list` runs to completion regardless of
/// whether ~/.claude/projects exists. Catches Rule A.2 violations
/// (silently swallowing errors when the dir is missing).
#[test]
fn cli_sessions_list_handles_missing_projects_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path(); // no ~/.claude/projects
    let (code, stdout, _) = run_coach(&["sessions", "list", "--limit", "5"], home);
    assert_eq!(code, 0);
    assert!(stdout.contains("0 saved session"));
}

#[test]
fn cli_replay_unknown_session_errors_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let (code, _stdout, stderr) = run_coach(&["replay", "no-such-session-xyz"], home);
    assert_eq!(code, 1);
    assert!(stderr.contains("Session not found") || stderr.contains("not found"));
}

// ── headless `coach serve` ─────────────────────────────────────────────

/// Pick a free TCP port by binding to 0 and reading what the kernel
/// assigned. The listener is dropped immediately, so there is a small
/// race window before the spawned daemon claims the same port. Fine for
/// tests on a developer machine; if it ever flakes, retry.
fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Wait until `127.0.0.1:port` accepts a TCP connection, or panic
/// after `timeout`. Polls every 50ms.
fn wait_for_port(port: u16, timeout: std::time::Duration) {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("port {port} did not open within {timeout:?}");
}

/// Kill the child and wait for it to exit. Used in test cleanup so we
/// don't leave headless coach daemons running between test runs.
fn kill_and_wait(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Block on a future from a sync test. Cheap throwaway runtime — same
/// pattern the CLI uses for `coach status`.
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

/// Fetch a URL and decode as JSON. Panics on any failure — tests want
/// the panic message, not graceful degradation.
fn http_get_json(url: &str) -> serde_json::Value {
    block_on(async move {
        reqwest::get(url)
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap()
    })
}

/// End-to-end smoke test for `coach serve`: spawn the binary as a
/// background process under a tempdir HOME, verify it binds the
/// requested port, hit the HTTP API, mutate state via the CLI (which
/// the running daemon should serve via HTTP, not the file fallback),
/// and confirm the change is visible through both `coach status` and
/// the on-disk settings file.
///
/// This is the test that was missing — and the absence of it is why
/// the single-instance plugin breakage went unnoticed until VM-style
/// integration testing tried to spawn a second daemon.
#[test]
fn cli_serve_starts_headless_daemon_and_round_trips_config() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    // Pre-seed settings.json so both the daemon and subsequent CLI
    // invocations agree on the port. The CLI's `server_running` probe
    // also reads `port` from this file, so a CLI `config set` will
    // route to HTTP not the file fallback.
    let port = pick_free_port();
    std::fs::create_dir_all(home.join(".coach")).unwrap();
    std::fs::write(
        home.join(".coach").join("settings.json"),
        format!(r#"{{"port":{port}}}"#),
    )
    .unwrap();

    // Spawn `coach serve` as a child process. We pass --port too so
    // the test does not depend on the settings.json round-trip working
    // (smaller failure surface if something is broken).
    let mut child = Command::new(coach_bin())
        .args(["serve", "--port", &port.to_string()])
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        // Capture daemon stderr so we can include it in panic messages
        // if something goes wrong; ignore stdout (none expected).
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("failed to spawn `coach serve`");

    // Make sure we always reap the child even on assertion failure.
    // (Wrapping in a guard struct would be cleaner, but tests panic
    // and we just need to avoid leaving daemons around.)
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wait_for_port(port, std::time::Duration::from_secs(10));

        // /version is a cheap liveness probe.
        let version = http_get_json(&format!("http://127.0.0.1:{port}/version"));
        assert!(
            version.get("version").is_some(),
            "version response missing 'version' field: {version}"
        );

        // /api/state should reflect the configured port and a sane
        // default snapshot (no real claude sessions in a tempdir HOME).
        let state = http_get_json(&format!("http://127.0.0.1:{port}/api/state"));
        assert_eq!(state["port"].as_u64().unwrap(), port as u64);
        assert!(
            state["sessions"].as_array().unwrap().is_empty(),
            "fresh tempdir HOME should have no sessions, got {state:#}"
        );

        // CLI `config set` against a running daemon should route via
        // HTTP. Verify by reading the snapshot back through HTTP — if
        // it had hit the file fallback, the running daemon's
        // in-memory state would not have moved.
        let (code, _stdout, stderr) = run_coach(
            &["config", "set", "priorities", "served-A,served-B"],
            home,
        );
        assert_eq!(code, 0, "config set failed; stderr: {stderr}");

        let state = http_get_json(&format!("http://127.0.0.1:{port}/api/state"));
        let priorities: Vec<String> = state["priorities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            priorities,
            vec!["served-A".to_string(), "served-B".to_string()],
            "HTTP-routed config set must update the daemon's in-memory state"
        );

        // The same write must also be persisted to disk so a daemon
        // restart picks it up. Read the file directly.
        let on_disk: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".coach").join("settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            on_disk["priorities"][0], "served-A",
            "config set via HTTP must also persist to settings.json"
        );
    }));

    kill_and_wait(&mut child);

    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

/// Property: `coach serve` exits cleanly when its process is killed
/// (no orphaned listener on the port). Catches a regression where the
/// HTTP server kept the port held after the parent CLI process died.
#[test]
fn cli_serve_releases_port_on_kill() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let port = pick_free_port();
    std::fs::create_dir_all(home.join(".coach")).unwrap();
    std::fs::write(
        home.join(".coach").join("settings.json"),
        format!(r#"{{"port":{port}}}"#),
    )
    .unwrap();

    let mut child = Command::new(coach_bin())
        .args(["serve", "--port", &port.to_string()])
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("failed to spawn `coach serve`");

    wait_for_port(port, std::time::Duration::from_secs(10));
    kill_and_wait(&mut child);

    // Give the kernel a moment to release the port. SIGKILL on the
    // child is immediate, but the close happens through the process
    // teardown path.
    std::thread::sleep(std::time::Duration::from_millis(200));

    let probe = std::net::TcpStream::connect(("127.0.0.1", port));
    assert!(
        probe.is_err(),
        "port {port} still listening after `coach serve` was killed"
    );
}
