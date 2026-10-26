// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a node pulls and pushes for ingress connectors (ADR 058).
//!
//! The control plane says *what* a node runs; the node's agent decides *how*
//! from the [`DriverChoice`] list, taking the first driver it supports. The
//! agent mirrors these types in `temps_agent::connectors::wire`.
//!
//! Strings in a driver spec may contain placeholders the agent fills in:
//! `{edge_addr}` (the node's tunnel origin listener), `{metrics_addr}` (a
//! free loopback port allocated per connector) and `{secret_file:NAME}` (a
//! 0600 file holding secret `NAME`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NodeConnectorsResponse {
    pub connectors: Vec<NodeConnectorSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NodeConnectorSpec {
    pub id: i32,
    pub provider: String,
    /// Changes whenever anything the node runs changes; the node restarts
    /// the connector when it does.
    pub revision: String,
    /// Loopback port the node's tunnel origin listener binds.
    pub edge_port: u16,
    /// Header carrying the visitor's address, trusted only from loopback.
    pub client_address_header: Option<String>,
    /// Header carrying the visitor's JA4 TLS fingerprint (ADR 066, D4),
    /// trusted only from loopback. Absent from older control planes.
    #[serde(default)]
    pub client_ja4: Option<ForwardedJa4>,
    /// Preference order; the node runs the first one it supports.
    pub drivers: Vec<DriverChoice>,
    /// Secret values by name, referenced from the driver spec.
    pub secrets: BTreeMap<String, String>,
}

/// Where a connector puts the visitor's JA4, and who computed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ForwardedJa4 {
    pub header: String,
    /// The request log's `ja4_source`: `cloudflare` for Cloudflare's edge.
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DriverChoice {
    /// Run a pinned binary as a supervised child process.
    Process(ProcessSpec),
    /// Run an image, pinned by digest, as a supervised container.
    Container(ContainerSpec),
    /// Run a connector compiled into the agent.
    Library {
        name: String,
        #[schema(value_type = Object)]
        config: serde_json::Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProcessSpec {
    /// One per platform; the node picks its own and verifies the sha256.
    pub artifacts: Vec<Artifact>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub readiness: Readiness,
    pub stop_grace_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Artifact {
    /// `linux`.
    pub os: String,
    /// `x86_64` or `aarch64` (Rust's `std::env::consts::ARCH`).
    pub arch: String,
    pub url: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ContainerSpec {
    /// Image reference pinned by digest.
    pub image: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub readiness: Readiness,
    pub stop_grace_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Readiness {
    /// Ready when the URL answers 200. A JSON body with `readyConnections`
    /// and `connectorId` (cloudflared's `/ready`) is reported as well.
    HttpReady { url: String },
    /// Ready when the address accepts a TCP connection.
    TcpConnect { address: String },
    /// Ready once running.
    Running,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NodeConnectorStatusReport {
    pub reports: Vec<ConnectorStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ConnectorStatus {
    pub connector_id: i32,
    /// `process`, `container` or `library:<name>`; `None` when no driver fits.
    pub driver: Option<String>,
    /// `starting`, `ready`, `degraded` or `down`.
    pub state: String,
    pub ready_connections: Option<i32>,
    pub connector_ref: Option<String>,
    pub last_error: Option<String>,
    pub restarts: u32,
}
