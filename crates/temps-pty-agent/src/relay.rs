// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Relay mode: pipe this process's stdin/stdout to the agent's unix socket.
//!
//! The Docker backend reaches the agent with `docker exec … socat - UNIX-CONNECT:…`.
//! The microVM backends cannot assume `socat` exists in the guest (the default
//! image is `alpine:3.20`), so the agent binary carries the same relay itself:
//! `temps-pty-agent --relay` is the whole bridge, with no extra packages.
//!
//! The relay is byte-transparent. It never parses the ADR-008 framing, so the
//! frame-size limits stay enforced where they already are (the host's
//! `FrameCodec` and the agent's `read_frame`), and a relay bug cannot corrupt
//! the protocol — at worst it drops the connection.
//!
//! Lifetime: the relay ends when *either* direction ends. The socket closing
//! means the agent went away. Stdin closing means the host dropped its
//! attachment; the relay then half-closes the socket (as `socat` does) and
//! gives the agent a short grace period to flush replies already in flight
//! before exiting. Either way the agent keeps every tab alive, which is the
//! property that makes a terminal reattachable.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixStream;

/// Size of the relay's copy buffer, one per direction. Fixed, so a relay's
/// memory use does not depend on how fast either side produces data.
const RELAY_BUF_BYTES: usize = 16 * 1024;

/// Why a relay (or probe) could not reach the agent.
#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    #[error("cannot connect to the PTY agent socket {path}: {source}")]
    Connect { path: PathBuf, source: io::Error },

    #[error("relay to the PTY agent socket {path} failed: {source}")]
    Io { path: PathBuf, source: io::Error },
}

/// How long the relay keeps draining the agent's replies after its own input
/// closed. The agent ends the connection as soon as it sees our half-close, so
/// this only bounds an agent that has stopped responding.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Which side of the relay ended first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayEnd {
    /// The caller's input closed (the host detached).
    InputClosed,
    /// The agent closed the socket.
    AgentClosed,
}

/// Copy `reader` to `writer` until EOF, flushing after every chunk.
///
/// Flushing per chunk is what keeps an interactive session responsive: stdout
/// is a pipe to the host and a buffered keystroke echo would otherwise sit
/// behind the next 16 KiB of output.
async fn pipe<R, W>(mut reader: R, writer: &mut W) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; RELAY_BUF_BYTES];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        writer.write_all(&buf[..n]).await?;
        writer.flush().await?;
    }
}

/// Relay between an established agent connection and a caller's stdio.
///
/// Generic over the stdio halves so the behaviour is testable without a real
/// process boundary.
pub async fn relay_streams<I, O>(
    socket: UnixStream,
    input: I,
    output: O,
) -> Result<RelayEnd, io::Error>
where
    I: AsyncRead + Unpin,
    O: AsyncWrite + Unpin,
{
    let (socket_rx, mut socket_tx) = socket.into_split();
    let mut output = output;
    let to_agent = async {
        pipe(input, &mut socket_tx).await?;
        // Half-close: the agent sees EOF and ends the connection once it has
        // answered everything already sent.
        socket_tx.shutdown().await
    };
    let from_agent = pipe(socket_rx, &mut output);
    tokio::pin!(to_agent, from_agent);
    tokio::select! {
        result = &mut to_agent => {
            result?;
            // Best effort: replies to the last frames may still be in flight.
            let _ = tokio::time::timeout(DRAIN_GRACE, &mut from_agent).await;
            Ok(RelayEnd::InputClosed)
        }
        result = &mut from_agent => result.map(|()| RelayEnd::AgentClosed),
    }
}

/// Connect to the agent at `socket_path` and relay stdin/stdout to it.
pub async fn run(socket_path: &str) -> Result<RelayEnd, RelayError> {
    let path = Path::new(socket_path);
    let socket = UnixStream::connect(path)
        .await
        .map_err(|source| RelayError::Connect {
            path: path.to_path_buf(),
            source,
        })?;
    relay_streams(socket, tokio::io::stdin(), tokio::io::stdout())
        .await
        .map_err(|source| RelayError::Io {
            path: path.to_path_buf(),
            source,
        })
}

/// Check that an agent is accepting connections on `socket_path`.
///
/// Used by the host-side bootstrap to decide whether the agent needs
/// starting. Connecting is the only reliable test: a socket file left behind
/// by a crashed agent still exists, but nothing answers on it.
pub async fn probe(socket_path: &str) -> Result<(), RelayError> {
    let path = Path::new(socket_path);
    UnixStream::connect(path)
        .await
        .map(drop)
        .map_err(|source| RelayError::Connect {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    fn socket_in(dir: &tempfile::TempDir) -> String {
        dir.path().join("agent.sock").to_string_lossy().into_owned()
    }

    #[tokio::test]
    async fn relays_both_directions_until_input_closes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = socket_in(&dir);
        let listener = UnixListener::bind(&path).expect("bind");

        // Agent stand-in: echo whatever arrives, prefixed so direction is
        // unambiguous, and report what it received at EOF.
        let agent = tokio::spawn(async move {
            let (mut conn, _) = listener.accept().await.expect("accept");
            let mut received = Vec::new();
            let mut buf = [0u8; 64];
            while received.len() < 5 {
                let n = conn.read(&mut buf).await.expect("read");
                if n == 0 {
                    break;
                }
                received.extend_from_slice(&buf[..n]);
            }
            conn.write_all(b"agent:").await.expect("write");
            conn.write_all(&received).await.expect("write");
            // Hold the connection open: the relay must end because the
            // *input* closed, not because the agent hung up.
            let mut rest = Vec::new();
            let _ = conn.read_to_end(&mut rest).await;
            received
        });

        let socket = UnixStream::connect(&path).await.expect("connect");
        let (mut host_in_tx, host_in_rx) = tokio::io::duplex(1024);
        let (host_out_tx, mut host_out_rx) = tokio::io::duplex(1024);
        let relay = tokio::spawn(relay_streams(socket, host_in_rx, host_out_tx));

        host_in_tx.write_all(b"hello").await.expect("send");
        let mut echoed = vec![0u8; "agent:hello".len()];
        host_out_rx.read_exact(&mut echoed).await.expect("recv");
        assert_eq!(echoed, b"agent:hello");

        drop(host_in_tx);
        let end = relay.await.expect("join").expect("relay");
        assert_eq!(end, RelayEnd::InputClosed);
        assert_eq!(agent.await.expect("agent"), b"hello");
    }

    #[tokio::test]
    async fn replies_in_flight_when_input_closes_are_still_delivered() {
        // A one-shot caller (`printf PING | temps-pty-agent --relay`) closes
        // stdin right after writing. Tearing down at that moment would drop
        // the reply the agent is about to send.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = socket_in(&dir);
        let listener = UnixListener::bind(&path).expect("bind");
        tokio::spawn(async move {
            let (mut conn, _) = listener.accept().await.expect("accept");
            let mut request = Vec::new();
            conn.read_to_end(&mut request).await.expect("read request");
            // Answer only after the half-close was observed.
            conn.write_all(b"reply:").await.expect("write");
            conn.write_all(&request).await.expect("write");
        });

        let socket = UnixStream::connect(&path).await.expect("connect");
        let (mut host_in_tx, host_in_rx) = tokio::io::duplex(1024);
        let (host_out_tx, mut host_out_rx) = tokio::io::duplex(1024);
        let relay = tokio::spawn(relay_streams(socket, host_in_rx, host_out_tx));

        host_in_tx.write_all(b"ping").await.expect("send");
        drop(host_in_tx);

        let mut received = Vec::new();
        host_out_rx.read_to_end(&mut received).await.expect("read");
        assert_eq!(received, b"reply:ping");
        assert_eq!(
            relay.await.expect("join").expect("relay"),
            RelayEnd::InputClosed
        );
    }

    #[tokio::test]
    async fn ends_when_the_agent_closes_the_socket() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = socket_in(&dir);
        let listener = UnixListener::bind(&path).expect("bind");
        tokio::spawn(async move {
            let (conn, _) = listener.accept().await.expect("accept");
            drop(conn);
        });

        let socket = UnixStream::connect(&path).await.expect("connect");
        let (_host_in_tx, host_in_rx) = tokio::io::duplex(64);
        let (host_out_tx, _host_out_rx) = tokio::io::duplex(64);
        let end = relay_streams(socket, host_in_rx, host_out_tx)
            .await
            .expect("relay");
        assert_eq!(end, RelayEnd::AgentClosed);
    }

    #[tokio::test]
    async fn moves_more_than_one_buffer_without_loss() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = socket_in(&dir);
        let listener = UnixListener::bind(&path).expect("bind");
        let payload: Vec<u8> = (0..RELAY_BUF_BYTES * 5 + 7)
            .map(|i| (i % 251) as u8)
            .collect();
        let expected = payload.clone();

        let agent = tokio::spawn(async move {
            let (mut conn, _) = listener.accept().await.expect("accept");
            conn.write_all(&payload).await.expect("write");
            conn.shutdown().await.expect("shutdown");
        });

        let socket = UnixStream::connect(&path).await.expect("connect");
        let (_host_in_tx, host_in_rx) = tokio::io::duplex(64);
        let (host_out_tx, mut host_out_rx) = tokio::io::duplex(8 * 1024);
        let relay = tokio::spawn(relay_streams(socket, host_in_rx, host_out_tx));

        let mut received = Vec::new();
        host_out_rx.read_to_end(&mut received).await.expect("read");
        assert_eq!(received.len(), expected.len());
        assert!(received == expected, "relay corrupted the byte stream");
        assert_eq!(
            relay.await.expect("join").expect("relay"),
            RelayEnd::AgentClosed
        );
        agent.await.expect("agent");
    }

    #[tokio::test]
    async fn connect_error_names_the_socket_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = socket_in(&dir);
        let err = probe(&path).await.expect_err("nothing listens here");
        let message = err.to_string();
        assert!(message.contains(&path), "{message}");
        assert!(matches!(err, RelayError::Connect { .. }));
    }

    #[tokio::test]
    async fn probe_succeeds_against_a_live_listener_and_fails_on_a_stale_socket() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = socket_in(&dir);
        let listener = UnixListener::bind(&path).expect("bind");
        assert!(probe(&path).await.is_ok());
        // A crashed agent leaves its socket file behind. The file exists but
        // nothing answers, which is exactly the case probe must report.
        drop(listener);
        assert!(std::path::Path::new(&path).exists());
        // Other tests in this process spawn children, and a child forked
        // just before `exec` still holds a copy of the closed listener's fd
        // (CLOEXEC only takes effect at exec). Give that window a moment to
        // close instead of assuming the close is instantaneous.
        let mut refused = false;
        for _ in 0..100 {
            if probe(&path).await.is_err() {
                refused = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            refused,
            "probe kept succeeding against a stale socket {path}"
        );
    }
}
