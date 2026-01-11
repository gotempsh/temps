use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "performance_metrics")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    pub environment_id: i32,
    pub deployment_id: i32,
    pub ttfb: Option<f32>,
    pub lcp: Option<f32>,
    pub fid: Option<f32>,
    pub fcp: Option<f32>,
    pub cls: Option<f32>,
    pub inp: Option<f32>,
    pub recorded_at: DBDateTime,
    pub ip_address_id: Option<i32>,
    pub session_id: Option<i32>,
    pub visitor_id: Option<i32>,
    pub is_crawler: bool,
    pub pathname: Option<String>,
    pub query: Option<String>,
    pub host: Option<String>,
    pub browser: Option<String>,
    pub browser_version: Option<String>,
    pub operating_system: Option<String>,
    pub operating_system_version: Option<String>,
    pub device_type: Option<String>,
    pub screen_width: Option<i16>,
    pub screen_height: Option<i16>,
    pub viewport_width: Option<i16>,
    pub viewport_height: Option<i16>,
    pub language: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id"
    )]
    Project,
    #[sea_orm(
        belongs_to = "super::environments::Entity",
        from = "Column::EnvironmentId",
        to = "super::environments::Column::Id"
    )]
    Environment,
    #[sea_orm(
        belongs_to = "super::deployments::Entity",
        from = "Column::DeploymentId",
        to = "super::deployments::Column::Id"
    )]
    Deployment,
    #[sea_orm(
        belongs_to = "super::request_sessions::Entity",
        from = "Column::SessionId",
        to = "super::request_sessions::Column::Id"
    )]
    RequestSession,
    #[sea_orm(
        belongs_to = "super::visitor::Entity",
        from = "Column::VisitorId",
        to = "super::visitor::Column::Id"
    )]
    Visitor,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl Related<super::environments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Environment.def()
    }
}

impl Related<super::deployments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Deployment.def()
    }
}

impl Related<super::request_sessions::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::RequestSession.def()
    }
}

impl Related<super::visitor::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Visitor.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
