//! Per-node applied-state for the internal DNS resolver (ADR-011).
//!
//! Each row tracks the highest `service_endpoints.generation` a node's
//! resolver has applied, when it last successfully synced, and the
//! resolver's self-reported health. Drift detection is a single query:
//!
//! ```sql
//! SELECT node_id FROM node_dns_state
//! WHERE last_sync_at < now() - interval '60 seconds';
//! ```
//!
//! `node_id` is the primary key, not a separate `id` — there is exactly
//! one resolver state per node. FK is `ON DELETE CASCADE` so removing a
//! node removes its resolver state (the records the node owned are
//! handled separately by the `service_endpoints` GC reconciler).

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "node_dns_state")]
pub struct Model {
    /// FK to `nodes.id`. Acts as the primary key — exactly one row per node.
    #[sea_orm(primary_key, auto_increment = false)]
    pub node_id: i32,
    /// Highest `service_endpoints.generation` this node has applied to
    /// its resolver. Defaults to 0 (no records applied yet).
    pub applied_generation: i64,
    /// Wall-clock time of the last successful sync ACK. NULL until first sync.
    pub last_sync_at: Option<DBDateTime>,
    /// One of: `'healthy'`, `'degraded'`, `'stale'`, `'unknown'`. CHECK
    /// constraint enforces. `'unknown'` is the post-migration default.
    pub health: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::nodes::Entity",
        from = "Column::NodeId",
        to = "super::nodes::Column::Id",
        on_delete = "Cascade"
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
    async fn before_save<C>(self, _db: &C, _insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        // No created_at / updated_at on this table — `last_sync_at` is the
        // only timestamp and it's set explicitly by the sync ACK handler.
        Ok(self)
    }
}
