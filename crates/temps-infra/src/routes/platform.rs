// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Arc;

use axum::http::StatusCode;
use axum::{extract::State, http::HeaderMap, response::IntoResponse, routing::get, Json, Router};
use temps_authz::{permission_guard, RequireAuth};
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
/// Returns how this request reached the server, and its public and private IP
/// addresses when available. Which ways a domain can get HTTPS is per domain:
/// `GET /ingress/certificate-methods?domain=`.
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
        // A tunnel or proxy in front of this request's hostname blocks no
        // other domain (ADR 058 D10): the methods are judged per domain.
        can_create_domains: true,
        domain_creation_error: None,
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

    // ------------------------------------------------------------------
    // GET /platform/access-info
    // ------------------------------------------------------------------

    use axum::body::Body;
    use axum::http::Request;
    use temps_authz::{AuthContext, Permission, Role};
    use tower::ServiceExt;

    struct TestState(PlatformInfoService);

    impl InfraAppState for TestState {
        fn platform_info_service(&self) -> &PlatformInfoService {
            &self.0
        }
    }

    fn user() -> temps_entities::users::Model {
        let now = chrono::Utc::now();
        temps_entities::users::Model {
            id: 5,
            name: "Operator".into(),
            email: "operator@example.test".into(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// `GET /platform/access-info` on `host`, with `headers`, as `auth`.
    async fn access_info(
        host: &str,
        headers: &[(&str, &str)],
        auth: Option<AuthContext>,
    ) -> (StatusCode, serde_json::Value) {
        let service = PlatformInfoService::without_docker();
        // Documentation addresses: no lookup leaves the test.
        service.seed_network_cache("203.0.113.7", "10.0.0.7");
        let app = configure_platform_routes::<TestState>().with_state(Arc::new(TestState(service)));
        let mut request = Request::builder()
            .uri("/platform/access-info")
            .header("host", host);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let mut request = request.body(Body::empty()).expect("request");
        if let Some(auth) = auth {
            request.extensions_mut().insert(auth);
        }
        let response = app.oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    fn admin() -> Option<AuthContext> {
        Some(AuthContext::new_session(user(), Role::Admin))
    }

    #[tokio::test]
    async fn access_info_refuses_anonymous_callers() {
        let (status, _) = access_info("console.example.com", &[], None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn access_info_refuses_a_key_without_platform_read() {
        let key = AuthContext::new_api_key(
            user(),
            None,
            Some(vec![Permission::ProjectsRead]),
            "projects-only".into(),
            8,
        );
        let (status, _) = access_info("console.example.com", &[], Some(key)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    /// The reported bug: a console opened through a Cloudflare Tunnel said
    /// no domain could be added. The tunnel covers its own hostnames only.
    #[tokio::test]
    async fn a_request_through_cloudflare_still_allows_adding_domains() {
        let (status, body) = access_info(
            "console.example.com",
            &[("cf-ray", "0000000000000000-AAA")],
            admin(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["access_mode"], "cloudflare_tunnel");
        assert_eq!(body["can_create_domains"], true);
        assert!(body["domain_creation_error"].is_null());
        // The server's own address is still reported, for direct domains.
        assert_eq!(body["public_ip"], "203.0.113.7");
    }

    #[tokio::test]
    async fn a_local_request_still_allows_adding_domains() {
        let (status, body) = access_info("127.0.0.1:8081", &[], admin()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["access_mode"], "local");
        assert_eq!(body["can_create_domains"], true);
        assert!(body["domain_creation_error"].is_null());
    }

    #[tokio::test]
    async fn a_direct_request_reports_nat_from_the_cached_addresses() {
        let (status, body) = access_info("console.example.com", &[], admin()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["access_mode"], "nat");
        assert_eq!(body["private_ip"], "10.0.0.7");
        assert_eq!(body["can_create_domains"], true);
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
