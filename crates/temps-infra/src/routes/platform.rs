// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Arc;

use axum::{extract::State, http::HeaderMap, response::IntoResponse, routing::get, Json, Router};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use tracing::{debug, info};
use utoipa::OpenApi;

use crate::services::PlatformInfoService;
use crate::types::{PlatformInfo, ServiceAccessInfo};

/// Application state containing the platform info service
pub trait InfraAppState: Send + Sync + 'static {
    fn platform_info_service(&self) -> &PlatformInfoService;
}

/// OpenAPI documentation for platform information endpoints
#[derive(OpenApi)]
#[openapi(
    paths(get_platform_info, get_public_ip, get_private_ip, get_access_info),
    components(
        schemas(PlatformInfo, ServiceAccessInfo)
    ),
    tags(
        (name = "Platform", description = "Platform information and compatibility")
    )
)]
pub struct PlatformInfoApiDoc;

/// Get platform information
#[utoipa::path(
    get,
    path = "/.well-known/temps.json",
    responses(
        (status = 200, description = "Successfully retrieved platform information", body = PlatformInfo),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    tag = "Platform",
    security(("bearer_auth" = []))
)]
pub async fn get_platform_info<T>(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<T>>,
) -> Result<impl IntoResponse, Problem>
where
    T: InfraAppState,
{
    permission_guard!(auth, PlatformInfoRead);

    info!("Getting platform info");

    match app_state.platform_info_service().get_platform_info().await {
        Ok(platform_info) => Ok(Json(serde_json::json!({
            "platforms": platform_info.platforms
        }))),
        Err(e) => {
            tracing::error!("Failed to get platform info: {}", e);
            Ok(Json(serde_json::json!({
                "platforms": ["linux/amd64"]  // Fallback to default
            })))
        }
    }
}

/// Get public IP address of the server
#[utoipa::path(
    get,
    path = "/platform/public-ip",
    responses(
        (status = 200, description = "Successfully retrieved public IP address"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    tag = "Platform",
    security(("bearer_auth" = []))
)]
pub async fn get_public_ip<T>(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<T>>,
) -> Result<impl IntoResponse, Problem>
where
    T: InfraAppState,
{
    permission_guard!(auth, PlatformInfoRead);

    info!("Getting public IP address");

    let ip_info = app_state.platform_info_service().get_public_ip().await;

    if let Some(ip) = ip_info.ip {
        Ok(Json(serde_json::json!({
            "ip": ip,
            "source": ip_info.source
        })))
    } else {
        Ok(Json(serde_json::json!({
            "error": ip_info.error.unwrap_or_else(|| "Unable to determine public IP address".to_string()),
            "ip": null
        })))
    }
}

/// Get private/local IP address of the server
#[utoipa::path(
    get,
    path = "/platform/private-ip",
    responses(
        (status = 200, description = "Successfully retrieved private IP address"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    tag = "Platform",
    security(("bearer_auth" = []))
)]
pub async fn get_private_ip<T>(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<T>>,
) -> Result<impl IntoResponse, Problem>
where
    T: InfraAppState,
{
    permission_guard!(auth, PlatformInfoRead);

    info!("Getting private IP address");

    match app_state.platform_info_service().get_private_ip().await {
        Ok(ip_info) => Ok(Json(serde_json::json!({
            "primary_ip": ip_info.primary_ip,
            "ipv4_addresses": ip_info.ipv4_addresses,
            "ipv6_addresses": ip_info.ipv6_addresses
        }))),
        Err(e) => Ok(Json(serde_json::json!({
            "error": "Unable to get network interfaces",
            "details": e.to_string()
        }))),
    }
}

/// Get information about how the service is being accessed
///
/// Returns details about the server's access mode, public IP address, private IP address,
/// and domain creation capabilities. Both IP addresses are always included when available.
#[utoipa::path(
    get,
    path = "/platform/access-info",
    responses(
        (status = 200, description = "Service access information", body = ServiceAccessInfo),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Platform",
    security(("bearer_auth" = []))
)]
pub async fn get_access_info<T>(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<T>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, Problem>
where
    T: InfraAppState,
{
    permission_guard!(auth, PlatformInfoRead);

    debug!("Getting service access information");

    // Get server mode using the enhanced service
    let server_mode = app_state
        .platform_info_service()
        .get_server_mode_from_headers(&headers)
        .await;

    // Always get both public and private IPs (with automatic fallback to fetch if not cached)
    let public_ip = app_state
        .platform_info_service()
        .get_public_ip_with_fallback()
        .await;
    let private_ip = app_state
        .platform_info_service()
        .get_private_ip_with_fallback()
        .await;

    Ok(Json(ServiceAccessInfo {
        access_mode: server_mode.to_string(),
        public_ip,
        private_ip,
        can_create_domains: server_mode.can_create_domains(),
        domain_creation_error: server_mode
            .domain_creation_error_message()
            .map(|s| s.to_string()),
    }))
}

/// Configure platform infrastructure routes
///
/// This function returns a router with all platform-related routes configured.
/// The generic parameter T must implement InfraAppState to provide access to
/// the platform info service.
pub fn configure_platform_routes<T>() -> Router<Arc<T>>
where
    T: InfraAppState,
{
    Router::new()
        .route("/.well-known/temps.json", get(get_platform_info::<T>))
        .route("/platform/public-ip", get(get_public_ip::<T>))
        .route("/platform/private-ip", get(get_private_ip::<T>))
        .route("/platform/access-info", get(get_access_info::<T>))
}
