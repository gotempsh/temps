//! Authoritative DNS records for the internal `*.temps.local` zone.
//!
//! See ADR-011 for the full design. Each row is one DNS record served by
//! every per-node Hickory resolver. Mutations bump `generation`; agents
//! long-poll for changes since their last applied generation.
//!
//! `target_ip` is stored as a string (matching the `nodes.private_address`
//! / `nodes.compute_cidr` / `nodes.underlay_address` convention) and parsed
//! to `std::net::IpAddr` at the application boundary. The same column carries
//! both v4 (A records) and v6 (AAAA records).

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "service_endpoints")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Fully-qualified domain name, e.g. `pg-orders-0.pg-orders.temps.local`.
    pub fqdn: String,
    /// One of: `'A'`, `'AAAA'`, `'SRV'`, `'CNAME'`. Enforced by CHECK
    /// constraint at the schema level (see migration
    /// `m20260427_000002_add_dns_service_endpoints`).
    pub record_type: String,
    /// IP literal for A/AAAA, target hostname for CNAME. NULL only for
    /// records that don't carry an address (rare; reserved for future use).
    pub target_ip: Option<String>,
    /// Port for SRV records. Optional for plain A/AAAA — set when the
    /// record represents a service endpoint and consumers want to read
    /// the port off the same row instead of constructing it elsewhere.
    pub target_port: Option<i32>,
    /// Time-to-live in seconds. Defaults to 30 at the schema level.
    /// Reconcilers override (5 for primaries, 30 for replicas, 300 for static).
    pub ttl: i32,
    /// One of: `'service_member'`, `'service_role'`, `'node'`, `'static'`.
    /// Enforced by CHECK constraint. Determines how `owner_id` is interpreted
    /// for GC: `service_member` → `service_members.id`, `service_role` →
    /// `external_services.id`, `node` → `nodes.id`, `static` → opaque.
    pub owner_kind: String,
    pub owner_id: i64,
    /// Node this record points to, when known. NULL for records like
    /// `<svc>.temps.local` (multi-A) where membership lives in the IP set.
    /// FK is `ON DELETE SET NULL` so removing a node leaves stale records
    /// addressable until the GC reconciles them.
    pub node_id: Option<i32>,
    /// Monotonic counter. Bumped on every mutation. Agents request
    /// `WHERE generation > $applied_generation` and ACK back what they applied.
    pub generation: i64,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::nodes::Entity",
        from = "Column::NodeId",
        to = "super::nodes::Column::Id",
        on_delete = "SetNull"
    )]
    Node,
}

impl Related<super::nodes::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Node.def()
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
