// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The agent's socket server. One task per accepted connection. Shared state
//! (`Tabs`) holds the living map of tab_id → PTY + subscribers.
//!
//! Subscriber model: every accepted connection gets a tokio broadcast
//! receiver on the tab it OPENs. The PTY-reader task holds the sole
//! broadcast sender. When a connection closes, its receiver is dropped;
//! the PTY + sender live on until explicit KILL or child-exit.
//!
//! The broadcast channel carries `Bytes` (shared, zero-copy fan-out). On
//! subscriber lag we drop the oldest frames — better than blocking the PTY
//! reader and starving other subscribers of output.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, Mutex, RwLock};

use crate::protocol::{
    decode_resize, read_frame, write_frame, write_json_frame, ErrorPayload, ExitEvent, OpenRequest,
    OpenedResponse, TabInfo, OP_DETACH, OP_ERROR, OP_EXIT, OP_INPUT, OP_KILL, OP_LIST, OP_OPEN,
    OP_OPENED, OP_OUTPUT, OP_PING, OP_PONG, OP_RESIZE, OP_TABS,
};
use crate::pty::{kill_tree, resize_pty, spawn_pty, try_reap, Pty};

/// Cap per-tab ring; OPEN's `replay_bytes` is clamped to this.
const RING_BYTES: usize = 64 * 1024;
/// Broadcast depth. PTY output arrives in small bursts; this covers most
/// backpressure without dropping on a mildly-slow subscriber.
const BROADCAST_DEPTH: usize = 256;

struct Tab {
    kind: String,
    label: Option<String>,
    pid: i32,
    master_fd: i32,
    /// Bounded ring of recent output bytes, sent to new subscribers.
    ring: Mutex<Ring>,
    /// Fan-out to subscribers. PTY-reader task holds the sender; each
    /// connection keeps its own receiver.
    tx: broadcast::Sender<Bytes>,
    /// Stdin half of the master fd, wrapped for concurrent writes from
    /// multiple subscribers. Writes are serialized by the mutex so two
    /// subscribers don't interleave INPUT frames mid-character.
    stdin: Mutex<tokio::fs::File>,
    created_at_ms: u64,
}

struct Ring {
    buf: Vec<u8>,
    /// Write index into `buf` (wraps around). We don't track "fullness"
    /// separately — first write fills from 0 upward, subsequent writes wrap.
    pos: usize,
    /// True once we've wrapped at least once; before that, only bytes
    /// `[0..pos]` are valid.
    wrapped: bool,
}

impl Ring {
    fn new() -> Self {
        Self {
            buf: vec![0u8; RING_BYTES],
            pos: 0,
            wrapped: false,
        }
    }
    fn push(&mut self, data: &[u8]) {
        // For writes larger than the ring, keep only the tail — the older
        // bytes wouldn't survive the wrap anyway.
        let data = if data.len() >= self.buf.len() {
            self.wrapped = true;
            self.pos = 0;
            &data[data.len() - self.buf.len()..]
        } else {
            data
        };
        let space = self.buf.len() - self.pos;
        if data.len() <= space {
            self.buf[self.pos..self.pos + data.len()].copy_from_slice(data);
            self.pos += data.len();
            if self.pos == self.buf.len() {
                self.pos = 0;
                self.wrapped = true;
            }
        } else {
            let (first, second) = data.split_at(space);
            self.buf[self.pos..].copy_from_slice(first);
            self.buf[..second.len()].copy_from_slice(second);
            self.pos = second.len();
            self.wrapped = true;
        }
    }
    fn tail(&self, max: usize) -> Vec<u8> {
        let available = if self.wrapped {
            self.buf.len()
        } else {
            self.pos
        };
        let n = max.min(available);
        let mut out = Vec::with_capacity(n);
        if self.wrapped {
            // Oldest byte is at `pos`, newest at `pos - 1` (mod len).
            let start = (self.pos + self.buf.len() - n) % self.buf.len();
            if start + n <= self.buf.len() {
                out.extend_from_slice(&self.buf[start..start + n]);
            } else {
                let first = self.buf.len() - start;
                out.extend_from_slice(&self.buf[start..]);
                out.extend_from_slice(&self.buf[..n - first]);
            }
        } else {
            let start = self.pos.saturating_sub(n);
            out.extend_from_slice(&self.buf[start..self.pos]);
        }
        out
    }
}

#[derive(Default)]
pub struct Tabs {
    inner: RwLock<HashMap<String, Arc<Tab>>>,
}

impl Tabs {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    async fn list(&self) -> Vec<TabInfo> {
        let guard = self.inner.read().await;
        guard
            .iter()
            .map(|(id, tab)| TabInfo {
                tab_id: id.clone(),
                kind: tab.kind.clone(),
                label: tab.label.clone(),
                pid: tab.pid,
                created_at_ms: tab.created_at_ms,
                subscriber_count: tab.tx.receiver_count() as u32,
            })
            .collect()
    }

    async fn get(&self, tab_id: &str) -> Option<Arc<Tab>> {
        self.inner.read().await.get(tab_id).cloned()
    }

    async fn remove(&self, tab_id: &str) -> Option<Arc<Tab>> {
        self.inner.write().await.remove(tab_id)
    }

    async fn open_or_attach(self: &Arc<Self>, req: &OpenRequest) -> io::Result<(Arc<Tab>, bool)> {
        if let Some(existing) = self.get(&req.tab_id).await {
            // Treat attach to an existing tab as idempotent. Resize to the
            // new client's dims so the TUI repaints at their size.
            let _ = resize_pty(existing.master_fd, req.cols, req.rows);
            return Ok((existing, true));
        }

        // Caller can override via the OPEN payload (tests on macOS need a
        // minimal PATH + their own HOME); otherwise the shell gets the
        // sandbox's own environment, see `default_shell_env`.
        let env = req
            .env
            .clone()
            .unwrap_or_else(|| default_shell_env(std::env::vars()));
        let cwd = req.cwd.as_deref().unwrap_or("/workspace");
        let pty = spawn_pty(&req.cmd, cwd, &env, req.cols.max(1), req.rows.max(1))?;
        let Pty {
            master,
            master_fd,
            pid,
        } = pty;
        // Split the master File into a read half (owned by the reader task)
        // and a write half (parked in the Tab, used by INPUT frames).
        let stdin = master.try_clone().await?;
        let (tx, _) = broadcast::channel(BROADCAST_DEPTH);
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let tab = Arc::new(Tab {
            kind: req.kind.clone(),
            label: req.label.clone(),
            pid,
            master_fd,
            ring: Mutex::new(Ring::new()),
            tx,
            stdin: Mutex::new(stdin),
            created_at_ms: now_ms,
        });
        self.inner
            .write()
            .await
            .insert(req.tab_id.clone(), tab.clone());
        // Spawn the PTY-reader. Holds the only clone of `master` (read half)
        // so when the child exits and EOF propagates, the task ends and
        // drops it.
        tokio::spawn(pty_reader_task(
            self.clone(),
            req.tab_id.clone(),
            master,
            tab.clone(),
        ));
        Ok((tab, false))
    }
}

async fn pty_reader_task(
    tabs: Arc<Tabs>,
    tab_id: String,
    mut master: tokio::fs::File,
    tab: Arc<Tab>,
) {
    // 16 KiB matches Linux's default pipe buffer — reads usually come back
    // in one shot.
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match master.read(&mut buf).await {
            Ok(0) => break, // EOF — child closed the slave side.
            Ok(n) => {
                let chunk = Bytes::copy_from_slice(&buf[..n]);
                tab.ring.lock().await.push(&buf[..n]);
                // If every subscriber has disconnected `send` returns Err,
                // which is fine — we just keep reading into the ring so the
                // next attacher gets replay.
                let _ = tab.tx.send(chunk);
            }
            Err(e) => {
                tracing::debug!("pty read error on tab {tab_id}: {e}");
                break;
            }
        }
    }
    // Child is done. Reap, announce, drop.
    let (code, signal) = match try_reap(tab.pid) {
        Some(v) => v,
        None => {
            // Brief retry: child may still be a zombie for a few ms after
            // EOF. If it's still not reaped, fall through with Nones.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            try_reap(tab.pid).unwrap_or((None, None))
        }
    };
    let evt = ExitEvent {
        tab_id: tab_id.clone(),
        code,
        signal,
    };
    // Fire the EXIT event as a JSON OP_EXIT frame broadcast to all
    // subscribers. Encode once, wrap as a Bytes.
    if let Ok(json) = serde_json::to_vec(&evt) {
        // We reuse the OUTPUT channel to carry the EXIT frame; subscribers
        // know to distinguish OUTPUT vs EXIT by the framed stream they sit
        // on — but since we fan out on a raw broadcast<Bytes>, the
        // connection-level task must tag it. Simpler: we use a sentinel
        // tagged vec. Prefix a single 0xFE byte → "synthetic exit event".
        let mut tagged = Vec::with_capacity(1 + json.len());
        tagged.push(0xFE);
        tagged.extend_from_slice(&json);
        let _ = tab.tx.send(Bytes::from(tagged));
    }
    tabs.remove(&tab_id).await;
}

/// Handle one accepted connection. Reads frames, routes them against the
/// shared Tabs map, writes OUTPUT/events back on the same socket.
pub async fn handle_connection(tabs: Arc<Tabs>, stream: UnixStream) -> io::Result<()> {
    let (mut reader, writer) = stream.into_split();
    let writer = Arc::new(Mutex::new(writer));

    // Per-connection state: which tab (if any) we've opened, and a handle
    // to the broadcast-forward task so we can cancel it on detach.
    let mut current_tab: Option<(String, Arc<Tab>, tokio::task::JoinHandle<()>)> = None;

    loop {
        let frame = match read_frame(&mut reader).await {
            Ok(Some(f)) => f,
            Ok(None) => {
                tracing::debug!("client closed connection");
                break;
            }
            Err(e) => {
                tracing::warn!("frame read error: {e}");
                break;
            }
        };
        let (ty, payload) = frame;
        match ty {
            OP_PING => {
                let _ = write_frame(&mut *writer.lock().await, OP_PONG, b"").await;
            }
            OP_OPEN => {
                // Detach any prior tab on this connection first.
                if let Some((_, _, h)) = current_tab.take() {
                    h.abort();
                }
                let req: OpenRequest = match serde_json::from_slice(&payload) {
                    Ok(r) => r,
                    Err(e) => {
                        send_error(&writer, "bad_open", &e.to_string()).await;
                        continue;
                    }
                };
                match tabs.open_or_attach(&req).await {
                    Ok((tab, existed)) => {
                        // Replay buffer: send tail *before* wiring up the
                        // broadcast, so subscribers never see a gap or
                        // duplicate between replay and live output. (The
                        // broadcast channel itself only delivers frames
                        // produced *after* the subscribe call.)
                        let replay_n = (req.replay_bytes as usize).min(RING_BYTES);
                        let replay = if replay_n > 0 {
                            tab.ring.lock().await.tail(replay_n)
                        } else {
                            Vec::new()
                        };
                        let rx = tab.tx.subscribe();
                        let _ = write_json_frame(
                            &mut *writer.lock().await,
                            OP_OPENED,
                            &OpenedResponse {
                                tab_id: req.tab_id.clone(),
                                pid: tab.pid,
                                existed,
                            },
                        )
                        .await;
                        if !replay.is_empty() {
                            let _ =
                                write_frame(&mut *writer.lock().await, OP_OUTPUT, &replay).await;
                        }
                        let h = tokio::spawn(broadcast_forward(writer.clone(), rx));
                        current_tab = Some((req.tab_id.clone(), tab, h));
                    }
                    Err(e) => {
                        send_error(&writer, "spawn_failed", &e.to_string()).await;
                    }
                }
            }
            OP_INPUT => {
                if let Some((_, tab, _)) = current_tab.as_ref() {
                    let mut stdin = tab.stdin.lock().await;
                    if let Err(e) = stdin.write_all(&payload).await {
                        tracing::debug!("stdin write failed: {e}");
                    }
                    let _ = stdin.flush().await;
                }
            }
            OP_RESIZE => {
                if let Some((_, tab, _)) = current_tab.as_ref() {
                    if let Some((c, r)) = decode_resize(&payload) {
                        if let Err(e) = resize_pty(tab.master_fd, c, r) {
                            tracing::debug!("resize failed: {e}");
                        }
                    }
                }
            }
            OP_DETACH => {
                if let Some((_, _, h)) = current_tab.take() {
                    h.abort();
                }
            }
            OP_KILL => {
                if let Some((id, tab, h)) = current_tab.take() {
                    h.abort();
                    tabs.remove(&id).await;
                    kill_tree(tab.pid).await;
                }
            }
            OP_LIST => {
                let list = tabs.list().await;
                let _ = write_json_frame(&mut *writer.lock().await, OP_TABS, &list).await;
            }
            other => {
                send_error(&writer, "unknown_op", &format!("opcode 0x{other:02x}")).await;
            }
        }
    }

    if let Some((_, _, h)) = current_tab.take() {
        h.abort();
    }
    Ok(())
}

async fn broadcast_forward(
    writer: Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
    mut rx: broadcast::Receiver<Bytes>,
) {
    loop {
        match rx.recv().await {
            Ok(chunk) => {
                // 0xFE-prefixed synthetic = EXIT event; forward as OP_EXIT
                // so the client knows the child died.
                if !chunk.is_empty() && chunk[0] == 0xFE {
                    let _ = write_frame(&mut *writer.lock().await, OP_EXIT, &chunk[1..]).await;
                    // After EXIT, the PTY reader has already dropped the
                    // tab; break so the connection can no-op INPUT/RESIZE
                    // frames until the client either DETACHes or OPENs a
                    // replacement.
                    break;
                }
                if write_frame(&mut *writer.lock().await, OP_OUTPUT, &chunk)
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::debug!("subscriber lagged {n} frames");
                // Continue: the ring replay already patched us up on
                // attach; losing a few live frames is acceptable.
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

async fn send_error(
    writer: &Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
    code: &str,
    message: &str,
) {
    let payload = ErrorPayload {
        code: code.into(),
        message: message.into(),
    };
    let _ = write_json_frame(&mut *writer.lock().await, OP_ERROR, &payload).await;
}

/// How long the startup probe waits for an existing socket to accept.
const LIVE_AGENT_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// What a startup connection attempt says about the socket path.
#[derive(Debug, PartialEq, Eq)]
enum SocketProbe {
    /// A listener accepted: another agent is running.
    Live,
    /// Nothing answers (no file, or the connection was refused): stale.
    Stale,
    /// The connection neither completed nor failed within the deadline, or
    /// the kernel said it would block: a listener exists but is not
    /// accepting, so the socket is not provably stale.
    Busy,
}

/// Classify a connection attempt to an existing socket, bounded by
/// `deadline` so a listener that stopped accepting cannot hold startup.
async fn probe_socket<F>(connect: F, deadline: Duration) -> SocketProbe
where
    F: std::future::Future<Output = io::Result<()>>,
{
    match tokio::time::timeout(deadline, connect).await {
        Ok(Ok(())) => SocketProbe::Live,
        Ok(Err(e)) if e.kind() == io::ErrorKind::WouldBlock => SocketProbe::Busy,
        Ok(Err(_)) => SocketProbe::Stale,
        Err(_) => SocketProbe::Busy,
    }
}

/// Top-level: bind the Unix socket and accept forever. Returns only on bind
/// error or explicit shutdown (SIGTERM from docker-init).
pub async fn run(socket_path: &str) -> io::Result<()> {
    // Refuse to displace a live agent. Removing the socket file and binding
    // a new one would succeed even with another instance running, silently
    // orphaning every tab it owns — a second `temps-pty-agent` started by a
    // careless bootstrap must be a no-op, not an outage. Only a socket that
    // nothing answers on is stale and safe to clear.
    let probe = probe_socket(
        async { UnixStream::connect(socket_path).await.map(drop) },
        LIVE_AGENT_PROBE_TIMEOUT,
    )
    .await;
    match probe {
        SocketProbe::Stale => {}
        SocketProbe::Live => {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("a temps-pty-agent is already listening on {socket_path}"),
            ));
        }
        SocketProbe::Busy => {
            // Something owns the socket but is not accepting right now (a
            // full backlog, a stopped process). It is not provably stale, so
            // it must not be displaced.
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "the socket {socket_path} did not accept a probe connection within {}s; \
                     refusing to displace what may be a live temps-pty-agent",
                    LIVE_AGENT_PROBE_TIMEOUT.as_secs()
                ),
            ));
        }
    }
    let _ = std::fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)?;
    // 0600 — only the `temps` user should be able to connect. docker exec
    // --user temps is the only path in from the host.
    let _ = std::fs::set_permissions(
        socket_path,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    );
    tracing::info!("temps-pty-agent listening on {socket_path}");
    let tabs = Tabs::new();
    loop {
        let (stream, _) = listener.accept().await?;
        let tabs = tabs.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(tabs, stream).await {
                tracing::warn!("connection handler error: {e}");
            }
        });
    }
}

/// `PATH` every terminal shell gets: the sandbox user's tool directories
/// first, then the system ones. Pinned rather than inherited because the
/// agent's own `PATH` can be the minimal one its supervisor started with.
const SHELL_PATH: &str = "/home/temps/.local/bin:/home/temps/.bun/bin:/home/temps/.opencode/bin:/usr/local/bun/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// Variables the shell maintains itself; inheriting the agent's copies
/// would start every tab with the agent's working directory and depth.
const SHELL_OWNED_VARS: [&str; 4] = ["PWD", "OLDPWD", "SHLVL", "_"];

/// Environment for a tab opened without an explicit `env`.
///
/// Starts from the agent's own environment, which is the sandbox
/// container's: the egress proxy (`HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY`
/// and their lowercase forms) and the variables the sandbox owner set.
/// Dropping those leaves a terminal where `curl`, `git clone` and
/// `npm install` cannot reach the internet, because a managed sandbox
/// network only routes outbound traffic through the proxy. `TERM` and
/// `PATH` are then pinned, and `HOME` defaults to the sandbox user's.
fn default_shell_env(
    inherited: impl IntoIterator<Item = (String, String)>,
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = inherited
        .into_iter()
        .filter(|(key, _)| {
            key != "TERM" && key != "PATH" && !SHELL_OWNED_VARS.contains(&key.as_str())
        })
        .collect();
    if !env
        .iter()
        .any(|(key, value)| key == "HOME" && !value.is_empty())
    {
        env.retain(|(key, _)| key != "HOME");
        env.push(("HOME".into(), "/home/temps".into()));
    }
    env.push(("TERM".into(), "xterm-256color".into()));
    env.push(("PATH".into(), SHELL_PATH.into()));
    env
}

// PermissionsExt import sugar so the single use above reads clean.
#[allow(unused_imports)]
use std::os::unix::fs::PermissionsExt;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_refuses_to_displace_a_live_agent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("agent.sock");
        let _live = tokio::net::UnixListener::bind(&path).expect("bind");

        let err = run(&path.to_string_lossy())
            .await
            .expect_err("a second agent must not start");
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
        // The live listener's socket file must be untouched.
        assert!(path.exists());
    }

    #[tokio::test]
    async fn a_probe_that_never_completes_is_busy_not_a_hang() {
        let started = std::time::Instant::now();
        let probe = probe_socket(
            std::future::pending::<io::Result<()>>(),
            Duration::from_millis(50),
        )
        .await;
        assert_eq!(probe, SocketProbe::Busy);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn probe_classifies_accept_refusal_and_would_block() {
        let d = Duration::from_secs(5);
        assert_eq!(probe_socket(async { Ok(()) }, d).await, SocketProbe::Live);
        for kind in [io::ErrorKind::NotFound, io::ErrorKind::ConnectionRefused] {
            assert_eq!(
                probe_socket(async move { Err(io::Error::from(kind)) }, d).await,
                SocketProbe::Stale,
                "{kind:?}"
            );
        }
        assert_eq!(
            probe_socket(async { Err(io::Error::from(io::ErrorKind::WouldBlock)) }, d).await,
            SocketProbe::Busy
        );
    }

    #[tokio::test]
    async fn probe_treats_a_dead_agents_leftover_socket_as_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("agent.sock");
        // Bind then drop: the file remains but nothing listens on it.
        drop(std::os::unix::net::UnixListener::bind(&path).expect("bind"));
        assert!(path.exists());

        // Other tests in this process spawn children, and a child forked
        // just before `exec` still holds a copy of the closed listener's fd
        // (CLOEXEC only takes effect at exec), so the socket keeps answering
        // until that window closes. Wait it out rather than assume the close
        // is instantaneous.
        let mut probe = SocketProbe::Live;
        for _ in 0..100 {
            probe = probe_socket(
                async { UnixStream::connect(&path).await.map(drop) },
                LIVE_AGENT_PROBE_TIMEOUT,
            )
            .await;
            if probe == SocketProbe::Stale {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(probe, SocketProbe::Stale);
    }

    /// A listener that never accepts, with its backlog full: on Linux a
    /// blocking `connect(2)` to it waits for the peer forever. Startup must
    /// still return, and must leave the occupied socket alone.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn run_does_not_hang_on_a_listener_with_a_full_backlog() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("agent.sock");
        let wedged = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        // Shrink the backlog so the queue fills well inside the test's
        // file-descriptor budget (std's default is the system maximum).
        // SAFETY: `listen` on a descriptor `wedged` owns for this whole test.
        let rc = unsafe { libc::listen(std::os::fd::AsRawFd::as_raw_fd(&wedged), 1) };
        assert_eq!(rc, 0, "listen: {}", io::Error::last_os_error());

        // Fill the accept queue; the kernel pushes back once it is full.
        let mut held = Vec::new();
        let mut saturated = false;
        for _ in 0..64 {
            match tokio::time::timeout(Duration::from_millis(200), UnixStream::connect(&path)).await
            {
                Ok(Ok(stream)) => held.push(stream),
                Ok(Err(e)) if e.kind() == io::ErrorKind::WouldBlock => {
                    saturated = true;
                    break;
                }
                Ok(Err(e)) => panic!(
                    "unexpected connect error after {} connects: {e}",
                    held.len()
                ),
                Err(_) => {
                    saturated = true;
                    break;
                }
            }
        }
        assert!(
            saturated,
            "the accept queue never filled after {} connects",
            held.len()
        );

        let result = tokio::time::timeout(Duration::from_secs(20), run(&path.to_string_lossy()))
            .await
            .expect("startup must not hang on a listener that stopped accepting");
        let err = result.expect_err("a wedged listener must not be displaced");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err}");
        assert!(path.exists(), "the occupied socket file must be untouched");
    }

    #[test]
    fn ring_push_and_tail_under_capacity() {
        let mut r = Ring::new();
        r.push(b"hello");
        assert_eq!(r.tail(5), b"hello");
        assert_eq!(r.tail(100), b"hello"); // asking for more than present
    }

    #[test]
    fn ring_wraps_and_returns_tail() {
        let mut r = Ring::new();
        let big = vec![b'a'; RING_BYTES + 128];
        r.push(&big);
        let tail = r.tail(10);
        assert_eq!(tail.len(), 10);
        assert!(tail.iter().all(|&b| b == b'a'));
    }

    fn pairs(entries: &[(&str, &str)]) -> Vec<(String, String)> {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn lookup<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
        env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    #[test]
    fn default_shell_env_keeps_the_sandbox_egress_proxy_and_owner_vars() {
        let env = default_shell_env(pairs(&[
            ("HTTP_PROXY", "http://temps-sandbox-egress-proxy:3128"),
            ("https_proxy", "http://temps-sandbox-egress-proxy:3128"),
            ("NO_PROXY", "localhost,127.0.0.1"),
            ("DATABASE_URL", "postgres://db/app"),
            ("HOME", "/home/temps"),
        ]));
        assert_eq!(
            lookup(&env, "HTTP_PROXY"),
            Some("http://temps-sandbox-egress-proxy:3128")
        );
        assert_eq!(
            lookup(&env, "https_proxy"),
            Some("http://temps-sandbox-egress-proxy:3128")
        );
        assert_eq!(lookup(&env, "NO_PROXY"), Some("localhost,127.0.0.1"));
        assert_eq!(lookup(&env, "DATABASE_URL"), Some("postgres://db/app"));
        assert_eq!(lookup(&env, "HOME"), Some("/home/temps"));
    }

    #[test]
    fn default_shell_env_pins_term_and_path_over_the_inherited_ones() {
        let env = default_shell_env(pairs(&[
            ("PATH", "/usr/bin"),
            ("TERM", "dumb"),
            ("PWD", "/"),
            ("SHLVL", "3"),
        ]));
        assert_eq!(lookup(&env, "PATH"), Some(SHELL_PATH));
        assert_eq!(lookup(&env, "TERM"), Some("xterm-256color"));
        assert_eq!(env.iter().filter(|(k, _)| k == "PATH").count(), 1);
        assert_eq!(env.iter().filter(|(k, _)| k == "TERM").count(), 1);
        assert_eq!(lookup(&env, "PWD"), None);
        assert_eq!(lookup(&env, "SHLVL"), None);
    }

    #[test]
    fn default_shell_env_defaults_a_missing_or_empty_home() {
        let missing = default_shell_env(Vec::new());
        assert_eq!(lookup(&missing, "HOME"), Some("/home/temps"));

        let empty = default_shell_env(pairs(&[("HOME", "")]));
        assert_eq!(lookup(&empty, "HOME"), Some("/home/temps"));
        assert_eq!(empty.iter().filter(|(k, _)| k == "HOME").count(), 1);
    }
}
