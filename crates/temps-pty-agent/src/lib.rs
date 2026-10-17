// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `temps-pty-agent` — in-sandbox PTY multiplexer reached over a Unix socket.
//!
//! The agent runs as a long-lived process inside every workspace sandbox
//! (container or microVM), owning all PTYs for that sandbox's interactive
//! terminals. The host (Axum handler) reaches it through a relay that pipes
//! bytes to the agent's socket — `docker exec socat UNIX-CONNECT:…` on Docker,
//! `temps-pty-agent --relay` (see [`relay`]) where `socat` isn't available —
//! and speaks the framed protocol in `protocol`.
//!
//! See `docs/adr/008-pty-agent.md` for the full design + rationale.

pub mod protocol;
pub mod pty;
pub mod relay;
pub mod server;

pub const DEFAULT_SOCKET_PATH: &str = "/run/temps-pty/agent.sock";

/// Commit this crate was built from, or `unknown` without a git checkout
/// (or when the build script did not run, as in a bundled container build).
pub const BUILD_COMMIT: &str = match option_env!("TEMPS_AGENT_BUILD_COMMIT") {
    Some(commit) => commit,
    None => "unknown",
};

/// Prefix of the build marker embedded in every guest agent binary. The host
/// finds it by scanning the file, so no agent has to be executed to learn
/// which build it is.
pub const BUILD_MARKER_PREFIX: &str = "TEMPS_GUEST_AGENT_BUILD=";

const MARKER_LEN: usize = BUILD_MARKER_PREFIX.len() + 41;

/// The marker: prefix, commit (at most 40 bytes), then NULs. Referenced from
/// each agent's `main` so the linker keeps it.
pub static BUILD_MARKER: [u8; MARKER_LEN] = build_marker(BUILD_COMMIT);

const fn build_marker(commit: &str) -> [u8; MARKER_LEN] {
    let mut out = [0u8; MARKER_LEN];
    let prefix = BUILD_MARKER_PREFIX.as_bytes();
    let commit = commit.as_bytes();
    let mut i = 0;
    while i < prefix.len() {
        out[i] = prefix[i];
        i += 1;
    }
    let mut j = 0;
    while j < commit.len() && j < 40 {
        out[prefix.len() + j] = commit[j];
        j += 1;
    }
    out
}
