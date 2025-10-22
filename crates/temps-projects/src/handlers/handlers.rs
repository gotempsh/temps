use super::audit::{
    AuditContext, ProjectCreatedAudit, ProjectDeletedAudit, ProjectSettingsUpdatedAudit,
    ProjectSettingsUpdatedFields, ProjectUpdatedAudit, ProjectUpdatedFields,
};
use utoipa::OpenApi;

use super::AppState;
use axum::Router;
use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post, put},
    Json,
};
use std::sync::Arc;
use temps_auth::permission_guard;
use temps_auth::RequireAuth;
use temps_core::RequestMetadata;
use tracing::{debug, error, info};

use super::types::{
    CreateProjectRequest, PaginatedProjectList, PaginationParams, ProjectResponse,
    ProjectStatisticsResponse, TriggerPipelinePayload, TriggerPipelineResponse,
    UpdateAutomaticDeployRequest, UpdateGitSettingsRequest, UpdateProjectSettingsRequest,
};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use temps_core::problemdetails;
use temps_core::problemdetails::Problem;

pub fn configure_routes() -> Router<Arc<AppState>> {
    let custom_domain_routes = super::custom_domains::configure_routes();

    Router::new()
        // Project CRUD routes
        .route("/projects/{id}", get(get_project))
        .route("/projects/by-slug/{slug}", get(get_project_by_slug))
        .route("/projects/{id}", put(update_project))
        .route("/projects/{id}", delete(delete_project))
        .route("/projects", post(create_project))
        .route("/projects", get(get_projects))
        .route("/projects/statistics", get(get_project_statistics))
        // Pipeline trigger route
        .route(
            "/projects/{id}/trigger-pipeline",
            post(trigger_project_pipeline),
        )
        .route(
            "/projects/{project_id}/settings",
            post(update_project_settings),
        )
        .route("/projects/{project_id}/git", post(update_git_settings))
        .route(
            "/projects/{project_id}/automatic-deploy",
            post(update_automatic_deploy),
        )
        // Merge custom domain routes
        .merge(custom_domain_routes)
}

#[derive(OpenApi)]
#[openapi(
    paths(
        create_project,
        get_project,
        update_project,
        delete_project,
        get_projects,
        get_project_by_slug,
        update_project_settings,
        update_git_settings,
        update_automatic_deploy,
        trigger_project_pipeline,
        get_project_statistics,
    ),
    components(
        schemas(
            CreateProjectRequest,
            ProjectResponse,
            PaginatedProjectList,
            PaginationParams,
            UpdateProjectSettingsRequest,
            UpdateGitSettingsRequest,
            UpdateAutomaticDeployRequest,
            TriggerPipelinePayload,
            TriggerPipelineResponse,
            ProjectStatisticsResponse,
        )
    ),
    tags(
        (name = "Projects", description = "Project management endpoints")
    ),
    nest(
        (path = "/projects", api = super::custom_domains::CustomDomainsApiDoc)
    )
)]
pub struct ApiDoc;

/// Create a new project
#[utoipa::path(
    post,
    path = "/projects",
    tag = "Projects",
    request_body = CreateProjectRequest,
    responses(
        (status = 200, description = "Project created successfully", body = ProjectResponse),
        (status = 400, description = "Invalid input"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn create_project(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(project): Json<CreateProjectRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsCreate);

    // If git provider is specified, repo_name and repo_owner should also be provided
    if project.repo_name.is_none() || project.repo_owner.is_none() {
        return Err(problemdetails::new(http::StatusCode::BAD_REQUEST)
            .with_title("Missing Repository Information")
            .with_detail(
                "When using a git provider, both repo_name and repo_owner must be specified",
            ));
    }

    let project_req = crate::services::types::CreateProjectRequest {
        name: project.name,
        repo_name: project.repo_name,
        repo_owner: project.repo_owner,
        directory: project.directory,
        main_branch: project.main_branch,
        preset: project.preset,
        output_dir: project.output_dir,
        build_command: project.build_command,
        install_command: project.install_command,
        environment_variables: project.environment_variables,
        automatic_deploy: project.automatic_deploy.unwrap_or(false),
        project_type: project.project_type,
        is_web_app: project.is_web_app.unwrap_or(true),
        performance_metrics_enabled: project.performance_metrics_enabled,
        storage_service_ids: project.storage_service_ids,
        use_default_wildcard: project.use_default_wildcard,
        custom_domain: project.custom_domain,
        is_public_repo: project.is_public_repo,
        git_url: project.git_url,
        git_provider_connection_id: project.git_provider_connection_id,
        is_on_demand: Some(false),
    };

    let new_project = state
        .project_service
        .create_project(project_req)
        .await
        .map_err(Problem::from)?;

    // Create audit event
    let audit_context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent,
    };

    let audit_event = ProjectCreatedAudit {
        context: audit_context,
        project_id: new_project.id,
        project_name: new_project.name.clone(),
        project_slug: new_project.slug.clone(),
        repo_name: new_project.repo_name.clone(),
        repo_owner: new_project.repo_owner.clone(),
        directory: new_project.directory.clone(),
        main_branch: new_project.main_branch.clone(),
        preset: new_project.preset.clone(),
        automatic_deploy: new_project.automatic_deploy,
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit_event).await {
        error!("Failed to create audit log: {:?}", e);
        // Continue with the operation even if audit logging fails
    }

    Ok(Json(ProjectResponse::map_from_project(new_project)))
}

/// Get a list of all projects
#[utoipa::path(
    get,
    path = "/projects",
    tag = "Projects",
    params(
        ("page" = Option<i64>, Query, description = "Page number (1-based)"),
        ("per_page" = Option<i64>, Query, description = "Number of items per page")
    ),
    responses(
        (status = 200, description = "List of projects", body = PaginatedProjectList),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_projects(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Query(params): Query<PaginationParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);

    let page = params.page.unwrap_or(1);
    let per_page = params.per_page.unwrap_or(10);

    let (projects, total) = state
        .project_service
        .get_projects_paginated(page, per_page)
        .await
        .map_err(Problem::from)?;

    let response = PaginatedProjectList {
        projects: projects
            .into_iter()
            .map(super::types::ProjectResponse::map_from_project)
            .collect(),
        total,
        page,
        per_page,
    };

    Ok(Json(response))
}

/// Get details of a specific project
#[utoipa::path(
    get,
    params(
        ("id" = i32, Path, description = "Project ID")
    ),
    responses(
        (status = 200, description = "Project details", body = ProjectResponse),
        (status = 404, description = "Project not found")
    ),
    path = "/projects/{id}",
    tag = "Projects",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_project(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);

    info!("get project called with id: {}", id);
    let project = state
        .project_service
        .get_project(id)
        .await
        .map_err(Problem::from)?;

    Ok(Json(ProjectResponse::map_from_project(project)))
}

/// Get details of a specific project by slug
#[utoipa::path(
    get,
    params(
        ("slug" = String, Path, description = "Project slug"),
    ),
    tag = "Projects",
    responses(
        (status = 200, description = "Project details", body = ProjectResponse),
        (status = 404, description = "Project not found")
    ),
    path = "/projects/by-slug/{slug}",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_project_by_slug(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);

    debug!("get project by slug called with slug: {}", slug);
    let project = state.project_service.get_project_by_slug(&slug).await?;
    Ok(Json(ProjectResponse::map_from_project(project)).into_response())
}

#[utoipa::path(
    put,
    params(
        ("id" = i32, Path, description = "Project ID")
    ),
    path = "/projects/{id}",
    request_body = CreateProjectRequest,
    responses(
        (status = 200, description = "Project updated successfully", body = ProjectResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Projects",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn update_project(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(project): Json<CreateProjectRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);

    let project_req = crate::services::types::CreateProjectRequest {
        name: project.name.clone(),
        repo_name: project.repo_name.clone(),
        repo_owner: project.repo_owner.clone(),
        directory: project.directory.clone(),
        main_branch: project.main_branch.clone(),
        preset: project.preset.clone(),
        output_dir: project.output_dir.clone(),
        build_command: project.build_command.clone(),
        install_command: project.install_command.clone(),
        environment_variables: project.environment_variables.clone(),
        automatic_deploy: project.automatic_deploy.unwrap_or(false),
        project_type: project.project_type.clone(),
        is_web_app: project.is_web_app.unwrap_or(true),
        performance_metrics_enabled: project.performance_metrics_enabled,
        storage_service_ids: project.storage_service_ids.clone(),
        use_default_wildcard: None,       // Keep existing setting
        custom_domain: None,              // Keep existing setting
        is_public_repo: None,             // Keep existing setting
        git_url: None,                    // Keep existing setting
        git_provider_connection_id: None, // Keep existing setting
        is_on_demand: None,               // Keep existing setting
    };
    let updated_project = state
        .project_service
        .update_project(id, project_req)
        .await?;
    // Create audit event
    let audit_context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent,
    };

    let updated_fields = ProjectUpdatedFields {
        name: Some(project.name),
        repo_name: project.repo_name,
        repo_owner: project.repo_owner,
        directory: Some(project.directory),
        main_branch: Some(project.main_branch),
        preset: Some(project.preset),
        automatic_deploy: project.automatic_deploy,
    };

    let audit_event = ProjectUpdatedAudit {
        context: audit_context,
        project_id: updated_project.id,
        project_name: updated_project.name.clone(),
        project_slug: updated_project.slug.clone(),
        updated_fields,
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit_event).await {
        error!("Failed to create audit log: {:?}", e);
        // Continue with the operation even if audit logging fails
    }

    Ok(Json(ProjectResponse::map_from_project(updated_project)).into_response())
}

#[utoipa::path(
    delete,
    path = "/projects/{id}",
    tag = "Projects",
    params(
        ("id" = i32, Path, description = "Project ID")
    ),
    responses(
        (status = 204, description = "Project deleted successfully"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn delete_project(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsDelete);

    // Get project details before deletion
    let project = state.project_service.get_project(id).await?;

    state.project_service.delete_project(id).await?;

    // Create audit event
    let audit_context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent,
    };

    let audit_event = ProjectDeletedAudit {
        context: audit_context,
        project_id: project.id,
        project_name: project.name,
        project_slug: project.slug,
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit_event).await {
        error!("Failed to create audit log: {:?}", e);
        // Continue with the operation even if audit logging fails
    }

    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Update project settings
#[utoipa::path(
    post,
    path = "/projects/{project_id}/settings",
    tag = "Projects",
    request_body = UpdateProjectSettingsRequest,
    responses(
        (status = 200, description = "Project settings updated successfully", body = ProjectResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn update_project_settings(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(settings): Json<UpdateProjectSettingsRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);

    let updated_project = state
        .project_service
        .update_project_settings(
            project_id,
            settings.slug.clone(),
            settings.git_provider_connection_id,
            settings.main_branch.clone(),
            settings.repo_owner.clone(),
            settings.repo_name.clone(),
            settings.preset.clone(),
            settings.directory.clone(),
        )
        .await
        .map_err(Problem::from)?;

    // Create audit event
    let audit_context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent,
    };

    let updated_settings = ProjectSettingsUpdatedFields {
        cpu_request: None,
        cpu_limit: None,
        memory_request: None,
        memory_limit: None,
        performance_metrics_enabled: None,
        slug: settings.slug,
    };

    let audit_event = ProjectSettingsUpdatedAudit {
        context: audit_context,
        project_id: updated_project.id,
        project_name: updated_project.name.clone(),
        project_slug: updated_project.slug.clone(),
        updated_settings,
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit_event).await {
        error!("Failed to create audit log: {:?}", e);
        // Continue with the operation even if audit logging fails
    }

    Ok(Json(ProjectResponse::map_from_project(updated_project)))
}

/// Update automatic deployment setting for a project
#[utoipa::path(
    post,
    path = "/projects/{project_id}/automatic-deploy",
    tag = "Projects",
    request_body = UpdateAutomaticDeployRequest,
    responses(
        (status = 200, description = "Automatic deployment setting updated successfully", body = ProjectResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn update_automatic_deploy(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Json(request): Json<UpdateAutomaticDeployRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);

    info!(
        "Updating automatic deployment setting for project: {}",
        project_id
    );

    let updated_project = state
        .project_service
        .update_automatic_deploy(project_id, request.automatic_deploy)
        .await
        .map_err(|e| {
            error!("Error updating automatic deployment setting: {:?}", e);
            Problem::from(e)
        })?;

    Ok(Json(ProjectResponse::map_from_project(updated_project)))
}

/// Update git settings for a project
#[utoipa::path(
    post,
    path = "/projects/{project_id}/git",
    tag = "Projects",
    request_body = UpdateGitSettingsRequest,
    responses(
        (status = 200, description = "Git settings updated successfully", body = ProjectResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Project not found"),
        (status = 400, description = "Invalid git configuration or branch does not exist"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn update_git_settings(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(settings): Json<UpdateGitSettingsRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);

    info!(
        "Updating git settings for project: {} (branch: {}, repo: {}/{})",
        project_id, settings.main_branch, settings.repo_owner, settings.repo_name
    );

    let updated_project = state
        .project_service
        .update_git_settings(
            project_id,
            settings.git_provider_connection_id,
            settings.main_branch.clone(),
            settings.repo_owner.clone(),
            settings.repo_name.clone(),
            settings.preset.clone(),
            settings.directory.clone(),
        )
        .await
        .map_err(|e| {
            error!("Error updating git settings: {:?}", e);
            Problem::from(e)
        })?;

    // Create audit event
    let audit_context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent,
    };

    let updated_fields = ProjectUpdatedFields {
        name: None,
        repo_name: Some(settings.repo_name),
        repo_owner: Some(settings.repo_owner),
        directory: Some(settings.directory),
        main_branch: Some(settings.main_branch),
        preset: settings.preset,
        automatic_deploy: None,
    };

    let audit_event = ProjectUpdatedAudit {
        context: audit_context,
        project_id: updated_project.id,
        project_name: updated_project.name.clone(),
        project_slug: updated_project.slug.clone(),
        updated_fields,
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit_event).await {
        error!("Failed to create audit log: {:?}", e);
        // Continue with the operation even if audit logging fails
    }

    Ok(Json(ProjectResponse::map_from_project(updated_project)))
}

/// Trigger pipeline for a specific project
#[utoipa::path(
    post,
    path = "/projects/{id}/trigger-pipeline",
    params(
        ("id" = i32, Path, description = "Project ID"),
    ),
    request_body = TriggerPipelinePayload,
    responses(
        (status = 200, description = "Pipeline triggered successfully", body = TriggerPipelineResponse),
        (status = 404, description = "Project not found"),
        (status = 400, description = "Invalid request"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Projects"
)]
pub async fn trigger_project_pipeline(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(payload): Json<super::types::TriggerPipelinePayload>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);

    info!("Triggering pipeline for project with id: {}", id);

    // Get the project for audit logging
    let project = state.project_service.get_project(id).await?;

    // Get the environment for audit logging
    let environment = temps_entities::environments::Entity::find_by_id(payload.environment_id)
        .filter(temps_entities::environments::Column::ProjectId.eq(id))
        .one(state.project_service.db.as_ref())
        .await
        .map_err(|e| {
            temps_core::error_builder::internal_server_error()
                .detail(e.to_string())
                .build()
        })?
        .ok_or_else(|| {
            temps_core::error_builder::not_found()
                .detail("Environment not found or doesn't belong to project")
                .build()
        })?;

    // Create audit context
    let audit_context = super::audit::AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent,
    };

    // Create audit event
    let audit_event = super::audit::PipelineTriggeredAudit {
        context: audit_context,
        project_id: id,
        project_slug: project.slug.clone(),
        environment_id: environment.id,
        environment_slug: environment.slug.clone(),
        branch: payload.branch.clone(),
        tag: payload.tag.clone(),
        commit: payload.commit.clone(),
    };

    // Log the audit event
    if let Err(e) = state.audit_service.create_audit_log(&audit_event).await {
        error!("Failed to create audit log: {:?}", e);
        // Continue with the operation even if audit logging fails
    }

    // Trigger the pipeline
    let (project_id, environment_id, branch, tag, commit) = state
        .project_service
        .trigger_pipeline(
            id,
            payload.environment_id,
            payload.branch,
            payload.tag,
            payload.commit,
        )
        .await
        .map_err(|e| {
            error!("Error triggering pipeline: {:?}", e);
            Problem::from(e)
        })?;

    let response = super::types::TriggerPipelineResponse {
        message: "Pipeline triggered successfully".to_string(),
        project_id,
        environment_id,
        branch,
        tag,
        commit,
    };

    Ok(Json(response).into_response())
}

/// Get project statistics
#[utoipa::path(
    get,
    path = "/projects/statistics",
    tag = "Projects",
    responses(
        (status = 200, description = "Project statistics", body = ProjectStatisticsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_project_statistics(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);

    let statistics = state
        .project_service
        .get_project_statistics()
        .await
        .map_err(Problem::from)?;

    let response = ProjectStatisticsResponse {
        total_count: statistics.total_count,
    };

    Ok(Json(response))
}
