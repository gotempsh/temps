use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// A captured, persisted log dump for a deployment container that has since been
/// torn down. Written just before a superseded deployment's containers are
/// stopped and removed, so the logs of "the container that ran a few days ago"
/// survive and can be viewed later.
///
/// The dump itself is a plain-text file on disk under the data dir (managed by
/// `temps_logs::LogService`); this row carries only the metadata and the
/// server-generated relative `log_path` to it.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "deployment_container_logs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub deployment_id: i32,
    /// Denormalised for fast project-scoped authorization on read.
    pub project_id: i32,
    pub environment_id: i32,
    /// Docker container id at capture time (the container no longer exists).
    pub container_id: String,
    /// Human-visible container name, e.g. "web-2".
    pub container_name: String,
    /// Compose service name (e.g. "web"). NULL for single-container deployments.
    #[sea_orm(column_type = "String(StringLen::N(255))", nullable)]
    pub service_name: Option<String>,
    /// Node this container ran on. NULL = local node (single-node mode).
    pub node_id: Option<i32>,
    /// Server-generated relative path of the captured log file under the data
    /// dir. Never user-supplied — safe from path traversal.
    pub log_path: String,
    /// Size of the captured dump in bytes.
    pub size_bytes: i64,
    /// True when the dump was truncated to the tail because the live log
    /// exceeded the capture cap.
    pub truncated: bool,
    /// When the logs were captured (just before teardown).
    pub captured_at: DBDateTime,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::deployments::Entity",
        from = "Column::DeploymentId",
        to = "super::deployments::Column::Id"
    )]
    Deployment,
}

impl Related<super::deployments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Deployment.def()
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
            if self.captured_at.is_not_set() {
                self.captured_at = Set(now);
            }
        }

        Ok(self)
    }
}
