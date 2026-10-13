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

/// How often a blocked relay write wakes up to check whether the side it is
/// copying *from* has gone away.
const RELAY_WRITE_SLICE: std::time::Duration = std::time::Duration::from_millis(500);

/// Longest a relay write may make no progress at all before the attachment
/// is ended. A peer that has stopped reading (a program ignoring its input,
/// a host that stopped draining) must not pin a relay slot for good.
const RELAY_STALL_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);

/// Copy bytes both ways between `host` (a vsock connection, wrapped as a
/// unix stream — the fd type is irrelevant to `read`/`write`) and `agent`
/// (the PTY agent's unix socket) until either side closes.
///
/// Byte-transparent by design: the ADR-008 framing, including its frame-size
/// limits, is enforced by the two ends. Each direction uses one fixed buffer,
/// so a relay's memory does not depend on traffic. When one direction ends,
/// both sockets are shut down so the other direction's blocked `read`
/// returns and the relay's threads exit instead of lingering.
///
/// A *write* can block too — the receiver may have stopped reading — and a
/// closed source is not noticed by a thread stuck writing. So writes proceed
/// in short slices, and between slices the relay checks that the source is
/// still open and that some progress was made within [`RELAY_STALL_LIMIT`];
/// either failure ends the relay and shuts both sockets down.
pub fn relay(
    host: std::os::unix::net::UnixStream,
    agent: std::os::unix::net::UnixStream,
) -> std::io::Result<()> {
    relay_with_limits(host, agent, RELAY_WRITE_SLICE, RELAY_STALL_LIMIT)
}

fn relay_with_limits(
    host: std::os::unix::net::UnixStream,
    agent: std::os::unix::net::UnixStream,
    write_slice: std::time::Duration,
    stall_limit: std::time::Duration,
) -> std::io::Result<()> {
    use std::io::Read;
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;

    /// Whether the peer of `socket` has fully disconnected (closed, reset or
    /// shut down both ways), without consuming data.
    ///
    /// Not a `recv` peek: the source may still hold queued input, and an
    /// end-of-file only shows once that is read — which is exactly what a
    /// thread stuck writing cannot do. `poll` reports the hang-up
    /// regardless of what is queued. A peer that merely stopped *sending*
    /// (half-close) is not a disconnect and is left to finish.
    fn source_closed(socket: &UnixStream) -> bool {
        use std::os::fd::AsRawFd;

        let mut fd = libc::pollfd {
            fd: socket.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `fd` is a valid pollfd for one descriptor that stays open
        // for the lifetime of `socket`; a zero timeout never blocks.
        let ready = unsafe { libc::poll(&mut fd, 1, 0) };
        ready > 0 && fd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
    }

    /// `write_all` that gives up when the source closes or no progress is
    /// made for `stall_limit`. `to` must have a write timeout of one slice.
    fn write_watching(
        to: &mut UnixStream,
        source: &UnixStream,
        mut data: &[u8],
        stall_limit: std::time::Duration,
    ) -> std::io::Result<()> {
        use std::io::Write;

        let mut last_progress = std::time::Instant::now();
        while !data.is_empty() {
            match to.write(data) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "relay peer accepted no bytes",
                    ))
                }
                Ok(n) => {
                    data = &data[n..];
                    last_progress = std::time::Instant::now();
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    if source_closed(source) {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::BrokenPipe,
                            "relay source closed while its peer was not reading",
                        ));
                    }
                    if last_progress.elapsed() >= stall_limit {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            format!("relay peer made no progress for {}s", stall_limit.as_secs()),
                        ));
                    }
                }
                Err(e) => return Err(e),
            }
        }
        to.flush_ok()
    }

    /// `UnixStream` flushes are no-ops; named so the intent is explicit.
    trait FlushOk {
        fn flush_ok(&mut self) -> std::io::Result<()>;
    }
    impl FlushOk for UnixStream {
        fn flush_ok(&mut self) -> std::io::Result<()> {
            std::io::Write::flush(self)
        }
    }

    fn pump(
        from: &mut UnixStream,
        to: &mut UnixStream,
        stall_limit: std::time::Duration,
    ) -> std::io::Result<()> {
        let source = from.try_clone()?;
        let mut buf = vec![0u8; RELAY_BUF_BYTES];
        loop {
            let n = match from.read(&mut buf) {
                Ok(0) => return Ok(()),
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            write_watching(to, &source, &buf[..n], stall_limit)?;
        }
    }

    // A socket whose peer is already gone can refuse the option (EINVAL on
    // some kernels); its first read then ends the relay anyway, so that one
    // refusal is not an error.
    for socket in [&host, &agent] {
        match socket.set_write_timeout(Some(write_slice)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => {}
            Err(e) => return Err(e),
        }
    }

    let mut host_rx = host.try_clone()?;
    let mut agent_tx = agent.try_clone()?;
    let host_ctl = host.try_clone()?;
    let agent_ctl = agent.try_clone()?;

    let upstream = std::thread::Builder::new()
        .name("pty-relay-up".to_string())
        .spawn(move || {
            let result = pump(&mut host_rx, &mut agent_tx, stall_limit);
            let _ = agent_ctl.shutdown(Shutdown::Both);
            let _ = host_ctl.shutdown(Shutdown::Both);
            result
        })?;

    let mut agent_rx = agent;
    let mut host_tx = host;
    let downstream = pump(&mut agent_rx, &mut host_tx, stall_limit);
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

    /// Run `relay_with_limits` on a thread and wait for it, failing the test
    /// (rather than hanging it) when the relay does not end.
    fn run_relay_to_end(
        host: UnixStream,
        agent: UnixStream,
        slice: std::time::Duration,
        stall: std::time::Duration,
        within: std::time::Duration,
    ) -> std::io::Result<()> {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = done_tx.send(relay_with_limits(host, agent, slice, stall));
        });
        done_rx
            .recv_timeout(within)
            .expect("the relay must end instead of holding its slot")
    }

    /// Fill `peer`'s outgoing buffer from a thread, as a pasted blob or a
    /// busy terminal would; the thread ends when the stream breaks.
    fn flood(mut peer: UnixStream) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let chunk = vec![b'x'; 64 * 1024];
            while peer.write_all(&chunk).is_ok() {}
        })
    }

    #[test]
    fn a_host_that_disconnects_while_the_agent_is_not_reading_frees_the_relay() {
        // The agent never reads: the upstream copy blocks in `write`, and the
        // downstream copy waits for output that will never come. Only the
        // host going away can end this, and neither blocked call sees it.
        let (host, host_peer) = UnixStream::pair().expect("host pair");
        let (agent, _agent_peer) = UnixStream::pair().expect("agent pair");
        let host_ctl = host_peer.try_clone().expect("clone");
        let writer = flood(host_peer);

        let relay = std::thread::spawn(move || {
            run_relay_to_end(
                host,
                agent,
                std::time::Duration::from_millis(20),
                std::time::Duration::from_secs(60),
                std::time::Duration::from_secs(10),
            )
        });
        // Let the buffers fill so the relay is genuinely stuck mid-write.
        std::thread::sleep(std::time::Duration::from_millis(300));
        host_ctl
            .shutdown(std::net::Shutdown::Both)
            .expect("host disconnects");

        let result = relay.join().expect("join");
        assert!(result.is_err(), "ended by a broken pipe, not cleanly");
        writer.join().expect("writer");
    }

    #[test]
    fn a_peer_that_never_reads_is_cut_off_after_the_stall_limit() {
        // The host stays connected but the agent stops draining: with no
        // disconnect to observe, only the stall limit can free the slot.
        let (host, host_peer) = UnixStream::pair().expect("host pair");
        let (agent, _agent_peer) = UnixStream::pair().expect("agent pair");
        let writer = flood(host_peer);

        let started = std::time::Instant::now();
        let result = run_relay_to_end(
            host,
            agent,
            std::time::Duration::from_millis(20),
            std::time::Duration::from_millis(400),
            std::time::Duration::from_secs(10),
        );
        let error = result.expect_err("a stalled relay is an error");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut, "{error}");
        assert!(started.elapsed() >= std::time::Duration::from_millis(400));
        writer.join().expect("writer");
    }

    #[test]
    fn a_slow_but_reading_peer_is_not_mistaken_for_a_stall() {
        let (host, mut host_peer) = UnixStream::pair().expect("host pair");
        let (agent, mut agent_peer) = UnixStream::pair().expect("agent pair");
        let payload = vec![7u8; 1024 * 1024];
        let expected = payload.len();

        // Drain slowly: every read makes progress inside the stall limit.
        let reader = std::thread::spawn(move || {
            let mut total = 0usize;
            let mut buf = vec![0u8; 32 * 1024];
            while total < expected {
                std::thread::sleep(std::time::Duration::from_millis(5));
                match agent_peer.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => total += n,
                }
            }
            total
        });
        let relay = std::thread::spawn(move || {
            run_relay_to_end(
                host,
                agent,
                std::time::Duration::from_millis(20),
                std::time::Duration::from_millis(1500),
                std::time::Duration::from_secs(30),
            )
        });
        host_peer.write_all(&payload).expect("host write");
        drop(host_peer);

        assert_eq!(reader.join().expect("reader"), expected);
        relay.join().expect("join").expect("relay result");
    }
}
