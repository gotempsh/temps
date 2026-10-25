// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Mirror of `temps_ingress::wire`: what the control plane hands a node.
//! Kept as a copy, like the route snapshot types, so the agent does not
//! depend on the control-plane crates.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeConnectorsResponse {
    pub connectors: Vec<NodeConnectorSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConnectorSpec {
    pub id: i32,
    pub provider: String,
    pub revision: String,
    pub edge_port: u16,
    pub client_address_header: Option<String>,
    pub drivers: Vec<DriverChoice>,
    pub secrets: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DriverChoice {
    Process(ProcessSpec),
    Container(ContainerSpec),
    Library {
        name: String,
        config: serde_json::Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessSpec {
    pub artifacts: Vec<Artifact>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub readiness: Readiness,
    pub stop_grace_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub os: String,
    pub arch: String,
    pub url: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerSpec {
    pub image: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub readiness: Readiness,
    pub stop_grace_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Readiness {
    HttpReady { url: String },
    TcpConnect { address: String },
    Running,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConnectorStatusReport {
    pub reports: Vec<ConnectorStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorStatus {
    pub connector_id: i32,
    pub driver: Option<String>,
    pub state: String,
    pub ready_connections: Option<i32>,
    pub connector_ref: Option<String>,
    pub last_error: Option<String>,
    pub restarts: u32,
}
