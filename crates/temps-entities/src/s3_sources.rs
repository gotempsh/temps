use sea_orm::entity::prelude::*;
use async_trait::async_trait;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "s3_sources")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub bucket_name: String,
    pub region: String,
    pub endpoint: Option<String>,
    pub bucket_path: String,
    pub access_key_id: String,
    pub secret_key: String,
    pub force_path_style: Option<bool>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::backup_schedules::Entity")]
    BackupSchedules,
    #[sea_orm(has_many = "super::backups::Entity")]
    Backups,
}

impl Related<super::backup_schedules::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::BackupSchedules.def()
    }
}

impl Related<super::backups::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Backups.def()
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