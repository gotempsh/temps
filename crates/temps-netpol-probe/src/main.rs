// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `temps-netpol-probe serve <config.json>` | `sink <udp-port|-> <tcp-port,...>` |
//! `public-dns <port>`

use std::path::PathBuf;
use std::process::ExitCode;

use temps_netpol_probe::{public_dns, read_config, serve, sink, ProbeError};

const USAGE: &str =
    "temps-netpol-probe serve <config.json> | sink <udp-port|-> <tcp-port,...|-> | public-dns <port>";

fn run(args: Vec<String>) -> Result<(), ProbeError> {
    let usage = || ProbeError::Usage {
        usage: USAGE.to_string(),
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| ProbeError::Bind {
            addr: "tokio runtime".to_string(),
            reason: e.to_string(),
        })?;
    match args.get(1).map(String::as_str) {
        Some("serve") => {
            let path = PathBuf::from(args.get(2).ok_or_else(usage)?);
            let config = read_config(&path)?;
            runtime.block_on(serve(config))
        }
        Some("sink") => {
            let udp = match args.get(2).map(String::as_str) {
                None => return Err(usage()),
                Some("-") => None,
                Some(port) => Some(port.parse::<u16>().map_err(|_| usage())?),
            };
            let tcp = match args.get(3).map(String::as_str) {
                None | Some("-") => Vec::new(),
                Some(list) => list
                    .split(',')
                    .map(|p| p.parse::<u16>().map_err(|_| usage()))
                    .collect::<Result<Vec<_>, _>>()?,
            };
            runtime.block_on(sink(udp, tcp))
        }
        Some("public-dns") => {
            let port = args
                .get(2)
                .and_then(|p| p.parse::<u16>().ok())
                .ok_or_else(usage)?;
            runtime.block_on(public_dns(port))
        }
        _ => Err(usage()),
    }
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .json()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    match run(std::env::args().collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("temps-netpol-probe: {e}");
            ExitCode::FAILURE
        }
    }
}
