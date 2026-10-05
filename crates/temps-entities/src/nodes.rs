// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "nodes")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub token_hash: String,
    /// Encrypted plaintext token (AES-256-GCM via EncryptionService)
    /// Used by control plane to authenticate with the agent for remote deployments
    pub token_encrypted: Option<String>,
    /// Agent API address, e.g. "https://203.0.113.50:3100"
    pub address: String,
    /// WireGuard IP or user-provided private address, e.g. "10.100.0.2"
    pub private_address: String,
    /// WireGuard endpoint for peer connections, e.g. "203.0.113.50:51820"
    pub public_endpoint: Option<String>,
    /// WireGuard public key (base64-encoded, 44 chars)
    pub wg_public_key: Option<String>,
    /// "worker" or "control"
    pub role: String,
    /// "pending", "active", "offline", "draining"
    pub status: String,
    /// Arbitrary key-value labels for scheduling, e.g. {"region": "us-east"}
    pub labels: serde_json::Value,
    /// Resource capacity metrics from heartbeats
    pub capacity: serde_json::Value,
    pub last_heartbeat: Option<DBDateTime>,
    /// X25519 public key for edge nodes (base64-encoded, used for ECIES cert encryption)
    pub edge_public_key: Option<String>,
    /// Per-node CIDR for the multi-host overlay (e.g. "172.20.5.0/24"). Other
    /// nodes route this CIDR to us via the configured transport. Allocated by
    /// the control-plane `ComputeNetworkAllocator` when the node joins.
    /// Stored as text to mirror `private_address` / `public_endpoint`; parsed
    /// to `ipnet::Ipv4Net` at the application boundary.
    pub compute_cidr: Option<String>,
    /// Container platform this node runs, in OCI form (`linux/amd64`,
    /// `linux/arm64`). Reported by the agent from `docker info` on every
    /// heartbeat — it is the *daemon's* architecture, which is what decides
    /// whether an image can run here, not the agent binary's.
    ///
    /// `None` means "not reported yet": an agent older than the multi-arch
    /// support, or a node that hasn't heartbeated since the upgrade. The
    /// scheduler treats that as compatible-but-unknown rather than excluding
    /// the node, so a rolling upgrade doesn't drain the cluster.
    pub architecture: Option<String>,
    /// Address other nodes use to reach this one over the underlay. Cloud
    /// private IP for same-DC clusters, public IP for cross-DC. Parsed to
    /// `std::net::IpAddr` at the application boundary.
    pub underlay_address: Option<String>,
    /// Mesh WireGuard public key, reported by the running agent. Separate
    /// from `wg_public_key` (legacy `temps join` field that is part of the
    /// registration identity check) so re-registration leaves the mesh alone.
    pub mesh_wg_public_key: Option<String>,
    /// `ip:port` other nodes dial to reach this node's WireGuard socket.
    pub mesh_wg_endpoint: Option<String>,
    /// This node's mesh address; its `underlay_address` when the mesh is on.
    pub mesh_wg_address: Option<String>,
    /// Whether this node's per-node DNS resolver (ADR-024) is currently
    /// running, as of the last heartbeat that reported it. `None` means
    /// "never reported" — either an agent binary older than this feature,
    /// or a node that has never ticked its network-sync loop (a true
    /// single-host node with no `compute_cidr` allocation never touches
    /// cluster DNS at all). That's distinct from `Some(false)`, which means
    /// a heartbeat arrived and the resolver was confirmed not running
    /// (cluster DNS disabled, or the resolver failed to start).
    pub dns_resolver_running: Option<bool>,
    /// Whether the resolver's background tasks (sync loop, DNS server) were
    /// alive as of the last heartbeat. Only meaningful when
    /// `dns_resolver_running == Some(true)`; `None` has the same
    /// never-reported meaning as `dns_resolver_running`.
    pub dns_resolver_tasks_alive: Option<bool>,
    /// Timestamp of the resolver's last successful sync against the control
    /// plane's DNS change feed, as of the last heartbeat. `None` means
    /// either never reported, or reported-but-never-synced.
    pub dns_resolver_last_sync_at: Option<DBDateTime>,
    /// Consecutive sync-tick failures the resolver's sync loop has recorded,
    /// as of the last heartbeat. Resets to 0 on every successful sync tick,
    /// so a growing value means the node has lost contact with the control
    /// plane's DNS change feed and is serving an increasingly stale zone.
    pub dns_resolver_consecutive_failures: i32,
    /// The resolver's most recent error (a sync tick failure, or a startup
    /// failure if the resolver never came up at all), as of the last
    /// heartbeat. `None` when the resolver is healthy or has never reported.
    pub dns_resolver_last_error: Option<String>,
    /// Number of DNS records the resolver was serving as of the last
    /// heartbeat (from its last successful sync, or its on-disk snapshot if
    /// it hasn't synced yet). `None` means never reported.
    pub dns_resolver_record_count: Option<i32>,
    /// When this node's workloads were failed over for the current outage.
    /// `None` means they have not been: the node is healthy, or it is offline
    /// but still inside the failover grace period (or a failover pass failed
    /// and is being retried). Stamped by the health loop once failover is
    /// durably queued and cleared by the next heartbeat, so failover completes
    /// once per outage and survives a control-plane restart.
    pub failover_at: Option<DBDateTime>,
    /// Operator-controlled opt-in for serving public HTTP/HTTPS on this worker.
    pub public_ingress_enabled: bool,
    /// Last runtime state reported by an ingress-capable agent. `None` means
    /// the worker has not reported support yet.
    pub public_ingress_running: Option<bool>,
    pub public_ingress_last_error: Option<String>,
    pub public_ingress_certificate_count: Option<i32>,
    pub public_ingress_route_count: Option<i32>,
    pub public_ingress_unsupported_route_count: Option<i32>,
    pub public_ingress_unsupported_reasons: Json,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::deployment_containers::Entity")]
    DeploymentContainers,
    #[sea_orm(has_many = "super::external_services::Entity")]
    ExternalServices,
}

impl Related<super::deployment_containers::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::DeploymentContainers.def()
    }
}

impl Related<super::external_services::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ExternalServices.def()
    }
}

impl Model {
    /// Where the control plane reaches this node's workloads (published
    /// container ports, managed services, proxy upstreams, health checks).
    ///
    /// A node that joined with a private address keeps using it, mesh or not:
    /// its workloads are already published there (port bindings are fixed at
    /// container creation), and that network was trusted with them before.
    /// A node that joined with a public address has no such network; its
    /// workloads live on its WireGuard mesh address, so they are never bound
    /// to a public interface and the traffic to them is encrypted.
    pub fn data_address(&self) -> &str {
        match &self.mesh_wg_address {
            Some(mesh)
                if !temps_core::node_address::is_private_node_address(&self.private_address) =>
            {
                mesh
            }
            _ => self.private_address.as_str(),
        }
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        let now = chrono::Utc::now();

        if insert {
            if self.created_at.is_not_set() {
                self.created_at = Set(now);
            }
            if self.updated_at.is_not_set() {
                self.updated_at = Set(now);
            }
        } else {
            self.updated_at = Set(now);
        }

        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(private_address: &str, mesh_wg_address: Option<&str>) -> Model {
        let now = chrono::Utc::now();
        Model {
            id: 1,
            name: "worker-1".into(),
            token_hash: "hash".into(),
            token_encrypted: None,
            address: "https://203.0.113.10:3100".into(),
            private_address: private_address.into(),
            public_endpoint: None,
            wg_public_key: None,
            role: "worker".into(),
            status: "active".into(),
            labels: serde_json::json!({}),
            capacity: serde_json::json!({}),
            last_heartbeat: None,
            edge_public_key: None,
            compute_cidr: None,
            architecture: None,
            underlay_address: None,
            mesh_wg_public_key: None,
            mesh_wg_endpoint: None,
            mesh_wg_address: mesh_wg_address.map(str::to_owned),
            dns_resolver_running: None,
            dns_resolver_tasks_alive: None,
            dns_resolver_last_sync_at: None,
            dns_resolver_consecutive_failures: 0,
            dns_resolver_last_error: None,
            dns_resolver_record_count: None,
            failover_at: None,
            public_ingress_enabled: false,
            public_ingress_running: None,
            public_ingress_last_error: None,
            public_ingress_certificate_count: None,
            public_ingress_route_count: None,
            public_ingress_unsupported_route_count: None,
            public_ingress_unsupported_reasons: serde_json::json!([]),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn a_node_that_joined_privately_keeps_its_private_address() {
        // Mesh or not: its workloads are already published there.
        assert_eq!(node("10.0.0.5", None).data_address(), "10.0.0.5");
        assert_eq!(
            node("10.0.0.5", Some("10.201.0.2")).data_address(),
            "10.0.0.5"
        );
    }

    #[test]
    fn a_node_that_joined_publicly_is_reached_on_its_mesh_address() {
        assert_eq!(
            node("203.0.113.10", Some("10.201.0.2")).data_address(),
            "10.201.0.2"
        );
        // Until it registers on the mesh there is nothing else to use.
        assert_eq!(node("203.0.113.10", None).data_address(), "203.0.113.10");
    }
}
