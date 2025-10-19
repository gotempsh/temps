use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;
/// Proxy logs for general traffic - includes unrouted requests, system requests, and errors
/// Separate from request_logs which has strong FK constraints to project/environment/deployment
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "proxy_logs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,

    /// Timestamp of the request (TimescaleDB partition key)
    pub timestamp: DBDateTime,

    /// Request method (GET, POST, etc.)
    pub method: String,

    /// Request path
    pub path: String,

    /// Query string (optional)
    pub query_string: Option<String>,

    /// Host header from request
    pub host: String,

    /// HTTP status code
    pub status_code: i16,

    /// Total response time in milliseconds
    pub response_time_ms: Option<i32>,

    /// Request source: 'proxy', 'api', 'console', 'cli'
    pub request_source: String,

    /// Whether this is a system/admin request (true) or user traffic (false)
    pub is_system_request: bool,

    /// Routing status: 'routed', 'no_project', 'no_environment', 'no_deployment', 'no_container', 'upstream_404', 'error'
    pub routing_status: String,

    /// Project ID (nullable - unrouted requests won't have this)
    pub project_id: Option<i32>,

    /// Environment ID (nullable)
    pub environment_id: Option<i32>,

    /// Deployment ID (nullable)
    pub deployment_id: Option<i32>,

    /// Container ID that handled the request (nullable)
    pub container_id: Option<String>,

    /// Upstream host that was proxied to (nullable)
    pub upstream_host: Option<String>,

    /// Error message if routing failed
    pub error_message: Option<String>,

    /// Client IP address
    pub client_ip: Option<String>,

    /// User agent string
    pub user_agent: Option<String>,

    /// Referrer URL
    pub referrer: Option<String>,

    /// Request ID for tracing (from pingora)
    pub request_id: String,

    /// Foreign key to ip_geolocations table
    pub ip_geolocation_id: Option<i32>,

    /// Browser name from user agent parsing
    pub browser: Option<String>,

    /// Browser version from user agent parsing
    pub browser_version: Option<String>,

    /// Operating system from user agent parsing
    pub operating_system: Option<String>,

    /// Device type (mobile, desktop, tablet)
    pub device_type: Option<String>,

    /// Whether this is a bot/crawler
    pub is_bot: Option<bool>,

    /// Bot/crawler name if detected
    pub bot_name: Option<String>,

    /// Request size in bytes
    pub request_size_bytes: Option<i64>,

    /// Response size in bytes
    pub response_size_bytes: Option<i64>,

    /// Cache status for future caching layer
    pub cache_status: Option<String>,

    /// Request headers (JSON)
    pub request_headers: Option<serde_json::Value>,

    /// Response headers (JSON)
    pub response_headers: Option<serde_json::Value>,

    /// Created date for partitioning (denormalized from timestamp)
    pub created_date: Date,
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
        belongs_to = "super::ip_geolocations::Entity",
        from = "Column::IpGeolocationId",
        to = "super::ip_geolocations::Column::Id"
    )]
    IpGeolocation,
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

impl Related<super::ip_geolocations::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::IpGeolocation.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
