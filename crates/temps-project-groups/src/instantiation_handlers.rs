// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP surface for creating templates and importing apps (ADR-051).
//!
//! Every request is checked in full before anything is stored: permissions,
//! the template, the databases, the slugs. What is accepted then runs in the
//! background as an instantiation the caller polls with
//! `GET /instantiations/{id}` and, if a step fails, retries with
//! `POST /instantiations/{id}/retry`.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use sea_orm::SqlErr;
use serde::Deserialize;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::error_builder::ErrorBuilder;
use temps_core::problemdetails::Problem;
use temps_core::templates::{
    managed_service_types_compatible, template_provenance, TemplateKind, TemplateService,
};
use temps_core::RequestMetadata;
use temps_projects::services::types::SlugClaimCaller;
use temps_projects::template_creation::TemplateEnvironmentError;
use temps_projects::template_creation::{
    authorize_storage_services, missing_required_template_configuration,
    require_template_creation_permissions, resolve_image_template_runtime, RepositoryTarget,
};
use temps_projects::ProjectService;
use tracing::error;
use utoipa::{IntoParams, OpenApi};

use crate::error::ProjectGroupError;
use crate::handlers::{
    authorize_group_env, hidden_projects, reject_deployment_token, visible_or_not_found,
    ProjectGroupAudit, ProjectGroupsAppState,
};
use crate::instantiation::InstantiationError;
use crate::instantiation::{
    build_plan, ensure_secrets_available, import_template, validate_plan_variables, DatabaseChoice,
    ForkRequest, ImportServicesRequest, ImportedApp, InstantiateTemplateRequest,
    InstantiatedService, InstantiationEngine, InstantiationPlan, InstantiationResponse,
    InstantiationStatus, InstantiationStep, InstantiationTarget, PlanInput, PlannedService,
    PlannedTarget, RequestOrigin, StepStatus, TemplateVariableInput,
};
use crate::service::ProjectGroupService;

const MAX_IDEMPOTENCY_KEY_LEN: usize = 128;

/// What the instantiation endpoints need beyond the group services.
#[derive(Clone)]
pub struct InstantiationAppState {
    pub groups: Arc<ProjectGroupsAppState>,
    pub engine: InstantiationEngine,
    pub templates: Arc<TemplateService>,
    pub projects: Arc<ProjectService>,
    pub external_services: Arc<temps_providers::ExternalServiceManager>,
    pub sensitive_action_authorizer: Arc<dyn temps_core::SensitiveActionAuthorizer>,
}

pub fn router(state: Arc<InstantiationAppState>) -> Router {
    Router::new()
        .route("/templates/{slug}/instantiate", post(instantiate_template))
        .route(
            "/project-groups/{group_id}/import",
            post(import_project_services),
        )
        .route("/instantiations", get(list_instantiations))
        .route("/instantiations/{id}", get(get_instantiation))
        .route("/instantiations/{id}/retry", post(retry_instantiation))
        .with_state(state)
}

#[derive(OpenApi)]
#[openapi(
    paths(
        instantiate_template,
        import_project_services,
        list_instantiations,
        get_instantiation,
        retry_instantiation,
    ),
    components(schemas(
        InstantiateTemplateRequest,
        InstantiationTarget,
        DatabaseChoice,
        ForkRequest,
        TemplateVariableInput,
        temps_projects::template_creation::TemplateRuntimeOverrides,
        ImportServicesRequest,
        ImportedApp,
        InstantiationResponse,
        InstantiationListResponse,
        InstantiationStatus,
        InstantiationStep,
        StepStatus,
        InstantiatedService,
    )),
    tags((name = "Templates", description = "Create templates and import apps as services of a project"))
)]
pub struct InstantiationApiDoc;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn bad_request(title: &str, detail: impl Into<String>) -> Problem {
    temps_core::error_builder::bad_request()
        .title(title)
        .detail(detail.into())
        .build()
}

/// A planning failure as a response: a rejected request keeps the title of the
/// endpoint that planned it, anything else is classified by its variant.
fn planning_problem(title: &str, error: InstantiationError) -> Problem {
    match error {
        InstantiationError::Validation { message } => bad_request(title, message),
        other => Problem::from(other),
    }
}

fn instantiation_problem(status: StatusCode, title: &str, error: &InstantiationError) -> Problem {
    ErrorBuilder::new(status)
        .title(title)
        .detail(error.to_string())
        .build()
}

impl From<InstantiationError> for Problem {
    fn from(error: InstantiationError) -> Self {
        match &error {
            InstantiationError::Validation { .. } => {
                instantiation_problem(StatusCode::BAD_REQUEST, "Invalid Request", &error)
            }
            InstantiationError::ClaimLost { .. } => instantiation_problem(
                StatusCode::CONFLICT,
                "Instantiation Owned Elsewhere",
                &error,
            ),
            InstantiationError::SlugsExhausted { .. }
            | InstantiationError::DatabaseNameTaken { .. }
            | InstantiationError::RouteTaken { .. } => {
                instantiation_problem(StatusCode::CONFLICT, "Name Already In Use", &error)
            }
            InstantiationError::ServiceProjectDeleted { .. }
            | InstantiationError::ServiceProjectUninitialized { .. } => {
                instantiation_problem(StatusCode::CONFLICT, "Instantiation Cannot Resume", &error)
            }
            InstantiationError::Database { .. } => {
                error!(%error, "Instantiation database operation failed");
                instantiation_problem(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal Server Error",
                    &error,
                )
            }
            InstantiationError::InvalidPlan { .. }
            | InstantiationError::PlanNotSerializable { .. }
            | InstantiationError::SecretsUnreadable { .. }
            | InstantiationError::SecretsInvalid { .. }
            | InstantiationError::UnknownStep { .. }
            | InstantiationError::TemplateServiceMissing { .. }
            | InstantiationError::PlannedServiceMissing { .. }
            | InstantiationError::PlannedDatabaseMissing { .. }
            | InstantiationError::ProjectNotCreated
            | InstantiationError::ServiceNotCreated { .. }
            | InstantiationError::DiscardFailed { .. }
            | InstantiationError::CreationNotRecorded { .. }
            | InstantiationError::ExternalService(_) => instantiation_problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Instantiation Failed",
                &error,
            ),
            InstantiationError::Upstream { status, .. } => {
                instantiation_problem(*status, "Instantiation Step Failed", &error)
            }
            InstantiationError::Variables(source) => match source {
                TemplateEnvironmentError::SecureGeneration { .. } => instantiation_problem(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Variable Generation Failed",
                    &error,
                ),
                TemplateEnvironmentError::Duplicate { .. }
                | TemplateEnvironmentError::MissingRequired { .. }
                | TemplateEnvironmentError::UnsupportedGenerator { .. }
                | TemplateEnvironmentError::AlreadyConfigured { .. }
                | TemplateEnvironmentError::Unknown { .. } => {
                    instantiation_problem(StatusCode::BAD_REQUEST, "Invalid Variables", &error)
                }
            },
            InstantiationError::Group(_)
            | InstantiationError::Project(_)
            | InstantiationError::StepFailed { .. } => match error {
                InstantiationError::Group(source) => Problem::from(*source),
                InstantiationError::Project(source) => Problem::from(*source),
                InstantiationError::StepFailed { source, .. } => Problem::from(*source),
                other => {
                    error!(%other, "Instantiation error reached the wrong conversion arm");
                    instantiation_problem(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Instantiation Failed",
                        &other,
                    )
                }
            },
        }
    }
}

fn validate_idempotency_key(key: &str) -> Result<(), Problem> {
    let key = key.trim();
    if key.is_empty() || key.len() > MAX_IDEMPOTENCY_KEY_LEN {
        return Err(bad_request(
            "Invalid Idempotency Key",
            format!("idempotency_key must be 1 to {MAX_IDEMPOTENCY_KEY_LEN} characters"),
        ));
    }
    if key.chars().any(|ch| ch.is_control()) {
        return Err(bad_request(
            "Invalid Idempotency Key",
            "idempotency_key cannot contain control characters",
        ));
    }
    Ok(())
}

fn origin(metadata: &RequestMetadata) -> RequestOrigin {
    RequestOrigin {
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent.clone(),
        instantiation_id: None,
    }
}

/// The stored instantiation for this key, if the caller sent it before.
/// A key reused for something else is a client bug, reported as a conflict.
async fn replay(
    state: &InstantiationAppState,
    user_id: i32,
    key: &str,
    kind: &str,
    subject: Option<&str>,
    group_id: Option<i32>,
) -> Result<Option<InstantiationResponse>, Problem> {
    let Some(existing) = state.engine.find_by_key(user_id, key.trim()).await? else {
        return Ok(None);
    };
    let same = existing.kind == kind
        && match kind {
            "template" => existing.template_slug.as_deref() == subject,
            _ => existing.group_id == group_id,
        };
    if !same {
        return Err(ErrorBuilder::new(StatusCode::CONFLICT)
            .type_("https://temps.sh/probs/idempotency-key-reused")
            .title("Idempotency Key Already Used")
            .detail("This idempotency_key was already used for a different request; send a new key")
            .build());
    }
    Ok(Some(state.engine.describe(&existing)))
}

/// Store a new instantiation, handling the idempotency race.
///
/// Two concurrent requests with the same `idempotency_key` can both pass the
/// `replay` check (finding nothing) and then race to insert. The second insert
/// fails with a unique-constraint violation on `(user_id, idempotency_key)`.
/// Rather than surfacing a 500, we re-run `replay` and return the winner's
/// instantiation exactly as an idempotent success.
#[allow(clippy::too_many_arguments)]
async fn store_or_replay(
    state: &InstantiationAppState,
    user_id: i32,
    key: &str,
    kind: &str,
    plan: &crate::instantiation::InstantiationPlan,
    secrets: &crate::instantiation::PlanSecrets,
    subject: Option<&str>,
    group_id: Option<i32>,
    auth: &temps_auth::AuthContext,
    origin: &RequestOrigin,
) -> Result<(StatusCode, InstantiationResponse), Problem> {
    match state.engine.store(user_id, key, kind, plan, secrets).await {
        Ok(stored) => {
            state.engine.spawn(stored.id, auth.clone(), origin.clone());
            Ok((StatusCode::ACCEPTED, state.engine.describe(&stored)))
        }
        Err(crate::error::ProjectGroupError::Database { ref source, .. })
            if matches!(source.sql_err(), Some(SqlErr::UniqueConstraintViolation(_))) =>
        {
            // Race: the other requester won. Return their instantiation.
            let replayed = replay(state, user_id, key, kind, subject, group_id)
                .await?
                .ok_or_else(|| {
                    Problem::from(crate::error::ProjectGroupError::Internal {
                        message: format!(
                            "idempotency race: unique constraint on key '{key}' but no row found"
                        ),
                    })
                })?;
            Ok((StatusCode::OK, replayed))
        }
        Err(error) => Err(Problem::from(error)),
    }
}

/// Check the slug of every planned service against the reserved slugs, and
/// that no two planned services share one.
async fn plan_service_slugs(
    state: &InstantiationAppState,
    auth: &temps_auth::AuthContext,
    names: Vec<(String, String)>,
) -> Result<Vec<PlannedService>, Problem> {
    let slug_claim =
        SlugClaimCaller::from_request(auth, state.sensitive_action_authorizer.as_ref());
    let mut seen = HashSet::new();
    let mut services = Vec::with_capacity(names.len());
    for (template_service, name) in names {
        let mut slug = state
            .projects
            .plan_project_slug(&name)
            .await
            .map_err(Problem::from)?;
        // Two services of one request can plan the same free slug.
        let base = slug.clone();
        let mut suffix = 2;
        while !seen.insert(slug.clone()) {
            slug = format!("{base}-{suffix}");
            suffix += 1;
        }
        state
            .projects
            .preflight_guard_reserved_slug(&slug, &slug_claim)
            .await
            .map_err(Problem::from)?;
        services.push(PlannedService {
            template_service,
            name,
            slug,
            runtime_overrides: Default::default(),
            project_id: None,
            creation_started: false,
            // Filled in by `build_plan`, which assigns the variables.
            supplied_variable_names: Vec::new(),
        });
    }
    Ok(services)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Create a template
///
/// Creates every service of a template, the databases it needs, and, for a
/// template going into a project, its shared variables and path routes. The
/// request is checked in full, then runs in the background: poll
/// `GET /instantiations/{id}`. Sending the same `idempotency_key` again
/// returns the same instantiation.
#[utoipa::path(
    post,
    path = "/templates/{slug}/instantiate",
    tag = "Templates",
    params(("slug" = String, Path, description = "Template slug")),
    request_body = InstantiateTemplateRequest,
    responses(
        (status = 202, description = "Accepted; the services are being created", body = InstantiationResponse),
        (status = 200, description = "The instantiation already started with this idempotency key", body = InstantiationResponse),
        (status = 400, description = "Invalid request"),
        (status = 403, description = "Insufficient permissions, or a reserved slug only an instance admin may claim"),
        (status = 404, description = "Template or project not found"),
        (status = 409, description = "The idempotency key was used for a different request"),
        (status = 428, description = "A reserved slug requires a recently MFA-verified session"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn instantiate_template(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<InstantiationAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(slug): Path<String>,
    Json(request): Json<InstantiateTemplateRequest>,
) -> Result<impl IntoResponse, Problem> {
    reject_deployment_token(&auth)?;
    require_template_creation_permissions(&auth)?;
    validate_idempotency_key(&request.idempotency_key)?;
    let group_id = match request.target {
        InstantiationTarget::ExistingProject { group_id } => Some(group_id),
        _ => None,
    };
    if let Some(existing) = replay(
        &state,
        auth.user_id(),
        &request.idempotency_key,
        "template",
        Some(&slug),
        group_id,
    )
    .await?
    {
        return Ok((StatusCode::OK, Json(existing)));
    }

    let template = state.templates.get_template(&slug).await.map_err(|error| {
        ErrorBuilder::new(StatusCode::NOT_FOUND)
            .title("Template Not Found")
            .detail(error.to_string())
            .build()
    })?;
    let (template, provenance, carries_release) = if template.kind == TemplateKind::App {
        let release = state
            .templates
            .get_service_template_instance(&slug)
            .await
            .map_err(|error| bad_request("Invalid App Template", error.to_string()))?;
        (release.template, release.slug, true)
    } else {
        let provenance = template_provenance(&template).to_string();
        (template, provenance, false)
    };
    for service in &template.services {
        temps_projects::handlers::compose_security::reject_legacy_sandbox_grants(
            service.preset_config.as_ref(),
        )?;
    }

    // Where the services go.
    let (target, base_name) = match &request.target {
        InstantiationTarget::NewProject {
            name,
            slug: explicit_slug,
        } => {
            let name = ProjectGroupService::validate_name(name)?;
            let group_slug = match explicit_slug.as_deref().filter(|s| !s.trim().is_empty()) {
                Some(slug) => {
                    let slug = slug.trim().to_string();
                    ProjectGroupService::validate_slug(&slug)?;
                    slug
                }
                None => {
                    let derived = ProjectGroupService::slugify(&name);
                    if derived.is_empty() {
                        return Err(bad_request(
                            "Slug Required",
                            "The project name does not produce a valid slug; pass an explicit slug \
                             in the `target.slug` field",
                        ));
                    }
                    ProjectGroupService::validate_slug(&derived)?;
                    derived
                }
            };
            if state
                .groups
                .service
                .get_group_by_slug(&group_slug)
                .await
                .is_ok()
            {
                return Err(ProjectGroupError::SlugConflict { slug: group_slug }.into());
            }
            (
                PlannedTarget::NewProject {
                    name: name.clone(),
                    slug: group_slug.clone(),
                },
                group_slug,
            )
        }
        InstantiationTarget::ExistingProject { group_id } => {
            // Joining a project hands its shared variables to the new
            // services, and the template's to its members: what managing
            // those variables takes.
            let group = authorize_group_env(&state.groups, &auth, *group_id, true).await?;
            (
                PlannedTarget::ExistingProject {
                    group_id: *group_id,
                },
                group.group.slug,
            )
        }
        InstantiationTarget::NoProject => (PlannedTarget::NoProject, template.slug.clone()),
    };

    // Names of the services.
    for name in request
        .service_names
        .keys()
        .chain(request.runtime_overrides.keys())
    {
        if template.service(name).is_none() {
            return Err(bad_request(
                "Unknown Service",
                format!("'{}' has no service named '{name}'", template.name),
            ));
        }
    }
    let names: Vec<(String, String)> = template
        .services
        .iter()
        .map(|service| {
            let name = request
                .service_names
                .get(&service.name)
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| {
                    if template.is_multi_service() {
                        format!("{base_name}-{}", service.name)
                    } else if let InstantiationTarget::NewProject { name, .. } = &request.target {
                        name.trim().to_string()
                    } else {
                        base_name.clone()
                    }
                });
            (service.name.clone(), name)
        })
        .collect();
    let mut services = plan_service_slugs(&state, &auth, names).await?;
    for service in &mut services {
        let spec = template
            .service(&service.template_service)
            .ok_or_else(|| bad_request(
                "Unknown Service",
                format!(
                    "'{}' has no service named '{}' (this is a bug; the plan was built from this template)",
                    template.name, service.template_service
                ),
            ))?;
        let overrides = request
            .runtime_overrides
            .get(&service.template_service)
            .cloned()
            .unwrap_or_default();
        resolve_image_template_runtime(spec, &overrides).map_err(|error| {
            bad_request("Invalid Template Runtime Configuration", error.to_string())
        })?;
        service.runtime_overrides = overrides;
    }

    // Fork: the code becomes the caller's, so it only makes sense for a
    // starter that builds from the template's repository.
    if let Some(fork) = &request.fork {
        if carries_release {
            return Err(bad_request(
                "Apps Cannot Be Forked",
                format!(
                    "'{}' is an app: it deploys a pinned release so it can be upgraded later. Create it without a fork.",
                    template.name
                ),
            ));
        }
        if !template
            .services
            .iter()
            .any(|service| !service.is_image() && service.git.is_none())
        {
            return Err(bad_request(
                "Nothing To Fork",
                format!(
                    "No service of '{}' builds from its repository",
                    template.name
                ),
            ));
        }
        if fork.repository_name.trim().is_empty() {
            return Err(bad_request(
                "Repository Name Required",
                "fork.repository_name is required",
            ));
        }
        permission_guard!(auth, GitConnectionsRead);
        // The fork runs later; a connection that is missing or not the
        // caller's must fail now, before the project is created.
        state
            .engine
            .git
            .get_connection_for_user(fork.git_provider_connection_id, auth.user_id())
            .await
            .map_err(Problem::from)?;
    }

    // Databases: linking existing ones takes access to them; creating new
    // ones takes the right to create databases.
    let existing_ids: Vec<i32> = request
        .databases
        .values()
        .filter_map(|choice| match choice {
            DatabaseChoice::Existing { service_id } => Some(*service_id),
            DatabaseChoice::Create { .. } => None,
        })
        .collect();
    let storage_claim_ids = if existing_ids.is_empty() {
        Vec::new()
    } else {
        permission_guard!(auth, ExternalServicesWrite);
        authorize_storage_services(
            &state.external_services,
            state.groups.project_access_checker.as_deref(),
            &auth,
            &existing_ids,
        )
        .await?
    };
    if request
        .databases
        .values()
        .any(|choice| matches!(choice, DatabaseChoice::Create { .. }))
    {
        permission_guard!(auth, ExternalServicesCreate);
    }
    for (database_type, choice) in &request.databases {
        if let DatabaseChoice::Existing { service_id } = choice {
            let selected = state
                .external_services
                .get_service(*service_id)
                .await
                .map_err(|_| temps_projects::template_creation::storage_service_access_denied())?;
            let selected_type = selected.service_type.to_string();
            if !managed_service_types_compatible(database_type, &selected_type) {
                return Err(bad_request(
                    "Wrong Database Type",
                    format!("Database {service_id} is {selected_type}, not {database_type}"),
                ));
            }
        }
    }

    let (plan, secrets) = build_plan(PlanInput {
        template: template.clone(),
        carries_release,
        provenance: Some(provenance),
        target,
        fork: request.fork.clone(),
        repository: None,
        follow_repository: false,
        databases: request.databases.clone(),
        services,
        storage_claim_ids,
        group_id,
        variables: &request.environment_variables,
    })
    .map_err(|error| planning_problem("Invalid Template Request", error))?;

    // Required values must be known now, not discovered halfway through.
    for service in &template.services {
        let mut declared = service.env_vars.clone();
        if matches!(plan.target, PlannedTarget::NoProject) {
            declared.extend(template.shared_env_vars.iter().cloned());
        }
        let configured: BTreeSet<String> = secrets
            .services
            .get(&service.name)
            .into_iter()
            .flatten()
            .filter(|variable| !variable.value.is_empty())
            .map(|variable| variable.name.clone())
            .chain(
                service
                    .database_bindings
                    .values()
                    .flat_map(|bindings| bindings.keys().cloned()),
            )
            .collect();
        let missing = missing_required_template_configuration(&declared, &configured);
        if !missing.is_empty() {
            let names: Vec<&str> = missing
                .iter()
                .map(|variable| variable.name.as_str())
                .collect();
            return Err(bad_request(
                "Missing Required Variables",
                format!(
                    "Service '{}' needs values for: {}",
                    service.name,
                    names.join(", ")
                ),
            ));
        }
    }

    // The values every service will be created with must resolve now. A fork
    // creates a remote repository before the first service is made, so a
    // value that only fails then (a duplicate, a generator that cannot run)
    // would leave that repository behind and block a corrected retry under
    // the same name.
    validate_plan_variables(&plan, &secrets)?;

    let key = request.idempotency_key.trim();
    let (status, response) = store_or_replay(
        &state,
        auth.user_id(),
        key,
        "template",
        &plan,
        &secrets,
        Some(&slug),
        None,
        &auth,
        &origin(&metadata),
    )
    .await?;
    Ok((status, Json(response)))
}

/// Import apps as services of a project
///
/// Creates one service per app, each building from its directory of the
/// project's repository and following it. Runs in the background like
/// `POST /templates/{slug}/instantiate`: poll `GET /instantiations/{id}`.
#[utoipa::path(
    post,
    path = "/project-groups/{group_id}/import",
    tag = "Project Groups",
    params(("group_id" = i32, Path, description = "Project group ID")),
    request_body = ImportServicesRequest,
    responses(
        (status = 202, description = "Accepted; the services are being created", body = InstantiationResponse),
        (status = 200, description = "The import already started with this idempotency key", body = InstantiationResponse),
        (status = 400, description = "Invalid request, or the project has no repository"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project group not found"),
        (status = 409, description = "The idempotency key was used for a different request"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn import_project_services(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<InstantiationAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(group_id): Path<i32>,
    Json(request): Json<ImportServicesRequest>,
) -> Result<impl IntoResponse, Problem> {
    reject_deployment_token(&auth)?;
    require_template_creation_permissions(&auth)?;
    validate_idempotency_key(&request.idempotency_key)?;
    if let Some(existing) = replay(
        &state,
        auth.user_id(),
        &request.idempotency_key,
        "import",
        None,
        Some(group_id),
    )
    .await?
    {
        return Ok((StatusCode::OK, Json(existing)));
    }

    let group = authorize_group_env(&state.groups, &auth, group_id, true).await?;
    let repository = state
        .groups
        .repository
        .repository_of(&group.group)
        .await?
        .ok_or(ProjectGroupError::RepositoryNotSet { group_id })?;
    let target = RepositoryTarget {
        git_url: repository.clone_url.clone(),
        git_provider_connection_id: repository.git_provider_connection_id,
        is_public: repository.public_provider.is_some(),
        repo_owner: repository.repo_owner.clone(),
        repo_name: repository.repo_name.clone(),
        branch: repository.main_branch.clone(),
        directory: ".".to_string(),
        automatic_deploy: repository.git_provider_connection_id.is_some(),
    };
    if target.git_provider_connection_id.is_none() && !target.is_public {
        return Err(bad_request(
            "Repository Not Connected",
            format!(
                "The git connection of {}/{} was removed; set the project's repository again before importing",
                repository.repo_owner, repository.repo_name
            ),
        ));
    }
    let clone_url = repository.clone_url.clone().unwrap_or_else(|| {
        format!(
            "https://github.com/{}/{}.git",
            repository.repo_owner, repository.repo_name
        )
    });
    let template = import_template(
        &group.group,
        clone_url,
        repository.main_branch.clone(),
        &request.apps,
    )
    .map_err(|error| planning_problem("Invalid Import", error))?;
    let problems = TemplateService::validate_template(&template);
    if !problems.is_empty() {
        return Err(bad_request("Invalid Import", problems.join("; ")));
    }
    for service in &template.services {
        temps_projects::handlers::compose_security::reject_legacy_sandbox_grants(
            service.preset_config.as_ref(),
        )?;
    }

    let names = template
        .services
        .iter()
        .zip(&request.apps)
        .map(|(service, app)| (service.name.clone(), app.name.trim().to_string()))
        .collect();
    let services = plan_service_slugs(&state, &auth, names).await?;
    let (mut plan, secrets) = build_plan(PlanInput {
        template,
        carries_release: false,
        provenance: None,
        target: PlannedTarget::ExistingProject { group_id },
        fork: None,
        repository: Some(target),
        follow_repository: true,
        databases: BTreeMap::new(),
        services,
        storage_claim_ids: Vec::new(),
        group_id: Some(group_id),
        variables: &[],
    })
    .map_err(|error| planning_problem("Invalid Import", error))?;
    plan.template.name = format!("Import into {}", group.group.name);

    let key = request.idempotency_key.trim();
    let (status, response) = store_or_replay(
        &state,
        auth.user_id(),
        key,
        "import",
        &plan,
        &secrets,
        None,
        Some(group_id),
        &auth,
        &origin(&metadata),
    )
    .await?;
    Ok((status, Json(response)))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ListInstantiationsQuery {
    /// Only instantiations that put services into this project.
    pub group_id: Option<i32>,
    /// 1-indexed page number (default 1).
    pub page: Option<u64>,
    /// Items per page (default 20, max 100). `limit` is accepted as an alias
    /// for compatibility with earlier clients.
    pub per_page: Option<u64>,
    /// Alias for `per_page`; `per_page` takes precedence when both are sent.
    pub limit: Option<u64>,
}

/// Paginated list of instantiations.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct InstantiationListResponse {
    pub items: Vec<InstantiationResponse>,
    pub total: u64,
    pub page: u64,
    pub per_page: u64,
}

/// List your recent template creations and imports
#[utoipa::path(
    get,
    path = "/instantiations",
    tag = "Templates",
    params(ListInstantiationsQuery),
    responses(
        (status = 200, description = "Newest first, paginated", body = InstantiationListResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_instantiations(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<InstantiationAppState>>,
    Query(query): Query<ListInstantiationsQuery>,
) -> Result<impl IntoResponse, Problem> {
    reject_deployment_token(&auth)?;
    // Filtering by a project the caller cannot see would tell them it exists.
    if let Some(group_id) = query.group_id {
        let hidden = hidden_projects(&state.groups, &auth).await?;
        visible_or_not_found(
            state.groups.service.get_group(group_id).await?,
            &hidden,
            ProjectGroupError::NotFound { group_id },
        )?;
    }
    // `per_page` takes precedence; `limit` is the backward-compatible alias.
    let per_page = query.per_page.or(query.limit).unwrap_or(20);
    let page = query.page.unwrap_or(1);
    let (models, total) = state
        .engine
        .list(Some(auth.user_id()), query.group_id, page, per_page)
        .await?;
    let per_page = per_page.clamp(1, 100);
    Ok(Json(InstantiationListResponse {
        items: models
            .iter()
            .map(|model| state.engine.describe(model))
            .collect(),
        total,
        page: page.max(1),
        per_page,
    }))
}

/// The instantiation, if the caller may see it: its creator, or an instance
/// admin. Anything else is reported as missing.
async fn owned_instantiation(
    state: &InstantiationAppState,
    auth: &temps_auth::AuthContext,
    id: i32,
) -> Result<temps_entities::template_instantiations::Model, Problem> {
    let not_found = || {
        ErrorBuilder::new(StatusCode::NOT_FOUND)
            .type_("https://temps.sh/probs/instantiation-not-found")
            .title("Instantiation Not Found")
            .detail(format!("Instantiation {id} does not exist"))
            .build()
    };
    let model = state.engine.find(id).await?.ok_or_else(not_found)?;
    if model.user_id != auth.user_id() && !auth.is_instance_admin() {
        return Err(not_found());
    }
    Ok(model)
}

/// Show a template creation or import
#[utoipa::path(
    get,
    path = "/instantiations/{id}",
    tag = "Templates",
    params(("id" = i32, Path, description = "Instantiation ID")),
    responses(
        (status = 200, description = "Its status and every step", body = InstantiationResponse),
        (status = 404, description = "Not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_instantiation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<InstantiationAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    reject_deployment_token(&auth)?;
    let model = owned_instantiation(&state, &auth, id).await?;
    Ok(Json(state.engine.describe(&model)))
}

/// Retry a template creation or import
///
/// Continues from the step that failed (or was interrupted by a restart);
/// steps that finished are not run again.
#[utoipa::path(
    post,
    path = "/instantiations/{id}/retry",
    tag = "Templates",
    params(("id" = i32, Path, description = "Instantiation ID")),
    responses(
        (status = 202, description = "Running again", body = InstantiationResponse),
        (status = 404, description = "Not found"),
        (status = 409, description = "It is running or already succeeded"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn retry_instantiation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<InstantiationAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    reject_deployment_token(&auth)?;
    require_template_creation_permissions(&auth)?;
    let model = owned_instantiation(&state, &auth, id).await?;
    let current = state.engine.describe(&model);
    // Only stored states that can still make progress; anything else
    // (a row edited by hand) is never run again.
    if !matches!(model.status.as_str(), "pending" | "running" | "failed") {
        return Err(ErrorBuilder::new(StatusCode::CONFLICT)
            .type_("https://temps.sh/probs/instantiation-not-retryable")
            .title("Nothing To Retry")
            .detail(match model.status.as_str() {
                "succeeded" => "It already succeeded".to_string(),
                other => format!("It is {other}, which cannot be retried"),
            })
            .build());
    }
    match current.status {
        InstantiationStatus::Failed | InstantiationStatus::Interrupted => {}
        InstantiationStatus::Pending if !state.engine.is_active(id) => {}
        status => {
            return Err(ErrorBuilder::new(StatusCode::CONFLICT)
                .type_("https://temps.sh/probs/instantiation-not-retryable")
                .title("Nothing To Retry")
                .detail(match status {
                    InstantiationStatus::Succeeded => "It already succeeded".to_string(),
                    _ => "It is still running; wait for it to finish".to_string(),
                })
                .build());
        }
    }
    // Whoever retries must still be allowed everything the remaining steps
    // do: change the project the services go into, and link the databases.
    let plan: InstantiationPlan = serde_json::from_value(model.plan.clone()).map_err(|error| {
        temps_core::error_builder::internal_server_error()
            .title("Instantiation Unreadable")
            .detail(format!("Its stored plan could not be read: {error}"))
            .build()
    })?;
    if let PlannedTarget::ExistingProject { group_id } = plan.target {
        if plan.group_id != Some(group_id) {
            return Err(temps_core::error_builder::internal_server_error()
                .title("Instantiation Unreadable")
                .detail("Its stored plan names no project")
                .build());
        }
    }
    // The retention cleanup clears the stored variable values of a failed
    // run. Retrying then would create services without what the caller
    // supplied and skip the shared variables, yet report success.
    ensure_secrets_available(id, model.secrets.is_some(), &plan).map_err(Problem::from)?;
    if let Some(group_id) = plan.group_id {
        authorize_group_env(&state.groups, &auth, group_id, true).await?;
    }
    let linked: Vec<i32> = plan
        .databases
        .iter()
        .filter_map(|database| match database.choice {
            DatabaseChoice::Existing { service_id } => Some(service_id),
            DatabaseChoice::Create { .. } => None,
        })
        .collect();
    if !linked.is_empty() {
        permission_guard!(auth, ExternalServicesWrite);
        authorize_storage_services(
            &state.external_services,
            state.groups.project_access_checker.as_deref(),
            &auth,
            &linked,
        )
        .await?;
    }
    if plan
        .databases
        .iter()
        .any(|database| database.service_id.is_none())
    {
        permission_guard!(auth, ExternalServicesCreate);
    }
    if let (Some(fork), None) = (&plan.fork, &plan.repository) {
        permission_guard!(auth, GitConnectionsRead);
        state
            .engine
            .git
            .get_connection_for_user(fork.git_provider_connection_id, auth.user_id())
            .await
            .map_err(Problem::from)?;
    }
    let retry_origin = origin(&metadata);
    {
        let group_id = plan.group_id.unwrap_or(0);
        let group_slug = match &plan.target {
            PlannedTarget::NewProject { slug, .. } => slug.clone(),
            _ => String::new(),
        };
        let mut event = ProjectGroupAudit::from_engine(
            "TEMPLATE_INSTANTIATION_RETRIED",
            auth.user_id(),
            &retry_origin,
            group_id,
            &group_slug,
        );
        event.instantiation_id = Some(id);
        event.template = Some(plan.template.slug.clone());
        event.record(&state.engine.audit).await;
    }
    if !state.engine.spawn(id, auth.clone(), retry_origin) {
        return Err(ErrorBuilder::new(StatusCode::CONFLICT)
            .type_("https://temps.sh/probs/instantiation-not-retryable")
            .title("Nothing To Retry")
            .detail("It is already running")
            .build());
    }
    let mut response = current;
    response.status = InstantiationStatus::Running;
    response.error = None;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rejected_request_is_a_400_with_the_endpoint_title() {
        let problem = planning_problem(
            "Invalid Import",
            InstantiationError::Validation {
                message: "app 'web' has no path".into(),
            },
        );
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
        assert_eq!(problem.body["title"], "Invalid Import");
        assert_eq!(problem.body["detail"], "app 'web' has no path");
    }

    #[test]
    fn a_lost_claim_is_a_conflict_naming_the_instantiation() {
        let problem = Problem::from(InstantiationError::ClaimLost {
            instantiation_id: 42,
        });
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        assert!(problem.body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("42"));
    }

    #[test]
    fn a_step_failure_is_classified_by_its_cause() {
        let problem = Problem::from(InstantiationError::StepFailed {
            step: "database".into(),
            source: Box::new(InstantiationError::DatabaseNameTaken {
                name: "orders".into(),
            }),
        });
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
    }

    #[test]
    fn an_upstream_refusal_keeps_its_status() {
        let problem = Problem::from(InstantiationError::Upstream {
            status: StatusCode::FORBIDDEN,
            message: "Forbidden: missing permission".into(),
        });
        assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
    }

    #[test]
    fn corrupt_state_is_a_500_and_variables_are_a_400() {
        let corrupt = Problem::from(InstantiationError::UnknownStep { key: "x".into() });
        assert_eq!(corrupt.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        let variables = Problem::from(InstantiationError::Variables(
            TemplateEnvironmentError::MissingRequired {
                name: "TOKEN".into(),
            },
        ));
        assert_eq!(variables.status_code, StatusCode::BAD_REQUEST);
    }
}
