use super::audit::{
    EnvironmentDeletedAudit, EnvironmentSettingsUpdatedAudit, EnvironmentSettingsUpdatedFields,
};
use super::types::AppState;
use axum::Router;
use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post, put},
    Json,
};
use std::sync::Arc;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::AuditContext;
use temps_core::RequestMetadata;
use tracing::{error, info};
use utoipa::OpenApi;

use super::types::{
    AddEnvironmentDomainRequest, CreateEnvironmentRequest, CreateEnvironmentVariableRequest,
    EnvironmentDomainResponse, EnvironmentInfo, EnvironmentResponse, EnvironmentVariableResponse,
    EnvironmentVariableValueResponse, GetEnvironmentVariablesQuery,
    UpdateEnvironmentSettingsRequest,
};
use temps_core::problemdetails::Problem;

impl From<crate::services::env_var_service::EnvVarError> for Problem {
    fn from(err: crate::services::env_var_service::EnvVarError) -> Self {
        use crate::services::env_var_service::EnvVarError;
        match err {
            EnvVarError::NotFound(msg) => {
                temps_core::error_builder::not_found().detail(msg).build()
            }
            EnvVarError::InvalidInput(msg) => {
                temps_core::error_builder::bad_request().detail(msg).build()
            }
            EnvVarError::DatabaseConnectionError(msg) => {
                temps_core::error_builder::internal_server_error()
                    .detail(msg)
                    .build()
            }
            EnvVarError::DatabaseError { reason } => {
                temps_core::error_builder::internal_server_error()
                    .detail(reason)
                    .build()
            }
            EnvVarError::Other(msg) => temps_core::error_builder::internal_server_error()
                .detail(msg)
                .build(),
        }
    }
}

/// Get all environments for a project
#[utoipa::path(
    get,
    path = "/projects/{project_id}/environments",
    tag = "Projects",
    responses(
        (status = 200, description = "List of environments", body = Vec<EnvironmentResponse>),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID or slug")
    )
)]
pub async fn get_environments(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsRead);

    let environments = state
        .environment_service
        .get_environments(project_id)
        .await?;

    let response: Vec<EnvironmentResponse> = environments
        .into_iter()
        .map(EnvironmentResponse::from)
        .collect();

    Ok(Json(response))
}

/// Get a specific environment by ID or slug
#[utoipa::path(
    get,
    path = "/projects/{project_id}/environments/{env_id}",
    tag = "Projects",
    responses(
        (status = 200, description = "Environment details", body = EnvironmentResponse),
        (status = 404, description = "Project or environment not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID or slug"),
        ("env_id" = i32, Path, description = "Environment ID or slug")
    )
)]
pub async fn get_environment(
    State(state): State<Arc<AppState>>,
    Path((project_id, env_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsRead);

    let env = state
        .environment_service
        .get_environment(project_id, env_id)
        .await
        .map_err(Problem::from)?;

    Ok(Json(EnvironmentResponse::from(env)))
}

/// Get all environment domains for a specific environment
#[utoipa::path(
    get,
    path = "/projects/{project_id}/environments/{env_id}/domains",
    tag = "Projects",
    responses(
        (status = 200, description = "List of environment domains", body = Vec<EnvironmentDomainResponse>),
        (status = 404, description = "Project or environment not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID or slug"),
        ("env_id" = i32, Path, description = "Environment ID or slug")
    )
)]
pub async fn get_environment_domains(
    State(state): State<Arc<AppState>>,
    Path((project_id, env_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsRead);

    let domains = state
        .environment_service
        .get_environment_domains(project_id, env_id)
        .await
        .map_err(Problem::from)?;

    let mut response: Vec<EnvironmentDomainResponse> = Vec::new();
    for d in domains {
        let fqdn = state
            .environment_service
            .compute_environment_fqdn(&d.domain)
            .await;

        let url = state
            .environment_service
            .compute_environment_url(&d.domain)
            .await;

        response.push(EnvironmentDomainResponse {
            id: d.id,
            environment_id: d.environment_id,
            domain: fqdn,
            created_at: d.created_at.timestamp_millis(),
            url,
        });
    }

    Ok(Json(response))
}

/// Add a new environment domain
#[utoipa::path(
    post,
    path = "/projects/{project_id}/environments/{env_id}/domains",
    tag = "Projects",
    request_body = AddEnvironmentDomainRequest,
    responses(
        (status = 201, description = "Domain added successfully", body = EnvironmentDomainResponse),
        (status = 400, description = "Invalid input"),
        (status = 404, description = "Project or environment not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID or slug"),
        ("env_id" = i32, Path, description = "Environment ID or slug")
    )
)]
pub async fn add_environment_domain(
    State(state): State<Arc<AppState>>,
    Path((project_id, env_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
    Json(request): Json<AddEnvironmentDomainRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsWrite);

    let domain = state
        .environment_service
        .add_environment_domain(project_id, env_id, request.domain)
        .await
        .map_err(Problem::from)?;

    let fqdn = state
        .environment_service
        .compute_environment_fqdn(&domain.domain)
        .await;

    let url = state
        .environment_service
        .compute_environment_url(&domain.domain)
        .await;

    let response = EnvironmentDomainResponse {
        id: domain.id,
        environment_id: domain.environment_id,
        domain: fqdn,
        created_at: domain.created_at.timestamp_millis(),
        url,
    };

    Ok((StatusCode::CREATED, Json(response)))
}

/// Delete an environment domain
#[utoipa::path(
    delete,
    path = "/projects/{project_id}/environments/{env_id}/domains/{domain_id}",
    tag = "Projects",
    responses(
        (status = 204, description = "Domain deleted successfully"),
        (status = 404, description = "Project, environment, or domain not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID or slug"),
        ("env_id" = i32, Path, description = "Environment ID or slug"),
        ("domain_id" = i32, Path, description = "Domain ID")
    )
)]
pub async fn delete_environment_domain(
    State(state): State<Arc<AppState>>,
    Path((project_id, env_id, domain_id)): Path<(i32, i32, i32)>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsDelete);

    state
        .environment_service
        .delete_environment_domain(project_id, env_id, domain_id)
        .await
        .map_err(|e| {
            error!("Error deleting environment domain: {:?}", e);
            Problem::from(e)
        })?;

    Ok(StatusCode::NO_CONTENT)
}

/// Get environment variables for a project, optionally filtered by environment
#[utoipa::path(
    get,
    path = "/projects/{project_id}/env-vars",
    tag = "Projects",
    responses(
        (status = 200, description = "List of environment variables", body = Vec<EnvironmentVariableResponse>),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID or slug"),
        ("environment_id" = Option<i32>, Query, description = "Optional environment ID to filter by")
    )
)]
pub async fn get_environment_variables(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(params): Query<GetEnvironmentVariablesQuery>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsRead);

    let vars = state
        .env_var_service
        .get_environment_variables(project_id, params.environment_id)
        .await?;

    let response: Vec<EnvironmentVariableResponse> = vars
        .into_iter()
        .map(|v| EnvironmentVariableResponse {
            id: v.id,
            key: v.key,
            value: v.value,
            created_at: v.created_at.timestamp_millis(),
            updated_at: v.updated_at.timestamp_millis(),
            environments: v
                .environments
                .into_iter()
                .map(|env| EnvironmentInfo {
                    id: env.id,
                    name: env.name,
                    main_url: env.main_url,
                    current_deployment_id: env.current_deployment_id,
                })
                .collect(),
            include_in_preview: v.include_in_preview,
        })
        .collect();

    Ok(Json(response))
}

/// Create a new environment variable
#[utoipa::path(
    post,
    path = "/projects/{project_id}/env-vars",
    tag = "Projects",
    request_body = CreateEnvironmentVariableRequest,
    responses(
        (status = 201, description = "Environment variables created successfully", body = EnvironmentVariableResponse),
        (status = 400, description = "Invalid input"),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID or slug")
    )
)]
pub async fn create_environment_variable(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Json(request): Json<CreateEnvironmentVariableRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsCreate);

    let var = state
        .env_var_service
        .create_environment_variable(
            project_id,
            request.environment_ids,
            request.key,
            request.value,
            request.include_in_preview,
        )
        .await
        .map_err(Problem::from)?;

    let response = EnvironmentVariableResponse {
        id: var.id,
        key: var.key,
        value: var.value,
        created_at: var.created_at.timestamp_millis(),
        updated_at: var.updated_at.timestamp_millis(),
        environments: var
            .environments
            .into_iter()
            .map(|env| EnvironmentInfo {
                id: env.id,
                name: env.name,
                main_url: env.main_url,
                current_deployment_id: env.current_deployment_id,
            })
            .collect(),
        include_in_preview: var.include_in_preview,
    };

    Ok((StatusCode::CREATED, Json(response)))
}

/// Delete an environment variable
#[utoipa::path(
    delete,
    path = "/projects/{project_id}/env-vars/{var_id}",
    tag = "Projects",
    responses(
        (status = 204, description = "Environment variable deleted successfully"),
        (status = 404, description = "Project or variable not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID or slug"),
        ("var_id" = i32, Path, description = "Environment variable ID")
    )
)]
pub async fn delete_environment_variable(
    State(state): State<Arc<AppState>>,
    Path((project_id, var_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsDelete);

    state
        .env_var_service
        .delete_environment_variable(project_id, var_id)
        .await?;

    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Update an environment variable
#[utoipa::path(
    put,
    path = "/projects/{project_id}/env-vars/{var_id}",
    tag = "Projects",
    request_body = CreateEnvironmentVariableRequest,
    responses(
        (status = 200, description = "Environment variables updated successfully", body = EnvironmentVariableResponse),
        (status = 400, description = "Invalid input"),
        (status = 404, description = "Project or variable not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID or slug"),
        ("var_id" = i32, Path, description = "Environment variable ID")
    )
)]
pub async fn update_environment_variable(
    State(state): State<Arc<AppState>>,
    Path((project_id, var_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
    Json(request): Json<CreateEnvironmentVariableRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsWrite);

    let var = state
        .env_var_service
        .update_environment_variable(
            project_id,
            var_id,
            request.key,
            request.value,
            request.environment_ids,
            request.include_in_preview,
        )
        .await?;

    let response = EnvironmentVariableResponse {
        id: var.id,
        key: var.key,
        value: var.value,
        created_at: var.created_at.timestamp_millis(),
        updated_at: var.updated_at.timestamp_millis(),
        environments: var
            .environments
            .into_iter()
            .map(|env| EnvironmentInfo {
                id: env.id,
                name: env.name,
                main_url: env.main_url,
                current_deployment_id: env.current_deployment_id,
            })
            .collect(),
        include_in_preview: var.include_in_preview,
    };

    Ok(Json(response))
}

/// Get environment variable value by key
#[utoipa::path(
    get,
    path = "/projects/{project_id}/env-vars/{key}/value",
    tag = "Projects",
    responses(
        (status = 200, description = "Environment variable value", body = EnvironmentVariableValueResponse),
        (status = 404, description = "Project or variable not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID or slug"),
        ("key" = String, Path, description = "Environment variable key"),
        ("environment_id" = Option<i32>, Query, description = "Optional environment ID")
    )
)]
pub async fn get_environment_variable_value(
    State(state): State<Arc<AppState>>,
    Path((project_id, key)): Path<(i32, String)>,
    Query(params): Query<GetEnvironmentVariablesQuery>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsRead);

    let value = state
        .env_var_service
        .get_environment_variable_value(project_id, &key, params.environment_id)
        .await?;

    Ok(Json(EnvironmentVariableValueResponse { value }))
}

/// Update environment settings
#[utoipa::path(
    put,
    path = "/projects/{project_id}/environments/{env_id}/settings",
    tag = "Projects",
    request_body = UpdateEnvironmentSettingsRequest,
    responses(
        (status = 200, description = "Environment settings updated successfully", body = EnvironmentResponse),
        (status = 404, description = "Project or environment not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID or slug"),
        ("env_id" = i32, Path, description = "Environment ID or slug")
    )
)]
pub async fn update_environment_settings(
    State(state): State<Arc<AppState>>,
    Path((project_id, env_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(settings): Json<UpdateEnvironmentSettingsRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsWrite);

    // Get project details for audit log
    let project = state.environment_service.get_project(project_id).await?;

    // Get environment details for audit log
    let environment = state
        .environment_service
        .get_environment(project_id, env_id)
        .await?;

    let updated_environment = state
        .environment_service
        .update_environment_settings(project_id, env_id, settings.clone())
        .await?;

    // Create audit event
    let audit_context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent,
    };

    let updated_settings = EnvironmentSettingsUpdatedFields {
        cpu_request: settings.cpu_request,
        cpu_limit: settings.cpu_limit,
        memory_request: settings.memory_request,
        memory_limit: settings.memory_limit,
        branch: settings.branch,
        replicas: settings.replicas,
        security_updated: settings.security.is_some(),
    };

    let audit_event = EnvironmentSettingsUpdatedAudit {
        context: audit_context,
        project_id: project.id,
        project_name: project.name,
        project_slug: project.slug,
        environment_id: environment.id,
        environment_name: environment.name,
        environment_slug: environment.slug,
        updated_settings,
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit_event).await {
        error!("Failed to create audit log: {:?}", e);
        // Continue with the operation even if audit logging fails
    }

    Ok(Json(EnvironmentResponse::from(updated_environment)).into_response())
}

/// Delete an environment permanently
///
/// Permanently deletes an environment and all related data. Cannot delete:
/// - Production environments (name = "Production")
///
/// Warning: This action is permanent and cannot be undone.
/// Active deployments are automatically cancelled before deletion.
#[utoipa::path(
    delete,
    path = "/projects/{project_id}/environments/{env_id}",
    tag = "Projects",
    responses(
        (status = 204, description = "Environment permanently deleted"),
        (status = 400, description = "Cannot delete production environment"),
        (status = 404, description = "Project or environment not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("env_id" = i32, Path, description = "Environment ID")
    )
)]
pub async fn delete_environment(
    State(state): State<Arc<AppState>>,
    Path((project_id, env_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsDelete);

    // Get environment details before deletion for audit log
    let environment = state
        .environment_service
        .get_environment(project_id, env_id)
        .await?;

    let project = state.environment_service.get_project(project_id).await?;

    // Cancel all active deployments for this environment
    match state
        .deployment_service
        .cancel_all_environment_deployments(env_id)
        .await
    {
        Ok(count) => {
            if count > 0 {
                info!(
                    "Cancelled {} active deployment(s) before deleting environment {}",
                    count, env_id
                );
            }
        }
        Err(e) => {
            error!(
                "Failed to cancel deployments for environment {}: {:?}",
                env_id, e
            );
            // Continue with deletion even if cancellation fails
        }
    }

    // Delete the environment
    state
        .environment_service
        .delete_environment(project_id, env_id)
        .await?;

    // Create audit event
    let audit_context = temps_core::AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    };

    let audit_event = EnvironmentDeletedAudit {
        context: audit_context,
        project_id: project.id,
        project_name: project.name,
        project_slug: project.slug,
        environment_id: environment.id,
        environment_name: environment.name,
        environment_slug: environment.slug,
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit_event).await {
        error!("Failed to create audit log: {:?}", e);
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Create a new environment for a project
#[utoipa::path(
    post,
    path = "/projects/{project_id}/environments",
    tag = "Projects",
    request_body = CreateEnvironmentRequest,
    responses(
        (status = 201, description = "Environment created successfully", body = EnvironmentResponse),
        (status = 400, description = "Invalid input"),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID or slug")
    )
)]
pub async fn create_environment(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Json(request): Json<CreateEnvironmentRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsCreate);

    let environment = state
        .environment_service
        .create_new_environment(project_id, request.name, request.branch, None)
        .await?;

    Ok((
        StatusCode::CREATED,
        Json(EnvironmentResponse::from(environment)),
    )
        .into_response())
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        // Environment routes
        .route("/projects/{project_id}/environments", get(get_environments))
        .route(
            "/projects/{project_id}/environments",
            post(create_environment),
        )
        .route(
            "/projects/{project_id}/environments/{id_or_slug}",
            get(get_environment).delete(delete_environment),
        )
        .route(
            "/projects/{project_id}/environments/{id_or_slug}/settings",
            put(update_environment_settings),
        )
        // Environment domains
        .route(
            "/projects/{project_id}/environments/{environment_id}/domains",
            get(get_environment_domains),
        )
        .route(
            "/projects/{project_id}/environments/{environment_id}/domains",
            post(add_environment_domain),
        )
        .route(
            "/projects/{project_id}/environments/{environment_id}/domains/{domain_id}",
            delete(delete_environment_domain),
        )
        // Environment variables
        .route(
            "/projects/{project_id}/env-vars",
            get(get_environment_variables),
        )
        .route(
            "/projects/{project_id}/env-vars",
            post(create_environment_variable),
        )
        .route(
            "/projects/{project_id}/env-vars/{var_id}",
            put(update_environment_variable),
        )
        .route(
            "/projects/{project_id}/env-vars/{var_id}",
            delete(delete_environment_variable),
        )
        .route(
            "/projects/{project_id}/env-vars/{key}/value",
            get(get_environment_variable_value),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_environments,
        get_environment,
        create_environment,
        update_environment_settings,
        delete_environment,
        get_environment_domains,
        add_environment_domain,
        delete_environment_domain,
        get_environment_variables,
        create_environment_variable,
        update_environment_variable,
        delete_environment_variable,
        get_environment_variable_value,
    ),
    components(
        schemas(
            EnvironmentResponse,
            CreateEnvironmentRequest,
            UpdateEnvironmentSettingsRequest,
            EnvironmentDomainResponse,
            AddEnvironmentDomainRequest,
            EnvironmentVariableResponse,
            CreateEnvironmentVariableRequest,
            EnvironmentVariableValueResponse,
            GetEnvironmentVariablesQuery,
            EnvironmentInfo,
        )
    ),
    tags(
        (name = "Environments", description = "Environment management operations")
    )
)]
pub struct ApiDoc;
