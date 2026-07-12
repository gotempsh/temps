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
use temps_auth::RequireAuth;
use temps_auth::{
    permission_guard, project_access_guard, project_permission_guard, project_scope_guard,
};
use temps_core::RequestMetadata;
use tracing::{debug, error, info};

use super::types::{
    ChangeProjectSourceRequest, CreateProjectRequest, PaginatedProjectList, PaginationParams,
    ProjectResponse, ProjectStatisticsResponse, ReinstallWebhookResponse, TriggerPipelinePayload,
    TriggerPipelineResponse, UpdateAutomaticDeployRequest, UpdateDeploymentConfigRequest,
    UpdateGitSettingsRequest, UpdateProjectSettingsRequest,
};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use temps_core::problemdetails;
use temps_core::problemdetails::Problem;
use temps_entities::source_type::SourceType;

pub fn configure_routes() -> Router<Arc<AppState>> {
    let custom_domain_routes = super::custom_domains::configure_routes();

    Router::new()
        // Project CRUD routes
        .route("/projects/{id}", get(get_project))
        .route("/projects/by-slug/{slug}", get(get_project_by_slug))
        .route("/projects/{id}", put(update_project))
        .route("/projects/{id}/source", patch(change_project_source))
        .route("/projects/{id}", delete(delete_project))
        .route("/projects", post(create_project))
        .route("/projects", get(get_projects))
        .route("/projects/statistics", get(get_project_statistics))
        // Create project from template
        .route(
            "/projects/from-template",
            post(create_project_from_template),
        )
        // Presets routes
        .route("/presets", get(list_presets))
        .route(
            "/presets/{slug}/dockerfile",
            post(generate_preset_dockerfile),
        )
        // Template routes
        .route("/templates", get(list_project_templates))
        .route("/templates/tags", get(list_project_template_tags))
        .route("/templates/{slug}", get(get_project_template))
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
        .route(
            "/projects/{project_id}/gitlab/reinstall-webhook",
            post(reinstall_gitlab_webhook),
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
        change_project_source,
        delete_project,
        get_projects,
        get_project_by_slug,
        update_project_settings,
        update_git_settings,
        update_automatic_deploy,
        update_project_deployment_config,
        reinstall_gitlab_webhook,
        trigger_project_pipeline,
        get_project_statistics,
        list_presets,
        generate_preset_dockerfile,
        list_project_templates,
        get_project_template,
        list_project_template_tags,
        create_project_from_template,
    ),
    components(
        schemas(
            CreateProjectRequest,
            ChangeProjectSourceRequest,
            ProjectResponse,
            PaginatedProjectList,
            PaginationParams,
            UpdateProjectSettingsRequest,
            UpdateGitSettingsRequest,
            UpdateAutomaticDeployRequest,
            UpdateDeploymentConfigRequest,
            ReinstallWebhookResponse,
            TriggerPipelinePayload,
            TriggerPipelineResponse,
            ProjectStatisticsResponse,
            super::types::PresetResponse,
            super::types::ListPresetsResponse,
            super::types::GenerateDockerfileRequest,
            super::types::GenerateDockerfileResponse,
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

    // Only require repo_name and repo_owner for Git source type
    // For docker_image and static_files, Git info is optional
    if project.source_type.requires_git_info()
        && (project.repo_name.is_none() || project.repo_owner.is_none())
    {
        return Err(problemdetails::new(http::StatusCode::BAD_REQUEST)
            .with_title("Missing Repository Information")
            .with_detail(
                "For Git-based projects, both repo_name and repo_owner must be specified. \
                Use source_type 'docker_image' or 'static_files' for Git-less deployments.",
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
        source_type: project.source_type,
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

    state.telemetry.report(
        temps_core::telemetry::TelemetryEvent::new(
            temps_core::telemetry::TelemetryEventKind::ProjectCreated,
        )
        .with("source_type", new_project.source_type.to_string())
        .with_opt("preset", new_project.preset.clone()),
    );

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
    permission_guard!(auth, ProjectsRead); // 1. instance-wide role check
    project_scope_guard!(auth, id); // 2. deployment-token IDOR check
    project_access_guard!(auth, id, state.project_access_checker); // 3. team-based access

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
    permission_guard!(auth, ProjectsRead); // 1. instance-wide role check

    debug!("get project by slug called with slug: {}", slug);
    // Resolve the project first so we have the numeric ID for the guards.
    // Guards must run on the resolved ID — not skipped because the caller
    // used a slug instead of an ID path.
    let project = state.project_service.get_project_by_slug(&slug).await?;
    project_scope_guard!(auth, project.id); // 2. deployment-token IDOR check
    project_access_guard!(auth, project.id, state.project_access_checker); // 3. team-based access
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
    project_permission_guard!(auth, ProjectsWrite, id, state.project_access_checker);
    project_scope_guard!(auth, id);

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
        source_type: project.source_type,   // Preserve source type
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

/// Change a project's source type to a Git-less type (docker_image /
/// static_files / manual). Switching TO Git is done via the Git settings
/// endpoint (`POST /projects/{id}/git`), which also supplies the repository and
/// provider connection.
#[utoipa::path(
    patch,
    path = "/projects/{id}/source",
    tag = "Projects",
    params(("id" = i32, Path, description = "Project ID")),
    request_body = ChangeProjectSourceRequest,
    responses(
        (status = 200, description = "Source type changed", body = ProjectResponse),
        (status = 400, description = "Invalid source type change (e.g. switching to Git here)"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn change_project_source(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(req): Json<super::types::ChangeProjectSourceRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, id);
    project_access_guard!(auth, id, state.project_access_checker);

    let updated = state
        .project_service
        .set_source_type(id, req.source_type)
        .await?;

    let audit_event = ProjectUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.to_string()),
            user_agent: metadata.user_agent,
        },
        project_id: updated.id,
        project_name: updated.name.clone(),
        project_slug: updated.slug.clone(),
        updated_fields: ProjectUpdatedFields {
            name: Some(updated.name.clone()),
            repo_name: None,
            repo_owner: None,
            directory: None,
            main_branch: None,
            preset: None,
            automatic_deploy: None,
        },
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit_event).await {
        error!("Failed to create audit log: {:?}", e);
    }

    Ok(Json(ProjectResponse::map_from_project(updated)).into_response())
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
    project_permission_guard!(auth, ProjectsDelete, id, state.project_access_checker);
    project_scope_guard!(auth, id);

    // Get project details before deletion
    let project = state.project_service.get_project(id).await?;

    state
        .project_service
        .delete_project(id, &project.name)
        .await?;

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
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

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
            settings.preview_envs_on_demand,
            settings.preview_envs_idle_timeout_seconds,
            settings.preview_envs_wake_timeout_seconds,
            settings.preset_config.clone(),
            settings.ai_alert_summaries_enabled,
            settings.ai_debug_chat_enabled,
            settings.ai_write_actions_enabled,
            settings.cross_project_trace_sharing,
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
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

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

    // Anonymous telemetry: only when auto-deploy is turned ON (adoption signal).
    if request.automatic_deploy {
        state
            .telemetry
            .report(temps_core::telemetry::TelemetryEvent::new(
                temps_core::telemetry::TelemetryEventKind::AutoDeployEnabled,
            ));
    }

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
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

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
            settings.preset_config.clone(),
            settings.git_url.clone(),
            settings.is_public_repo,
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

/// Reinstall the GitLab webhook for a project
///
/// Removes the existing webhook (if any) and installs a fresh one.
/// Use this when a webhook has been manually deleted on the GitLab side
/// and automatic deployments have stopped working.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/gitlab/reinstall-webhook",
    tag = "Projects",
    responses(
        (status = 200, description = "Webhook reinstalled", body = ReinstallWebhookResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Project not found"),
        (status = 400, description = "Project is not connected to a GitLab repository"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn reinstall_gitlab_webhook(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    info!("Reinstalling GitLab webhook for project: {}", project_id);

    let hook_id = state
        .project_service
        .reinstall_gitlab_webhook(project_id)
        .await
        .map_err(|e| {
            error!("Error reinstalling GitLab webhook: {:?}", e);
            Problem::from(e)
        })?;

    // Audit log the reinstall.
    let audit_context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent,
    };

    let audit_event = ProjectUpdatedAudit {
        context: audit_context,
        project_id,
        project_name: format!("project-{}", project_id),
        project_slug: String::new(),
        updated_fields: ProjectUpdatedFields {
            name: None,
            repo_name: None,
            repo_owner: None,
            directory: None,
            main_branch: None,
            preset: None,
            automatic_deploy: None,
        },
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit_event).await {
        error!("Failed to create audit log: {:?}", e);
    }

    Ok(Json(ReinstallWebhookResponse {
        hook_id,
        message: "GitLab webhook reinstalled successfully".to_string(),
    }))
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
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

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
    project_scope_guard!(auth, id);
    project_access_guard!(auth, id, state.project_access_checker);

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

    // Get the environment for audit logging (only active environments)
    let environment = temps_entities::environments::Entity::find_by_id(environment_id)
        .filter(temps_entities::environments::Column::ProjectId.eq(id))
        .filter(temps_entities::environments::Column::DeletedAt.is_null())
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

/// Generate a Dockerfile from a preset
///
/// Returns the Dockerfile content and build arguments for a given preset slug.
/// The CLI can use this to build Docker images locally without needing a Dockerfile
/// in the project directory, enabling zero-config deployments.
#[utoipa::path(
    post,
    path = "/presets/{slug}/dockerfile",
    tag = "Presets",
    params(
        ("slug" = String, Path, description = "Preset slug (e.g., nextjs, vite, python)")
    ),
    request_body = super::types::GenerateDockerfileRequest,
    responses(
        (status = 200, description = "Generated Dockerfile", body = super::types::GenerateDockerfileResponse),
        (status = 404, description = "Preset not found"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn generate_preset_dockerfile(
    RequireAuth(_auth): RequireAuth,
    Path(slug): Path<String>,
    Json(request): Json<super::types::GenerateDockerfileRequest>,
) -> Result<impl IntoResponse, Problem> {
    let preset = temps_presets::get_preset_by_slug(&slug).ok_or_else(|| {
        problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("Preset Not Found")
            .with_detail(format!("No preset found with slug '{slug}'"))
    })?;

    // Create a temporary directory with the appropriate lockfile
    // so the preset can detect the package manager
    let temp_dir = tempfile::tempdir().map_err(|e| {
        problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Internal Error")
            .with_detail(format!("Failed to create temp directory: {e}"))
    })?;

    let temp_path = temp_dir.path();

    // Write the lockfile for the requested package manager
    let pm = request.package_manager.as_deref().unwrap_or("npm");
    let lockfile = match pm {
        "pnpm" => Some("pnpm-lock.yaml"),
        "yarn" => Some("yarn.lock"),
        "bun" => Some("bun.lock"),
        "npm" => Some("package-lock.json"),
        _ => Some("package-lock.json"),
    };

    if let Some(lockfile_name) = lockfile {
        std::fs::write(temp_path.join(lockfile_name), "").map_err(|e| {
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Error")
                .with_detail(format!("Failed to write lockfile: {e}"))
        })?;
    }

    // Write a minimal package.json so presets that read it don't fail
    std::fs::write(
        temp_path.join("package.json"),
        r#"{"name":"app","version":"1.0.0"}"#,
    )
    .map_err(|e| {
        problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Internal Error")
            .with_detail(format!("Failed to write package.json: {e}"))
    })?;

    let project_name = request.project_name.as_deref().unwrap_or("app");

    let install_cmd_owned = request.install_command.clone();
    let build_cmd_owned = request.build_command.clone();
    let output_dir_owned = request.output_dir.clone();
    let build_vars = Vec::new();

    let config = temps_presets::DockerfileConfig {
        root_local_path: temp_path,
        local_path: temp_path,
        install_command: install_cmd_owned.as_deref(),
        build_command: build_cmd_owned.as_deref(),
        output_dir: output_dir_owned.as_deref(),
        build_vars: Some(&build_vars),
        project_slug: project_name,
        use_buildkit: request.use_buildkit,
    };

    let result = preset.dockerfile(config).await;

    Ok(Json(super::types::GenerateDockerfileResponse {
        dockerfile: result.content,
        build_args: result.build_args,
        preset: slug,
    }))
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
    operation_id = "list_project_templates",
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
pub async fn list_project_templates(
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
    operation_id = "get_project_template",
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
pub async fn get_project_template(
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
    operation_id = "list_project_template_tags",
    responses(
        (status = 200, description = "List of tags", body = super::templates::ListTagsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_project_template_tags(
    State(state): State<Arc<AppState>>,
    RequireAuth(_auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    let tags = state.template_service.list_tags().await;
    let total = tags.len();

    Ok(Json(super::templates::ListTagsResponse { tags, total }))
}

/// Best-effort parse of `owner` and `repo` from a git URL for use as project
/// labels in the public-repo (one-click) deploy path.
///
/// These are NOT validated against any Git connection — the actual clone uses
/// the full `git_url`. They only need to be non-empty so the deploy pipeline
/// plans the download job and queues the initial deploy. Handles
/// `https://host/owner/repo(.git)` and `git@host:owner/repo(.git)` shapes;
/// falls back to `("template", "<repo-or-app>")` so both fields are always set.
fn parse_owner_repo_from_git_url(git_url: &str) -> (String, String) {
    // Normalize: strip scheme, an optional `git@host:` prefix, and `.git`.
    let trimmed = git_url.trim();
    let without_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    // For SCP-style `git@host:owner/repo`, drop everything up to and including ':'.
    let path_part = without_scheme
        .rsplit_once(':')
        .map(|(_, rest)| rest)
        .unwrap_or(without_scheme);
    let path_part = path_part.trim_end_matches('/');
    let path_part = path_part.strip_suffix(".git").unwrap_or(path_part);

    let mut segments = path_part.rsplit('/');
    let repo = segments.next().filter(|s| !s.is_empty());
    let owner = segments.next().filter(|s| !s.is_empty());

    match (owner, repo) {
        (Some(o), Some(r)) => (o.to_string(), r.to_string()),
        (None, Some(r)) => ("template".to_string(), r.to_string()),
        _ => ("template".to_string(), "app".to_string()),
    }
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

    // 2. Build the environment variables from the request (shared by both modes).
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

    // 3. Resolve the deploy mode, producing the project-create request, a
    //    source URL for the response, a non-identifying `deploy_mode` label for
    //    telemetry, and (image mode only) the image to deploy after creation.
    //
    //    Priority:
    //      * "image"       — the template carries a prebuilt image: create a
    //        docker_image project and pull/run the image (instant, no build).
    //        Wins over any Git connection — fastest activation path.
    //      * "fork"        — a Git connection is supplied: fork the template
    //        into the user's account and build from the fork.
    //      * "public_repo" — no connection: build straight from the template's
    //        public source repository.
    let (create_request, repository_url, deploy_mode, image_to_deploy): (
        crate::services::types::CreateProjectRequest,
        String,
        &'static str,
        Option<String>,
    ) = if let Some(image_ref) = template.image.clone().filter(|s| !s.is_empty()) {
        info!(
            "Deploying template {} from prebuilt image {} (image mode)",
            request.template_slug, image_ref
        );
        let req = crate::services::types::CreateProjectRequest {
            name: request.project_name.clone(),
            // No Git source — the image is pulled from its registry.
            repo_name: None,
            repo_owner: None,
            directory: ".".to_string(),
            main_branch: template.git.r#ref.clone(),
            preset: template.preset.clone(),
            preset_config: template.preset_config.clone(),
            environment_variables: env_vars,
            automatic_deploy: false,
            storage_service_ids: request.storage_service_ids.clone(),
            is_public_repo: None,
            git_url: None,
            git_provider_connection_id: None,
            exposed_port: template.exposed_port,
            // docker_image source skips the build pipeline entirely; the deploy
            // is triggered explicitly below via Job::DeployImageRequested.
            source_type: SourceType::DockerImage,
        };
        // Surface the template's source repo as the response URL (the image ref
        // isn't a browsable URL); the message clarifies it deployed from an image.
        (req, template.git.url.clone(), "image", Some(image_ref))
    } else {
        let (create_request, repository_url, deploy_mode) = match request.git_provider_connection_id
        {
            Some(connection_id) => {
                // Fork mode requires a repository name to create under the account.
                let repository_name = request.repository_name.as_deref().filter(|s| !s.is_empty());
                let Some(repository_name) = repository_name else {
                    return Err(temps_core::error_builder::bad_request()
                    .title("Repository Name Required")
                    .detail(
                        "repository_name is required when a Git provider connection is supplied",
                    )
                    .build());
                };

                info!(
                    "Creating repository {} from template {} (fork mode)",
                    repository_name, request.template_slug
                );

                let new_repo = state
                    .project_service
                    .git_provider_manager
                    .create_repository_and_push_template(
                        connection_id,
                        repository_name,
                        request.repository_owner.as_deref(),
                        Some(&format!("Created from template: {}", template.name)),
                        request.private,
                        &template.git.url,
                        &template.git.r#ref,
                        template.git.path.as_deref(),
                    )
                    .await
                    .map_err(|e| {
                        error!("Failed to create repository from template: {:?}", e);
                        // Forward the typed Problem (e.g. 409 for "name already exists",
                        // 401 for auth failures) instead of flattening everything to 500.
                        Problem::from(e)
                    })?;

                info!(
                    "Successfully created repository {} from template",
                    new_repo.full_name
                );

                // Point the project at the new fork. The template subfolder has been
                // flattened into the fork root by create_repository_and_push_template.
                let req = crate::services::types::CreateProjectRequest {
                    name: request.project_name.clone(),
                    repo_name: Some(new_repo.name.clone()),
                    repo_owner: Some(new_repo.owner.clone()),
                    directory: ".".to_string(),
                    main_branch: new_repo.default_branch.clone(),
                    preset: template.preset.clone(),
                    preset_config: template.preset_config.clone(),
                    environment_variables: env_vars,
                    automatic_deploy: request.automatic_deploy,
                    storage_service_ids: request.storage_service_ids.clone(),
                    is_public_repo: Some(!new_repo.private),
                    git_url: Some(new_repo.clone_url.clone()),
                    git_provider_connection_id: Some(connection_id),
                    exposed_port: None,
                    source_type: SourceType::Git,
                };
                (req, new_repo.clone_url, "fork")
            }
            None => {
                // One-click public-repo mode: no fork, no Git account. Deploy
                // directly from the template's public source repository. We clone
                // the whole public repo, so the project's build directory is the
                // template's subfolder (not flattened).
                info!(
                    "Deploying template {} directly from public repo {} (one-click mode)",
                    request.template_slug, template.git.url
                );

                let directory = template
                    .git
                    .path
                    .clone()
                    .filter(|p| !p.is_empty())
                    .unwrap_or_else(|| ".".to_string());

                // The deploy pipeline uses repo_owner/repo_name as labels and gates
                // the clone+initial-deploy on them being non-empty (they're NOT
                // validated against a Git connection — the actual clone uses
                // git_url). Derive them from the public URL so the public-repo
                // download job is planned and the first deploy fires automatically.
                let (repo_owner, repo_name) = parse_owner_repo_from_git_url(&template.git.url);

                let req = crate::services::types::CreateProjectRequest {
                    name: request.project_name.clone(),
                    repo_name: Some(repo_name),
                    repo_owner: Some(repo_owner),
                    directory,
                    main_branch: template.git.r#ref.clone(),
                    preset: template.preset.clone(),
                    preset_config: template.preset_config.clone(),
                    environment_variables: env_vars,
                    // Push webhooks can't reach a public upstream we don't own, so
                    // auto-deploy-on-push is meaningless here regardless of request.
                    automatic_deploy: false,
                    storage_service_ids: request.storage_service_ids.clone(),
                    is_public_repo: Some(true),
                    git_url: Some(template.git.url.clone()),
                    git_provider_connection_id: None,
                    exposed_port: None,
                    source_type: SourceType::Git,
                };
                (req, template.git.url.clone(), "public_repo")
            }
        };
        (create_request, repository_url, deploy_mode, None)
    };

    let project = state
        .project_service
        .create_project(create_request)
        .await
        .map_err(Problem::from)?;

    // 4. Image mode: docker_image projects don't auto-deploy on create (no Git
    //    push), so explicitly queue the image deploy. The deployments side
    //    resolves the target environment, pulls the image, and runs it — no
    //    build. Failure to enqueue is logged but doesn't fail project creation
    //    (the user can redeploy from the UI).
    if let Some(image_ref) = image_to_deploy {
        let deploy_job =
            temps_core::Job::DeployImageRequested(temps_core::DeployImageRequestedJob {
                project_id: project.id,
                image_ref: image_ref.clone(),
                health_check_path: template.health_check_path.clone(),
            });
        if let Err(e) = state.project_service.queue_service.send(deploy_job).await {
            error!(
                "Failed to queue image deploy for project {} (image {}): {}",
                project.id, image_ref, e
            );
        } else {
            info!(
                "Queued image deploy for project {} from image {}",
                project.id, image_ref
            );
        }
    }

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

    // 6. Anonymous telemetry. Emit both the generic project-created event (so the
    //    template path counts the same as any other project creation) and a
    //    template-specific one carrying the public, non-identifying template slug
    //    + deploy mode so we can measure which templates drive activation.
    state.telemetry.report(
        temps_core::telemetry::TelemetryEvent::new(
            temps_core::telemetry::TelemetryEventKind::ProjectCreated,
        )
        .with("source_type", project.source_type.to_string())
        .with_opt("preset", project.preset.clone()),
    );
    state.telemetry.report(
        temps_core::telemetry::TelemetryEvent::new(
            temps_core::telemetry::TelemetryEventKind::ProjectCreatedFromTemplate,
        )
        .with("template_slug", request.template_slug.clone())
        .with("deploy_mode", deploy_mode)
        .with("service_count", request.storage_service_ids.len() as i64),
    );

    // 7. Return the response with the source/repository URL.
    let deploy_note = match deploy_mode {
        "image" => "Deployed from the template's prebuilt image (no build).",
        "fork" => "Repository created and initialized with template code.",
        _ => "Deployed directly from the template's public source repository.",
    };
    let response = super::templates::CreateProjectFromTemplateResponse {
        project_id: project.id,
        project_slug: project.slug,
        project_name: project.name,
        repository_url,
        template_slug: request.template_slug,
        message: format!(
            "Project created successfully from template '{}'. {} Services required: {:?}",
            template.name, deploy_note, template.services
        ),
    };

    Ok((StatusCode::CREATED, Json(response)))
}

#[cfg(test)]
mod tests {
    use super::parse_owner_repo_from_git_url;

    /// Regression test for ADR-028 finding #2: `get_project_by_slug` guard bypass.
    ///
    /// Before the fix, `GET /projects/by-slug/{slug}` only called
    /// `permission_guard!` and skipped both `project_scope_guard!` and
    /// `project_access_guard!`. Any authenticated user with `ProjectsRead`
    /// could bypass team-access restrictions by using the slug endpoint
    /// instead of the numeric-ID endpoint — slugs are guessable (used in
    /// deployment URLs, CLI output, and webhook paths).
    ///
    /// After the fix, the handler resolves the project first and then applies
    /// both guards using the resolved `project.id`, matching the guard order
    /// in `get_project`.
    ///
    /// This test scans the handler source to verify both guards are present,
    /// which catches the regression if either is removed while leaving the
    /// rest of the function intact.
    #[test]
    fn get_project_by_slug_applies_scope_and_access_guards_on_resolved_id() {
        let source = include_str!("handlers.rs");

        // Locate the function body.
        let fn_start = source
            .find("pub async fn get_project_by_slug")
            .expect("get_project_by_slug handler not found in source");
        // Extract up to the start of the next pub async fn so we scope to
        // just this handler and avoid false-positives from other functions.
        let after_start = &source[fn_start + 1..];
        let next_fn_offset = after_start
            .find("pub async fn")
            .unwrap_or(after_start.len());
        let fn_body = &source[fn_start..fn_start + 1 + next_fn_offset];

        assert!(
            fn_body.contains("project_scope_guard!(auth, project.id)"),
            "get_project_by_slug must call project_scope_guard! on the resolved project.id \
             to block cross-project deployment-token IDOR via slug"
        );
        assert!(
            fn_body
                .contains("project_access_guard!(auth, project.id, state.project_access_checker)"),
            "get_project_by_slug must call project_access_guard! on the resolved project.id \
             to enforce team-based access (the same guard get_project applies)"
        );
    }

    #[test]
    fn parses_https_url_with_dot_git() {
        let (owner, repo) =
            parse_owner_repo_from_git_url("https://github.com/gotempsh/temps-examples.git");
        assert_eq!(owner, "gotempsh");
        assert_eq!(repo, "temps-examples");
    }

    #[test]
    fn parses_https_url_without_dot_git_and_trailing_slash() {
        let (owner, repo) = parse_owner_repo_from_git_url("https://gitlab.com/acme/widgets/");
        assert_eq!(owner, "acme");
        assert_eq!(repo, "widgets");
    }

    #[test]
    fn parses_scp_style_url() {
        let (owner, repo) = parse_owner_repo_from_git_url("git@github.com:gotempsh/temps.git");
        assert_eq!(owner, "gotempsh");
        assert_eq!(repo, "temps");
    }

    #[test]
    fn falls_back_when_single_segment() {
        // A bare single path segment (no owner) → owner falls back to
        // "template", repo preserved; both stay non-empty.
        let (owner, repo) = parse_owner_repo_from_git_url("loose.git");
        assert_eq!(owner, "template");
        assert_eq!(repo, "loose");
    }

    #[test]
    fn falls_back_to_non_empty_on_garbage() {
        let (owner, repo) = parse_owner_repo_from_git_url("not-a-url");
        assert!(!owner.is_empty());
        assert!(!repo.is_empty());
    }
}
