//! Shared types and logic for the in-sandbox git credential helper +
//! daemon pair.
//!
//! ## Architecture (one paragraph)
//! Git, running as user `temps` (uid 1000), invokes the credential
//! helper. The helper reads git's request from stdin (key=value lines),
//! connects to the daemon over a Unix socket at `/run/temps/git.sock`
//! (mode `0660`, owned by `temps-git:git-users`, with `temps` in the
//! `git-users` group), forwards the request as a JSON line, and writes
//! the daemon's JSON response back to stdout in git's expected
//! `key=value` format. The daemon, running as user `temps-git` (uid
//! 1001), holds the workspace's deployment token in its own memory
//! (loaded from `/etc/temps/credential-daemon.env` which is mode `0600`
//! and owned by `temps-git:temps-git`), validates the request against
//! the configured project + repo, and calls the control plane's
//! `/workspace/git-credential` endpoint to mint a per-operation scoped
//! token. The token comes back over the socket and goes straight to git
//! — never touches disk, never lands in any env var the user shell can
//! read.
//!
//! ## Why two uids
//! User code (uid 1000) cannot:
//! - Read `/etc/temps/credential-daemon.env` (different uid, mode 0600).
//! - `ptrace`/inspect the daemon process (different uid).
//! - Read `/proc/<daemon-pid>/environ` (different uid).
//! - Speak the daemon's HTTP-to-control-plane channel directly (it
//!   doesn't have the deployment token).
//!
//! User code (uid 1000, member of `git-users`) *can*:
//! - Connect to `/run/temps/git.sock` and ask for a credential (mode 0660).
//! - Receive a per-op token narrowed to its own project's repo only.
//!
//! That is exactly what git needs and nothing more.

use serde::{Deserialize, Serialize};

pub mod helper_protocol;
pub mod ipc;

/// Default location of the IPC socket the helper connects to and the
/// daemon listens on. Overridable via the `TEMPS_GIT_CREDENTIAL_SOCKET`
/// environment variable, primarily for tests.
///
/// Path matches the directory the sandbox Dockerfile pre-creates with
/// `temps-git:git-users` ownership and mode `0750`. Changing this
/// constant requires a matching Dockerfile change or the daemon will
/// fail to bind.
pub const DEFAULT_SOCKET_PATH: &str = "/run/temps-git/git.sock";

/// Default location of the daemon's environment file. Holds
/// `TEMPS_API_URL` + `TEMPS_API_TOKEN` (the workspace session's
/// deployment token) and *must* be mode `0600` owned by the daemon's
/// uid. Overridable via `TEMPS_GIT_CREDENTIAL_DAEMON_ENV` for tests.
pub const DEFAULT_DAEMON_ENV_PATH: &str = "/etc/temps/credential-daemon.env";

/// Operation types the helper passes through to the daemon. Matches the
/// control-plane mint endpoint's `operation` field.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    /// `git clone`/`git fetch`/`git ls-remote`. Read-only.
    Fetch,
    /// `git push`. Write to repo `contents` only.
    Push,
}

impl Operation {
    /// Best-effort guess at what git is doing based on the credential
    /// request alone. Git's helper protocol does NOT explicitly tell us
    /// fetch vs push — it just says "I need creds for host X path Y".
    /// We default to the safer (read-only) operation; callers that
    /// know they're pushing must override via the
    /// `TEMPS_GIT_CREDENTIAL_OP` env var, set by the helper based on
    /// argv inspection if available, or by the daemon based on per-op
    /// retry signals.
    pub fn default_safe() -> Self {
        Self::Fetch
    }
}
