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

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use bollard::Docker;
use temps_workload_netpol::{
    available_datapath, choose_executor, choose_port_resolver, spawn_container_wake,
    DatapathConfig, DockerInstanceSource, EgressTable, EnforcerError, IdentityMap, NodeEnforcer,
    NodeMode, NodePolicy, NodePolicyReport, PassOutcome, PolicySnapshot, PolicySource, TableName,
};
use tracing::{info, warn};

use crate::netpol_guard::BootGuard;

/// File name, in the agent data directory, of the last policy the control
/// plane sent.
pub const POLICY_CACHE_FILE: &str = "netpol-last-policy.json";

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
    /// Where the last policy received is kept, so an agent that boots while
    /// the control plane is unreachable enforces it instead of nothing.
    cache: Option<PathBuf>,
    /// The last policy written to `cache`, to skip identical writes.
    cached: Mutex<Option<String>>,
    guard: Option<BootGuard>,
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
            cache: None,
            cached: Mutex::new(None),
            guard: None,
        }
    }

    /// Keep the last policy in `path`, and use it when the control plane
    /// cannot be reached before this process got its first answer.
    pub fn with_cache(mut self, path: PathBuf) -> Self {
        self.cache = Some(path);
        self
    }

    pub fn with_boot_guard(mut self, guard: BootGuard) -> Self {
        self.guard = Some(guard);
        self
    }

    fn store_cache(&self, policy: &NodePolicy) {
        let Some(path) = &self.cache else { return };
        let encoded = match serde_json::to_string(policy) {
            Ok(encoded) => encoded,
            Err(error) => {
                warn!(node = %self.node, %error, "could not encode the network policy for its cache");
                return;
            }
        };
        let mut cached = self.cached.lock().unwrap_or_else(|e| e.into_inner());
        if cached.as_deref() == Some(encoded.as_str()) {
            return;
        }
        match write_private(path, &encoded) {
            Ok(()) => *cached = Some(encoded),
            Err(error) => warn!(
                node = %self.node,
                path = %path.display(),
                %error,
                "could not cache the network policy; a reboot during a control plane outage \
                 would leave this node without it"
            ),
        }
    }

    /// The cached policy, only before this process received one: after
    /// that, a failed fetch keeps what the kernel holds, as before.
    fn cached_policy(&self) -> Option<NodePolicy> {
        if self
            .last
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return None;
        }
        let path = self.cache.as_ref()?;
        let data = std::fs::read_to_string(path).ok()?;
        match serde_json::from_str(&data) {
            Ok(policy) => Some(policy),
            Err(error) => {
                warn!(node = %self.node, path = %path.display(), %error, "ignoring an unreadable network policy cache");
                None
            }
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
        let policy = match self.fetch(&report).await {
            Ok(policy) => {
                self.store_cache(&policy);
                policy
            }
            Err(error) => match self.cached_policy() {
                Some(cached) => {
                    warn!(
                        node = %self.node,
                        %error,
                        mode = ?cached.mode,
                        "control plane unreachable before this agent's first policy; enforcing \
                         the last policy it sent until it answers"
                    );
                    cached
                }
                None => return Err(error),
            },
        };
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

    async fn applied(&self, outcome: &PassOutcome) {
        if let Some(guard) = &self.guard {
            guard.after_pass(outcome).await;
        }
    }
}

impl AgentPolicySource {
    async fn fetch(&self, report: &NodePolicyReport) -> Result<NodePolicy, EnforcerError> {
        let response = self
            .http
            .post(&self.url)
            .json(report)
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
        response
            .json()
            .await
            .map_err(|e| self.source_error(format!("decoding the policy from {}: {e}", self.url)))
    }
}

fn write_private(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let temporary = path.with_extension("json.tmp");
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(contents.as_bytes())?;
    }
    std::fs::rename(&temporary, path)
}

/// Starts this worker's enforcer on the local Docker daemon.
pub fn spawn_worker_enforcer(
    http: reqwest::Client,
    control_plane_url: &str,
    node_id: i32,
    token: String,
    docker: Docker,
    dns: temps_node_dns::DnsPolicySlot,
    data_dir: &std::path::Path,
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
        Arc::new(
            AgentPolicySource::new(http, control_plane_url, node_id, token, backend)
                .with_cache(data_dir.join(POLICY_CACHE_FILE))
                .with_boot_guard(BootGuard::new(data_dir, docker.clone())),
        ),
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
