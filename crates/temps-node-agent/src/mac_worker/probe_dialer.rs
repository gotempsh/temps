// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runtime health probes on a Mac worker.
//!
//! A Mac worker's containers publish their ports on its mesh address, which
//! exists only in the Docker app's VM: macOS, where the agent runs, has no
//! route to it. So the agent opens each probe connection from the mesh
//! sidecar, which shares the VM's host network: `docker exec -i` runs
//! `socat` there, connected to the endpoint on one side and to the exec's
//! stdin/stdout on the other. The endpoint is the one the trusted inspector
//! derived from the container's own published port, never a caller's, and
//! it reaches `socat` as one argv element of fixed shape (no shell).

use super::{DockerEndpoint, SIDECAR_CONTAINER};
use async_trait::async_trait;
use std::io;
use std::net::{SocketAddr, SocketAddrV4};
use std::pin::Pin;
use std::process::Stdio;
use std::task::{Context, Poll};
use temps_runtime_health::{ProbeConnection, ProbeDialer};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};

/// What `socat -d -d` logs once both of its ends are open, that is once the
/// TCP connection to the endpoint is established. Its 1.8 series (the one
/// the sidecar's pinned Alpine base ships) logs it as
/// `N starting data transfer loop with FDs [0,1] and [5,5]`.
const SOCAT_CONNECTED: &[u8] = b"starting data transfer loop";
/// Most of `socat`'s log read while waiting for the connection: a handful
/// of short lines precede it.
const CONNECT_LOG_LIMIT: usize = 4096;
/// Backstop for an exec the agent no longer holds: `socat` gives up a
/// connect, or a connection idle this long. A probe's own deadline is
/// shorter, and dropping the connection kills the exec.
const SOCAT_TIMEOUT_SECS: u64 = 30;

/// Opens runtime health probe connections from the mesh sidecar in the
/// Docker app's VM (see the module documentation).
pub struct SidecarProbeDialer {
    endpoint: DockerEndpoint,
}

impl SidecarProbeDialer {
    pub fn new(endpoint: DockerEndpoint) -> Self {
        Self { endpoint }
    }
}

/// The Docker CLI arguments, after `--host`, that connect the exec's
/// stdin/stdout to `target` from inside the sidecar.
pub fn probe_exec_args(target: SocketAddrV4) -> Vec<String> {
    let timeout = SOCAT_TIMEOUT_SECS.to_string();
    vec![
        "exec".into(),
        "-i".into(),
        SIDECAR_CONTAINER.into(),
        "socat".into(),
        "-d".into(),
        "-d".into(),
        "-T".into(),
        timeout.clone(),
        "STDIO".into(),
        format!(
            "TCP4:{}:{},connect-timeout={timeout}",
            target.ip(),
            target.port()
        ),
    ]
}

#[async_trait]
impl ProbeDialer for SidecarProbeDialer {
    async fn connect(&self, endpoint: SocketAddr) -> io::Result<Box<dyn ProbeConnection>> {
        // Mesh addresses are IPv4, and the sidecar has no IPv6 route.
        let SocketAddr::V4(target) = endpoint else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("probe endpoint {endpoint} is not IPv4; a Mac worker's mesh sidecar only dials IPv4"),
            ));
        };
        let mut child = Command::new(&self.endpoint.cli)
            .arg("--host")
            .arg(&self.endpoint.host)
            .args(probe_exec_args(target))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "cannot run {} to probe {target} from the {SIDECAR_CONTAINER} sidecar: {error}",
                        self.endpoint.cli
                    ),
                )
            })?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(io::Error::other(format!(
                "the docker exec probing {target} has no stdio pipes"
            )));
        };
        let mut log = BufReader::new(stderr);
        // On an error the child is dropped here, which kills the exec.
        wait_until_connected(&mut log, target).await?;
        Ok(Box::new(SidecarConnection {
            _child: child,
            stdin,
            stdout,
            // Kept open, unread: socat's closing lines fit the pipe buffer,
            // and closing it could kill socat before it delivers a response.
            _log: log,
        }))
    }
}

/// Read `socat`'s log until it reports the connection established. It
/// exiting first (refused, unreachable, timed out, no sidecar) is a failed
/// connection.
async fn wait_until_connected<R: AsyncBufRead + Unpin>(
    log: &mut R,
    target: SocketAddrV4,
) -> io::Result<()> {
    let mut read = 0;
    let mut line = Vec::new();
    while read < CONNECT_LOG_LIMIT {
        line.clear();
        let count = (&mut *log)
            .take((CONNECT_LOG_LIMIT - read) as u64)
            .read_until(b'\n', &mut line)
            .await?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                format!("the {SIDECAR_CONTAINER} sidecar could not connect to {target}"),
            ));
        }
        read += count;
        if line
            .windows(SOCAT_CONNECTED.len())
            .any(|window| window == SOCAT_CONNECTED)
        {
            return Ok(());
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "the {SIDECAR_CONTAINER} sidecar logged over {CONNECT_LOG_LIMIT} bytes without connecting to {target}"
        ),
    ))
}

/// A probe connection: the exec's stdout and stdin. Dropping it kills the
/// exec, which closes the connection in the VM.
struct SidecarConnection {
    _child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    _log: BufReader<ChildStderr>,
}

impl AsyncRead for SidecarConnection {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stdout).poll_read(cx, buf)
    }
}

impl AsyncWrite for SidecarConnection {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stdin).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stdin).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stdin).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    fn target() -> SocketAddrV4 {
        "10.201.0.2:32768".parse().unwrap()
    }

    #[test]
    fn the_probe_runs_socat_in_the_sidecar_with_the_endpoint_as_one_argument() {
        assert_eq!(
            probe_exec_args(target()),
            [
                "exec",
                "-i",
                "temps-mac-mesh",
                "socat",
                "-d",
                "-d",
                "-T",
                "30",
                "STDIO",
                "TCP4:10.201.0.2:32768,connect-timeout=30",
            ]
        );
    }

    #[tokio::test]
    async fn connected_only_once_socat_says_its_transfer_loop_started() {
        let log = b"2026/11/06 08:22:16 socat[1041] N reading from and writing to stdio\n\
            2026/11/06 08:22:16 socat[1041] N opening connection to AF=2 10.201.0.2:32768\n\
            2026/11/06 08:22:16 socat[1041] N successfully connected to 10.201.0.2:32768\n\
            2026/11/06 08:22:16 socat[1041] N starting data transfer loop with FDs [0,1] and [5,5]\n";
        wait_until_connected(&mut &log[..], target()).await.unwrap();
    }

    #[tokio::test]
    async fn socat_exiting_before_connecting_is_a_refused_connection_naming_the_target() {
        let log = b"2026/11/06 08:22:16 socat[1047] W connect(5, AF=2 10.201.0.2:1, 16): Connection refused\n\
            2026/11/06 08:22:16 socat[1047] E TCP4:10.201.0.2:1: Connection refused\n\
            2026/11/06 08:22:16 socat[1047] N exit(1)\n";
        let error = wait_until_connected(&mut &log[..], target())
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
        assert!(error.to_string().contains("10.201.0.2:32768"), "{error}");
        assert!(error.to_string().contains("temps-mac-mesh"), "{error}");
    }

    #[tokio::test]
    async fn an_endless_log_without_the_connected_line_is_bounded() {
        let log = b"noise\n".repeat(CONNECT_LOG_LIMIT);
        let error = wait_until_connected(&mut &log[..], target())
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        // A line split by the limit is not read past it either.
        let log = vec![b'x'; CONNECT_LOG_LIMIT * 2];
        let error = wait_until_connected(&mut &log[..], target())
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn ipv6_endpoints_are_refused_without_running_docker() {
        let dialer = SidecarProbeDialer::new(DockerEndpoint {
            cli: "/nonexistent/docker".into(),
            host: "unix:///nonexistent.sock".into(),
        });
        let error = dialer
            .connect("[fd00::2]:32768".parse().unwrap())
            .await
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }

    /// A stand-in `docker` CLI: records its arguments, then acts like the
    /// sidecar's socat in `mode` (connect and echo, or refuse).
    #[cfg(unix)]
    fn fake_docker(dir: &std::path::Path, mode: &str) -> DockerEndpoint {
        use std::os::unix::fs::PermissionsExt;
        let args = dir.join("args");
        let script = dir.join("docker");
        let body = match mode {
            "echo" => {
                "echo 'N starting data transfer loop with FDs [0,1] and [5,5]' >&2\nexec cat\n"
            }
            _ => "echo 'E TCP4:10.201.0.2:32768: Connection refused' >&2\nexit 1\n",
        };
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n{body}",
                args.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        DockerEndpoint {
            cli: script.display().to_string(),
            host: "unix:///var/run/vm-docker.sock".into(),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_connection_carries_bytes_through_the_exec_on_the_named_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let dialer = SidecarProbeDialer::new(fake_docker(dir.path(), "echo"));
        let mut connection = dialer.connect(target().into()).await.unwrap();
        connection.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
        connection.flush().await.unwrap();
        let mut echoed = [0u8; 16];
        connection.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"GET / HTTP/1.1\r\n");
        let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
        let mut expected = vec![
            "--host".to_string(),
            "unix:///var/run/vm-docker.sock".to_string(),
        ];
        expected.extend(probe_exec_args(target()));
        assert_eq!(args.lines().collect::<Vec<_>>(), expected);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_refused_connection_fails_the_dial() {
        let dir = tempfile::tempdir().unwrap();
        let dialer = SidecarProbeDialer::new(fake_docker(dir.path(), "refuse"));
        let error = dialer.connect(target().into()).await.err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
    }
}
