// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The control plane's own public address, as the server itself finds it.
//!
//! Whoever needs the address DNS should point at to reach this machine
//! (*Make it reachable*'s direct options) asks the server, never the DNS of
//! the console's hostname: a console behind a CDN or reverse proxy resolves
//! to the proxy's edge, which is not this machine. The infrastructure
//! plugin implements it; the ingress plugin only reads it.

use std::net::IpAddr;

use async_trait::async_trait;

#[async_trait]
pub trait ServerPublicAddress: Send + Sync {
    /// The public address this machine reaches the internet from, detected
    /// by the server (and cached). `None` when it cannot be told.
    async fn detected_public_ip(&self) -> Option<IpAddr>;
}
