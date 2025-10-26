use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr, FromJsonQueryResult};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;
use utoipa::ToSchema;

use super::deployment_config::DeploymentConfigSnapshot;

/// Git push event information that triggered the deployment
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GitPushEvent {
    /// Repository name
    pub repo: String,
    /// Repository owner/organization
    pub owner: String,
    /// Branch that was pushed
    pub branch: String,
    /// Commit SHA
    pub commit: String,
}

/// Deployment metadata - typed information about the deployment
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, FromJsonQueryResult, ToSchema)]
#[serde(rename_all = "camelCase")]
#[derive(Default)]
pub struct DeploymentMetadata {
    /// Git push event that triggered this deployment (if from webhook)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_push_event: Option<GitPushEvent>,

    /// Build duration in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_duration_ms: Option<i64>,

    /// Deployment duration in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment_duration_ms: Option<i64>,

    /// Total size of the built image in bytes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_size_bytes: Option<i64>,

    /// Number of files in the build output
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_count: Option<i32>,

    /// Docker builder used (e.g., "nixpacks", "dockerfile")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub builder: Option<String>,

    /// Dockerfile path if using Dockerfile builder
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dockerfile_path: Option<String>,

    /// Whether this is a rollback deployment
    #[serde(default)]
    pub is_rollback: bool,

    /// ID of the deployment this was rolled back from (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rolled_back_from_id: Option<i32>,

    /// Custom labels/tags for the deployment
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "deployments")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    pub environment_id: i32,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    pub slug: String,
    pub state: String,
    /// Typed deployment metadata (build info, rollback info, etc.)
    pub metadata: Option<DeploymentMetadata>,
    pub deploying_at: Option<DBDateTime>,
    pub ready_at: Option<DBDateTime>,
    // Workflow fields (replacing pipeline_id)
    pub started_at: Option<DBDateTime>,
    pub finished_at: Option<DBDateTime>,
    pub context_vars: Option<Json>, // Global context variables for the workflow
    pub branch_ref: Option<String>,
    pub tag_ref: Option<String>,
    pub commit_sha: Option<String>,
    pub commit_message: Option<String>,
    pub commit_author: Option<String>,
    pub commit_json: Option<Json>,
    pub cancelled_reason: Option<String>,
    // Static deployment fields
    pub static_dir_location: Option<String>,
    pub screenshot_location: Option<String>,
    pub image_name: Option<String>,
    /// Deployment configuration snapshot at the time of deployment
    /// This captures what resources, replicas, environment variables, and settings were used for this specific deployment
    /// Allows tracking changes in configuration over time
    pub deployment_config: Option<DeploymentConfigSnapshot>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id"
    )]
    Project,
    #[sea_orm(
        belongs_to = "super::environments::Entity",
        from = "Column::EnvironmentId",
        to = "super::environments::Column::Id"
    )]
    Environment,
    #[sea_orm(has_many = "super::deployment_jobs::Entity")]
    Jobs,
    #[sea_orm(has_many = "super::deployment_containers::Entity")]
    Containers,
    #[sea_orm(has_many = "super::deployment_domains::Entity")]
    Domains,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl Related<super::environments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Environment.def()
    }
}

impl Related<super::deployment_jobs::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Jobs.def()
    }
}

impl Related<super::deployment_containers::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Containers.def()
    }
}

impl Related<super::deployment_domains::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Domains.def()
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
