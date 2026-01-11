//! Deployment Jobs entity
//!
//! Represents individual jobs within a deployment workflow (like GitHub Actions jobs)

use super::types::JobStatus;
use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "deployment_jobs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub deployment_id: i32, // The workflow run (deployment)
    pub job_id: String,     // User-defined job identifier (e.g., "download_repo", "build_image")
    pub job_type: String, // Job type (e.g., "DownloadRepoJob", "BuildImageJob", "DeployContainerJob")
    pub name: String,     // Human-readable job name
    pub description: Option<String>,
    pub status: JobStatus,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    pub started_at: Option<DBDateTime>,
    pub finished_at: Option<DBDateTime>,
    pub log_id: String, // Log identifier for temps-logs service
    pub error_message: Option<String>,
    pub job_config: Option<Json>,     // Job-specific configuration
    pub outputs: Option<Json>,        // Job outputs as key-value pairs
    pub dependencies: Option<Json>,   // List of job IDs this job depends on
    pub execution_order: Option<i32>, // Calculated execution order
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
            if self.updated_at.is_not_set() {
                self.updated_at = Set(now);
            }
        } else {
            self.updated_at = Set(now);
        }

        Ok(self)
    }
}
