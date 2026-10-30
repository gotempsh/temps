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
pub mod restart;
pub mod stats;
pub mod supervisor;
pub mod workload_log;

pub use restart::SpawnRestartPolicy;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Commit this crate was built from, or `unknown` without a git checkout
/// (or when the build script did not run, as in a bundled container build).
pub const BUILD_COMMIT: &str = match option_env!("TEMPS_AGENT_BUILD_COMMIT") {
    Some(commit) => commit,
    None => "unknown",
};

/// Prefix of the build marker embedded in every guest agent binary. The host
/// finds it by scanning the file, so no agent has to be executed to learn
/// which build it is.
pub const BUILD_MARKER_PREFIX: &str = "TEMPS_GUEST_AGENT_BUILD=";

const MARKER_LEN: usize = BUILD_MARKER_PREFIX.len() + 41;

/// The marker: prefix, commit (at most 40 bytes), then NULs. Referenced from
/// each agent's `main` so the linker keeps it.
pub static BUILD_MARKER: [u8; MARKER_LEN] = build_marker(BUILD_COMMIT);

const fn build_marker(commit: &str) -> [u8; MARKER_LEN] {
    let mut out = [0u8; MARKER_LEN];
    let prefix = BUILD_MARKER_PREFIX.as_bytes();
    let commit = commit.as_bytes();
    let mut i = 0;
    while i < prefix.len() {
        out[i] = prefix[i];
        i += 1;
    }
    let mut j = 0;
    while j < commit.len() && j < 40 {
        out[prefix.len() + j] = commit[j];
        j += 1;
    }
    out
}

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
    /// Start a long-running process and return at once (ADR-055 Phase 10:
    /// a deployment's main process in a microVM). The agent pipes its
    /// stdout/stderr into a size-bounded, rotating log at `log_path` (at
    /// most `log_max_files` files of `log_max_bytes` each; defaults in
    /// [`workload_log`]), restarts it per `restart`, and
    /// [`Request::ProcessStatus`] reports on it. Every field after `cmd` is
    /// optional so an older host keeps its old meaning (no restarts).
    Spawn {
        cmd: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        uid: Option<u32>,
        #[serde(default)]
        gid: Option<u32>,
        #[serde(default)]
        log_path: Option<String>,
        #[serde(default)]
        restart: SpawnRestartPolicy,
        #[serde(default)]
        log_max_bytes: Option<u64>,
        #[serde(default)]
        log_max_files: Option<u32>,
    },
    /// Whether a process [`Request::Spawn`] started still runs. `pid` is the
    /// pid `Spawned` returned; it keeps naming the process across restarts.
    ProcessStatus {
        pid: u32,
    },
    /// The last `tail_lines` lines of a spawned process's log, read across
    /// its rotated files. Needs no `tail` binary in the guest image.
    ReadLog {
        #[serde(default)]
        path: Option<String>,
        tail_lines: u32,
    },
    /// The log's complete lines after `cursor` (from the start of what is
    /// still retained when `None`), at most `max_bytes` of them, read across
    /// rotated files. The returned cursor goes into the next request, so a
    /// reader sees every line once without the guest keeping any state.
    /// A line still being written is held back until it ends, unless
    /// `include_partial` asks for it.
    ReadLogSince {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        cursor: Option<LogCursor>,
        #[serde(default)]
        max_bytes: Option<u32>,
        #[serde(default)]
        include_partial: bool,
    },
    /// Cumulative CPU, memory, disk and network counters of the guest, with
    /// CPU and memory taken from the tree of the process [`Request::Spawn`]
    /// started as `pid` (every process when absent). The host turns two
    /// samples into rates; the counters restart from zero when the supervised
    /// process does.
    ReadStats {
        #[serde(default)]
        pid: Option<u32>,
    },
    Shutdown,
}

/// A position in a rotating log: the file (by inode, which survives the
/// rename a rotation does) and the byte offset in it. Handed back by
/// [`Response::LogChunk`] and passed to the next [`Request::ReadLogSince`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogCursor {
    pub inode: u64,
    pub offset: u64,
}

/// Most bytes one [`Request::ReadLogSince`] returns when the request names
/// no limit.
pub const DEFAULT_LOG_CHUNK_BYTES: u32 = 256 * 1024;

/// Largest chunk honoured, so one request cannot build a frame the host
/// would refuse ([`MAX_FRAME_BYTES`]).
pub const MAX_LOG_CHUNK_BYTES: u32 = 4 * 1024 * 1024;

/// Where a spawned process writes its output when the request names no path.
pub const DEFAULT_SPAWN_LOG: &str = "/var/log/temps-workload.log";

/// Most lines one [`Request::ReadLog`] returns.
pub const MAX_READ_LOG_LINES: u32 = 10_000;

/// Most processes [`Request::Spawn`] tracks at once: a workload VM runs one
/// main process, so this only bounds a misbehaving host.
pub const MAX_SPAWNED_PROCESSES: usize = 16;

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
    Spawned {
        pid: u32,
    },
    ProcessStatus {
        running: bool,
        /// Set once the process exited (`-1` when killed by a signal); the
        /// last run's code while a restart is pending.
        #[serde(default)]
        exit_code: Option<i32>,
        /// Times the agent restarted the process under its restart policy.
        #[serde(default)]
        restart_count: u32,
        /// The process exited and the agent will start it again once the
        /// backoff has passed.
        #[serde(default)]
        restarting: bool,
    },
    /// The tail [`Request::ReadLog`] asked for (lossy UTF-8).
    Log {
        data: String,
    },
    /// What [`Request::ReadLogSince`] found.
    LogChunk {
        /// Complete lines (lossy UTF-8), oldest first.
        data: String,
        /// Where the next read starts; `None` when the log has no file yet.
        #[serde(default)]
        cursor: Option<LogCursor>,
        /// The file the request's cursor named was rotated away before it
        /// was read: lines between the cursor and the oldest retained file
        /// are lost.
        #[serde(default)]
        skipped: bool,
        /// More is already available: read again without waiting.
        #[serde(default)]
        more: bool,
    },
    /// What [`Request::ReadStats`] sampled.
    Stats(stats::GuestStats),
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

/// Shorter no-progress limit once the *source* has disconnected. Nobody is
/// left to receive anything the other side would produce, so there is no
/// reason to wait the full stall limit — but a receiver that is merely slow
/// (still taking bytes) must be allowed to finish taking what was already
/// sent, which is why this is a no-progress limit and not an abort on
/// hang-up.
const RELAY_CLOSED_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Timing limits of one relay; a value type so tests can shrink them.
#[derive(Clone, Copy)]
struct RelayLimits {
    write_slice: std::time::Duration,
    stall_limit: std::time::Duration,
    closed_grace: std::time::Duration,
}

impl RelayLimits {
    const DEFAULT: RelayLimits = RelayLimits {
        write_slice: RELAY_WRITE_SLICE,
        stall_limit: RELAY_STALL_LIMIT,
        closed_grace: RELAY_CLOSED_GRACE,
    };
}

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
/// in short slices. A write that makes no progress ends the relay (and shuts
/// both sockets down) after [`RELAY_STALL_LIMIT`], or after the shorter
/// [`RELAY_CLOSED_GRACE`] once the source has hung up. A receiver that keeps
/// taking bytes is never cut off, so input already sent still arrives.
pub fn relay(
    host: std::os::unix::net::UnixStream,
    agent: std::os::unix::net::UnixStream,
) -> std::io::Result<()> {
    relay_with_limits(host, agent, RelayLimits::DEFAULT)
}

fn relay_with_limits(
    host: std::os::unix::net::UnixStream,
    agent: std::os::unix::net::UnixStream,
    limits: RelayLimits,
) -> std::io::Result<()> {
    let RelayLimits {
        write_slice,
        stall_limit,
        closed_grace,
    } = limits;
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

    /// `write_all` that gives up when no progress is made for `stall_limit`,
    /// or for `closed_grace` once the source has hung up. `to` must have a
    /// write timeout of one slice.
    fn write_watching(
        to: &mut UnixStream,
        source: &UnixStream,
        mut data: &[u8],
        stall_limit: std::time::Duration,
        closed_grace: std::time::Duration,
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
                    if last_progress.elapsed() >= closed_grace && source_closed(source) {
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
        closed_grace: std::time::Duration,
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
            write_watching(to, &source, &buf[..n], stall_limit, closed_grace)?;
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
            let result = pump(&mut host_rx, &mut agent_tx, stall_limit, closed_grace);
            let _ = agent_ctl.shutdown(Shutdown::Both);
            let _ = host_ctl.shutdown(Shutdown::Both);
            result
        })?;

    let mut agent_rx = agent;
    let mut host_tx = host;
    let downstream = pump(&mut agent_rx, &mut host_tx, stall_limit, closed_grace);
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
    fn spawn_and_status_round_trip_on_the_wire() {
        let request: Request = serde_json::from_str(
            r#"{"op":"spawn","cmd":["/app/server","--port","8080"],"env":{"A":"b"},"uid":1000}"#,
        )
        .expect("spawn parses");
        match request {
            Request::Spawn {
                cmd,
                env,
                uid,
                gid,
                log_path,
                restart,
                log_max_bytes,
                log_max_files,
                ..
            } => {
                assert_eq!(cmd, ["/app/server", "--port", "8080"]);
                assert_eq!(env["A"], "b");
                assert_eq!(uid, Some(1000));
                assert_eq!(gid, None);
                assert_eq!(log_path, None);
                // An older host's request: no restarts, default log bounds.
                assert_eq!(restart, SpawnRestartPolicy::Never);
                assert_eq!((log_max_bytes, log_max_files), (None, None));
            }
            other => panic!("unexpected {other:?}"),
        }
        let status: Request =
            serde_json::from_str(r#"{"op":"process_status","pid":42}"#).expect("status parses");
        assert!(matches!(status, Request::ProcessStatus { pid: 42 }));
        let response = serde_json::to_string(&Response::ProcessStatus {
            running: false,
            exit_code: Some(3),
            restart_count: 2,
            restarting: true,
        })
        .expect("serialises");
        assert_eq!(
            response,
            r#"{"result":"process_status","running":false,"exit_code":3,"restart_count":2,"restarting":true}"#
        );
        // An older agent's status still parses on a newer host.
        let old: Response = serde_json::from_str(r#"{"result":"process_status","running":true}"#)
            .expect("old status parses");
        assert!(matches!(
            old,
            Response::ProcessStatus {
                running: true,
                exit_code: None,
                restart_count: 0,
                restarting: false
            }
        ));
    }

    #[test]
    fn a_spawn_with_a_restart_policy_and_log_bounds_round_trips() {
        let request = Request::Spawn {
            cmd: vec!["/app/server".into()],
            env: HashMap::new(),
            cwd: None,
            uid: None,
            gid: None,
            log_path: None,
            restart: SpawnRestartPolicy::UnlessStopped,
            log_max_bytes: Some(10 * 1024 * 1024),
            log_max_files: Some(3),
        };
        let json = serde_json::to_string(&request).expect("serialises");
        assert!(
            json.contains(r#""restart":{"policy":"unless_stopped"}"#),
            "{json}"
        );
        let back: Request = serde_json::from_str(&json).expect("parses");
        assert!(matches!(
            back,
            Request::Spawn {
                restart: SpawnRestartPolicy::UnlessStopped,
                log_max_bytes: Some(10485760),
                log_max_files: Some(3),
                ..
            }
        ));
        let read_log: Request =
            serde_json::from_str(r#"{"op":"read_log","tail_lines":50}"#).expect("parses");
        assert!(matches!(
            read_log,
            Request::ReadLog {
                path: None,
                tail_lines: 50
            }
        ));
    }

    #[test]
    fn read_stats_parses_with_and_without_a_pid() {
        let all: Request = serde_json::from_str(r#"{"op":"read_stats"}"#).expect("parses");
        assert!(matches!(all, Request::ReadStats { pid: None }));
        let one: Request = serde_json::from_str(r#"{"op":"read_stats","pid":42}"#).expect("parses");
        assert!(matches!(one, Request::ReadStats { pid: Some(42) }));
    }

    #[test]
    fn a_stats_response_round_trips_with_its_result_tag() {
        let stats = stats::GuestStats {
            cpu_nanos: 5,
            memory_bytes: 6,
            pids: 2,
            net_tx_bytes: 9,
            ..Default::default()
        };
        let json = serde_json::to_string(&Response::Stats(stats)).expect("serializes");
        assert!(json.contains(r#""result":"stats""#), "{json}");
        let back: Response = serde_json::from_str(&json).expect("parses");
        assert!(matches!(back, Response::Stats(s) if s == stats));
    }

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
        limits: RelayLimits,
        within: std::time::Duration,
    ) -> std::io::Result<()> {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = done_tx.send(relay_with_limits(host, agent, limits));
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
                RelayLimits {
                    write_slice: std::time::Duration::from_millis(20),
                    stall_limit: std::time::Duration::from_secs(60),
                    // Short, but far longer than a slice: the point is that
                    // it is *this* limit, not the 60s stall limit, that
                    // frees the relay once the host has gone.
                    closed_grace: std::time::Duration::from_millis(150),
                },
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
            RelayLimits {
                write_slice: std::time::Duration::from_millis(20),
                stall_limit: std::time::Duration::from_millis(400),
                closed_grace: std::time::Duration::from_millis(400),
            },
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
                RelayLimits {
                    write_slice: std::time::Duration::from_millis(20),
                    stall_limit: std::time::Duration::from_millis(1500),
                    // The host closes right after writing, while the reader
                    // is still draining: a closed source with a receiver that
                    // keeps taking bytes must be delivered in full.
                    closed_grace: std::time::Duration::from_millis(300),
                },
                std::time::Duration::from_secs(30),
            )
        });
        host_peer.write_all(&payload).expect("host write");
        drop(host_peer);

        assert_eq!(reader.join().expect("reader"), expected);
        relay.join().expect("join").expect("relay result");
    }
}
