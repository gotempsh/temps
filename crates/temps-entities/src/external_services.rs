use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "external_services")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub service_type: String,
    pub version: Option<String>,
    pub status: String,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    pub slug: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::external_service_params::Entity")]
    Params,
    #[sea_orm(has_many = "super::external_service_backups::Entity")]
    Backups,
    #[sea_orm(has_many = "super::project_services::Entity")]
    ProjectServices,
}

impl Related<super::external_service_params::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Params.def()
    }
}

impl Related<super::external_service_backups::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Backups.def()
    }
}

impl Related<super::project_services::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ProjectServices.def()
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
