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
