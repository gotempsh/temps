// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `temps-git-credential-helper` — the binary git invokes via
//! `credential.helper`.
//!
//! Trust model: this binary runs as the same uid as user code (uid
//! 1000). It MUST hold no secrets. Its only job is to forward git's
//! request to the daemon and forward the daemon's response back to git.
//! Reading the helper binary off disk leaks nothing.
//!
//! Lifecycle: short-lived, one process per git operation. Spawned by
//! git, reads stdin, writes stdout, exits.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use temps_git_credential::helper_protocol::{parse_request, render_get_response};
use temps_git_credential::ipc::{IpcRequest, IpcResponse};
use temps_git_credential::{Operation, DEFAULT_SOCKET_PATH};

fn main() {
    // First arg is the action: `get`, `store`, or `erase`. Default to
    // `get` if missing — same default git uses internally.
    let action = std::env::args().nth(1).unwrap_or_else(|| "get".to_string());

    // Read all of stdin so the protocol parser can scan in one pass.
    // Helper requests are small (tens of bytes), no streaming concern.
    let mut input = String::new();
    if let Err(e) = io::stdin().read_to_string(&mut input) {
        eprintln!("temps-git-credential-helper: failed to read stdin: {e}");
        std::process::exit(1);
    }

    match action.as_str() {
        "get" => run_get(&input),
        "store" => run_store_or_erase(IpcRequest::Store),
        "erase" => run_erase(&input),
        other => {
            eprintln!("temps-git-credential-helper: unknown action {other:?}");
            std::process::exit(1);
        }
    }
}

fn run_get(input: &str) {
    let req = match parse_request(input) {
        Ok(r) => r,
        Err(e) => {
            // Print to stderr so the user sees what went wrong; exit 0
            // so git falls through to the next helper / prompts for
            // credentials. Aborting the helper here with non-zero would
            // make every `git clone` fail loudly even when a user just
            // wanted to override creds manually.
            eprintln!("temps-git-credential-helper: malformed request: {e}");
            return;
        }
    };

    // Operation hint: env var override, else `Fetch` by default.
    let operation = std::env::var("TEMPS_GIT_CREDENTIAL_OP")
        .ok()
        .and_then(|s| match s.as_str() {
            "push" => Some(Operation::Push),
            "fetch" => Some(Operation::Fetch),
            _ => None,
        })
        .unwrap_or_else(Operation::default_safe);

    let ipc_req = IpcRequest::Get {
        host: req.host.clone(),
        owner: req.owner.clone(),
        repo: req.repo.clone(),
        operation,
    };

    match send_to_daemon(&ipc_req) {
        Ok(IpcResponse::Credential { username, password }) => {
            let out = render_get_response(&req, &username, &password);
            // Use raw write_all rather than print! so an interrupted
            // pipe doesn't panic — git may have already given up.
            if let Err(e) = io::stdout().write_all(out.as_bytes()) {
                eprintln!("temps-git-credential-helper: stdout write failed: {e}");
                std::process::exit(1);
            }
        }
        Ok(IpcResponse::Ok) => {
            // Nothing to return — let git fall through.
        }
        Ok(IpcResponse::Refused { reason }) => {
            // Make the refusal visible. Git will then fail the operation
            // with its own "Authentication failed" message; without our
            // stderr the user would never know *why*.
            eprintln!("temps-git-credential-helper: refused by daemon: {reason}");
        }
        Err(e) => {
            eprintln!("temps-git-credential-helper: {}", explain_ipc_error(&e));
        }
    }
}

fn run_store_or_erase(req: IpcRequest) {
    // Store: we don't track caller-supplied creds (would let user code
    // inject tokens into our flow). Erase without a parsed body is also
    // a no-op against the daemon — handled here as "ack and forget".
    if let Err(e) = send_to_daemon(&req) {
        eprintln!("temps-git-credential-helper: {}", explain_ipc_error(&e));
    }
}

fn run_erase(input: &str) {
    let req = match parse_request(input) {
        Ok(r) => r,
        Err(_) => {
            // Erase without a valid body is harmless — drop silently.
            return;
        }
    };
    let ipc_req = IpcRequest::Erase {
        host: req.host,
        owner: req.owner,
        repo: req.repo,
    };
    if let Err(e) = send_to_daemon(&ipc_req) {
        eprintln!("temps-git-credential-helper: {}", explain_ipc_error(&e));
    }
}

fn socket_path() -> PathBuf {
    PathBuf::from(
        std::env::var("TEMPS_GIT_CREDENTIAL_SOCKET")
            .unwrap_or_else(|_| DEFAULT_SOCKET_PATH.to_string()),
    )
}

/// Map an IPC error into a message that points the user at the actual
/// cause. The default `daemon IPC failed: …` error was a debugging
/// black hole — every failure mode (missing daemon, wrong group
/// membership, dead daemon, malformed response) surfaced identically.
/// These messages are the user's only feedback path because git itself
/// only reports "Authentication failed" downstream.
fn explain_ipc_error(e: &io::Error) -> String {
    let path = socket_path();
    let path_str = path.display();
    match e.kind() {
        io::ErrorKind::NotFound => format!(
            "credential daemon socket not found at {path_str}. \
             The daemon was not started for this session — try reopening \
             the workspace, or check /tmp/temps-git-credential-daemon.log \
             inside the sandbox for startup errors."
        ),
        io::ErrorKind::PermissionDenied => format!(
            "permission denied connecting to {path_str}. \
             Your shell user is likely missing the 'git-users' group \
             membership that grants access to the daemon socket. \
             Run `id` inside the sandbox to verify; expected groups \
             include 'git-users'."
        ),
        io::ErrorKind::ConnectionRefused => format!(
            "connection refused at {path_str}. \
             A stale socket exists but no daemon is listening — the \
             daemon may have crashed. Check \
             /tmp/temps-git-credential-daemon.log inside the sandbox."
        ),
        _ => format!("daemon IPC failed: {e}"),
    }
}

fn send_to_daemon(req: &IpcRequest) -> io::Result<IpcResponse> {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path)?;
    let line = serde_json::to_string(req)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("serialize: {e}")))?;
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut buf = String::new();
    stream.read_to_string(&mut buf)?;
    let response: IpcResponse = serde_json::from_str(buf.trim()).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("deserialize daemon response: {e}"),
        )
    })?;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ENOENT must steer the user toward "daemon never started" rather
    /// than the generic IPC error — the original bug surfaced as opaque
    /// "Permission denied" when the socket dir was actually readable but
    /// the daemon never launched.
    #[test]
    fn explain_ipc_error_distinguishes_not_found() {
        let err = io::Error::from(io::ErrorKind::NotFound);
        let msg = explain_ipc_error(&err);
        assert!(msg.contains("not found"), "msg: {msg}");
        assert!(msg.contains("daemon was not started"), "msg: {msg}");
    }

    /// EACCES means the socket exists but the caller's group membership
    /// is wrong — a different fix path from "daemon down". The message
    /// must mention `git-users` or the user has nothing to grep for.
    #[test]
    fn explain_ipc_error_distinguishes_permission_denied() {
        let err = io::Error::from(io::ErrorKind::PermissionDenied);
        let msg = explain_ipc_error(&err);
        assert!(msg.contains("permission denied"), "msg: {msg}");
        assert!(msg.contains("git-users"), "msg: {msg}");
    }

    /// ECONNREFUSED only happens when a socket file exists but no
    /// process is bound to it — a crashed daemon. The message must
    /// point at the daemon log so the user can find the crash reason.
    #[test]
    fn explain_ipc_error_distinguishes_connection_refused() {
        let err = io::Error::from(io::ErrorKind::ConnectionRefused);
        let msg = explain_ipc_error(&err);
        assert!(msg.contains("connection refused"), "msg: {msg}");
        assert!(
            msg.contains("temps-git-credential-daemon.log"),
            "msg: {msg}"
        );
    }
}
