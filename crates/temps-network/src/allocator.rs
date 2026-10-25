// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Control-plane allocator for compute CIDRs and the per-node `Peer` list.
//!
//! Carved out of [`temps_entities::network_config`] + `nodes`:
//!
//! - **Pool**: read once from the singleton `network_config` row
//!   (`compute_pool_cidr`, `subnet_prefix_len`).
//! - **Allocation**: pick the lowest-numbered subnet of the configured
//!   prefix size that no other node already owns, write it to the
//!   `nodes.compute_cidr` column inside a transaction. The partial-unique
//!   index installed by `m20260427_000001_add_compute_network` guarantees
//!   no two nodes ever share a CIDR even under concurrent allocation.
//! - **Peer list**: simple `SELECT … FROM nodes WHERE compute_cidr IS NOT
//!   NULL AND id <> $caller`, mapped to [`Peer`].
//!
//! All public methods return typed errors carrying enough context (node
//! id, CIDR, pool size, exhaustion reason) to debug a misallocation from
//! a single log line.

use crate::config::Peer;
use crate::occupied::{self, OccupiedRange};
use async_trait::async_trait;
use ipnet::Ipv4Net;
use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DatabaseConnection,
    DbErr, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use std::collections::HashSet;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use temps_entities::{network_config as nc, nodes, service_members};
use thiserror::Error;
use tracing::{info, warn};
use uuid::Uuid;

/// Synthetic identity used by the data plane only. The control plane remains
/// absent from the `nodes` table so it can never become a scheduler target.
pub const CONTROL_PLANE_NODE_UUID: Uuid = Uuid::from_u128(0x74656d70732d63702d6f7665726c6179);

/// Errors returned by the compute-network allocator.
#[derive(Debug, Error)]
pub enum AllocatorError {
    /// Node referenced by id does not exist in the `nodes` table.
    #[error("node {node_id} not found")]
    NodeNotFound { node_id: i32 },

    /// The configured pool can't fit another /N subnet.
    #[error(
        "compute pool {pool} (subnets of /{prefix_len}) is exhausted: {used_count} subnets in use"
    )]
    PoolExhausted {
        pool: Ipv4Net,
        prefix_len: u8,
        used_count: usize,
    },

    /// `network_config` row missing or malformed (e.g. invalid CIDR text).
    #[error("network_config is invalid: {reason}")]
    InvalidConfig { reason: String },

    /// The overlay is up on the current pool (ADR 056). Moving a live overlay
    /// means re-addressing every node and container; not supported yet.
    #[error(
        "cannot change the compute pool from {current_pool} to {requested_pool}: the cross-node \
         overlay is in use on it by {}. Moving a live overlay is not supported yet: keep \
         {current_pool}, or remove those nodes, change the pool, and enrol them again",
        users.join("; ")
    )]
    PoolInUse {
        current_pool: Ipv4Net,
        requested_pool: Ipv4Net,
        /// One entry per member using the pool, naming it and why.
        users: Vec<String>,
    },

    /// The control plane holds an overlay allocation that is not ready: its
    /// setup is running, or failed after it changed the host.
    #[error(
        "cannot change the compute pool to {requested_pool} right now: the control plane's \
         overlay setup on {compute_cidr} is in progress or failed after changing this host. \
         Wait a minute and try again; if it persists, fix the control-plane setup failure shown \
         under Cluster network first"
    )]
    ControlPlaneSetupInFlight {
        requested_pool: Ipv4Net,
        compute_cidr: String,
    },

    #[error(
        "compute pool {pool} overlaps the WireGuard mesh pool {mesh}; choose a range outside {mesh}"
    )]
    PoolOverlapsMesh { pool: Ipv4Net, mesh: Ipv4Net },

    #[error(
        "compute pool {pool} contains {member}'s address {address}; choose a range that contains \
         no node's address"
    )]
    PoolOverlapsUnderlay {
        pool: Ipv4Net,
        member: String,
        address: IpAddr,
    },

    #[error(
        "compute pool {pool} overlaps {range} on {member}; choose another range, or move that \
         network on {member} first"
    )]
    PoolOverlapsOccupied {
        pool: Ipv4Net,
        member: String,
        range: String,
    },

    /// A concurrent setup or pool reconfiguration replaced the reservation
    /// this caller prepared. Stale setup attempts must never publish or
    /// withdraw the replacement allocation.
    #[error(
        "control-plane overlay setup for {compute_cidr} via {underlay_address} was superseded by another configuration change"
    )]
    SupersededControlPlaneSetup {
        compute_cidr: Ipv4Net,
        underlay_address: IpAddr,
    },

    #[error(
        "refusing to release ready control-plane allocation {compute_cidr} via {underlay_address}; tear down the cluster explicitly"
    )]
    ReadyControlPlaneRelease {
        compute_cidr: Ipv4Net,
        underlay_address: IpAddr,
    },

    #[error("underlay address {address} is inside compute pool {pool}")]
    UnderlayOverlapsComputePool { address: IpAddr, pool: Ipv4Net },

    /// The node already has a `compute_cidr` allocated.
    #[error("node {node_id} already has compute_cidr {existing}")]
    AlreadyAllocated { node_id: i32, existing: Ipv4Net },

    /// Underlay address must be set before allocation (allocator has no
    /// way to guess the right value — that's a node-registration concern).
    #[error("node {node_id} has no underlay_address; cannot allocate compute_cidr")]
    UnderlayMissing { node_id: i32 },

    /// Persisted underlay address is not a valid IP.
    #[error("node {node_id} has malformed underlay_address {raw:?}: {reason}")]
    UnderlayInvalid {
        node_id: i32,
        raw: String,
        reason: String,
    },

    /// Public addresses would expose VXLAN directly to the internet and are
    /// never valid cluster-underlay endpoints.
    #[error("node {node_id} has publicly-routable underlay address {address}")]
    PublicUnderlayAddress { node_id: i32, address: IpAddr },

    /// Persisted compute_cidr is not a valid IPv4 CIDR.
    #[error("node {node_id} has malformed compute_cidr {raw:?}: {reason}")]
    ComputeCidrInvalid {
        node_id: i32,
        raw: String,
        reason: String,
    },

    /// Sea-ORM / Postgres error.
    #[error("database error: {0}")]
    Database(#[from] DbErr),
}

/// Allocator surface — trait so callers can mock it out in unit tests
/// without spinning up Postgres.
#[async_trait]
pub trait ComputeNetworkAllocator: Send + Sync {
    /// Reserve a CIDR for `node_id` and return the resulting [`NodeAlloc`].
    ///
    /// Idempotent in the sense that calling it twice for a node that
    /// already has an allocation returns [`AllocatorError::AlreadyAllocated`]
    /// rather than producing a second one. Callers should treat that as a
    /// success ("we already have one") and fetch the existing alloc with
    /// [`Self::get_alloc`].
    async fn allocate_for_node(&self, node_id: i32) -> Result<NodeAllocPersisted, AllocatorError>;

    /// Release the CIDR for `node_id` (set the column back to NULL). Safe
    /// to call when no allocation exists.
    async fn release(&self, node_id: i32) -> Result<(), AllocatorError>;

    /// Peer list as seen by `viewer_node_id` — every node with a
    /// `compute_cidr` set, excluding the viewer.
    async fn peer_list(&self, viewer_node_id: i32) -> Result<Vec<Peer>, AllocatorError>;

    /// Fetch the current allocation for a node, if any.
    async fn get_alloc(&self, node_id: i32) -> Result<Option<NodeAllocPersisted>, AllocatorError>;
}

/// Persisted form of a [`crate::NodeAlloc`] — same fields plus the
/// integer database id, which the kernel layer doesn't care about but
/// the control plane uses everywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeAllocPersisted {
    pub node_id: i32,
    /// The opaque uuid the kernel layer logs against. We synthesize a
    /// stable v5 from the integer id so `NodeAlloc.node_id` is always
    /// derivable from the database row.
    pub external_id: Uuid,
    pub compute_cidr: Ipv4Net,
    pub bridge_address: IpAddr,
    pub underlay_address: IpAddr,
}

/// Authoritative cluster-wide pool from the `network_config` singleton.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClusterNetworkConfig {
    pub compute_pool_cidr: Ipv4Net,
    pub subnet_prefix_len: u8,
}

/// Outcome of [`PostgresAllocator::configure_pool`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolChange {
    pub config: ClusterNetworkConfig,
    /// Unapplied allocations moved into the new pool, lowest node id first.
    pub reassigned: Vec<Reassignment>,
}

/// One node's allocation moved by a pool change or an auto-remediation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reassignment {
    pub node_id: i32,
    pub node_name: String,
    pub from: String,
    pub to: Ipv4Net,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlPlaneReservation {
    pub alloc: NodeAllocPersisted,
    pub was_ready: bool,
    pub cluster_config: ClusterNetworkConfig,
    pub setup_generation: i64,
}

impl From<NodeAllocPersisted> for crate::NodeAlloc {
    fn from(p: NodeAllocPersisted) -> Self {
        Self {
            node_id: p.external_id,
            compute_cidr: p.compute_cidr,
            bridge_address: p.bridge_address,
            underlay_address: p.underlay_address,
        }
    }
}

impl From<NodeAllocPersisted> for Peer {
    fn from(p: NodeAllocPersisted) -> Self {
        Self {
            node_id: p.external_id,
            compute_cidr: p.compute_cidr,
            underlay_address: p.underlay_address,
        }
    }
}

/// Postgres-backed implementation. Cheap to clone (`Arc<DatabaseConnection>`).
#[derive(Clone)]
pub struct PostgresAllocator {
    db: Arc<DatabaseConnection>,
}

impl PostgresAllocator {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Read the one authoritative compute pool shared by the control plane
    /// and every worker.
    pub async fn cluster_config(&self) -> Result<ClusterNetworkConfig, AllocatorError> {
        let cfg = nc::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await?
            .ok_or(AllocatorError::InvalidConfig {
                reason: "network_config singleton row missing".into(),
            })?;
        cluster_config_from_model(&cfg)
    }

    /// Change the cluster compute pool (ADR 056). Shared by the console
    /// (`PUT /settings/cluster-network`) and `temps network setup-multi-node`.
    ///
    /// The new pool must be RFC 1918, fit the per-node prefix, and overlap
    /// neither the WireGuard mesh pool, any member's address, nor any
    /// node's reported occupied ranges.
    ///
    /// Allowed while no allocation is in use. An allocation is in use once
    /// its node reported the overlay ready on it (`overlay_ready_cidr`), or
    /// the control plane did. A node that never reported overlay status (an
    /// agent older than ADR 056, or one that has not sent a heartbeat since
    /// it registered) is treated as in use too: nothing proves it has not
    /// applied its allocation. Allocations handed out but never applied are
    /// re-assigned from the new pool in the same transaction; their nodes
    /// pick them up on their next poll.
    pub async fn configure_pool(
        &self,
        requested_pool: Ipv4Net,
        requested_prefix_len: u8,
    ) -> Result<PoolChange, AllocatorError> {
        validate_pool(requested_pool, requested_prefix_len)?;
        let txn = self.db.begin().await?;
        let cfg = nc::Entity::find_by_id(1)
            .lock_exclusive()
            .one(&txn)
            .await?
            .ok_or(AllocatorError::InvalidConfig {
                reason: "network_config singleton row missing".into(),
            })?;
        let current = cluster_config_from_model(&cfg)?;
        let requested = ClusterNetworkConfig {
            compute_pool_cidr: requested_pool,
            subnet_prefix_len: requested_prefix_len,
        };
        if current == requested {
            txn.commit().await?;
            return Ok(PoolChange {
                config: current,
                reassigned: Vec::new(),
            });
        }

        // Lock every node row: a heartbeat must not mark one ready, nor a
        // registration allocate one, between the in-use check and the
        // re-assignment below.
        let members = nodes::Entity::find()
            .order_by_asc(nodes::Column::Id)
            .lock_exclusive()
            .all(&txn)
            .await?;
        validate_pool_against_cluster(requested_pool, &cfg, &members)?;

        let with_services = nodes_with_overlay_members(&txn, &members).await?;
        if cfg.control_plane_overlay_ready {
            let mut users = vec![format!(
                "the control plane ({})",
                cfg.control_plane_compute_cidr
                    .as_deref()
                    .unwrap_or("unknown subnet")
            )];
            users.extend(nodes_using_pool(&members, &with_services));
            return Err(AllocatorError::PoolInUse {
                current_pool: current.compute_pool_cidr,
                requested_pool,
                users,
            });
        }
        let users = nodes_using_pool(&members, &with_services);
        if !users.is_empty() {
            return Err(AllocatorError::PoolInUse {
                current_pool: current.compute_pool_cidr,
                requested_pool,
                users,
            });
        }
        if let Some(compute_cidr) = cfg.control_plane_compute_cidr.clone() {
            return Err(AllocatorError::ControlPlaneSetupInFlight {
                requested_pool,
                compute_cidr,
            });
        }

        // Nothing is in use: every allocation is re-assigned from the new
        // pool, lowest node id first, steering around occupied ranges.
        let allocated: Vec<&nodes::Model> = members
            .iter()
            .filter(|node| node.compute_cidr.is_some())
            .collect();
        if !allocated.is_empty() {
            nodes::Entity::update_many()
                .col_expr(
                    nodes::Column::ComputeCidr,
                    Expr::value(Option::<String>::None),
                )
                .filter(nodes::Column::ComputeCidr.is_not_null())
                .exec(&txn)
                .await?;
        }
        let control_plane_underlay = cfg
            .control_plane_underlay_address
            .as_deref()
            .and_then(|raw| raw.parse::<IpAddr>().ok());
        let mut used: Vec<Ipv4Net> = Vec::with_capacity(allocated.len());
        let mut reassigned = Vec::with_capacity(allocated.len());
        for node in allocated {
            let avoid = AvoidSets::for_node(node.id, &members, control_plane_underlay);
            let (chosen, fit) = pick_tiered(
                requested_pool
                    .subnets(requested_prefix_len)
                    .map_err(|error| AllocatorError::InvalidConfig {
                        reason: format!("compute pool {requested_pool}: {error}"),
                    })?,
                &used,
                &avoid,
            )
            .ok_or(AllocatorError::PoolExhausted {
                pool: requested_pool,
                prefix_len: requested_prefix_len,
                used_count: used.len(),
            })?;
            warn_on_fit(node.id, chosen, fit);
            let from = node.compute_cidr.clone().unwrap_or_default();
            nodes::Entity::update_many()
                .col_expr(
                    nodes::Column::ComputeCidr,
                    Expr::value(Some(chosen.to_string())),
                )
                .col_expr(
                    nodes::Column::OverlayRemediation,
                    Expr::value(Some(format!(
                        "moved from {from} to {chosen}: the cluster compute pool changed to \
                         {requested_pool}"
                    ))),
                )
                .filter(nodes::Column::Id.eq(node.id))
                .exec(&txn)
                .await?;
            used.push(chosen);
            reassigned.push(Reassignment {
                node_id: node.id,
                node_name: node.name.clone(),
                from,
                to: chosen,
            });
        }

        let next_generation = cfg
            .control_plane_setup_generation
            .checked_add(1)
            .ok_or_else(|| AllocatorError::InvalidConfig {
                reason: "control-plane setup generation exhausted".into(),
            })?;
        let mut active: nc::ActiveModel = cfg.into();
        active.compute_pool_cidr = Set(requested_pool.to_string());
        active.subnet_prefix_len = Set(i32::from(requested_prefix_len));
        // No control-plane allocation exists here; clear defensively to keep
        // the singleton internally consistent.
        active.control_plane_compute_cidr = Set(None);
        active.control_plane_underlay_address = Set(None);
        active.control_plane_overlay_ready = Set(false);
        // Fence any setup attempt that captured the previous pool.
        active.control_plane_setup_generation = Set(next_generation);
        active.update(&txn).await?;
        txn.commit().await?;
        info!(
            from = %current.compute_pool_cidr,
            to = %requested_pool,
            prefix_len = requested_prefix_len,
            reassigned = reassigned.len(),
            "cluster compute pool changed"
        );
        Ok(PoolChange {
            config: requested,
            reassigned,
        })
    }

    /// Auto-remediation (ADR 056): a node whose allocation overlaps one of
    /// its own occupied ranges and that never applied it gets a new one.
    ///
    /// Returns the move, or `None` when nothing needed (or could) be done:
    /// no allocation, already applied, no collision, or no subnet in the pool
    /// clear of the node's ranges (logged; the node then reports the overlay
    /// unavailable with the cause and the fixes).
    pub async fn remediate_unapplied_collision(
        &self,
        node_id: i32,
    ) -> Result<Option<Reassignment>, AllocatorError> {
        let txn = self.db.begin().await?;
        let cfg = nc::Entity::find_by_id(1)
            .lock_exclusive()
            .one(&txn)
            .await?
            .ok_or(AllocatorError::InvalidConfig {
                reason: "network_config singleton row missing".into(),
            })?;
        let current = cluster_config_from_model(&cfg)?;
        let members = nodes::Entity::find()
            .order_by_asc(nodes::Column::Id)
            .all(&txn)
            .await?;
        let Some(node) = members.iter().find(|node| node.id == node_id) else {
            return Err(AllocatorError::NodeNotFound { node_id });
        };
        let Some(raw) = node.compute_cidr.as_deref() else {
            return Ok(None);
        };
        if node.overlay_ready_cidr.as_deref() == Some(raw) {
            return Ok(None);
        }
        let allocated = parse_cidr(raw).map_err(|error| AllocatorError::ComputeCidrInvalid {
            node_id,
            raw: raw.to_owned(),
            reason: error.to_string(),
        })?;
        let own = occupied_of(node);
        let Some(collision) = occupied::first_overlap(&allocated, &own) else {
            return Ok(None);
        };
        let cause = collision.describe();
        if nodes_with_overlay_members(&txn, std::slice::from_ref(node))
            .await?
            .contains(&node_id)
        {
            warn!(
                node_id,
                compute_cidr = %allocated,
                cause = %cause,
                "not moving the node's colliding overlay allocation: services on it already hold \
                 addresses in it, and moving it would leave them unreachable at those addresses; \
                 free the range on the node"
            );
            return Ok(None);
        }

        let mut used = Vec::with_capacity(members.len());
        for other in members.iter().filter(|other| other.id != node_id) {
            if let Some(raw) = other.compute_cidr.as_deref() {
                used.push(
                    parse_cidr(raw).map_err(|error| AllocatorError::ComputeCidrInvalid {
                        node_id: other.id,
                        raw: raw.to_owned(),
                        reason: error.to_string(),
                    })?,
                );
            }
        }
        if let Some(raw) = cfg.control_plane_compute_cidr.as_deref() {
            used.push(
                parse_cidr(raw).map_err(|error| AllocatorError::ComputeCidrInvalid {
                    node_id: 0,
                    raw: raw.to_owned(),
                    reason: error.to_string(),
                })?,
            );
        }
        let control_plane_underlay = cfg
            .control_plane_underlay_address
            .as_deref()
            .and_then(|raw| raw.parse::<IpAddr>().ok());
        let avoid = AvoidSets::for_node(node_id, &members, control_plane_underlay);
        let subnets = current
            .compute_pool_cidr
            .subnets(current.subnet_prefix_len)
            .map_err(|error| AllocatorError::InvalidConfig {
                reason: format!("compute pool {}: {error}", current.compute_pool_cidr),
            })?;
        let chosen = match pick_tiered(subnets, &used, &avoid) {
            Some((chosen, fit)) if fit != Fit::OverlapsOwn => {
                warn_on_fit(node_id, chosen, fit);
                chosen
            }
            _ => {
                warn!(
                    node_id,
                    compute_cidr = %allocated,
                    cause = %cause,
                    pool = %current.compute_pool_cidr,
                    "cannot auto-remediate the node's colliding overlay allocation: no subnet of \
                     the pool is clear of its occupied ranges; change the cluster pool or free \
                     the range on the node"
                );
                return Ok(None);
            }
        };
        let note = format!("moved from {allocated} to {chosen}: {allocated} overlaps {cause}");
        // Compare-and-set: a heartbeat that just reported the overlay ready on
        // this allocation wins, and the node keeps it.
        let moved = nodes::Entity::update_many()
            .col_expr(
                nodes::Column::ComputeCidr,
                Expr::value(Some(chosen.to_string())),
            )
            .col_expr(nodes::Column::OverlayRemediation, Expr::value(Some(note)))
            .filter(nodes::Column::Id.eq(node_id))
            .filter(nodes::Column::ComputeCidr.eq(raw))
            .filter(
                Condition::any()
                    .add(nodes::Column::OverlayReadyCidr.is_null())
                    .add(nodes::Column::OverlayReadyCidr.ne(raw)),
            )
            // Same statement as the move, so a service given an address
            // since the check above still keeps the node where it is.
            .filter(Expr::cust_with_values(
                "NOT EXISTS (SELECT 1 FROM service_members sm \
                 WHERE sm.node_id = $1 AND sm.compute_ip IS NOT NULL)",
                [node_id],
            ))
            .exec(&txn)
            .await?;
        txn.commit().await?;
        if moved.rows_affected == 0 {
            return Ok(None);
        }
        info!(
            node_id,
            from = %allocated,
            to = %chosen,
            cause = %cause,
            "auto-remediated an unapplied overlay allocation that collided with the node's own network"
        );
        Ok(Some(Reassignment {
            node_id,
            node_name: node.name.clone(),
            from: allocated.to_string(),
            to: chosen,
        }))
    }

    /// Reserve (or refresh) the control plane's stable overlay allocation.
    ///
    /// The highest free subnet is used so upgrades of existing clusters do
    /// not collide with workers, which historically allocate from the bottom
    /// of the pool. The value is persisted in `network_config`, making this
    /// operation safe to run repeatedly from startup and `temps network setup`.
    pub async fn ensure_control_plane_alloc(
        &self,
        underlay_address: IpAddr,
    ) -> Result<NodeAllocPersisted, AllocatorError> {
        Ok(self
            .ensure_control_plane_reservation(underlay_address)
            .await?
            .alloc)
    }

    pub async fn ensure_control_plane_reservation(
        &self,
        underlay_address: IpAddr,
    ) -> Result<ControlPlaneReservation, AllocatorError> {
        let txn = self.db.begin().await?;
        let cfg = nc::Entity::find_by_id(1)
            .lock_exclusive()
            .one(&txn)
            .await?
            .ok_or(AllocatorError::InvalidConfig {
                reason: "network_config singleton row missing".into(),
            })?;

        let pool =
            parse_cidr(&cfg.compute_pool_cidr).map_err(|error| AllocatorError::InvalidConfig {
                reason: format!("compute_pool_cidr: {error}"),
            })?;
        let prefix_len =
            u8::try_from(cfg.subnet_prefix_len).map_err(|_| AllocatorError::InvalidConfig {
                reason: format!("subnet_prefix_len {} out of range", cfg.subnet_prefix_len),
            })?;
        validate_subnet_prefix(pool, prefix_len)?;
        validate_underlay_outside_pool(underlay_address, pool)?;
        let cluster_config = ClusterNetworkConfig {
            compute_pool_cidr: pool,
            subnet_prefix_len: prefix_len,
        };

        let previous_cidr = cfg.control_plane_compute_cidr.clone();
        let previous_underlay = cfg.control_plane_underlay_address.clone();
        let was_ready = cfg.control_plane_overlay_ready;
        let cidr = if let Some(raw) = previous_cidr.as_deref() {
            parse_cidr(raw).map_err(|error| AllocatorError::ComputeCidrInvalid {
                node_id: 0,
                raw: raw.to_owned(),
                reason: error.to_string(),
            })?
        } else {
            let members = nodes::Entity::find()
                .order_by_asc(nodes::Column::Id)
                .all(&txn)
                .await?;
            let mut used = Vec::with_capacity(members.len());
            for member in &members {
                let Some(raw) = member.compute_cidr.as_deref() else {
                    continue;
                };
                used.push(
                    parse_cidr(raw).map_err(|error| AllocatorError::ComputeCidrInvalid {
                        node_id: member.id,
                        raw: raw.to_owned(),
                        reason: error.to_string(),
                    })?,
                );
            }
            // Every node routes to the control plane's subnet, so it avoids
            // their occupied ranges too, highest first.
            let avoid = AvoidSets::for_control_plane(&members);
            let (cidr, fit) = pick_tiered(highest_first(pool, prefix_len), &used, &avoid).ok_or(
                AllocatorError::PoolExhausted {
                    pool,
                    prefix_len,
                    used_count: used.len(),
                },
            )?;
            warn_on_fit(0, cidr, fit);
            cidr
        };

        let cidr_string = cidr.to_string();
        let underlay_string = underlay_address.to_string();
        let unchanged = previous_cidr.as_deref() == Some(cidr_string.as_str())
            && previous_underlay.as_deref() == Some(underlay_string.as_str());
        let setup_generation = cfg
            .control_plane_setup_generation
            .checked_add(1)
            .ok_or_else(|| AllocatorError::InvalidConfig {
                reason: "control-plane setup generation exhausted".into(),
            })?;
        let mut active: nc::ActiveModel = cfg.into();
        active.control_plane_compute_cidr = Set(Some(cidr_string));
        active.control_plane_underlay_address = Set(Some(underlay_string));
        // A reservation is not a routable peer until privileged setup has
        // completed. Preserve readiness only for an unchanged allocation.
        active.control_plane_overlay_ready = Set(was_ready && unchanged);
        active.control_plane_setup_generation = Set(setup_generation);
        active.update(&txn).await?;
        txn.commit().await?;

        Ok(ControlPlaneReservation {
            alloc: NodeAllocPersisted {
                node_id: 0,
                external_id: CONTROL_PLANE_NODE_UUID,
                compute_cidr: cidr,
                bridge_address: bridge_address_for(&cidr),
                underlay_address,
            },
            was_ready: was_ready && unchanged,
            cluster_config,
            setup_generation,
        })
    }

    /// Publish or withdraw exactly the reservation this setup attempt owns.
    /// The compare-and-set filters fence stale concurrent setup attempts.
    pub async fn set_control_plane_ready_for(
        &self,
        reservation: &ControlPlaneReservation,
        ready: bool,
    ) -> Result<(), AllocatorError> {
        let result = nc::Entity::update_many()
            .col_expr(nc::Column::ControlPlaneOverlayReady, Expr::value(ready))
            .filter(nc::Column::Id.eq(1))
            .filter(
                nc::Column::ControlPlaneComputeCidr
                    .eq(Some(reservation.alloc.compute_cidr.to_string())),
            )
            .filter(
                nc::Column::ControlPlaneUnderlayAddress
                    .eq(Some(reservation.alloc.underlay_address.to_string())),
            )
            .filter(
                nc::Column::ComputePoolCidr
                    .eq(reservation.cluster_config.compute_pool_cidr.to_string()),
            )
            .filter(
                nc::Column::SubnetPrefixLen
                    .eq(i32::from(reservation.cluster_config.subnet_prefix_len)),
            )
            .filter(nc::Column::ControlPlaneSetupGeneration.eq(reservation.setup_generation))
            .exec(self.db.as_ref())
            .await?;
        if result.rows_affected == 0 {
            return Err(AllocatorError::SupersededControlPlaneSetup {
                compute_cidr: reservation.alloc.compute_cidr,
                underlay_address: reservation.alloc.underlay_address,
            });
        }
        Ok(())
    }

    /// Release a failed, unpublished first-setup reservation. The generation
    /// and topology comparison fence concurrent setup attempts; a ready
    /// control plane is never removed by failure recovery.
    pub async fn release_unready_control_plane_reservation(
        &self,
        reservation: &ControlPlaneReservation,
    ) -> Result<(), AllocatorError> {
        let txn = self.db.begin().await?;
        let cfg = nc::Entity::find_by_id(1)
            .lock_exclusive()
            .one(&txn)
            .await?
            .ok_or(AllocatorError::InvalidConfig {
                reason: "network_config singleton row missing".into(),
            })?;
        let matches_reservation = cfg.control_plane_compute_cidr.as_deref()
            == Some(reservation.alloc.compute_cidr.to_string().as_str())
            && cfg.control_plane_underlay_address.as_deref()
                == Some(reservation.alloc.underlay_address.to_string().as_str())
            && cfg.compute_pool_cidr == reservation.cluster_config.compute_pool_cidr.to_string()
            && cfg.subnet_prefix_len == i32::from(reservation.cluster_config.subnet_prefix_len)
            && cfg.control_plane_setup_generation == reservation.setup_generation;
        if !matches_reservation {
            return Err(AllocatorError::SupersededControlPlaneSetup {
                compute_cidr: reservation.alloc.compute_cidr,
                underlay_address: reservation.alloc.underlay_address,
            });
        }
        if cfg.control_plane_overlay_ready {
            return Err(AllocatorError::ReadyControlPlaneRelease {
                compute_cidr: reservation.alloc.compute_cidr,
                underlay_address: reservation.alloc.underlay_address,
            });
        }
        let next_generation = cfg
            .control_plane_setup_generation
            .checked_add(1)
            .ok_or_else(|| AllocatorError::InvalidConfig {
                reason: "control-plane setup generation exhausted".into(),
            })?;
        let mut active: nc::ActiveModel = cfg.into();
        active.control_plane_compute_cidr = Set(None);
        active.control_plane_underlay_address = Set(None);
        active.control_plane_overlay_ready = Set(false);
        active.control_plane_setup_generation = Set(next_generation);
        active.update(&txn).await?;
        txn.commit().await?;
        Ok(())
    }

    /// Return the persisted control-plane allocation, if networking has been
    /// configured for it.
    pub async fn get_control_plane_alloc(
        &self,
    ) -> Result<Option<NodeAllocPersisted>, AllocatorError> {
        let Some(cfg) = nc::Entity::find_by_id(1).one(self.db.as_ref()).await? else {
            return Err(AllocatorError::InvalidConfig {
                reason: "network_config singleton row missing".into(),
            });
        };
        if !cfg.control_plane_overlay_ready {
            return Ok(None);
        }
        let (Some(cidr_raw), Some(underlay_raw)) = (
            cfg.control_plane_compute_cidr,
            cfg.control_plane_underlay_address,
        ) else {
            return Ok(None);
        };
        let cidr = parse_cidr(&cidr_raw).map_err(|error| AllocatorError::ComputeCidrInvalid {
            node_id: 0,
            raw: cidr_raw,
            reason: error.to_string(),
        })?;
        let underlay_address =
            underlay_raw
                .parse()
                .map_err(
                    |error: std::net::AddrParseError| AllocatorError::UnderlayInvalid {
                        node_id: 0,
                        raw: underlay_raw,
                        reason: error.to_string(),
                    },
                )?;
        validate_private_underlay(0, underlay_address)?;
        Ok(Some(NodeAllocPersisted {
            node_id: 0,
            external_id: CONTROL_PLANE_NODE_UUID,
            compute_cidr: cidr,
            bridge_address: bridge_address_for(&cidr),
            underlay_address,
        }))
    }

    /// Worker peers as seen by the control plane.
    pub async fn control_plane_peer_list(&self) -> Result<Vec<Peer>, AllocatorError> {
        self.worker_peers(None).await
    }

    async fn worker_peers(
        &self,
        excluded_node_id: Option<i32>,
    ) -> Result<Vec<Peer>, AllocatorError> {
        let mut query = nodes::Entity::find()
            .filter(nodes::Column::ComputeCidr.is_not_null())
            .filter(nodes::Column::UnderlayAddress.is_not_null())
            .order_by_asc(nodes::Column::Id);
        if let Some(node_id) = excluded_node_id {
            query = query.filter(nodes::Column::Id.ne(node_id));
        }
        let rows = query.all(self.db.as_ref()).await?;
        let mut peers = Vec::with_capacity(rows.len());
        for row in rows {
            let id = row.id;
            let cidr_raw = row.compute_cidr.unwrap_or_default();
            let underlay_raw = row.underlay_address.unwrap_or_default();
            let compute_cidr =
                parse_cidr(&cidr_raw).map_err(|error| AllocatorError::ComputeCidrInvalid {
                    node_id: id,
                    raw: cidr_raw,
                    reason: error.to_string(),
                })?;
            let underlay_address =
                underlay_raw
                    .parse()
                    .map_err(
                        |error: std::net::AddrParseError| AllocatorError::UnderlayInvalid {
                            node_id: id,
                            raw: underlay_raw,
                            reason: error.to_string(),
                        },
                    )?;
            peers.push(Peer {
                node_id: Uuid::new_v5(&Uuid::NAMESPACE_OID, format!("temps-node-{id}").as_bytes()),
                compute_cidr,
                underlay_address,
            });
        }
        Ok(peers)
    }
}

#[async_trait]
impl ComputeNetworkAllocator for PostgresAllocator {
    async fn allocate_for_node(&self, node_id: i32) -> Result<NodeAllocPersisted, AllocatorError> {
        let txn = self.db.begin().await?;

        // 1. Load the cluster network config (singleton row id = 1).
        let cfg = nc::Entity::find().lock_exclusive().one(&txn).await?.ok_or(
            AllocatorError::InvalidConfig {
                reason: "network_config singleton row missing".into(),
            },
        )?;

        let pool =
            parse_cidr(&cfg.compute_pool_cidr).map_err(|e| AllocatorError::InvalidConfig {
                reason: format!("compute_pool_cidr: {}", e),
            })?;
        let prefix_len =
            u8::try_from(cfg.subnet_prefix_len).map_err(|_| AllocatorError::InvalidConfig {
                reason: format!("subnet_prefix_len {} out of range", cfg.subnet_prefix_len),
            })?;
        validate_subnet_prefix(pool, prefix_len)?;

        // 2. Load the target node and verify preconditions.
        let node = nodes::Entity::find_by_id(node_id)
            .one(&txn)
            .await?
            .ok_or(AllocatorError::NodeNotFound { node_id })?;

        if let Some(existing) = node.compute_cidr.as_deref() {
            let parsed = parse_cidr(existing).map_err(|e| AllocatorError::ComputeCidrInvalid {
                node_id,
                raw: existing.into(),
                reason: e.to_string(),
            })?;
            return Err(AllocatorError::AlreadyAllocated {
                node_id,
                existing: parsed,
            });
        }

        let underlay_raw = node
            .underlay_address
            .clone()
            .ok_or(AllocatorError::UnderlayMissing { node_id })?;
        let underlay: IpAddr = underlay_raw
            .parse()
            .map_err(
                |e: std::net::AddrParseError| AllocatorError::UnderlayInvalid {
                    node_id,
                    raw: underlay_raw.clone(),
                    reason: e.to_string(),
                },
            )?;
        validate_underlay_outside_pool(underlay, pool)?;
        // Public-underlay worker clusters predate control-plane overlay
        // participation and must keep working on upgrade. Once the control
        // plane is advertised as a peer, however, every newly allocated node
        // must be on the same private underlay; accepting a public endpoint
        // would create an unreachable mixed topology and expose VXLAN.
        if cfg.control_plane_overlay_ready {
            validate_private_underlay(node_id, underlay)?;
        }

        // 3. Load every member: their allocations are taken, and their
        //    occupied ranges and addresses are steered around (ADR 056).
        let members = nodes::Entity::find()
            .order_by_asc(nodes::Column::Id)
            .all(&txn)
            .await?;
        let mut used: Vec<Ipv4Net> = Vec::with_capacity(members.len() + 1);
        for member in &members {
            let Some(raw) = member.compute_cidr.as_deref() else {
                continue;
            };
            // A malformed row would silently shadow a valid free subnet;
            // surface it loudly instead of carrying on.
            used.push(
                parse_cidr(raw).map_err(|e| AllocatorError::ComputeCidrInvalid {
                    node_id: member.id,
                    raw: raw.to_owned(),
                    reason: e.to_string(),
                })?,
            );
        }
        if let Some(raw) = cfg.control_plane_compute_cidr.as_deref() {
            used.push(
                parse_cidr(raw).map_err(|error| AllocatorError::ComputeCidrInvalid {
                    node_id: 0,
                    raw: raw.to_owned(),
                    reason: error.to_string(),
                })?,
            );
        }
        let control_plane_underlay = cfg
            .control_plane_underlay_address
            .as_deref()
            .and_then(|raw| raw.parse::<IpAddr>().ok());
        let avoid = AvoidSets::for_node(node_id, &members, control_plane_underlay);

        // 4. The lowest free subnet that overlaps no member's occupied
        //    ranges or address; failing that, the best available.
        let subnets = pool
            .subnets(prefix_len)
            .map_err(|error| AllocatorError::InvalidConfig {
                reason: format!("compute pool {pool}: {error}"),
            })?;
        let (chosen, fit) =
            pick_tiered(subnets, &used, &avoid).ok_or(AllocatorError::PoolExhausted {
                pool,
                prefix_len,
                used_count: used.len(),
            })?;
        warn_on_fit(node_id, chosen, fit);
        let bridge = bridge_address_for(&chosen);

        // 5. Persist. The partial-unique index on compute_cidr is the
        //    backstop against a concurrent allocator picking the same
        //    subnet — we'd hit `RecordNotInserted` on conflict; but
        //    inside a SERIALIZABLE-equivalent of REPEATABLE READ + the
        //    transactional select the race window is empty in practice.
        let mut active: nodes::ActiveModel = node.clone().into();
        active.compute_cidr = Set(Some(chosen.to_string()));
        active.update(&txn).await?;

        txn.commit().await?;

        let external_id = Uuid::new_v5(
            &Uuid::NAMESPACE_OID,
            format!("temps-node-{}", node_id).as_bytes(),
        );
        info!(node_id, %chosen, %bridge, "compute_cidr allocated");
        Ok(NodeAllocPersisted {
            node_id,
            external_id,
            compute_cidr: chosen,
            bridge_address: bridge,
            underlay_address: underlay,
        })
    }

    async fn release(&self, node_id: i32) -> Result<(), AllocatorError> {
        nodes::Entity::update_many()
            .col_expr(
                nodes::Column::ComputeCidr,
                Expr::value(Option::<String>::None),
            )
            .filter(nodes::Column::Id.eq(node_id))
            .exec(self.db.as_ref())
            .await?;
        info!(node_id, "compute_cidr released");
        Ok(())
    }

    async fn peer_list(&self, viewer_node_id: i32) -> Result<Vec<Peer>, AllocatorError> {
        let mut peers = self.worker_peers(Some(viewer_node_id)).await?;
        if let Some(control_plane) = self.get_control_plane_alloc().await? {
            peers.push(control_plane.into());
        }
        Ok(peers)
    }

    async fn get_alloc(&self, node_id: i32) -> Result<Option<NodeAllocPersisted>, AllocatorError> {
        let Some(node) = nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await?
        else {
            return Ok(None);
        };

        let Some(cidr_raw) = node.compute_cidr.as_deref() else {
            return Ok(None);
        };
        let cidr = parse_cidr(cidr_raw).map_err(|e| AllocatorError::ComputeCidrInvalid {
            node_id,
            raw: cidr_raw.into(),
            reason: e.to_string(),
        })?;
        let underlay_raw = node
            .underlay_address
            .clone()
            .ok_or(AllocatorError::UnderlayMissing { node_id })?;
        let underlay: IpAddr = underlay_raw
            .parse()
            .map_err(
                |e: std::net::AddrParseError| AllocatorError::UnderlayInvalid {
                    node_id,
                    raw: underlay_raw.clone(),
                    reason: e.to_string(),
                },
            )?;
        let external_id = Uuid::new_v5(
            &Uuid::NAMESPACE_OID,
            format!("temps-node-{}", node_id).as_bytes(),
        );
        Ok(Some(NodeAllocPersisted {
            node_id,
            external_id,
            compute_cidr: cidr,
            bridge_address: bridge_address_for(&cidr),
            underlay_address: underlay,
        }))
    }
}

fn validate_subnet_prefix(pool: Ipv4Net, prefix_len: u8) -> Result<(), AllocatorError> {
    if prefix_len <= pool.prefix_len() || prefix_len > 30 {
        return Err(AllocatorError::InvalidConfig {
            reason: format!(
                "subnet_prefix_len {} must be greater than the pool prefix {} and <= 30 so each node has a gateway and container address",
                prefix_len,
                pool.prefix_len()
            ),
        });
    }
    if prefix_len - pool.prefix_len() > 20 {
        return Err(AllocatorError::InvalidConfig {
            reason: format!(
                "compute pool {pool} split into /{prefix_len} would create more than 1,048,576 subnets"
            ),
        });
    }
    Ok(())
}

fn validate_underlay_outside_pool(address: IpAddr, pool: Ipv4Net) -> Result<(), AllocatorError> {
    if let IpAddr::V4(address) = address {
        if pool.contains(&address) {
            return Err(AllocatorError::UnderlayOverlapsComputePool {
                address: IpAddr::V4(address),
                pool,
            });
        }
    }
    Ok(())
}

fn validate_pool(pool: Ipv4Net, prefix_len: u8) -> Result<(), AllocatorError> {
    if !pool.network().is_private() || !pool.broadcast().is_private() {
        return Err(AllocatorError::InvalidConfig {
            reason: format!("compute pool {pool} must be entirely within RFC1918 private space"),
        });
    }
    validate_subnet_prefix(pool, prefix_len)
}

fn cluster_config_from_model(cfg: &nc::Model) -> Result<ClusterNetworkConfig, AllocatorError> {
    let pool =
        parse_cidr(&cfg.compute_pool_cidr).map_err(|error| AllocatorError::InvalidConfig {
            reason: format!("compute_pool_cidr: {error}"),
        })?;
    let prefix_len =
        u8::try_from(cfg.subnet_prefix_len).map_err(|_| AllocatorError::InvalidConfig {
            reason: format!("subnet_prefix_len {} out of range", cfg.subnet_prefix_len),
        })?;
    validate_pool(pool, prefix_len)?;
    Ok(ClusterNetworkConfig {
        compute_pool_cidr: pool,
        subnet_prefix_len: prefix_len,
    })
}

pub(crate) fn is_private_underlay(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => address.is_private() || address.is_link_local(),
        IpAddr::V6(address) => address.is_unique_local() || address.is_unicast_link_local(),
    }
}

fn validate_private_underlay(node_id: i32, address: IpAddr) -> Result<(), AllocatorError> {
    if is_private_underlay(address) {
        Ok(())
    } else {
        Err(AllocatorError::PublicUnderlayAddress { node_id, address })
    }
}

/// A node's reported occupied ranges. A malformed column (hand-edited, or
/// from a future agent) is ignored rather than blocking allocation: the node
/// still preflights every route itself.
pub fn occupied_of(node: &nodes::Model) -> Vec<OccupiedRange> {
    let Some(raw) = node.occupied_ranges.as_ref() else {
        return Vec::new();
    };
    match serde_json::from_value::<Vec<OccupiedRange>>(raw.clone()) {
        Ok(ranges) => ranges,
        Err(error) => {
            warn!(node_id = node.id, %error, "ignoring malformed nodes.occupied_ranges");
            Vec::new()
        }
    }
}

/// Whether a node's allocation is in use, and why, as one line naming it.
/// `None` when it may be re-assigned. `with_services` comes from
/// [`nodes_with_overlay_members`]. See [`PostgresAllocator::configure_pool`].
pub fn allocation_in_use(node: &nodes::Model, with_services: &HashSet<i32>) -> Option<String> {
    let cidr = node.compute_cidr.as_deref()?;
    if node.overlay_ready_cidr.as_deref() == Some(cidr) {
        return Some(format!("node '{}' ({cidr}, overlay up)", node.name));
    }
    if with_services.contains(&node.id) {
        return Some(format!(
            "node '{}' ({cidr}, runs services that hold addresses on it)",
            node.name
        ));
    }
    if node.overlay_status.is_none() {
        return Some(format!(
            "node '{}' ({cidr}, has not reported its overlay status; upgrade its agent or wait \
             for its next heartbeat)",
            node.name
        ));
    }
    None
}

/// Nodes whose service members already hold an overlay address. Their
/// allocation is in use even before the node reports the overlay ready:
/// `service_members.compute_ip`, and the DNS records built from it, would
/// keep pointing at the old subnet after a move.
pub async fn nodes_with_overlay_members<C: ConnectionTrait>(
    db: &C,
    members: &[nodes::Model],
) -> Result<HashSet<i32>, DbErr> {
    if members.is_empty() {
        return Ok(HashSet::new());
    }
    let rows: Vec<Option<i32>> = service_members::Entity::find()
        .select_only()
        .column(service_members::Column::NodeId)
        .distinct()
        .filter(service_members::Column::NodeId.is_in(members.iter().map(|node| node.id)))
        .filter(service_members::Column::ComputeIp.is_not_null())
        .into_tuple()
        .all(db)
        .await?;
    Ok(rows.into_iter().flatten().collect())
}

fn nodes_using_pool(members: &[nodes::Model], with_services: &HashSet<i32>) -> Vec<String> {
    members
        .iter()
        .filter_map(|node| allocation_in_use(node, with_services))
        .collect()
}

/// Refuse a pool that would collide with the cluster itself: the mesh pool,
/// any member's address, or a node's reported occupied ranges.
fn validate_pool_against_cluster(
    pool: Ipv4Net,
    cfg: &nc::Model,
    members: &[nodes::Model],
) -> Result<(), AllocatorError> {
    // The mesh pool is checked even while the mesh is off: it is the range
    // the mesh takes when it is turned on.
    if let Ok(mesh) = parse_cidr(&cfg.wireguard_cidr) {
        if crate::config::cidrs_overlap(&mesh, &pool) {
            return Err(AllocatorError::PoolOverlapsMesh { pool, mesh });
        }
    }
    let control_plane = cfg
        .control_plane_underlay_address
        .as_deref()
        .and_then(|raw| raw.parse::<IpAddr>().ok());
    if let Some(IpAddr::V4(address)) = control_plane {
        if pool.contains(&address) {
            return Err(AllocatorError::PoolOverlapsUnderlay {
                pool,
                member: "the control plane".into(),
                address: IpAddr::V4(address),
            });
        }
    }
    for node in members {
        let addresses = [
            node.underlay_address.as_deref(),
            node.mesh_wg_address.as_deref(),
            Some(node.private_address.as_str()),
        ];
        for address in addresses.into_iter().flatten() {
            if let Ok(IpAddr::V4(address)) = address.parse::<IpAddr>() {
                if pool.contains(&address) {
                    return Err(AllocatorError::PoolOverlapsUnderlay {
                        pool,
                        member: format!("node '{}'", node.name),
                        address: IpAddr::V4(address),
                    });
                }
            }
        }
        if let Some(range) = occupied::first_overlap(&pool, &occupied_of(node)) {
            return Err(AllocatorError::PoolOverlapsOccupied {
                pool,
                member: format!("node '{}'", node.name),
                range: range.describe(),
            });
        }
    }
    Ok(())
}

/// Ranges an allocation for one node should stay clear of.
#[derive(Debug, Default)]
pub(crate) struct AvoidSets {
    /// The node's own occupied ranges: overlapping one makes its overlay
    /// fail outright.
    own: Vec<Ipv4Net>,
    /// Other members' occupied ranges and every member's address: an overlap
    /// breaks routing to this node from that member.
    others: Vec<Ipv4Net>,
}

impl AvoidSets {
    fn for_node(node_id: i32, members: &[nodes::Model], control_plane: Option<IpAddr>) -> Self {
        let mut sets = Self::default();
        for member in members {
            let ranges = occupied_of(member).into_iter().map(|range| range.cidr);
            if member.id == node_id {
                sets.own.extend(ranges);
            } else {
                sets.others.extend(ranges);
            }
            let addresses = [
                member.underlay_address.as_deref(),
                member.mesh_wg_address.as_deref(),
            ];
            sets.others.extend(
                addresses
                    .into_iter()
                    .flatten()
                    .filter_map(|raw| raw.parse::<IpAddr>().ok())
                    .filter_map(host_net),
            );
        }
        sets.others.extend(control_plane.and_then(host_net));
        sets
    }

    /// For the control plane: every node's occupied ranges and address.
    fn for_control_plane(members: &[nodes::Model]) -> Self {
        Self::for_node(0, members, None)
    }
}

fn host_net(address: IpAddr) -> Option<Ipv4Net> {
    match address {
        IpAddr::V4(address) => Ipv4Net::new(address, 32).ok(),
        IpAddr::V6(_) => None,
    }
}

/// How well a chosen subnet avoided known collisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fit {
    Clear,
    /// Overlaps another member's occupied range or address (no clear subnet).
    OverlapsOtherMembers,
    /// Overlaps the node's own occupied ranges (nothing better is free).
    OverlapsOwn,
}

/// The first free candidate clear of everything; failing that, the first one
/// clear of the node's own ranges; failing that, the first free one.
pub(crate) fn pick_tiered(
    candidates: impl Iterator<Item = Ipv4Net>,
    used: &[Ipv4Net],
    avoid: &AvoidSets,
) -> Option<(Ipv4Net, Fit)> {
    let clear_of = |candidate: &Ipv4Net, set: &[Ipv4Net]| {
        !set.iter()
            .any(|range| crate::config::cidrs_overlap(range, candidate))
    };
    let mut clear_of_own = None;
    let mut first_free = None;
    for candidate in candidates {
        if !clear_of(&candidate, used) {
            continue;
        }
        if clear_of(&candidate, &avoid.own) {
            if clear_of(&candidate, &avoid.others) {
                return Some((candidate, Fit::Clear));
            }
            clear_of_own.get_or_insert(candidate);
        } else {
            first_free.get_or_insert(candidate);
        }
    }
    clear_of_own
        .map(|candidate| (candidate, Fit::OverlapsOtherMembers))
        .or(first_free.map(|candidate| (candidate, Fit::OverlapsOwn)))
}

fn warn_on_fit(node_id: i32, chosen: Ipv4Net, fit: Fit) {
    match fit {
        Fit::Clear => {}
        Fit::OverlapsOtherMembers => warn!(
            node_id,
            %chosen,
            "no subnet of the compute pool is clear of every member's occupied ranges; this one \
             overlaps another member's, which cannot reach this node over the overlay until the \
             pool changes or that network moves"
        ),
        Fit::OverlapsOwn => warn!(
            node_id,
            %chosen,
            "no subnet of the compute pool is clear of this node's own occupied ranges; its \
             overlay will report unavailable until the pool changes or that network moves"
        ),
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (unit-tested below; no DB / IO).
// ---------------------------------------------------------------------------

fn parse_cidr(s: &str) -> Result<Ipv4Net, ipnet::AddrParseError> {
    Ipv4Net::from_str(s)
}

/// First usable host in a /N: network address + 1.
pub(crate) fn bridge_address_for(cidr: &Ipv4Net) -> IpAddr {
    let net = cidr.network();
    let octets = net.octets();
    let bumped = u32::from_be_bytes(octets).saturating_add(1).to_be_bytes();
    IpAddr::V4(std::net::Ipv4Addr::from(bumped))
}

/// Return the lowest-numbered /prefix_len subnet of `pool` that does not
/// overlap any subnet in `used`. `None` when the pool is exhausted.
#[cfg(test)]
fn pick_free_subnet(pool: Ipv4Net, prefix_len: u8, used: &[Ipv4Net]) -> Option<Ipv4Net> {
    pool.subnets(prefix_len).ok()?.find(|candidate| {
        !used
            .iter()
            .any(|u| crate::config::cidrs_overlap(u, candidate))
    })
}

/// Every /prefix_len subnet of `pool`, highest first.
fn highest_first(pool: Ipv4Net, prefix_len: u8) -> impl Iterator<Item = Ipv4Net> {
    let bits = prefix_len.saturating_sub(pool.prefix_len());
    let subnet_count = 1_u64.checked_shl(u32::from(bits)).unwrap_or(0);
    let subnet_size = 1_u64
        .checked_shl(u32::from(32_u8.saturating_sub(prefix_len)))
        .unwrap_or(0);
    let pool_start = u64::from(u32::from(pool.network()));
    (0..subnet_count).rev().filter_map(move |index| {
        let address = pool_start.checked_add(index.checked_mul(subnet_size)?)?;
        let address = std::net::Ipv4Addr::from(u32::try_from(address).ok()?);
        Ipv4Net::new(address, prefix_len).ok()
    })
}

#[cfg(test)]
fn pick_highest_free_subnet(pool: Ipv4Net, prefix_len: u8, used: &[Ipv4Net]) -> Option<Ipv4Net> {
    pick_tiered(highest_first(pool, prefix_len), used, &AvoidSets::default()).map(|(c, _)| c)
}

// ---------------------------------------------------------------------------
// Tests — pure helpers only. Postgres-touching tests live in
// crates/temps-network/tests/it_allocator.rs (Docker-gated).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_must_be_private_and_larger_than_node_subnets() {
        let valid: Ipv4Net = "10.240.0.0/16".parse().unwrap();
        assert!(validate_pool(valid, 24).is_ok());

        let public: Ipv4Net = "100.64.0.0/16".parse().unwrap();
        assert!(matches!(
            validate_pool(public, 24),
            Err(AllocatorError::InvalidConfig { .. })
        ));
        assert!(matches!(
            validate_pool(valid, 16),
            Err(AllocatorError::InvalidConfig { .. })
        ));
        assert!(matches!(
            validate_pool(valid, 31),
            Err(AllocatorError::InvalidConfig { .. })
        ));
        assert!(matches!(
            validate_pool(valid, 32),
            Err(AllocatorError::InvalidConfig { .. })
        ));
    }

    #[test]
    fn underlay_must_not_be_inside_compute_pool() {
        let pool: Ipv4Net = "10.240.0.0/16".parse().unwrap();
        assert!(validate_underlay_outside_pool("10.200.4.2".parse().unwrap(), pool).is_ok());
        assert!(matches!(
            validate_underlay_outside_pool("10.240.4.2".parse().unwrap(), pool),
            Err(AllocatorError::UnderlayOverlapsComputePool { .. })
        ));
    }

    #[test]
    fn bridge_address_is_first_host() {
        let c = Ipv4Net::from_str("172.20.5.0/24").unwrap();
        assert_eq!(bridge_address_for(&c).to_string(), "172.20.5.1");
    }

    #[test]
    fn picks_lowest_free_subnet() {
        let pool = Ipv4Net::from_str("172.20.0.0/16").unwrap();
        let used = vec![
            Ipv4Net::from_str("172.20.0.0/24").unwrap(),
            Ipv4Net::from_str("172.20.1.0/24").unwrap(),
            Ipv4Net::from_str("172.20.3.0/24").unwrap(),
        ];
        let chosen = pick_free_subnet(pool, 24, &used).unwrap();
        assert_eq!(chosen.to_string(), "172.20.2.0/24");
    }

    #[test]
    fn skips_overlapping_supernet() {
        // If 172.20.0.0/20 is in use, /24 candidates inside it must be skipped.
        let pool = Ipv4Net::from_str("172.20.0.0/16").unwrap();
        let used = vec![Ipv4Net::from_str("172.20.0.0/20").unwrap()];
        let chosen = pick_free_subnet(pool, 24, &used).unwrap();
        assert_eq!(chosen.to_string(), "172.20.16.0/24");
    }

    #[test]
    fn returns_none_when_exhausted() {
        let pool = Ipv4Net::from_str("172.20.0.0/30").unwrap();
        let used = vec![Ipv4Net::from_str("172.20.0.0/30").unwrap()];
        assert!(pick_free_subnet(pool, 30, &used).is_none());
    }

    #[test]
    fn empty_used_picks_first() {
        let pool = Ipv4Net::from_str("10.50.0.0/16").unwrap();
        let chosen = pick_free_subnet(pool, 24, &[]).unwrap();
        assert_eq!(chosen.to_string(), "10.50.0.0/24");
    }

    #[test]
    fn control_plane_picks_highest_free_subnet() {
        let pool = Ipv4Net::from_str("172.20.0.0/16").unwrap();
        let used = vec![Ipv4Net::from_str("172.20.255.0/24").unwrap()];
        let chosen = pick_highest_free_subnet(pool, 24, &used).unwrap();
        assert_eq!(chosen.to_string(), "172.20.254.0/24");
    }

    fn nets(raw: &[&str]) -> Vec<Ipv4Net> {
        raw.iter().map(|r| r.parse().unwrap()).collect()
    }

    #[test]
    fn allocation_avoids_members_occupied_ranges() {
        let pool: Ipv4Net = "10.202.0.0/16".parse().unwrap();
        let avoid = AvoidSets {
            own: nets(&["10.202.0.0/24"]),
            others: nets(&["10.202.1.0/24", "10.202.2.7/32"]),
        };
        let used = nets(&["10.202.3.0/24"]);
        assert_eq!(
            pick_tiered(pool.subnets(24).unwrap(), &used, &avoid),
            Some(("10.202.4.0/24".parse().unwrap(), Fit::Clear))
        );
    }

    #[test]
    fn allocation_prefers_overlapping_others_to_overlapping_its_own() {
        let pool: Ipv4Net = "10.202.0.0/30".parse().unwrap();
        let avoid = AvoidSets {
            own: nets(&["10.202.0.0/31"]),
            others: nets(&["10.202.0.2/31"]),
        };
        assert_eq!(
            pick_tiered(pool.subnets(31).unwrap(), &[], &avoid),
            Some(("10.202.0.2/31".parse().unwrap(), Fit::OverlapsOtherMembers))
        );
        // Only its own ranges remain: still allocated, flagged.
        let used = nets(&["10.202.0.2/31"]);
        assert_eq!(
            pick_tiered(pool.subnets(31).unwrap(), &used, &avoid),
            Some(("10.202.0.0/31".parse().unwrap(), Fit::OverlapsOwn))
        );
    }

    #[test]
    fn a_covering_occupied_range_is_avoided_like_any_other() {
        let pool: Ipv4Net = "10.202.0.0/23".parse().unwrap();
        let avoid = AvoidSets {
            own: nets(&["10.0.0.0/8"]),
            others: Vec::new(),
        };
        assert_eq!(
            pick_tiered(pool.subnets(24).unwrap(), &[], &avoid).map(|(_, fit)| fit),
            Some(Fit::OverlapsOwn)
        );
    }

    #[test]
    fn highest_first_walks_the_pool_downwards() {
        let pool: Ipv4Net = "10.202.0.0/22".parse().unwrap();
        let all: Vec<String> = highest_first(pool, 24).map(|c| c.to_string()).collect();
        assert_eq!(
            all,
            [
                "10.202.3.0/24",
                "10.202.2.0/24",
                "10.202.1.0/24",
                "10.202.0.0/24"
            ]
        );
    }

    #[test]
    fn underlay_accepts_private_addresses_and_rejects_public_addresses() {
        assert!(is_private_underlay("10.200.4.2".parse().unwrap()));
        assert!(is_private_underlay("fd00::2".parse().unwrap()));
        assert!(!is_private_underlay("88.198.50.37".parse().unwrap()));
        assert!(!is_private_underlay(
            "2001:4860:4860::8888".parse().unwrap()
        ));
    }
}
