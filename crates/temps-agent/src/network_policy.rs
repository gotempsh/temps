// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The worker's network policy enforcer (network access plan, §4.15).
//!
//! The same [`NodeEnforcer`] node 0 runs, fed from the control plane
//! instead of the database: every pass posts a [`NodePolicyReport`] to
//! `/api/internal/nodes/{id}/network-policy` with the node token and gets
//! the node's mode and snapshot back. The report tells the control plane
//! which datapath this node enforces with, which is what allows an
//! operator to move the node out of `legacy`, and which workloads it
//! observed on its previous pass, which is how every other node learns
//! their addresses. In `legacy` nothing else happens: no Docker call, no
//! program loaded.
//!
//! A failed fetch fails the pass, and the kernel keeps what the last pass
//! applied: a control plane outage never opens or closes anything.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use bollard::Docker;
use temps_workload_netpol::{
    available_datapath, choose_executor, choose_port_resolver, spawn_container_wake,
    DatapathConfig, DockerInstanceSource, EgressTable, EnforcerError, IdentityMap, NodeEnforcer,
    NodeMode, NodePolicy, NodePolicyReport, PolicySnapshot, PolicySource, TableName,
};
use tracing::info;

/// How often the worker re-reads its mode and policy.
pub const PASS_INTERVAL: Duration = Duration::from_secs(15);

/// Reads this node's mode and snapshot from the control plane. `mode`
/// fetches both, `snapshot` returns what that fetch brought, so a pass is
/// one request. The request carries what `observed` recorded on the
/// previous pass, so the control plane's view of this node trails it by
/// one pass.
pub struct AgentPolicySource {
    http: reqwest::Client,
    url: String,
    node: String,
    token: String,
    backend: &'static str,
    last: Mutex<Option<PolicySnapshot>>,
    observed: Mutex<IdentityMap>,
}

impl AgentPolicySource {
    pub fn new(
        http: reqwest::Client,
        control_plane_url: &str,
        node_id: i32,
        token: String,
        backend: &'static str,
    ) -> Self {
        Self {
            http,
            url: policy_url(control_plane_url, node_id),
            node: format!("node-{node_id}"),
            token,
            backend,
            last: Mutex::new(None),
            observed: Mutex::new(IdentityMap::new()),
        }
    }

    fn source_error(&self, reason: String) -> EnforcerError {
        EnforcerError::Source {
            node: self.node.clone(),
            reason,
        }
    }
}

/// The policy endpoint of `node_id` on the control plane.
pub fn policy_url(control_plane_url: &str, node_id: i32) -> String {
    format!(
        "{}/api/internal/nodes/{node_id}/network-policy",
        control_plane_url.trim_end_matches('/')
    )
}

#[async_trait]
impl PolicySource for AgentPolicySource {
    async fn mode(&self) -> Result<NodeMode, EnforcerError> {
        let report = NodePolicyReport {
            backend: self.backend.to_string(),
            identities: self
                .observed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        };
        let response = self
            .http
            .post(&self.url)
            .json(&report)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| self.source_error(format!("POST {}: {e}", self.url)))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(self.source_error(format!(
                "POST {} returned {status}: {}",
                self.url,
                body.chars().take(300).collect::<String>()
            )));
        }
        let policy: NodePolicy = response.json().await.map_err(|e| {
            self.source_error(format!("decoding the policy from {}: {e}", self.url))
        })?;
        if policy.mode == NodeMode::Legacy {
            // Nothing is observed in `legacy`; stop reporting what was.
            *self.observed.lock().unwrap_or_else(|e| e.into_inner()) = IdentityMap::new();
        }
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        *last = policy.snapshot;
        Ok(policy.mode)
    }

    async fn snapshot(&self) -> Result<PolicySnapshot, EnforcerError> {
        let last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        last.clone().ok_or_else(|| {
            self.source_error(
                "the control plane sent a mode outside legacy without a snapshot".to_string(),
            )
        })
    }

    fn observed(&self, local: &IdentityMap) {
        *self.observed.lock().unwrap_or_else(|e| e.into_inner()) = local.clone();
    }
}

/// Starts this worker's enforcer on the local Docker daemon.
pub fn spawn_worker_enforcer(
    http: reqwest::Client,
    control_plane_url: &str,
    node_id: i32,
    token: String,
    docker: Docker,
    dns: temps_dns_resolver::DnsPolicySlot,
) -> Result<tokio::task::JoinHandle<()>, EnforcerError> {
    let node = format!("node-{node_id}");
    let table = TableName::new(format!("temps_netpol_node{node_id}")).map_err(|source| {
        EnforcerError::Compile {
            node: node.clone(),
            source,
        }
    })?;
    // The agent always runs next to its daemon.
    let daemon_is_local = true;
    let config = DatapathConfig::default();
    let datapath = available_datapath(config.clone());
    let backend = if datapath.is_ok() { "ebpf" } else { "nftables" };
    let enforcer = NodeEnforcer::new(
        node.clone(),
        table,
        Arc::from(choose_executor(docker.clone(), daemon_is_local)),
        Arc::from(choose_port_resolver(docker.clone(), daemon_is_local)),
        Arc::new(DockerInstanceSource::new(node.clone(), docker.clone())),
        Arc::new(AgentPolicySource::new(
            http,
            control_plane_url,
            node_id,
            token,
            backend,
        )),
        dns,
        Arc::new(ArcSwap::from_pointee(EgressTable::default())),
    );
    let enforcer = match datapath {
        Ok(opener) => enforcer
            .with_datapath(opener, &config)
            .with_wake(spawn_container_wake(node.clone(), docker)),
        Err(reason) => {
            info!(node = %node, %reason, "network policy backend: nftables");
            enforcer
        }
    };
    info!(
        node = %node,
        backend,
        interval_secs = PASS_INTERVAL.as_secs(),
        "worker network policy enforcer started (mode is read from the control plane every pass)"
    );
    Ok(tokio::spawn(enforcer.run(PASS_INTERVAL)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_policy_url_never_doubles_slashes() {
        assert_eq!(
            policy_url("https://cp.example:8081/", 4),
            "https://cp.example:8081/api/internal/nodes/4/network-policy"
        );
        assert_eq!(
            policy_url("https://cp.example:8081", 4),
            "https://cp.example:8081/api/internal/nodes/4/network-policy"
        );
    }
}
