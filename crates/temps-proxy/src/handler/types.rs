use std::sync::Arc;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::service::lb_service::LbService;
use crate::service::request_log_service::RequestLogService;

pub struct AppState {
    pub lb_service: Arc<LbService>,
    pub request_log_service: Arc<RequestLogService>,
}

pub fn create_lb_app_state(
    lb_service: Arc<LbService>,
    request_log_service: Arc<RequestLogService>,
) -> Arc<AppState> {
    Arc::new(AppState {
        lb_service,
        request_log_service,
    })
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct CreateRouteRequest {
    pub domain: String,
    pub host: String,
    pub port: i32,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct UpdateRouteRequest {
    pub host: String,
    pub port: i32,
    pub enabled: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RouteResponse {
    pub id: i32,
    pub domain: String,
    pub host: String,
    pub port: i32,
    pub enabled: bool,
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
            created_at: route.created_at.timestamp_millis(),
            updated_at: route.updated_at.timestamp_millis(),
        }
    }
}
