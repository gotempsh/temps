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
//!   under [`OwnerKind::Deployment`], with `owner_id = deployment.id`.
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
//! For one deployment, we emit N records (one per node) with the same
//! FQDN, all `record_type=A`. The registry's unique index on
//! `(fqdn, record_type, target_ip)` accepts this — DNS clients receive
//! all N answers and pick one per RFC 1035. Hickory's resolver
//! preserves the multi-answer set.

use std::sync::Arc;

use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
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
}

impl DeploymentDnsPublisher {
    pub fn new(db: Arc<DatabaseConnection>, registry: Arc<DnsRegistry>) -> Self {
        Self { db, registry }
    }

    /// Publish the full deployment-FQDN set. Walks every environment
    /// with a current deployment, computes the multi-A record set
    /// (one record per node bridge gateway), and replaces the
    /// per-deployment owner's records atomically.
    ///
    /// Returns the number of deployments whose records were
    /// reconciled. Failures for individual deployments are logged
    /// and skipped — one bad row doesn't poison the rest.
    pub async fn reconcile_all(&self) -> Result<usize, DnsRegistryError> {
        let bridge_ips = self.collect_bridge_gateway_ips().await?;
        if bridge_ips.is_empty() {
            // No nodes have a usable compute_cidr yet. Nothing to
            // advertise; the registry stays empty for these owners.
            // Don't error — happens on a fresh cluster before the
            // first agent has registered its CIDR.
            debug!("no node bridge gateway IPs known; skipping deployment DNS publish");
            return Ok(0);
        }

        let active_envs = environments::Entity::find()
            .filter(environments::Column::CurrentDeploymentId.is_not_null())
            .filter(environments::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        let mut reconciled = 0;

        for env in active_envs {
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

            let project = match projects::Entity::find_by_id(env.project_id)
                .one(self.db.as_ref())
                .await?
            {
                Some(p) => p,
                None => {
                    warn!(
                        environment_id = env.id,
                        project_id = env.project_id,
                        "environment references missing project; skipping DNS publish"
                    );
                    continue;
                }
            };

            let proj_slug = project.slug.trim().to_string();
            if proj_slug.is_empty() {
                continue;
            }

            // Confirm the referenced deployment still exists.
            // current_deployment_id can briefly point at a
            // soft-deleted row; advertising a name that has no proxy
            // backing is worse than briefly NXDOMAIN'ing.
            let deployment_present = deployments::Entity::find_by_id(deployment_id)
                .one(self.db.as_ref())
                .await?
                .is_some();
            if !deployment_present {
                continue;
            }

            let fqdn = format!("{}.{}.temps.local", env_slug, proj_slug);
            let drafts: Vec<EndpointDraft> = bridge_ips
                .iter()
                .map(|(node_id, ip)| EndpointDraft {
                    fqdn: fqdn.clone(),
                    record_type: RecordType::A,
                    target_ip: Some(ip.clone()),
                    target_port: Some(INTERNAL_PROXY_PORT),
                    ttl: DEPLOYMENT_TTL_SECS,
                    owner_kind: OwnerKind::Deployment,
                    owner_id: deployment_id as i64,
                    node_id: Some(*node_id),
                })
                .collect();

            match self
                .registry
                .replace_endpoints_for_owner(OwnerKind::Deployment, deployment_id as i64, &drafts)
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
                deployments = reconciled,
                nodes = bridge_ips.len(),
                "reconciled internal deployment DNS"
            );
        }
        Ok(reconciled)
    }

    /// Drop all DNS records for a specific deployment. Called on
    /// teardown so a destroyed deployment's hostname stops resolving.
    pub async fn delete_for_deployment(&self, deployment_id: i32) -> Result<(), DnsRegistryError> {
        self.registry
            .delete_by_owner(OwnerKind::Deployment, deployment_id as i64)
            .await?;
        Ok(())
    }

    /// Read every node's bridge-gateway IP — the `.1` of its
    /// `compute_cidr`. Returns `(node_id, ip_string)` pairs.
    /// Nodes without a `compute_cidr` (not yet provisioned) are
    /// skipped silently; they'll appear once their agent registers.
    async fn collect_bridge_gateway_ips(&self) -> Result<Vec<(i32, String)>, DnsRegistryError> {
        let nodes = nodes::Entity::find().all(self.db.as_ref()).await?;
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
}
