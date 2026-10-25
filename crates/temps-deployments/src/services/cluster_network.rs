// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The cluster compute pool, as the console sees and changes it (ADR 056).
//!
//! `GET /settings/cluster-network` shows the pool, whether it can change and
//! what uses it, each node's cross-node overlay state with the cause when it
//! is unavailable, and the control plane's. `PUT` changes the pool through
//! [`PostgresAllocator::configure_pool`], the same path as
//! `temps network setup-multi-node --compute-pool-cidr`.

use std::sync::Arc;

use ipnet::Ipv4Net;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder};
use serde::Serialize;
use temps_entities::{network_config, nodes};
use temps_network::allocator::{AllocatorError, PoolChange, PostgresAllocator};
use thiserror::Error;
use utoipa::ToSchema;

use crate::handlers::nodes::OccupiedRangeReport;

#[derive(Debug, Error)]
pub enum ClusterNetworkError {
    #[error("compute pool {raw:?} is not an IPv4 network such as 10.202.0.0/16: {reason}")]
    InvalidCidr { raw: String, reason: String },

    #[error(transparent)]
    Allocator(#[from] AllocatorError),

    #[error("could not read the cluster network: {0}")]
    Database(#[from] sea_orm::DbErr),
}

/// One worker's view of the overlay.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ClusterNetworkNode {
    pub id: i32,
    pub name: String,
    /// Its subnet of the pool, if allocated.
    pub compute_cidr: Option<String>,
    /// `ready`, `unavailable`, or `None` when its agent never reported it.
    pub overlay_status: Option<String>,
    /// Why the overlay is unavailable: the colliding range, its owner and
    /// the fixes.
    pub overlay_last_error: Option<String>,
    /// The last automatic move of its allocation, and why.
    pub overlay_remediation: Option<String>,
    /// Whether its allocation locks the pool, and why when it does.
    pub in_use: bool,
    pub in_use_reason: Option<String>,
    /// What the node reported it already uses outside Temps.
    pub occupied_ranges: Vec<OccupiedRangeReport>,
}

/// The control plane's overlay.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ClusterNetworkControlPlane {
    pub compute_cidr: Option<String>,
    pub overlay_ready: bool,
    /// Why its last overlay setup failed, with the fix, when it did.
    pub setup_failure: Option<String>,
}

/// `GET /settings/cluster-network`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ClusterNetworkStatus {
    pub compute_pool_cidr: String,
    pub subnet_prefix_len: u8,
    /// What a new cluster starts with.
    pub default_compute_pool_cidr: String,
    /// The WireGuard mesh pool; a compute pool may not overlap it.
    pub mesh_pool_cidr: String,
    pub mesh_enabled: bool,
    /// Whether an operator has confirmed the pool, or a member already uses
    /// it. The console asks for it once before adding the first node.
    pub pool_confirmed: bool,
    /// Whether `PUT` can change the pool now.
    pub can_change: bool,
    /// Why it cannot: the members using the pool, or a control-plane setup
    /// in progress.
    pub change_blocked_by: Vec<String>,
    pub control_plane: ClusterNetworkControlPlane,
    pub nodes: Vec<ClusterNetworkNode>,
}

/// A node whose allocation a change moved.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ClusterNetworkReassignment {
    pub node_id: i32,
    pub node_name: String,
    pub from: String,
    pub to: String,
}

/// `PUT /settings/cluster-network` answer.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ClusterNetworkChangeResponse {
    pub reassigned: Vec<ClusterNetworkReassignment>,
    pub status: ClusterNetworkStatus,
}

pub struct ClusterNetworkService {
    db: Arc<DatabaseConnection>,
}

impl ClusterNetworkService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    pub async fn status(&self) -> Result<ClusterNetworkStatus, ClusterNetworkError> {
        let db = self.db.as_ref();
        let cfg = network_config::Entity::find_by_id(1).one(db).await?.ok_or(
            AllocatorError::InvalidConfig {
                reason: "network_config singleton row missing".into(),
            },
        )?;
        let members = nodes::Entity::find()
            .order_by_asc(nodes::Column::Id)
            .all(db)
            .await?;
        let with_services =
            temps_network::allocator::nodes_with_overlay_members(db, &members).await?;
        Ok(status_of(
            &cfg,
            &members,
            &with_services,
            temps_network::control_plane::last_setup_failure(),
        ))
    }

    /// Change the pool; see [`PostgresAllocator::configure_pool`]. Also
    /// returns whether this save was the pool's first confirmation.
    pub async fn change_pool(
        &self,
        compute_pool_cidr: &str,
        subnet_prefix_len: u8,
    ) -> Result<(PoolChange, bool), ClusterNetworkError> {
        let raw = compute_pool_cidr.trim();
        let pool: Ipv4Net = raw.parse().map_err(|error: ipnet::AddrParseError| {
            ClusterNetworkError::InvalidCidr {
                raw: raw.to_owned(),
                reason: error.to_string(),
            }
        })?;
        if pool != pool.trunc() {
            return Err(ClusterNetworkError::InvalidCidr {
                raw: raw.to_owned(),
                reason: format!("host bits are set; did you mean {}?", pool.trunc()),
            });
        }
        let change = PostgresAllocator::new(self.db.clone())
            .configure_pool(pool, subnet_prefix_len)
            .await?;
        let first_confirmation = self.confirm_pool().await?;
        Ok((change, first_confirmation))
    }

    /// Record that an operator chose the pool. Saving it, changed or not, is
    /// the confirmation; the first one is kept. True when this was it.
    async fn confirm_pool(&self) -> Result<bool, ClusterNetworkError> {
        let result = network_config::Entity::update_many()
            .col_expr(
                network_config::Column::ComputePoolConfirmedAt,
                sea_orm::sea_query::Expr::current_timestamp().into(),
            )
            .filter(network_config::Column::Id.eq(1))
            .filter(network_config::Column::ComputePoolConfirmedAt.is_null())
            .exec(self.db.as_ref())
            .await?;
        Ok(result.rows_affected > 0)
    }
}

pub(crate) fn status_of(
    cfg: &network_config::Model,
    members: &[nodes::Model],
    with_services: &std::collections::HashSet<i32>,
    setup_failure: Option<String>,
) -> ClusterNetworkStatus {
    let nodes: Vec<ClusterNetworkNode> = members
        .iter()
        .map(|node| {
            let in_use_reason = temps_network::allocator::allocation_in_use(node, with_services);
            ClusterNetworkNode {
                id: node.id,
                name: node.name.clone(),
                compute_cidr: node.compute_cidr.clone(),
                overlay_status: node.overlay_status.clone(),
                overlay_last_error: node.overlay_last_error.clone(),
                overlay_remediation: node.overlay_remediation.clone(),
                in_use: in_use_reason.is_some(),
                in_use_reason,
                occupied_ranges: temps_network::allocator::occupied_of(node)
                    .into_iter()
                    .map(|range| OccupiedRangeReport {
                        cidr: range.cidr.to_string(),
                        source: match range.source {
                            temps_network::occupied::OccupiedSource::Route => "route",
                            temps_network::occupied::OccupiedSource::Docker => "docker",
                        }
                        .to_owned(),
                        name: range.name,
                    })
                    .collect(),
            }
        })
        .collect();
    let mut change_blocked_by: Vec<String> = Vec::new();
    if cfg.control_plane_overlay_ready {
        change_blocked_by.push(format!(
            "the control plane ({}, overlay up)",
            cfg.control_plane_compute_cidr
                .as_deref()
                .unwrap_or("unknown subnet")
        ));
    } else if let Some(cidr) = cfg.control_plane_compute_cidr.as_deref() {
        change_blocked_by.push(format!(
            "the control plane ({cidr}, overlay setup in progress or failed after changing the \
             host)"
        ));
    }
    change_blocked_by.extend(nodes.iter().filter_map(|node| node.in_use_reason.clone()));
    // Only meaningful while the control plane has not brought it up.
    let setup_failure = (!cfg.control_plane_overlay_ready)
        .then_some(setup_failure)
        .flatten();
    ClusterNetworkStatus {
        compute_pool_cidr: cfg.compute_pool_cidr.clone(),
        subnet_prefix_len: u8::try_from(cfg.subnet_prefix_len).unwrap_or(0),
        default_compute_pool_cidr: network_config::DEFAULT_COMPUTE_POOL_CIDR.to_owned(),
        mesh_pool_cidr: cfg.wireguard_cidr.clone(),
        mesh_enabled: cfg.wireguard_enabled,
        pool_confirmed: cfg.compute_pool_confirmed_at.is_some() || !change_blocked_by.is_empty(),
        can_change: change_blocked_by.is_empty(),
        change_blocked_by,
        control_plane: ClusterNetworkControlPlane {
            compute_cidr: cfg.control_plane_compute_cidr.clone(),
            overlay_ready: cfg.control_plane_overlay_ready,
            setup_failure,
        },
        nodes,
    }
}

/// The allocations a pool change moved, for the API answer and the audit.
pub fn reassignments(change: &PoolChange) -> Vec<ClusterNetworkReassignment> {
    change
        .reassigned
        .iter()
        .map(|moved| ClusterNetworkReassignment {
            node_id: moved.node_id,
            node_name: moved.node_name.clone(),
            from: moved.from.clone(),
            to: moved.to.to_string(),
        })
        .collect()
}
