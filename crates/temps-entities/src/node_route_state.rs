//! Per-node applied-state for the worker internal route store.
//!
//! Mirror of `node_dns_state` for the route-sync path: each row tracks
//! the highest in-memory route-table generation a worker has applied
//! and ACKed back to the CP. `mark_deployment_complete` blocks on
//! `MIN(applied_generation)` across healthy workers reaching the CP's
//! current generation, ensuring "completed" implies every worker can
//! correctly serve the new deployment.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "node_route_state")]
pub struct Model {
    /// FK to `nodes.id`. Acts as primary key — exactly one row per node.
    #[sea_orm(primary_key, auto_increment = false)]
    pub node_id: i32,
    /// Highest CP route-table generation this node has applied.
    /// Defaults to 0 (no applies yet).
    pub applied_generation: i64,
    /// Wall-clock time of the last successful ACK. NULL until first sync.
    pub last_sync_at: Option<DBDateTime>,
    /// One of: `'healthy'`, `'degraded'`, `'stale'`, `'unknown'`.
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
        Ok(self)
    }
}
