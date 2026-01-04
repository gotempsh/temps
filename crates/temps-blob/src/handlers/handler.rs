//! HTTP handlers for Blob service

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Extension, Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{delete, get, head, post},
    Json, Router,
};
use bytes::Bytes;
use futures::TryStreamExt;
use std::collections::HashMap;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::{Problem, ProblemDetails};
use temps_core::RequestMetadata;
use temps_providers::externalsvc::{ExternalService, ServiceType};
use temps_providers::CreateExternalServiceRequest;
use tracing::{error, info};
use utoipa::OpenApi;

use super::audit::{AuditContext, BlobServiceDisabledAudit, BlobServiceEnabledAudit};

use super::types::*;
use crate::services::{ListOptions, PutOptions};

/// Extract project_id from request or authentication context
///
/// Priority:
/// 1. Deployment tokens: Use project_id from token (request value ignored for security)
/// 2. API keys/sessions: Use project_id from request (required)
fn extract_project_id(
    auth: &temps_auth::AuthContext,
    request_project_id: Option<i32>,
) -> Result<i32, Problem> {
    // For deployment tokens, always use the token's project_id (security: prevent access to other projects)
    if let Some(token_project_id) = auth.project_id() {
        return Ok(token_project_id);
    }

    // For API keys and sessions, require project_id in the request
    request_project_id.ok_or_else(|| {
        temps_core::problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Project ID Required")
            .with_detail("The 'project_id' field is required for API key or session authentication")
    })
}

/// OpenAPI documentation for Blob API
#[derive(OpenApi)]
#[openapi(
    paths(
        blob_put,
        blob_delete,
        blob_list,
        blob_head,
        blob_download,
        blob_copy,
        blob_status,
        blob_enable,
        blob_disable,
    ),
    components(
        schemas(
            BlobResponse,
            DeleteBlobRequest,
            DeleteBlobResponse,
            CopyBlobRequest,
            ListBlobsQuery,
            ListBlobsResponse,
            BlobStatusResponse,
            EnableBlobRequest,
            EnableBlobResponse,
            DisableBlobResponse,
        )
    ),
    tags(
        (name = "Blob", description = "Blob storage operations"),
        (name = "Blob Management", description = "Blob service management operations")
    )
)]
pub struct BlobApiDoc;

/// Configure blob routes
pub fn configure_routes() -> Router<Arc<BlobAppState>> {
    Router::new()
        // Data operations
        .route("/blob", post(blob_put))
        .route("/blob", delete(blob_delete))
        .route("/blob", get(blob_list))
        .route("/blob/copy", post(blob_copy))
        .route("/blob/{project_id}/{*path}", head(blob_head))
        .route("/blob/{project_id}/{*path}", get(blob_download))
        // Management operations
        .route("/blob/status", get(blob_status))
        .route("/blob/enable", post(blob_enable))
        .route("/blob/disable", delete(blob_disable))
}

/// Upload a blob
#[utoipa::path(
    tag = "Blob",
    post,
    path = "/blob",
    request_body(content = String, content_type = "application/octet-stream", description = "Binary blob data"),
    responses(
        (status = 201, description = "Blob uploaded successfully", body = BlobResponse),
        (status = 400, description = "Invalid request", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn blob_put(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BlobAppState>>,
    Query(query): Query<PutBlobQuery>,
    body: Bytes,
) -> Result<impl IntoResponse, Problem> {
    // Get project ID from query or auth context
    let project_id = extract_project_id(&auth, query.project_id)?;

    // Use pathname from query or default
    let pathname = query.pathname.as_deref().unwrap_or("upload");
    let options = PutOptions {
        content_type: query.content_type,
        add_random_suffix: query.add_random_suffix,
    };

    let blob_info = state
        .blob_service
        .put(project_id, pathname, body, options)
        .await?;

    Ok((StatusCode::CREATED, Json(BlobResponse::from(blob_info))))
}

/// Delete blobs
#[utoipa::path(
    tag = "Blob",
    delete,
    path = "/blob",
    request_body = DeleteBlobRequest,
    responses(
        (status = 200, description = "Blobs deleted successfully", body = DeleteBlobResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn blob_delete(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BlobAppState>>,
    Json(request): Json<DeleteBlobRequest>,
) -> Result<impl IntoResponse, Problem> {
    let project_id = extract_project_id(&auth, request.project_id)?;

    let deleted = state
        .blob_service
        .del(project_id, request.pathnames)
        .await?;

    Ok(Json(DeleteBlobResponse { deleted }))
}

/// List blobs
#[utoipa::path(
    tag = "Blob",
    get,
    path = "/blob",
    params(
        ("limit" = Option<i32>, Query, description = "Maximum number of items to return"),
        ("prefix" = Option<String>, Query, description = "Prefix to filter by"),
        ("cursor" = Option<String>, Query, description = "Continuation token for pagination"),
    ),
    responses(
        (status = 200, description = "List of blobs", body = ListBlobsResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn blob_list(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BlobAppState>>,
    Query(query): Query<ListBlobsQuery>,
) -> Result<impl IntoResponse, Problem> {
    let project_id = extract_project_id(&auth, query.project_id)?;

    let options = ListOptions {
        limit: query.limit,
        prefix: query.prefix,
        cursor: query.cursor,
    };

    let result = state.blob_service.list(project_id, options).await?;

    Ok(Json(ListBlobsResponse::from(result)))
}

/// Copy a blob to a new location
#[utoipa::path(
    tag = "Blob",
    post,
    path = "/blob/copy",
    request_body = CopyBlobRequest,
    responses(
        (status = 200, description = "Blob copied successfully", body = BlobResponse),
        (status = 400, description = "Invalid request", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 404, description = "Source blob not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn blob_copy(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BlobAppState>>,
    Json(request): Json<CopyBlobRequest>,
) -> Result<impl IntoResponse, Problem> {
    let project_id = extract_project_id(&auth, request.project_id)?;

    // Extract pathname from URL (handles both full URLs and relative paths)
    let from_pathname = extract_pathname_from_url(&request.from_url);

    let blob_info = state
        .blob_service
        .copy(project_id, &from_pathname, &request.to_pathname)
        .await?;

    Ok(Json(BlobResponse::from(blob_info)))
}

/// Extract pathname from a blob URL or path
/// Handles formats like:
/// - "/api/blob/10/images/avatar.png" -> "images/avatar.png"
/// - "images/avatar.png" -> "images/avatar.png"
/// - "http://example.com/api/blob/10/images/avatar.png" -> "images/avatar.png"
fn extract_pathname_from_url(url: &str) -> String {
    let mut path = url.to_string();

    // If it's a full URL, extract just the path
    if let Some(pos) = path.find("://") {
        if let Some(slash_pos) = path[pos + 3..].find('/') {
            path = path[pos + 3 + slash_pos..].to_string();
        }
    }

    // Remove /api/blob/ prefix if present
    if path.starts_with("/api/blob/") {
        path = path["/api/blob/".len()..].to_string();
    }

    // Remove leading slash
    if path.starts_with('/') {
        path = path[1..].to_string();
    }

    // Remove project_id prefix if present (e.g., "10/images/avatar.png" -> "images/avatar.png")
    if let Some(slash_pos) = path.find('/') {
        let potential_project_id = &path[..slash_pos];
        if potential_project_id.chars().all(|c| c.is_ascii_digit()) {
            path = path[slash_pos + 1..].to_string();
        }
    }

    path
}

/// Path parameters for blob operations
#[derive(Debug, serde::Deserialize)]
struct BlobPathParams {
    project_id: i32,
    path: String,
}

/// Get blob metadata
#[utoipa::path(
    tag = "Blob",
    head,
    path = "/blob/{project_id}/{path}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("path" = String, Path, description = "Blob path"),
    ),
    responses(
        (status = 200, description = "Blob metadata in headers"),
        (status = 404, description = "Blob not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn blob_head(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BlobAppState>>,
    Path(params): Path<BlobPathParams>,
) -> Result<impl IntoResponse, Problem> {
    // For deployment tokens, verify the token's project matches the path
    // For API keys/sessions, use the project_id from the path (admins can access any project)
    let project_id = if let Some(token_project_id) = auth.project_id() {
        // Deployment token: must match path
        if token_project_id != params.project_id {
            return Err(temps_core::problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Access Denied")
                .with_detail("You do not have access to this project's blobs"));
        }
        token_project_id
    } else {
        // API key/session: use path parameter
        params.project_id
    };

    let blob_info = state.blob_service.head(project_id, &params.path).await?;

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, blob_info.content_type),
            (header::CONTENT_LENGTH, blob_info.size.to_string()),
            (
                header::LAST_MODIFIED,
                blob_info
                    .uploaded_at
                    .format("%a, %d %b %Y %H:%M:%S GMT")
                    .to_string(),
            ),
        ],
    ))
}

/// Download a blob
#[utoipa::path(
    tag = "Blob",
    get,
    path = "/blob/{project_id}/{path}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("path" = String, Path, description = "Blob path"),
    ),
    responses(
        (status = 200, description = "Blob content"),
        (status = 404, description = "Blob not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn blob_download(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BlobAppState>>,
    Path(params): Path<BlobPathParams>,
) -> Result<impl IntoResponse, Problem> {
    // For deployment tokens, verify the token's project matches the path
    // For API keys/sessions, use the project_id from the path (admins can access any project)
    let project_id = if let Some(token_project_id) = auth.project_id() {
        // Deployment token: must match path
        if token_project_id != params.project_id {
            return Err(temps_core::problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Access Denied")
                .with_detail("You do not have access to this project's blobs"));
        }
        token_project_id
    } else {
        // API key/session: use path parameter
        params.project_id
    };

    let (stream, content_type, size) = state
        .blob_service
        .download(project_id, &params.path)
        .await?;

    // Convert the stream to axum Body
    let body =
        Body::from_stream(stream.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)));

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::CONTENT_LENGTH, size.to_string()),
        ],
        body,
    ))
}

// =============================================================================
// Management Handlers
// =============================================================================

/// Get Blob service status
#[utoipa::path(
    tag = "Blob Management",
    get,
    path = "/blob/status",
    responses(
        (status = 200, description = "Blob service status", body = BlobStatusResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn blob_status(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BlobAppState>>,
) -> Result<impl IntoResponse, Problem> {
    // Status check requires SystemRead permission
    permission_guard!(auth, SystemRead);

    // Check if the service exists in the database via ExternalServiceManager
    let service_result = state
        .external_service_manager
        .get_service_by_name("temps-blob")
        .await;

    match service_result {
        Ok(service) => {
            // Service exists in database, get full details
            let details = state
                .external_service_manager
                .get_service_details(service.id)
                .await
                .ok();

            let healthy = service.status == "running";
            let version = details.as_ref().and_then(|d| d.service.version.clone());
            let docker_image = details
                .and_then(|d| d.current_parameters)
                .and_then(|p| p.get("docker_image").cloned())
                .and_then(|v| v.as_str().map(String::from));

            Ok(Json(BlobStatusResponse {
                enabled: true,
                healthy,
                version,
                docker_image,
            }))
        }
        Err(_) => {
            // Service not found in database - not enabled
            Ok(Json(BlobStatusResponse {
                enabled: false,
                healthy: false,
                version: None,
                docker_image: None,
            }))
        }
    }
}

/// Enable Blob service
#[utoipa::path(
    tag = "Blob Management",
    post,
    path = "/blob/enable",
    request_body = EnableBlobRequest,
    responses(
        (status = 200, description = "Blob service enabled", body = EnableBlobResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn blob_enable(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BlobAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<EnableBlobRequest>,
) -> Result<impl IntoResponse, Problem> {
    // Enable requires SystemAdmin permission
    permission_guard!(auth, SystemAdmin);

    info!("Enabling Blob service with config: {:?}", request);

    // Build parameters for S3/Blob service creation
    let mut parameters: HashMap<String, serde_json::Value> = HashMap::new();
    if let Some(docker_image) = &request.docker_image {
        parameters.insert("docker_image".to_string(), serde_json::json!(docker_image));
    }
    if let Some(root_user) = &request.root_user {
        parameters.insert("access_key".to_string(), serde_json::json!(root_user));
    }
    if let Some(root_password) = &request.root_password {
        parameters.insert("secret_key".to_string(), serde_json::json!(root_password));
    }

    // Extract version from docker_image (e.g., "minio/minio:RELEASE.2025-01-01" -> "RELEASE.2025-01-01")
    let version = parameters
        .get("docker_image")
        .and_then(|v| v.as_str())
        .and_then(|img| img.split(':').nth(1))
        .map(String::from)
        .or_else(|| Some("latest".to_string())); // Default version

    // Create service request for ExternalServiceManager
    // Using S3 service type since Blob is S3-compatible storage
    let create_request = CreateExternalServiceRequest {
        name: "temps-blob".to_string(),
        service_type: ServiceType::S3,
        version,
        parameters,
    };

    // Create the service through ExternalServiceManager
    // This creates the database record AND initializes/starts the container
    let service_info = state
        .external_service_manager
        .create_service(create_request)
        .await
        .map_err(|e| {
            error!("Failed to create Blob service: {}", e);
            temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Failed to Enable Blob Service")
                .with_detail(format!("Could not create S3/Blob service: {}", e))
        })?;

    // Get status from the created service
    let healthy = service_info.status == "running";
    let version = service_info.version.clone();

    // Get docker image from service details
    let docker_image = state
        .external_service_manager
        .get_service_details(service_info.id)
        .await
        .ok()
        .and_then(|details| details.current_parameters)
        .and_then(|p| p.get("docker_image").cloned())
        .and_then(|v| v.as_str().map(String::from));

    // Create audit log
    let audit = BlobServiceEnabledAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        service_name: "temps-blob".to_string(),
        docker_image: docker_image.clone(),
        version: version.clone(),
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(Json(EnableBlobResponse {
        success: true,
        message: "Blob service enabled successfully".to_string(),
        status: BlobStatusResponse {
            enabled: true,
            healthy,
            version,
            docker_image,
        },
    }))
}

/// Disable Blob service
#[utoipa::path(
    tag = "Blob Management",
    delete,
    path = "/blob/disable",
    responses(
        (status = 200, description = "Blob service disabled", body = DisableBlobResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn blob_disable(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BlobAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    // Disable requires SystemAdmin permission
    permission_guard!(auth, SystemAdmin);

    info!("Disabling Blob service");

    // Stop the RustFS service
    if let Err(e) = state.rustfs_service.stop().await {
        error!("Failed to stop Blob service: {}", e);
        return Err(
            temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Failed to Disable Blob Service")
                .with_detail(format!("Could not stop RustFS container: {}", e)),
        );
    }

    // Create audit log
    let audit = BlobServiceDisabledAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        service_name: "temps-blob".to_string(),
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(Json(DisableBlobResponse {
        success: true,
        message: "Blob service disabled successfully".to_string(),
    }))
}
