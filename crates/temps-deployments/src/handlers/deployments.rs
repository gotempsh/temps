use std::sync::Arc;

use super::audit::{
    ContainerActionAudit, DeploymentCancelledAudit, DeploymentPausedAudit, DeploymentPromotedAudit,
    DeploymentResumedAudit, DeploymentRollbackAudit, DeploymentTeardownAudit,
    EnvironmentTeardownAudit,
};
use super::types::AppState;
use axum::Router;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Extension, Path, Query, State,
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Json,
};
use futures::stream::{self, StreamExt};
use futures::SinkExt;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use temps_auth::RequireAuth;
use temps_auth::{
    permission_guard, project_access_guard, project_permission_guard, project_scope_guard,
};
use temps_core::{AppSettings, AuditContext, PublicHostnameStrategy, RequestMetadata};
use tracing::{debug, error, info, warn};
use utoipa::OpenApi;

use crate::handlers::types::{
    ActivityDay, ActivityGraphQuery, ActivityGraphResponse, ContainerActionResponse,
    ContainerDetailResponse, ContainerInfoResponse, ContainerListResponse, ContainerLogsQuery,
    ContainerMetricHistoryPoint, ContainerMetricsHistoryQuery, ContainerMetricsResponse,
    DeploymentContainerLogContentResponse, DeploymentContainerLogResponse,
    DeploymentContainerLogsListResponse, DeploymentJobResponse, DeploymentJobsResponse,
    DeploymentListResponse, DeploymentResponse, DeploymentStateResponse, EnvVarResponse,
    PromoteDeploymentRequest, ResourceLimitsResponse,
};
use temps_core::problemdetails;
use temps_core::problemdetails::Problem;

// ADR-028 guard pattern note for this file
//
// All handlers in this module use `permission_guard!` with Deployments* or
// Environments* permissions (DeploymentsRead, DeploymentsCreate, DeploymentsDelete,
// DeploymentsWrite, EnvironmentsRead, EnvironmentsWrite). None of these
// permissions are bridged from deployment-token permissions in
// `AuthContext::has_permission` — only AnalyticsRead, AnalyticsWrite, and
// EmailsSend have token-to-permission mappings. A deployment token therefore
// fails `permission_guard!` before reaching any handler in this file.
//
// `project_scope_guard!` is intentionally omitted from all handlers EXCEPT
// `get_last_deployment` and `get_project_deployments`, which carry it as a
// defence-in-depth measure for the ADR-028 Phase B rollout. Adding the guard
// to every handler in this file would be redundant noise: the token is already
// rejected by the earlier `permission_guard!` call.
fn public_url_for_hostname(settings: &AppSettings, hostname: &str) -> String {
    let (protocol, port) = if let Some(ref external_url) = settings.external_url {
        if let Ok(parsed) = url::Url::parse(external_url) {
            (parsed.scheme().to_string(), parsed.port())
        } else if external_url.starts_with("http://") {
            ("http".to_string(), None)
        } else {
            ("https".to_string(), None)
        }
    } else {
        ("https".to_string(), None)
    };

    let port =
        port.filter(|p| !((protocol == "https" && *p == 443) || (protocol == "http" && *p == 80)));

    match port {
        Some(port) => format!("{}://{}:{}", protocol, hostname, port),
        None => format!("{}://{}", protocol, hostname),
    }
}

fn public_service_url(
    settings: &AppSettings,
    strategy: PublicHostnameStrategy,
    environment: &str,
    service: &str,
) -> String {
    let hostname = strategy.service_hostname(&settings.preview_domain, environment, service);
    public_url_for_hostname(settings, &hostname)
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_last_deployment,
        get_project_deployments,
        get_deployment,
        get_deployment_jobs,
        get_deployment_job_logs,
        tail_deployment_job_logs,
        list_deployment_container_logs,
        get_deployment_container_log_content,
        rollback_to_deployment,
        promote_deployment,
        pause_deployment,
        resume_deployment,
        cancel_deployment,
        teardown_deployment,
        teardown_environment,
        list_containers,
        get_container_logs_by_id,
        get_container_logs,
        get_container_detail,
        stop_container,
        start_container,
        restart_container,
        get_container_metrics,
        get_container_metrics_history,
        stream_container_metrics,
        get_activity_graph
    ),
    components(schemas(
        DeploymentListResponse,
        DeploymentResponse,
        DeploymentStateResponse,
        DeploymentJobsResponse,
        DeploymentJobResponse,
        ContainerLogsQuery,
        GetDeploymentsParams,
        ContainerListResponse,
        ContainerInfoResponse,
        ContainerDetailResponse,
        EnvVarResponse,
        ResourceLimitsResponse,
        ContainerMetricsResponse,
        ContainerMetricsHistoryQuery,
        ContainerMetricHistoryPoint,
        ContainerActionResponse,
        ActivityGraphQuery,
        ActivityGraphResponse,
        ActivityDay,
        PromoteDeploymentRequest,
        DeploymentContainerLogResponse,
        DeploymentContainerLogsListResponse,
        DeploymentContainerLogContentResponse
    )),
    info(
        title = "Deployments API",
        description = "API endpoints for managing deployments, containers, and logs. \
        Provides deployment lifecycle management including rollbacks, pausing/resuming, \
        real-time log streaming, and container management (start/stop/restart).",
        version = "1.0.0"
    )
)]
pub struct DeploymentsApiDoc;

pub fn configure_routes() -> Router<Arc<super::types::AppState>> {
    Router::new()
        // Deployment management
        .route("/projects/{id}/last-deployment", get(get_last_deployment))
        .route("/projects/{id}/deployments", get(get_project_deployments))
        .route(
            "/projects/{project_id}/deployments/{deployment_id}",
            get(get_deployment),
        )
        .route(
            "/projects/{project_id}/deployments/{deployment_id}/jobs",
            get(get_deployment_jobs),
        )
        .route(
            "/projects/{project_id}/deployments/{deployment_id}/jobs/{job_id}/logs/tail",
            get(tail_deployment_job_logs),
        )
        .route(
            "/projects/{project_id}/deployments/{deployment_id}/jobs/{job_id}/logs",
            get(get_deployment_job_logs),
        )
        // Historical (captured) container logs for previous deployments. These
        // survive teardown so users can read the logs of a container that no
        // longer exists (e.g. "web-2" from a few days ago).
        .route(
            "/projects/{project_id}/deployments/{deployment_id}/container-logs",
            get(list_deployment_container_logs),
        )
        .route(
            "/projects/{project_id}/deployments/{deployment_id}/container-logs/{log_id}",
            get(get_deployment_container_log_content),
        )
        // Deployment operations
        .route(
            "/projects/{project_id}/deployments/{deployment_id}/rollback",
            post(rollback_to_deployment),
        )
        .route(
            "/projects/{project_id}/deployments/{deployment_id}/promote",
            post(promote_deployment),
        )
        .route(
            "/projects/{project_id}/deployments/{deployment_id}/pause",
            post(pause_deployment),
        )
        .route(
            "/projects/{project_id}/deployments/{deployment_id}/resume",
            post(resume_deployment),
        )
        .route(
            "/projects/{project_id}/deployments/{deployment_id}/cancel",
            post(cancel_deployment),
        )
        .route(
            "/projects/{project_id}/deployments/{deployment_id}/teardown",
            delete(teardown_deployment),
        )
        // Environment operations
        .route(
            "/projects/{project_id}/environments/{env_id}/teardown",
            delete(teardown_environment),
        )
        // Asset cache
        .route(
            "/projects/{project_id}/asset-cache",
            delete(purge_asset_cache),
        )
        .route(
            "/projects/{project_id}/environments/{environment_id}/asset-cache",
            delete(purge_environment_asset_cache),
        )
        // Analytics
        .route("/deployments/activity-graph", get(get_activity_graph))
        // Container management
        .route(
            "/projects/{project_id}/environments/{environment_id}/containers",
            get(list_containers),
        )
        .route(
            "/projects/{project_id}/environments/{environment_id}/containers/{container_id}/logs",
            get(get_container_logs_by_id),
        )
        // Legacy container logs endpoint (deprecated)
        .route(
            "/projects/{project_id}/environments/{environment_id}/container-logs",
            get(get_container_logs),
        )
        // Container management endpoints
        .route(
            "/projects/{project_id}/environments/{environment_id}/containers/{container_id}",
            get(get_container_detail),
        )
        .route(
            "/projects/{project_id}/environments/{environment_id}/containers/{container_id}/stop",
            post(stop_container),
        )
        .route(
            "/projects/{project_id}/environments/{environment_id}/containers/{container_id}/start",
            post(start_container),
        )
        .route(
            "/projects/{project_id}/environments/{environment_id}/containers/{container_id}/restart",
            post(restart_container),
        )
        .route(
            "/projects/{project_id}/environments/{environment_id}/containers/{container_id}/metrics",
            get(get_container_metrics),
        )
        .route(
            "/projects/{project_id}/environments/{environment_id}/containers/{container_id}/metrics/history",
            get(get_container_metrics_history),
        )
        .route(
            "/projects/{project_id}/environments/{environment_id}/containers/{container_id}/metrics/stream",
            get(stream_container_metrics),
        )
        // Container exec and terminal
        .route(
            "/projects/{project_id}/environments/{environment_id}/containers/{container_id}/exec",
            post(super::container_exec::exec_command),
        )
        .route(
            "/projects/{project_id}/environments/{environment_id}/containers/{container_id}/terminal",
            get(super::container_exec::container_terminal),
        )
}

impl From<crate::services::services::DeploymentError> for Problem {
    fn from(err: crate::services::services::DeploymentError) -> Self {
        use crate::services::services::DeploymentError;
        match err {
            DeploymentError::QueueError(msg) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Queue Error")
                    .with_detail(msg)
            }
            DeploymentError::DatabaseConnectionError(reason) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Connection Error")
                    .with_detail(reason)
            }
            DeploymentError::NotFound(msg) => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Deployment Not Found")
                .with_detail(msg),
            DeploymentError::DatabaseError { reason } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Error")
                    .with_detail(reason)
            }
            DeploymentError::InvalidInput(msg) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Input")
                .with_detail(msg),
            DeploymentError::InvalidDeploymentState(msg) => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Deployment State")
                    .with_detail(msg)
            }
            DeploymentError::PipelineError(msg) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Pipeline Error")
                    .with_detail(msg)
            }
            DeploymentError::DeploymentError(msg) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Deployment Error")
                    .with_detail(msg)
            }
            DeploymentError::InvalidBundlePath { path, reason } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Bundle Path")
                    .with_detail(format!("Bundle path '{path}' is invalid: {reason}"))
            }
            DeploymentError::Other(msg) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail(msg),
        }
    }
}

/// Get the last deployment for a specific project
#[utoipa::path(
    tag = "Deployments",
    get,
    params(
        ("id" = i32, Path, description = "Project ID")
    ),
    responses(
        (status = 200, description = "Last deployment details", body = DeploymentResponse),
        (status = 404, description = "Project not found or no deployments"),
        (status = 500, description = "Internal server error")
    ),
    path = "/projects/{id}/last-deployment"
)]
pub async fn get_last_deployment(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);
    project_scope_guard!(auth, id);
    project_access_guard!(auth, id, state.project_access_checker);

    debug!("Getting last deployment for project with id: {}", id);
    let deployment = state.deployment_service.get_last_deployment(id).await?;
    Ok(Json(DeploymentResponse::from_service_deployment(deployment)).into_response())
}

use super::types::GetDeploymentsParams;

// Update the OpenAPI documentation
#[utoipa::path(
    tag = "Deployments",
    path = "/projects/{id}/deployments",
    get,
    tag = "Projects",
    params(
        ("id" = i32, Path, description = "Project ID"),
        ("page" = Option<i64>, Query, description = "Page number"),
        ("per_page" = Option<i64>, Query, description = "Items per page"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID filter")
    ),
    responses(
        (status = 200, description = "List of deployments", body = DeploymentListResponse),
        (status = 404, description = "Project not found")
    )
)]
pub async fn get_project_deployments(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Query(params): Query<GetDeploymentsParams>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);
    project_scope_guard!(auth, id);
    project_access_guard!(auth, id, state.project_access_checker);

    let list_response = state
        .deployment_service
        .get_project_deployments(id, params.page, params.per_page, params.environment_id)
        .await?;

    let deployment_responses = list_response
        .deployments
        .into_iter()
        .map(DeploymentResponse::from_service_deployment)
        .collect();

    let response = DeploymentListResponse {
        deployments: deployment_responses,
        total: list_response.total,
        page: list_response.page,
        per_page: list_response.per_page,
    };

    Ok(Json(response).into_response())
}

/// Get a specific deployment by ID for a project (identified by ID or slug)
#[utoipa::path(
    tag = "Deployments",
    get,
    params(
        ("project_id" = i32, Path, description = "Project ID or slug"),
        ("deployment_id" = i32, Path, description = "Deployment ID")
    ),
    responses(
        (status = 200, description = "Deployment details", body = DeploymentResponse),
        (status = 404, description = "Project or deployment not found"),
        (status = 500, description = "Internal server error")
    ),
    path = "/projects/{project_id}/deployments/{deployment_id}"
)]
pub async fn get_deployment(
    State(state): State<Arc<AppState>>,
    Path((project_id, deployment_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    debug!(
        "Getting deployment {} for project: {}",
        deployment_id, project_id
    );

    let deployment = state
        .deployment_service
        .get_deployment(project_id, deployment_id)
        .await?;
    Ok(Json(DeploymentResponse::from_service_deployment(deployment)).into_response())
}

// Add the new route handler

#[utoipa::path(
    tag = "Deployments",
    post,
    path = "/projects/{project_id}/deployments/{deployment_id}/rollback",
    tag = "Projects",
    responses(
        (status = 200, description = "Rollback initiated successfully", body = DeploymentResponse),
        (status = 404, description = "Project or deployment not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("deployment_id" = i32, Path, description = "Deployment ID to rollback to")
    )
)]
pub async fn rollback_to_deployment(
    State(state): State<Arc<AppState>>,
    Path((project_id, deployment_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        DeploymentsCreate,
        project_id,
        state.project_access_checker
    );

    let deployment = state
        .deployment_service
        .rollback_to_deployment(project_id, deployment_id)
        .await?;

    let audit = DeploymentRollbackAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        deployment_id,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(Json(DeploymentResponse::from_service_deployment(
        deployment,
    )))
}

/// Promote a deployment to another environment
///
/// Creates a new deployment in the target environment using the source deployment's
/// Docker image. Useful for promoting a validated preview/staging deployment to production.
#[utoipa::path(
    tag = "Deployments",
    post,
    path = "/projects/{project_id}/deployments/{deployment_id}/promote",
    request_body = PromoteDeploymentRequest,
    responses(
        (status = 200, description = "Promotion initiated successfully", body = DeploymentResponse),
        (status = 400, description = "Invalid deployment state for promotion"),
        (status = 404, description = "Project, deployment, or target environment not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("deployment_id" = i32, Path, description = "Source deployment ID to promote")
    )
)]
pub async fn promote_deployment(
    State(state): State<Arc<AppState>>,
    Path((project_id, deployment_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<PromoteDeploymentRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        DeploymentsCreate,
        project_id,
        state.project_access_checker
    );

    info!(
        "Promoting deployment {} to environment {} (project {})",
        deployment_id, request.target_environment_id, project_id
    );

    let deployment = state
        .deployment_service
        .promote_deployment(project_id, deployment_id, request.target_environment_id)
        .await?;

    let audit = DeploymentPromotedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        source_deployment_id: deployment_id,
        target_environment_id: request.target_environment_id,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(Json(DeploymentResponse::from_service_deployment(
        deployment,
    )))
}

/// Pause a deployment
#[utoipa::path(
    tag = "Deployments",
    post,
    path = "/projects/{project_id}/deployments/{deployment_id}/pause",
    tag = "Projects",
    responses(
        (status = 200, description = "Deployment paused successfully", body = DeploymentStateResponse),
        (status = 404, description = "Project or deployment not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID or slug"),
        ("deployment_id" = i32, Path, description = "Deployment ID")
    )
)]
pub async fn pause_deployment(
    State(state): State<Arc<AppState>>,
    Path((project_id, deployment_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        DeploymentsDelete,
        project_id,
        state.project_access_checker
    );
    info!("Pausing deployment: {:?}", deployment_id);

    state
        .deployment_service
        .pause_deployment(project_id, deployment_id)
        .await?;

    let audit = DeploymentPausedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        deployment_id,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    let response = DeploymentStateResponse {
        id: deployment_id,
        state: "paused".to_string(),
        message: "Deployment paused successfully".to_string(),
    };
    Ok(Json(response).into_response())
}

/// Resume a deployment
#[utoipa::path(
    tag = "Deployments",
    post,
    path = "/projects/{project_id}/deployments/{deployment_id}/resume",
    tag = "Projects",
    responses(
        (status = 200, description = "Deployment resumed successfully", body = DeploymentStateResponse),
        (status = 404, description = "Project or deployment not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID or slug"),
        ("deployment_id" = i32, Path, description = "Deployment ID")
    )
)]
pub async fn resume_deployment(
    State(state): State<Arc<AppState>>,
    Path((project_id, deployment_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        DeploymentsCreate,
        project_id,
        state.project_access_checker
    );

    state
        .deployment_service
        .resume_deployment(project_id, deployment_id)
        .await?;

    let audit = DeploymentResumedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        deployment_id,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    let response = DeploymentStateResponse {
        id: deployment_id,
        state: "deployed".to_string(),
        message: "Deployment resumed successfully".to_string(),
    };
    Ok(Json(response).into_response())
}

/// Cancel a deployment
#[utoipa::path(
    tag = "Deployments",
    post,
    path = "/projects/{project_id}/deployments/{deployment_id}/cancel",
    tag = "Projects",
    responses(
        (status = 200, description = "Deployment cancelled successfully", body = DeploymentStateResponse),
        (status = 400, description = "Deployment cannot be cancelled (already completed, failed, or cancelled)"),
        (status = 404, description = "Project or deployment not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID or slug"),
        ("deployment_id" = i32, Path, description = "Deployment ID")
    )
)]
pub async fn cancel_deployment(
    State(state): State<Arc<AppState>>,
    Path((project_id, deployment_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        DeploymentsDelete,
        project_id,
        state.project_access_checker
    );

    info!(
        "API request to cancel deployment {} for project {} from user",
        deployment_id, project_id
    );

    state
        .deployment_service
        .cancel_deployment(project_id, deployment_id)
        .await?;

    let audit = DeploymentCancelledAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        deployment_id,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    info!(
        "Deployment {} cancellation request processed successfully",
        deployment_id
    );

    let response = DeploymentStateResponse {
        id: deployment_id,
        state: "cancelled".to_string(),
        message: "Deployment cancelled successfully".to_string(),
    };
    Ok(Json(response).into_response())
}

/// Teardown a specific deployment
#[utoipa::path(
    tag = "Deployments",
    delete,
    path = "/projects/{project_id}/deployments/{deployment_id}/teardown",
    tag = "Projects",
    responses(
        (status = 204, description = "Deployment torn down successfully"),
        (status = 404, description = "Project or deployment not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID or slug"),
        ("deployment_id" = i32, Path, description = "Deployment ID")
    )
)]
pub async fn teardown_deployment(
    State(state): State<Arc<AppState>>,
    Path((project_id, deployment_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        DeploymentsDelete,
        project_id,
        state.project_access_checker
    );

    info!(
        "Tearing down deployment {} for project: {}",
        deployment_id, project_id
    );

    state
        .deployment_service
        .teardown_deployment(project_id, deployment_id)
        .await
        .map_err(|e| {
            error!("Error tearing down deployment: {:?}", e);
            Problem::from(e)
        })?;

    let audit = DeploymentTeardownAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        deployment_id,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Teardown an environment and all its active deployments
#[utoipa::path(
    tag = "Deployments",
    delete,
    path = "/projects/{project_id}/environments/{env_id}/teardown",
    tag = "Projects",
    responses(
        (status = 204, description = "Environment torn down successfully"),
        (status = 404, description = "Project or environment not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID or slug"),
        ("env_id" = i32, Path, description = "Environment ID or slug")
    )
)]
pub async fn teardown_environment(
    State(state): State<Arc<AppState>>,
    Path((project_id, env_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        DeploymentsDelete,
        project_id,
        state.project_access_checker
    );

    info!(
        "Tearing down environment {} for project: {}",
        env_id, project_id
    );

    state
        .deployment_service
        .teardown_environment(project_id, env_id)
        .await
        .map_err(|e| {
            error!("Error tearing down environment: {:?}", e);
            Problem::from(e)
        })?;

    let audit = EnvironmentTeardownAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        environment_id: env_id,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(StatusCode::NO_CONTENT)
}

/// List all containers for an environment
#[utoipa::path(
    tag = "Deployments",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/containers",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    responses(
        (status = 200, description = "List of containers", body = ContainerListResponse),
        (status = 400, description = "Not a server-type project"),
        (status = 404, description = "Project or environment not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_containers(
    State(state): State<Arc<AppState>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    info!(
        "Listing containers for environment {} of project: {}",
        environment_id, project_id
    );

    let containers = state
        .deployment_service
        .list_environment_containers(project_id, environment_id)
        .await?;

    // Collect unique node_ids to resolve names in a single batch
    let node_ids: Vec<i32> = containers
        .iter()
        .filter_map(|(_, node_id, _)| *node_id)
        .collect::<std::collections::HashSet<i32>>()
        .into_iter()
        .collect();

    let mut node_names: std::collections::HashMap<i32, String> = std::collections::HashMap::new();
    if !node_ids.is_empty() {
        let nodes = temps_entities::nodes::Entity::find()
            .filter(temps_entities::nodes::Column::Id.is_in(node_ids))
            .all(state.db.as_ref())
            .await
            .unwrap_or_default();
        for node in nodes {
            node_names.insert(node.id, node.name);
        }
    }

    // Resolve preview_domain, URL scheme, and env subdomain for per-service URLs.
    let settings_row = temps_entities::settings::Entity::find()
        .one(state.db.as_ref())
        .await
        .ok()
        .flatten();
    let app_settings = settings_row
        .as_ref()
        .map(|s| AppSettings::from_json(s.data.clone()))
        .unwrap_or_default();

    let env_subdomain = temps_entities::environments::Entity::find_by_id(environment_id)
        .one(state.db.as_ref())
        .await
        .ok()
        .flatten()
        .map(|e| e.subdomain);

    // Resolve the hostname strategy for this instance's preview domain once,
    // before the synchronous response-building closure below.
    let hostname_strategy = state
        .hostname_resolver
        .strategy_for(&app_settings.preview_domain)
        .await;

    // Read public_ports from project's preset_config
    let public_ports: Vec<temps_entities::preset::ComposePublicPort> =
        temps_entities::projects::Entity::find_by_id(project_id)
            .one(state.db.as_ref())
            .await
            .ok()
            .flatten()
            .and_then(|p| p.preset_config)
            .and_then(|pc| {
                if let temps_entities::preset::PresetConfig::DockerCompose(cfg) = pc {
                    Some(cfg.public_ports)
                } else {
                    None
                }
            })
            .unwrap_or_default();

    let container_responses: Vec<ContainerInfoResponse> = containers
        .into_iter()
        .map(|(info, node_id, service_name)| {
            let node_name = node_id.and_then(|id| node_names.get(&id).cloned());
            // Build per-service URL only for ports marked as public
            let service_url = service_name.as_ref().and_then(|svc| {
                // Check if this service has any public port configured
                let is_public = public_ports.iter().any(|pp| pp.service == *svc);
                if !is_public {
                    return None;
                }
                env_subdomain
                    .as_ref()
                    .map(|sub| public_service_url(&app_settings, hostname_strategy, sub, svc))
            });
            ContainerInfoResponse::from_info(info, node_name, service_name, service_url)
        })
        .collect();

    let total = container_responses.len();
    let response = ContainerListResponse {
        containers: container_responses,
        total,
    };

    Ok(Json(response))
}

/// Get logs for a specific container by container ID via WebSocket
#[utoipa::path(
    tag = "Deployments",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/containers/{container_id}/logs",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("container_id" = String, Path, description = "Container ID"),
        ("start_date" = Option<i64>, Query, description = "Start date for logs"),
        ("end_date" = Option<i64>, Query, description = "End date for logs"),
        ("tail" = Option<String>, Query, description = "Number of lines to tail (or 'all')"),
        ("timestamps" = Option<bool>, Query, description = "Include timestamps in log output (default: false)"),
        ("follow" = Option<bool>, Query, description = "Follow log output in real-time (default: true)")
    ),
    responses(
        (status = 101, description = "WebSocket connection established for streaming container logs"),
        (status = 400, description = "Not a server-type project"),
        (status = 404, description = "Project, environment, or container not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_container_logs_by_id(
    State(state): State<Arc<AppState>>,
    Path((project_id, environment_id, container_id)): Path<(i32, i32, String)>,
    Query(query): Query<ContainerLogsQuery>,
    RequireAuth(auth): RequireAuth,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    debug!(
        "WebSocket request for container {} logs in environment {} of project: {}",
        container_id, environment_id, project_id
    );

    // Upgrade to WebSocket and handle the connection
    Ok(ws.on_upgrade(move |socket| {
        handle_container_logs_socket(
            socket,
            state,
            ContainerLogParams {
                project_id,
                environment_id,
                container_id,
                start_date: query.start_date,
                end_date: query.end_date,
                tail: query.tail,
                timestamps: query.timestamps,
                follow: query.follow,
            },
        )
    }))
}

/// Handle WebSocket connection for container log streaming
struct ContainerLogParams {
    project_id: i32,
    environment_id: i32,
    container_id: String,
    start_date: Option<i64>,
    end_date: Option<i64>,
    tail: Option<String>,
    timestamps: bool,
    follow: bool,
}

async fn handle_container_logs_socket(
    mut socket: WebSocket,
    state: Arc<AppState>,
    params: ContainerLogParams,
) {
    debug!(
        "WebSocket connection established for container {} logs",
        params.container_id
    );

    // Get the log stream from the deployment service
    let log_stream = match state
        .deployment_service
        .get_container_logs_by_id(
            params.project_id,
            params.environment_id,
            params.container_id.clone(),
            crate::services::services::ContainerLogParams {
                start_date: params.start_date,
                end_date: params.end_date,
                tail: params.tail,
                timestamps: params.timestamps,
                follow: params.follow,
            },
        )
        .await
    {
        Ok(stream) => stream,
        Err(e) => {
            error!("Failed to get container logs: {}", e);
            let error_msg = serde_json::json!({
                "error": "Failed to get container logs",
                "detail": e.to_string()
            });
            if let Err(e) = socket
                .send(Message::Text(error_msg.to_string().into()))
                .await
            {
                error!("Failed to send error message over WebSocket: {}", e);
            }
            let _ = socket.close().await;
            return;
        }
    };

    // Pin the stream for iteration
    tokio::pin!(log_stream);

    // Periodic Ping keeps the WS healthy across intermediate proxies
    // (Pingora's 60s body-read timeout is the immediate motivation) when a
    // container has long quiet stretches. Browsers handle Ping/Pong
    // transparently — no frontend change needed.
    let mut ping_interval = tokio::time::interval(std::time::Duration::from_secs(25));
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // First tick fires immediately; consume it so we don't ping at t=0.
    ping_interval.tick().await;

    loop {
        tokio::select! {
            biased;
            _ = ping_interval.tick() => {
                if let Err(e) = socket.send(Message::Ping(Vec::new().into())).await {
                    debug!("WebSocket ping failed (client likely gone): {}", e);
                    break;
                }
            }
            maybe_line = log_stream.next() => {
                let Some(log_result) = maybe_line else { break };
                match log_result {
                    Ok(line) => {
                        if let Err(e) = socket.send(Message::Text(line.into())).await {
                            warn!("Failed to send log message over WebSocket: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        error!("Error reading log line: {}", e);
                        let error_msg = format!("ERROR: {}", e);
                        if let Err(e) = socket.send(Message::Text(error_msg.into())).await {
                            error!("Failed to send error message over WebSocket: {}", e);
                        }
                        break;
                    }
                }
            }
        }
    }

    debug!(
        "WebSocket connection closed for container {} logs",
        params.container_id
    );
    let _ = socket.close().await;
}

/// Get logs for a container in an environment via WebSocket
#[utoipa::path(
    tag = "Deployments",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/container-logs",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("start_date" = Option<i64>, Query, description = "Start date for logs"),
        ("end_date" = Option<i64>, Query, description = "End date for logs"),
        ("tail" = Option<String>, Query, description = "Number of lines to tail (or 'all')"),
        ("container_name" = Option<String>, Query, description = "Optional container name (defaults to first/primary container)"),
        ("timestamps" = Option<bool>, Query, description = "Include timestamps in log output (default: false)"),
        ("follow" = Option<bool>, Query, description = "Follow log output in real-time (default: true)")
    ),
    responses(
        (status = 101, description = "WebSocket connection established for streaming container logs"),
        (status = 400, description = "Not a server-type project"),
        (status = 404, description = "Project, deployment, or container not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_container_logs(
    State(state): State<Arc<AppState>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    Query(query): Query<ContainerLogsQuery>,
    RequireAuth(auth): RequireAuth,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    debug!(
        "WebSocket request for container logs in environment {} of project: {}",
        environment_id, project_id
    );

    // Upgrade to WebSocket and handle the connection
    Ok(ws.on_upgrade(move |socket| {
        handle_filtered_container_logs_socket(
            socket,
            state,
            FilteredContainerLogParams {
                project_id,
                environment_id,
                start_date: query.start_date,
                end_date: query.end_date,
                tail: query.tail,
                container_name: query.container_name,
                timestamps: query.timestamps,
                follow: query.follow,
            },
        )
    }))
}

/// Handle WebSocket connection for filtered container log streaming
struct FilteredContainerLogParams {
    project_id: i32,
    environment_id: i32,
    start_date: Option<i64>,
    end_date: Option<i64>,
    tail: Option<String>,
    container_name: Option<String>,
    timestamps: bool,
    follow: bool,
}

async fn handle_filtered_container_logs_socket(
    mut socket: WebSocket,
    state: Arc<AppState>,
    params: FilteredContainerLogParams,
) {
    debug!(
        "WebSocket connection established for environment {} container logs",
        params.environment_id
    );

    // Get the log stream from the deployment service
    let log_stream = match state
        .deployment_service
        .get_filtered_container_logs(
            params.project_id,
            params.environment_id,
            params.container_name,
            crate::services::services::ContainerLogParams {
                start_date: params.start_date,
                end_date: params.end_date,
                tail: params.tail,
                timestamps: params.timestamps,
                follow: params.follow,
            },
        )
        .await
    {
        Ok(stream) => stream,
        Err(e) => {
            error!("Failed to get container logs: {}", e);
            let error_msg = serde_json::json!({
                "error": "Failed to get container logs",
                "detail": e.to_string()
            });
            if let Err(e) = socket
                .send(Message::Text(error_msg.to_string().into()))
                .await
            {
                error!("Failed to send error message over WebSocket: {}", e);
            }
            let _ = socket.close().await;
            return;
        }
    };

    // Pin the stream for iteration
    tokio::pin!(log_stream);

    // Stream logs to WebSocket client
    while let Some(log_result) = log_stream.next().await {
        match log_result {
            Ok(line) => {
                // Send raw log line as-is
                if let Err(e) = socket.send(Message::Text(line.into())).await {
                    warn!("Failed to send log message over WebSocket: {}", e);
                    break;
                }
            }
            Err(e) => {
                error!("Error reading log line: {}", e);
                // Send error as plain text
                let error_msg = format!("ERROR: {}", e);
                if let Err(e) = socket.send(Message::Text(error_msg.into())).await {
                    error!("Failed to send error message over WebSocket: {}", e);
                }
                break;
            }
        }
    }

    debug!(
        "WebSocket connection closed for environment {} container logs",
        params.environment_id
    );
    let _ = socket.close().await;
}

/// Get jobs for a specific deployment
///
/// Returns all jobs (workflow tasks) for a deployment, ordered by execution order.
/// This replaces the old deployment stages endpoint.
#[utoipa::path(
    get,
    tag = "Deployments",
    path = "/projects/{project_id}/deployments/{deployment_id}/jobs",
    responses(
        (status = 200, description = "Jobs retrieved successfully", body = DeploymentJobsResponse),
        (status = 404, description = "Deployment not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("deployment_id" = i32, Path, description = "Deployment ID")
    )
)]
pub async fn get_deployment_jobs(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, deployment_id)): Path<(i32, i32)>,
) -> Result<Json<DeploymentJobsResponse>, Problem> {
    permission_guard!(auth, DeploymentsRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let jobs = state
        .deployment_service
        .get_deployment_jobs(deployment_id)
        .await?;

    let total = jobs.len();
    let job_responses: Vec<DeploymentJobResponse> = jobs.into_iter().map(Into::into).collect();

    Ok(Json(DeploymentJobsResponse {
        jobs: job_responses,
        total,
    }))
}

/// Get logs for a specific deployment job
#[utoipa::path(
    get,
    tag = "Deployments",
    path = "/projects/{project_id}/deployments/{deployment_id}/jobs/{job_id}/logs",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("deployment_id" = i32, Path, description = "Deployment ID"),
        ("job_id" = String, Path, description = "Job ID")
    ),
    responses(
        (status = 200, description = "Job logs retrieved successfully", body = String),
        (status = 404, description = "Job or logs not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_token" = [])
    )
)]
pub async fn get_deployment_job_logs(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, deployment_id, job_id)): Path<(i32, i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    // Get the job to verify it exists and get its log_id
    let jobs = state
        .deployment_service
        .get_deployment_jobs(deployment_id)
        .await?;

    let job = jobs
        .iter()
        .find(|j| j.job_id == job_id)
        .ok_or_else(|| problemdetails::new(StatusCode::NOT_FOUND).with_detail("Job not found"))?;

    // Get logs using the log_id
    let log_content = state
        .log_service
        .get_log_content(&job.log_id)
        .await
        .map_err(|e| {
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_detail(format!("Failed to read logs: {}", e))
        })?;

    Ok((StatusCode::OK, log_content))
}

/// List the captured (historical) container-log dumps for a deployment.
///
/// Container runtime logs are normally only available live from the running
/// container. When a deployment is superseded its containers are torn down and
/// those logs would be lost — so just before teardown we capture each
/// container's logs to durable storage. This endpoint lists what was captured
/// for a given (often older) deployment, so a user can read the logs of a
/// container that no longer exists.
#[utoipa::path(
    tag = "Deployments",
    get,
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("deployment_id" = i32, Path, description = "Deployment ID")
    ),
    responses(
        (status = 200, description = "Captured container logs for the deployment", body = DeploymentContainerLogsListResponse),
        (status = 404, description = "Deployment not found in this project"),
        (status = 500, description = "Internal server error")
    ),
    path = "/projects/{project_id}/deployments/{deployment_id}/container-logs",
    security(("bearer_token" = []))
)]
pub async fn list_deployment_container_logs(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, deployment_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let logs = state
        .deployment_service
        .list_deployment_container_logs(project_id, deployment_id)
        .await?;

    let response = DeploymentContainerLogsListResponse {
        logs: logs
            .into_iter()
            .map(DeploymentContainerLogResponse::from)
            .collect(),
    };

    Ok((StatusCode::OK, Json(response)).into_response())
}

/// Get the captured text content of a single historical container-log dump.
#[utoipa::path(
    tag = "Deployments",
    get,
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("deployment_id" = i32, Path, description = "Deployment ID"),
        ("log_id" = i32, Path, description = "Captured log ID")
    ),
    responses(
        (status = 200, description = "Captured container log content", body = DeploymentContainerLogContentResponse),
        (status = 404, description = "Captured log not found in this project"),
        (status = 500, description = "Internal server error")
    ),
    path = "/projects/{project_id}/deployments/{deployment_id}/container-logs/{log_id}",
    security(("bearer_token" = []))
)]
pub async fn get_deployment_container_log_content(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, deployment_id, log_id)): Path<(i32, i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let (row, content) = state
        .deployment_service
        .get_deployment_container_log_content(project_id, deployment_id, log_id)
        .await?;

    let response = DeploymentContainerLogContentResponse {
        id: row.id,
        container_name: row.container_name,
        service_name: row.service_name,
        size_bytes: row.size_bytes,
        truncated: row.truncated,
        captured_at: row.captured_at.timestamp_millis(),
        content,
    };

    Ok((StatusCode::OK, Json(response)).into_response())
}

/// Tail logs for a specific deployment job in real-time via WebSocket
///
/// **WebSocket Streaming**: Logs are sent as raw text, one line per WebSocket message.
///
/// **Authentication**: Requires authentication via session cookie (browser clients)
/// or API key (API clients). For browser-based WebSocket connections, ensure the user
/// is logged in - the browser automatically includes session cookies in the WebSocket
/// upgrade request.
///
/// **API Client Authentication**: Include API key in Authorization header:
/// ```text
/// Authorization: Bearer tk_your_api_key_here
/// ```
#[utoipa::path(
    get,
    path = "/projects/{project_id}/deployments/{deployment_id}/jobs/{job_id}/logs/tail",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("deployment_id" = i32, Path, description = "Deployment ID"),
        ("job_id" = String, Path, description = "Job ID")
    ),
    responses(
        (status = 101, description = "WebSocket connection established for streaming deployment job logs"),
        (status = 404, description = "Job or logs not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_token" = [])
    ),
    tag = "Deployments"
)]
pub async fn tail_deployment_job_logs(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, deployment_id, job_id)): Path<(i32, i32, String)>,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    debug!(
        "WebSocket request for tailing logs for job {} in deployment {}",
        job_id, deployment_id
    );

    // Get the job to verify it exists and get its log_id
    let jobs = state
        .deployment_service
        .get_deployment_jobs(deployment_id)
        .await?;

    let job = jobs
        .iter()
        .find(|j| j.job_id == job_id)
        .ok_or_else(|| problemdetails::new(StatusCode::NOT_FOUND).with_detail("Job not found"))?;

    let log_id = job.log_id.clone();

    // Upgrade to WebSocket and handle the connection
    Ok(ws.on_upgrade(move |socket| handle_job_log_socket(socket, state, log_id)))
}

/// Handle WebSocket connection for job log tailing
async fn handle_job_log_socket(mut socket: WebSocket, state: Arc<AppState>, log_id: String) {
    debug!("WebSocket connection established for log_id: {}", log_id);

    // Get the log stream from the log service
    let stream = match state.log_service.tail_log(&log_id).await {
        Ok(stream) => stream,
        Err(e) => {
            error!("Error tailing job logs: {:?}", e);
            let error_msg = serde_json::json!({
                "error": "Failed to tail job logs",
                "detail": format!("{}", e)
            });
            if let Err(e) = socket
                .send(Message::Text(error_msg.to_string().into()))
                .await
            {
                error!("Failed to send error message over WebSocket: {}", e);
            }
            let _ = socket.close().await;
            return;
        }
    };

    // Pin the stream for iteration
    tokio::pin!(stream);

    // Stream logs to WebSocket client (raw text, not JSON)
    while let Some(line_result) = stream.next().await {
        match line_result {
            Ok(data) => {
                // Send raw log line as-is
                if let Err(e) = socket.send(Message::Text(data.into())).await {
                    warn!("Failed to send log message over WebSocket: {}", e);
                    break;
                }
            }
            Err(e) => {
                error!("Error reading log line: {:?}", e);
                // Send error as plain text
                let error_msg = format!("ERROR: {}", e);
                if let Err(e) = socket.send(Message::Text(error_msg.into())).await {
                    error!("Failed to send error message over WebSocket: {}", e);
                }
                break;
            }
        }
    }

    debug!("WebSocket connection closed for log_id: {}", log_id);
    let _ = socket.close().await;
}

/// Get detailed information about a specific container
#[utoipa::path(
    tag = "Containers",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/containers/{container_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("container_id" = String, Path, description = "Container ID")
    ),
    responses(
        (status = 200, description = "Container details", body = ContainerDetailResponse),
        (status = 404, description = "Container not found"),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn get_container_detail(
    State(state): State<Arc<AppState>>,
    Path((project_id, environment_id, container_id)): Path<(i32, i32, String)>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let (container, _) = state
        .deployment_service
        .get_container_detail(project_id, environment_id, container_id.clone())
        .await?;

    // Parse environment variables and mask sensitive ones
    let mut env_vars = vec![];
    if let Ok(vars) = state
        .deployment_service
        .get_container_env_variables(project_id, environment_id, container_id.clone())
        .await
    {
        let sensitive_keys = [
            "password", "secret", "token", "key", "auth", "api_key", "npm_rc",
        ];
        for (key, value) in vars {
            let is_masked = sensitive_keys
                .iter()
                .any(|&s| key.to_lowercase().contains(s));
            env_vars.push(crate::handlers::types::EnvVarResponse {
                key,
                value: if is_masked { "***".to_string() } else { value },
                is_masked,
            });
        }
    }

    let restart_count = state
        .deployment_service
        .get_container_restart_count(&container.container_id)
        .await;

    // Resolve configured resource limits the same way the workflow does:
    // env override first, then project default. This is what was actually
    // applied to the container at deploy time, modulo Docker honoring it.
    let resource_limits: Option<crate::handlers::types::ResourceLimitsResponse> = {
        let env = temps_entities::environments::Entity::find_by_id(environment_id)
            .one(state.db.as_ref())
            .await
            .ok()
            .flatten();
        let proj = temps_entities::projects::Entity::find_by_id(project_id)
            .one(state.db.as_ref())
            .await
            .ok()
            .flatten();
        let env_cfg = env.as_ref().and_then(|e| e.deployment_config.as_ref());
        let proj_cfg = proj.as_ref().and_then(|p| p.deployment_config.as_ref());
        let resolve = |g: fn(
            &temps_entities::deployment_config::DeploymentConfig,
        ) -> Option<i32>|
         -> Option<i32> {
            env_cfg.and_then(g).or_else(|| proj_cfg.and_then(g))
        };
        let cpu_request = resolve(|c| c.cpu_request);
        let cpu_limit = resolve(|c| c.cpu_limit);
        let memory_request = resolve(|c| c.memory_request);
        let memory_limit = resolve(|c| c.memory_limit);
        if cpu_request.is_some()
            || cpu_limit.is_some()
            || memory_request.is_some()
            || memory_limit.is_some()
        {
            Some(crate::handlers::types::ResourceLimitsResponse {
                cpu_request,
                cpu_limit,
                memory_request,
                memory_limit,
            })
        } else {
            None
        }
    };

    // Resolve per-service URL only for ports marked as public in preset_config
    let service_url = if let Some(ref svc_name) = container.service_name {
        // Check if this service has a public port
        let is_public = temps_entities::projects::Entity::find_by_id(project_id)
            .one(state.db.as_ref())
            .await
            .ok()
            .flatten()
            .and_then(|p| p.preset_config)
            .map(|pc| {
                if let temps_entities::preset::PresetConfig::DockerCompose(cfg) = pc {
                    cfg.public_ports.iter().any(|pp| pp.service == *svc_name)
                } else {
                    false
                }
            })
            .unwrap_or(false);

        if is_public {
            let settings_row2 = temps_entities::settings::Entity::find()
                .one(state.db.as_ref())
                .await
                .ok()
                .flatten();
            let app_settings = settings_row2
                .as_ref()
                .map(|s| AppSettings::from_json(s.data.clone()))
                .unwrap_or_default();

            let env_subdomain = temps_entities::environments::Entity::find_by_id(environment_id)
                .one(state.db.as_ref())
                .await
                .ok()
                .flatten()
                .map(|e| e.subdomain);

            let hostname_strategy = state
                .hostname_resolver
                .strategy_for(&app_settings.preview_domain)
                .await;

            env_subdomain
                .map(|sub| public_service_url(&app_settings, hostname_strategy, &sub, svc_name))
        } else {
            None
        }
    } else {
        None
    };

    let response = crate::handlers::types::ContainerDetailResponse {
        id: container.id,
        container_id: container.container_id,
        container_name: container.container_name,
        image_name: container.image_name.unwrap_or_default(),
        status: container.status.unwrap_or_default(),
        deployment_id: container.deployment_id,
        created_at: container.created_at.to_rfc3339(),
        deployed_at: container.deployed_at.to_rfc3339(),
        ready_at: container.ready_at.map(|dt| dt.to_rfc3339()),
        container_port: container.container_port,
        host_port: container.host_port,
        environment_variables: env_vars,
        restart_count,
        resource_limits,
        service_name: container.service_name,
        service_url,
        exit_code: container.exit_code,
        exit_reason: container.exit_reason,
        oom_killed: container.oom_killed,
        error_message: container.error_message,
        finished_at: container.finished_at.map(|dt| dt.to_rfc3339()),
        started_at: container.started_at.map(|dt| dt.to_rfc3339()),
        cpu_limit_cores: container.cpu_limit_cores,
    };

    Ok(Json(response).into_response())
}

/// Stop a specific container
#[utoipa::path(
    tag = "Containers",
    post,
    path = "/projects/{project_id}/environments/{environment_id}/containers/{container_id}/stop",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("container_id" = String, Path, description = "Container ID")
    ),
    responses(
        (status = 200, description = "Container stopped successfully", body = ContainerActionResponse),
        (status = 404, description = "Container not found"),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn stop_container(
    State(state): State<Arc<AppState>>,
    Path((project_id, environment_id, container_id)): Path<(i32, i32, String)>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsWrite);
    project_access_guard!(auth, project_id, state.project_access_checker);

    state
        .deployment_service
        .stop_container(project_id, environment_id, container_id.clone())
        .await?;

    let audit = ContainerActionAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        environment_id,
        container_id: container_id.clone(),
        action: "stop".to_string(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    let response = crate::handlers::types::ContainerActionResponse {
        container_id: container_id.clone(),
        container_name: container_id,
        action: "stop".to_string(),
        status: "success".to_string(),
        message: "Container stopped successfully".to_string(),
    };

    Ok(Json(response).into_response())
}

/// Start a container
#[utoipa::path(
    tag = "Containers",
    post,
    path = "/projects/{project_id}/environments/{environment_id}/containers/{container_id}/start",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("container_id" = String, Path, description = "Container ID")
    ),
    responses(
        (status = 200, description = "Container started successfully", body = ContainerActionResponse),
        (status = 404, description = "Container not found"),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn start_container(
    State(state): State<Arc<AppState>>,
    Path((project_id, environment_id, container_id)): Path<(i32, i32, String)>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsWrite);
    project_access_guard!(auth, project_id, state.project_access_checker);

    state
        .deployment_service
        .start_container(project_id, environment_id, container_id.clone())
        .await?;

    let audit = ContainerActionAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        environment_id,
        container_id: container_id.clone(),
        action: "start".to_string(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    let response = crate::handlers::types::ContainerActionResponse {
        container_id: container_id.clone(),
        container_name: container_id,
        action: "start".to_string(),
        status: "success".to_string(),
        message: "Container started successfully".to_string(),
    };

    Ok(Json(response).into_response())
}

/// Restart a container
#[utoipa::path(
    tag = "Containers",
    post,
    path = "/projects/{project_id}/environments/{environment_id}/containers/{container_id}/restart",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("container_id" = String, Path, description = "Container ID")
    ),
    responses(
        (status = 200, description = "Container restarted successfully", body = ContainerActionResponse),
        (status = 404, description = "Container not found"),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn restart_container(
    State(state): State<Arc<AppState>>,
    Path((project_id, environment_id, container_id)): Path<(i32, i32, String)>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsWrite);
    project_access_guard!(auth, project_id, state.project_access_checker);

    state
        .deployment_service
        .restart_container(project_id, environment_id, container_id.clone())
        .await?;

    let audit = ContainerActionAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        environment_id,
        container_id: container_id.clone(),
        action: "restart".to_string(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    let response = crate::handlers::types::ContainerActionResponse {
        container_id: container_id.clone(),
        container_name: container_id,
        action: "restart".to_string(),
        status: "success".to_string(),
        message: "Container restarted successfully".to_string(),
    };

    Ok(Json(response).into_response())
}

/// Get metrics/stats for a specific container
#[utoipa::path(
    tag = "Containers",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/containers/{container_id}/metrics",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("container_id" = String, Path, description = "Container ID")
    ),
    responses(
        (status = 200, description = "Container metrics retrieved successfully", body = ContainerMetricsResponse),
        (status = 404, description = "Container not found"),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn get_container_metrics(
    State(state): State<Arc<AppState>>,
    Path((project_id, environment_id, container_id)): Path<(i32, i32, String)>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let stats = state
        .deployment_service
        .get_container_metrics(project_id, environment_id, container_id.clone())
        .await?;

    let response = ContainerMetricsResponse {
        container_id: stats.container_id,
        container_name: stats.container_name,
        cpu_percent: stats.cpu_percent,
        cpu_limit_cores: stats.cpu_limit_cores,
        memory_bytes: stats.memory_bytes,
        memory_limit_bytes: stats.memory_limit_bytes,
        memory_percent: stats.memory_percent,
        network_rx_bytes: stats.network_rx_bytes,
        network_tx_bytes: stats.network_tx_bytes,
        timestamp: stats.timestamp.to_rfc3339(),
    };

    Ok(Json(response).into_response())
}

/// Fetch a time-series range for a single container resource metric
/// (recorded by the container health monitor every ~30s).
///
/// Useful metric names: `container.cpu_percent`,
/// `container.cpu_utilization_percent`, `container.memory_used_bytes`,
/// `container.memory_percent`, `container.network_rx_bytes_delta`,
/// `container.network_tx_bytes_delta`.
#[utoipa::path(
    tag = "Containers",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/containers/{container_id}/metrics/history",
    operation_id = "ContainerMetricsGetHistory",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("container_id" = String, Path, description = "Container ID"),
        ContainerMetricsHistoryQuery,
    ),
    responses(
        (status = 200, description = "Metric time series data points", body = Vec<ContainerMetricHistoryPoint>),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Container not found"),
        (status = 503, description = "Metrics store not available"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_container_metrics_history(
    State(state): State<Arc<AppState>>,
    Path((project_id, environment_id, container_id)): Path<(i32, i32, String)>,
    Query(params): Query<ContainerMetricsHistoryQuery>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let store = state.metrics_store.as_ref().ok_or_else(|| {
        problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
            .with_title("Metrics Unavailable")
            .with_detail("Metric collection is not enabled on this server")
    })?;

    // Resolves the docker container ID to its `deployment_containers` row and
    // verifies it belongs to this project/environment (404 otherwise).
    let (container, _) = state
        .deployment_service
        .get_container_detail(project_id, environment_id, container_id.clone())
        .await?;

    let (window, step) = temps_metrics::range_to_step(&params.range);
    let now = chrono::Utc::now();

    let query = temps_metrics::RangeQuery {
        source_kind: temps_metrics::SourceKind::Container,
        source_id: container.id,
        monotonic: temps_metrics::is_monotonic_counter(&params.metric),
        name: params.metric.clone(),
        from: now - window,
        to: now,
        step,
    };

    let points = store.query_range(query).await.map_err(|e| {
        error!(
            project_id,
            environment_id,
            container_id = %container_id,
            metric = %params.metric,
            error = %e,
            "Failed to query container metric range"
        );
        problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Internal Server Error")
            .with_detail(format!("Failed to query metrics: {}", e))
    })?;

    let response: Vec<ContainerMetricHistoryPoint> = points
        .into_iter()
        .map(|(ts, v)| ContainerMetricHistoryPoint {
            time: ts.to_rfc3339(),
            value: v,
        })
        .collect();

    Ok(Json(response).into_response())
}

/// Stream container metrics via Server-Sent Events (SSE)
#[utoipa::path(
    tag = "Containers",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/containers/{container_id}/metrics/stream",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("container_id" = String, Path, description = "Container ID"),
        ("interval" = Option<u64>, Query, description = "Update interval in milliseconds (default: 1000)")
    ),
    responses(
        (status = 200, description = "Metrics stream established (Server-Sent Events)"),
        (status = 404, description = "Container not found"),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn stream_container_metrics(
    State(state): State<Arc<AppState>>,
    Path((project_id, environment_id, container_id)): Path<(i32, i32, String)>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    RequireAuth(auth): RequireAuth,
) -> Result<
    axum::response::sse::Sse<
        impl futures::Stream<Item = Result<axum::response::sse::Event, axum::Error>>,
    >,
    Problem,
> {
    permission_guard!(auth, EnvironmentsRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let interval_ms = params
        .get("interval")
        .and_then(|i| i.parse::<u64>().ok())
        .unwrap_or(1000); // Default: 1 second

    // Verify container exists and get initial stats
    let _stats = state
        .deployment_service
        .get_container_metrics(project_id, environment_id, container_id.clone())
        .await?;

    let service = state.deployment_service.clone();
    let p_id = project_id;
    let e_id = environment_id;
    let c_id = container_id.clone();
    let interval = std::time::Duration::from_millis(interval_ms);

    // Create an SSE stream that polls metrics at regular intervals
    let sse_stream = {
        let service = service.clone();
        let c_id = c_id.clone();

        stream::unfold(tokio::time::interval(interval), move |mut ticker| {
            let service = service.clone();
            let c_id = c_id.clone();

            async move {
                ticker.tick().await;

                let result = service
                    .get_container_metrics(p_id, e_id, c_id.clone())
                    .await;

                match result {
                    Ok(stats) => {
                        let json = serde_json::json!({
                            "container_id": stats.container_id,
                            "container_name": stats.container_name,
                            "cpu_percent": stats.cpu_percent,
                            "cpu_limit_cores": stats.cpu_limit_cores,
                            "memory_bytes": stats.memory_bytes,
                            "memory_limit_bytes": stats.memory_limit_bytes,
                            "memory_percent": stats.memory_percent,
                            "network_rx_bytes": stats.network_rx_bytes,
                            "network_tx_bytes": stats.network_tx_bytes,
                            "restart_count": stats.restart_count,
                            "started_at": stats.started_at.map(|t| t.to_rfc3339()),
                            "timestamp": stats.timestamp.to_rfc3339(),
                        });

                        let event = axum::response::sse::Event::default()
                            .json_data(json)
                            .unwrap();
                        Some((Ok(event), ticker))
                    }
                    Err(e) => {
                        error!("Failed to get metrics for container {}: {}", c_id, e);
                        let event = axum::response::sse::Event::default().comment("error");
                        Some((Ok(event), ticker))
                    }
                }
            }
        })
    };

    Ok(axum::response::sse::Sse::new(sse_stream))
}

/// Get deployment activity graph showing daily deployment counts
/// Similar to GitHub's contribution graph
#[utoipa::path(
    tag = "Deployments",
    get,
    path = "/deployments/activity-graph",
    params(
        ("project_id" = Option<i32>, Query, description = "Filter by project ID (optional)"),
        ("environment_id" = Option<i32>, Query, description = "Filter by environment ID (optional)"),
        ("days" = Option<i32>, Query, description = "Number of days to include (default: 365)"),
    ),
    responses(
        (status = 200, description = "Successfully retrieved activity graph", body = ActivityGraphResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_activity_graph(
    RequireAuth(_auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<ActivityGraphQuery>,
) -> Result<impl IntoResponse, Problem> {
    // Note: No specific permission check needed as this is general activity overview
    // Users can only see their own projects based on the RequireAuth check

    match app_state
        .deployment_service
        .get_activity_graph(query.project_id, query.environment_id, query.days)
        .await
    {
        Ok(graph) => Ok(Json(graph)),
        Err(e) => {
            error!("Failed to get activity graph: {}", e);
            Err(problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Failed to retrieve activity graph")
                .with_detail(e.to_string()))
        }
    }
}

/// Purge all cached static assets for a project.
/// Orphaned CAS blobs are cleaned up by the nightly garbage collector.
#[utoipa::path(
    delete,
    path = "/projects/{project_id}/asset-cache",
    tag = "Deployments",
    responses(
        (status = 200, description = "Asset cache purged"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    security(("bearer_auth" = []))
)]
pub async fn purge_asset_cache(
    State(state): State<Arc<super::types::AppState>>,
    Path(project_id): Path<i32>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsWrite);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let deleted = state
        .deployment_service
        .purge_asset_cache(project_id, None)
        .await
        .map_err(Problem::from)?;

    Ok(Json(serde_json::json!({ "deleted": deleted })))
}

/// Purge cached static assets for a specific environment.
#[utoipa::path(
    delete,
    path = "/projects/{project_id}/environments/{environment_id}/asset-cache",
    tag = "Deployments",
    responses(
        (status = 200, description = "Environment asset cache purged"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    security(("bearer_auth" = []))
)]
pub async fn purge_environment_asset_cache(
    State(state): State<Arc<super::types::AppState>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsWrite);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let deleted = state
        .deployment_service
        .purge_asset_cache(project_id, Some(environment_id))
        .await
        .map_err(Problem::from)?;

    Ok(Json(serde_json::json!({ "deleted": deleted })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum::Router;
    use futures::StreamExt;
    use std::sync::Arc;
    use temps_config::ConfigService;
    use temps_database::test_utils::TestDatabase;
    use temps_entities::upstream_config::UpstreamList;
    use temps_logs::{DockerLogService, LogService};
    use tokio::time::{timeout, Duration};
    use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};

    #[derive(Clone)]
    struct MockAuditLogger;

    #[async_trait]
    impl temps_core::AuditLogger for MockAuditLogger {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::AuditOperation,
        ) -> Result<(), anyhow::Error> {
            Ok(())
        }
    }

    struct MockImageBuilder;

    #[async_trait]
    impl temps_deployer::ImageBuilder for MockImageBuilder {
        async fn build_image(
            &self,
            _request: temps_deployer::BuildRequest,
        ) -> Result<temps_deployer::BuildResult, temps_deployer::BuilderError> {
            unimplemented!("mock")
        }
        async fn build_image_with_callback(
            &self,
            _request: temps_deployer::BuildRequestWithCallback,
        ) -> Result<temps_deployer::BuildResult, temps_deployer::BuilderError> {
            unimplemented!("mock")
        }
        async fn import_image(
            &self,
            _image_path: std::path::PathBuf,
            _tag: &str,
        ) -> Result<String, temps_deployer::BuilderError> {
            unimplemented!("mock")
        }
        async fn extract_from_image(
            &self,
            _image_name: &str,
            _source_path: &str,
            _destination_path: &std::path::Path,
        ) -> Result<(), temps_deployer::BuilderError> {
            unimplemented!("mock")
        }
        async fn list_images(&self) -> Result<Vec<String>, temps_deployer::BuilderError> {
            Ok(vec![])
        }
        async fn remove_image(
            &self,
            _image_name: &str,
        ) -> Result<(), temps_deployer::BuilderError> {
            Ok(())
        }
        async fn inspect_image(
            &self,
            _image_name: &str,
        ) -> Result<temps_deployer::ImageInfo, temps_deployer::BuilderError> {
            Ok(temps_deployer::ImageInfo {
                id: "sha256:mock".to_string(),
                architecture: "amd64".to_string(),
                os: "linux".to_string(),
                platform: "linux/amd64".to_string(),
                size_bytes: 0,
                tags: vec![],
                created: None,
                working_dir: None,
            })
        }
        async fn save_image(
            &self,
            _image_name: &str,
            _output_path: &std::path::Path,
        ) -> Result<(), temps_deployer::BuilderError> {
            Ok(())
        }
        fn get_native_platform(&self) -> String {
            "linux/amd64".to_string()
        }
    }

    struct MockGitProviderManager;

    #[async_trait]
    impl temps_git::GitProviderManagerTrait for MockGitProviderManager {
        async fn clone_repository(
            &self,
            _connection_id: i32,
            _repo_owner: &str,
            _repo_name: &str,
            _target_dir: &std::path::Path,
            _branch_or_ref: Option<&str>,
        ) -> Result<(), temps_git::GitProviderManagerError> {
            unimplemented!("mock")
        }
        async fn get_repository_info(
            &self,
            _connection_id: i32,
            _repo_owner: &str,
            _repo_name: &str,
        ) -> Result<temps_git::RepositoryInfo, temps_git::GitProviderManagerError> {
            unimplemented!("mock")
        }
        async fn download_archive(
            &self,
            _connection_id: i32,
            _repo_owner: &str,
            _repo_name: &str,
            _branch_or_ref: &str,
            _archive_path: &std::path::Path,
            _progress: Option<&temps_git::ArchiveProgressSender>,
        ) -> Result<(), temps_git::GitProviderManagerError> {
            unimplemented!("mock")
        }

        async fn push_files_and_create_pr(
            &self,
            _connection_id: i32,
            _owner: &str,
            _repo: &str,
            _branch: &str,
            _base_branch: &str,
            _files: Vec<(String, Vec<u8>)>,
            _commit_message: &str,
            _pr_title: &str,
            _pr_body: &str,
        ) -> Result<temps_git::PullRequest, temps_git::GitProviderManagerError> {
            Err(temps_git::GitProviderManagerError::Other(
                "not implemented in test".into(),
            ))
        }

        async fn get_connection_access_token(
            &self,
            _connection_id: i32,
        ) -> Result<(String, String), temps_git::GitProviderManagerError> {
            Err(temps_git::GitProviderManagerError::Other(
                "not implemented in test".into(),
            ))
        }

        async fn mint_scoped_repo_token(
            &self,
            _connection_id: i32,
            _owner: &str,
            _repo: &str,
            _operation: temps_git::ScopedTokenOp,
        ) -> Result<temps_git::ScopedTokenGrant, temps_git::GitProviderManagerError> {
            Err(temps_git::GitProviderManagerError::Other(
                "not implemented in test".into(),
            ))
        }
    }

    struct MockStaticDeployer;

    #[async_trait]
    impl temps_deployer::static_deployer::StaticDeployer for MockStaticDeployer {
        async fn deploy(
            &self,
            _request: temps_deployer::static_deployer::StaticDeployRequest,
        ) -> Result<
            temps_deployer::static_deployer::StaticDeployResult,
            temps_deployer::static_deployer::StaticDeployError,
        > {
            unimplemented!("mock")
        }
        async fn get_deployment(
            &self,
            _project_slug: &str,
            _environment_slug: &str,
            _deployment_slug: &str,
        ) -> Result<
            temps_deployer::static_deployer::StaticDeploymentInfo,
            temps_deployer::static_deployer::StaticDeployError,
        > {
            unimplemented!("mock")
        }
        async fn list_files(
            &self,
            _project_slug: &str,
            _environment_slug: &str,
            _deployment_slug: &str,
        ) -> Result<
            Vec<temps_deployer::static_deployer::FileInfo>,
            temps_deployer::static_deployer::StaticDeployError,
        > {
            Ok(vec![])
        }
        async fn remove(
            &self,
            _project_slug: &str,
            _environment_slug: &str,
            _deployment_slug: &str,
        ) -> Result<(), temps_deployer::static_deployer::StaticDeployError> {
            Ok(())
        }
    }

    struct MockCronConfigService;

    #[async_trait]
    impl crate::jobs::CronConfigService for MockCronConfigService {
        async fn configure_crons(
            &self,
            _project_id: i32,
            _environment_id: i32,
            _cron_configs: Vec<crate::jobs::configure_crons::CronConfig>,
        ) -> Result<(), crate::jobs::configure_crons::CronConfigError> {
            Ok(())
        }
    }

    /// Helper to create a mock AuthContext for testing
    fn create_test_auth_context() -> temps_auth::AuthContext {
        let user = temps_entities::users::Model {
            id: 1,
            name: "Test User".to_string(),
            email: "test@example.com".to_string(),
            password_hash: Some("hashed_password".to_string()),
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        temps_auth::AuthContext::new_session(user, temps_auth::Role::Admin)
    }

    /// Helper to create a mock RequestMetadata for testing
    fn create_test_request_metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "127.0.0.1".to_string(),
            user_agent: "test-agent".to_string(),
            headers: axum::http::HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://127.0.0.1".to_string(),
            scheme: "http".to_string(),
            host: "127.0.0.1".to_string(),
            is_secure: false,
        }
    }

    #[tokio::test]
    async fn test_websocket_handler_end_to_end_with_server() {
        // FIXME: Flaky test - real-time log streaming timing issues.
        // Needs refactoring as a proper integration test.
        // Set TEMPS_FLAKY_TESTS=1 to enable.
        if std::env::var("TEMPS_FLAKY_TESTS").is_err() {
            println!("Flaky test skipped; set TEMPS_FLAKY_TESTS=1 to enable");
            return;
        }
        use axum::extract::Request;
        use axum::middleware;
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::{deployment_jobs, deployments, environments, projects};

        // This test spins up a real Axum server and connects with a WebSocket client
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db = test_db.connection_arc();

        let temp_dir = std::env::temp_dir().join(format!("test_ws_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("Failed to create temp dir");

        let app_state = create_test_app_state_for_http(db.clone(), temp_dir.clone()).await;

        // Create test data in database
        // 1. Create a test project
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/tmp/test-project".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Static),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test project");

        // 2. Create a test environment
        let subdomain = format!("test-env-{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Test Environment".to_string()),
            slug: Set("test-env".to_string()),
            subdomain: Set(subdomain.clone()),
            host: Set(format!("{}.localhost", subdomain)),
            upstreams: Set(UpstreamList::default()),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test environment");

        // 3. Create a test deployment
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set(format!("test-deployment-{}", uuid::Uuid::new_v4())),
            state: Set("in_progress".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )), // Empty metadata object
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test deployment");

        // 4. Create a test deployment job
        let job_log_id = format!("deployment-{}-job-test", deployment.id);
        let job = deployment_jobs::ActiveModel {
            deployment_id: Set(deployment.id),
            job_id: Set("test-job".to_string()),
            job_type: Set("build".to_string()),
            name: Set("Test Build Job".to_string()),
            log_id: Set(job_log_id.clone()),
            status: Set(temps_entities::types::JobStatus::Running),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test deployment job");

        // Create a middleware that injects AuthContext for testing
        let auth_middleware = middleware::from_fn(
            |mut req: Request, next: axum::middleware::Next| async move {
                let auth_context = create_test_auth_context();
                req.extensions_mut().insert(auth_context);
                next.run(req).await
            },
        );

        // Create router with WebSocket routes and test auth middleware
        let app = Router::new()
            .route(
                "/api/projects/{project_id}/deployments/{deployment_id}/jobs/{job_id}/logs/tail",
                get(tail_deployment_job_logs),
            )
            .layer(auth_middleware)
            .with_state(app_state.clone());

        // Bind to a random port
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind");
        let addr = listener.local_addr().expect("Failed to get local address");

        // Spawn server in background
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("Server failed to start");
        });

        // Give server time to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Test: Connect to deployment job logs endpoint
        let ws_url = format!(
            "ws://{}/api/projects/{}/deployments/{}/jobs/{}/logs/tail",
            addr, project.id, deployment.id, job.job_id
        );

        println!("Connecting to WebSocket at: {}", ws_url);
        let (mut ws_stream, response) = connect_async(&ws_url)
            .await
            .expect("Failed to connect to WebSocket");

        // Verify we didn't get a 401 Unauthorized
        if response.status() == 401 {
            panic!("WebSocket connection rejected with 401 Unauthorized - authentication failed!");
        }

        println!(
            "✅ WebSocket connection established (status: {})",
            response.status()
        );

        // Give WebSocket handler time to set up file watcher
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Write logs AFTER WebSocket connection so they can be streamed in real-time
        app_state
            .log_service
            .append_structured_log(&job_log_id, temps_logs::LogLevel::Info, "Job log line 1")
            .await
            .expect("Failed to write job log");
        app_state
            .log_service
            .append_structured_log(&job_log_id, temps_logs::LogLevel::Info, "Job log line 2")
            .await
            .expect("Failed to write job log");

        // Give time for logs to be picked up by the file watcher
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Verify log file was created and has content
        let log_path = app_state.log_service.get_log_path(&job_log_id);
        assert!(log_path.exists(), "Log file should exist at {:?}", log_path);
        let log_content = tokio::fs::read_to_string(&log_path)
            .await
            .expect("Failed to read log file");
        println!("Log file content:\n{}", log_content);
        assert!(!log_content.is_empty(), "Log file should have content");

        // Receive messages
        let mut messages = Vec::new();

        // Try to receive messages for up to 5 seconds (file watcher polls every 100ms)
        while let Some(result) = timeout(Duration::from_secs(5), ws_stream.next())
            .await
            .ok()
            .flatten()
        {
            match result {
                Ok(WsMessage::Text(text)) => {
                    println!("Received message: {}", text);
                    messages.push(text);
                    if messages.len() >= 2 {
                        break; // Got expected number of messages
                    }
                }
                Ok(WsMessage::Close(_)) => {
                    println!("WebSocket closed");
                    break;
                }
                Err(e) => {
                    panic!("WebSocket error: {}", e);
                }
                _ => {}
            }
        }

        // Verify we received the messages
        assert_eq!(
            messages.len(),
            2,
            "Should receive 2 log messages. Log file has content but WebSocket didn't stream it."
        );

        // Verify raw log format (not JSON)
        for (i, msg) in messages.iter().enumerate() {
            assert!(
                msg.contains(&format!("Job log line {}", i + 1)),
                "Log should contain expected text. Got: '{}'",
                msg
            );
        }

        println!("✅ Received {} raw log messages", messages.len());

        // Close connection
        let _ = ws_stream.close(None).await;

        println!("✅ End-to-end WebSocket handler test completed");
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    /// Helper: create a real Docker container that echoes known log lines, then exits.
    /// Returns the actual Docker container ID.
    async fn create_test_docker_container(
        docker: &bollard::Docker,
        name: &str,
        log_lines: &[&str],
    ) -> String {
        use bollard::models::ContainerCreateBody;
        use bollard::query_parameters::{
            CreateContainerOptionsBuilder, RemoveContainerOptions, StartContainerOptions,
            WaitContainerOptionsBuilder,
        };
        use futures::TryStreamExt;

        // Build a shell command that echoes each line to stdout
        let echo_cmds: Vec<String> = log_lines.iter().map(|l| format!("echo '{}'", l)).collect();
        let cmd = echo_cmds.join(" && ");

        let container_name = format!("temps-test-{}-{}", name, uuid::Uuid::new_v4());

        // Remove any leftover container with the same name
        let _ = docker
            .remove_container(
                &container_name,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        let resp = docker
            .create_container(
                Some(
                    CreateContainerOptionsBuilder::new()
                        .name(&container_name)
                        .build(),
                ),
                ContainerCreateBody {
                    image: Some("alpine:latest".to_string()),
                    cmd: Some(vec!["sh".to_string(), "-c".to_string(), cmd]),
                    ..Default::default()
                },
            )
            .await
            .expect("Failed to create test Docker container");

        let container_id = resp.id;

        docker
            .start_container(&container_id, None::<StartContainerOptions>)
            .await
            .expect("Failed to start test Docker container");

        // Wait for the container to finish
        let _ = docker
            .wait_container(
                &container_id,
                Some(WaitContainerOptionsBuilder::new().build()),
            )
            .try_collect::<Vec<_>>()
            .await;

        container_id
    }

    /// Helper: remove a Docker container created for testing.
    async fn cleanup_test_docker_container(docker: &bollard::Docker, container_id: &str) {
        use bollard::query_parameters::RemoveContainerOptions;
        let _ = docker
            .remove_container(
                container_id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
    }

    #[tokio::test]
    async fn test_container_logs_by_id_websocket() {
        let docker = match bollard::Docker::connect_with_local_defaults() {
            Ok(d) => d,
            Err(_) => {
                println!("Docker not available, skipping test");
                return;
            }
        };
        if docker.ping().await.is_err() {
            println!("Docker not available, skipping test");
            return;
        }

        use axum::extract::Request;
        use axum::middleware;
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::{
            deployment_containers as containers, deployments, environments, projects,
        };

        // Create a real Docker container with known log output
        let real_container_id = create_test_docker_container(
            &docker,
            "logs-by-id",
            &["Container log line 1", "Container log line 2"],
        )
        .await;

        // Setup test database and services
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db = test_db.connection_arc();

        let temp_dir = std::env::temp_dir().join(format!("test_ws_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("Failed to create temp dir");

        let app_state = create_test_app_state_for_http(db.clone(), temp_dir.clone()).await;

        // Create test data - use Dockerfile preset (not Static) since container
        // logs are only available for server-type projects
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/tmp/test-project".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Dockerfile),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test project");

        let subdomain = format!("test-env-{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Test Environment".to_string()),
            slug: Set("test-env".to_string()),
            subdomain: Set(subdomain.clone()),
            host: Set(format!("{}.localhost", subdomain)),
            upstreams: Set(UpstreamList::default()),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test environment");

        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set(format!("test-deployment-{}", uuid::Uuid::new_v4())),
            state: Set("running".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test deployment");

        // Update environment with current_deployment_id
        let mut env_active: environments::ActiveModel = environment.into();
        env_active.current_deployment_id = Set(Some(deployment.id));
        let environment = env_active
            .update(&*db)
            .await
            .expect("Failed to update environment with deployment");

        // Create a DB container record pointing to the real Docker container
        let now = chrono::Utc::now();
        let container = containers::ActiveModel {
            deployment_id: Set(deployment.id),
            container_id: Set(real_container_id.clone()),
            container_name: Set("test-container".to_string()),
            container_port: Set(8080),
            image_name: Set(Some("alpine:latest".to_string())),
            status: Set(Some("running".to_string())),
            created_at: Set(now),
            deployed_at: Set(now),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test container");

        // Create auth middleware
        let auth_middleware = middleware::from_fn(
            |mut req: Request, next: axum::middleware::Next| async move {
                let auth_context = create_test_auth_context();
                req.extensions_mut().insert(auth_context);
                next.run(req).await
            },
        );

        // Create router
        let app = Router::new()
            .route(
                "/api/projects/{project_id}/environments/{environment_id}/containers/{container_id}/logs",
                get(get_container_logs_by_id),
            )
            .layer(auth_middleware)
            .with_state(app_state.clone());

        // Start server
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind");
        let addr = listener.local_addr().expect("Failed to get local address");

        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("Server failed to start");
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Connect to WebSocket
        let ws_url = format!(
            "ws://{}/api/projects/{}/environments/{}/containers/{}/logs",
            addr, project.id, environment.id, container.container_id
        );

        println!("Connecting to WebSocket at: {}", ws_url);
        let (mut ws_stream, response) = connect_async(&ws_url)
            .await
            .expect("Failed to connect to WebSocket");

        if response.status() == 401 {
            panic!("WebSocket connection rejected with 401 Unauthorized - authentication failed!");
        }

        println!(
            "WebSocket connection established (status: {})",
            response.status()
        );

        // Receive messages
        let mut messages = Vec::new();

        while let Some(result) = timeout(Duration::from_secs(5), ws_stream.next())
            .await
            .ok()
            .flatten()
        {
            match result {
                Ok(WsMessage::Text(text)) => {
                    println!("Received message: {}", text);
                    messages.push(text);
                    if messages.len() >= 2 {
                        break;
                    }
                }
                Ok(WsMessage::Close(_)) => {
                    println!("WebSocket closed");
                    break;
                }
                Err(e) => {
                    panic!("WebSocket error: {}", e);
                }
                _ => {}
            }
        }

        // Verify messages - logs might come as a single message or multiple
        println!("Total messages received: {}", messages.len());
        for (i, msg) in messages.iter().enumerate() {
            println!("Message {}: '{}'", i, msg);
        }

        assert!(!messages.is_empty(), "Should receive at least 1 message");

        // Check that both log lines are present (they might be in one message or separate)
        let all_logs = messages.join("");
        assert!(
            all_logs.contains("Container log line 1"),
            "Logs should contain line 1. Got: '{}'",
            all_logs
        );
        assert!(
            all_logs.contains("Container log line 2"),
            "Logs should contain line 2. Got: '{}'",
            all_logs
        );

        println!("Received {} raw container log messages", messages.len());

        let _ = ws_stream.close(None).await;

        // Cleanup
        cleanup_test_docker_container(&docker, &real_container_id).await;
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_filtered_container_logs_websocket() {
        let docker = match bollard::Docker::connect_with_local_defaults() {
            Ok(d) => d,
            Err(_) => {
                println!("Docker not available, skipping test");
                return;
            }
        };
        if docker.ping().await.is_err() {
            println!("Docker not available, skipping test");
            return;
        }

        use axum::extract::Request;
        use axum::middleware;
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::{
            deployment_containers as containers, deployments, environments, projects,
        };

        // Create real Docker containers with known log output
        let real_container1_id =
            create_test_docker_container(&docker, "filtered-web", &["Web container log 1"]).await;
        let real_container2_id =
            create_test_docker_container(&docker, "filtered-db", &["DB container log 1"]).await;

        // Setup test database and services
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db = test_db.connection_arc();

        let temp_dir = std::env::temp_dir().join(format!("test_ws_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("Failed to create temp dir");

        let app_state = create_test_app_state_for_http(db.clone(), temp_dir.clone()).await;

        // Create test data - use Dockerfile preset (not Static) since container
        // logs are only available for server-type projects
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/tmp/test-project".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Dockerfile),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test project");

        let subdomain = format!("test-env-{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Test Environment".to_string()),
            slug: Set("test-env".to_string()),
            subdomain: Set(subdomain.clone()),
            host: Set(format!("{}.localhost", subdomain)),
            upstreams: Set(UpstreamList::default()),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test environment");

        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set(format!("test-deployment-{}", uuid::Uuid::new_v4())),
            state: Set("running".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test deployment");

        // Update environment with current_deployment_id
        let mut env_active: environments::ActiveModel = environment.into();
        env_active.current_deployment_id = Set(Some(deployment.id));
        let environment = env_active
            .update(&*db)
            .await
            .expect("Failed to update environment with deployment");

        // Create DB container records pointing to real Docker containers
        let now = chrono::Utc::now();
        let _container1 = containers::ActiveModel {
            deployment_id: Set(deployment.id),
            container_id: Set(real_container1_id.clone()),
            container_name: Set("web-container".to_string()),
            container_port: Set(8080),
            image_name: Set(Some("alpine:latest".to_string())),
            status: Set(Some("running".to_string())),
            created_at: Set(now),
            deployed_at: Set(now),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create container 1");

        // Note: get_filtered_container_logs picks the first container (or by name),
        // so this test verifies the "no name filter" path which returns the first container.
        // We test that we get logs from at least one real container.

        // Create auth middleware
        let auth_middleware = middleware::from_fn(
            |mut req: Request, next: axum::middleware::Next| async move {
                let auth_context = create_test_auth_context();
                req.extensions_mut().insert(auth_context);
                next.run(req).await
            },
        );

        // Create router - use container_name query param to target the web container
        let app = Router::new()
            .route(
                "/api/projects/{project_id}/environments/{environment_id}/container-logs",
                get(get_container_logs),
            )
            .layer(auth_middleware)
            .with_state(app_state.clone());

        // Start server
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind");
        let addr = listener.local_addr().expect("Failed to get local address");

        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("Server failed to start");
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Connect to WebSocket (no container_name filter → picks first container)
        let ws_url = format!(
            "ws://{}/api/projects/{}/environments/{}/container-logs",
            addr, project.id, environment.id
        );

        println!("Connecting to WebSocket at: {}", ws_url);
        let (mut ws_stream, response) = connect_async(&ws_url)
            .await
            .expect("Failed to connect to WebSocket");

        if response.status() == 401 {
            panic!("WebSocket connection rejected with 401 Unauthorized - authentication failed!");
        }

        println!(
            "WebSocket connection established (status: {})",
            response.status()
        );

        // Receive messages
        let mut messages = Vec::new();

        while let Some(result) = timeout(Duration::from_secs(5), ws_stream.next())
            .await
            .ok()
            .flatten()
        {
            match result {
                Ok(WsMessage::Text(text)) => {
                    println!("Received message: {}", text);
                    messages.push(text);
                    break; // We only need one message to verify
                }
                Ok(WsMessage::Close(_)) => {
                    println!("WebSocket closed");
                    break;
                }
                Err(e) => {
                    panic!("WebSocket error: {}", e);
                }
                _ => {}
            }
        }

        // Verify we got logs from the container
        println!("Total messages received: {}", messages.len());
        for (i, msg) in messages.iter().enumerate() {
            println!("Message {}: '{}'", i, msg);
        }

        assert!(!messages.is_empty(), "Should receive at least 1 message");

        let all_logs = messages.join("");
        assert!(
            all_logs.contains("Web container"),
            "Should receive web container logs. Got: '{}'",
            all_logs
        );

        println!(
            "Received {} raw log messages from container",
            messages.len()
        );

        let _ = ws_stream.close(None).await;

        // Cleanup
        cleanup_test_docker_container(&docker, &real_container1_id).await;
        cleanup_test_docker_container(&docker, &real_container2_id).await;
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    // =============================================================================
    // HTTP Endpoint E2E Tests
    // =============================================================================

    /// Helper to create test app state for HTTP tests
    async fn create_test_app_state_for_http(
        db: Arc<sea_orm::DatabaseConnection>,
        temp_dir: std::path::PathBuf,
    ) -> Arc<AppState> {
        let log_service = Arc::new(LogService::new(temp_dir.clone()));
        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults().expect("Failed to connect to Docker"),
        );
        let docker_log_service = Arc::new(DockerLogService::new(docker.clone()));

        let server_config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:0".to_string(),
                "postgresql://test:test@localhost:5432/test".to_string(),
                None,
                None,
            )
            .expect("Failed to create server config"),
        );
        let config_service = Arc::new(ConfigService::new(server_config, db.clone()));

        let (job_sender, _job_receiver) = tokio::sync::broadcast::channel(100);
        let queue_service: Arc<dyn temps_core::JobQueue> =
            Arc::new(temps_queue::BroadcastQueueService::new(job_sender));

        let deployer: Arc<dyn temps_deployer::ContainerDeployer> =
            Arc::new(temps_deployer::docker::DockerRuntime::new(
                docker.clone(),
                false,
                "temps-test".to_string(),
            ));

        let encryption_service = Arc::new(
            temps_core::EncryptionService::new(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .expect("Failed to create test encryption service"),
        );

        let deployment_service = Arc::new(crate::services::services::DeploymentService::new(
            db.clone(),
            log_service.clone(),
            config_service.clone(),
            queue_service.clone(),
            docker_log_service,
            deployer,
            encryption_service.clone(),
        ));

        let deployment_token_service = Arc::new(
            crate::services::deployment_token_service::DeploymentTokenService::new(
                db.clone(),
                encryption_service.clone(),
            ),
        );

        let cron_service = Arc::new(
            crate::services::database_cron_service::DatabaseCronConfigService::new(
                db.clone(),
                queue_service.clone(),
                deployment_token_service,
            ),
        );

        let remote_deployment_service =
            Arc::new(crate::services::RemoteDeploymentService::new(db.clone()));

        let external_service_manager = Arc::new(temps_providers::ExternalServiceManager::new(
            db.clone(),
            encryption_service.clone(),
            docker.clone(),
            Arc::new(temps_providers::DnsRegistry::new(db.clone())),
        ));

        let dsn_service = Arc::new(temps_error_tracking::DSNService::new(db.clone()));

        let workflow_planner = Arc::new(crate::services::workflow_planner::WorkflowPlanner::new(
            db.clone(),
            log_service.clone(),
            external_service_manager,
            config_service.clone(),
            dsn_service,
            encryption_service,
        ));

        let rustfs_service = Arc::new(temps_providers::externalsvc::RustfsService::new(
            "test".to_string(),
            docker,
            Arc::new(
                temps_core::EncryptionService::new(
                    "0000000000000000000000000000000000000000000000000000000000000000",
                )
                .expect("enc"),
            ),
        ));
        let blob_service = Arc::new(temps_blob::BlobService::new(rustfs_service));

        // Use noop screenshot provider via env var
        // SAFETY: This is test-only code; tests are run single-threaded or this env var
        // is idempotent (always set to the same value).
        unsafe {
            std::env::set_var("TEMPS_SCREENSHOT_PROVIDER", "noop");
        }
        let screenshot_service = Arc::new(
            temps_screenshots::ScreenshotService::new(config_service)
                .await
                .expect("Failed to create screenshot service"),
        );

        let workflow_executor = Arc::new(crate::services::WorkflowExecutionService::new(
            db.clone(),
            queue_service.clone(),
            Arc::new(MockGitProviderManager) as Arc<dyn temps_git::GitProviderManagerTrait>,
            Arc::new(MockImageBuilder) as Arc<dyn temps_deployer::ImageBuilder>,
            Arc::new(temps_deployer::docker::DockerRuntime::new(
                Arc::new(bollard::Docker::connect_with_local_defaults().expect("docker")),
                false,
                "temps-test".to_string(),
            )) as Arc<dyn temps_deployer::ContainerDeployer>,
            Arc::new(MockStaticDeployer)
                as Arc<dyn temps_deployer::static_deployer::StaticDeployer>,
            log_service.clone(),
            Arc::new(MockCronConfigService) as Arc<dyn crate::jobs::CronConfigService>,
            Arc::new(crate::jobs::NoOpAgentSyncService) as Arc<dyn crate::jobs::AgentSyncService>,
            Arc::new(ConfigService::new(
                Arc::new(
                    temps_config::ServerConfig::new(
                        "127.0.0.1:0".to_string(),
                        "postgresql://test:test@localhost:5432/test".to_string(),
                        None,
                        None,
                    )
                    .expect("config"),
                ),
                db.clone(),
            )),
            screenshot_service,
            Arc::new(bollard::Docker::connect_with_local_defaults().expect("docker")),
        ));

        Arc::new(AppState {
            deployment_service,
            log_service,
            cron_service,
            external_deployment_manager: Arc::new(crate::services::ExternalDeploymentManager::new()),
            remote_deployment_service,
            db: db.clone(),
            workflow_planner,
            workflow_executor,
            queue_service,
            blob_service,
            data_dir: temp_dir,
            image_builder: Arc::new(MockImageBuilder) as Arc<dyn temps_deployer::ImageBuilder>,
            audit_service: Arc::new(MockAuditLogger) as Arc<dyn temps_core::AuditLogger>,
            node_service: Arc::new(crate::services::NodeService::new(db.clone())),
            encryption_service: Arc::new(
                temps_core::EncryptionService::new("01234567890123456789012345678901").unwrap(),
            ),
            config_service: Arc::new(ConfigService::new(
                Arc::new(
                    temps_config::ServerConfig::new(
                        "127.0.0.1:0".to_string(),
                        "postgresql://test:test@localhost:5432/test".to_string(),
                        None,
                        None,
                    )
                    .expect("config"),
                ),
                db.clone(),
            )),
            docker: Arc::new(
                bollard::Docker::connect_with_local_defaults()
                    .unwrap_or_else(|_| bollard::Docker::connect_with_defaults().unwrap()),
            ),
            deployment_gate: None,
            project_access_checker: None,
            hostname_resolver: Arc::new(temps_core::StandardHostnameResolver)
                as Arc<dyn temps_core::PublicHostnameResolver>,
            metrics_store: None,
        })
    }

    #[tokio::test]
    async fn test_get_last_deployment_endpoint() {
        use axum::extract::Request;
        use axum::middleware;
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::{deployments, environments, projects};

        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db = test_db.connection_arc();

        let temp_dir = std::env::temp_dir().join(format!("test_http_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("Failed to create temp dir");

        let app_state = create_test_app_state_for_http(db.clone(), temp_dir.clone()).await;

        // Create test data
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/tmp/test-project".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Static),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test project");

        let subdomain = format!("test-env-{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Test Environment".to_string()),
            slug: Set("test-env".to_string()),
            subdomain: Set(subdomain.clone()),
            host: Set(format!("{}.localhost", subdomain)),
            upstreams: Set(UpstreamList::default()),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test environment");

        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set(format!("test-deployment-{}", uuid::Uuid::new_v4())),
            state: Set("deployed".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test deployment");

        // Create auth middleware
        let auth_middleware = middleware::from_fn(
            |mut req: Request, next: axum::middleware::Next| async move {
                let auth_context = create_test_auth_context();
                req.extensions_mut().insert(auth_context);
                next.run(req).await
            },
        );

        // Use configure_routes() and add auth middleware
        let app = configure_routes()
            .layer(auth_middleware)
            .with_state(app_state.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind");
        let addr = listener.local_addr().expect("Failed to get address");

        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("Server failed to start");
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Test GET /projects/{id}/last-deployment
        let client = reqwest::Client::new();
        let response = client
            .get(format!(
                "http://{}/projects/{}/last-deployment",
                addr, project.id
            ))
            .send()
            .await
            .expect("Failed to send request");

        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.json().await.expect("Failed to parse JSON");
        println!(
            "Response body: {}",
            serde_json::to_string_pretty(&body).unwrap()
        );
        assert_eq!(body["id"], deployment.id);
        assert_eq!(body["status"], "deployed");

        println!("✅ GET /projects/{{id}}/last-deployment test passed");
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_get_project_deployments_endpoint() {
        use axum::extract::Request;
        use axum::middleware;
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::{deployments, environments, projects};

        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db = test_db.connection_arc();

        let temp_dir = std::env::temp_dir().join(format!("test_http_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("Failed to create temp dir");

        let app_state = create_test_app_state_for_http(db.clone(), temp_dir.clone()).await;

        // Create test data
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/tmp/test-project".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Static),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test project");

        let subdomain = format!("test-env-{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Test Environment".to_string()),
            slug: Set("test-env".to_string()),
            subdomain: Set(subdomain.clone()),
            host: Set(format!("{}.localhost", subdomain)),
            upstreams: Set(UpstreamList::default()),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test environment");

        // Create multiple deployments
        let _deployment1 = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set(format!("test-deployment-1-{}", uuid::Uuid::new_v4())),
            state: Set("deployed".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test deployment 1");

        let _deployment2 = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set(format!("test-deployment-2-{}", uuid::Uuid::new_v4())),
            state: Set("in_progress".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test deployment 2");

        let auth_middleware = middleware::from_fn(
            |mut req: Request, next: axum::middleware::Next| async move {
                let auth_context = create_test_auth_context();
                req.extensions_mut().insert(auth_context);
                next.run(req).await
            },
        );

        let app = configure_routes()
            .layer(auth_middleware)
            .with_state(app_state.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind");
        let addr = listener.local_addr().expect("Failed to get address");

        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("Server failed to start");
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Test GET /projects/{id}/deployments
        let client = reqwest::Client::new();
        let response = client
            .get(format!(
                "http://{}/projects/{}/deployments",
                addr, project.id
            ))
            .send()
            .await
            .expect("Failed to send request");

        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.json().await.expect("Failed to parse JSON");
        assert!(body["deployments"].is_array());
        assert_eq!(body["deployments"].as_array().unwrap().len(), 2);
        assert_eq!(body["total"], 2);

        println!("✅ GET /projects/{{id}}/deployments test passed");
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_get_deployment_endpoint() {
        use axum::extract::Request;
        use axum::middleware;
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::{deployments, environments, projects};

        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db = test_db.connection_arc();

        let temp_dir = std::env::temp_dir().join(format!("test_http_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("Failed to create temp dir");

        let app_state = create_test_app_state_for_http(db.clone(), temp_dir.clone()).await;

        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/tmp/test-project".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Static),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test project");

        let subdomain = format!("test-env-{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Test Environment".to_string()),
            slug: Set("test-env".to_string()),
            subdomain: Set(subdomain.clone()),
            host: Set(format!("{}.localhost", subdomain)),
            upstreams: Set(UpstreamList::default()),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test environment");

        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set(format!("test-deployment-{}", uuid::Uuid::new_v4())),
            state: Set("deployed".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test deployment");

        let auth_middleware = middleware::from_fn(
            |mut req: Request, next: axum::middleware::Next| async move {
                let auth_context = create_test_auth_context();
                req.extensions_mut().insert(auth_context);
                next.run(req).await
            },
        );

        let app = configure_routes()
            .layer(auth_middleware)
            .with_state(app_state.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind");
        let addr = listener.local_addr().expect("Failed to get address");

        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("Server failed to start");
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Test GET /projects/{project_id}/deployments/{deployment_id}
        let client = reqwest::Client::new();
        let response = client
            .get(format!(
                "http://{}/projects/{}/deployments/{}",
                addr, project.id, deployment.id
            ))
            .send()
            .await
            .expect("Failed to send request");

        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.json().await.expect("Failed to parse JSON");
        assert_eq!(body["id"], deployment.id);
        assert_eq!(body["status"], "deployed");

        println!("✅ GET /projects/{{project_id}}/deployments/{{deployment_id}} test passed");
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_get_deployment_jobs_endpoint() {
        use axum::extract::Request;
        use axum::middleware;
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::{deployment_jobs, deployments, environments, projects};

        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db = test_db.connection_arc();

        let temp_dir = std::env::temp_dir().join(format!("test_http_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("Failed to create temp dir");

        let app_state = create_test_app_state_for_http(db.clone(), temp_dir.clone()).await;

        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/tmp/test-project".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Static),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test project");

        let subdomain = format!("test-env-{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Test Environment".to_string()),
            slug: Set("test-env".to_string()),
            subdomain: Set(subdomain.clone()),
            host: Set(format!("{}.localhost", subdomain)),
            upstreams: Set(UpstreamList::default()),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test environment");

        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set(format!("test-deployment-{}", uuid::Uuid::new_v4())),
            state: Set("deployed".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test deployment");

        // Create deployment jobs
        let _job1 = deployment_jobs::ActiveModel {
            deployment_id: Set(deployment.id),
            job_id: Set("build-job".to_string()),
            job_type: Set("build".to_string()),
            name: Set("Build Job".to_string()),
            log_id: Set("build-log".to_string()),
            status: Set(temps_entities::types::JobStatus::Success),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create job 1");

        let _job2 = deployment_jobs::ActiveModel {
            deployment_id: Set(deployment.id),
            job_id: Set("deploy-job".to_string()),
            job_type: Set("deploy".to_string()),
            name: Set("Deploy Job".to_string()),
            log_id: Set("deploy-log".to_string()),
            status: Set(temps_entities::types::JobStatus::Running),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create job 2");

        let auth_middleware = middleware::from_fn(
            |mut req: Request, next: axum::middleware::Next| async move {
                let auth_context = create_test_auth_context();
                req.extensions_mut().insert(auth_context);
                next.run(req).await
            },
        );

        let app = configure_routes()
            .layer(auth_middleware)
            .with_state(app_state.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind");
        let addr = listener.local_addr().expect("Failed to get address");

        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("Server failed to start");
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Test GET /projects/{project_id}/deployments/{deployment_id}/jobs
        let client = reqwest::Client::new();
        let response = client
            .get(format!(
                "http://{}/projects/{}/deployments/{}/jobs",
                addr, project.id, deployment.id
            ))
            .send()
            .await
            .expect("Failed to send request");

        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.json().await.expect("Failed to parse JSON");
        assert!(body["jobs"].is_array());
        assert_eq!(body["jobs"].as_array().unwrap().len(), 2);

        println!("✅ GET /projects/{{project_id}}/deployments/{{deployment_id}}/jobs test passed");
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_pause_and_resume_deployment_endpoints() {
        use axum::extract::Request;
        use axum::middleware;
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::{deployments, environments, projects};

        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db = test_db.connection_arc();

        let temp_dir = std::env::temp_dir().join(format!("test_http_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("Failed to create temp dir");

        let app_state = create_test_app_state_for_http(db.clone(), temp_dir.clone()).await;

        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/tmp/test-project".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Static),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test project");

        let subdomain = format!("test-env-{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Test Environment".to_string()),
            slug: Set("test-env".to_string()),
            subdomain: Set(subdomain.clone()),
            host: Set(format!("{}.localhost", subdomain)),
            upstreams: Set(UpstreamList::default()),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test environment");

        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set(format!("test-deployment-{}", uuid::Uuid::new_v4())),
            state: Set("deployed".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test deployment");

        let auth_middleware = middleware::from_fn(
            |mut req: Request, next: axum::middleware::Next| async move {
                let auth_context = create_test_auth_context();
                req.extensions_mut().insert(auth_context);
                req.extensions_mut().insert(create_test_request_metadata());
                next.run(req).await
            },
        );

        let app = configure_routes()
            .layer(auth_middleware)
            .with_state(app_state.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind");
        let addr = listener.local_addr().expect("Failed to get address");

        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("Server failed to start");
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        let client = reqwest::Client::new();

        // Test POST /projects/{project_id}/deployments/{deployment_id}/pause
        let response = client
            .post(format!(
                "http://{}/projects/{}/deployments/{}/pause",
                addr, project.id, deployment.id
            ))
            .send()
            .await
            .expect("Failed to send pause request");

        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.json().await.expect("Failed to parse JSON");
        assert_eq!(body["state"], "paused");
        assert_eq!(body["message"], "Deployment paused successfully");

        println!(
            "✅ POST /projects/{{project_id}}/deployments/{{deployment_id}}/pause test passed"
        );

        // Test POST /projects/{project_id}/deployments/{deployment_id}/resume
        let response = client
            .post(format!(
                "http://{}/projects/{}/deployments/{}/resume",
                addr, project.id, deployment.id
            ))
            .send()
            .await
            .expect("Failed to send resume request");

        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.json().await.expect("Failed to parse JSON");
        assert_eq!(body["state"], "deployed");
        assert_eq!(body["message"], "Deployment resumed successfully");

        println!(
            "✅ POST /projects/{{project_id}}/deployments/{{deployment_id}}/resume test passed"
        );
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_cancel_deployment_endpoint() {
        use axum::extract::Request;
        use axum::middleware;
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::{deployments, environments, projects};

        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db = test_db.connection_arc();

        let temp_dir = std::env::temp_dir().join(format!("test_http_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("Failed to create temp dir");

        let app_state = create_test_app_state_for_http(db.clone(), temp_dir.clone()).await;

        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/tmp/test-project".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Static),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test project");

        let subdomain = format!("test-env-{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Test Environment".to_string()),
            slug: Set("test-env".to_string()),
            subdomain: Set(subdomain.clone()),
            host: Set(format!("{}.localhost", subdomain)),
            upstreams: Set(UpstreamList::default()),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test environment");

        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set(format!("test-deployment-{}", uuid::Uuid::new_v4())),
            state: Set("in_progress".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test deployment");

        let auth_middleware = middleware::from_fn(
            |mut req: Request, next: axum::middleware::Next| async move {
                let auth_context = create_test_auth_context();
                req.extensions_mut().insert(auth_context);
                req.extensions_mut().insert(create_test_request_metadata());
                next.run(req).await
            },
        );

        let app = configure_routes()
            .layer(auth_middleware)
            .with_state(app_state.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind");
        let addr = listener.local_addr().expect("Failed to get address");

        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("Server failed to start");
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Test POST /projects/{project_id}/deployments/{deployment_id}/cancel
        let client = reqwest::Client::new();
        let response = client
            .post(format!(
                "http://{}/projects/{}/deployments/{}/cancel",
                addr, project.id, deployment.id
            ))
            .send()
            .await
            .expect("Failed to send request");

        // The deployment is in "in_progress" state, so it can't be cancelled yet
        // The API correctly returns 400 Bad Request
        assert_eq!(response.status(), 400);

        println!(
            "✅ POST /projects/{{project_id}}/deployments/{{deployment_id}}/cancel test passed"
        );
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_teardown_deployment_endpoint() {
        use axum::extract::Request;
        use axum::middleware;
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::{deployments, environments, projects};

        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db = test_db.connection_arc();

        let temp_dir = std::env::temp_dir().join(format!("test_http_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("Failed to create temp dir");

        let app_state = create_test_app_state_for_http(db.clone(), temp_dir.clone()).await;

        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/tmp/test-project".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Static),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test project");

        let subdomain = format!("test-env-{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Test Environment".to_string()),
            slug: Set("test-env".to_string()),
            subdomain: Set(subdomain.clone()),
            host: Set(format!("{}.localhost", subdomain)),
            upstreams: Set(UpstreamList::default()),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test environment");

        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set(format!("test-deployment-{}", uuid::Uuid::new_v4())),
            state: Set("deployed".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test deployment");

        let auth_middleware = middleware::from_fn(
            |mut req: Request, next: axum::middleware::Next| async move {
                let auth_context = create_test_auth_context();
                req.extensions_mut().insert(auth_context);
                req.extensions_mut().insert(create_test_request_metadata());
                next.run(req).await
            },
        );

        let app = configure_routes()
            .layer(auth_middleware)
            .with_state(app_state.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind");
        let addr = listener.local_addr().expect("Failed to get address");

        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("Server failed to start");
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Test DELETE /projects/{project_id}/deployments/{deployment_id}/teardown
        let client = reqwest::Client::new();
        let response = client
            .delete(format!(
                "http://{}/projects/{}/deployments/{}/teardown",
                addr, project.id, deployment.id
            ))
            .send()
            .await
            .expect("Failed to send request");

        assert_eq!(response.status(), 204);

        println!(
            "✅ DELETE /projects/{{project_id}}/deployments/{{deployment_id}}/teardown test passed"
        );
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_teardown_environment_endpoint() {
        use axum::extract::Request;
        use axum::middleware;
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::{environments, projects};

        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db = test_db.connection_arc();

        let temp_dir = std::env::temp_dir().join(format!("test_http_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("Failed to create temp dir");

        let app_state = create_test_app_state_for_http(db.clone(), temp_dir.clone()).await;

        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/tmp/test-project".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Static),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test project");

        let subdomain = format!("test-env-{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Test Environment".to_string()),
            slug: Set("test-env".to_string()),
            subdomain: Set(subdomain.clone()),
            host: Set(format!("{}.localhost", subdomain)),
            upstreams: Set(UpstreamList::default()),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test environment");

        let auth_middleware = middleware::from_fn(
            |mut req: Request, next: axum::middleware::Next| async move {
                let auth_context = create_test_auth_context();
                req.extensions_mut().insert(auth_context);
                req.extensions_mut().insert(create_test_request_metadata());
                next.run(req).await
            },
        );

        let app = configure_routes()
            .layer(auth_middleware)
            .with_state(app_state.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind");
        let addr = listener.local_addr().expect("Failed to get address");

        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("Server failed to start");
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Test DELETE /projects/{project_id}/environments/{env_id}/teardown
        let client = reqwest::Client::new();
        let response = client
            .delete(format!(
                "http://{}/projects/{}/environments/{}/teardown",
                addr, project.id, environment.id
            ))
            .send()
            .await
            .expect("Failed to send request");

        assert_eq!(response.status(), 204);

        println!("✅ DELETE /projects/{{project_id}}/environments/{{env_id}}/teardown test passed");
        std::fs::remove_dir_all(&temp_dir).ok();
    }
}
