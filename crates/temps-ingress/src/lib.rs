// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Ingress connectors (ADR 058): ways into the cluster that are not a public
//! address on a node, starting with Cloudflare Tunnel.
//!
//! Two sides, one per trait:
//! - [`provider::TunnelProvider`] (here, on the control plane) owns the
//!   vendor API and says *what* a node runs.
//! - The node's agent owns *how*: it picks a driver (child process,
//!   container or compiled-in library) and supervises it. See
//!   `temps_node_agent::connectors`.

pub mod cloudflare;
pub mod error;
pub mod handlers;
pub mod plugin;
pub mod provider;
pub mod reachability;
pub mod registry;
pub mod service;
pub mod tailscale;
pub mod wire;

pub use error::IngressError;
pub use handlers::{node_routes, IngressNodeState};
pub use plugin::IngressPlugin;
pub use service::IngressConnectorService;
