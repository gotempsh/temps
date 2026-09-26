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
use async_trait::async_trait;
use ipnet::Ipv4Net;
use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ColumnTrait, DatabaseConnection, DbErr, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use temps_entities::{network_config as nc, nodes};
use thiserror::Error;
use tracing::info;
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

    /// Cluster-wide pool changes are unsafe after a node has received an
    /// allocation. Every member must agree on one pool for routes and DNS.
    #[error(
        "cannot change compute pool from {current_pool} (/{current_prefix_len} per node) to \
         {requested_pool} (/{requested_prefix_len} per node): {allocation_count} allocation(s) \
         already exist; remove/re-enrol those nodes before changing the cluster pool"
    )]
    PoolChangeAfterAllocation {
        current_pool: Ipv4Net,
        current_prefix_len: u8,
        requested_pool: Ipv4Net,
        requested_prefix_len: u8,
        allocation_count: u64,
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

    /// Configure the global pool before the first node is allocated.
    ///
    /// Once any allocation or reservation exists, changing the pool would
    /// leave an in-flight setup mutating kernel state for a stale topology.
    /// Failed setup is retried with the same authoritative pool; changing it
    /// requires explicitly removing the cluster allocation first.
    pub async fn configure_pool(
        &self,
        requested_pool: Ipv4Net,
        requested_prefix_len: u8,
    ) -> Result<ClusterNetworkConfig, AllocatorError> {
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
            return Ok(current);
        }

        let worker_allocations = nodes::Entity::find()
            .filter(nodes::Column::ComputeCidr.is_not_null())
            .count(&txn)
            .await?;
        let control_plane_allocations = u64::from(cfg.control_plane_compute_cidr.is_some());
        let allocation_count = worker_allocations + control_plane_allocations;
        if allocation_count > 0 {
            return Err(AllocatorError::PoolChangeAfterAllocation {
                current_pool: current.compute_pool_cidr,
                current_prefix_len: current.subnet_prefix_len,
                requested_pool,
                requested_prefix_len,
                allocation_count,
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
        // No allocation exists here, so these fields should already be empty.
        // Clear them defensively to keep the singleton internally consistent.
        active.control_plane_compute_cidr = Set(None);
        active.control_plane_underlay_address = Set(None);
        active.control_plane_overlay_ready = Set(false);
        // Fence any setup attempt that captured the previous pool.
        active.control_plane_setup_generation = Set(next_generation);
        active.update(&txn).await?;
        txn.commit().await?;
        Ok(requested)
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
            let used_rows: Vec<Option<String>> = nodes::Entity::find()
                .filter(nodes::Column::ComputeCidr.is_not_null())
                .select_only()
                .column(nodes::Column::ComputeCidr)
                .into_tuple()
                .all(&txn)
                .await?;
            let mut used = Vec::with_capacity(used_rows.len());
            for raw in used_rows.into_iter().flatten() {
                used.push(parse_cidr(&raw).map_err(|error| {
                    AllocatorError::ComputeCidrInvalid {
                        node_id: 0,
                        raw,
                        reason: error.to_string(),
                    }
                })?);
            }
            pick_highest_free_subnet(pool, prefix_len, &used).ok_or(
                AllocatorError::PoolExhausted {
                    pool,
                    prefix_len,
                    used_count: used.len(),
                },
            )?
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

        // 3. Load all currently-used CIDRs so we can pick a free one.
        let used_rows: Vec<Option<String>> = nodes::Entity::find()
            .filter(nodes::Column::ComputeCidr.is_not_null())
            .select_only()
            .column(nodes::Column::ComputeCidr)
            .into_tuple()
            .all(&txn)
            .await?;

        let mut used: Vec<Ipv4Net> = Vec::with_capacity(used_rows.len() + 1);
        for raw in used_rows.into_iter().flatten() {
            match parse_cidr(&raw) {
                Ok(c) => used.push(c),
                Err(e) => {
                    // A malformed row would silently shadow a valid free
                    // subnet; surface it loudly instead of carrying on.
                    return Err(AllocatorError::ComputeCidrInvalid {
                        node_id: 0,
                        raw,
                        reason: e.to_string(),
                    });
                }
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

        // 4. Find the lowest-numbered free subnet of `prefix_len` inside `pool`.
        let chosen =
            pick_free_subnet(pool, prefix_len, &used).ok_or(AllocatorError::PoolExhausted {
                pool,
                prefix_len,
                used_count: used.len(),
            })?;
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
pub(crate) fn pick_free_subnet(pool: Ipv4Net, prefix_len: u8, used: &[Ipv4Net]) -> Option<Ipv4Net> {
    pool.subnets(prefix_len).ok()?.find(|candidate| {
        !used
            .iter()
            .any(|u| crate::config::cidrs_overlap(u, candidate))
    })
}

fn pick_highest_free_subnet(pool: Ipv4Net, prefix_len: u8, used: &[Ipv4Net]) -> Option<Ipv4Net> {
    let subnet_count = 1_u64.checked_shl(u32::from(prefix_len.checked_sub(pool.prefix_len())?))?;
    let subnet_size = 1_u64.checked_shl(u32::from(32_u8.checked_sub(prefix_len)?))?;
    let pool_start = u64::from(u32::from(pool.network()));
    for index in (0..subnet_count).rev() {
        let address = pool_start.checked_add(index.checked_mul(subnet_size)?)?;
        let address = std::net::Ipv4Addr::from(u32::try_from(address).ok()?);
        let candidate = Ipv4Net::new(address, prefix_len).ok()?;
        if !used
            .iter()
            .any(|existing| crate::config::cidrs_overlap(existing, &candidate))
        {
            return Some(candidate);
        }
    }
    None
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
