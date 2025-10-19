use sea_orm::entity::prelude::*;
use async_trait::async_trait;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "status_incident_updates")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub incident_id: i32,
    pub status: String, // investigating, identified, monitoring, resolved
    pub message: String,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::status_incidents::Entity",
        from = "Column::IncidentId",
        to = "super::status_incidents::Column::Id"
    )]
    StatusIncident,
}

impl Related<super::status_incidents::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::StatusIncident.def()
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
