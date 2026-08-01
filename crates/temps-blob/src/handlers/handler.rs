//! HTTP handlers for Blob service

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Extension, Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{delete, get, head, patch, post},
    Json, Router,
};
use bytes::Bytes;
use futures::TryStreamExt;
use std::collections::HashMap;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::{Problem, ProblemDetails};
use temps_core::RequestMetadata;
use temps_providers::externalsvc::{ExternalService, ServiceType};
use temps_providers::{CreateExternalServiceRequest, UpdateExternalServiceRequest};
use tracing::{error, info};
use utoipa::OpenApi;

use super::audit::{
    AuditContext, BlobServiceDisabledAudit, BlobServiceEnabledAudit, BlobServiceUpdatedAudit,
};

use super::types::*;
use crate::services::{ListOptions, PutOptions};

/// Extract project_id from request or authentication context
///
/// Priority:
/// 1. Deployment tokens: Use project_id from token (request value ignored for security)
/// 2. API keys/sessions: Use project_id from request (required)
async fn extract_project_id(
    auth: &temps_auth::AuthContext,
    request_project_id: Option<i32>,
    project_access_checker: &Option<Arc<dyn temps_core::ProjectAccessChecker>>,
) -> Result<i32, Problem> {
    // For deployment tokens, always use the token's project_id (security: prevent access to other projects)
    if let Some(token_project_id) = auth.project_id() {
        return Ok(token_project_id);
    }

    // For API keys and sessions, require project_id in the request
    let project_id = request_project_id.ok_or_else(|| {
        temps_core::problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Project ID Required")
            .with_detail("The 'project_id' field is required for API key or session authentication")
    })?;

    // Confine session/API-key callers to projects they may access; see the KV
    // handler for the full rationale. No-op in plain OSS, enforced when a
    // team-access plugin registers a checker.
    authorize_project_access(auth, project_id, project_access_checker).await?;

    Ok(project_id)
}

/// Run the team-based project access guard for session/API-key callers.
///
/// Shared by the request-body handlers (via [`extract_project_id`]) and the
/// path-based `blob_head`/`blob_download` handlers, which resolve `project_id`
/// from the URL path rather than the body.
async fn authorize_project_access(
    auth: &temps_auth::AuthContext,
    project_id: i32,
    project_access_checker: &Option<Arc<dyn temps_core::ProjectAccessChecker>>,
) -> Result<(), Problem> {
    temps_auth::project_access_guard!(auth, project_id, project_access_checker);
    Ok(())
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
        blob_update,
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
            UpdateBlobRequest,
            UpdateBlobResponse,
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
        .route("/blob/update", patch(blob_update))
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
    let project_id =
        extract_project_id(&auth, query.project_id, &state.project_access_checker).await?;

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
    let project_id =
        extract_project_id(&auth, request.project_id, &state.project_access_checker).await?;

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
    let project_id =
        extract_project_id(&auth, query.project_id, &state.project_access_checker).await?;

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
    let project_id =
        extract_project_id(&auth, request.project_id, &state.project_access_checker).await?;

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
        // API key/session: use path parameter, then verify the caller may
        // reach that project (no-op in OSS; enforced with a team-access plugin).
        authorize_project_access(&auth, params.project_id, &state.project_access_checker).await?;
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
        // API key/session: use path parameter, then verify the caller may
        // reach that project (no-op in OSS; enforced with a team-access plugin).
        authorize_project_access(&auth, params.project_id, &state.project_access_checker).await?;
        params.project_id
    };

    let (stream, content_type, size) = state
        .blob_service
        .download(project_id, &params.path)
        .await?;

    // Derive a safe filename for the Content-Disposition header.
    // Take only the final path component, strip CR/LF, and percent-encode
    // non-ASCII characters (RFC 5987) to prevent header injection and XSS.
    let raw_filename = std::path::Path::new(&params.path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("download");
    // Strip CR, LF, and NUL bytes that could inject additional headers.
    let sanitized: String = raw_filename
        .chars()
        .filter(|c| *c != '\r' && *c != '\n' && *c != '\0')
        .collect();
    let encoded_filename = urlencoding::encode(&sanitized);
    let content_disposition = format!("attachment; filename=\"{encoded_filename}\"");

    info!(
        blob_path = %params.path,
        filename = %sanitized,
        size = %size,
        "blob download requested"
    );

    // Convert the stream to axum Body
    let body = Body::from_stream(stream.map_err(std::io::Error::other));

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::CONTENT_LENGTH, size.to_string()),
            // Defense: force browser to download rather than render active content
            // (prevents stored-XSS via text/html, image/svg+xml, application/javascript).
            (header::CONTENT_DISPOSITION, content_disposition),
            // Defense: prevent browser MIME-sniffing that could re-enable rendering.
            (
                header::HeaderName::from_static("x-content-type-options"),
                "nosniff".to_string(),
            ),
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

            let is_running = service.status == "running";
            let is_stopped = service.status == "stopped";

            // Get docker_image from parameters
            let docker_image = details
                .as_ref()
                .and_then(|d| d.current_parameters.as_ref())
                .and_then(|p| p.get("docker_image").cloned())
                .and_then(|v| v.as_str().map(String::from));

            // Extract version from docker_image tag (e.g., "rustfs/rustfs:1.0.0" -> "1.0.0")
            // This ensures the version always matches the actual docker image being used
            let version = docker_image
                .as_ref()
                .and_then(|img| img.split(':').nth(1))
                .map(String::from)
                .or_else(|| Some("latest".to_string()));

            // Service is enabled if it exists and is not stopped
            let enabled = !is_stopped;

            Ok(Json(BlobStatusResponse {
                enabled,
                healthy: is_running,
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

    info!(
        "Enabling Blob service (image={:?}, root_user_provided={}, root_password_provided={})",
        request.docker_image,
        request.root_user.is_some(),
        request.root_password.is_some()
    );

    // Check if the service already exists (might be stopped)
    let existing_service = state
        .external_service_manager
        .get_service_by_name("temps-blob")
        .await
        .ok();

    let service_info = if let Some(existing) = existing_service {
        // Service exists - need to initialize the plugin's RustfsService and start the container
        info!(
            "Blob service exists with status '{}', ensuring it's running...",
            existing.status
        );

        // Get the service config from the database and initialize the plugin's RustfsService
        let service_config = state
            .external_service_manager
            .get_service_config(existing.id)
            .await
            .map_err(|e| {
                error!("Failed to get Blob service config: {}", e);
                temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Failed to Enable Blob Service")
                    .with_detail(format!("Could not get Blob service config: {}", e))
            })?;

        info!(
            "Retrieved Blob service config (service_id: {}), initializing RustfsService...",
            existing.id
        );

        // Initialize the plugin's RustfsService with the config from database
        if let Err(e) = state.rustfs_service.init(service_config).await {
            error!("Failed to initialize RustfsService: {}", e);
            return Err(
                temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Failed to Enable Blob Service")
                    .with_detail(format!("Could not initialize RustFS service: {}", e)),
            );
        }

        info!("RustfsService initialized, starting container...");

        // Start the container through the plugin's RustfsService
        if let Err(e) = state.rustfs_service.start().await {
            // Not a fatal error - container may already be running
            info!(
                "RustFS container start returned: {} (may already be running)",
                e
            );
        }

        // Also start via ExternalServiceManager to update DB status
        state
            .external_service_manager
            .start_service(existing.id)
            .await
            .map_err(|e| {
                error!("Failed to update Blob service status in database: {}", e);
                temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Failed to Enable Blob Service")
                    .with_detail(format!("Could not start Blob service: {}", e))
            })?
    } else {
        // Service doesn't exist, create it
        info!("Blob service doesn't exist, creating new service...");

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

        // Extract version from docker_image (e.g., "rustfs/rustfs:1.0.0" -> "1.0.0")
        let version = parameters
            .get("docker_image")
            .and_then(|v| v.as_str())
            .and_then(|img| img.split(':').nth(1))
            .map(String::from)
            .or_else(|| Some("latest".to_string())); // Default version

        // Create service request for ExternalServiceManager
        // Using Blob service type which uses RustfsService (high-performance S3-compatible storage)
        let create_request = CreateExternalServiceRequest {
            name: "temps-blob".to_string(),
            service_type: ServiceType::Blob,
            version,
            parameters,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        };

        // Create the service through ExternalServiceManager
        // This creates the database record AND initializes/starts the container
        state
            .external_service_manager
            .create_service(create_request)
            .await
            .map_err(|e| {
                error!("Failed to create Blob service: {}", e);
                temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Failed to Enable Blob Service")
                    .with_detail(format!("Could not create S3/Blob service: {}", e))
            })?
    };

    // Get status from the service
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

/// Update Blob service configuration
#[utoipa::path(
    tag = "Blob Management",
    patch,
    path = "/blob/update",
    request_body = UpdateBlobRequest,
    responses(
        (status = 200, description = "Blob service updated", body = UpdateBlobResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Blob service not enabled"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn blob_update(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BlobAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<UpdateBlobRequest>,
) -> Result<impl IntoResponse, Problem> {
    // Update requires SystemAdmin permission
    permission_guard!(auth, SystemAdmin);

    info!("Updating Blob service with config: {:?}", request);

    // Get existing service
    let service = state
        .external_service_manager
        .get_service_by_name("temps-blob")
        .await
        .map_err(|_| {
            temps_core::problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Blob Service Not Found")
                .with_detail("Blob service is not enabled. Enable it first before updating.")
        })?;

    // Get current details for audit log
    let current_details = state
        .external_service_manager
        .get_service_details(service.id)
        .await
        .ok();

    let old_docker_image = current_details
        .as_ref()
        .and_then(|d| d.current_parameters.as_ref())
        .and_then(|p| p.get("docker_image").cloned())
        .and_then(|v| v.as_str().map(String::from));

    let old_version = current_details
        .as_ref()
        .and_then(|d| d.service.version.clone());

    // Build parameters for update
    let mut parameters: HashMap<String, serde_json::Value> = HashMap::new();
    if let Some(docker_image) = &request.docker_image {
        parameters.insert("docker_image".to_string(), serde_json::json!(docker_image));
    }

    // Extract version from docker_image
    let new_version = request
        .docker_image
        .as_ref()
        .and_then(|img| img.split(':').nth(1))
        .map(String::from);

    // Build update request
    let update_request = UpdateExternalServiceRequest {
        name: None,
        parameters,
        docker_image: request.docker_image.clone(),
    };

    // Update the service
    let updated_service = state
        .external_service_manager
        .update_service(service.id, update_request)
        .await
        .map_err(|e| {
            error!("Failed to update Blob service: {}", e);
            temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Failed to Update Blob Service")
                .with_detail(format!("Could not update S3/Blob service: {}", e))
        })?;

    // Get updated docker image from service details
    let new_docker_image = state
        .external_service_manager
        .get_service_details(updated_service.id)
        .await
        .ok()
        .and_then(|details| details.current_parameters)
        .and_then(|p| p.get("docker_image").cloned())
        .and_then(|v| v.as_str().map(String::from));

    // Create audit log
    let audit = BlobServiceUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        service_name: "temps-blob".to_string(),
        old_docker_image,
        new_docker_image: new_docker_image.clone(),
        old_version,
        new_version: new_version.clone(),
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    let healthy = updated_service.status == "running";

    Ok(Json(UpdateBlobResponse {
        success: true,
        message:
            "Blob service updated successfully. Restart may be required for changes to take effect."
                .to_string(),
        status: BlobStatusResponse {
            enabled: true,
            healthy,
            version: new_version,
            docker_image: new_docker_image,
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
        (status = 404, description = "Blob service not enabled"),
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

    // Get the service record
    let service = state
        .external_service_manager
        .get_service_by_name("temps-blob")
        .await
        .map_err(|_| {
            temps_core::problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Blob Service Not Found")
                .with_detail("Blob service is not enabled")
        })?;

    // Stop the service through external_service_manager (stops container + updates DB status)
    state
        .external_service_manager
        .stop_service(service.id)
        .await
        .map_err(|e| {
            error!("Failed to stop Blob service: {}", e);
            temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Failed to Disable Blob Service")
                .with_detail(format!("Could not stop Blob service: {}", e))
        })?;

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

#[cfg(test)]
mod tests {
    /// Replicates the filename-sanitization logic used in `blob_download` so it
    /// can be tested without spinning up Axum or a real BlobService.
    fn sanitize_download_filename(path: &str) -> String {
        let raw_filename = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("download");
        let sanitized: String = raw_filename
            .chars()
            .filter(|c| *c != '\r' && *c != '\n' && *c != '\0')
            .collect();
        urlencoding::encode(&sanitized).into_owned()
    }

    #[test]
    fn test_download_filename_strips_path_components() {
        // An attacker-supplied path must not let the basename traverse directories.
        assert_eq!(
            sanitize_download_filename("evil/../../passwd.html"),
            "passwd.html"
        );
    }

    #[test]
    fn test_download_filename_strips_cr_lf() {
        // CR / LF in filenames can inject additional HTTP headers.
        let input = "file\r\nX-Injected: bad\r\n.html";
        let result = sanitize_download_filename(input);
        assert!(!result.contains('\r'));
        assert!(!result.contains('\n'));
    }

    #[test]
    fn test_download_filename_strips_nul() {
        let input = "evil\0.html";
        let result = sanitize_download_filename(input);
        assert!(!result.contains('\0'));
    }

    #[test]
    fn test_download_filename_percent_encodes_non_ascii() {
        // Non-ASCII characters must be percent-encoded (RFC 5987).
        let result = sanitize_download_filename("résumé.pdf");
        // urlencoding turns é -> %C3%A9
        assert!(
            result.contains('%'),
            "expected percent-encoding in {result}"
        );
        assert!(!result.contains('é'));
    }

    #[test]
    fn test_download_filename_simple_ascii_unchanged() {
        assert_eq!(sanitize_download_filename("report.pdf"), "report.pdf");
    }

    #[test]
    fn test_download_filename_falls_back_when_no_file_component() {
        // A path of just "/" or "" has no file_name; must not panic.
        let result = sanitize_download_filename("/");
        // The fallback is "download"
        assert_eq!(result, "download");
    }
}

#[cfg(test)]
mod idor_tests {
    //! Regression tests for the cross-tenant IDOR on the blob data plane
    //! (security review finding #3). Before the fix, `extract_project_id` (and
    //! the path-based `blob_head`/`blob_download`) trusted the client-supplied
    //! `project_id` verbatim for session/API-key auth. They now run the
    //! team-based `project_access_guard!` via `authorize_project_access`.

    use super::{authorize_project_access, extract_project_id};
    use async_trait::async_trait;
    use std::sync::Arc;
    use temps_auth::{AuthContext, Role};
    use temps_core::ProjectAccessChecker;

    struct MockChecker {
        allow: bool,
    }

    #[async_trait]
    impl ProjectAccessChecker for MockChecker {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            _project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(self.allow)
        }
    }

    fn checker(allow: bool) -> Option<Arc<dyn ProjectAccessChecker>> {
        Some(Arc::new(MockChecker { allow }))
    }

    fn session_auth() -> AuthContext {
        let now = chrono::Utc::now();
        let user = temps_entities::users::Model {
            id: 42,
            name: "Test User".to_string(),
            email: "user42@example.com".to_string(),
            password_hash: None,
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
            created_at: now,
            updated_at: now,
        };
        AuthContext::new_session(user, Role::User)
    }

    // Request-body handlers (blob_put/delete/list/copy) go through
    // extract_project_id.
    #[tokio::test]
    async fn body_handler_denied_project_is_rejected() {
        let auth = session_auth();
        let err = extract_project_id(&auth, Some(999), &checker(false))
            .await
            .expect_err("cross-tenant project id must be rejected");
        assert_eq!(err.status_code, axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn body_handler_allowed_project_is_accepted() {
        let auth = session_auth();
        let pid = extract_project_id(&auth, Some(7), &checker(true))
            .await
            .expect("accessible project id must be accepted");
        assert_eq!(pid, 7);
    }

    // Path handlers (blob_head/blob_download) go through
    // authorize_project_access directly.
    #[tokio::test]
    async fn path_handler_denied_project_is_rejected() {
        let auth = session_auth();
        let err = authorize_project_access(&auth, 999, &checker(false))
            .await
            .expect_err("cross-tenant project id must be rejected on path handlers");
        assert_eq!(err.status_code, axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn oss_without_checker_is_a_noop() {
        let auth = session_auth();
        let pid = extract_project_id(&auth, Some(7), &None)
            .await
            .expect("OSS with no checker must remain a no-op");
        assert_eq!(pid, 7);
        authorize_project_access(&auth, 7, &None)
            .await
            .expect("OSS with no checker must remain a no-op on path handlers");
    }
}
