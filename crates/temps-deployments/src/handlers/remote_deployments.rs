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
use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, Set};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::{DeploymentCreatedJob, Job, UtcDateTime};
use temps_entities::deployments::DeploymentMetadata;
use temps_entities::source_type::SourceType;
use temps_entities::types::PipelineStatus;
use temps_entities::{deployments, environments, projects};
use tracing::{debug, error, info};
use utoipa::{IntoParams, OpenApi, ToSchema};

use crate::services::{ExternalImageInfo, RegisterExternalImageRequest, StaticBundleInfo};

#[derive(OpenApi)]
#[openapi(
    paths(
        deploy_from_image,
        deploy_from_image_upload,
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
        DeployFromImageUploadQuery,
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
    /// Required if external_image_id is not provided
    #[schema(example = "ghcr.io/myorg/myapp:v1.0")]
    pub image_ref: Option<String>,
    /// External image ID (if already registered). If provided without image_ref,
    /// the image reference will be fetched from the registered external image.
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

/// Query parameters for deploying from an uploaded image tarball
#[derive(Debug, Clone, Deserialize, ToSchema, IntoParams)]
pub struct DeployFromImageUploadQuery {
    /// Tag to apply to the imported image (e.g., "myapp:v1.0")
    /// If not provided, a unique tag will be generated
    #[schema(example = "myapp:v1.0")]
    pub tag: Option<String>,
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
    State(state): State<Arc<AppState>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    Json(req): Json<DeployFromImageRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsCreate);

    // Resolve image_ref: either use provided value or fetch from external_image_id
    let (image_ref, external_image_id) = match (&req.image_ref, req.external_image_id) {
        // Both provided: use image_ref directly
        (Some(ref img_ref), ext_id) => {
            if img_ref.is_empty() {
                return Err(problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Image Reference")
                    .with_detail("Image reference cannot be empty"));
            }
            (img_ref.clone(), ext_id)
        }
        // Only external_image_id provided: fetch image_ref from database
        (None, Some(ext_id)) => {
            let external_image = state
                .remote_deployment_service
                .get_external_image(ext_id)
                .await
                .map_err(|e| {
                    error!("Failed to get external image {}: {}", ext_id, e);
                    match e {
                        crate::services::remote_deployment_service::RemoteDeploymentError::ImageNotFound(_) => {
                            problemdetails::new(StatusCode::NOT_FOUND)
                                .with_title("External Image Not Found")
                                .with_detail(format!("External image with ID {} not found", ext_id))
                        }
                        _ => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                            .with_title("Database Error")
                            .with_detail(e.to_string()),
                    }
                })?;

            // Verify the external image belongs to the same project
            if external_image.project_id != project_id {
                return Err(problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid External Image")
                    .with_detail("External image does not belong to this project"));
            }

            (external_image.image_ref, Some(ext_id))
        }
        // Neither provided: error
        (None, None) => {
            return Err(problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Missing Image Reference")
                .with_detail("Either image_ref or external_image_id must be provided"));
        }
    };

    info!(
        "Deploying external image {} to project {} environment {}",
        image_ref, project_id, environment_id
    );

    // 1. Verify project exists and has DockerImage source type
    let project = projects::Entity::find_by_id(project_id)
        .one(state.db.as_ref())
        .await
        .map_err(|e| {
            error!("Database error: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Database Error")
                .with_detail(e.to_string())
        })?
        .ok_or_else(|| {
            problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Project Not Found")
                .with_detail(format!("Project {} not found", project_id))
        })?;

    // Verify project source type allows Docker image deployments
    if !project
        .source_type
        .allows_deployment_method(&SourceType::DockerImage)
    {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Project Type")
            .with_detail(format!(
                "Project with source type {:?} does not allow Docker image deployments",
                project.source_type
            )));
    }

    // 2. Verify environment exists and belongs to project
    let environment = environments::Entity::find_by_id(environment_id)
        .one(state.db.as_ref())
        .await
        .map_err(|e| {
            error!("Database error: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Database Error")
                .with_detail(e.to_string())
        })?
        .ok_or_else(|| {
            problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Environment Not Found")
                .with_detail(format!("Environment {} not found", environment_id))
        })?;

    if environment.project_id != project_id {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Environment")
            .with_detail("Environment does not belong to this project"));
    }

    // 3. Generate deployment slug
    let deployment_number = deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(project_id))
        .count(state.db.as_ref())
        .await
        .unwrap_or(0)
        + 1;
    let deployment_slug = format!("{}-{}", project.slug, deployment_number);

    // 4. Create deployment metadata (track deployment source type for flexible projects)
    let deployment_metadata = DeploymentMetadata {
        external_image_ref: Some(image_ref.clone()),
        external_image_id,
        deployment_source_type: Some(SourceType::DockerImage),
        ..Default::default()
    };

    // 5. Create deployment record
    let now = Utc::now();
    let new_deployment = deployments::ActiveModel {
        project_id: Set(project_id),
        environment_id: Set(environment_id),
        slug: Set(deployment_slug),
        state: Set("pending".to_string()),
        metadata: Set(Some(deployment_metadata)),
        context_vars: Set(Some(serde_json::json!({
            "trigger": "remote_deploy",
            "source": "docker_image"
        }))),
        image_name: Set(Some(image_ref.clone())),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };

    let deployment = new_deployment
        .insert(state.db.as_ref())
        .await
        .map_err(|e| {
            error!("Failed to create deployment: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Deployment Creation Failed")
                .with_detail(e.to_string())
        })?;

    info!(
        "Created deployment {} for Docker image deployment",
        deployment.id
    );

    // 6. Fire DeploymentCreated event
    let deployment_created_event = Job::DeploymentCreated(DeploymentCreatedJob {
        deployment_id: deployment.id,
        project_id: project.id,
        environment_id: environment.id,
        environment_name: environment.name.clone(),
        branch: None,
        commit_sha: None,
    });
    if let Err(e) = state.queue_service.send(deployment_created_event).await {
        error!("Failed to send DeploymentCreated event: {}", e);
    }

    // 7. Create jobs using WorkflowPlanner
    let create_jobs_result = state
        .workflow_planner
        .create_deployment_jobs(deployment.id)
        .await;

    match create_jobs_result {
        Ok(created_jobs) => {
            info!(
                "Created {} jobs for deployment {}",
                created_jobs.len(),
                deployment.id
            );

            // Update deployment status to Running
            if let Err(e) =
                crate::services::job_processor::JobProcessorService::update_deployment_status(
                    &state.db,
                    deployment.id,
                    PipelineStatus::Running,
                )
                .await
            {
                error!("Failed to update deployment status: {}", e);
            }

            // Execute the workflow in background
            let workflow_executor = state.workflow_executor.clone();
            let deployment_id = deployment.id;
            let db = state.db.clone();
            tokio::spawn(async move {
                match workflow_executor
                    .execute_deployment_workflow(deployment_id)
                    .await
                {
                    Ok(_) => {
                        info!(
                            "Workflow execution completed for deployment {}",
                            deployment_id
                        );
                    }
                    Err(e) => {
                        error!(
                            "Workflow execution failed for deployment {}: {}",
                            deployment_id, e
                        );
                        let _ = crate::services::job_processor::JobProcessorService::update_deployment_status_with_message(
                            &db,
                            deployment_id,
                            PipelineStatus::Failed,
                            Some(e.to_string()),
                        )
                        .await;
                    }
                }
            });
        }
        Err(e) => {
            error!("Failed to create jobs for deployment: {}", e);
            // Mark deployment as failed
            let _ = crate::services::job_processor::JobProcessorService::update_deployment_status_with_message(
                &state.db,
                deployment.id,
                PipelineStatus::Failed,
                Some(e.to_string()),
            )
            .await;

            return Err(problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Job Creation Failed")
                .with_detail(e.to_string()));
        }
    }

    // 8. Return deployment info
    Ok((
        StatusCode::ACCEPTED,
        Json(DeploymentResponse {
            id: deployment.id,
            project_id: deployment.project_id,
            environment_id: deployment.environment_id,
            slug: deployment.slug,
            state: deployment.state,
            source_type: "docker_image".to_string(),
            created_at: deployment.created_at,
        }),
    ))
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
    State(state): State<Arc<AppState>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    Json(req): Json<DeployFromStaticRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsCreate);

    info!(
        "Deploying static bundle {} to project {} environment {}",
        req.static_bundle_id, project_id, environment_id
    );

    // 1. Verify project exists and has StaticFiles source type
    let project = projects::Entity::find_by_id(project_id)
        .one(state.db.as_ref())
        .await
        .map_err(|e| {
            error!("Database error: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Database Error")
                .with_detail(e.to_string())
        })?
        .ok_or_else(|| {
            problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Project Not Found")
                .with_detail(format!("Project {} not found", project_id))
        })?;

    // Verify project source type allows static file deployments
    if !project
        .source_type
        .allows_deployment_method(&SourceType::StaticFiles)
    {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Project Type")
            .with_detail(format!(
                "Project with source type {:?} does not allow static file deployments",
                project.source_type
            )));
    }

    // 2. Verify environment exists and belongs to project
    let environment = environments::Entity::find_by_id(environment_id)
        .one(state.db.as_ref())
        .await
        .map_err(|e| {
            error!("Database error: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Database Error")
                .with_detail(e.to_string())
        })?
        .ok_or_else(|| {
            problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Environment Not Found")
                .with_detail(format!("Environment {} not found", environment_id))
        })?;

    if environment.project_id != project_id {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Environment")
            .with_detail("Environment does not belong to this project"));
    }

    // 3. Verify static bundle exists and belongs to project
    let bundle = state
        .remote_deployment_service
        .get_static_bundle(req.static_bundle_id)
        .await
        .map_err(|e| {
            error!("Failed to get static bundle: {}", e);
            match e {
                crate::services::remote_deployment_service::RemoteDeploymentError::BundleNotFound(_) => {
                    problemdetails::new(StatusCode::NOT_FOUND)
                        .with_title("Static Bundle Not Found")
                        .with_detail(e.to_string())
                }
                _ => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Fetch Failed")
                    .with_detail(e.to_string()),
            }
        })?;

    if bundle.project_id != project_id {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Bundle")
            .with_detail("Static bundle does not belong to this project"));
    }

    // 4. Generate deployment slug
    let deployment_number = deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(project_id))
        .count(state.db.as_ref())
        .await
        .unwrap_or(0)
        + 1;
    let deployment_slug = format!("{}-{}", project.slug, deployment_number);

    // 5. Create deployment metadata (track deployment source type for flexible projects)
    let deployment_metadata = DeploymentMetadata {
        static_bundle_path: Some(bundle.blob_path.clone()),
        static_bundle_id: Some(req.static_bundle_id),
        static_bundle_content_type: Some(bundle.content_type.clone()),
        deployment_source_type: Some(SourceType::StaticFiles),
        ..Default::default()
    };

    // 6. Create deployment record
    let now = Utc::now();
    let new_deployment = deployments::ActiveModel {
        project_id: Set(project_id),
        environment_id: Set(environment_id),
        slug: Set(deployment_slug),
        state: Set("pending".to_string()),
        metadata: Set(Some(deployment_metadata)),
        context_vars: Set(Some(serde_json::json!({
            "trigger": "remote_deploy",
            "source": "static_bundle",
            "bundle_id": req.static_bundle_id
        }))),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };

    let deployment = new_deployment
        .insert(state.db.as_ref())
        .await
        .map_err(|e| {
            error!("Failed to create deployment: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Deployment Creation Failed")
                .with_detail(e.to_string())
        })?;

    info!(
        "Created deployment {} for static bundle deployment",
        deployment.id
    );

    // 7. Fire DeploymentCreated event
    let deployment_created_event = Job::DeploymentCreated(DeploymentCreatedJob {
        deployment_id: deployment.id,
        project_id: project.id,
        environment_id: environment.id,
        environment_name: environment.name.clone(),
        branch: None,
        commit_sha: None,
    });
    if let Err(e) = state.queue_service.send(deployment_created_event).await {
        error!("Failed to send DeploymentCreated event: {}", e);
    }

    // 8. Create jobs using WorkflowPlanner
    let create_jobs_result = state
        .workflow_planner
        .create_deployment_jobs(deployment.id)
        .await;

    match create_jobs_result {
        Ok(created_jobs) => {
            info!(
                "Created {} jobs for deployment {}",
                created_jobs.len(),
                deployment.id
            );

            // Update deployment status to Running
            if let Err(e) =
                crate::services::job_processor::JobProcessorService::update_deployment_status(
                    &state.db,
                    deployment.id,
                    PipelineStatus::Running,
                )
                .await
            {
                error!("Failed to update deployment status: {}", e);
            }

            // Execute the workflow in background
            let workflow_executor = state.workflow_executor.clone();
            let deployment_id = deployment.id;
            let db = state.db.clone();
            tokio::spawn(async move {
                match workflow_executor
                    .execute_deployment_workflow(deployment_id)
                    .await
                {
                    Ok(_) => {
                        info!(
                            "Workflow execution completed for deployment {}",
                            deployment_id
                        );
                    }
                    Err(e) => {
                        error!(
                            "Workflow execution failed for deployment {}: {}",
                            deployment_id, e
                        );
                        let _ = crate::services::job_processor::JobProcessorService::update_deployment_status_with_message(
                            &db,
                            deployment_id,
                            PipelineStatus::Failed,
                            Some(e.to_string()),
                        )
                        .await;
                    }
                }
            });
        }
        Err(e) => {
            error!("Failed to create jobs for deployment: {}", e);
            // Mark deployment as failed
            let _ = crate::services::job_processor::JobProcessorService::update_deployment_status_with_message(
                &state.db,
                deployment.id,
                PipelineStatus::Failed,
                Some(e.to_string()),
            )
            .await;

            return Err(problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Job Creation Failed")
                .with_detail(e.to_string()));
        }
    }

    // 9. Return deployment info
    Ok((
        StatusCode::ACCEPTED,
        Json(DeploymentResponse {
            id: deployment.id,
            project_id: deployment.project_id,
            environment_id: deployment.environment_id,
            slug: deployment.slug,
            state: deployment.state,
            source_type: "static_files".to_string(),
            created_at: deployment.created_at,
        }),
    ))
}

/// Deploy from an uploaded Docker image tarball
///
/// Uploads a Docker image tarball (from `docker save`) and deploys it directly.
/// The image is imported using `docker load` and then deployed to the specified environment.
/// This is useful when you want to deploy an image without pushing to a registry first.
///
/// The uploaded file should be a tarball created by `docker save myimage:tag > image.tar`
/// or `docker save myimage:tag | gzip > image.tar.gz` (gzip compressed tarballs are also supported).
#[utoipa::path(
    post,
    path = "/projects/{project_id}/environments/{environment_id}/deploy/image-upload",
    params(DeployFromImageUploadQuery),
    responses(
        (status = 202, description = "Image imported and deployment started", body = DeploymentResponse),
        (status = 400, description = "Invalid request or unsupported format"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project or environment not found"),
        (status = 413, description = "Image tarball too large"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn deploy_from_image_upload(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    Query(query): Query<DeployFromImageUploadQuery>,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsCreate);

    info!(
        "Deploying from uploaded image tarball to project {} environment {}",
        project_id, environment_id
    );

    // 1. Verify project exists and has DockerImage source type
    let project = projects::Entity::find_by_id(project_id)
        .one(state.db.as_ref())
        .await
        .map_err(|e| {
            error!("Database error: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Database Error")
                .with_detail(e.to_string())
        })?
        .ok_or_else(|| {
            problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Project Not Found")
                .with_detail(format!("Project {} not found", project_id))
        })?;

    // Verify project source type allows Docker image deployments
    if !project
        .source_type
        .allows_deployment_method(&SourceType::DockerImage)
    {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Project Type")
            .with_detail(format!(
                "Project with source type {:?} does not allow Docker image deployments",
                project.source_type
            )));
    }

    // 2. Verify environment exists and belongs to project
    let environment = environments::Entity::find_by_id(environment_id)
        .one(state.db.as_ref())
        .await
        .map_err(|e| {
            error!("Database error: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Database Error")
                .with_detail(e.to_string())
        })?
        .ok_or_else(|| {
            problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Environment Not Found")
                .with_detail(format!("Environment {} not found", environment_id))
        })?;

    if environment.project_id != project_id {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Environment")
            .with_detail("Environment does not belong to this project"));
    }

    // 3. Read the uploaded image tarball from multipart
    let mut file_data: Option<bytes::Bytes> = None;
    let mut original_filename: Option<String> = None;

    // Maximum file size: 2GB for Docker images
    const MAX_FILE_SIZE: usize = 2 * 1024 * 1024 * 1024;

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        error!("Multipart error: {}", e);
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Multipart Error")
            .with_detail(e.to_string())
    })? {
        let name = field.name().unwrap_or_default().to_string();

        if name == "file" || name == "image" {
            original_filename = field.file_name().map(String::from);

            let data = field.bytes().await.map_err(|e| {
                error!("Failed to read file data: {}", e);
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("File Read Error")
                    .with_detail(e.to_string())
            })?;

            if data.len() > MAX_FILE_SIZE {
                return Err(problemdetails::new(StatusCode::PAYLOAD_TOO_LARGE)
                    .with_title("Image Tarball Too Large")
                    .with_detail(format!(
                        "Image tarball size {} exceeds maximum of {} bytes",
                        data.len(),
                        MAX_FILE_SIZE
                    )));
            }

            file_data = Some(data);
        }
    }

    let file_data = file_data.ok_or_else(|| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Missing File")
            .with_detail("No image tarball file was uploaded. Use field name 'file' or 'image'")
    })?;

    info!(
        "Received image tarball: {} bytes, filename: {:?}",
        file_data.len(),
        original_filename
    );

    // 4. Write tarball to temporary file
    let temp_dir = std::env::temp_dir();
    let temp_filename = format!(
        "temps-image-{}-{}.tar",
        project_id,
        Utc::now().timestamp_millis()
    );
    let temp_path = temp_dir.join(&temp_filename);

    tokio::fs::write(&temp_path, &file_data)
        .await
        .map_err(|e| {
            error!("Failed to write temporary file: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("File Write Error")
                .with_detail(e.to_string())
        })?;

    // 5. Generate image tag if not provided
    let image_tag = query.tag.unwrap_or_else(|| {
        format!(
            "temps-{}-{}:upload-{}",
            project.slug,
            environment.slug,
            Utc::now().timestamp()
        )
    });

    // 6. Import the image using docker load
    info!("Importing image from tarball with tag: {}", image_tag);
    let import_result = state
        .image_builder
        .import_image(temp_path.clone(), &image_tag)
        .await;

    // Clean up temp file
    if let Err(e) = tokio::fs::remove_file(&temp_path).await {
        debug!("Failed to remove temporary file: {}", e);
    }

    let imported_image_id = import_result.map_err(|e| {
        error!("Failed to import image: {}", e);
        problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Image Import Failed")
            .with_detail(format!("Failed to import Docker image: {}", e))
    })?;

    info!(
        "Successfully imported image with ID: {}, tag: {}",
        imported_image_id, image_tag
    );

    // 7. Generate deployment slug
    let deployment_number = deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(project_id))
        .count(state.db.as_ref())
        .await
        .unwrap_or(0)
        + 1;

    let env_slug = format!("{}-{}", environment.slug, deployment_number);

    // 8. Get deployment config from environment
    let deployment_config_snapshot = environment.deployment_config.as_ref().map(|config| {
        temps_entities::deployment_config::DeploymentConfigSnapshot::from_config(
            config,
            std::collections::HashMap::new(),
        )
    });

    // 9. Build deployment metadata
    let deployment_metadata = DeploymentMetadata {
        external_image_ref: Some(image_tag.clone()),
        external_image_id: None,
        deployment_source_type: Some(SourceType::DockerImage),
        ..Default::default()
    };

    // 10. Create deployment record
    let now = Utc::now();
    let new_deployment = deployments::ActiveModel {
        project_id: Set(project_id),
        environment_id: Set(environment_id),
        slug: Set(env_slug),
        state: Set("pending".to_string()),
        metadata: Set(Some(deployment_metadata)),
        context_vars: Set(Some(serde_json::json!({
            "trigger": "image_upload",
            "source": "api",
            "image_tag": image_tag,
            "imported_image_id": imported_image_id,
        }))),
        image_name: Set(Some(image_tag.clone())),
        deployment_config: Set(deployment_config_snapshot),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };

    let deployment = new_deployment
        .insert(state.db.as_ref())
        .await
        .map_err(|e| {
            error!("Failed to create deployment: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Deployment Creation Failed")
                .with_detail(e.to_string())
        })?;

    info!(
        "Created deployment {} for image upload deployment",
        deployment.id
    );

    // 11. Fire DeploymentCreated event
    let deployment_created_event = Job::DeploymentCreated(DeploymentCreatedJob {
        deployment_id: deployment.id,
        project_id: project.id,
        environment_id: environment.id,
        environment_name: environment.name.clone(),
        branch: None,
        commit_sha: None,
    });
    if let Err(e) = state.queue_service.send(deployment_created_event).await {
        error!("Failed to send DeploymentCreated event: {}", e);
    }

    // Update project's last_deployment timestamp
    let mut active_project: projects::ActiveModel = project.into();
    active_project.last_deployment = Set(Some(Utc::now()));
    if let Err(e) = active_project.update(state.db.as_ref()).await {
        error!(
            "Failed to update last_deployment for project {}: {}",
            project_id, e
        );
    } else {
        debug!(
            "Updated last_deployment timestamp for project {}",
            project_id
        );
    }

    // 12. Create jobs using WorkflowPlanner
    let create_jobs_result = state
        .workflow_planner
        .create_deployment_jobs(deployment.id)
        .await;

    match create_jobs_result {
        Ok(created_jobs) => {
            info!(
                "Created {} jobs for deployment {}",
                created_jobs.len(),
                deployment.id
            );

            // Update deployment status to Running
            if let Err(e) =
                crate::services::job_processor::JobProcessorService::update_deployment_status(
                    &state.db,
                    deployment.id,
                    PipelineStatus::Running,
                )
                .await
            {
                error!("Failed to update deployment status: {}", e);
            }

            // Execute the workflow in background
            let workflow_executor = state.workflow_executor.clone();
            let deployment_id = deployment.id;
            let db = state.db.clone();
            tokio::spawn(async move {
                match workflow_executor
                    .execute_deployment_workflow(deployment_id)
                    .await
                {
                    Ok(_) => {
                        info!(
                            "Workflow execution completed for deployment {}",
                            deployment_id
                        );
                    }
                    Err(e) => {
                        error!(
                            "Workflow execution failed for deployment {}: {}",
                            deployment_id, e
                        );
                        // Update deployment status to Failed
                        if let Err(e) =
                            crate::services::job_processor::JobProcessorService::update_deployment_status(
                                &db,
                                deployment_id,
                                PipelineStatus::Failed,
                            )
                            .await
                        {
                            error!("Failed to update deployment status: {}", e);
                        }
                    }
                }
            });
        }
        Err(e) => {
            error!(
                "Failed to create jobs for deployment {}: {}",
                deployment.id, e
            );
            // Update deployment status to Failed
            if let Err(e) =
                crate::services::job_processor::JobProcessorService::update_deployment_status(
                    &state.db,
                    deployment.id,
                    PipelineStatus::Failed,
                )
                .await
            {
                error!("Failed to update deployment status: {}", e);
            }

            return Err(problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Job Creation Failed")
                .with_detail(e.to_string()));
        }
    }

    // 13. Return deployment info
    Ok((
        StatusCode::ACCEPTED,
        Json(DeploymentResponse {
            id: deployment.id,
            project_id: deployment.project_id,
            environment_id: deployment.environment_id,
            slug: deployment.slug,
            state: deployment.state,
            source_type: "docker_image_upload".to_string(),
            created_at: deployment.created_at,
        }),
    ))
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
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsCreate);

    debug!("Uploading static bundle for project {}", project_id);

    // 1. Verify project exists and has StaticFiles source type
    let project = projects::Entity::find_by_id(project_id)
        .one(state.db.as_ref())
        .await
        .map_err(|e| {
            error!("Database error: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Database Error")
                .with_detail(e.to_string())
        })?
        .ok_or_else(|| {
            problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Project Not Found")
                .with_detail(format!("Project {} not found", project_id))
        })?;

    // Verify project source type allows static file deployments
    if !project
        .source_type
        .allows_deployment_method(&SourceType::StaticFiles)
    {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Project Type")
            .with_detail(format!(
                "Project with source type {:?} does not allow static file deployments",
                project.source_type
            )));
    }

    // 2. Read the uploaded file from multipart
    let mut file_data: Option<bytes::Bytes> = None;
    let mut original_filename: Option<String> = None;
    let mut content_type: Option<String> = None;
    let mut explicit_content_type: Option<String> = None; // From form field
    let mut metadata: Option<serde_json::Value> = None;

    // Maximum file size: 500MB
    const MAX_FILE_SIZE: usize = 500 * 1024 * 1024;

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        error!("Multipart error: {}", e);
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Multipart Error")
            .with_detail(e.to_string())
    })? {
        let name = field.name().unwrap_or_default().to_string();

        match name.as_str() {
            "file" => {
                original_filename = field.file_name().map(|s| s.to_string());
                content_type = field.content_type().map(|s| s.to_string());

                let data = field.bytes().await.map_err(|e| {
                    error!("Failed to read file data: {}", e);
                    problemdetails::new(StatusCode::BAD_REQUEST)
                        .with_title("File Read Error")
                        .with_detail(e.to_string())
                })?;

                if data.len() > MAX_FILE_SIZE {
                    return Err(problemdetails::new(StatusCode::PAYLOAD_TOO_LARGE)
                        .with_title("File Too Large")
                        .with_detail(format!(
                            "File size {} exceeds maximum allowed size of {}",
                            data.len(),
                            MAX_FILE_SIZE
                        )));
                }

                file_data = Some(data);
            }
            "metadata" => {
                let data = field.text().await.map_err(|e| {
                    error!("Failed to read metadata: {}", e);
                    problemdetails::new(StatusCode::BAD_REQUEST)
                        .with_title("Metadata Error")
                        .with_detail(e.to_string())
                })?;

                metadata = serde_json::from_str(&data).ok();
            }
            "content_type" => {
                // Explicit content_type field from CLI (more reliable than multipart header)
                let data = field.text().await.map_err(|e| {
                    error!("Failed to read content_type field: {}", e);
                    problemdetails::new(StatusCode::BAD_REQUEST)
                        .with_title("Content Type Error")
                        .with_detail(e.to_string())
                })?;
                explicit_content_type = Some(data);
            }
            _ => {
                // Ignore other fields
            }
        }
    }

    // Prefer explicit content_type field over multipart header
    if explicit_content_type.is_some() {
        content_type = explicit_content_type;
    }

    // Ensure file was provided
    let file_bytes = file_data.ok_or_else(|| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Missing File")
            .with_detail("No file was provided in the multipart request")
    })?;

    let size_bytes = file_bytes.len() as i64;

    // 3. Validate content type (tar.gz or zip)
    // Detect from filename if content type is missing or generic (application/octet-stream)
    let detected_content_type = match content_type.as_deref() {
        Some(ct) if ct != "application/octet-stream" => ct.to_string(),
        _ => {
            // Detect from filename extension
            if let Some(ref filename) = original_filename {
                if filename.ends_with(".tar.gz") || filename.ends_with(".tgz") {
                    "application/gzip".to_string()
                } else if filename.ends_with(".zip") {
                    "application/zip".to_string()
                } else if filename.ends_with(".tar") {
                    "application/x-tar".to_string()
                } else {
                    "application/octet-stream".to_string()
                }
            } else {
                "application/octet-stream".to_string()
            }
        }
    };

    // Validate content type
    let valid_types = [
        "application/gzip",
        "application/x-gzip",
        "application/zip",
        "application/x-tar",
    ];
    if !valid_types
        .iter()
        .any(|t| detected_content_type.contains(t))
    {
        // Check filename extension as fallback
        let has_valid_extension = original_filename
            .as_ref()
            .map(|f| {
                f.ends_with(".tar.gz")
                    || f.ends_with(".tgz")
                    || f.ends_with(".zip")
                    || f.ends_with(".tar")
            })
            .unwrap_or(false);

        if !has_valid_extension {
            return Err(problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Unsupported Format")
                .with_detail(format!(
                    "Unsupported file format. Expected tar.gz or zip, got content-type: {}",
                    detected_content_type
                )));
        }
    }

    // 4. Generate blob path and upload to blob storage
    let bundle_id = uuid::Uuid::new_v4();
    let extension = original_filename
        .as_ref()
        .and_then(|f| {
            if f.ends_with(".tar.gz") {
                Some("tar.gz")
            } else if f.ends_with(".tgz") {
                Some("tgz")
            } else if f.ends_with(".zip") {
                Some("zip")
            } else if f.ends_with(".tar") {
                Some("tar")
            } else {
                None
            }
        })
        .unwrap_or("tar.gz");

    let blob_path = format!("static-bundles/{}.{}", bundle_id, extension);

    // Calculate checksum
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(&file_bytes);
    let checksum = format!("sha256:{:x}", hasher.finalize());

    // Store static bundle in local data directory
    let local_path = state.data_dir.join(&blob_path);

    // Ensure parent directory exists
    if let Some(parent) = local_path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            error!("Failed to create directory for static bundle: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Upload Failed")
                .with_detail(format!("Failed to create storage directory: {}", e))
        })?;
    }

    // Write file to local filesystem
    tokio::fs::write(&local_path, &file_bytes)
        .await
        .map_err(|e| {
            error!("Failed to write static bundle to local storage: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Upload Failed")
                .with_detail(format!("Failed to write file to local storage: {}", e))
        })?;

    info!(
        "Uploaded static bundle to local storage: {} ({} bytes)",
        local_path.display(),
        size_bytes
    );

    // 5. Register in static_bundles table via RemoteDeploymentService
    let upload_request = crate::services::UploadStaticBundleRequest {
        original_filename,
        content_type: Some(detected_content_type),
        metadata,
    };

    let bundle_info = state
        .remote_deployment_service
        .register_static_bundle(
            project_id,
            blob_path,
            size_bytes,
            upload_request,
            Some(checksum),
        )
        .await
        .map_err(|e| {
            error!("Failed to register static bundle: {}", e);
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Registration Failed")
                .with_detail(e.to_string())
        })?;

    info!(
        "Registered static bundle (id={}) for project {}",
        bundle_info.id, project_id
    );

    // 6. Return bundle info
    Ok((
        StatusCode::CREATED,
        Json(StaticBundleResponse::from(bundle_info)),
    ))
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
        .route(
            "/projects/{project_id}/environments/{environment_id}/deploy/image-upload",
            post(deploy_from_image_upload),
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
