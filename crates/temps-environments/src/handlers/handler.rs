use super::audit::{
    EnvironmentDeletedAudit, EnvironmentSettingsUpdatedAudit, EnvironmentSettingsUpdatedFields,
    EnvironmentSleepStateChangedAudit,
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
    CreateProjectSecretRequest, EnvVarIntegrationInfo, EnvironmentDomainResponse, EnvironmentInfo,
    EnvironmentResponse, EnvironmentVariableResponse, EnvironmentVariableValueResponse,
    GetEnvironmentVariablesQuery, GetProjectSecretsQuery, ProjectSecretEnvironmentInfo,
    ProjectSecretResponse, ResolvedEnvVarResponse, ResolvedEnvVarSource,
    UpdateEnvironmentSettingsRequest, UpdateProjectSecretRequest,
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
            EnvVarError::EncryptionFailed { .. } => {
                temps_core::error_builder::internal_server_error()
                    .detail(err.to_string())
                    .build()
            }
            EnvVarError::DecryptionFailed { .. } => {
                temps_core::error_builder::internal_server_error()
                    .detail(err.to_string())
                    .build()
            }
            EnvVarError::Other(msg) => temps_core::error_builder::internal_server_error()
                .detail(msg)
                .build(),
        }
    }
}

impl From<crate::services::secret_service::SecretError> for Problem {
    fn from(err: crate::services::secret_service::SecretError) -> Self {
        use crate::services::secret_service::SecretError;
        match err {
            SecretError::NotFound { .. } => temps_core::error_builder::not_found()
                .detail(err.to_string())
                .build(),
            SecretError::KeyAlreadyExists { .. } => temps_core::error_builder::conflict()
                .detail(err.to_string())
                .build(),
            SecretError::ValueTooLarge { .. } => temps_core::error_builder::bad_request()
                .detail(err.to_string())
                .build(),
            SecretError::InvalidKey { .. } => temps_core::error_builder::bad_request()
                .detail(err.to_string())
                .build(),
            SecretError::EnvironmentNotFound { .. } => temps_core::error_builder::not_found()
                .detail(err.to_string())
                .build(),
            SecretError::EncryptionFailed { .. }
            | SecretError::DecryptionFailed { .. }
            | SecretError::DatabaseConnection(_)
            | SecretError::Database(_) => temps_core::error_builder::internal_server_error()
                .detail(err.to_string())
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

    let mut response: Vec<EnvironmentResponse> = Vec::new();
    for env in environments {
        let main_url = state
            .environment_service
            .compute_environment_url(&env.subdomain)
            .await;

        response.push(EnvironmentResponse {
            id: env.id,
            project_id: env.project_id,
            name: env.name,
            slug: env.slug,
            main_url,
            current_deployment_id: env.current_deployment_id,
            created_at: env.created_at.timestamp_millis(),
            updated_at: env.updated_at.timestamp_millis(),
            branch: env.branch,
            is_preview: env.is_preview,
            deployment_config: env.deployment_config.clone(),
            protected: env.protected,
            sleeping: env.sleeping,
            last_activity_at: env.last_activity_at.map(|t| t.timestamp_millis()),
            estimated_sleep_at: if !env.sleeping {
                env.deployment_config
                    .as_ref()
                    .filter(|dc| dc.on_demand)
                    .and_then(|dc| {
                        env.last_activity_at.map(|last| {
                            last.timestamp_millis() + (dc.idle_timeout_seconds as i64 * 1000)
                        })
                    })
            } else {
                None
            },
        });
    }

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

    let main_url = state
        .environment_service
        .compute_environment_url(&env.subdomain)
        .await;

    Ok(Json(EnvironmentResponse {
        id: env.id,
        project_id: env.project_id,
        name: env.name,
        slug: env.slug,
        main_url,
        current_deployment_id: env.current_deployment_id,
        created_at: env.created_at.timestamp_millis(),
        updated_at: env.updated_at.timestamp_millis(),
        branch: env.branch,
        is_preview: env.is_preview,
        deployment_config: env.deployment_config.clone(),
        protected: env.protected,
        sleeping: env.sleeping,
        last_activity_at: env.last_activity_at.map(|t| t.timestamp_millis()),
        estimated_sleep_at: if !env.sleeping {
            env.deployment_config
                .as_ref()
                .filter(|dc| dc.on_demand)
                .and_then(|dc| {
                    env.last_activity_at.map(|last| {
                        last.timestamp_millis() + (dc.idle_timeout_seconds as i64 * 1000)
                    })
                })
        } else {
            None
        },
    }))
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

    // Always mask plaintext values in the list response. Callers that
    // legitimately need the decrypted value must hit
    // GET /projects/{id}/env-vars/{key}/value (audited) one secret at
    // a time. Bulk-dumping every project secret over a single GET is
    // the kind of mistake that turns a compromised reader token into
    // a total credential exfiltration.
    let response: Vec<EnvironmentVariableResponse> = vars
        .into_iter()
        .map(|v| EnvironmentVariableResponse {
            id: v.id,
            key: v.key,
            value: "***".to_string(),
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

/// Resolved env vars for a project (manual + integration-sourced, merged).
///
/// Returns the effective set of environment variables a deployment would see,
/// combining manually-defined vars with those contributed by linked external
/// services (Postgres, Redis, S3, etc.). Each entry is tagged with its source
/// so the UI can render an integration icon, and manual entries that shadow an
/// integration key carry a reference to the integration they override.
///
/// Values are always returned as a masked preview. Use the per-key reveal
/// endpoint for plaintext (audit-logged).
#[utoipa::path(
    get,
    path = "/projects/{project_id}/env-vars/resolved",
    tag = "Projects",
    responses(
        (status = 200, description = "Resolved environment variables", body = Vec<ResolvedEnvVarResponse>),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID or slug"),
        ("environment_id" = Option<i32>, Query, description = "Optional environment ID to filter manual vars by")
    )
)]
pub async fn get_resolved_environment_variables(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(params): Query<GetEnvironmentVariablesQuery>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsRead);

    // Manual vars (already includes environment memberships).
    let manual = state
        .env_var_service
        .get_environment_variables(project_id, params.environment_id)
        .await?;

    // Every environment on the project — used to surface integration vars
    // against the whole environment set since integrations are not scoped.
    let all_envs = state
        .environment_service
        .get_environments(project_id)
        .await?;
    let env_infos: Vec<EnvironmentInfo> = all_envs
        .into_iter()
        .map(|e| EnvironmentInfo {
            id: e.id,
            name: e.name,
            main_url: e.subdomain,
            current_deployment_id: e.current_deployment_id,
        })
        .collect();

    // Integration vars, if the provider is wired up. Missing provider = manual
    // only (keeps the handler useful in test harnesses that skip the providers
    // plugin).
    let integrations = match state.integration_env_provider.as_ref() {
        Some(provider) => provider
            .get_project_integration_env_vars(project_id)
            .await
            .map_err(|e| {
                error!("Failed to load integration env vars: {}", e);
                temps_core::error_builder::internal_server_error()
                    .detail(format!("Failed to load integration env vars: {}", e))
                    .build()
            })?,
        None => Vec::new(),
    };

    // Flatten integrations into a lookup keyed by env var name. Last writer
    // wins on collisions between two integrations — rare in practice (Postgres
    // + Redis don't share keys) but worth a log line when it happens.
    let mut integration_by_key: std::collections::HashMap<String, EnvVarIntegrationInfo> =
        std::collections::HashMap::new();
    for svc in &integrations {
        let info = EnvVarIntegrationInfo {
            service_id: svc.service.service_id,
            service_name: svc.service.service_name.clone(),
            service_type: svc.service.service_type.clone(),
            service_slug: svc.service.service_slug.clone(),
        };
        for var in &svc.variables {
            if let Some(prev) = integration_by_key.insert(var.key.clone(), info.clone()) {
                info!(
                    project_id,
                    key = %var.key,
                    previous_service_id = prev.service_id,
                    new_service_id = info.service_id,
                    "resolved_env_vars: two integrations produced the same key; later one wins"
                );
            }
        }
    }

    let mut manual_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut response: Vec<ResolvedEnvVarResponse> = Vec::new();

    // Manual vars first — preserves the original ordering (updated_at desc).
    for v in manual {
        let overrides_service = integration_by_key.get(&v.key).cloned();
        manual_keys.insert(v.key.clone());
        response.push(ResolvedEnvVarResponse {
            key: v.key,
            value_preview: "***".to_string(),
            source: ResolvedEnvVarSource::Manual {
                var_id: v.id,
                overrides_service,
            },
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
        });
    }

    // Integration vars that are not shadowed by a manual entry.
    for svc in integrations {
        let info = EnvVarIntegrationInfo {
            service_id: svc.service.service_id,
            service_name: svc.service.service_name,
            service_type: svc.service.service_type,
            service_slug: svc.service.service_slug,
        };
        for var in svc.variables {
            if manual_keys.contains(&var.key) {
                continue;
            }
            response.push(ResolvedEnvVarResponse {
                key: var.key,
                value_preview: "***".to_string(),
                source: ResolvedEnvVarSource::Integration {
                    service: info.clone(),
                },
                environments: env_infos.clone(),
                include_in_preview: true,
            });
        }
    }

    Ok(Json(response))
}

/// Reveal the plaintext value of a resolved environment variable.
///
/// Mirrors `GET /projects/{id}/env-vars/{key}/value` but handles keys sourced
/// from linked integrations (which are not stored in the `env_vars` table).
/// Resolution order mirrors the merged view:
///
/// 1. Manual env var with this key (already audit-logged via the existing reveal
///    endpoint flow) — this endpoint defers to the manual store when the key
///    exists there, so callers can use a single endpoint regardless of source.
/// 2. Integration env var supplied by a linked external service.
///
/// Returns 404 when neither a manual var nor an integration produces the key.
#[utoipa::path(
    get,
    path = "/projects/{project_id}/env-vars/resolved/{key}/value",
    tag = "Projects",
    responses(
        (status = 200, description = "Resolved environment variable value", body = EnvironmentVariableValueResponse),
        (status = 404, description = "Project, key, or integration not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID or slug"),
        ("key" = String, Path, description = "Environment variable key"),
        ("environment_id" = Option<i32>, Query, description = "Optional environment ID (manual vars only)")
    )
)]
pub async fn get_resolved_environment_variable_value(
    State(state): State<Arc<AppState>>,
    Path((project_id, key)): Path<(i32, String)>,
    Query(params): Query<GetEnvironmentVariablesQuery>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsRead);

    info!(
        user_id = auth.user_id(),
        project_id = project_id,
        env_var_key = %key,
        environment_id = ?params.environment_id,
        "env_var.reveal_resolved"
    );

    // Prefer a manual value when one exists — same audit surface as the
    // per-key reveal endpoint, and manual values shadow integration values.
    match state
        .env_var_service
        .get_environment_variable_value(project_id, &key, params.environment_id)
        .await
    {
        Ok(value) => return Ok(Json(EnvironmentVariableValueResponse { value })),
        Err(crate::services::env_var_service::EnvVarError::NotFound(_)) => {
            // Fall through to integration lookup.
        }
        Err(e) => return Err(e.into()),
    }

    // No manual entry — look the key up in the integration provider.
    let provider = state.integration_env_provider.as_ref().ok_or_else(|| {
        temps_core::error_builder::not_found()
            .title("Environment variable not found")
            .detail(format!(
                "Environment variable '{}' not found for project {}",
                key, project_id
            ))
            .build()
    })?;

    let services = provider
        .get_project_integration_env_vars(project_id)
        .await
        .map_err(|e| {
            error!("Failed to load integration env vars: {}", e);
            temps_core::error_builder::internal_server_error()
                .detail(format!("Failed to load integration env vars: {}", e))
                .build()
        })?;

    // Walk services in order; later services win on collisions (matches the
    // list endpoint).
    let mut resolved_value: Option<String> = None;
    for svc in &services {
        for var in &svc.variables {
            if var.key == key {
                resolved_value = Some(var.value.clone());
            }
        }
    }

    match resolved_value {
        Some(value) => Ok(Json(EnvironmentVariableValueResponse { value })),
        None => Err(temps_core::error_builder::not_found()
            .title("Environment variable not found")
            .detail(format!(
                "Environment variable '{}' not found for project {}",
                key, project_id
            ))
            .build()),
    }
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

    // Reveal of a single decrypted secret. Logged at info so any
    // bulk-reveal pattern (one of the obvious post-compromise behaviors)
    // is grep-able in the structured logs even before a dedicated audit
    // event is added.
    info!(
        user_id = auth.user_id(),
        project_id = project_id,
        env_var_key = %key,
        environment_id = ?params.environment_id,
        "env_var.reveal"
    );

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

    let main_url = state
        .environment_service
        .compute_environment_url(&updated_environment.subdomain)
        .await;

    Ok(Json(EnvironmentResponse {
        id: updated_environment.id,
        project_id: updated_environment.project_id,
        name: updated_environment.name,
        slug: updated_environment.slug,
        main_url,
        current_deployment_id: updated_environment.current_deployment_id,
        created_at: updated_environment.created_at.timestamp_millis(),
        updated_at: updated_environment.updated_at.timestamp_millis(),
        branch: updated_environment.branch,
        is_preview: updated_environment.is_preview,
        deployment_config: updated_environment.deployment_config.clone(),
        protected: updated_environment.protected,
        sleeping: updated_environment.sleeping,
        last_activity_at: updated_environment
            .last_activity_at
            .map(|t| t.timestamp_millis()),
        estimated_sleep_at: if !updated_environment.sleeping {
            updated_environment
                .deployment_config
                .as_ref()
                .filter(|dc| dc.on_demand)
                .and_then(|dc| {
                    updated_environment.last_activity_at.map(|last| {
                        last.timestamp_millis() + (dc.idle_timeout_seconds as i64 * 1000)
                    })
                })
        } else {
            None
        },
    })
    .into_response())
}

/// Wake a sleeping on-demand environment
///
/// Manually wake an environment that has been put to sleep by the on-demand
/// idle timeout. Starts containers, waits for health checks, then sets
/// `sleeping = false`. If no OnDemandWaker is available (proxy not running
/// in same process), falls back to setting the DB flag only.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/environments/{env_id}/wake",
    tag = "Environments",
    responses(
        (status = 200, description = "Environment woken up", body = EnvironmentResponse),
        (status = 400, description = "On-demand not enabled for this environment"),
        (status = 404, description = "Environment not found"),
        (status = 429, description = "Too many state transitions, retry after cooldown"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("env_id" = i32, Path, description = "Environment ID")
    )
)]
pub async fn wake_environment(
    State(state): State<Arc<AppState>>,
    Path((project_id, env_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsWrite);

    // Cooldown: reject if last state change was less than 30 seconds ago
    let environment = state
        .environment_service
        .get_environment(project_id, env_id)
        .await?;

    let seconds_since_update = (chrono::Utc::now() - environment.updated_at).num_seconds();
    if seconds_since_update < 30 {
        return Err(temps_core::error_builder::too_many_requests()
            .title("State Transition Cooldown")
            .detail(format!(
                "Environment {} was updated {}s ago. Please wait at least 30s between state transitions.",
                env_id, seconds_since_update
            ))
            .build());
    }

    // Use the full container lifecycle wake if available
    if let Some(ref waker) = state.on_demand_waker {
        let wake_timeout = environment
            .deployment_config
            .as_ref()
            .map(|c| c.wake_timeout_seconds)
            .unwrap_or(30);

        waker
            .wake_environment(env_id, wake_timeout)
            .await
            .map_err(|e| {
                error!(
                    environment_id = env_id,
                    error = %e,
                    "Failed to wake environment via OnDemandWaker"
                );
                temps_core::error_builder::internal_server_error()
                    .title("Wake Failed")
                    .detail(format!("Failed to wake environment {}: {}", env_id, e))
                    .build()
            })?;
    } else {
        // No OnDemandWaker available — cannot safely wake without starting containers
        return Err(temps_core::error_builder::internal_server_error()
            .title("Wake Unavailable")
            .detail(format!(
                "Cannot wake environment {}: on-demand container lifecycle manager is not available. \
                 The environment will be woken automatically when the next request arrives via the proxy.",
                env_id
            ))
            .build());
    }

    // Re-read the environment after wake
    let updated_environment = state
        .environment_service
        .get_environment(project_id, env_id)
        .await?;

    info!(
        environment_id = env_id,
        project_id = project_id,
        user_id = auth.user_id(),
        "Environment manually woken up"
    );

    let audit_context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent,
    };

    let _ = state
        .audit_service
        .create_audit_log(&EnvironmentSleepStateChangedAudit {
            context: audit_context,
            project_id,
            environment_id: env_id,
            environment_name: updated_environment.name.clone(),
            environment_slug: updated_environment.slug.clone(),
            previous_state: "sleeping",
            new_state: "awake",
        })
        .await;

    let main_url = state
        .environment_service
        .compute_environment_url(&updated_environment.subdomain)
        .await;

    Ok(Json(EnvironmentResponse {
        id: updated_environment.id,
        project_id: updated_environment.project_id,
        name: updated_environment.name,
        slug: updated_environment.slug,
        main_url,
        current_deployment_id: updated_environment.current_deployment_id,
        created_at: updated_environment.created_at.timestamp_millis(),
        updated_at: updated_environment.updated_at.timestamp_millis(),
        branch: updated_environment.branch,
        is_preview: updated_environment.is_preview,
        deployment_config: updated_environment.deployment_config.clone(),
        protected: updated_environment.protected,
        sleeping: updated_environment.sleeping,
        last_activity_at: updated_environment
            .last_activity_at
            .map(|t| t.timestamp_millis()),
        estimated_sleep_at: if !updated_environment.sleeping {
            updated_environment
                .deployment_config
                .as_ref()
                .filter(|dc| dc.on_demand)
                .and_then(|dc| {
                    updated_environment.last_activity_at.map(|last| {
                        last.timestamp_millis() + (dc.idle_timeout_seconds as i64 * 1000)
                    })
                })
        } else {
            None
        },
    })
    .into_response())
}

/// Sleep an on-demand environment
///
/// Manually put an on-demand environment to sleep. Stops containers and sets
/// `sleeping = true`. If no OnDemandWaker is available, falls back to DB flag only.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/environments/{env_id}/sleep",
    tag = "Environments",
    responses(
        (status = 200, description = "Environment put to sleep", body = EnvironmentResponse),
        (status = 400, description = "On-demand not enabled for this environment"),
        (status = 404, description = "Environment not found"),
        (status = 429, description = "Too many state transitions, retry after cooldown"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("env_id" = i32, Path, description = "Environment ID")
    )
)]
pub async fn sleep_environment(
    State(state): State<Arc<AppState>>,
    Path((project_id, env_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsWrite);

    // Cooldown: reject if last state change was less than 30 seconds ago
    let environment = state
        .environment_service
        .get_environment(project_id, env_id)
        .await?;

    let seconds_since_update = (chrono::Utc::now() - environment.updated_at).num_seconds();
    if seconds_since_update < 30 {
        return Err(temps_core::error_builder::too_many_requests()
            .title("State Transition Cooldown")
            .detail(format!(
                "Environment {} was updated {}s ago. Please wait at least 30s between state transitions.",
                env_id, seconds_since_update
            ))
            .build());
    }

    // Use the full container lifecycle sleep if available
    if let Some(ref waker) = state.on_demand_waker {
        waker.sleep_environment(env_id).await.map_err(|e| {
            error!(
                environment_id = env_id,
                error = %e,
                "Failed to sleep environment via OnDemandWaker"
            );
            temps_core::error_builder::internal_server_error()
                .title("Sleep Failed")
                .detail(format!("Failed to sleep environment {}: {}", env_id, e))
                .build()
        })?;
    } else {
        // Fallback: set DB flag only
        state
            .environment_service
            .set_sleeping(project_id, env_id, true)
            .await?;
    }

    // Re-read the environment after sleep
    let updated_environment = state
        .environment_service
        .get_environment(project_id, env_id)
        .await?;

    info!(
        environment_id = env_id,
        project_id = project_id,
        user_id = auth.user_id(),
        "Environment manually put to sleep"
    );

    let audit_context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent,
    };

    let _ = state
        .audit_service
        .create_audit_log(&EnvironmentSleepStateChangedAudit {
            context: audit_context,
            project_id,
            environment_id: env_id,
            environment_name: updated_environment.name.clone(),
            environment_slug: updated_environment.slug.clone(),
            previous_state: "awake",
            new_state: "sleeping",
        })
        .await;

    let main_url = state
        .environment_service
        .compute_environment_url(&updated_environment.subdomain)
        .await;

    Ok(Json(EnvironmentResponse {
        id: updated_environment.id,
        project_id: updated_environment.project_id,
        name: updated_environment.name,
        slug: updated_environment.slug,
        main_url,
        current_deployment_id: updated_environment.current_deployment_id,
        created_at: updated_environment.created_at.timestamp_millis(),
        updated_at: updated_environment.updated_at.timestamp_millis(),
        branch: updated_environment.branch,
        is_preview: updated_environment.is_preview,
        deployment_config: updated_environment.deployment_config.clone(),
        protected: updated_environment.protected,
        sleeping: updated_environment.sleeping,
        last_activity_at: updated_environment
            .last_activity_at
            .map(|t| t.timestamp_millis()),
        estimated_sleep_at: if !updated_environment.sleeping {
            updated_environment
                .deployment_config
                .as_ref()
                .filter(|dc| dc.on_demand)
                .and_then(|dc| {
                    updated_environment.last_activity_at.map(|last| {
                        last.timestamp_millis() + (dc.idle_timeout_seconds as i64 * 1000)
                    })
                })
        } else {
            None
        },
    })
    .into_response())
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

    let main_url = state
        .environment_service
        .compute_environment_url(&environment.subdomain)
        .await;

    Ok((
        StatusCode::CREATED,
        Json(EnvironmentResponse {
            id: environment.id,
            project_id: environment.project_id,
            name: environment.name,
            slug: environment.slug,
            main_url,
            current_deployment_id: environment.current_deployment_id,
            created_at: environment.created_at.timestamp_millis(),
            updated_at: environment.updated_at.timestamp_millis(),
            branch: environment.branch,
            is_preview: environment.is_preview,
            deployment_config: environment.deployment_config.clone(),
            protected: environment.protected,
            sleeping: environment.sleeping,
            last_activity_at: environment.last_activity_at.map(|t| t.timestamp_millis()),
            estimated_sleep_at: if !environment.sleeping {
                environment
                    .deployment_config
                    .as_ref()
                    .filter(|dc| dc.on_demand)
                    .and_then(|dc| {
                        environment.last_activity_at.map(|last| {
                            last.timestamp_millis() + (dc.idle_timeout_seconds as i64 * 1000)
                        })
                    })
            } else {
                None
            },
        }),
    )
        .into_response())
}

// ======================================================================
// Secrets: file-mounted secret values.
//
// Secrets are delivered to containers as files under /run/secrets/<KEY>
// (tmpfs, mode 0400) rather than as environment variables. Plaintext is
// NEVER returned from the API after creation — GET responses always
// carry only metadata. The mounted file inside the running container is
// the source of truth for reads.
// ======================================================================

/// List project secrets (metadata only — values never returned).
#[utoipa::path(
    get,
    path = "/projects/{project_id}/secrets",
    tag = "Secrets",
    operation_id = "listProjectSecrets",
    responses(
        (status = 200, description = "List of secrets (metadata only, no values)", body = Vec<ProjectSecretResponse>),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Optional environment filter")
    )
)]
pub async fn list_project_secrets(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(params): Query<GetProjectSecretsQuery>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsRead);

    let secrets = state
        .secret_service
        .list(project_id, params.environment_id)
        .await?;

    let response: Vec<ProjectSecretResponse> = secrets
        .into_iter()
        .map(|s| ProjectSecretResponse {
            id: s.id,
            project_id: s.project_id,
            key: s.key,
            include_in_preview: s.include_in_preview,
            created_at: s.created_at.timestamp_millis(),
            updated_at: s.updated_at.timestamp_millis(),
            environments: s
                .environments
                .into_iter()
                .map(|env| ProjectSecretEnvironmentInfo {
                    id: env.id,
                    name: env.name,
                    main_url: env.main_url,
                })
                .collect(),
        })
        .collect();

    Ok(Json(response))
}

/// Create a new secret. The value is encrypted before storage and will be
/// mounted as a file at `/run/secrets/<KEY>` on the next deployment.
/// The plaintext value is NOT returned — the response carries only metadata.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/secrets",
    tag = "Secrets",
    operation_id = "createProjectSecret",
    request_body = CreateProjectSecretRequest,
    responses(
        (status = 201, description = "Secret created", body = ProjectSecretResponse),
        (status = 400, description = "Invalid key or value too large"),
        (status = 409, description = "Key already exists in project"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID")
    )
)]
pub async fn create_project_secret(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Json(request): Json<CreateProjectSecretRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsCreate);

    let secret = state
        .secret_service
        .create(
            project_id,
            request.environment_ids,
            request.key,
            request.value,
            request.include_in_preview,
        )
        .await?;

    let response = ProjectSecretResponse {
        id: secret.id,
        project_id: secret.project_id,
        key: secret.key,
        include_in_preview: secret.include_in_preview,
        created_at: secret.created_at.timestamp_millis(),
        updated_at: secret.updated_at.timestamp_millis(),
        environments: secret
            .environments
            .into_iter()
            .map(|env| ProjectSecretEnvironmentInfo {
                id: env.id,
                name: env.name,
                main_url: env.main_url,
            })
            .collect(),
    };

    Ok((StatusCode::CREATED, Json(response)))
}

/// Update a project secret. Value rotation requires a redeploy to take effect —
/// running containers keep their currently-mounted values until the next
/// deployment.
#[utoipa::path(
    put,
    path = "/projects/{project_id}/secrets/{secret_id}",
    tag = "Secrets",
    operation_id = "updateProjectSecret",
    request_body = UpdateProjectSecretRequest,
    responses(
        (status = 200, description = "Secret updated", body = ProjectSecretResponse),
        (status = 400, description = "Value too large"),
        (status = 404, description = "Secret not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("secret_id" = i32, Path, description = "Secret ID")
    )
)]
pub async fn update_project_secret(
    State(state): State<Arc<AppState>>,
    Path((project_id, secret_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
    Json(request): Json<UpdateProjectSecretRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsWrite);

    let secret = state
        .secret_service
        .update(
            project_id,
            secret_id,
            request.value,
            request.environment_ids,
            request.include_in_preview,
        )
        .await?;

    let response = ProjectSecretResponse {
        id: secret.id,
        project_id: secret.project_id,
        key: secret.key,
        include_in_preview: secret.include_in_preview,
        created_at: secret.created_at.timestamp_millis(),
        updated_at: secret.updated_at.timestamp_millis(),
        environments: secret
            .environments
            .into_iter()
            .map(|env| ProjectSecretEnvironmentInfo {
                id: env.id,
                name: env.name,
                main_url: env.main_url,
            })
            .collect(),
    };

    Ok(Json(response))
}

/// Delete a project secret. Running containers keep their mounted secret files
/// until they are redeployed.
#[utoipa::path(
    delete,
    path = "/projects/{project_id}/secrets/{secret_id}",
    tag = "Secrets",
    operation_id = "deleteProjectSecret",
    responses(
        (status = 204, description = "Secret deleted"),
        (status = 404, description = "Secret not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("secret_id" = i32, Path, description = "Secret ID")
    )
)]
pub async fn delete_project_secret(
    State(state): State<Arc<AppState>>,
    Path((project_id, secret_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsDelete);

    state.secret_service.delete(project_id, secret_id).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
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
        // Environment wake/sleep (on-demand)
        .route(
            "/projects/{project_id}/environments/{env_id}/wake",
            post(wake_environment),
        )
        .route(
            "/projects/{project_id}/environments/{env_id}/sleep",
            post(sleep_environment),
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
            "/projects/{project_id}/env-vars/resolved",
            get(get_resolved_environment_variables),
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
        .route(
            "/projects/{project_id}/env-vars/resolved/{key}/value",
            get(get_resolved_environment_variable_value),
        )
        // Secrets (file-mounted values at /run/secrets/<KEY>)
        .route(
            "/projects/{project_id}/secrets",
            get(list_project_secrets).post(create_project_secret),
        )
        .route(
            "/projects/{project_id}/secrets/{secret_id}",
            put(update_project_secret).delete(delete_project_secret),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_environments,
        get_environment,
        create_environment,
        update_environment_settings,
        wake_environment,
        sleep_environment,
        delete_environment,
        get_environment_domains,
        add_environment_domain,
        delete_environment_domain,
        get_environment_variables,
        get_resolved_environment_variables,
        create_environment_variable,
        update_environment_variable,
        delete_environment_variable,
        get_environment_variable_value,
        get_resolved_environment_variable_value,
        list_project_secrets,
        create_project_secret,
        update_project_secret,
        delete_project_secret,
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
            ResolvedEnvVarResponse,
            ResolvedEnvVarSource,
            EnvVarIntegrationInfo,
            CreateProjectSecretRequest,
            UpdateProjectSecretRequest,
            ProjectSecretResponse,
            ProjectSecretEnvironmentInfo,
            GetProjectSecretsQuery,
        )
    ),
    tags(
        (name = "Environments", description = "Environment management operations"),
        (name = "Secrets", description = "File-mounted secrets (/run/secrets/<KEY>)")
    )
)]
pub struct ApiDoc;
