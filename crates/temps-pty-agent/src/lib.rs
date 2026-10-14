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
