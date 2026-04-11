//! Resolve a peer TCP port to the PID that owns it.
//!
//! Used as display metadata: sessions are identified by the
//! `session_id` in the hook payload, and we record the owning PID so
//! the UI can show it. If this resolver fails (e.g. for loopback
//! connections inside a single process where both ends share a PID we
//! exclude), sessions still work — they just carry pid 0 until a
//! future hook lands with a resolvable peer.

use netstat2::{
    get_sockets_info, AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo,
};

/// Find the PID that owns the TCP source port `peer_port` for an
/// established connection to our `listen_port` on 127.0.0.1. Returns
/// None if the kernel call fails or no matching connection is found.
///
/// `listen_port` is currently unused — we filter purely on
/// `local_port == peer_port`, which uniquely identifies the client side
/// of the loopback connection. The argument is kept in the signature
/// for documentation: it's the port the resolver was set up against,
/// and a future implementation could narrow the kernel query to
/// connections involving that port for a marginal speedup.
///
/// We exclude our own PID because both ends of a loopback connection
/// show up in the table.
pub fn resolve_peer_pid(peer_port: u16, _listen_port: u16) -> Option<u32> {
    let our_pid = std::process::id();
    let sockets =
        get_sockets_info(AddressFamilyFlags::IPV4, ProtocolFlags::TCP).ok()?;

    sockets.into_iter().find_map(|si| {
        let ProtocolSocketInfo::Tcp(tcp) = &si.protocol_socket_info else {
            return None;
        };
        if tcp.local_port != peer_port {
            return None;
        }
        si.associated_pids
            .into_iter()
            .find(|&pid| pid != our_pid)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end: spawn a child that opens a TCP connection to a
    /// listener in this process, then resolve the peer port → the
    /// child's PID via real netstat2. This is the integration check
    /// that the kernel call returns what we need on this platform.
    #[test]
    fn resolves_real_connection_to_child_pid() {
        use std::net::TcpListener;
        use std::process::{Command, Stdio};

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
            "netstat2 should resolve peer port {peer_port} on listen port {listen_port} to child pid {child_pid}"
        );
    }

    /// Property: a port that nobody is connected to resolves to None,
    /// not to some random other process. Picks a port high enough that
    /// it's almost certainly free on the test machine.
    #[test]
    fn unconnected_port_resolves_to_none() {
        // 0 is never a valid peer port for an established connection
        // (it's the "any" placeholder).
        assert_eq!(resolve_peer_pid(0, 7700), None);
    }
}
