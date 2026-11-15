// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Entrypoint for the agent binary.
//!
//! Default mode parses `--socket` and hands off to `server::run`; the process
//! lives for the sandbox's lifetime and a supervisor respawns it on crash.
//!
//! `--relay` and `--probe` are the client-side modes used by microVM
//! backends, whose guests don't ship `socat`: `--relay` pipes stdin/stdout to
//! the agent's socket, `--probe` only checks that an agent is listening.

use clap::Parser;
use tracing_subscriber::fmt::writer::BoxMakeWriter;

#[derive(Parser, Debug)]
#[command(name = "temps-pty-agent", about = "In-sandbox PTY multiplexer")]
struct Args {
    /// Unix socket path to bind (server mode) or connect to (`--relay`,
    /// `--probe`). Default matches what the host handler connects to.
    #[arg(long, default_value = temps_pty_agent::DEFAULT_SOCKET_PATH)]
    socket: String,

    /// Relay stdin/stdout to a running agent's socket instead of serving.
    /// Exits when either side closes; the agent and its tabs keep running.
    #[arg(long, conflicts_with = "probe")]
    relay: bool,

    /// Exit 0 if an agent is accepting connections on the socket, 1
    /// otherwise. Does not start anything.
    #[arg(long)]
    probe: bool,

    /// Forward loopback HTTPS proxy connections to the host-owned service
    /// access socket. This mode carries no service credentials.
    #[arg(long, conflicts_with_all = ["relay", "probe", "socket"])]
    service_proxy: bool,
}

fn init_logging(client_mode: bool) {
    // Server mode logs to stdout as it always has. In `--relay` mode stdout
    // carries protocol bytes and must never be interleaved with log lines,
    // so client modes log to stderr.
    let writer = if client_mode {
        BoxMakeWriter::new(std::io::stderr)
    } else {
        BoxMakeWriter::new(std::io::stdout)
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .with_writer(writer)
        .init();
}

fn main() -> std::io::Result<()> {
    std::hint::black_box(&temps_pty_agent::BUILD_MARKER);
    let args = Args::parse();
    init_logging(args.relay || args.probe);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .thread_name("pty-agent")
        .build()?;

    if args.service_proxy {
        return rt
            .block_on(temps_pty_agent::service_proxy::run())
            .map_err(std::io::Error::other);
    }

    if args.probe {
        let code = match rt.block_on(temps_pty_agent::relay::probe(&args.socket)) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("temps-pty-agent: {e}");
                1
            }
        };
        std::process::exit(code);
    }

    if args.relay {
        let code = match rt.block_on(temps_pty_agent::relay::run(&args.socket)) {
            Ok(end) => {
                tracing::debug!("relay ended: {end:?}");
                0
            }
            Err(e) => {
                eprintln!("temps-pty-agent: {e}");
                1
            }
        };
        // Exit explicitly: tokio's stdin reader parks a blocking-pool thread
        // in `read(2)` that cannot be cancelled, and dropping the runtime
        // would wait on it for as long as the host keeps the pipe open.
        std::process::exit(code);
    }

    rt.block_on(run_server(
        &args.socket,
        std::path::Path::new("/run/temps-service-access"),
    ))
}

async fn run_server(socket: &str, access_mount: &std::path::Path) -> std::io::Result<()> {
    let server = temps_pty_agent::server::run(socket);
    if !access_mount.is_dir() {
        return server.await;
    }
    // The mount survives worker restarts even while its socket is recovered.
    // Dropping either server lifetime also drops the relay and its connections.
    let relay = async {
        loop {
            if let Err(error) = temps_pty_agent::service_proxy::run().await {
                tracing::warn!(%error, "Sandbox service relay stopped; retrying");
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    };
    tokio::select! { result = server => result, () = relay => Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn server_starts_mount_relay_and_cancellation_releases_listener() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("pty.sock");
        let socket_string = socket.to_str().unwrap().to_string();
        let mount = directory.path().to_path_buf();
        let task = tokio::spawn(async move { run_server(&socket_string, &mount).await });
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if tokio::net::TcpStream::connect(temps_pty_agent::service_proxy::LISTEN_ADDRESS)
                    .await
                    .is_ok()
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let listener =
            tokio::net::TcpListener::bind(temps_pty_agent::service_proxy::LISTEN_ADDRESS)
                .await
                .unwrap();
        drop(listener);
    }

    #[test]
    fn service_proxy_is_an_exclusive_mode_with_a_host_assigned_socket() {
        assert!(
            Args::try_parse_from(["temps-pty-agent", "--service-proxy"])
                .unwrap()
                .service_proxy
        );
        for conflicting in ["--relay", "--probe"] {
            assert!(
                Args::try_parse_from(["temps-pty-agent", "--service-proxy", conflicting]).is_err()
            );
        }
        assert!(Args::try_parse_from([
            "temps-pty-agent",
            "--service-proxy",
            "--socket",
            "/tmp/other.sock"
        ])
        .is_err());
    }
}
