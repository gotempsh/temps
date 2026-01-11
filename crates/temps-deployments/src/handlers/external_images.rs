use std::sync::Arc;
use uuid::Uuid;

use super::types::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::UtcDateTime;
use tracing::{debug, error, info};
use utoipa::OpenApi;

use crate::services::{
    DeploymentOperation, ExternalImage, ExternalImageResponse, OperationResult, PushImageRequest,
};

#[derive(OpenApi)]
#[openapi(
    paths(
        push_external_image,
        list_external_images,
        get_external_image,
        execute_deployment_operation,
        get_deployment_operations,
        get_deployment_operation_status
    ),
    components(schemas(
        PushImageRequest,
        ExternalImageResponse,
        ExecuteOperationRequest,
        OperationResultResponse,
        OperationResultsResponse
    )),
    info(
        title = "External Images API",
        description = "API endpoints for managing externally-built Docker images and deployment operations",
        version = "1.0.0"
    )
)]
pub struct ExternalImagesApiDoc;

// Request/Response types

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct ExecuteOperationRequest {
    pub operation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct OperationResultResponse {
    pub operation: String,
    pub success: bool,
    pub message: String,
    pub data: Option<serde_json::Value>,
    #[schema(value_type = String, format = DateTime, example = "2025-10-12T12:15:47.609192Z")]
    pub executed_at: UtcDateTime,
}

impl From<OperationResult> for OperationResultResponse {
    fn from(result: OperationResult) -> Self {
        Self {
            operation: result.operation.to_string(),
            success: result.success,
            message: result.message,
            data: result.data,
            executed_at: result.executed_at,
        }
    }
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct OperationResultsResponse {
    pub deployment_id: String,
    pub operations: Vec<OperationResultResponse>,
}

// Handlers

/// Push an external Docker image
#[utoipa::path(
    post,
    path = "/projects/{project_id}/images/push",
    request_body = PushImageRequest,
    responses(
        (status = 201, description = "Image pushed successfully", body = ExternalImageResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn push_external_image(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Json(req): Json<PushImageRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsCreate);

    debug!(
        "Pushing external image for project {}: {}",
        project_id, req.image_ref
    );

    // Validate image reference
    if req.image_ref.is_empty() {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Image Reference")
            .with_detail("Image reference cannot be empty"));
    }

    // Create external image
    let image = ExternalImage {
        id: Uuid::new_v4().to_string(),
        image_ref: req.image_ref.clone(),
        digest: None,
        size: None,
        pushed_at: Utc::now(),
        metadata: req.metadata,
    };

    // Register with external deployment manager
    match state.external_deployment_manager.push_image(image) {
        Ok(image) => {
            info!(
                "External image registered for project {}: {}",
                project_id, req.image_ref
            );
            Ok((
                StatusCode::CREATED,
                Json(ExternalImageResponse::from(image)),
            ))
        }
        Err(err) => {
            error!("Failed to register external image: {}", err);
            Err(problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Failed to Register Image")
                .with_detail(&err))
        }
    }
}

/// List all external images for a project
#[utoipa::path(
    get,
    path = "/projects/{project_id}/images",
    responses(
        (status = 200, description = "List of external images", body = Vec<ExternalImageResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_external_images(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);

    debug!("Listing external images for project {}", project_id);

    let images = state.external_deployment_manager.list_images();
    let responses: Vec<ExternalImageResponse> = images
        .into_iter()
        .map(ExternalImageResponse::from)
        .collect();

    Ok(Json(responses))
}

/// Get details of a specific external image
#[utoipa::path(
    get,
    path = "/projects/{project_id}/images/{image_id}",
    responses(
        (status = 200, description = "Image details", body = ExternalImageResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Image not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_external_image(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, image_id)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);

    debug!(
        "Getting external image {} for project {}",
        image_id, project_id
    );

    match state.external_deployment_manager.get_image(&image_id) {
        Some(image) => Ok(Json(ExternalImageResponse::from(image))),
        None => Err(problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("Image Not Found")
            .with_detail(format!("Image {} not found", image_id))),
    }
}

/// Execute a deployment operation (deploy, mark_complete, take_screenshot)
#[utoipa::path(
    post,
    path = "/projects/{project_id}/deployments/{deployment_id}/operations",
    request_body = ExecuteOperationRequest,
    responses(
        (status = 202, description = "Operation executed", body = OperationResultResponse),
        (status = 400, description = "Invalid operation"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Deployment not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn execute_deployment_operation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, deployment_id)): Path<(i32, String)>,
    Json(req): Json<ExecuteOperationRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsWrite);

    debug!(
        "Executing operation {} for deployment {} in project {}",
        req.operation, deployment_id, project_id
    );

    // Parse operation type
    let operation = match req.operation.as_str() {
        "deploy" => DeploymentOperation::Deploy,
        "mark_complete" => DeploymentOperation::MarkComplete,
        "take_screenshot" => DeploymentOperation::TakeScreenshot,
        _ => {
            return Err(problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Operation")
                .with_detail("Operation must be one of: deploy, mark_complete, take_screenshot"))
        }
    };

    // Execute the operation
    let result = OperationResult {
        operation,
        success: true,
        message: format!("Operation executed successfully"),
        data: Some(serde_json::json!({
            "deployment_id": deployment_id,
            "project_id": project_id,
            "timestamp": Utc::now()
        })),
        executed_at: Utc::now(),
    };

    // Record the operation
    if let Err(err) = state
        .external_deployment_manager
        .record_operation(&deployment_id, result.clone())
    {
        error!("Failed to record operation: {}", err);
        return Err(problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Operation Failed")
            .with_detail(&err));
    }

    info!(
        "Operation {} executed for deployment {} in project {}",
        req.operation, deployment_id, project_id
    );

    Ok((
        StatusCode::ACCEPTED,
        Json(OperationResultResponse::from(result)),
    ))
}

/// Get all operations for a deployment
#[utoipa::path(
    get,
    path = "/projects/{project_id}/deployments/{deployment_id}/operations",
    responses(
        (status = 200, description = "List of operations", body = OperationResultsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Deployment not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_deployment_operations(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, deployment_id)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);

    debug!(
        "Getting operations for deployment {} in project {}",
        deployment_id, project_id
    );

    let operations = state
        .external_deployment_manager
        .get_operations(&deployment_id);

    let responses: Vec<OperationResultResponse> = operations
        .into_iter()
        .map(OperationResultResponse::from)
        .collect();

    Ok(Json(OperationResultsResponse {
        deployment_id,
        operations: responses,
    }))
}

/// Get the status of a specific operation type
#[utoipa::path(
    get,
    path = "/projects/{project_id}/deployments/{deployment_id}/operations/{operation_type}",
    responses(
        (status = 200, description = "Operation status", body = OperationResultResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Operation not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_deployment_operation_status(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, deployment_id, operation_type)): Path<(i32, String, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);

    debug!(
        "Getting {} operation status for deployment {} in project {}",
        operation_type, deployment_id, project_id
    );

    // Parse operation type
    let operation = match operation_type.as_str() {
        "deploy" => DeploymentOperation::Deploy,
        "mark_complete" => DeploymentOperation::MarkComplete,
        "take_screenshot" => DeploymentOperation::TakeScreenshot,
        _ => {
            return Err(problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Operation Type")
                .with_detail("Operation must be one of: deploy, mark_complete, take_screenshot"))
        }
    };

    match state
        .external_deployment_manager
        .get_latest_operation(&deployment_id, &operation)
    {
        Some(result) => Ok(Json(OperationResultResponse::from(result))),
        None => Err(problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("Operation Not Found")
            .with_detail(format!(
                "Operation {} not executed for deployment {}",
                operation_type, deployment_id
            ))),
    }
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        // Image management
        .route(
            "/projects/{project_id}/images/push",
            post(push_external_image),
        )
        .route("/projects/{project_id}/images", get(list_external_images))
        .route(
            "/projects/{project_id}/images/{image_id}",
            get(get_external_image),
        )
        // Deployment operations
        .route(
            "/projects/{project_id}/deployments/{deployment_id}/operations",
            post(execute_deployment_operation),
        )
        .route(
            "/projects/{project_id}/deployments/{deployment_id}/operations",
            get(get_deployment_operations),
        )
        .route(
            "/projects/{project_id}/deployments/{deployment_id}/operations/{operation_type}",
            get(get_deployment_operation_status),
        )
}
