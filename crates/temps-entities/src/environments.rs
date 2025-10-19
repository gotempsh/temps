use sea_orm::entity::prelude::*;
use async_trait::async_trait;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;


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
    pub upstreams: Json,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    pub project_id: i32,
    pub current_deployment_id: Option<i32>,
    pub cpu_request: Option<i32>,
    pub cpu_limit: Option<i32>,
    pub memory_request: Option<i32>,
    pub memory_limit: Option<i32>,
    pub branch: Option<String>,
    pub replicas: Option<i32>,
    pub deleted_at: Option<DBDateTime>,
    pub use_default_wildcard: bool,
    pub custom_domain: Option<String>,
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