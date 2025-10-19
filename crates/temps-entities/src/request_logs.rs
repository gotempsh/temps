use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "request_logs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    pub environment_id: i32,
    pub deployment_id: i32,
    pub date: String,
    pub host: String,
    pub method: String,
    pub request_path: String,
    pub message: String,
    pub status_code: i32,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub request_id: String,
    pub level: String,
    pub user_agent: String,
    pub started_at: String,
    pub finished_at: String,
    pub elapsed_time: Option<i32>,
    pub is_static_file: Option<bool>,
    pub referrer: Option<String>,
    pub ip_address: Option<String>,
    pub session_id: Option<i32>,
    pub headers: Option<String>,
    pub request_headers: Option<String>,
    pub ip_address_id: Option<i32>,
    pub browser: Option<String>,
    pub browser_version: Option<String>,
    pub operating_system: Option<String>,
    pub is_mobile: bool,
    pub is_entry_page: bool,
    pub is_crawler: bool,
    pub crawler_name: Option<String>,
    pub visitor_id: Option<i32>,
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

impl Related<super::visitor::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Visitor.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}