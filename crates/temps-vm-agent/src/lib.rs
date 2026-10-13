// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Wire protocol between the host-side Firecracker sandbox provider and the
//! in-guest `temps-vm-agent` (ADR-029 §5).
//!
//! Transport: one RPC per vsock connection. The host connects through
//! Firecracker's hybrid vsock Unix socket (`CONNECT <port>\n` handshake),
//! then sends a single length-prefixed request and reads a single
//! length-prefixed response. Framing is `u32` big-endian byte length
//! followed by a JSON payload — same shape as the pty-agent protocol so a
//! later unification is mechanical.
//!
//! Binary file contents travel hex-encoded. That doubles the on-wire size,
//! but keeps the protocol JSON-debuggable and dependency-free; vsock
//! throughput makes this a non-issue at sandbox file sizes. Exec output is
//! lossy UTF-8 by design — it feeds `SandboxExecResult { stdout: String }`.

pub mod logging;
pub mod mounts;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Vsock port the agent listens on for exec/fs RPCs.
pub const AGENT_PORT: u32 = 52;

/// Vsock port of the interactive-terminal relay (ADR-029 §5).
///
/// Unlike [`AGENT_PORT`] this is a raw byte pipe, not an RPC: after the
/// hybrid-socket `CONNECT` handshake every byte is relayed verbatim to the
/// `temps-pty-agent` unix socket at [`PTY_AGENT_SOCKET`], and the framed
/// ADR-008 protocol runs end to end between the host and that agent.
pub const PTY_PORT: u32 = 5000;

/// Where the host-side conversion pipeline places the static
/// `temps-pty-agent` binary inside the rootfs, next to this agent. Root-owned
/// and outside any directory a workload writes to.
pub const PTY_AGENT_BIN: &str = "/sbin/temps-pty-agent";

/// Control socket of the guest's PTY agent. Same path as the Docker sandboxes
/// (`temps_agents::sandbox::PTY_AGENT_SOCKET`; a test there keeps the two in
/// step) so a terminal behaves identically on every backend.
pub const PTY_AGENT_SOCKET: &str = "/run/temps-pty/agent.sock";

/// Most terminal relays served at once. The host caps interactive sessions
/// far below this (`MAX_CONCURRENT_TERMINALS`); the guest enforces its own
/// bound so a misbehaving peer cannot spawn threads without limit inside a
/// small VM.
pub const MAX_PTY_RELAYS: usize = 64;

/// Frames larger than this are rejected — the peer is untrusted.
pub const MAX_FRAME_BYTES: u32 = 64 * 1024 * 1024;

/// Default working directory for sandbox commands; created by the agent at
/// boot when missing. Matches the Docker sandboxes' work dir so the public
/// API's `work_dir` field means the same thing on both backends.
pub const WORK_DIR: &str = "/workspace";

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Ping,
    Exec {
        cmd: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        user: Option<u32>,
        #[serde(default)]
        timeout_secs: Option<u64>,
    },
    WriteFile {
        path: String,
        data_hex: String,
        mode: u32,
    },
    ReadFile {
        path: String,
    },
    Mkdir {
        path: String,
        mode: u32,
    },
    Kill {
        pattern: String,
        signal: i32,
    },
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Pong,
    Exec {
        exit_code: i32,
        stdout: String,
        stderr: String,
    },
    File {
        data_hex: String,
    },
    Err {
        message: String,
    },
}

/// Read one length-prefixed JSON frame.
pub fn read_frame<R: std::io::Read>(r: &mut R) -> std::io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf);
    if len == 0 || len > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame length {} out of bounds", len),
        ));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// Write one length-prefixed JSON frame.
pub fn write_frame<W: std::io::Write>(w: &mut W, payload: &[u8]) -> std::io::Result<()> {
    let len = payload.len() as u32;
    if len > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    w.write_all(&len.to_be_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

/// Size of the relay's copy buffer, one per direction.
const RELAY_BUF_BYTES: usize = 16 * 1024;

/// Copy bytes both ways between `host` (a vsock connection, wrapped as a
/// unix stream — the fd type is irrelevant to `read`/`write`) and `agent`
/// (the PTY agent's unix socket) until either side closes.
///
/// Byte-transparent by design: the ADR-008 framing, including its frame-size
/// limits, is enforced by the two ends. Each direction uses one fixed buffer,
/// so a relay's memory does not depend on traffic. When one direction ends,
/// both sockets are shut down so the other direction's blocked `read`
/// returns and the relay's threads exit instead of lingering.
pub fn relay(
    host: std::os::unix::net::UnixStream,
    agent: std::os::unix::net::UnixStream,
) -> std::io::Result<()> {
    use std::io::{Read, Write};
    use std::net::Shutdown;

    fn pump<R: Read, W: Write>(from: &mut R, to: &mut W) -> std::io::Result<()> {
        let mut buf = vec![0u8; RELAY_BUF_BYTES];
        loop {
            let n = match from.read(&mut buf) {
                Ok(0) => return Ok(()),
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            to.write_all(&buf[..n])?;
            to.flush()?;
        }
    }

    let mut host_rx = host.try_clone()?;
    let mut agent_tx = agent.try_clone()?;
    let host_ctl = host.try_clone()?;
    let agent_ctl = agent.try_clone()?;

    let upstream = std::thread::Builder::new()
        .name("pty-relay-up".to_string())
        .spawn(move || {
            let result = pump(&mut host_rx, &mut agent_tx);
            let _ = agent_ctl.shutdown(Shutdown::Both);
            let _ = host_ctl.shutdown(Shutdown::Both);
            result
        })?;

    let mut agent_rx = agent;
    let mut host_tx = host;
    let downstream = pump(&mut agent_rx, &mut host_tx);
    let _ = host_tx.shutdown(Shutdown::Both);
    let _ = agent_rx.shutdown(Shutdown::Both);

    let upstream = upstream
        .join()
        .unwrap_or_else(|_| Err(std::io::Error::other("relay thread panicked")));
    downstream.and(upstream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    #[test]
    fn relay_moves_bytes_both_ways_and_ends_when_the_host_closes() {
        let (host, mut host_peer) = UnixStream::pair().expect("host pair");
        let (agent, mut agent_peer) = UnixStream::pair().expect("agent pair");
        let relay = std::thread::spawn(move || relay(host, agent));

        host_peer.write_all(b"ping").expect("host write");
        let mut got = [0u8; 4];
        agent_peer.read_exact(&mut got).expect("agent read");
        assert_eq!(&got, b"ping");

        agent_peer.write_all(b"pong").expect("agent write");
        host_peer.read_exact(&mut got).expect("host read");
        assert_eq!(&got, b"pong");

        // The host detaching must tear the whole relay down.
        drop(host_peer);
        relay.join().expect("join").expect("relay result");
        let mut rest = Vec::new();
        agent_peer.read_to_end(&mut rest).expect("agent sees EOF");
        assert!(rest.is_empty());
    }

    #[test]
    fn relay_ends_when_the_agent_goes_away() {
        let (host, mut host_peer) = UnixStream::pair().expect("host pair");
        let (agent, agent_peer) = UnixStream::pair().expect("agent pair");
        let relay = std::thread::spawn(move || relay(host, agent));

        drop(agent_peer);
        relay.join().expect("join").expect("relay result");
        let mut rest = Vec::new();
        host_peer.read_to_end(&mut rest).expect("host sees EOF");
        assert!(rest.is_empty());
    }

    #[test]
    fn relay_preserves_a_payload_larger_than_its_buffer() {
        let (host, mut host_peer) = UnixStream::pair().expect("host pair");
        let (agent, mut agent_peer) = UnixStream::pair().expect("agent pair");
        let relay = std::thread::spawn(move || relay(host, agent));

        let payload: Vec<u8> = (0..RELAY_BUF_BYTES * 4 + 3)
            .map(|i| (i % 253) as u8)
            .collect();
        let expected = payload.clone();
        let writer = std::thread::spawn(move || {
            agent_peer.write_all(&payload).expect("agent write");
            // Dropping the peer closes the stream, which ends the relay.
        });
        let mut received = Vec::new();
        host_peer.read_to_end(&mut received).expect("host read");
        writer.join().expect("writer");
        relay.join().expect("join").expect("relay result");
        assert_eq!(received.len(), expected.len());
        assert!(received == expected, "relay corrupted the byte stream");
    }

    #[test]
    fn pty_port_does_not_collide_with_the_rpc_port() {
        assert_ne!(PTY_PORT, AGENT_PORT);
        assert_eq!(PTY_PORT, 5000, "ADR-029 §5 fixes the PTY vsock port");
    }

    #[test]
    fn frame_roundtrip() {
        let req = Request::Exec {
            cmd: vec!["echo".into(), "hi".into()],
            env: HashMap::new(),
            cwd: Some("/work".into()),
            user: None,
            timeout_secs: Some(5),
        };
        let json = serde_json::to_vec(&req).unwrap();
        let mut buf = Vec::new();
        write_frame(&mut buf, &json).unwrap();
        let got = read_frame(&mut &buf[..]).unwrap();
        assert_eq!(got, json);
        let parsed: Request = serde_json::from_slice(&got).unwrap();
        assert!(matches!(parsed, Request::Exec { .. }));
    }

    #[test]
    fn oversized_frame_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(MAX_FRAME_BYTES + 1).to_be_bytes());
        assert!(read_frame(&mut &buf[..]).is_err());
    }

    #[test]
    fn zero_frame_rejected() {
        let buf = 0u32.to_be_bytes();
        assert!(read_frame(&mut &buf[..]).is_err());
    }
}
