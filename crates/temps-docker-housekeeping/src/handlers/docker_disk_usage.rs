// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use utoipa::OpenApi;

use crate::services::{
    DockerDiskUsage, DockerDiskUsageCategory, DockerDiskUsageError, DockerDiskUsageService,
};

#[derive(OpenApi)]
#[openapi(
    paths(node_docker_disk_usage),
    components(schemas(DockerDiskUsage, DockerDiskUsageCategory))
)]
pub struct DockerDiskUsageApiDoc;

pub fn configure_routes() -> Router<Arc<DockerDiskUsageService>> {
    Router::new().route(
        "/nodes/{node_id}/docker-disk-usage",
        get(node_docker_disk_usage),
    )
}

/// Docker disk usage (`docker system df`) for the control-plane host.
///
/// On-demand rather than sampled: walking every layer, rootfs and volume can
/// take seconds, so the server monitoring page fetches it when opened or when
/// the operator presses refresh. Node `0` only for now — a worker's daemon is
/// only reachable through its agent, which does not relay this yet, and the
/// error says exactly that instead of 404ing.
#[utoipa::path(
    tag = "Nodes",
    get,
    path = "/nodes/{node_id}/docker-disk-usage",
    operation_id = "NodeDockerDiskUsageGet",
    params(
        ("node_id" = i32, Path, description = "Node ID (0 = control plane)")
    ),
    responses(
        (status = 200, description = "Docker disk usage by category", body = DockerDiskUsage),
        (status = 400, description = "Node is not the control plane"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 502, description = "Docker daemon answered with an unexpected response"),
        (status = 503, description = "Docker daemon unreachable"),
        (status = 504, description = "Docker daemon timed out"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn node_docker_disk_usage(
    RequireAuth(auth): RequireAuth,
    State(service): State<Arc<DockerDiskUsageService>>,
    Path(node_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);

    let usage = service.fetch(node_id).await.map_err(Problem::from)?;

    Ok(Json(usage))
}

impl From<DockerDiskUsageError> for Problem {
    fn from(error: DockerDiskUsageError) -> Self {
        match error {
            DockerDiskUsageError::UnsupportedNode { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Docker Disk Usage Not Available For Node")
                    .with_detail(error.to_string())
            }
            DockerDiskUsageError::Unavailable { .. } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("Docker Daemon Unreachable")
                    .with_detail(error.to_string())
            }
            DockerDiskUsageError::Timeout { .. } => {
                problemdetails::new(StatusCode::GATEWAY_TIMEOUT)
                    .with_title("Docker Daemon Timed Out")
                    .with_detail(error.to_string())
            }
            DockerDiskUsageError::UnexpectedStatus { .. } | DockerDiskUsageError::Parse { .. } => {
                problemdetails::new(StatusCode::BAD_GATEWAY)
                    .with_title("Unexpected Docker Daemon Response")
                    .with_detail(error.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn user() -> temps_entities::users::Model {
        temps_entities::users::Model {
            id: 1,
            name: "Reader".to_string(),
            email: "reader@example.com".to_string(),
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
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn api_key(permission: temps_auth::Permission) -> temps_auth::AuthContext {
        temps_auth::AuthContext::new_api_key(
            user(),
            None,
            Some(vec![permission]),
            "key".to_string(),
            7,
        )
    }

    async fn get_usage(uri: &str, auth: Option<temps_auth::AuthContext>) -> (StatusCode, String) {
        let mut request = Request::builder()
            .uri(uri)
            .body(Body::empty())
            .expect("request");
        if let Some(auth) = auth {
            request.extensions_mut().insert(auth);
        }
        let response = configure_routes()
            .with_state(Arc::new(DockerDiskUsageService::local()))
            .oneshot(request)
            .await
            .expect("response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn the_report_refuses_anonymous_callers() {
        let (status, _) = get_usage("/nodes/0/docker-disk-usage", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn the_report_needs_settings_read() {
        let outsider = api_key(temps_auth::Permission::ProjectsRead);
        let (status, _) = get_usage("/nodes/0/docker-disk-usage", Some(outsider)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_worker_node_is_a_400_naming_the_problem() {
        let reader = api_key(temps_auth::Permission::SettingsRead);
        let (status, body) = get_usage("/nodes/4/docker-disk-usage", Some(reader)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body.contains("Docker Disk Usage Not Available For Node"),
            "{body}"
        );
    }

    #[test]
    fn daemon_failures_keep_their_own_status() {
        let host = || "docker-host".to_string();
        for (error, status) in [
            (
                DockerDiskUsageError::Unavailable {
                    host: host(),
                    reason: "refused".to_string(),
                },
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                DockerDiskUsageError::Timeout {
                    host: host(),
                    timeout_secs: 30,
                },
                StatusCode::GATEWAY_TIMEOUT,
            ),
            (
                DockerDiskUsageError::UnexpectedStatus {
                    host: host(),
                    status: 500,
                    body: "boom".to_string(),
                },
                StatusCode::BAD_GATEWAY,
            ),
            (
                DockerDiskUsageError::Parse {
                    host: host(),
                    reason: "not json".to_string(),
                },
                StatusCode::BAD_GATEWAY,
            ),
        ] {
            assert_eq!(Problem::from(error).status_code, status);
        }
    }
}
