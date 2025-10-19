use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "session_replay_sessions")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub session_replay_id: String,
    pub visitor_id: i32,
    pub project_id: i32,
    pub environment_id: i32,
    pub deployment_id: i32,
    pub created_at: Option<DBDateTime>,
    pub user_agent: Option<String>,
    pub browser: Option<String>,
    pub browser_version: Option<String>,
    pub operating_system: Option<String>,
    pub operating_system_version: Option<String>,
    pub device_type: Option<String>,
    pub viewport_width: Option<i32>,
    pub viewport_height: Option<i32>,
    pub screen_width: Option<i32>,
    pub screen_height: Option<i32>,
    pub language: Option<String>,
    pub timezone: Option<String>,
    pub url: Option<String>,
    pub duration: Option<i32>,
    pub is_active: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::visitor::Entity",
        from = "Column::VisitorId",
        to = "super::visitor::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Visitor,
    #[sea_orm(has_many = "super::session_replay_events::Entity")]
    SessionReplayEvents,
}

impl Related<super::visitor::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Visitor.def()
    }
}

impl Related<super::session_replay_events::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::SessionReplayEvents.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}