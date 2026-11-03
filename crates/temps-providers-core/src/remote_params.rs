// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The request that creates a service on a remote node, shared by the agent's
//! HTTP client and the workload spec built from it.

use std::collections::HashMap;

/// Parameters needed to create a service on a remote node.
#[derive(Debug, serde::Serialize)]
pub struct RemoteServiceCreateParams {
    pub name: String,
    pub service_type: String,
    pub image: String,
    pub environment: HashMap<String, String>,
    pub port_mappings: Vec<RemotePortMapping>,
    pub volumes: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    /// Optional cgroup limits applied to the container (memory, swap, CPU).
    /// Skipped from the wire when unset, so older agents that don't know
    /// about the field still parse the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_limits: Option<crate::externalsvc::ServiceResourceLimits>,
}

#[derive(Debug, serde::Serialize)]
pub struct RemotePortMapping {
    /// Host port to bind. `0` means let Docker auto-assign a free port.
    pub host_port: u16,
    pub container_port: u16,
}
