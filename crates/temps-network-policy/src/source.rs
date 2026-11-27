// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The database as a node enforcer's [`PolicySource`], and the node-0
//! enforcer task.

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use temps_workload_netpol::{
    choose_executor, choose_port_resolver, spawn_container_wake, with_best_datapath, DnsPolicy,
    DockerInstanceSource, EgressTable, EnforcerError, IdentityMap, NodeEnforcer, NodeMode,
    PolicySnapshot, PolicySource, TableName,
};
use tracing::info;

use crate::service::{NetworkPolicyService, CONTROL_PLANE_NODE_ID};

/// How often node 0 re-checks its mode and, outside `off`, its policy.
pub const PASS_INTERVAL: Duration = Duration::from_secs(15);

/// Reads one node's mode every pass and, outside `off`, rebuilds the
/// snapshot every pass too.
///
/// It is not cached by `network_policy_state.revision`: that revision only
/// moves with rules and profiles, while the snapshot also depends on live
/// sandboxes, service members, name records and worker-hosted addresses. A
/// revision-keyed cache kept serving a removed member's address, and never
/// saw a new one. The enforcer compares the compiled ruleset and touches the
/// kernel only when it changed, so a rebuild costs a few bounded queries
/// every [`PASS_INTERVAL`] on node 0.
pub struct DbPolicySource {
    service: Arc<NetworkPolicyService>,
    node_id: i32,
    /// The mode and policy revision the last `mode` call read, for
    /// `off_snapshot` and `observed` in the same pass.
    last: std::sync::Mutex<(NodeMode, i64)>,
}

impl DbPolicySource {
    pub fn new(service: Arc<NetworkPolicyService>, node_id: i32) -> Self {
        Self {
            service,
            node_id,
            last: std::sync::Mutex::new((NodeMode::default(), 0)),
        }
    }

    fn last(&self) -> (NodeMode, i64) {
        *self.last.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn node(&self) -> String {
        format!("node-{}", self.node_id)
    }
}

#[async_trait]
impl PolicySource for DbPolicySource {
    /// One query: the mode and the policy revision.
    async fn mode(&self) -> Result<NodeMode, EnforcerError> {
        let (mode, revision) = self
            .service
            .mode_and_revision(self.node_id)
            .await
            .map_err(|e| EnforcerError::Source {
                node: self.node(),
                reason: e.to_string(),
            })?;
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = (mode, revision);
        Ok(mode)
    }

    /// None: in `off` nothing on the control plane reads what this
    /// enforcer would observe. Its egress table is its own, and the control
    /// plane's own workloads are recorded by the local recorder, which keeps
    /// a table of its own; other nodes never take an `off` node's workloads
    /// as peers. So an idle pass lists no containers and clones no snapshot.
    async fn off_snapshot(&self) -> Result<Option<PolicySnapshot>, EnforcerError> {
        Ok(None)
    }

    async fn snapshot(&self) -> Result<PolicySnapshot, EnforcerError> {
        self.service
            .snapshot()
            .await
            .map_err(|e| EnforcerError::Source {
                node: self.node(),
                reason: e.to_string(),
            })
    }

    /// Node 0's workloads, for the workers: the same record a worker's
    /// report makes.
    fn observed(&self, local: &IdentityMap) {
        let (mode, _) = self.last();
        self.service
            .record_observed(self.node_id, local.clone(), mode);
    }
}

/// The nftables table node 0 owns.
pub fn control_plane_table() -> Result<TableName, EnforcerError> {
    TableName::new("temps_netpol_node0").map_err(|source| EnforcerError::Compile {
        node: format!("node-{CONTROL_PLANE_NODE_ID}"),
        source,
    })
}

/// Starts node 0's enforcer. In `off` (the default) each pass reads the
/// mode and does nothing else: no Docker call, no nftables.
///
/// `dns` is the slot node 0's resolver answers restricted workloads from;
/// each pass stores its snapshot there.
pub fn spawn_control_plane_enforcer(
    service: Arc<NetworkPolicyService>,
    docker: bollard::Docker,
    daemon_is_local: bool,
    dns: Arc<ArcSwap<DnsPolicy>>,
) -> Result<tokio::task::JoinHandle<()>, EnforcerError> {
    let node = format!("node-{CONTROL_PLANE_NODE_ID}");
    let table = control_plane_table()?;
    let enforcer = NodeEnforcer::new(
        node.clone(),
        table,
        Arc::from(choose_executor(docker.clone(), daemon_is_local)),
        Arc::from(choose_port_resolver(docker.clone(), daemon_is_local)),
        Arc::new(DockerInstanceSource::new(node.clone(), docker.clone())),
        Arc::new(DbPolicySource::new(service, CONTROL_PLANE_NODE_ID)),
        dns,
        Arc::new(ArcSwap::from_pointee(EgressTable::default())),
    );
    let enforcer = with_best_datapath(enforcer, daemon_is_local);
    let enforcer = if enforcer.uses_datapath() {
        enforcer.with_wake(spawn_container_wake(node.clone(), docker))
    } else {
        enforcer
    };
    info!(
        node = %node,
        interval_secs = PASS_INTERVAL.as_secs(),
        "network policy enforcer started (mode is read from the database every pass)"
    );
    Ok(tokio::spawn(enforcer.run(PASS_INTERVAL)))
}
