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
            eprintln!("temps-git-credential-helper: daemon IPC failed: {e}");
        }
    }
}

fn run_store_or_erase(req: IpcRequest) {
    // Store: we don't track caller-supplied creds (would let user code
    // inject tokens into our flow). Erase without a parsed body is also
    // a no-op against the daemon — handled here as "ack and forget".
    if let Err(e) = send_to_daemon(&req) {
        eprintln!("temps-git-credential-helper: daemon IPC failed: {e}");
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
        eprintln!("temps-git-credential-helper: daemon IPC failed: {e}");
    }
}

fn socket_path() -> PathBuf {
    PathBuf::from(
        std::env::var("TEMPS_GIT_CREDENTIAL_SOCKET")
            .unwrap_or_else(|_| DEFAULT_SOCKET_PATH.to_string()),
    )
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
