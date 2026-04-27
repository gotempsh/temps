use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "service_members")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub service_id: i32,
    /// Node this member runs on. NULL = local node (control plane).
    pub node_id: Option<i32>,
    /// Service-type-specific role: 'primary', 'replica', 'monitor', 'arbiter', 'sentinel', 'node'
    pub role: String,
    pub container_id: Option<String>,
    pub container_name: String,
    /// FQDN for inter-member communication, in the form
    /// `<svc>-<ordinal>.<svc>.temps.local`. Resolves via the per-node DNS
    /// resolver (ADR-011). Populated by the lifecycle hook on container
    /// start; never an IP, always an FQDN.
    pub hostname: Option<String>,
    pub port: Option<i32>,
    /// Per-container overlay IP from `temps-overlay` (e.g. `172.20.5.42`).
    /// Set by the post-create lifecycle hook (`docker inspect` output).
    /// NULL on single-host clusters and on members from before the
    /// multi-host overlay was enabled. Read by the proxy's route table
    /// for cross-node routing and by the DNS registry as the A record
    /// target.
    pub compute_ip: Option<String>,
    pub status: String,
    /// Stable member identity (member-0, member-1, etc.)
    pub ordinal: i32,
    /// Encrypted member-specific config overrides
    pub config: Option<String>,
    /// Last-attempted phase of the async `add_cluster_member` background
    /// task (e.g. `validating`, `provisioning_container`, `done`). NULL
    /// for members that didn't go through that path. The frontend uses
    /// this to render a live timeline while the new replica spins up.
    pub provisioning_step: Option<String>,
    /// Most recent error seen by the background provisioning task. Set
    /// only when `status = 'failed'` so the UI can surface why.
    pub provisioning_error: Option<String>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::external_services::Entity",
        from = "Column::ServiceId",
        to = "super::external_services::Column::Id"
    )]
    Service,
    #[sea_orm(
        belongs_to = "super::nodes::Entity",
        from = "Column::NodeId",
        to = "super::nodes::Column::Id"
    )]
    Node,
}

impl Related<super::external_services::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Service.def()
    }
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
