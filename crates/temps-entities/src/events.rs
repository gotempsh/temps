//! `SeaORM` Entity for events table

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "events")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub timestamp: DBDateTime,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    
    // Session tracking
    pub session_id: Option<String>,
    pub visitor_id: Option<i32>,
    
    // Page data
    pub hostname: String,
    pub pathname: String,
    pub page_path: String, // For analytics grouping
    pub href: String,
    pub querystring: Option<String>,
    pub page_title: Option<String>,
    pub referrer: Option<String>,
    pub referrer_hostname: Option<String>,
    
    // Session flow tracking
    pub is_entry: bool,
    pub is_exit: bool,
    pub is_bounce: bool,
    pub time_on_page: Option<i32>,
    pub session_page_number: Option<i32>,
    
    // User interaction metrics
    pub scroll_depth: Option<i32>,
    pub clicks: Option<i32>,
    pub custom_properties: Option<serde_json::Value>,
    
    // Performance metrics
    pub lcp: Option<f32>,
    pub cls: Option<f32>,
    pub inp: Option<f32>,
    pub fcp: Option<f32>,
    pub ttfb: Option<f32>,
    pub fid: Option<f32>,
    
    // Device/Browser
    pub browser: Option<String>,
    pub browser_version: Option<String>,
    pub operating_system: Option<String>,
    pub operating_system_version: Option<String>,
    pub device_type: Option<String>,
    pub screen_width: Option<i16>,
    pub screen_height: Option<i16>,
    pub viewport_width: Option<i16>,
    pub viewport_height: Option<i16>,
    
    // Geography (cached)
    pub ip_geolocation_id: Option<i32>,
    
    // Traffic source
    pub channel: Option<String>,
    pub utm_source: Option<String>,
    pub utm_medium: Option<String>,
    pub utm_campaign: Option<String>,
    pub utm_term: Option<String>,
    pub utm_content: Option<String>,
    
    // Event details
    pub event_type: String,
    pub event_name: Option<String>,
    pub props: Option<serde_json::Value>,
    
    // Analytics compatibility fields
    pub event_data: Option<String>,
    pub request_query: Option<String>,
    
    // Metadata
    pub user_agent: Option<String>,
    pub is_crawler: bool,
    pub crawler_name: Option<String>,
    pub language: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Projects,
    #[sea_orm(
        belongs_to = "super::deployments::Entity",
        from = "Column::DeploymentId",
        to = "super::deployments::Column::Id",
        on_update = "NoAction",
        on_delete = "SetNull"
    )]
    Deployments,
    #[sea_orm(
        belongs_to = "super::environments::Entity",
        from = "Column::EnvironmentId",
        to = "super::environments::Column::Id",
        on_update = "NoAction",
        on_delete = "SetNull"
    )]
    Environments,
    #[sea_orm(
        belongs_to = "super::visitor::Entity",
        from = "Column::VisitorId",
        to = "super::visitor::Column::Id",
        on_update = "NoAction",
        on_delete = "SetNull"
    )]
    Visitor,
    #[sea_orm(
        belongs_to = "super::ip_geolocations::Entity",
        from = "Column::IpGeolocationId",
        to = "super::ip_geolocations::Column::Id",
        on_update = "NoAction",
        on_delete = "SetNull"
    )]
    IpGeolocations,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Projects.def()
    }
}

impl Related<super::deployments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Deployments.def()
    }
}

impl Related<super::environments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Environments.def()
    }
}

impl Related<super::visitor::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Visitor.def()
    }
}

impl Related<super::ip_geolocations::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::IpGeolocations.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}