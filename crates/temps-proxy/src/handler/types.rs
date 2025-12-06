use std::sync::Arc;

use serde::{Deserialize, Serialize};
use temps_entities::custom_routes::RouteType;
use utoipa::ToSchema;

use crate::service::lb_service::LbService;

pub struct AppState {
    pub lb_service: Arc<LbService>,
}

pub fn create_lb_app_state(lb_service: Arc<LbService>) -> Arc<AppState> {
    Arc::new(AppState { lb_service })
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct CreateRouteRequest {
    pub domain: String,
    pub host: String,
    pub port: i32,
    /// Route type: "http" (default) matches on HTTP Host header,
    /// "tls" matches on TLS SNI hostname for TCP passthrough
    #[serde(default)]
    pub route_type: Option<String>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct UpdateRouteRequest {
    pub host: String,
    pub port: i32,
    pub enabled: bool,
    /// Route type: "http" (default) matches on HTTP Host header,
    /// "tls" matches on TLS SNI hostname for TCP passthrough
    #[serde(default)]
    pub route_type: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RouteResponse {
    pub id: i32,
    pub domain: String,
    pub host: String,
    pub port: i32,
    pub enabled: bool,
    /// Route type: "http" or "tls"
    pub route_type: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<temps_entities::custom_routes::Model> for RouteResponse {
    fn from(route: temps_entities::custom_routes::Model) -> Self {
        Self {
            id: route.id,
            domain: route.domain,
            host: route.host,
            port: route.port,
            enabled: route.enabled,
            route_type: route.route_type.to_string(),
            created_at: route.created_at.timestamp_millis(),
            updated_at: route.updated_at.timestamp_millis(),
        }
    }
}

/// Helper function to parse route_type string to RouteType enum
pub fn parse_route_type(route_type: Option<&String>) -> Option<RouteType> {
    route_type.map(|rt| match rt.to_lowercase().as_str() {
        "tls" => RouteType::Tls,
        _ => RouteType::Http,
    })
}
