//! Remote Deployment API Handlers
//!
//! Handles remote deployments from pre-built Docker images and static file bundles.
//! These endpoints enable external CI/CD systems to deploy to Temps without Git integration.

use std::sync::Arc;

use super::types::AppState;
use axum::{
    extract::{Multipart, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::UtcDateTime;
use tracing::{debug, error, info};
use utoipa::{OpenApi, ToSchema};

use crate::services::{ExternalImageInfo, RegisterExternalImageRequest, StaticBundleInfo};

#[derive(OpenApi)]
#[openapi(
    paths(
        deploy_from_image,
        deploy_from_static,
        upload_static_bundle,
        register_external_image,
        list_external_images,
        get_external_image,
        delete_external_image,
        list_static_bundles,
        get_static_bundle,
        delete_static_bundle
    ),
    components(schemas(
        DeployFromImageRequest,
        DeployFromStaticRequest,
        DeploymentResponse,
        ExternalImageResponse,
        StaticBundleResponse,
        PaginatedExternalImagesResponse,
        PaginatedStaticBundlesResponse
    )),
    info(
        title = "Remote Deployments API",
        description = "API endpoints for deploying pre-built Docker images and static files",
        version = "1.0.0"
    )
)]
pub struct RemoteDeploymentsApiDoc;

// Request Types

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct DeployFromImageRequest {
    /// Docker image reference (e.g., "ghcr.io/org/app:v1.0")
    #[schema(example = "ghcr.io/myorg/myapp:v1.0")]
    pub image_ref: String,
    /// Optional external image ID (if already registered)
    pub external_image_id: Option<i32>,
    /// Optional deployment metadata
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct DeployFromStaticRequest {
    /// Static bundle ID (required)
    pub static_bundle_id: i32,
    /// Optional deployment metadata
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct RegisterImageRequest {
    /// Docker image reference (e.g., "ghcr.io/org/app:v1.0")
    #[schema(example = "ghcr.io/myorg/myapp:v1.0")]
    pub image_ref: String,
    /// Image digest (sha256:...)
    #[schema(example = "sha256:abc123def456")]
    pub digest: Option<String>,
    /// Image tag
    #[schema(example = "v1.0")]
    pub tag: Option<String>,
    /// Additional metadata
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, ToSchema, Default)]
pub struct PaginationQuery {
    pub page: Option<u64>,
    pub page_size: Option<u64>,
}

// Response Types

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DeploymentResponse {
    pub id: i32,
    pub project_id: i32,
    pub environment_id: i32,
    pub slug: String,
    pub state: String,
    pub source_type: String,
    #[schema(value_type = String, format = DateTime, example = "2025-10-12T12:15:47.609192Z")]
    pub created_at: UtcDateTime,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ExternalImageResponse {
    pub id: i32,
    pub project_id: i32,
    pub image_ref: String,
    pub digest: Option<String>,
    pub tag: Option<String>,
    pub size_bytes: Option<i64>,
    pub metadata: Option<serde_json::Value>,
    #[schema(value_type = String, format = DateTime, example = "2025-10-12T12:15:47.609192Z")]
    pub pushed_at: UtcDateTime,
    #[schema(value_type = String, format = DateTime, example = "2025-10-12T12:15:47.609192Z")]
    pub created_at: UtcDateTime,
}

impl From<ExternalImageInfo> for ExternalImageResponse {
    fn from(info: ExternalImageInfo) -> Self {
        Self {
            id: info.id,
            project_id: info.project_id,
            image_ref: info.image_ref,
            digest: info.digest,
            tag: info.tag,
            size_bytes: info.size_bytes,
            metadata: info.metadata,
            pushed_at: info.pushed_at,
            created_at: info.created_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StaticBundleResponse {
    pub id: i32,
    pub project_id: i32,
    pub blob_path: String,
    pub original_filename: Option<String>,
    pub content_type: String,
    pub format: Option<String>,
    pub size_bytes: i64,
    pub checksum: Option<String>,
    pub metadata: Option<serde_json::Value>,
    #[schema(value_type = String, format = DateTime, example = "2025-10-12T12:15:47.609192Z")]
    pub uploaded_at: UtcDateTime,
    #[schema(value_type = String, format = DateTime, example = "2025-10-12T12:15:47.609192Z")]
    pub created_at: UtcDateTime,
}

impl From<StaticBundleInfo> for StaticBundleResponse {
    fn from(info: StaticBundleInfo) -> Self {
        Self {
            id: info.id,
            project_id: info.project_id,
            blob_path: info.blob_path,
            original_filename: info.original_filename,
            content_type: info.content_type,
            format: info.format,
            size_bytes: info.size_bytes,
            checksum: info.checksum,
            metadata: info.metadata,
            uploaded_at: info.uploaded_at,
            created_at: info.created_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PaginatedExternalImagesResponse {
    pub data: Vec<ExternalImageResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PaginatedStaticBundlesResponse {
    pub data: Vec<StaticBundleResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

// Handlers

/// Deploy from an external Docker image
///
/// Triggers a deployment using a pre-built Docker image from an external registry.
/// The image will be pulled and deployed to the specified environment.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/environments/{environment_id}/deploy/image",
    request_body = DeployFromImageRequest,
    responses(
        (status = 202, description = "Deployment started", body = DeploymentResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project or environment not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn deploy_from_image(
    RequireAuth(auth): RequireAuth,
    State(_state): State<Arc<AppState>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    Json(req): Json<DeployFromImageRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsCreate);

    info!(
        "Deploying external image {} to project {} environment {}",
        req.image_ref, project_id, environment_id
    );

    // Validate image reference
    if req.image_ref.is_empty() {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Image Reference")
            .with_detail("Image reference cannot be empty"));
    }

    // TODO: Implement actual deployment logic
    // This would:
    // 1. Verify project and environment exist
    // 2. Create deployment record with source_type = DockerImage
    // 3. Queue deployment job via WorkflowPlanner
    // 4. Return deployment info

    // For now, return a placeholder response
    Err::<(StatusCode, Json<DeploymentResponse>), _>(
        problemdetails::new(StatusCode::NOT_IMPLEMENTED)
            .with_title("Not Implemented")
            .with_detail("Image deployment endpoint is not yet fully implemented"),
    )
}

/// Deploy from an uploaded static bundle
///
/// Triggers a deployment using a previously uploaded static file bundle.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/environments/{environment_id}/deploy/static",
    request_body = DeployFromStaticRequest,
    responses(
        (status = 202, description = "Deployment started", body = DeploymentResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project, environment, or bundle not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn deploy_from_static(
    RequireAuth(auth): RequireAuth,
    State(_state): State<Arc<AppState>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    Json(req): Json<DeployFromStaticRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsCreate);

    info!(
        "Deploying static bundle {} to project {} environment {}",
        req.static_bundle_id, project_id, environment_id
    );

    // TODO: Implement actual deployment logic
    Err::<(StatusCode, Json<DeploymentResponse>), _>(
        problemdetails::new(StatusCode::NOT_IMPLEMENTED)
            .with_title("Not Implemented")
            .with_detail("Static deployment endpoint is not yet fully implemented"),
    )
}

/// Upload a static bundle for later deployment
///
/// Uploads a tar.gz or zip file containing static assets. The bundle can be
/// deployed later using the deploy/static endpoint.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/upload/static",
    responses(
        (status = 201, description = "Bundle uploaded successfully", body = StaticBundleResponse),
        (status = 400, description = "Invalid request or unsupported format"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project not found"),
        (status = 413, description = "Bundle too large"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn upload_static_bundle(
    RequireAuth(auth): RequireAuth,
    State(_state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    _multipart: Multipart,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsCreate);

    debug!("Uploading static bundle for project {}", project_id);

    // TODO: Implement multipart upload handling
    // This would:
    // 1. Read the uploaded file from multipart
    // 2. Validate content type (tar.gz or zip)
    // 3. Upload to blob storage via BlobService
    // 4. Register in static_bundles table via RemoteDeploymentService
    // 5. Return bundle info

    Err::<(StatusCode, Json<StaticBundleResponse>), _>(
        problemdetails::new(StatusCode::NOT_IMPLEMENTED)
            .with_title("Not Implemented")
            .with_detail("Static bundle upload endpoint is not yet fully implemented"),
    )
}

/// Register an external Docker image
///
/// Registers an external Docker image reference without triggering a deployment.
/// The image can be deployed later using the deploy/image endpoint.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/external-images",
    request_body = RegisterImageRequest,
    responses(
        (status = 201, description = "Image registered successfully", body = ExternalImageResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn register_external_image(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Json(req): Json<RegisterImageRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsCreate);

    debug!(
        "Registering external image for project {}: {}",
        project_id, req.image_ref
    );

    if req.image_ref.is_empty() {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Image Reference")
            .with_detail("Image reference cannot be empty"));
    }

    let request = RegisterExternalImageRequest {
        image_ref: req.image_ref,
        digest: req.digest,
        tag: req.tag,
        metadata: req.metadata,
    };

    let result = state
        .remote_deployment_service
        .register_external_image(project_id, request)
        .await
        .map_err(|e| {
            error!("Failed to register external image: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Registration Failed")
                .with_detail(e.to_string())
        })?;

    info!(
        "External image registered for project {}: id={}",
        project_id, result.id
    );

    Ok((
        StatusCode::CREATED,
        Json(ExternalImageResponse::from(result)),
    ))
}

/// List external images for a project
#[utoipa::path(
    get,
    path = "/projects/{project_id}/external-images",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("page" = Option<u64>, Query, description = "Page number (default: 1)"),
        ("page_size" = Option<u64>, Query, description = "Items per page (default: 20)")
    ),
    responses(
        (status = 200, description = "List of external images", body = PaginatedExternalImagesResponse),
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
    Query(query): Query<PaginationQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);

    let (images, total) = state
        .remote_deployment_service
        .list_external_images(project_id, query.page, query.page_size)
        .await
        .map_err(|e| {
            error!("Failed to list external images: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("List Failed")
                .with_detail(e.to_string())
        })?;

    let page = query.page.unwrap_or(1);
    let page_size = query.page_size.unwrap_or(20);

    Ok(Json(PaginatedExternalImagesResponse {
        data: images
            .into_iter()
            .map(ExternalImageResponse::from)
            .collect(),
        total,
        page,
        page_size,
    }))
}

/// Get details of a specific external image
#[utoipa::path(
    get,
    path = "/projects/{project_id}/external-images/{image_id}",
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
    Path((_project_id, image_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);

    let image = state
        .remote_deployment_service
        .get_external_image(image_id)
        .await
        .map_err(|e| {
            error!("Failed to get external image: {}", e);
            match e {
                crate::services::remote_deployment_service::RemoteDeploymentError::ImageNotFound(_) => {
                    problemdetails::new(StatusCode::NOT_FOUND)
                        .with_title("Image Not Found")
                        .with_detail(e.to_string())
                }
                _ => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Fetch Failed")
                    .with_detail(e.to_string()),
            }
        })?;

    Ok(Json(ExternalImageResponse::from(image)))
}

/// Delete an external image
#[utoipa::path(
    delete,
    path = "/projects/{project_id}/external-images/{image_id}",
    responses(
        (status = 204, description = "Image deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Image not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_external_image(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((_project_id, image_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsDelete);

    state
        .remote_deployment_service
        .delete_external_image(image_id)
        .await
        .map_err(|e| {
            error!("Failed to delete external image: {}", e);
            match e {
                crate::services::remote_deployment_service::RemoteDeploymentError::ImageNotFound(_) => {
                    problemdetails::new(StatusCode::NOT_FOUND)
                        .with_title("Image Not Found")
                        .with_detail(e.to_string())
                }
                _ => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Delete Failed")
                    .with_detail(e.to_string()),
            }
        })?;

    Ok(StatusCode::NO_CONTENT)
}

/// List static bundles for a project
#[utoipa::path(
    get,
    path = "/projects/{project_id}/static-bundles",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("page" = Option<u64>, Query, description = "Page number (default: 1)"),
        ("page_size" = Option<u64>, Query, description = "Items per page (default: 20)")
    ),
    responses(
        (status = 200, description = "List of static bundles", body = PaginatedStaticBundlesResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_static_bundles(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<PaginationQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);

    let (bundles, total) = state
        .remote_deployment_service
        .list_static_bundles(project_id, query.page, query.page_size)
        .await
        .map_err(|e| {
            error!("Failed to list static bundles: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("List Failed")
                .with_detail(e.to_string())
        })?;

    let page = query.page.unwrap_or(1);
    let page_size = query.page_size.unwrap_or(20);

    Ok(Json(PaginatedStaticBundlesResponse {
        data: bundles
            .into_iter()
            .map(StaticBundleResponse::from)
            .collect(),
        total,
        page,
        page_size,
    }))
}

/// Get details of a specific static bundle
#[utoipa::path(
    get,
    path = "/projects/{project_id}/static-bundles/{bundle_id}",
    responses(
        (status = 200, description = "Bundle details", body = StaticBundleResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Bundle not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_static_bundle(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((_project_id, bundle_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);

    let bundle = state
        .remote_deployment_service
        .get_static_bundle(bundle_id)
        .await
        .map_err(|e| {
            error!("Failed to get static bundle: {}", e);
            match e {
                crate::services::remote_deployment_service::RemoteDeploymentError::BundleNotFound(_) => {
                    problemdetails::new(StatusCode::NOT_FOUND)
                        .with_title("Bundle Not Found")
                        .with_detail(e.to_string())
                }
                _ => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Fetch Failed")
                    .with_detail(e.to_string()),
            }
        })?;

    Ok(Json(StaticBundleResponse::from(bundle)))
}

/// Delete a static bundle
#[utoipa::path(
    delete,
    path = "/projects/{project_id}/static-bundles/{bundle_id}",
    responses(
        (status = 204, description = "Bundle deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Bundle not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_static_bundle(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((_project_id, bundle_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsDelete);

    state
        .remote_deployment_service
        .delete_static_bundle(bundle_id)
        .await
        .map_err(|e| {
            error!("Failed to delete static bundle: {}", e);
            match e {
                crate::services::remote_deployment_service::RemoteDeploymentError::BundleNotFound(_) => {
                    problemdetails::new(StatusCode::NOT_FOUND)
                        .with_title("Bundle Not Found")
                        .with_detail(e.to_string())
                }
                _ => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Delete Failed")
                    .with_detail(e.to_string()),
            }
        })?;

    Ok(StatusCode::NO_CONTENT)
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        // Deploy endpoints
        .route(
            "/projects/{project_id}/environments/{environment_id}/deploy/image",
            post(deploy_from_image),
        )
        .route(
            "/projects/{project_id}/environments/{environment_id}/deploy/static",
            post(deploy_from_static),
        )
        // Upload endpoints
        .route(
            "/projects/{project_id}/upload/static",
            post(upload_static_bundle),
        )
        // External images CRUD
        .route(
            "/projects/{project_id}/external-images",
            post(register_external_image).get(list_external_images),
        )
        .route(
            "/projects/{project_id}/external-images/{image_id}",
            get(get_external_image).delete(delete_external_image),
        )
        // Static bundles CRUD
        .route(
            "/projects/{project_id}/static-bundles",
            get(list_static_bundles),
        )
        .route(
            "/projects/{project_id}/static-bundles/{bundle_id}",
            get(get_static_bundle).delete(delete_static_bundle),
        )
}
