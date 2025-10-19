use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "session_replay_events")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub session_id: i32,
    pub data: String,
    pub timestamp: i64,
    pub r#type: Option<i32>,
    pub is_active: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::session_replay_sessions::Entity",
        from = "Column::SessionId",
        to = "super::session_replay_sessions::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    SessionReplaySessions,
}

impl Related<super::session_replay_sessions::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::SessionReplaySessions.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}