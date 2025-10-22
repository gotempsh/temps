use super::types::ProjectType;
use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "projects")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub repo_name: Option<String>,
    pub repo_owner: Option<String>,
    pub directory: String,
    pub main_branch: String,
    pub preset: Option<String>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    pub slug: String,
    pub is_deleted: bool,
    pub deleted_at: Option<DBDateTime>,
    pub cpu_request: Option<i32>,
    pub cpu_limit: Option<i32>,
    pub memory_request: Option<i32>,
    pub memory_limit: Option<i32>,
    pub build_command: Option<String>,
    pub install_command: Option<String>,
    pub output_dir: Option<String>,
    pub automatic_deploy: bool,
    pub project_type: ProjectType,
    pub is_web_app: bool,
    pub performance_metrics_enabled: bool,
    pub last_deployment: Option<DBDateTime>,
    pub use_default_wildcard: bool,
    pub custom_domain: Option<String>,
    pub is_public_repo: bool,
    pub git_url: Option<String>,
    pub git_provider_connection_id: Option<i32>,
    pub is_on_demand: bool,
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
