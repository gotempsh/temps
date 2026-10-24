// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where the proxy hands preview traffic for sandboxes on worker nodes.
//!
//! The preview gateway container only reaches sandboxes on this host. A
//! sandbox on a worker is reached through the worker's agent instead, by the
//! relay the sandbox plugin runs in this process. It listens on a loopback
//! port chosen at startup and accepts only connections presenting the token
//! it generated, which it registers here for the proxy to inject. Nothing is
//! configured: a process that runs no sandbox plugin (`temps proxy` on its
//! own) has no relay, and the proxy answers such previews with a 502 naming
//! why.

use std::net::SocketAddr;
use std::sync::OnceLock;

/// The relay's loopback address and the token it requires in
/// [`WORKER_PREVIEW_RELAY_TOKEN_HEADER`].
#[derive(Debug, Clone)]
pub struct WorkerPreviewRelay {
    pub address: SocketAddr,
    pub token: String,
}

/// Header carrying [`WorkerPreviewRelay::token`]. The same name the gateway
/// uses, so both strip it the same way before bytes reach a sandbox.
pub const WORKER_PREVIEW_RELAY_TOKEN_HEADER: &str = "x-temps-preview-token";

static RELAY: OnceLock<WorkerPreviewRelay> = OnceLock::new();

/// Record this process's relay. Only the first registration counts: there is
/// one relay per process, started once.
pub fn register_worker_preview_relay(relay: WorkerPreviewRelay) -> bool {
    RELAY.set(relay).is_ok()
}

/// This process's relay, if one is running.
pub fn worker_preview_relay() -> Option<&'static WorkerPreviewRelay> {
    RELAY.get()
}
