// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::{
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
    sync::Arc,
};
use temps_sandbox_runtime::{io, Daemon, Error, Response};
use tokio::{net::UnixListener, sync::Semaphore, task::JoinSet};

// Local-only transport. The socket directory is private to the sandbox uid;
// possession of Docker exec access is already equivalent to sandbox execution.
#[tokio::main]
async fn main() -> Result<(), Error> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "serve".into());
    if command == "check" {
        let socket = PathBuf::from(
            args.next()
                .unwrap_or_else(|| "/run/temps-runtime/agent.sock".into()),
        );
        match temps_sandbox_runtime::check_agent_runtime(&socket).await {
            Ok(()) => return Ok(()),
            Err(
                Error::Io {
                    operation: "connect retained runtime socket",
                    ..
                }
                | Error::Timeout { .. },
            ) => std::process::exit(2),
            Err(_) => std::process::exit(3),
        }
    }
    if command == "exec" {
        let program = args.next().ok_or_else(|| {
            io(
                "exec requires a program",
                std::io::Error::from(std::io::ErrorKind::InvalidInput),
            )
        })?;
        let code = temps_sandbox_runtime::exec_client(
            std::path::Path::new("/run/temps-runtime/control.sock"),
            program.into(),
            args.collect(),
        )
        .await?;
        std::process::exit(code);
    }
    if command == "exec-bounded" {
        let limit = args
            .next()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|limit| *limit > 0)
            .ok_or_else(|| {
                io(
                    "exec-bounded requires a positive byte limit",
                    std::io::Error::from(std::io::ErrorKind::InvalidInput),
                )
            })?;
        let program = args.next().ok_or_else(|| {
            io(
                "exec-bounded requires a program",
                std::io::Error::from(std::io::ErrorKind::InvalidInput),
            )
        })?;
        let code = temps_sandbox_runtime::exec_client_bounded(
            std::path::Path::new("/run/temps-runtime/control.sock"),
            program.into(),
            args.collect(),
            limit,
        )
        .await?;
        std::process::exit(code);
    }
    if command == "connect" {
        let socket = PathBuf::from(
            args.next()
                .unwrap_or_else(|| "/run/temps-runtime/agent.sock".into()),
        );
        return temps_sandbox_runtime::connect_client(&socket).await;
    }
    if command == "recover-harness" {
        let epoch = args
            .next()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| {
                io(
                    "parse recover-harness backend epoch",
                    std::io::Error::from(std::io::ErrorKind::InvalidInput),
                )
            })?;
        let payload = serde_json::to_vec(&temps_sandbox_runtime::Request {
            version: temps_sandbox_runtime::PROTOCOL_VERSION,
            operation: temps_sandbox_runtime::Operation::RecoverHarness { epoch },
        })?;
        let response = temps_sandbox_runtime::request(
            std::path::Path::new("/run/temps-runtime/control.sock"),
            &payload,
        )
        .await?;
        match serde_json::from_slice::<Response>(&response)? {
            Response::Deleted => return Ok(()),
            Response::Error { detail, .. } => {
                return Err(temps_sandbox_runtime::Error::Recovery { epoch, detail })
            }
            _ => {
                return Err(temps_sandbox_runtime::Error::Recovery {
                    epoch,
                    detail: "unexpected daemon recovery response".into(),
                })
            }
        }
    }
    let socket = PathBuf::from(
        args.next()
            .unwrap_or_else(|| "/run/temps-runtime/control.sock".into()),
    );
    if command == "request" {
        let payload = args
            .next()
            .unwrap_or_else(|| r#"{"version":1,"operation":{"type":"health"}}"#.into());
        let bytes = temps_sandbox_runtime::request(&socket, payload.as_bytes()).await?;
        let response: Response = serde_json::from_slice(&bytes)?;
        println!("{}", String::from_utf8_lossy(&bytes).trim_end());
        if matches!(response, Response::Error { .. }) {
            std::process::exit(1);
        }
        return Ok(());
    }
    if command != "serve" {
        return Err(io(
            "parse arguments (expected serve, request, exec, exec-bounded, connect, or check)",
            std::io::Error::from(std::io::ErrorKind::InvalidInput),
        ));
    }
    let root = PathBuf::from(
        args.next()
            .unwrap_or_else(|| "/home/temps/workspace".into()),
    );
    let parent = socket.parent().ok_or_else(|| {
        io(
            "resolve socket directory",
            std::io::Error::from(std::io::ErrorKind::InvalidInput),
        )
    })?;
    // Never unlink a pre-existing socket: it may belong to another live daemon.
    // Container /run is ephemeral, so a replacement starts with a clean socket.
    let metadata = std::fs::symlink_metadata(parent)
        .map_err(|source| io("inspect socket directory", source))?;
    if !metadata.is_dir() || metadata.mode() & 0o077 != 0 {
        return Err(io(
            "socket directory must be private (0700), not a symlink",
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        ));
    }
    let daemon = Arc::new(Daemon::new(&root)?);
    let listener = UnixListener::bind(&socket)
        .map_err(|source| io("bind socket (already running or stale socket)", source))?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))
        .map_err(|source| io("restrict socket", source))?;
    let agent_socket = socket.with_file_name("agent.sock");
    let agent_listener = match UnixListener::bind(&agent_socket) {
        Ok(listener) => listener,
        Err(source) => {
            let _ = std::fs::remove_file(&socket);
            return Err(io(
                "bind retained runtime socket (already running or stale socket)",
                source,
            ));
        }
    };
    if let Err(source) =
        std::fs::set_permissions(&agent_socket, std::fs::Permissions::from_mode(0o600))
    {
        let _ = std::fs::remove_file(&agent_socket);
        let _ = std::fs::remove_file(&socket);
        return Err(io("restrict retained runtime socket", source));
    }
    let expected_uid = metadata.uid();
    let slots = Arc::new(Semaphore::new(16));
    let agent_slots = Arc::new(Semaphore::new(8));
    let mut connections = JoinSet::new();
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|source| io("register termination signal", source))?;
    loop {
        tokio::select! {
            _ = terminate.recv() => break,
            _ = tokio::signal::ctrl_c() => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => {},
            accepted = listener.accept() => {
                let (stream, _) = accepted.map_err(|source| io("accept socket", source))?;
                let peer_uid = stream.peer_cred().map_err(|source| io("authenticate control socket peer", source))?.uid();
                if peer_uid != expected_uid && peer_uid != 0 {
                    drop(stream);
                    continue;
                }
                let Ok(permit) = slots.clone().try_acquire_owned() else { drop(stream); continue; };
                let daemon = daemon.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    if let Err(error) = temps_sandbox_runtime::serve_connection(stream, daemon).await {
                        // Do not log requests, arguments, or captured command output.
                        eprintln!("{}", serde_json::json!({"level":"warn","event":"runtime_connection_failed","detail":error.to_string()}));
                    }
                });
            }
            accepted = agent_listener.accept() => {
                let (stream, _) = accepted.map_err(|source| io("accept retained runtime socket", source))?;
                if stream.peer_cred().map_err(|source| io("authenticate retained runtime socket peer", source))?.uid() != expected_uid {
                    drop(stream);
                    continue;
                }
                let Ok(permit) = agent_slots.clone().try_acquire_owned() else { drop(stream); continue; };
                let daemon = daemon.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    if let Err(error) = temps_sandbox_runtime::serve_agent_connection(stream, daemon).await {
                        eprintln!("{}", serde_json::json!({"level":"warn","event":"retained_runtime_connection_failed","detail":error.to_string()}));
                    }
                });
            }
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    daemon.shutdown().await?;
    drop(listener);
    drop(agent_listener);
    std::fs::remove_file(&socket).map_err(|source| io("remove owned socket", source))?;
    std::fs::remove_file(&agent_socket)
        .map_err(|source| io("remove owned retained runtime socket", source))?;
    Ok(())
}
