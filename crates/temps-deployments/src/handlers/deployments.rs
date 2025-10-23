use std::sync::Arc;

use super::types::AppState;
use axum::Router;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Json,
};
use futures::stream::StreamExt;
use futures::SinkExt;
use temps_auth::permission_guard;
use temps_auth::RequireAuth;
use tracing::{debug, error, info, warn};
use utoipa::OpenApi;

use crate::handlers::types::{
    ContainerInfoResponse, ContainerListResponse, ContainerLogsQuery, DeploymentJobResponse,
    DeploymentJobsResponse, DeploymentListResponse, DeploymentResponse, DeploymentStateResponse,
};
use temps_core::problemdetails;
use temps_core::problemdetails::Problem;

#[derive(OpenApi)]
#[openapi(
    paths(
        get_last_deployment,
        get_project_deployments,
        get_deployment,
        get_deployment_jobs,
        get_deployment_job_logs,
        tail_deployment_job_logs,
        rollback_to_deployment,
        pause_deployment,
        resume_deployment,
        cancel_deployment,
        teardown_deployment,
        teardown_environment,
        list_containers,
        get_container_logs_by_id,
        get_container_logs
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
        ContainerInfoResponse
    )),
    info(
        title = "Deployments API",
        description = "API endpoints for managing deployments and container logs. \
        Provides comprehensive deployment lifecycle management including rollbacks, pausing/resuming, and real-time log streaming.",
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
        // Deployment operations
        .route(
            "/projects/{project_id}/deployments/{deployment_id}/rollback",
            post(rollback_to_deployment),
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
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsCreate);

    let deployment = state
        .deployment_service
        .rollback_to_deployment(project_id, deployment_id)
        .await?;

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
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsDelete);
    info!("Pausing deployment: {:?}", deployment_id);

    state
        .deployment_service
        .pause_deployment(project_id, deployment_id)
        .await?;

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
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsCreate);

    state
        .deployment_service
        .resume_deployment(project_id, deployment_id)
        .await?;

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
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsDelete);

    info!(
        "🛑 API request to cancel deployment {} for project {} from user",
        deployment_id, project_id
    );

    state
        .deployment_service
        .cancel_deployment(project_id, deployment_id)
        .await?;

    info!(
        "✅ Deployment {} cancellation request processed successfully",
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
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsDelete);

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
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsDelete);

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

    info!(
        "Listing containers for environment {} of project: {}",
        environment_id, project_id
    );

    let containers = state
        .deployment_service
        .list_environment_containers(project_id, environment_id)
        .await?;

    let container_responses: Vec<ContainerInfoResponse> =
        containers.into_iter().map(Into::into).collect();

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
        ("tail" = Option<String>, Query, description = "Number of lines to tail (or 'all')")
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

    info!(
        "WebSocket request for container {} logs in environment {} of project: {}",
        container_id, environment_id, project_id
    );

    // Upgrade to WebSocket and handle the connection
    Ok(ws.on_upgrade(move |socket| {
        handle_container_logs_socket(
            socket,
            state,
            project_id,
            environment_id,
            container_id,
            query.start_date,
            query.end_date,
            query.tail,
        )
    }))
}

/// Handle WebSocket connection for container log streaming
#[allow(clippy::too_many_arguments)]
async fn handle_container_logs_socket(
    mut socket: WebSocket,
    state: Arc<AppState>,
    project_id: i32,
    environment_id: i32,
    container_id: String,
    start_date: Option<i64>,
    end_date: Option<i64>,
    tail: Option<String>,
) {
    debug!(
        "WebSocket connection established for container {} logs",
        container_id
    );

    // Get the log stream from the deployment service
    let log_stream = match state
        .deployment_service
        .get_container_logs_by_id(
            project_id,
            environment_id,
            container_id.clone(),
            start_date,
            end_date,
            tail,
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

    // Stream logs to WebSocket client (raw text, not JSON)
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
        "WebSocket connection closed for container {} logs",
        container_id
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
        ("container_name" = Option<String>, Query, description = "Optional container name (defaults to first/primary container)")
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

    info!(
        "WebSocket request for container logs in environment {} of project: {}",
        environment_id, project_id
    );

    // Upgrade to WebSocket and handle the connection
    Ok(ws.on_upgrade(move |socket| {
        handle_filtered_container_logs_socket(
            socket,
            state,
            project_id,
            environment_id,
            query.start_date,
            query.end_date,
            query.tail,
            query.container_name,
        )
    }))
}

/// Handle WebSocket connection for filtered container log streaming
#[allow(clippy::too_many_arguments)]
async fn handle_filtered_container_logs_socket(
    mut socket: WebSocket,
    state: Arc<AppState>,
    project_id: i32,
    environment_id: i32,
    start_date: Option<i64>,
    end_date: Option<i64>,
    tail: Option<String>,
    container_name: Option<String>,
) {
    debug!(
        "WebSocket connection established for environment {} container logs",
        environment_id
    );

    // Get the log stream from the deployment service
    let log_stream = match state
        .deployment_service
        .get_filtered_container_logs(
            project_id,
            environment_id,
            start_date,
            end_date,
            tail,
            container_name,
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
        environment_id
    );
    let _ = socket.close().await;
}

/// Get jobs for a specific deployment
///
/// Returns all jobs (workflow tasks) for a deployment, ordered by execution order.
/// This replaces the old deployment stages endpoint.
#[utoipa::path(
    get,
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
    Path((_project_id, deployment_id)): Path<(i32, i32)>,
) -> Result<Json<DeploymentJobsResponse>, Problem> {
    permission_guard!(auth, DeploymentsRead);

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
    path = "/api/projects/{project_id}/deployments/{deployment_id}/jobs/{job_id}/logs",
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
    Path((_project_id, deployment_id, job_id)): Path<(i32, i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);

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
/// ```
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
    Path((_project_id, deployment_id, job_id)): Path<(i32, i32, String)>,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);

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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use futures::StreamExt;
    use std::sync::Arc;
    use temps_config::ConfigService;
    use temps_database::test_utils::TestDatabase;
    use temps_logs::{DockerLogService, LogService};
    use tokio::time::{timeout, Duration};
    use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};

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
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        temps_auth::AuthContext::new_session(user, temps_auth::Role::Admin)
    }

    #[tokio::test]
    async fn test_websocket_handler_end_to_end_with_server() {
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

        let log_service = Arc::new(LogService::new(temp_dir.clone()));

        // Create Docker client for logs
        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults().expect("Failed to connect to Docker"),
        );
        let docker_log_service = Arc::new(DockerLogService::new(docker.clone()));

        // Create a test ServerConfig
        let server_config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:0".to_string(),
                test_db.database_url.clone(),
                None,
                None,
            )
            .expect("Failed to create server config"),
        );
        let config_service = Arc::new(ConfigService::new(server_config, db.clone()));

        // Create a broadcast queue service
        let (job_sender, _job_receiver) = tokio::sync::broadcast::channel(100);
        let queue_service: Arc<dyn temps_core::JobQueue> =
            Arc::new(temps_queue::BroadcastQueueService::new(job_sender));

        // Create a Docker runtime
        let deployer: Arc<dyn temps_deployer::ContainerDeployer> = Arc::new(
            temps_deployer::docker::DockerRuntime::new(docker, false, "temps-test".to_string()),
        );

        let deployment_service = Arc::new(crate::services::services::DeploymentService::new(
            db.clone(),
            log_service.clone(),
            config_service,
            queue_service.clone(),
            docker_log_service,
            deployer,
        ));

        let cron_service = Arc::new(
            crate::services::database_cron_service::DatabaseCronConfigService::new(
                db.clone(),
                queue_service.clone(),
            ),
        );

        let app_state = Arc::new(AppState {
            deployment_service,
            log_service,
            cron_service,
        });

        // Create test data in database
        // 1. Create a test project
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            directory: Set("/tmp/test-project".to_string()),
            main_branch: Set("main".to_string()),
            project_type: Set(temps_entities::types::ProjectType::Server),
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
            upstreams: Set(serde_json::json!([])), // Empty upstreams array
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
            metadata: Set(serde_json::json!({})), // Empty metadata object
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

        // Pre-populate log files
        app_state
            .log_service
            .append_to_log(&job_log_id, "Job log line 1\n")
            .await
            .expect("Failed to write job log");
        app_state
            .log_service
            .append_to_log(&job_log_id, "Job log line 2\n")
            .await
            .expect("Failed to write job log");

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

        // Receive messages
        let mut messages = Vec::new();

        while let Some(result) = timeout(Duration::from_secs(2), ws_stream.next())
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
        assert_eq!(messages.len(), 2, "Should receive 2 log messages");

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

    #[tokio::test]
    async fn test_container_logs_by_id_websocket() {
        use axum::extract::Request;
        use axum::middleware;
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::{
            deployment_containers as containers, deployments, environments, projects,
        };

        // Setup test database and services
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db = test_db.connection_arc();

        let temp_dir = std::env::temp_dir().join(format!("test_ws_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("Failed to create temp dir");

        let log_service = Arc::new(LogService::new(temp_dir.clone()));

        // Create Docker client for logs
        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults().expect("Failed to connect to Docker"),
        );
        let docker_log_service = Arc::new(DockerLogService::new(docker.clone()));

        // Create ServerConfig
        let server_config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:0".to_string(),
                test_db.database_url.clone(),
                None,
                None,
            )
            .expect("Failed to create server config"),
        );
        let config_service = Arc::new(ConfigService::new(server_config, db.clone()));

        // Create queue service
        let (job_sender, _job_receiver) = tokio::sync::broadcast::channel(100);
        let queue_service: Arc<dyn temps_core::JobQueue> =
            Arc::new(temps_queue::BroadcastQueueService::new(job_sender));

        // Create deployer
        let deployer: Arc<dyn temps_deployer::ContainerDeployer> = Arc::new(
            temps_deployer::docker::DockerRuntime::new(docker, false, "temps-test".to_string()),
        );

        let deployment_service = Arc::new(crate::services::services::DeploymentService::new(
            db.clone(),
            log_service.clone(),
            config_service,
            queue_service.clone(),
            docker_log_service,
            deployer,
        ));

        let cron_service = Arc::new(
            crate::services::database_cron_service::DatabaseCronConfigService::new(
                db.clone(),
                queue_service.clone(),
            ),
        );

        let app_state = Arc::new(AppState {
            deployment_service,
            log_service: log_service.clone(),
            cron_service,
        });

        // Create test data
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            directory: Set("/tmp/test-project".to_string()),
            main_branch: Set("main".to_string()),
            project_type: Set(temps_entities::types::ProjectType::Server),
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
            upstreams: Set(serde_json::json!([])),
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
            metadata: Set(serde_json::json!({})),
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

        // Create a test container
        let container_id = "test-container-123";
        let now = chrono::Utc::now();
        let container = containers::ActiveModel {
            deployment_id: Set(deployment.id),
            container_id: Set(container_id.to_string()),
            container_name: Set("test-container".to_string()),
            container_port: Set(8080),
            image_name: Set(Some("nginx:latest".to_string())),
            status: Set(Some("running".to_string())),
            created_at: Set(now),
            deployed_at: Set(now),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test container");

        // Pre-populate container logs
        log_service
            .append_to_log(container_id, "Container log line 1\n")
            .await
            .expect("Failed to write container log");
        log_service
            .append_to_log(container_id, "Container log line 2\n")
            .await
            .expect("Failed to write container log");

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
            "✅ WebSocket connection established (status: {})",
            response.status()
        );

        // Receive messages
        let mut messages = Vec::new();

        while let Some(result) = timeout(Duration::from_secs(2), ws_stream.next())
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

        println!("✅ Received {} raw container log messages", messages.len());

        let _ = ws_stream.close(None).await;

        println!("✅ Container logs by ID WebSocket test completed");
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_filtered_container_logs_websocket() {
        use axum::extract::Request;
        use axum::middleware;
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::{
            deployment_containers as containers, deployments, environments, projects,
        };

        // Setup test database and services
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db = test_db.connection_arc();

        let temp_dir = std::env::temp_dir().join(format!("test_ws_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("Failed to create temp dir");

        let log_service = Arc::new(LogService::new(temp_dir.clone()));

        // Create Docker client
        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults().expect("Failed to connect to Docker"),
        );
        let docker_log_service = Arc::new(DockerLogService::new(docker.clone()));

        // Create ServerConfig
        let server_config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:0".to_string(),
                test_db.database_url.clone(),
                None,
                None,
            )
            .expect("Failed to create server config"),
        );
        let config_service = Arc::new(ConfigService::new(server_config, db.clone()));

        // Create queue service
        let (job_sender, _job_receiver) = tokio::sync::broadcast::channel(100);
        let queue_service: Arc<dyn temps_core::JobQueue> =
            Arc::new(temps_queue::BroadcastQueueService::new(job_sender));

        // Create deployer
        let deployer: Arc<dyn temps_deployer::ContainerDeployer> = Arc::new(
            temps_deployer::docker::DockerRuntime::new(docker, false, "temps-test".to_string()),
        );

        let deployment_service = Arc::new(crate::services::services::DeploymentService::new(
            db.clone(),
            log_service.clone(),
            config_service,
            queue_service.clone(),
            docker_log_service,
            deployer,
        ));

        let cron_service = Arc::new(
            crate::services::database_cron_service::DatabaseCronConfigService::new(
                db.clone(),
                queue_service.clone(),
            ),
        );

        let app_state = Arc::new(AppState {
            deployment_service,
            log_service: log_service.clone(),
            cron_service,
        });

        // Create test data
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            directory: Set("/tmp/test-project".to_string()),
            main_branch: Set("main".to_string()),
            project_type: Set(temps_entities::types::ProjectType::Server),
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
            upstreams: Set(serde_json::json!([])),
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
            metadata: Set(serde_json::json!({})),
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

        // Create multiple containers
        let now = chrono::Utc::now();
        let container1_id = "filtered-container-1";
        let _container1 = containers::ActiveModel {
            deployment_id: Set(deployment.id),
            container_id: Set(container1_id.to_string()),
            container_name: Set("web-container".to_string()),
            container_port: Set(8080),
            image_name: Set(Some("nginx:latest".to_string())),
            status: Set(Some("running".to_string())),
            created_at: Set(now),
            deployed_at: Set(now),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create container 1");

        let container2_id = "filtered-container-2";
        let _container2 = containers::ActiveModel {
            deployment_id: Set(deployment.id),
            container_id: Set(container2_id.to_string()),
            container_name: Set("db-container".to_string()),
            container_port: Set(5432),
            image_name: Set(Some("postgres:latest".to_string())),
            status: Set(Some("running".to_string())),
            created_at: Set(now),
            deployed_at: Set(now),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create container 2");

        // Pre-populate logs for both containers
        log_service
            .append_to_log(container1_id, "Web container log 1\n")
            .await
            .expect("Failed to write container 1 log");
        log_service
            .append_to_log(container2_id, "DB container log 1\n")
            .await
            .expect("Failed to write container 2 log");

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

        // Connect to WebSocket (all containers)
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
            "✅ WebSocket connection established (status: {})",
            response.status()
        );

        // Receive messages from all containers
        let mut messages = Vec::new();

        while let Some(result) = timeout(Duration::from_secs(2), ws_stream.next())
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

        // Verify we got logs from both containers - logs might come combined or separate
        println!("Total messages received: {}", messages.len());
        for (i, msg) in messages.iter().enumerate() {
            println!("Message {}: '{}'", i, msg);
        }

        assert!(!messages.is_empty(), "Should receive at least 1 message");

        // Check that both container logs are present (they might be in one message or separate)
        let all_logs = messages.join("");
        let has_web_log = all_logs.contains("Web container");
        let has_db_log = all_logs.contains("DB container");

        assert!(
            has_web_log,
            "Should receive web container logs. Got: '{}'",
            all_logs
        );
        assert!(
            has_db_log,
            "Should receive DB container logs. Got: '{}'",
            all_logs
        );

        println!(
            "✅ Received {} raw log messages from multiple containers",
            messages.len()
        );

        let _ = ws_stream.close(None).await;

        println!("✅ Filtered container logs WebSocket test completed");
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

        let deployer: Arc<dyn temps_deployer::ContainerDeployer> = Arc::new(
            temps_deployer::docker::DockerRuntime::new(docker, false, "temps-test".to_string()),
        );

        let deployment_service = Arc::new(crate::services::services::DeploymentService::new(
            db.clone(),
            log_service.clone(),
            config_service,
            queue_service.clone(),
            docker_log_service,
            deployer,
        ));

        let cron_service = Arc::new(
            crate::services::database_cron_service::DatabaseCronConfigService::new(
                db.clone(),
                queue_service.clone(),
            ),
        );

        Arc::new(AppState {
            deployment_service,
            log_service,
            cron_service,
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
            directory: Set("/tmp/test-project".to_string()),
            main_branch: Set("main".to_string()),
            project_type: Set(temps_entities::types::ProjectType::Server),
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
            upstreams: Set(serde_json::json!([])),
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
            metadata: Set(serde_json::json!({})),
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
            directory: Set("/tmp/test-project".to_string()),
            main_branch: Set("main".to_string()),
            project_type: Set(temps_entities::types::ProjectType::Server),
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
            upstreams: Set(serde_json::json!([])),
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
            metadata: Set(serde_json::json!({})),
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
            metadata: Set(serde_json::json!({})),
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
            directory: Set("/tmp/test-project".to_string()),
            main_branch: Set("main".to_string()),
            project_type: Set(temps_entities::types::ProjectType::Server),
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
            upstreams: Set(serde_json::json!([])),
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
            metadata: Set(serde_json::json!({})),
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
            directory: Set("/tmp/test-project".to_string()),
            main_branch: Set("main".to_string()),
            project_type: Set(temps_entities::types::ProjectType::Server),
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
            upstreams: Set(serde_json::json!([])),
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
            metadata: Set(serde_json::json!({})),
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
            directory: Set("/tmp/test-project".to_string()),
            main_branch: Set("main".to_string()),
            project_type: Set(temps_entities::types::ProjectType::Server),
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
            upstreams: Set(serde_json::json!([])),
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
            metadata: Set(serde_json::json!({})),
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
            directory: Set("/tmp/test-project".to_string()),
            main_branch: Set("main".to_string()),
            project_type: Set(temps_entities::types::ProjectType::Server),
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
            upstreams: Set(serde_json::json!([])),
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
            metadata: Set(serde_json::json!({})),
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
            directory: Set("/tmp/test-project".to_string()),
            main_branch: Set("main".to_string()),
            project_type: Set(temps_entities::types::ProjectType::Server),
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
            upstreams: Set(serde_json::json!([])),
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
            metadata: Set(serde_json::json!({})),
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
            directory: Set("/tmp/test-project".to_string()),
            main_branch: Set("main".to_string()),
            project_type: Set(temps_entities::types::ProjectType::Server),
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
            upstreams: Set(serde_json::json!([])),
            ..Default::default()
        }
        .insert(&*db)
        .await
        .expect("Failed to create test environment");

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
