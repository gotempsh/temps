// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use temps_entities::custom_routes::RouteType;
use utoipa::ToSchema;

use crate::service::lb_service::LbService;

pub struct AppState {
    pub lb_service: Arc<LbService>,
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
}

pub fn create_lb_app_state(
    lb_service: Arc<LbService>,
    audit_service: Arc<dyn temps_core::AuditLogger>,
) -> Arc<AppState> {
    Arc::new(AppState {
        lb_service,
        audit_service,
    })
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
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
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
            created_at: route.created_at,
            updated_at: route.updated_at,
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
