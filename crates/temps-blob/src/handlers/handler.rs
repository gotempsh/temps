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
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::{Problem, ProblemDetails};
use temps_core::RequestMetadata;
use temps_providers::externalsvc::ExternalService;
use tracing::{error, info};
use utoipa::OpenApi;

use super::audit::{AuditContext, BlobServiceDisabledAudit, BlobServiceEnabledAudit};

use super::types::*;
use crate::services::{ListOptions, PutOptions};

/// OpenAPI documentation for Blob API
#[derive(OpenApi)]
#[openapi(
    paths(
        blob_put,
        blob_delete,
        blob_list,
        blob_head,
        blob_download,
        blob_status,
        blob_enable,
        blob_disable,
    ),
    components(
        schemas(
            BlobResponse,
            DeleteBlobRequest,
            DeleteBlobResponse,
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
        .route("/blob/{project_id}/*path", head(blob_head))
        .route("/blob/{project_id}/*path", get(blob_download))
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
    body: Bytes,
) -> Result<impl IntoResponse, Problem> {
    // For MVP, we use a simple approach where pathname comes from query or we require multipart
    // Here we'll accept raw bytes with pathname in header for simplicity
    // In production, you'd want multipart form data

    // Get project ID from auth context
    let project_id = auth.project_id().ok_or_else(|| {
        temps_core::problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Project Required")
            .with_detail("A project context is required for blob operations")
    })?;

    // For MVP, use a default pathname - in production use multipart
    let pathname = "upload";
    let options = PutOptions {
        content_type: None,
        add_random_suffix: true,
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
    let project_id = auth.project_id().ok_or_else(|| {
        temps_core::problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Project Required")
            .with_detail("A project context is required for blob operations")
    })?;

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
    let project_id = auth.project_id().ok_or_else(|| {
        temps_core::problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Project Required")
            .with_detail("A project context is required for blob operations")
    })?;

    let options = ListOptions {
        limit: query.limit,
        prefix: query.prefix,
        cursor: query.cursor,
    };

    let result = state.blob_service.list(project_id, options).await?;

    Ok(Json(ListBlobsResponse::from(result)))
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
    // Verify project access
    let auth_project_id = auth.project_id().ok_or_else(|| {
        temps_core::problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Project Required")
            .with_detail("A project context is required for blob operations")
    })?;

    if auth_project_id != params.project_id {
        return Err(temps_core::problemdetails::new(StatusCode::FORBIDDEN)
            .with_title("Access Denied")
            .with_detail("You do not have access to this project's blobs"));
    }

    let blob_info = state
        .blob_service
        .head(params.project_id, &params.path)
        .await?;

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
    // Verify project access
    let auth_project_id = auth.project_id().ok_or_else(|| {
        temps_core::problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Project Required")
            .with_detail("A project context is required for blob operations")
    })?;

    if auth_project_id != params.project_id {
        return Err(temps_core::problemdetails::new(StatusCode::FORBIDDEN)
            .with_title("Access Denied")
            .with_detail("You do not have access to this project's blobs"));
    }

    let (stream, content_type, size) = state
        .blob_service
        .download(params.project_id, &params.path)
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

    let healthy = state.rustfs_service.health_check().await.unwrap_or(false);

    let (version, docker_image) = if healthy {
        let version = state.rustfs_service.get_current_version().await.ok();
        let image = state
            .rustfs_service
            .get_current_docker_image()
            .await
            .ok()
            .map(|(name, tag)| format!("{}:{}", name, tag));
        (version, image)
    } else {
        (None, None)
    };

    Ok(Json(BlobStatusResponse {
        enabled: healthy,
        healthy,
        version,
        docker_image,
    }))
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

    // Start the RustFS service
    if let Err(e) = state.rustfs_service.start().await {
        error!("Failed to start Blob service: {}", e);
        return Err(
            temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Failed to Enable Blob Service")
                .with_detail(format!("Could not start RustFS container: {}", e)),
        );
    }

    // Get status after starting
    let healthy = state.rustfs_service.health_check().await.unwrap_or(false);
    let version = state.rustfs_service.get_current_version().await.ok();
    let docker_image = state
        .rustfs_service
        .get_current_docker_image()
        .await
        .ok()
        .map(|(name, tag)| format!("{}:{}", name, tag));

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
