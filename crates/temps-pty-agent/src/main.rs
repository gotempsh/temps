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
    let args = Args::parse();
    init_logging(args.relay || args.probe);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .thread_name("pty-agent")
        .build()?;

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

    rt.block_on(async move { temps_pty_agent::server::run(&args.socket).await })
}
