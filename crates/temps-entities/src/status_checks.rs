use sea_orm::entity::prelude::*;
use async_trait::async_trait;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "status_checks")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i32,
    #[sea_orm(primary_key, auto_increment = false)]
    pub checked_at: DBDateTime,
    pub monitor_id: i32,
    pub status: String, // operational, degraded, down
    pub response_time_ms: Option<i32>,
    pub error_message: Option<String>,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::status_monitors::Entity",
        from = "Column::MonitorId",
        to = "super::status_monitors::Column::Id"
    )]
    StatusMonitor,
}

impl Related<super::status_monitors::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::StatusMonitor.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        if insert {
            if self.created_at.is_not_set() {
                let now = chrono::Utc::now();
                self.created_at = Set(now);
            }
        }

        Ok(self)
    }
}
