// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Restricted runtime-health protocol. Legacy listeners never mount it.

use crate::{
    auth::{require_agent_auth, AgentAuth, RequireAgentAuth},
    AgentConfig,
};
use axum::{
    extract::State,
    http::StatusCode,
    middleware,
    routing::{get, post},
    Extension, Json, Router,
};
use std::sync::Arc;
use temps_auth::permission_guard;
use temps_core::problemdetails::{self, Problem};
use temps_runtime_health::host::{
    HostError, HostIdentity, LeaseRequest, ProbeHost, ReportBatch, ReportRequest, RevokeRequest,
};
use tokio::sync::OnceCell;
use utoipa::OpenApi;

type HostSlot = Arc<OnceCell<Arc<ProbeHost>>>;

fn unavailable() -> Problem {
    problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
        .with_title("Runtime health host unavailable")
        .with_detail(
            "The worker's protected runtime health journal or Docker daemon is unavailable",
        )
}

fn host_error(error: HostError) -> Problem {
    let status = match &error {
        HostError::Fenced => StatusCode::CONFLICT,
        HostError::Limit | HostError::Engine(_) => StatusCode::BAD_REQUEST,
        HostError::Journal { .. }
        | HostError::Decode { .. }
        | HostError::JournalFailed
        | HostError::Coordination { .. } => StatusCode::SERVICE_UNAVAILABLE,
    };
    tracing::warn!(%error, "Runtime health request rejected");
    problemdetails::new(status)
        .with_title("Runtime health request rejected")
        .with_detail(
            "Reconcile the current worker session and deployment authorization before retrying",
        )
}

pub fn router(
    docker: Option<bollard::Docker>,
    config: &AgentConfig,
    auth: Arc<AgentAuth>,
) -> Router {
    if !config.require_mtls
        || config.tls_cert_path.is_none()
        || config.tls_key_path.is_none()
        || config.cluster_ca_path.is_none()
    {
        return Router::new();
    }
    let slot = Arc::new(OnceCell::new());
    if let Some(docker) = docker {
        let slot = slot.clone();
        tokio::spawn(async move {
            let docker = Arc::new(docker);
            loop {
                match temps_deployer::runtime_health::open_host(
                    docker.clone(),
                    temps_deployer::runtime_health::data_root(),
                    true,
                )
                .await
                {
                    Ok(host) => {
                        temps_deployer::runtime_health::spawn_host(host.clone());
                        let _ = slot.set(host);
                        tracing::info!(
                            event = "runtime_health_host_started",
                            "Worker runtime health engine started"
                        );
                        break;
                    }
                    Err(error) => {
                        tracing::warn!(%error, "Worker runtime health host unavailable; retrying")
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            }
        });
    }
    routes(slot, auth)
}

fn routes(slot: HostSlot, auth: Arc<AgentAuth>) -> Router {
    Router::new()
        .route("/agent/runtime-health", get(identity))
        .route("/agent/runtime-health/authorize", post(authorize))
        .route("/agent/runtime-health/revoke", post(revoke))
        .route("/agent/runtime-health/report", post(report))
        .layer(axum::extract::DefaultBodyLimit::max(32_768))
        .layer(middleware::from_fn(require_agent_auth))
        .layer(Extension(auth))
        .with_state(slot)
}

#[utoipa::path(get, path = "/agent/runtime-health", tag = "Runtime health", responses((status = 200, body = HostIdentity), (status = 401, description = "Node authentication required"), (status = 503, description = "Host unavailable")), security(("bearer_auth" = [])))]
async fn identity(
    RequireAgentAuth(auth): RequireAgentAuth,
    State(slot): State<HostSlot>,
) -> Result<Json<HostIdentity>, Problem> {
    permission_guard!(auth, DeploymentsRead);
    let host = slot.get().ok_or_else(unavailable)?;
    Ok(Json(host.identity()))
}

#[utoipa::path(post, path = "/agent/runtime-health/authorize", tag = "Runtime health", request_body = LeaseRequest, responses((status = 204, description = "Lease durably authorized"), (status = 400, description = "Invalid bounded assignment"), (status = 401, description = "Node authentication required"), (status = 409, description = "Stale authorization"), (status = 503, description = "Host unavailable")), security(("bearer_auth" = [])))]
async fn authorize(
    RequireAgentAuth(auth): RequireAgentAuth,
    State(slot): State<HostSlot>,
    Json(request): Json<LeaseRequest>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, DeploymentsCreate);
    let host = slot.get().cloned().ok_or_else(unavailable)?;
    tokio::task::spawn_blocking(move || host.authorize(request))
        .await
        .map_err(|_| unavailable())?
        .map_err(host_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/agent/runtime-health/revoke", tag = "Runtime health", request_body = RevokeRequest, responses((status = 204, description = "Lease durably revoked"), (status = 401, description = "Node authentication required"), (status = 409, description = "Stale authorization"), (status = 503, description = "Host unavailable")), security(("bearer_auth" = [])))]
async fn revoke(
    RequireAgentAuth(auth): RequireAgentAuth,
    State(slot): State<HostSlot>,
    Json(request): Json<RevokeRequest>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, DeploymentsCreate);
    let host = slot.get().cloned().ok_or_else(unavailable)?;
    tokio::task::spawn_blocking(move || {
        host.revoke(request.session, request.assignment_id, request.epoch)
    })
    .await
    .map_err(|_| unavailable())?
    .map_err(host_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/agent/runtime-health/report", tag = "Runtime health", request_body = ReportRequest, responses((status = 200, body = ReportBatch), (status = 400, description = "Invalid bounded request"), (status = 401, description = "Node authentication required"), (status = 409, description = "Stale session"), (status = 503, description = "Host unavailable")), security(("bearer_auth" = [])))]
async fn report(
    RequireAgentAuth(auth): RequireAgentAuth,
    State(slot): State<HostSlot>,
    Json(request): Json<ReportRequest>,
) -> Result<Json<ReportBatch>, Problem> {
    permission_guard!(auth, DeploymentsRead);
    slot.get()
        .ok_or_else(unavailable)?
        .report(request)
        .map(Json)
        .map_err(host_error)
}

#[derive(OpenApi)]
#[openapi(
    paths(identity, authorize, revoke, report),
    components(schemas(HostIdentity, LeaseRequest, ReportRequest, ReportBatch, RevokeRequest))
)]
pub struct RuntimeHealthApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    #[tokio::test]
    async fn node_authentication_is_required_and_an_unavailable_host_is_explicit() {
        let router = routes(
            Arc::new(OnceCell::new()),
            Arc::new(AgentAuth::new("fixture-node-token")),
        );
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/agent/runtime-health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/agent/runtime-health")
                    .header("Authorization", "Bearer another-node-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/agent/runtime-health")
                    .header("Authorization", "Bearer fixture-node-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn legacy_plaintext_workers_do_not_mount_the_runtime_protocol() {
        let config:AgentConfig=serde_json::from_value(serde_json::json!({"listen_address":"127.0.0.1:0","token":"fixture-node-token","node_name":"worker","control_plane_url":"https://control-plane.test","node_id":1})).unwrap();
        assert!(!config.require_mtls);
        let router = router(None, &config, Arc::new(AgentAuth::new(&config.token)));
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/agent/runtime-health")
                    .header("Authorization", "Bearer fixture-node-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
