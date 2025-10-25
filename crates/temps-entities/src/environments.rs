use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

use super::deployment_config::DeploymentConfig;
use super::upstream_config::UpstreamList;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "environments")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub slug: String,
    pub subdomain: String,
    pub last_deployment: Option<DBDateTime>,
    pub host: String,
    pub upstreams: UpstreamList,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    pub project_id: i32,
    pub current_deployment_id: Option<i32>,
    pub branch: Option<String>,
    pub deleted_at: Option<DBDateTime>,
    /// Deployment configuration (CPU, memory, port, analytics, auto-deploy settings)
    /// These override project-level defaults for this specific environment
    pub deployment_config: Option<DeploymentConfig>,
}

impl Model {
    /// Get the effective deployment configuration by merging project and environment configs
    ///
    /// The project configuration serves as defaults, and the environment configuration
    /// overrides specific values. This allows setting project-wide defaults with
    /// environment-specific overrides.
    pub fn get_effective_deployment_config(
        &self,
        project_config: &DeploymentConfig,
    ) -> DeploymentConfig {
        let env_config = self.deployment_config.clone().unwrap_or_default();
        project_config.merge(&env_config)
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id"
    )]
    Project,
    #[sea_orm(has_many = "super::env_vars::Entity")]
    EnvVars,
    #[sea_orm(
        belongs_to = "super::deployments::Entity",
        from = "Column::CurrentDeploymentId",
        to = "super::deployments::Column::Id",
        on_update = "NoAction",
        on_delete = "SetNull"
    )]
    CurrentDeployment,
    #[sea_orm(has_many = "super::environment_domains::Entity")]
    EnvironmentDomains,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl Related<super::env_vars::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::EnvVars.def()
    }
}

impl Related<super::deployments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::CurrentDeployment.def()
    }
}

impl Related<super::environment_domains::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::EnvironmentDomains.def()
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
