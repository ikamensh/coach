//! End-to-end smoke test for the pycoach Python sidecar.
//!
//! Spawns `pycoach serve` via `uv run --project ../../pycoach`, hits
//! `/healthz`, then drops the handle and asserts the child is gone.
//!
//! Compiled only when the `pycoach` feature is on. Within the feature,
//! the test still skips (with a clear log) when:
//!   * `uv` is not on PATH, or
//!   * the sibling `pycoach` checkout cannot be found.
//!
//! These conditions are normal on contributor machines without the Python
//! workspace, so they should be skips rather than failures.

#![cfg(feature = "pycoach")]

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use coach_lib::pycoach::Pycoach;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

fn pycoach_root() -> Option<PathBuf> {
    // src-tauri/tests/pycoach_sidecar.rs → src-tauri → coach → ilya/pycoach
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidate = manifest_dir.join("../../pycoach");
    candidate.canonicalize().ok().filter(|p| p.is_dir())
}

fn uv_available() -> bool {
    std::process::Command::new("uv")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn pycoach_via_uv() -> Option<Command> {
    if !uv_available() {
        eprintln!("skipping: `uv` not on PATH");
        return None;
    }
    let root = pycoach_root()?;
    let mut cmd = Command::new("uv");
    cmd.args(["run", "--project"])
        .arg(&root)
        .args(["pycoach", "serve"]);
    Some(cmd)
}

#[tokio::test]
async fn pycoach_sidecar_round_trip() {
    let Some(launcher) = pycoach_via_uv() else {
        eprintln!("skipping: pycoach checkout not found at ../../pycoach");
        return;
    };

    let py = Pycoach::launch(launcher)
        .await
        .expect("pycoach should launch");

    // Sanity: base_url is on loopback and points at the discovered port.
    assert!(
        py.base_url.starts_with("http://127.0.0.1:"),
        "unexpected base_url: {}",
        py.base_url
    );

    let body = py.healthz().await.expect("healthz should respond");
    assert_eq!(
        body.get("status").and_then(|v| v.as_str()),
        Some("ok"),
        "unexpected /healthz body: {body}"
    );
    assert_eq!(
        body.get("service").and_then(|v| v.as_str()),
        Some("pycoach"),
    );

    // Drop the handle: kill_on_drop should stop the child. We can't
    // observe the PID through the public API, but we can verify the
    // port stops accepting connections shortly after.
    let url = py.base_url.clone();
    drop(py);

    let client = reqwest::Client::new();
    let mut down = false;
    for _ in 0..30 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if client.get(format!("{url}/healthz")).send().await.is_err() {
            down = true;
            break;
        }
    }
    assert!(down, "pycoach sidecar still answering after drop");
}

/// Property test for the stdin-EOF shutdown trigger.
///
/// `Pycoach::launch` enables `kill_on_drop`, which would mask a broken
/// stdin watcher in the round-trip test above (the SIGKILL gets there
/// first). This test bypasses `Pycoach` entirely: spawn pycoach by hand
/// with piped stdin, drop *only* the stdin handle, and verify the child
/// exits on its own well before any reasonable wall-clock timeout.
///
/// This is the property that makes the design Windows-portable —
/// without it we'd be relying on PPID polling, which Windows doesn't
/// support reliably.
#[tokio::test]
async fn pycoach_exits_when_stdin_closes() {
    let Some(mut launcher) = pycoach_via_uv() else {
        eprintln!("skipping: pycoach checkout not found at ../../pycoach");
        return;
    };

    let mut child = launcher
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // No kill_on_drop here on purpose — we want to observe pycoach
        // exiting on its *own* in response to stdin EOF.
        .spawn()
        .expect("spawn pycoach");

    // Wait for the handshake line so we know pycoach is fully up. If it
    // never prints one (e.g. import error), the test fails fast rather
    // than blocking on the eventual exit.
    let stdout = child.stdout.take().expect("piped stdout");
    let mut lines = BufReader::new(stdout).lines();
    let handshake = tokio::time::timeout(Duration::from_secs(15), lines.next_line())
        .await
        .expect("handshake within 15s")
        .expect("read stdout")
        .expect("pycoach stdout closed before handshake");
    assert!(handshake.contains("\"ready\""), "bad handshake: {handshake}");

    // Close stdin — this is the entire signal under test.
    drop(child.stdin.take().expect("piped stdin"));

    // pycoach should notice within ~the time it takes to return from a
    // blocking read on a closed pipe, i.e. immediately. Give it a wide
    // budget to absorb scheduler jitter, but well below "we forgot to
    // wire it" territory.
    let exit = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("pycoach should exit on stdin EOF within 10s")
        .expect("wait succeeded");

    // We exit via os._exit(0), so the status should be a clean zero on
    // every platform. Anything else is a regression worth seeing.
    assert!(
        exit.success(),
        "pycoach exited with non-success status: {exit:?}"
    );
}
