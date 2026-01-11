use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "env_vars")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub key: String,
    pub value: String,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    /// Include this environment variable in preview environments
    pub include_in_preview: bool,
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
    #[sea_orm(
        has_many = "super::env_var_environments::Entity",
        from = "Column::Id",
        to = "super::env_var_environments::Column::EnvVarId"
    )]
    EnvVarEnvironments,
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

impl Related<super::env_var_environments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::EnvVarEnvironments.def()
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
