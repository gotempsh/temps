// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Arc;

use axum::http::StatusCode;
use axum::{extract::State, http::HeaderMap, response::IntoResponse, routing::get, Json, Router};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use tracing::{debug, info};
use utoipa::OpenApi;

use crate::services::{PlatformInfoError, PlatformInfoService};
use crate::types::{
    NetworkInterface, PlatformFeatures, PlatformInfo, PrivateIpInfo, PublicIpInfo,
    ServiceAccessInfo,
};

// ---------------------------------------------------------------------------
// Error conversion: PlatformInfoError -> Problem (RFC 7807)
// ---------------------------------------------------------------------------

impl From<PlatformInfoError> for Problem {
    fn from(err: PlatformInfoError) -> Self {
        match err {
            // The caller asked for Docker-derived data on a process that has
            // no daemon.  This is a client-visible conflict — the client
            // should consult the /platform/features endpoint instead of
            // retrying, and join a worker node if they need container
            // platform info.  Routed through the shared mapping in
            // `temps_core` so the status, `error_code`, title, remedy and
            // `setup_path` match every other endpoint that can hit it.
            PlatformInfoError::DockerUnavailable { .. } => {
                temps_core::worker_node_required_problem(err.to_string())
            }

            // Docker daemon connectivity / protocol errors.
            PlatformInfoError::Docker(_) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Docker Error")
                .with_detail(err.to_string()),

            // OS-level failure enumerating network interfaces.
            PlatformInfoError::NetworkInterfaces(_) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Network Interface Error")
                    .with_detail(err.to_string())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// App-state trait
// ---------------------------------------------------------------------------

/// Application state containing the platform info service
pub trait InfraAppState: Send + Sync + 'static {
    fn platform_info_service(&self) -> &PlatformInfoService;
}

// ---------------------------------------------------------------------------
// OpenAPI doc
// ---------------------------------------------------------------------------

/// OpenAPI documentation for platform information endpoints
#[derive(OpenApi)]
#[openapi(
    paths(
        get_platform_info,
        get_platform_features,
        get_public_ip,
        get_private_ip,
        get_access_info
    ),
    components(
        schemas(
            PlatformFeatures,
            PlatformInfo,
            ServiceAccessInfo,
            PublicIpInfo,
            PrivateIpInfo,
            NetworkInterface
        )
    ),
    tags(
        (name = "Platform", description = "Platform information and compatibility")
    )
)]
pub struct PlatformInfoApiDoc;

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Get platform information
#[utoipa::path(
    get,
    path = "/.well-known/temps.json",
    responses(
        (status = 200, description = "Successfully retrieved platform information", body = PlatformInfo),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 409, description = "Docker daemon unavailable in this profile"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Platform",
    security(("bearer_auth" = []))
)]
pub async fn get_platform_info<T>(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<T>>,
) -> Result<Json<PlatformInfo>, Problem>
where
    T: InfraAppState,
{
    permission_guard!(auth, PlatformInfoRead);

    info!("Getting platform info");

    let platform_info = app_state
        .platform_info_service()
        .get_platform_info()
        .await
        .map_err(Problem::from)?;

    Ok(Json(platform_info))
}

/// Report which capabilities this server process actually provides.
///
/// Always present, in every profile — a client must be able to tell
/// "unavailable here, and here is why" from "endpoint does not exist".
#[utoipa::path(
    get,
    path = "/platform/features",
    responses(
        (status = 200, description = "Capabilities of this server process", body = PlatformFeatures),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    tag = "Platform",
    security(("bearer_auth" = []))
)]
pub async fn get_platform_features<T>(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<T>>,
) -> Result<Json<PlatformFeatures>, Problem>
where
    T: InfraAppState,
{
    permission_guard!(auth, PlatformInfoRead);

    debug!("Reporting platform features");

    Ok(Json(app_state.platform_info_service().features().clone()))
}

/// Get public IP address of the server
#[utoipa::path(
    get,
    path = "/platform/public-ip",
    responses(
        (status = 200, description = "Successfully retrieved public IP address", body = PublicIpInfo),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    tag = "Platform",
    security(("bearer_auth" = []))
)]
pub async fn get_public_ip<T>(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<T>>,
) -> Result<Json<PublicIpInfo>, Problem>
where
    T: InfraAppState,
{
    permission_guard!(auth, PlatformInfoRead);

    info!("Getting public IP address");

    let mut ip_info = app_state.platform_info_service().get_public_ip().await;
    if ip_info.ip.is_none() && ip_info.error.is_none() {
        ip_info.error = Some("Unable to determine public IP address".to_string());
    }

    Ok(Json(ip_info))
}

/// Get private/local IP address of the server
#[utoipa::path(
    get,
    path = "/platform/private-ip",
    responses(
        (status = 200, description = "Successfully retrieved private IP address", body = PrivateIpInfo),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Failed to enumerate network interfaces"),
    ),
    tag = "Platform",
    security(("bearer_auth" = []))
)]
pub async fn get_private_ip<T>(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<T>>,
) -> Result<Json<PrivateIpInfo>, Problem>
where
    T: InfraAppState,
{
    permission_guard!(auth, PlatformInfoRead);

    info!("Getting private IP address");

    let ip_info = app_state
        .platform_info_service()
        .get_private_ip()
        .await
        .map_err(Problem::from)?;

    Ok(Json(ip_info))
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

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

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
        .route("/platform/features", get(get_platform_features::<T>))
        .route("/platform/public-ip", get(get_public_ip::<T>))
        .route("/platform/private-ip", get(get_private_ip::<T>))
        .route("/platform/access-info", get(get_access_info::<T>))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_unavailable_maps_to_409_conflict() {
        let err = PlatformInfoError::DockerUnavailable {
            profile: temps_core::PROFILE_CONTROL_PLANE.to_string(),
            reason: "no socket".to_string(),
        };
        let problem = Problem::from(err);
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
    }

    #[test]
    fn network_interfaces_error_maps_to_500() {
        // We can't construct bollard::errors::Error directly without a live
        // daemon, so we test the NetworkInterfaces arm as a proxy for the 500
        // mapping, and rely on the compiler's exhaustiveness check for the
        // Docker arm.
        let err = PlatformInfoError::NetworkInterfaces(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "access denied",
        ));
        let problem = Problem::from(err);
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn network_interfaces_not_found_maps_to_500() {
        let err = PlatformInfoError::NetworkInterfaces(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no such device",
        ));
        let problem = Problem::from(err);
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }
}
