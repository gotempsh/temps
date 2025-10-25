use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

use super::deployment_config::DeploymentConfig;
use super::preset::{Preset, PresetConfig};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "projects")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    /// Repository name (required)
    pub repo_name: String,
    /// Repository owner/namespace (required)
    pub repo_owner: String,
    pub directory: String,
    pub main_branch: String,
    /// Preset/framework type (required - every project must have a preset)
    pub preset: Preset,
    /// Preset-specific configuration (e.g., NextJsConfig with custom build commands)
    /// This is typed based on the preset enum variant
    pub preset_config: Option<PresetConfig>,
    /// Deployment configuration (CPU, memory, port, analytics, auto-deploy settings)
    /// These serve as defaults for all environments unless overridden
    pub deployment_config: Option<DeploymentConfig>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    pub slug: String,
    pub is_deleted: bool,
    pub deleted_at: Option<DBDateTime>,
    pub last_deployment: Option<DBDateTime>,
    pub is_public_repo: bool,
    pub git_url: Option<String>,
    pub git_provider_connection_id: Option<i32>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::env_vars::Entity")]
    EnvVars,
    #[sea_orm(has_many = "super::environments::Entity")]
    Environments,
    #[sea_orm(
        belongs_to = "super::git_provider_connections::Entity",
        from = "Column::GitProviderConnectionId",
        to = "super::git_provider_connections::Column::Id"
    )]
    GitProviderConnection,
}

impl Related<super::env_vars::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::EnvVars.def()
    }
}

impl Related<super::environments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Environments.def()
    }
}

impl Related<super::git_provider_connections::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::GitProviderConnection.def()
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
