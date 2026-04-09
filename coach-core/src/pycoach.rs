//! Pycoach sidecar manager.
//!
//! Spawns the `pycoach serve` Python sidecar as a child process and exposes
//! a [`reqwest`] client pointed at the port the child announces on stdout.
//! See `../../pycoach/src/pycoach/server.py` for the other side of the
//! handshake.
//!
//! This module deliberately does NOT define an LLM trait, structured request
//! types, or anything resembling a stable contract. Pycoach's shape is still
//! moving — the only commitments here are "we can start it, talk HTTP to it,
//! and shut it down." Endpoints get added once their shape settles.
//!
//! Lifecycle: [`Pycoach`] owns a `tokio::process::Child` with `kill_on_drop`,
//! so dropping the handle stops the child on the clean-shutdown path. The
//! Python side runs a parent-PID watcher as a fallback for the SIGKILL case.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// Where Tauri drops `externalBin` entries at runtime: next to the main
/// executable, with the host triple suffix stripped. We probe both the
/// platform-bare name and the `.exe` form so this works on all targets.
fn bundled_sidecar_path() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    for name in ["pycoach", "pycoach.exe"] {
        let candidate = exe_dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Handle to a running pycoach sidecar.
///
/// `_child` is held only for its `kill_on_drop` side effect — nothing reads
/// from it after the handshake.
pub struct Pycoach {
    pub base_url: String,
    pub http: reqwest::Client,
    _child: Child,
}

#[derive(Debug, Deserialize)]
struct Handshake {
    port: u16,
}

/// How long we wait for the child to print its handshake line.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// How long we keep retrying `/healthz` after the handshake before giving up.
const HEALTHCHECK_TIMEOUT: Duration = Duration::from_secs(5);

impl Pycoach {
    /// Spawn pycoach using the supplied [`Command`].
    ///
    /// The command must invoke `pycoach serve` (or an equivalent that
    /// follows the same stdout handshake protocol). Stdout is consumed
    /// for port discovery; remaining lines are forwarded to Coach's
    /// stderr with a `[pycoach]` prefix.
    ///
    /// Stdin is piped but never written to. The child uses EOF on its
    /// stdin as a cross-platform "parent has died, shut down" signal —
    /// when this Child handle drops the pipe closes, the read returns
    /// empty, and the Python side exits. This catches the case where
    /// `kill_on_drop` doesn't run (e.g. Coach itself was SIGKILL'd) on
    /// platforms where PPID polling isn't reliable (Windows).
    pub async fn launch(mut command: Command) -> Result<Self, String> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);

        let mut child = command
            .spawn()
            .map_err(|e| format!("failed to spawn pycoach: {e}"))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "pycoach child stdout was not captured".to_string())?;
        let mut lines = BufReader::new(stdout).lines();

        let handshake_line = tokio::time::timeout(HANDSHAKE_TIMEOUT, lines.next_line())
            .await
            .map_err(|_| "pycoach handshake timed out".to_string())?
            .map_err(|e| format!("pycoach stdout read failed: {e}"))?
            .ok_or_else(|| "pycoach exited before printing handshake".to_string())?;

        let handshake: Handshake = serde_json::from_str(&handshake_line).map_err(|e| {
            format!("pycoach handshake was not JSON ({e}): {handshake_line:?}")
        })?;

        // Drain remaining stdout in the background so the child never blocks
        // on a full pipe. Each line is mirrored into Coach's log.
        tokio::spawn(async move {
            let mut lines = lines;
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => eprintln!("[pycoach] {line}"),
                    Ok(None) => break,
                    Err(e) => {
                        eprintln!("[pycoach] stdout read error: {e}");
                        break;
                    }
                }
            }
        });

        let base_url = format!("http://127.0.0.1:{}", handshake.port);
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| format!("failed to build pycoach http client: {e}"))?;

        // Healthcheck loop: uvicorn may need a few ms after binding before
        // it accepts the first connection. The handshake is printed before
        // serve() so we can race the actual server coming up.
        let deadline = tokio::time::Instant::now() + HEALTHCHECK_TIMEOUT;
        let healthz_url = format!("{base_url}/healthz");
        let mut last_err: String = "no attempt".into();
        while tokio::time::Instant::now() < deadline {
            match http.get(&healthz_url).send().await {
                Ok(r) if r.status().is_success() => {
                    return Ok(Self {
                        base_url,
                        http,
                        _child: child,
                    });
                }
                Ok(r) => last_err = format!("status {}", r.status()),
                Err(e) => last_err = e.to_string(),
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(format!("pycoach healthcheck timed out: {last_err}"))
    }

    /// Build a launcher for `pycoach serve` from the environment.
    ///
    /// Resolution order (first match wins):
    ///   1. `COACH_PYCOACH_CMD` — escape hatch for dev workflows: a JSON
    ///      array like `["uv","run","--project","/path/to/pycoach","pycoach","serve"]`.
    ///      Used as-is.
    ///   2. `COACH_PYCOACH_BIN` — path to a pycoach executable; invoked as
    ///      `<bin> serve`.
    ///   3. A `pycoach` (or `pycoach.exe`) file living next to the current
    ///      executable. This is the path Tauri's `externalBin` bundling
    ///      drops the sidecar at, so production builds with the `pycoach`
    ///      Cargo feature pick up their bundled sidecar automatically.
    ///
    /// Returns `None` if none of those resolve. Selecting any source
    /// implicitly enables the sidecar — there is no separate "enable"
    /// flag, on purpose: the HTTP contract is still unstable and we
    /// don't want a persistent settings field for it yet.
    pub fn launcher_from_env() -> Option<Command> {
        if let Ok(json) = std::env::var("COACH_PYCOACH_CMD") {
            match serde_json::from_str::<Vec<String>>(&json) {
                Ok(parts) if !parts.is_empty() => {
                    let mut cmd = Command::new(&parts[0]);
                    cmd.args(&parts[1..]);
                    return Some(cmd);
                }
                Ok(_) => eprintln!("[pycoach] COACH_PYCOACH_CMD was empty array"),
                Err(e) => eprintln!("[pycoach] COACH_PYCOACH_CMD parse error: {e}"),
            }
        }
        if let Ok(bin) = std::env::var("COACH_PYCOACH_BIN") {
            if !bin.is_empty() {
                let mut cmd = Command::new(bin);
                cmd.arg("serve");
                return Some(cmd);
            }
        }
        if let Some(bundled) = bundled_sidecar_path() {
            let mut cmd = Command::new(bundled);
            cmd.arg("serve");
            return Some(cmd);
        }
        None
    }

    /// Convenience: hit `/healthz` and return the parsed JSON body.
    /// Used by the smoke test and by callers that want to verify the
    /// sidecar is still alive before issuing a real request.
    pub async fn healthz(&self) -> Result<serde_json::Value, String> {
        let url = format!("{}/healthz", self.base_url);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("healthz request failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("healthz status: {}", resp.status()));
        }
        resp.json()
            .await
            .map_err(|e| format!("healthz body parse failed: {e}"))
    }
}
