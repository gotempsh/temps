// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Arc;

use chrono::{DateTime, Utc};
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
    /// Whether the route serves traffic as soon as it is created. Defaults
    /// to `true`; send `false` to create the route disabled and enable it
    /// later with `PUT /lb/routes/{domain}`.
    #[serde(default)]
    #[schema(default = true)]
    pub enabled: Option<bool>,
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
    /// When the route was created (ISO 8601, UTC)
    #[schema(value_type = String, format = DateTime, example = "2026-11-25T11:39:26Z")]
    pub created_at: DateTime<Utc>,
    /// When the route was last changed (ISO 8601, UTC)
    #[schema(value_type = String, format = DateTime, example = "2026-11-25T11:39:26Z")]
    pub updated_at: DateTime<Utc>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use temps_entities::custom_routes;

    #[test]
    fn create_request_defaults_to_enabled_and_keeps_an_explicit_false() {
        let omitted: CreateRouteRequest = serde_json::from_str(
            r#"{"domain":"app.example.com","host":"backend.example.com","port":8080}"#,
        )
        .unwrap();
        assert_eq!(omitted.enabled, None);
        let disabled: CreateRouteRequest = serde_json::from_str(
            r#"{"domain":"app.example.com","host":"backend.example.com","port":8080,"enabled":false}"#,
        )
        .unwrap();
        assert_eq!(disabled.enabled, Some(false));
    }

    #[test]
    fn route_timestamps_are_iso_8601_with_a_z_suffix() {
        let at = Utc.with_ymd_and_hms(2026, 11, 25, 11, 39, 26).unwrap();
        let response = RouteResponse::from(custom_routes::Model {
            id: 3,
            domain: "legacy.example.com".to_string(),
            host: "backend.example.com".to_string(),
            port: 8080,
            domain_id: None,
            created_at: at,
            updated_at: at,
            enabled: false,
            route_type: RouteType::Http,
        });
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["created_at"], "2026-11-25T11:39:26Z");
        assert_eq!(json["updated_at"], "2026-11-25T11:39:26Z");
        assert_eq!(json["enabled"], false);
    }
}
