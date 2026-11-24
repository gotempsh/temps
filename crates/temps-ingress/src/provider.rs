// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The control-plane side of a tunnel provider (ADR 058).
//!
//! A provider owns its vendor's API: creating the tunnel, issuing the secret
//! nodes run it with, and tearing it down. It says *what* a node runs through
//! [`TunnelProvider::connector_spec`], never *how*: the node's agent picks a
//! driver. It never writes DNS either: [`TunnelProvider::publish`] returns a
//! [`DnsIntent`] that the service applies through whichever DNS provider
//! manages the zone.

use std::collections::HashSet;

use async_trait::async_trait;

use crate::error::IngressError;
use crate::wire::NodeConnectorSpec;

/// The provider-side tunnel, as stored on the connector row.
#[derive(Debug, Clone)]
pub struct TunnelHandle {
    pub external_id: String,
    pub settings: serde_json::Value,
    /// The secret nodes run the connector with.
    pub secret: String,
}

pub struct TunnelContext<'a> {
    pub connector_id: i32,
    pub name: &'a str,
    pub domain: &'a str,
    pub edge_port: u16,
    /// This install's id, so a new tunnel's name cannot collide with one
    /// another install created in the same provider account.
    pub instance_id: &'a str,
    /// The tunnel created earlier, if any; `ensure` adopts it when it still
    /// exists and replaces it when it is gone. A tunnel is never adopted by
    /// name.
    pub existing: Option<TunnelHandle>,
}

/// What DNS must say for a published hostname.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsIntent {
    /// A CNAME to the provider's target, proxied when `proxied`.
    Cname { target: String, proxied: bool },
    /// The provider owns the name (for example an ngrok-assigned domain).
    None,
}

#[derive(Debug, Clone)]
pub struct TunnelHealth {
    /// The provider's own word for it (Cloudflare: `healthy`, `degraded`,
    /// `down`, `inactive`).
    pub status: String,
    pub links: Vec<TunnelLink>,
}

/// One connection from a node's connector to the provider's edge.
#[derive(Debug, Clone)]
pub struct TunnelLink {
    /// The provider's data centre, such as `mad01`.
    pub location: Option<String>,
    pub opened_at: Option<String>,
    /// Dropped and being re-established.
    pub reconnecting: bool,
    /// Which running connector holds it; matches the node's reported
    /// `connector_ref`.
    pub connector_ref: Option<String>,
    pub client_version: Option<String>,
}

#[async_trait]
pub trait TunnelProvider: Send + Sync {
    fn kind(&self) -> &'static str;

    /// Create the tunnel, or adopt `ctx.existing`. Idempotent.
    async fn ensure(&self, ctx: &TunnelContext<'_>) -> Result<TunnelHandle, IngressError>;

    /// What a node runs for this tunnel.
    fn connector_spec(
        &self,
        connector_id: i32,
        handle: &TunnelHandle,
        edge_port: u16,
    ) -> NodeConnectorSpec;

    /// Make `host` reachable through the tunnel; returns what DNS must say.
    async fn publish(&self, handle: &TunnelHandle, host: &str) -> Result<DnsIntent, IngressError>;

    async fn unpublish(&self, handle: &TunnelHandle, host: &str) -> Result<(), IngressError>;

    async fn health(&self, handle: &TunnelHandle) -> Result<TunnelHealth, IngressError>;

    /// Invalidate the secret nodes run the tunnel with and return the new
    /// one: called when a node that held it stops running the tunnel, so a
    /// removed (or compromised) node cannot keep serving its traffic. An open
    /// connection outlives the secret, so the provider also cuts every live
    /// connection whose client is not in `keep_clients` (the `connector_ref`s
    /// of the nodes that stay; empty cuts them all), and fails if it cannot.
    /// `None` when the provider cannot rotate; the service then says so on
    /// the connector.
    async fn rotate_secret(
        &self,
        _handle: &TunnelHandle,
        _keep_clients: &HashSet<String>,
    ) -> Result<Option<String>, IngressError> {
        Ok(None)
    }

    async fn delete(&self, handle: &TunnelHandle) -> Result<(), IngressError>;
}
