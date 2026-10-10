// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Arc;
use uuid::Uuid;

use super::audit::{DeploymentOperationAudit, ExternalImagePushedAudit};
use super::types::AppState;
use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem, ProblemDetails};
use temps_core::{AuditContext, RequestMetadata, UtcDateTime};
use tracing::{debug, error, info};
use utoipa::OpenApi;

use crate::services::{
    DeploymentOperation, DeploymentOperationDetails, DeploymentScreenshotCapture, ExternalImage,
    OperationResult, OperationStatus, PushImageRequest, PushedExternalImageResponse,
    ScreenshotOperationError, SCREENSHOT_SETTINGS_PATH,
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
        PushedExternalImageResponse,
        ExecuteOperationRequest,
        OperationResultResponse,
        OperationResultsResponse,
        OperationStatus,
        DeploymentOperationDetails,
        DeploymentScreenshotCapture
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
    /// `pending` while background work (a screenshot capture) runs, then
    /// `completed` or `failed`. Poll
    /// `GET …/operations/{operation_type}` for the outcome.
    pub status: OperationStatus,
    /// `true` only when `status` is `completed`.
    pub success: bool,
    pub message: String,
    /// The project and deployment the record belongs to, plus the stored
    /// image once a `take_screenshot` has completed.
    pub data: DeploymentOperationDetails,
    #[schema(value_type = String, format = DateTime, example = "2025-10-12T12:15:47.609192Z")]
    pub executed_at: UtcDateTime,
}

impl From<OperationResult> for OperationResultResponse {
    fn from(result: OperationResult) -> Self {
        Self {
            operation: result.operation.to_string(),
            status: result.status,
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
    tag = "External Images",
    path = "/projects/{project_id}/images/push",
    request_body = PushImageRequest,
    responses(
        (status = 201, description = "Image pushed successfully", body = PushedExternalImageResponse),
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
    Extension(metadata): Extension<RequestMetadata>,
    Json(req): Json<PushImageRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsCreate);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

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

            let audit = ExternalImagePushedAudit {
                context: AuditContext {
                    user_id: auth.user_id(),
                    ip_address: Some(metadata.ip_address.clone()),
                    user_agent: metadata.user_agent.clone(),
                },
                project_id,
                image_ref: req.image_ref,
            };
            if let Err(e) = state.audit_service.create_audit_log(&audit).await {
                error!("Failed to create audit log: {}", e);
            }

            Ok((
                StatusCode::CREATED,
                Json(PushedExternalImageResponse::from(image)),
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
    tag = "External Images",
    path = "/projects/{project_id}/images",
    responses(
        (status = 200, description = "List of external images", body = Vec<PushedExternalImageResponse>),
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
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    debug!("Listing external images for project {}", project_id);

    let images = state.external_deployment_manager.list_images();
    let responses: Vec<PushedExternalImageResponse> = images
        .into_iter()
        .map(PushedExternalImageResponse::from)
        .collect();

    Ok(Json(responses))
}

/// Get details of a specific external image
#[utoipa::path(
    get,
    tag = "External Images",
    path = "/projects/{project_id}/images/{image_id}",
    responses(
        (status = 200, description = "Image details", body = PushedExternalImageResponse),
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
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    debug!(
        "Getting external image {} for project {}",
        image_id, project_id
    );

    match state.external_deployment_manager.get_image(&image_id) {
        Some(image) => Ok(Json(PushedExternalImageResponse::from(image))),
        None => Err(problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("Image Not Found")
            .with_detail(format!("Image {} not found", image_id))),
    }
}

/// Execute a deployment operation (deploy, mark_complete, take_screenshot)
#[utoipa::path(
    post,
    tag = "Deployments",
    path = "/projects/{project_id}/deployments/{deployment_id}/operations",
    request_body = ExecuteOperationRequest,
    responses(
        (status = 202, description = "Operation accepted. `take_screenshot` returns `status: pending`; poll the operation status for `completed` (with `screenshot_location`) or `failed` (with the reason)", body = OperationResultResponse),
        (status = 400, description = "Invalid operation or deployment ID", body = ProblemDetails),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Deployment not found in this project", body = ProblemDetails),
        (status = 409, description = "Screenshots are disabled (see `setup_path`), or a capture of this deployment is already running", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
        (status = 503, description = "The screenshot provider is unavailable; `detail` gives the reason and `setup_path` where to change it", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn execute_deployment_operation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, deployment_id)): Path<(i32, String)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(req): Json<ExecuteOperationRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let deployment_id = operation_deployment_key(&deployment_id);

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

    let result = if operation == DeploymentOperation::TakeScreenshot {
        let deployment_number: i32 = deployment_id.parse().map_err(|_| {
            problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Deployment ID")
                .with_detail(format!(
                    "Deployment ID '{}' in project {} is not a number",
                    deployment_id, project_id
                ))
        })?;
        // Starts the capture in the background; the response is `pending`.
        state
            .screenshot_operations
            .start(project_id, deployment_number)
            .await?
    } else {
        let result = legacy_operation_record(operation, project_id, &deployment_id);
        record_legacy_operation(&state, project_id, &deployment_id, &result)?;
        result
    };

    let audit = DeploymentOperationAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        deployment_id: deployment_id.clone(),
        operation: req.operation.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
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

/// Operation-history key for a deployment ID taken from the path. Numeric IDs
/// are put in canonical form (`007` and `+7` become `7`), matching how
/// `take_screenshot` parses them, so a record written by one request is found
/// by every later one however the ID was spelled. Other IDs are kept as-is.
fn operation_deployment_key(deployment_id: &str) -> String {
    deployment_id
        .parse::<i32>()
        .map(|id| id.to_string())
        .unwrap_or_else(|_| deployment_id.to_string())
}

/// `deploy` and `mark_complete` only record that they were requested; they
/// perform no work through this endpoint.
fn legacy_operation_record(
    operation: DeploymentOperation,
    project_id: i32,
    deployment_id: &str,
) -> OperationResult {
    OperationResult {
        operation,
        status: OperationStatus::Completed,
        success: true,
        message: "Operation executed successfully".to_string(),
        data: DeploymentOperationDetails::new(project_id, deployment_id),
        executed_at: Utc::now(),
    }
}

fn record_legacy_operation(
    state: &AppState,
    project_id: i32,
    deployment_id: &str,
    result: &OperationResult,
) -> Result<(), Problem> {
    state
        .external_deployment_manager
        .record_operation(project_id, deployment_id, result.clone())
        .map_err(|err| {
            error!(
                "Failed to record operation {} for deployment {} in project {}: {}",
                result.operation, deployment_id, project_id, err
            );
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Operation Failed")
                .with_detail(&err)
        })
}

impl From<ScreenshotOperationError> for Problem {
    fn from(error: ScreenshotOperationError) -> Self {
        match error {
            ScreenshotOperationError::DeploymentNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Deployment Not Found")
                    .with_detail(error.to_string())
            }
            ScreenshotOperationError::Disabled { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Screenshots Disabled")
                .with_detail(error.to_string())
                .with_value("setup_path", SCREENSHOT_SETTINGS_PATH),
            ScreenshotOperationError::ProviderUnavailable { .. } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("Screenshot Provider Unavailable")
                    .with_detail(error.to_string())
                    .with_value("setup_path", SCREENSHOT_SETTINGS_PATH)
            }
            ScreenshotOperationError::AlreadyRunning { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Screenshot Already In Progress")
                    .with_detail(error.to_string())
            }
            ScreenshotOperationError::Record { .. } | ScreenshotOperationError::Database { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Screenshot Operation Failed")
                    .with_detail(error.to_string())
            }
        }
    }
}

/// Get all operations for a deployment
#[utoipa::path(
    get,
    tag = "Deployments",
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
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let deployment_id = operation_deployment_key(&deployment_id);

    debug!(
        "Getting operations for deployment {} in project {}",
        deployment_id, project_id
    );

    let operations = state
        .external_deployment_manager
        .get_operations(project_id, &deployment_id);

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
    tag = "Deployments",
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
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let deployment_id = operation_deployment_key(&deployment_id);

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

    match state.external_deployment_manager.get_latest_operation(
        project_id,
        &deployment_id,
        &operation,
    ) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::ExternalDeploymentManager;

    #[test]
    fn numeric_deployment_ids_have_one_canonical_key() {
        assert_eq!(operation_deployment_key("7"), "7");
        assert_eq!(operation_deployment_key("007"), "7");
        assert_eq!(operation_deployment_key("+7"), "7");
        assert_eq!(operation_deployment_key("ext-image-1"), "ext-image-1");
    }

    #[test]
    fn an_operation_started_with_a_padded_id_can_be_polled_with_it() {
        let manager = ExternalDeploymentManager::new();
        let written_as = operation_deployment_key("007");
        manager
            .record_operation(
                3,
                &written_as,
                legacy_operation_record(DeploymentOperation::MarkComplete, 3, &written_as),
            )
            .unwrap();

        for spelling in ["007", "7"] {
            let key = operation_deployment_key(spelling);
            assert!(
                manager
                    .get_latest_operation(3, &key, &DeploymentOperation::MarkComplete)
                    .is_some(),
                "polling with {spelling:?} must find the record"
            );
            assert_eq!(manager.get_operations(3, &key).len(), 1);
        }
    }
}
