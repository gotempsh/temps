use sea_orm::entity::prelude::*;
use async_trait::async_trait;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "notifications")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub notification_id: String,
    pub title: String,
    pub message: String,
    pub notification_type: String,
    pub priority: String,
    pub metadata: String,
    pub created_at: DBDateTime,
    pub sent_at: Option<DBDateTime>,
    pub batch_key: String,
    pub occurrence_count: i32,
    pub next_allowed_at: DBDateTime,
    pub is_read: bool,
    pub read_at: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

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
        } else {
        }
        
        Ok(self)
    }
}