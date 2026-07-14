use std::sync::Arc;

use serde::{Deserialize, Serialize};
use temps_entities::custom_routes::RouteType;
use utoipa::ToSchema;

use crate::service::lb_service::LbService;
use temps_core::AuditLogger;

pub struct AppState {
    pub lb_service: Arc<LbService>,
    pub audit_service: Arc<dyn AuditLogger>,
}

pub fn create_lb_app_state(
    lb_service: Arc<LbService>,
    audit_service: Arc<dyn AuditLogger>,
) -> Arc<AppState> {
    Arc::new(AppState {
        lb_service,
        audit_service,
    })
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateRouteRequest {
    pub domain: String,
    pub host: String,
    pub port: i32,
    /// Route type: "http" (default) matches on HTTP Host header,
    /// "tls" matches on TLS SNI hostname for TCP passthrough
    #[serde(default)]
    pub route_type: Option<String>,
    /// Explicitly allow this route to override a Temps-managed domain.
    #[serde(default)]
    pub force_override: bool,
    /// Explicitly acknowledge that this public route targets a private or loopback address.
    #[serde(default)]
    pub allow_private_upstream: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateRouteRequest {
    pub host: String,
    pub port: i32,
    pub enabled: bool,
    /// Route type: "http" (default) matches on HTTP Host header,
    /// "tls" matches on TLS SNI hostname for TCP passthrough
    #[serde(default)]
    pub route_type: Option<String>,
    /// Explicitly acknowledge that this public route targets a private or loopback address.
    #[serde(default)]
    pub allow_private_upstream: bool,
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
    pub force_override: bool,
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
            force_override: route.force_override,
            created_at: route.created_at.timestamp_millis(),
            updated_at: route.updated_at.timestamp_millis(),
        }
    }
}

/// Helper function to parse route_type string to RouteType enum
pub fn parse_route_type(route_type: Option<&String>) -> Result<Option<RouteType>, String> {
    route_type
        .map(
            |route_type| match route_type.to_ascii_lowercase().as_str() {
                "http" => Ok(RouteType::Http),
                "tls" => Ok(RouteType::Tls),
                _ => Err(format!(
                    "invalid route type '{route_type}'; expected 'http' or 'tls'"
                )),
            },
        )
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_request_rejects_unknown_fields() {
        let result = serde_json::from_value::<CreateRouteRequest>(serde_json::json!({
            "domain": "api.example.com",
            "host": "upstream.example.com",
            "port": 8080,
            "unexpected": true
        }));
        assert!(result.is_err());
    }

    #[test]
    fn route_type_parser_rejects_typos() {
        assert_eq!(parse_route_type(None).expect("default type"), None);
        assert_eq!(
            parse_route_type(Some(&"TLS".to_string())).expect("TLS type"),
            Some(RouteType::Tls)
        );
        assert!(parse_route_type(Some(&"tcp".to_string())).is_err());
    }
}
