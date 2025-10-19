use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "cron_executions")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub cron_id: i32,
    pub executed_at: DBDateTime,
    pub url: String,
    pub status_code: i32,
    pub headers: String,
    pub response_time_ms: i32,
    pub error_message: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::crons::Entity",
        from = "Column::CronId",
        to = "super::crons::Column::Id"
    )]
    Cron,
}

impl Related<super::crons::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Cron.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}