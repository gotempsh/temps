use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "request_sessions")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(unique)]
    pub session_id: String,
    pub started_at: DBDateTime,
    pub last_accessed_at: DBDateTime,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub referrer: Option<String>,
    pub data: String,
    pub visitor_id: Option<i32>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::visitor::Entity",
        from = "Column::VisitorId",
        to = "super::visitor::Column::Id"
    )]
    Visitor,
}

impl Related<super::visitor::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Visitor.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}