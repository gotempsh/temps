use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "visitor")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub visitor_id: String,
    pub project_id: i32,
    pub environment_id: i32,
    pub first_seen: DBDateTime,
    pub last_seen: DBDateTime,
    pub user_agent: Option<String>,
    pub ip_address_id: Option<i32>,
    pub is_crawler: bool,
    pub crawler_name: Option<String>,
    #[sea_orm(column_type = "JsonBinary")]
    pub custom_data: Option<serde_json::Value>,
    /// Flag indicating visitor has recorded events/sessions (not a "ghost" visitor)
    pub has_activity: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::ip_geolocations::Entity",
        from = "Column::IpAddressId",
        to = "super::ip_geolocations::Column::Id"
    )]
    IpGeolocations,
    #[sea_orm(has_many = "super::request_sessions::Entity")]
    RequestSessions,
}

impl Related<super::ip_geolocations::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::IpGeolocations.def()
    }
}

impl Related<super::request_sessions::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::RequestSessions.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
