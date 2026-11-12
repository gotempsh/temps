// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-deployment DNS publisher for the internal `*.temps.local` zone.
//!
//! ADR-012-lite: every environment with a current deployment gets a
//! stable hostname `<env-slug>.<project-slug>.temps.local`. The record
//! is a **multi-A** pointing at every node's bridge gateway IP — i.e.
//! the IP every internal edge-proxy listener binds to. Containers
//! resolve, get back N candidate IPs (one per node), TCP-connect to
//! one (typically the local-bridge IP — same-node, no overlay hop),
//! and the proxy on that node fans out to whichever container is
//! actually serving the deployment right now.
//!
//! This decouples client-side DNS from container churn: redeploys
//! don't change the FQDN's record set, so even DNS-caching clients
//! (JVM `networkaddress.cache.ttl=-1`, every connection-pooling HTTP
//! client) keep working through redeploys.
//!
//! ## Where this fits
//!
//! - **Source of truth:** `environments.current_deployment_id` joined
//!   with `projects.slug` + `environments.slug` and `nodes.compute_cidr`.
//! - **Sink:** [`crate::services::DnsRegistry::replace_endpoints_for_owner`]
//!   under [`OwnerKind::Environment`], with `owner_id = environment.id`.
//!   The environment owns the name (ADR-055 §10): before, the owner was the
//!   deployment, a new id on every redeploy, which made every redeploy
//!   collide with the previous one on the same `(fqdn, A, bridge IP)`.
//! - **Trigger:** called from the same path that reloads the L7 route
//!   table after `current_deployment_id` changes (PG NOTIFY → listener).
//!   Keeps DNS and route-table in lockstep with one source of truth.
//!
//! ## Idempotence
//!
//! [`reconcile_all`] is whole-set idempotent: it computes the desired
//! record set for every active deployment and replaces each owner's
//! records via the registry's atomic per-owner replace. Safe to run on
//! every route reload; the registry only bumps generation when records
//! actually change.
//!
//! ## Multi-A semantics
//!
//! For one environment, we emit N records (one per node) with the same
//! FQDN, all `record_type=A`. The registry's unique index on
//! `(fqdn, record_type, target_ip)` accepts this — DNS clients receive
//! all N answers and pick one per RFC 1035. Hickory's resolver
//! preserves the multi-answer set.

use std::sync::Arc;

use std::collections::HashSet;

use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QuerySelect};
use temps_entities::{deployments, environments, nodes, projects};
use tracing::{debug, info, warn};

use crate::services::dns_registry::{
    DnsRegistry, DnsRegistryError, EndpointDraft, OwnerKind, RecordType,
};

/// TTL on the deployment FQDN. Short enough that node-set changes
/// (worker added / removed) propagate within seconds; long enough that
/// busy clients aren't re-resolving on every connect. Resolvers honor
/// this verbatim.
const DEPLOYMENT_TTL_SECS: i32 = 10;

/// Port advertised in the A record. Internal edge proxy always binds
/// `:80` on the bridge gateway — TLS terminates at the public edge,
/// the internal name is plain HTTP only (overlay-only, not exposed).
/// `target_port` is informational on A records; only used by clients
/// that resolve port via SRV-like pathways. Kept for symmetry with
/// the rest of the registry.
const INTERNAL_PROXY_PORT: i32 = 80;

pub struct DeploymentDnsPublisher {
    db: Arc<DatabaseConnection>,
    registry: Arc<DnsRegistry>,
    /// The control plane's app-network gateway while its resolver serves
    /// there (ADR-104 D3), shared with the deployer. `None` for a publisher
    /// that never points names at the control plane.
    control_plane_gateway: Option<temps_node_dns::OverlayDnsSlot>,
}

/// Where one environment's name points: a node's bridge gateway, or the
/// control plane's app-network gateway (`node_id` `None`, ADR-104 D3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameTarget {
    pub node_id: Option<i32>,
    pub ip: String,
}

/// The addresses every environment's name points at (ADR-104 D3): every
/// worker's bridge gateway, as before; with no worker bridge known, the
/// control plane's app-network gateway while its resolver serves there, so a
/// single-server install's own containers reach the control plane's internal
/// proxy. IPv6 gateways are skipped: the internal names are A records.
pub fn name_targets(
    worker_bridges: Vec<(i32, String)>,
    control_plane_gateway: Option<std::net::IpAddr>,
) -> Vec<NameTarget> {
    if !worker_bridges.is_empty() {
        return worker_bridges
            .into_iter()
            .map(|(node_id, ip)| NameTarget {
                node_id: Some(node_id),
                ip,
            })
            .collect();
    }
    control_plane_gateway
        .filter(std::net::IpAddr::is_ipv4)
        .map(|gateway| NameTarget {
            node_id: None,
            ip: gateway.to_string(),
        })
        .into_iter()
        .collect()
}

impl DeploymentDnsPublisher {
    pub fn new(db: Arc<DatabaseConnection>, registry: Arc<DnsRegistry>) -> Self {
        Self {
            db,
            registry,
            control_plane_gateway: None,
        }
    }

    /// Point names at the control plane when no worker can serve them
    /// (ADR-104 D3). `slot` holds the gateway while the control plane's
    /// resolver serves on it, which is when its internal proxy runs there too.
    pub fn with_control_plane_gateway(mut self, slot: temps_node_dns::OverlayDnsSlot) -> Self {
        self.control_plane_gateway = Some(slot);
        self
    }

    fn current_control_plane_gateway(&self) -> Option<std::net::IpAddr> {
        let slot = self.control_plane_gateway.as_ref()?;
        match slot.read() {
            Ok(gateway) => *gateway,
            Err(_) => {
                warn!(
                    "the control plane's DNS gateway slot is poisoned; not publishing names at it"
                );
                None
            }
        }
    }

    /// Remove every environment record that points at the control plane
    /// (`node_id IS NULL`), once its gateway no longer serves them: cluster
    /// DNS was turned off, or its resolver stopped. Workers' records are left
    /// as they are.
    async fn withdraw_control_plane_names(&self) -> Result<usize, DnsRegistryError> {
        let owners: Vec<i64> = temps_entities::service_endpoints::Entity::find()
            .select_only()
            .column(temps_entities::service_endpoints::Column::OwnerId)
            .filter(
                temps_entities::service_endpoints::Column::OwnerKind
                    .eq(OwnerKind::Environment.as_str()),
            )
            .filter(temps_entities::service_endpoints::Column::NodeId.is_null())
            .distinct()
            .into_tuple::<i64>()
            .all(self.db.as_ref())
            .await?;
        for owner_id in &owners {
            self.registry
                .replace_endpoints_for_owner(OwnerKind::Environment, *owner_id, &[])
                .await?;
        }
        if !owners.is_empty() {
            info!(
                environments = owners.len(),
                "withdrew internal names that pointed at the control plane's gateway"
            );
        }
        Ok(owners.len())
    }

    /// Publish the full deployment-FQDN set. Walks every environment
    /// with a current deployment, computes the multi-A record set
    /// (one record per node bridge gateway), and replaces the
    /// environment owner's records atomically.
    ///
    /// The records are owned by the **environment**, not the deployment
    /// (ADR-055 §10): the name outlives every redeploy, so its owner must
    /// too, or the registry could not tell a redeploy from another owner
    /// taking the name.
    ///
    /// Returns the number of environments whose records were
    /// reconciled. Failures for individual environments are logged
    /// and skipped — one bad row (or a name another owner holds) doesn't
    /// poison the rest.
    pub async fn reconcile_all(&self) -> Result<usize, DnsRegistryError> {
        let bridge_ips = name_targets(
            self.collect_bridge_gateway_ips().await?,
            self.current_control_plane_gateway(),
        );
        if bridge_ips.is_empty() {
            // No node has a usable compute_cidr yet and the control plane
            // does not serve the names either. Nothing to advertise; don't
            // error, it is a fresh cluster or cluster DNS is off. Names that
            // pointed at the control plane go: nothing answers them there.
            if self.control_plane_gateway.is_some() {
                self.withdraw_control_plane_names().await?;
            }
            debug!("no node bridge gateway IPs known; skipping deployment DNS publish");
            return Ok(0);
        }

        // One query: live environments with a current deployment, and
        // their project.
        let active_envs = environments::Entity::find()
            .find_also_related(projects::Entity)
            .filter(environments::Column::CurrentDeploymentId.is_not_null())
            .filter(environments::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        // One query: which of those current deployments still exist.
        // current_deployment_id can briefly point at a deleted row;
        // advertising a name that has no proxy backing is worse than
        // briefly NXDOMAIN'ing.
        let current_ids: Vec<i32> = active_envs
            .iter()
            .filter_map(|(env, _)| env.current_deployment_id)
            .collect();
        let present: HashSet<i32> = if current_ids.is_empty() {
            HashSet::new()
        } else {
            deployments::Entity::find()
                .select_only()
                .column(deployments::Column::Id)
                .filter(deployments::Column::Id.is_in(current_ids))
                .into_tuple::<i32>()
                .all(self.db.as_ref())
                .await?
                .into_iter()
                .collect()
        };

        let mut reconciled = 0;

        for (env, project) in active_envs {
            let Some(deployment_id) = env.current_deployment_id else {
                continue;
            };

            // Skip sleeping environments — their deployment exists
            // on paper but has no live containers, so resolving the
            // hostname would point at a proxy that returns 503 for
            // every request. Better to NXDOMAIN until the env is
            // awake.
            if env.sleeping {
                continue;
            }

            let env_slug = env.slug.trim().to_string();
            if env_slug.is_empty() {
                continue;
            }

            let Some(project) = project else {
                warn!(
                    environment_id = env.id,
                    project_id = env.project_id,
                    "environment references missing project; skipping DNS publish"
                );
                continue;
            };

            let proj_slug = project.slug.trim().to_string();
            if proj_slug.is_empty() {
                continue;
            }

            if !present.contains(&deployment_id) {
                continue;
            }

            let fqdn = format!("{}.{}.temps.local", env_slug, proj_slug);
            let drafts = environment_drafts(env.id, &fqdn, &bridge_ips);

            match self
                .registry
                .replace_endpoints_for_owner(OwnerKind::Environment, env.id as i64, &drafts)
                .await
            {
                Ok(_generation) => {
                    reconciled += 1;
                    debug!(
                        deployment_id,
                        environment_id = env.id,
                        fqdn = %fqdn,
                        nodes = drafts.len(),
                        "published deployment DNS"
                    );
                }
                // Already recorded and logged by the registry, at `warn`
                // the first time only; repeating it on every route reload
                // would flood the log.
                Err(DnsRegistryError::NameClaimed { .. }) => {
                    debug!(
                        environment_id = env.id,
                        fqdn = %fqdn,
                        "deployment DNS name is held by another owner; publish refused"
                    );
                }
                Err(e) => {
                    warn!(
                        deployment_id,
                        environment_id = env.id,
                        fqdn = %fqdn,
                        error = %e,
                        "failed to publish deployment DNS; will retry on next reconcile"
                    );
                }
            }
        }

        if reconciled > 0 {
            info!(
                environments = reconciled,
                nodes = bridge_ips.len(),
                "reconciled internal deployment DNS"
            );
        }
        Ok(reconciled)
    }

    /// Drop all DNS records of an environment's stable name, and release the
    /// name. Called when the environment is torn down, so its hostname stops
    /// resolving and another owner may claim it.
    pub async fn delete_for_environment(
        &self,
        environment_id: i32,
    ) -> Result<(), DnsRegistryError> {
        self.registry
            .delete_by_owner(OwnerKind::Environment, environment_id as i64)
            .await?;
        Ok(())
    }

    /// Read every node's bridge-gateway IP — the `.1` of its
    /// `compute_cidr`. Returns `(node_id, ip_string)` pairs.
    /// Nodes without a `compute_cidr` (not yet provisioned) are
    /// skipped silently; they'll appear once their agent registers.
    async fn collect_bridge_gateway_ips(&self) -> Result<Vec<(i32, String)>, DnsRegistryError> {
        Ok(worker_bridge_gateways(self.db.as_ref()).await?)
    }
}

/// Every worker's bridge gateway, from its `compute_cidr`: where the names
/// point while any is known. Empty on a single-server install, which is
/// when the control plane serves its own containers' names (ADR-104 D1, D3).
pub async fn worker_bridge_gateways<C: sea_orm::ConnectionTrait>(
    db: &C,
) -> Result<Vec<(i32, String)>, sea_orm::DbErr> {
    let nodes = nodes::Entity::find().all(db).await?;
    let mut out = Vec::with_capacity(nodes.len());
    for node in nodes {
        let Some(cidr) = node.compute_cidr.as_deref() else {
            continue;
        };
        match bridge_gateway_from_cidr(cidr) {
            Some(ip) => out.push((node.id, ip)),
            None => {
                warn!(
                    node_id = node.id,
                    cidr = %cidr,
                    "could not derive bridge gateway from compute_cidr"
                );
            }
        }
    }
    Ok(out)
}

/// The multi-A record set for one environment's stable name: one record
/// per node bridge gateway, owned by the environment.
fn environment_drafts(
    environment_id: i32,
    fqdn: &str,
    targets: &[NameTarget],
) -> Vec<EndpointDraft> {
    targets
        .iter()
        .map(|target| EndpointDraft {
            fqdn: fqdn.to_string(),
            record_type: RecordType::A,
            target_ip: Some(target.ip.clone()),
            target_port: Some(INTERNAL_PROXY_PORT),
            ttl: DEPLOYMENT_TTL_SECS,
            owner_kind: OwnerKind::Environment,
            owner_id: environment_id as i64,
            node_id: target.node_id,
        })
        .collect()
}

/// Parse a CIDR string like `172.20.1.0/24` and return the first
/// usable host as a string (`172.20.1.1`). Returns `None` for
/// malformed input or for CIDRs too small to host a gateway.
///
/// We don't validate the prefix length — `bridge_gateway` for any
/// `/24` or larger network is the network address + 1, which matches
/// what `temps-network` actually allocates on the bridge.
fn bridge_gateway_from_cidr(cidr: &str) -> Option<String> {
    let (addr, _prefix) = cidr.split_once('/')?;
    let mut octets: Vec<u8> = addr
        .split('.')
        .map(|s| s.parse::<u8>().ok())
        .collect::<Option<Vec<_>>>()?;
    if octets.len() != 4 {
        return None;
    }
    // Network address has 0 in the host bits; add 1 to get the
    // first usable IP. For all CIDRs the bridge actually uses
    // (/24 or /20), this is the gateway address.
    *octets.last_mut()? = octets.last()?.wrapping_add(1);
    Some(format!(
        "{}.{}.{}.{}",
        octets[0], octets[1], octets[2], octets[3]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_from_24() {
        assert_eq!(
            bridge_gateway_from_cidr("172.20.1.0/24"),
            Some("172.20.1.1".to_string())
        );
        assert_eq!(
            bridge_gateway_from_cidr("172.20.0.0/24"),
            Some("172.20.0.1".to_string())
        );
    }

    #[test]
    fn gateway_from_invalid_input() {
        assert_eq!(bridge_gateway_from_cidr("not-a-cidr"), None);
        assert_eq!(bridge_gateway_from_cidr("172.20.1.0"), None);
        assert_eq!(bridge_gateway_from_cidr("garbage/24"), None);
        assert_eq!(bridge_gateway_from_cidr("999.0.0.0/24"), None);
    }

    fn gateway() -> Option<std::net::IpAddr> {
        Some("172.20.0.1".parse().unwrap())
    }

    #[test]
    fn workers_bridges_win_over_the_control_plane_gateway() {
        // A cluster with workers keeps pointing names at their bridges, as
        // before ADR-104, even while the control plane's resolver serves.
        let targets = name_targets(
            vec![(3, "10.205.0.1".into()), (4, "10.206.0.1".into())],
            gateway(),
        );
        assert_eq!(
            targets,
            vec![
                NameTarget {
                    node_id: Some(3),
                    ip: "10.205.0.1".into()
                },
                NameTarget {
                    node_id: Some(4),
                    ip: "10.206.0.1".into()
                },
            ]
        );
    }

    #[test]
    fn a_single_server_install_points_names_at_its_gateway() {
        assert_eq!(
            name_targets(Vec::new(), gateway()),
            vec![NameTarget {
                node_id: None,
                ip: "172.20.0.1".into()
            }]
        );
    }

    #[test]
    fn nothing_is_published_when_nothing_serves_the_names() {
        // Cluster DNS off, or the control plane's resolver down.
        assert!(name_targets(Vec::new(), None).is_empty());
        // The names are A records; an IPv6 gateway cannot carry them.
        assert!(name_targets(Vec::new(), Some("fd00::1".parse().unwrap())).is_empty());
    }

    #[test]
    fn a_control_plane_record_has_no_node() {
        let drafts = environment_drafts(
            12,
            "production.api.temps.local",
            &name_targets(Vec::new(), gateway()),
        );
        assert_eq!(drafts.len(), 1);
        let draft = &drafts[0];
        assert_eq!(draft.fqdn, "production.api.temps.local");
        assert_eq!(draft.target_ip.as_deref(), Some("172.20.0.1"));
        assert_eq!(draft.target_port, Some(INTERNAL_PROXY_PORT));
        assert_eq!(draft.owner_id, 12);
        assert_eq!(draft.node_id, None);
    }

    #[test]
    fn the_gateway_slot_is_read_at_publish_time() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let slot: temps_node_dns::OverlayDnsSlot = Arc::new(std::sync::RwLock::new(None));
        let publisher = DeploymentDnsPublisher::new(db.clone(), Arc::new(DnsRegistry::new(db)))
            .with_control_plane_gateway(slot.clone());
        assert_eq!(publisher.current_control_plane_gateway(), None);
        *slot.write().unwrap() = gateway();
        assert_eq!(publisher.current_control_plane_gateway(), gateway());
    }
}
