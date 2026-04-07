//! Resolve a peer TCP port to the PID that owns it.
//!
//! Hooks reach Coach as HTTP POSTs from Claude Code. The hook payload
//! does not contain a PID, and `session_id` is the conversation id, which
//! changes on `/clear`. The only signal that always identifies which
//! Claude Code window the request came from is the kernel-level
//! ownership of the TCP socket — exposed via `lsof`.
//!
//! See `docs/SESSION_TRACKING.md` for the rationale.

use std::process::Command;

/// Find the PID that owns the TCP source port `peer_port` for a connection
/// to our `listen_port` on 127.0.0.1. Returns None if lsof fails or no
/// matching established connection is found.
///
/// We exclude our own PID because the server side of the connection also
/// shows up in lsof output (the accepted FD has the peer port as its
/// remote endpoint).
pub fn resolve_peer_pid(peer_port: u16, listen_port: u16) -> Option<u32> {
    let our_pid = std::process::id();
    let output = Command::new("lsof")
        .args(["-nP", &format!("-iTCP@127.0.0.1:{listen_port}")])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_lsof_for_peer_pid(&stdout, peer_port, our_pid)
}

/// Pure parser, separated for testability.
///
/// lsof output format we care about:
/// ```text
/// COMMAND   PID   USER   FD   TYPE  DEVICE  SIZE/OFF  NODE  NAME
/// node    76996 ikamen   20u  IPv4  …       0t0       TCP   127.0.0.1:54321->127.0.0.1:7700 (ESTABLISHED)
/// ```
/// We want the row where the **local** port equals `peer_port` (the
/// client side of the connection — i.e. Claude Code), excluding our own
/// PID. The listener row has no `->` and is naturally skipped.
fn parse_lsof_for_peer_pid(stdout: &str, peer_port: u16, our_pid: u32) -> Option<u32> {
    for line in stdout.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 9 {
            continue;
        }
        let pid: u32 = match fields[1].parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if pid == our_pid {
            continue;
        }
        let Some(name) = fields.iter().find(|f| f.contains("->")) else {
            continue;
        };
        let (left, _) = match name.split_once("->") {
            Some(parts) => parts,
            None => continue,
        };
        let Some(port_str) = left.rsplit(':').next() else {
            continue;
        };
        let local_port: u16 = match port_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if local_port == peer_port {
            return Some(pid);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parser should pull the PID off the line where the local port
    /// matches and the PID is not ours.
    #[test]
    fn parses_client_pid_from_lsof_output() {
        let sample = "\
COMMAND   PID   USER   FD   TYPE  DEVICE             SIZE/OFF NODE NAME
node    76996 ikamen  15u  IPv4  0x1b5ca2526bf7cde5  0t0      TCP  127.0.0.1:7700 (LISTEN)
node    76996 ikamen  16u  IPv4  0x1b5ca2526bf7ce00  0t0      TCP  127.0.0.1:7700->127.0.0.1:54321 (ESTABLISHED)
node    99999 ikamen  20u  IPv4  0x1b5ca2526bf7ce11  0t0      TCP  127.0.0.1:54321->127.0.0.1:7700 (ESTABLISHED)
";
        // our_pid = 76996 (the listener) → should return 99999 (the client)
        assert_eq!(parse_lsof_for_peer_pid(sample, 54321, 76996), Some(99999));
    }

    /// When no row's local port matches the peer_port, return None
    /// rather than picking the wrong row.
    #[test]
    fn returns_none_when_no_local_port_matches() {
        let sample = "\
COMMAND   PID   USER   FD   TYPE  DEVICE             SIZE/OFF NODE NAME
node    99999 ikamen  20u  IPv4  0x1b5ca2526bf7ce11  0t0      TCP  127.0.0.1:11111->127.0.0.1:7700 (ESTABLISHED)
";
        assert_eq!(parse_lsof_for_peer_pid(sample, 54321, 76996), None);
    }

    /// The listener row has no "->" in NAME — it should be skipped
    /// silently rather than tripping the parser.
    #[test]
    fn skips_listener_rows_without_arrow() {
        let sample = "\
COMMAND   PID   USER   FD   TYPE  DEVICE             SIZE/OFF NODE NAME
coach   23438 ikamen  15u  IPv4  0x1b5ca2526bf7cde5  0t0      TCP  127.0.0.1:7700 (LISTEN)
";
        assert_eq!(parse_lsof_for_peer_pid(sample, 54321, 23438), None);
    }

    /// Empty output (lsof found nothing) should yield None, not a panic.
    #[test]
    fn empty_input_yields_none() {
        assert_eq!(parse_lsof_for_peer_pid("", 54321, 76996), None);
    }

    /// End-to-end: spawn a child that opens a TCP connection to a listener
    /// in this process, then resolve the peer port → the child's PID via
    /// real `lsof`. This is the only test that proves the integration
    /// actually works on this machine.
    #[test]
    fn resolves_real_connection_to_child_pid() {
        use std::net::TcpListener;
        use std::process::{Command, Stdio};

        // Skip if lsof or python3 are missing — we don't want to fail
        // CI on minimal images that lack them.
        if Command::new("lsof").arg("-v").output().is_err() {
            eprintln!("lsof not available, skipping");
            return;
        }
        if Command::new("python3").arg("--version").output().is_err() {
            eprintln!("python3 not available, skipping");
            return;
        }

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let listen_port = listener.local_addr().unwrap().port();

        let mut child = Command::new("python3")
            .arg("-c")
            .arg(format!(
                "import socket, time; \
                 s = socket.socket(); \
                 s.connect(('127.0.0.1', {listen_port})); \
                 time.sleep(3)"
            ))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn python3");
        let child_pid = child.id();

        let (_stream, peer_addr) = listener.accept().expect("accept");
        let peer_port = peer_addr.port();

        let resolved = resolve_peer_pid(peer_port, listen_port);

        let _ = child.kill();
        let _ = child.wait();

        assert_eq!(
            resolved,
            Some(child_pid),
            "lsof should resolve peer port {peer_port} on listen port {listen_port} to child pid {child_pid}"
        );
    }
}
