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
    routing::{delete, get, patch, post, put},
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
    UpdateAutomaticDeployRequest, UpdateDeploymentConfigRequest, UpdateGitSettingsRequest,
    UpdateProjectSettingsRequest,
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
        // Create project from template
        .route(
            "/projects/from-template",
            post(create_project_from_template),
        )
        // Presets route
        .route("/presets", get(list_presets))
        // Template routes
        .route("/templates", get(list_templates))
        .route("/templates/tags", get(list_template_tags))
        .route("/templates/{slug}", get(get_template))
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
        .route(
            "/projects/{project_id}/deployment-config",
            patch(update_project_deployment_config),
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
        update_project_deployment_config,
        trigger_project_pipeline,
        get_project_statistics,
        list_presets,
        list_templates,
        get_template,
        list_template_tags,
        create_project_from_template,
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
            UpdateDeploymentConfigRequest,
            TriggerPipelinePayload,
            TriggerPipelineResponse,
            ProjectStatisticsResponse,
            super::types::PresetResponse,
            super::types::ListPresetsResponse,
            super::templates::ListTemplatesQuery,
            super::templates::TemplateResponse,
            super::templates::GitRefResponse,
            super::templates::EnvVarTemplateResponse,
            super::templates::ListTemplatesResponse,
            super::templates::ListTagsResponse,
            super::templates::CreateProjectFromTemplateRequest,
            super::templates::EnvVarInput,
            super::templates::CreateProjectFromTemplateResponse,
        )
    ),
    tags(
        (name = "Projects", description = "Project management endpoints"),
        (name = "Presets", description = "Available deployment presets"),
        (name = "Templates", description = "Project template endpoints")
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
        preset_config: project.preset_config,
        environment_variables: project.environment_variables,
        automatic_deploy: project.automatic_deploy.unwrap_or(false),
        storage_service_ids: project.storage_service_ids,
        is_public_repo: project.is_public_repo,
        git_url: project.git_url,
        git_provider_connection_id: project.git_provider_connection_id,
        exposed_port: project.exposed_port,
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
        preset_config: project.preset_config.clone(),
        environment_variables: project.environment_variables.clone(),
        automatic_deploy: project.automatic_deploy.unwrap_or(false),
        storage_service_ids: project.storage_service_ids.clone(),
        is_public_repo: None,               // Keep existing setting
        git_url: None,                      // Keep existing setting
        git_provider_connection_id: None,   // Keep existing setting
        exposed_port: project.exposed_port, // Keep existing or update if provided
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
            settings.attack_mode,
            settings.enable_preview_environments,
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

/// Update deployment configuration for a project
#[utoipa::path(
    patch,
    path = "/projects/{project_id}/deployment-config",
    tag = "Projects",
    request_body = UpdateDeploymentConfigRequest,
    responses(
        (status = 200, description = "Deployment configuration updated successfully", body = ProjectResponse),
        (status = 400, description = "Invalid deployment configuration"),
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
pub async fn update_project_deployment_config(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(config): Json<UpdateDeploymentConfigRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);

    info!("Updating deployment config for project: {}", project_id);

    let updated_project = state
        .project_service
        .update_project_deployment_config(project_id, config.clone())
        .await
        .map_err(|e| {
            error!("Error updating deployment config: {:?}", e);
            Problem::from(e)
        })?;

    // Create audit event
    let audit_context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent,
    };

    let mut updated_fields = std::collections::HashMap::new();
    if config.cpu_request.is_some() {
        updated_fields.insert("cpu_request".to_string(), "updated".to_string());
    }
    if config.cpu_limit.is_some() {
        updated_fields.insert("cpu_limit".to_string(), "updated".to_string());
    }
    if config.memory_request.is_some() {
        updated_fields.insert("memory_request".to_string(), "updated".to_string());
    }
    if config.memory_limit.is_some() {
        updated_fields.insert("memory_limit".to_string(), "updated".to_string());
    }
    if config.exposed_port.is_some() {
        updated_fields.insert("exposed_port".to_string(), "updated".to_string());
    }
    if config.automatic_deploy.is_some() {
        updated_fields.insert("automatic_deploy".to_string(), "updated".to_string());
    }
    if config.performance_metrics_enabled.is_some() {
        updated_fields.insert(
            "performance_metrics_enabled".to_string(),
            "updated".to_string(),
        );
    }
    if config.session_recording_enabled.is_some() {
        updated_fields.insert(
            "session_recording_enabled".to_string(),
            "updated".to_string(),
        );
    }
    if config.replicas.is_some() {
        updated_fields.insert("replicas".to_string(), "updated".to_string());
    }
    if config.security.is_some() {
        updated_fields.insert("security".to_string(), "updated".to_string());
    }

    let audit_event = super::audit::DeploymentConfigUpdatedAudit {
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

    // Determine which environment to use: explicit payload or project's preview template environment
    let environment_id = if let Some(env_id) = payload.environment_id {
        env_id
    } else {
        return Err(temps_core::error_builder::bad_request()
            .detail("No environment specified and project has no preview template environment configured")
            .build());
    };

    // Get the environment for audit logging
    let environment = temps_entities::environments::Entity::find_by_id(environment_id)
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
    let (project_id, triggered_env_id, branch, tag, commit) = state
        .project_service
        .trigger_pipeline(
            id,
            environment_id,
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
        environment_id: triggered_env_id,
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

/// List all available presets
#[utoipa::path(
    get,
    path = "/presets",
    tag = "Presets",
    responses(
        (status = 200, description = "List of available presets", body = super::types::ListPresetsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_presets(RequireAuth(_auth): RequireAuth) -> Result<impl IntoResponse, Problem> {
    // No permission check needed - all authenticated users can list presets

    // Get all presets from temps-presets crate
    let presets: Vec<super::types::PresetResponse> = temps_presets::all_presets()
        .into_iter()
        .map(|preset| {
            let slug = preset.slug();
            let label = preset.label();
            let description = preset.description();
            let project_type = preset.project_type().to_string();
            let default_port = Some(preset.default_port());

            // Generate relative icon URL
            let icon_url = format!("/presets/{}.svg", slug);

            super::types::PresetResponse {
                slug,
                label,
                icon_url,
                project_type,
                description,
                default_port,
            }
        })
        .collect();

    let total = presets.len();

    let response = super::types::ListPresetsResponse { presets, total };

    Ok(Json(response))
}

// ============================================================================
// Template Handlers
// ============================================================================

/// List all available templates
///
/// Returns a list of all public templates, optionally filtered by tag or featured status.
#[utoipa::path(
    get,
    path = "/templates",
    tag = "Templates",
    params(
        ("tag" = Option<String>, Query, description = "Filter templates by tag"),
        ("featured" = Option<bool>, Query, description = "Only return featured templates")
    ),
    responses(
        (status = 200, description = "List of templates", body = super::templates::ListTemplatesResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_templates(
    State(state): State<Arc<AppState>>,
    RequireAuth(_auth): RequireAuth,
    Query(query): Query<super::templates::ListTemplatesQuery>,
) -> Result<impl IntoResponse, Problem> {
    let templates = if let Some(true) = query.featured {
        state.template_service.list_featured_templates().await
    } else if let Some(tag) = query.tag {
        state.template_service.list_templates_by_tag(&tag).await
    } else {
        state.template_service.list_templates().await
    };

    let total = templates.len();
    let response = super::templates::ListTemplatesResponse {
        templates: templates
            .into_iter()
            .map(super::templates::TemplateResponse::from)
            .collect(),
        total,
    };

    Ok(Json(response))
}

/// Get a specific template by slug
///
/// Returns detailed information about a single template.
#[utoipa::path(
    get,
    path = "/templates/{slug}",
    tag = "Templates",
    params(
        ("slug" = String, Path, description = "Template slug")
    ),
    responses(
        (status = 200, description = "Template details", body = super::templates::TemplateResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Template not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_template(
    State(state): State<Arc<AppState>>,
    RequireAuth(_auth): RequireAuth,
    Path(slug): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    let template = state
        .template_service
        .get_template(&slug)
        .await
        .map_err(|e| {
            problemdetails::new(http::StatusCode::NOT_FOUND)
                .with_title("Template Not Found")
                .with_detail(e.to_string())
        })?;

    Ok(Json(super::templates::TemplateResponse::from(template)))
}

/// List all available template tags
///
/// Returns a list of all unique tags used by public templates.
#[utoipa::path(
    get,
    path = "/templates/tags",
    tag = "Templates",
    responses(
        (status = 200, description = "List of tags", body = super::templates::ListTagsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_template_tags(
    State(state): State<Arc<AppState>>,
    RequireAuth(_auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    let tags = state.template_service.list_tags().await;
    let total = tags.len();

    Ok(Json(super::templates::ListTagsResponse { tags, total }))
}

/// Create a new project from a template
///
/// Creates a new repository from a template and sets up the project with the
/// specified configuration. The template is cloned to a new repository under
/// the authenticated user's account or specified organization.
#[utoipa::path(
    post,
    path = "/projects/from-template",
    tag = "Projects",
    request_body = super::templates::CreateProjectFromTemplateRequest,
    responses(
        (status = 201, description = "Project created successfully", body = super::templates::CreateProjectFromTemplateResponse),
        (status = 400, description = "Invalid input"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Template not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_project_from_template(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<super::templates::CreateProjectFromTemplateRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsCreate);

    // 1. Get the template
    let template = state
        .template_service
        .get_template(&request.template_slug)
        .await
        .map_err(|e| {
            problemdetails::new(http::StatusCode::NOT_FOUND)
                .with_title("Template Not Found")
                .with_detail(e.to_string())
        })?;

    // 2. Determine the repository owner (use provided or get from git provider connection)
    let repo_owner = request.repository_owner.clone().unwrap_or_else(|| {
        // Default to repository name if owner not provided
        // In production, this would query the git provider for the authenticated user
        request.repository_name.clone()
    });

    // 3. Build the environment variables from the request
    let env_vars: Option<Vec<(String, String)>> = if request.environment_variables.is_empty() {
        None
    } else {
        Some(
            request
                .environment_variables
                .iter()
                .map(|ev| (ev.name.clone(), ev.value.clone()))
                .collect(),
        )
    };

    // 4. Create the project using the project service
    // Note: The actual repository creation from template would be done by the
    // git provider integration (cloning the template repo to the new repo)
    let create_request = crate::services::types::CreateProjectRequest {
        name: request.project_name.clone(),
        repo_name: Some(request.repository_name.clone()),
        repo_owner: Some(repo_owner.clone()),
        directory: template.git.path.clone().unwrap_or_else(|| ".".to_string()),
        main_branch: template.git.r#ref.clone(),
        preset: template.preset.clone(),
        preset_config: template.preset_config.clone(),
        environment_variables: env_vars,
        automatic_deploy: request.automatic_deploy,
        storage_service_ids: request.storage_service_ids.clone(),
        is_public_repo: Some(!request.private),
        git_url: Some(template.git.url.clone()),
        git_provider_connection_id: Some(request.git_provider_connection_id),
        exposed_port: None,
    };

    let project = state
        .project_service
        .create_project(create_request)
        .await
        .map_err(Problem::from)?;

    // 5. Create audit event
    let audit_context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent,
    };

    let audit_event = ProjectCreatedAudit {
        context: audit_context,
        project_id: project.id,
        project_name: project.name.clone(),
        project_slug: project.slug.clone(),
        repo_name: project.repo_name.clone(),
        repo_owner: project.repo_owner.clone(),
        directory: project.directory.clone(),
        main_branch: project.main_branch.clone(),
        preset: project.preset.clone(),
        automatic_deploy: project.automatic_deploy,
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit_event).await {
        error!("Failed to create audit log: {:?}", e);
    }

    // 6. Build the repository URL (for now, construct it from the template URL pattern)
    // In production, this would be the actual URL of the newly created repository
    let repository_url = format!(
        "https://github.com/{}/{}.git",
        repo_owner, request.repository_name
    );

    let response = super::templates::CreateProjectFromTemplateResponse {
        project_id: project.id,
        project_slug: project.slug,
        project_name: project.name,
        repository_url,
        template_slug: request.template_slug,
        message: format!(
            "Project created successfully from template. Services required: {:?}",
            template.services
        ),
    };

    Ok((StatusCode::CREATED, Json(response)))
}
