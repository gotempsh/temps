// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The database as a node enforcer's [`PolicySource`], and the node-0
//! enforcer task.

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use temps_workload_netpol::{
    choose_executor, choose_port_resolver, DnsPolicy, DockerInstanceSource, EgressTable,
    EnforcerError, NodeEnforcer, NodeMode, PolicySnapshot, PolicySource, TableName,
};
use tracing::info;

use crate::service::{NetworkPolicyService, CONTROL_PLANE_NODE_ID};

/// How often node 0 re-checks its mode and, outside `legacy`, its policy.
pub const PASS_INTERVAL: Duration = Duration::from_secs(15);

/// Reads one node's mode every pass, and rebuilds the snapshot only when
/// the database revision moved.
pub struct DbPolicySource {
    service: Arc<NetworkPolicyService>,
    node_id: i32,
    cache: ArcSwap<Option<(i64, Arc<PolicySnapshot>)>>,
}

impl DbPolicySource {
    pub fn new(service: Arc<NetworkPolicyService>, node_id: i32) -> Self {
        Self {
            service,
            node_id,
            cache: ArcSwap::from_pointee(None),
        }
    }

    fn node(&self) -> String {
        format!("node-{}", self.node_id)
    }
}

#[async_trait]
impl PolicySource for DbPolicySource {
    async fn mode(&self) -> Result<NodeMode, EnforcerError> {
        self.service
            .node_mode(self.node_id)
            .await
            .map_err(|e| EnforcerError::Source {
                node: self.node(),
                reason: e.to_string(),
            })
    }

    async fn snapshot(&self) -> Result<PolicySnapshot, EnforcerError> {
        let source = |e: crate::error::NetworkPolicyError| EnforcerError::Source {
            node: self.node(),
            reason: e.to_string(),
        };
        let revision = self.service.revision().await.map_err(source)?;
        if let Some((cached, snapshot)) = self.cache.load().as_ref() {
            if *cached == revision {
                return Ok(snapshot.as_ref().clone());
            }
        }
        let snapshot = Arc::new(self.service.snapshot().await.map_err(source)?);
        self.cache
            .store(Arc::new(Some((revision, snapshot.clone()))));
        Ok(snapshot.as_ref().clone())
    }
}

/// The nftables table node 0 owns.
pub fn control_plane_table() -> Result<TableName, EnforcerError> {
    TableName::new("temps_netpol_node0").map_err(|source| EnforcerError::Compile {
        node: format!("node-{CONTROL_PLANE_NODE_ID}"),
        source,
    })
}

/// Starts node 0's enforcer. In `legacy` (the default) each pass reads the
/// mode and does nothing else: no Docker call, no nftables.
pub fn spawn_control_plane_enforcer(
    service: Arc<NetworkPolicyService>,
    docker: bollard::Docker,
    daemon_is_local: bool,
) -> Result<tokio::task::JoinHandle<()>, EnforcerError> {
    let node = format!("node-{CONTROL_PLANE_NODE_ID}");
    let table = control_plane_table()?;
    let enforcer = NodeEnforcer::new(
        node.clone(),
        table,
        Arc::from(choose_executor(docker.clone(), daemon_is_local)),
        Arc::from(choose_port_resolver(docker.clone(), daemon_is_local)),
        Arc::new(DockerInstanceSource::new(node.clone(), docker)),
        Arc::new(DbPolicySource::new(service, CONTROL_PLANE_NODE_ID)),
        Arc::new(ArcSwap::from_pointee(DnsPolicy::open())),
        Arc::new(ArcSwap::from_pointee(EgressTable::default())),
    );
    info!(
        node = %node,
        interval_secs = PASS_INTERVAL.as_secs(),
        "network policy enforcer started (mode is read from the database every pass)"
    );
    Ok(tokio::spawn(enforcer.run(PASS_INTERVAL)))
}
