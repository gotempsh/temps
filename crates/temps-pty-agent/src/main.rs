// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Entrypoint for the agent binary. Parses `--socket` and hands off to
//! `server::run`. The process lives for the container's lifetime; the
//! supervisor in sandbox-entrypoint.sh respawns it on crash.

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "temps-pty-agent", about = "In-sandbox PTY multiplexer")]
struct Args {
    /// Unix socket path to bind. Default matches what the host handler
    /// connects to.
    #[arg(long, default_value = temps_pty_agent::DEFAULT_SOCKET_PATH)]
    socket: String,
}

fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();
    let args = Args::parse();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .thread_name("pty-agent")
        .build()?;
    rt.block_on(async move { temps_pty_agent::server::run(&args.socket).await })
}
