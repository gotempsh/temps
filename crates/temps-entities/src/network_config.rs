// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cluster-wide network configuration. Single-row table (`id = 1` enforced
//! by CHECK constraint) owned by the control plane. Drives the
//! `temps-network` data plane on every worker via the per-node allocator.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "network_config")]
pub struct Model {
    /// Always 1; CHECK (id = 1) makes the row unique by construction.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i32,
    /// CIDR pool we slice into per-node CIDRs (e.g. "172.20.0.0/16").
    pub compute_pool_cidr: String,
    /// Prefix length per allocated subnet — `24` means each node gets a /24
    /// (256 hosts) carved out of `compute_pool_cidr`.
    pub subnet_prefix_len: i32,
    /// Transport mode: "vxlan" or "native". Validated by a CHECK constraint
    /// at the schema level so an invalid value never reaches Rust.
    pub transport: String,
    /// VXLAN Network Identifier (ignored when `transport = "native"`).
    pub vxlan_vni: i32,
    /// VXLAN UDP destination port (4789 by IANA assignment).
    pub vxlan_port: i32,
    /// Underlay MTU. Bridge MTU is derived: `underlay_mtu - 50` for VXLAN,
    /// `underlay_mtu` for native.
    pub underlay_mtu: i32,
    /// Stable overlay CIDR reserved for the control plane. Keeping this in the
    /// singleton avoids modelling the control plane as a schedulable worker.
    pub control_plane_compute_cidr: Option<String>,
    /// Address workers use as the VXLAN underlay endpoint for the control
    /// plane. Usually the private/VLAN address configured by the operator.
    pub control_plane_underlay_address: Option<String>,
    /// Set only after kernel and Docker overlay setup completes successfully.
    pub control_plane_overlay_ready: bool,
    /// Monotonic fencing token for control-plane setup attempts. A stale
    /// attempt may only publish or withdraw the exact generation it reserved.
    pub control_plane_setup_generation: i64,
    /// Run the overlay over a managed WireGuard mesh: every node gets a mesh
    /// address from `wireguard_cidr` as its underlay, so nodes that only share
    /// public IPs can form the overlay.
    pub wireguard_enabled: bool,
    /// Mesh address pool. The control plane takes the first host address.
    pub wireguard_cidr: String,
    /// UDP port every node's WireGuard interface listens on.
    pub wireguard_port: i32,
    /// Control plane's mesh public key; its private key stays on the host.
    pub control_plane_wg_public_key: Option<String>,
    /// `ip:port` workers dial to reach the control plane's WireGuard socket.
    pub control_plane_wg_endpoint: Option<String>,
    /// TCP port of the node API on the control plane's mesh address; `None`
    /// means the mesh port number.
    pub node_api_port: Option<i32>,
    /// The node that is the mesh hub (ADR 048 D4), relaying traffic between
    /// members that cannot reach each other.
    pub mesh_hub_node_id: Option<i32>,
    /// The control plane is the mesh hub. Never set together with
    /// `mesh_hub_node_id`.
    pub mesh_hub_control_plane: bool,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, _insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        // Always bump updated_at — the control plane treats this row as a
        // versionable config snapshot.
        self.updated_at = Set(chrono::Utc::now());
        Ok(self)
    }
}
