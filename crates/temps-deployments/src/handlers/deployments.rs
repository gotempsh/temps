use std::sync::Arc;

use super::types::AppState;
use axum::Router;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        sse::{Event, Sse},
        IntoResponse,
    },
    routing::{delete, get, post},
    Json,
};
use futures::stream::StreamExt;
use temps_auth::permission_guard;
use temps_auth::RequireAuth;
use tracing::{debug, error, info};
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

    let _deployment = state
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

    let _deployment = state
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

    let container_responses: Vec<ContainerInfoResponse> = containers
        .into_iter()
        .map(Into::into)
        .collect();

    let total = container_responses.len();
    let response = ContainerListResponse {
        containers: container_responses,
        total,
    };

    Ok(Json(response))
}

/// Get logs for a specific container by container ID
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
        (status = 200, description = "Server-Sent Events stream of container logs"),
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
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);

    info!(
        "Getting logs for container {} in environment {} of project: {}",
        container_id, environment_id, project_id
    );

    let deployment_service = state.deployment_service.clone();
    let start_date = query.start_date;
    let end_date = query.end_date;
    let tail = query.tail.clone();

    // Get the log stream from the deployment service
    let log_stream = deployment_service
        .get_container_logs_by_id(project_id, environment_id, container_id, start_date, end_date, tail)
        .await
        .map_err(|e| {
            error!("Failed to get container logs: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Failed to get container logs")
                .with_detail(e.to_string())
        })?;

    // Convert the log stream to SSE events
    let event_stream = log_stream.map(|log_result| {
        match log_result {
            Ok(line) => Ok::<_, axum::BoxError>(Event::default().data(line)),
            Err(e) => {
                error!("Error reading log line: {}", e);
                Ok(Event::default().data(format!("Error: {}", e)))
            }
        }
    });

    Ok(Sse::new(event_stream))
}

/// Get logs for a container in an environment
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
        (status = 200, description = "Server-Sent Events stream of container logs"),
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
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);

    info!(
        "Getting container logs for environment {} of project: {}",
        environment_id, project_id
    );

    let deployment_service = state.deployment_service.clone();
    let start_date = query.start_date;
    let end_date = query.end_date;
    let tail = query.tail.clone();
    let container_name = query.container_name.clone();

    // Get the log stream from the deployment service
    let log_stream = deployment_service
        .get_filtered_container_logs(project_id, environment_id, start_date, end_date, tail, container_name)
        .await
        .map_err(|e| {
            error!("Failed to get container logs: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Failed to get container logs")
                .with_detail(e.to_string())
        })?;

    // Convert the log stream to SSE events
    let event_stream = log_stream.map(|log_result| {
        match log_result {
            Ok(line) => Ok::<_, axum::BoxError>(Event::default().data(line)),
            Err(e) => {
                error!("Error reading log line: {}", e);
                Ok(Event::default().data(format!("Error: {}", e)))
            }
        }
    });

    Ok(Sse::new(event_stream))
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

/// Tail logs for a specific deployment job in real-time via Server-Sent Events
#[utoipa::path(
    get,
    path = "/projects/{project_id}/deployments/{deployment_id}/jobs/{job_id}/logs/tail",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("deployment_id" = i32, Path, description = "Deployment ID"),
        ("job_id" = String, Path, description = "Job ID")
    ),
    responses(
        (status = 200, description = "Stream of deployment job logs", content_type = "text/event-stream"),
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
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);

    debug!(
        "Tailing logs for job {} in deployment {}",
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

    // Get the log stream from the log service
    let stream = match state.log_service.tail_log(&log_id).await {
        Ok(stream) => stream,
        Err(e) => {
            error!("Error tailing job logs: {:?}", e);
            return Err(problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Failed to tail job logs")
                .with_detail(format!("Error streaming logs: {}", e)));
        }
    };

    // Convert the log stream to SSE events
    let event_stream = stream.map(|line| match line {
        Ok(data) => {
            // Strip newlines and carriage returns from the log line
            // SSE doesn't allow newlines in field values
            let cleaned_data = data.trim_end_matches('\n').trim_end_matches('\r');

            Ok::<Event, std::io::Error>(
                Event::default()
                    .json_data(serde_json::json!({
                        "log": cleaned_data
                    }))
                    .expect("Failed to serialize log data")
            )
        },
        Err(e) => {
            error!("Error reading log line: {:?}", e);
            Ok::<Event, std::io::Error>(Event::default().data("Error reading log line"))
        }
    });

    Ok(Sse::new(event_stream).into_response())
}
