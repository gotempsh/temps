use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "funnel_steps")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub funnel_id: i32,
    pub step_order: i32,
    pub event_name: String,
    pub event_filter: Option<String>, // JSON filter conditions
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::funnels::Entity",
        from = "Column::FunnelId",
        to = "super::funnels::Column::Id"
    )]
    Funnel,
}

impl Related<super::funnels::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Funnel.def()
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
        }

        Ok(self)
    }
}
