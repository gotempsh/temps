// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::audit::{
    AuditContext, ProjectCreatedAudit, ProjectDeletedAudit, ProjectSettingsUpdatedAudit,
    ProjectSettingsUpdatedFields, ProjectUpdatedAudit, ProjectUpdatedFields,
};
use utoipa::OpenApi;

use super::AppState;
use axum::Router;
use axum::{
    extract::{Extension, Multipart, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post, put},
    Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use temps_auth::{
    permission_guard, project_access_guard, project_permission_guard, project_scope_guard,
};
use temps_auth::{AuthContext, Permission, RequireAuth, Role};
use temps_core::RequestMetadata;
use tracing::{debug, error, info, warn};

use super::types::{
    ChangeProjectSourceRequest, CreateProjectRequest, PaginatedProjectList, PaginationParams,
    ProjectResponse, ProjectStatisticsResponse, ReinstallWebhookResponse,
    ServiceTemplateChangeKind, ServiceTemplateInstanceResponse, ServiceTemplateUpgradeChange,
    SetAlternateSourcesRequest, TriggerPipelinePayload, TriggerPipelineResponse,
    UpdateAutomaticDeployRequest, UpdateDeploymentConfigRequest, UpdateGitSettingsRequest,
    UpdateProjectSettingsRequest, UpdateServiceTemplateRuntimeRequest,
    UpgradeServiceTemplateRequest,
};
use crate::services::types::CreateProjectEnvVar;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use temps_core::problemdetails;
use temps_core::problemdetails::Problem;
use temps_entities::source_type::SourceType;
use tokio::io::AsyncWriteExt;

pub fn configure_routes() -> Router<Arc<AppState>> {
    use axum::extract::DefaultBodyLimit;
    let custom_domain_routes = super::custom_domains::configure_routes();

    Router::new()
        // Project CRUD routes
        .route("/projects/{id}", get(get_project))
        .route("/projects/by-slug/{slug}", get(get_project_by_slug))
        .route("/projects/{id}", put(update_project))
        .route("/projects/{id}/source", patch(change_project_source))
        .route(
            "/projects/{id}/alternate-sources",
            patch(set_alternate_sources),
        )
        .route("/projects/{id}", delete(delete_project))
        .route("/projects", post(create_project))
        .route("/projects", get(get_projects))
        .route(
            "/drop/inspect",
            post(inspect_drop_archive).layer(DefaultBodyLimit::max(501 * 1024 * 1024)),
        )
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
            "/projects/{project_id}/service-runtime",
            patch(update_service_template_runtime),
        )
        .route(
            "/projects/{project_id}/service-template",
            get(get_project_service_template),
        )
        .route(
            "/projects/{project_id}/service-template/upgrade",
            post(upgrade_project_service_template),
        )
        .route(
            "/projects/{project_id}/gitlab/reinstall-webhook",
            post(reinstall_gitlab_webhook),
        )
        // Merge custom domain routes
        .merge(custom_domain_routes)
}

fn storage_service_access_denied() -> Problem {
    problemdetails::new(StatusCode::FORBIDDEN)
        .with_title("Insufficient Permissions")
        .with_detail("You do not have access to one or more selected databases")
}

fn text_option(value: Option<impl ToString>) -> Option<String> {
    value.map(|value| value.to_string())
}

fn production_environment_variable_names(
    variables: &[crate::services::types::EnvVarWithEnvironments],
) -> BTreeSet<String> {
    // A production-scoped value overrides a global value with the same key,
    // including when it is intentionally empty. Only fall back to the global
    // row when there is no production-specific row.
    let mut configured = BTreeMap::<String, (bool, bool)>::new();
    for variable in variables {
        let production_scoped = variable
            .environments
            .iter()
            .any(|environment| environment.name.eq_ignore_ascii_case("production"));
        let global = variable.environments.is_empty();
        if !production_scoped && !global {
            continue;
        }

        let entry = configured
            .entry(variable.key.clone())
            .or_insert((false, false));
        if production_scoped {
            if !entry.0 {
                *entry = (true, false);
            }
            entry.1 |= variable.has_value;
        } else if !entry.0 {
            entry.1 |= variable.has_value;
        }
    }

    configured
        .into_iter()
        .filter_map(|(key, (_, has_value))| has_value.then_some(key))
        .collect()
}

fn push_template_change(
    changes: &mut Vec<ServiceTemplateUpgradeChange>,
    field: impl Into<String>,
    current: Option<String>,
    target: Option<String>,
) {
    if current == target {
        return;
    }
    let kind = match (&current, &target) {
        (None, Some(_)) => ServiceTemplateChangeKind::Added,
        (Some(_), None) => ServiceTemplateChangeKind::Removed,
        _ => ServiceTemplateChangeKind::Changed,
    };
    changes.push(ServiceTemplateUpgradeChange {
        field: field.into(),
        kind,
        current,
        target,
    });
}

fn template_input_summary(input: &temps_core::templates::EnvVarTemplate) -> String {
    let mut parts = vec![if input.required {
        "required"
    } else {
        "optional"
    }
    .to_string()];
    if let Some(generator) = input.default_generator.as_deref() {
        parts.push(format!("generator={generator}"));
    } else if let Some(default) = input.default.as_deref().filter(|_| !input.is_secret()) {
        parts.push(format!("default={default}"));
    }
    parts.join(", ")
}

fn service_template_changes(
    applied: &temps_core::templates::ProjectTemplate,
    target: &temps_core::templates::ProjectTemplate,
) -> Vec<ServiceTemplateUpgradeChange> {
    let mut changes = Vec::new();
    push_template_change(
        &mut changes,
        "image",
        applied.image.clone(),
        target.image.clone(),
    );
    push_template_change(
        &mut changes,
        "command",
        applied.command.as_ref().map(|value| value.join(" ")),
        target.command.as_ref().map(|value| value.join(" ")),
    );
    push_template_change(
        &mut changes,
        "health_check_path",
        applied.health_check_path.clone(),
        target.health_check_path.clone(),
    );
    push_template_change(
        &mut changes,
        "exposed_port",
        text_option(applied.exposed_port),
        text_option(target.exposed_port),
    );

    let applied_resources = applied.resources.as_ref();
    let target_resources = target.resources.as_ref();
    for (field, current, next) in [
        (
            "resources.cpu_request",
            applied_resources.and_then(|value| value.cpu_request),
            target_resources.and_then(|value| value.cpu_request),
        ),
        (
            "resources.cpu_limit",
            applied_resources.and_then(|value| value.cpu_limit),
            target_resources.and_then(|value| value.cpu_limit),
        ),
        (
            "resources.memory_request",
            applied_resources.and_then(|value| value.memory_request),
            target_resources.and_then(|value| value.memory_request),
        ),
        (
            "resources.memory_limit",
            applied_resources.and_then(|value| value.memory_limit),
            target_resources.and_then(|value| value.memory_limit),
        ),
    ] {
        push_template_change(&mut changes, field, text_option(current), text_option(next));
    }

    let applied_inputs = applied
        .env_vars
        .iter()
        .map(|input| (input.name.as_str(), input))
        .collect::<BTreeMap<_, _>>();
    let target_inputs = target
        .env_vars
        .iter()
        .map(|input| (input.name.as_str(), input))
        .collect::<BTreeMap<_, _>>();
    let input_names = applied_inputs
        .keys()
        .chain(target_inputs.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    for name in input_names {
        let current = applied_inputs
            .get(name)
            .map(|input| template_input_summary(input));
        let next = target_inputs
            .get(name)
            .map(|input| template_input_summary(input));
        if applied_inputs.get(name) != target_inputs.get(name) {
            push_template_change(&mut changes, format!("configuration.{name}"), current, next);
        }
    }

    let applied_services = applied.services.iter().collect::<BTreeSet<_>>();
    let target_services = target.services.iter().collect::<BTreeSet<_>>();
    for service in applied_services.union(&target_services) {
        push_template_change(
            &mut changes,
            format!("managed_service.{service}"),
            applied_services
                .contains(service)
                .then(|| "required".to_string()),
            target_services
                .contains(service)
                .then(|| "required".to_string()),
        );
    }

    let applied_bindings = applied
        .managed_service_bindings
        .iter()
        .flat_map(|(service, bindings)| {
            bindings
                .iter()
                .map(move |(target, source)| ((service.as_str(), target.as_str()), source.as_str()))
        })
        .collect::<BTreeMap<_, _>>();
    let target_bindings = target
        .managed_service_bindings
        .iter()
        .flat_map(|(service, bindings)| {
            bindings
                .iter()
                .map(move |(target, source)| ((service.as_str(), target.as_str()), source.as_str()))
        })
        .collect::<BTreeMap<_, _>>();
    for binding in applied_bindings
        .keys()
        .chain(target_bindings.keys())
        .copied()
        .collect::<BTreeSet<_>>()
    {
        let (service, target_variable) = binding;
        push_template_change(
            &mut changes,
            format!("managed_service_binding.{service}.{target_variable}"),
            applied_bindings
                .get(&binding)
                .map(|source| (*source).to_string()),
            target_bindings
                .get(&binding)
                .map(|source| (*source).to_string()),
        );
    }
    changes
}

async fn require_storage_services_access(
    state: &AppState,
    auth: &AuthContext,
    service_ids: &[i32],
) -> Result<Vec<i32>, Problem> {
    if service_ids.is_empty() {
        return Ok(Vec::new());
    }

    let scopes = state
        .external_service_manager
        .project_scopes_for_services(service_ids)
        .await
        .map_err(|error| match &error {
            temps_providers::ExternalServiceError::ServiceNotFound { .. } => {
                storage_service_access_denied()
            }
            error => {
                error!(error = %error, "failed to resolve selected database access scopes");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Authorization Failed")
                    .with_detail("The selected databases could not be authorized")
            }
        })?;

    authorize_storage_service_scopes(auth, state.project_access_checker.as_deref(), &scopes).await
}

async fn authorize_storage_service_scopes(
    auth: &AuthContext,
    checker: Option<&dyn temps_core::ProjectAccessChecker>,
    scopes: &[temps_providers::ExternalServiceProjectScope],
) -> Result<Vec<i32>, Problem> {
    if auth.is_admin() || auth.has_role(&Role::PlatformAdmin) {
        return Ok(Vec::new());
    }
    let Some(checker) = checker else {
        // Plain OSS has no team boundary.
        return Ok(Vec::new());
    };

    let project_ids: Vec<i32> = scopes
        .iter()
        .flat_map(|scope| scope.project_ids.iter().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let permissions = checker
        .effective_project_permissions_batch(auth.user_id(), &project_ids)
        .await
        .map_err(|checker_error| {
            error!(
                user_id = auth.user_id(),
                ?project_ids,
                error = %checker_error,
                "selected database permission resolution failed closed"
            );
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Database Authorization Failed")
                .with_detail("Project permissions for the selected databases could not be resolved")
        })?;
    if project_ids
        .iter()
        .any(|project_id| !permissions.contains_key(project_id))
    {
        error!(
            user_id = auth.user_id(),
            ?project_ids,
            "selected database permission result omitted a project"
        );
        return Err(problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Database Authorization Failed")
            .with_detail("Project permissions for the selected databases were incomplete"));
    }

    let fallback_ids: Vec<i32> = project_ids
        .iter()
        .copied()
        .filter(|project_id| matches!(permissions.get(project_id), Some(None)))
        .collect();
    let coarse_access = checker
        .user_can_access_projects(auth.user_id(), &fallback_ids)
        .await
        .map_err(|checker_error| {
            error!(
                user_id = auth.user_id(),
                ?fallback_ids,
                error = %checker_error,
                "selected database membership resolution failed closed"
            );
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Database Authorization Failed")
                .with_detail("Project access for the selected databases could not be resolved")
        })?;
    if fallback_ids
        .iter()
        .any(|project_id| !coarse_access.contains_key(project_id))
    {
        return Err(problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Database Authorization Failed")
            .with_detail("Project access for the selected databases was incomplete"));
    }

    let required_permission = Permission::ExternalServicesWrite.to_string();
    let can_access_project = |project_id: &i32| match permissions.get(project_id) {
        Some(Some(project_permissions)) => project_permissions.contains(&required_permission),
        Some(None) => coarse_access.get(project_id).copied().unwrap_or(false),
        None => false,
    };
    let mut creator_claims = Vec::new();
    for scope in scopes {
        if scope.project_ids.is_empty() && scope.created_by_user_id == Some(auth.user_id()) {
            creator_claims.push(scope.service_id);
        } else if scope.project_ids.is_empty() || !scope.project_ids.iter().any(can_access_project)
        {
            return Err(storage_service_access_denied());
        }
    }

    Ok(creator_claims)
}

#[derive(OpenApi)]
#[openapi(
    paths(
        create_project,
        inspect_drop_archive,
        get_project,
        update_project,
        change_project_source,
        set_alternate_sources,
        delete_project,
        get_projects,
        get_project_by_slug,
        update_project_settings,
        update_git_settings,
        update_automatic_deploy,
        update_project_deployment_config,
        update_service_template_runtime,
        get_project_service_template,
        upgrade_project_service_template,
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
            super::types::ProjectEnvVarInput,
            DropInspectionResponse,
            DropPresetCandidate,
            ChangeProjectSourceRequest,
            SetAlternateSourcesRequest,
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

static DROP_INSPECTIONS_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Upper bound on how many deployable roots a single Drop inspection reports.
/// The response drives a picker, so a 20k-entry list is neither usable nor
/// safe to serialise.
const MAX_DROP_CANDIDATES: usize = 50;

struct DropInspectionPermit;

impl DropInspectionPermit {
    fn acquire() -> Result<Self, Problem> {
        DROP_INSPECTIONS_IN_FLIGHT
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < 4).then_some(current + 1)
            })
            .map_err(|_| {
                problemdetails::new(StatusCode::TOO_MANY_REQUESTS)
                    .with_title("Too Many Drop Inspections")
                    .with_detail("At most four Drop archives may be inspected concurrently")
            })?;
        Ok(Self)
    }
}

impl Drop for DropInspectionPermit {
    fn drop(&mut self) {
        DROP_INSPECTIONS_IN_FLIGHT.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DropPresetCandidate {
    pub directory: String,
    pub preset: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compose_path: Option<String>,
    pub label: String,
    pub confidence: String,
    pub reason: String,
    pub is_static: bool,
    /// Repository-root-relative path to the Dockerfile, when it does not
    /// live directly under `{directory}/Dockerfile` (e.g. `docker/Dockerfile`
    /// rolled up to a `directory` of `"."`). `None` for a Dockerfile located
    /// directly at `{directory}/Dockerfile` and for every non-Dockerfile
    /// preset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dockerfile_path: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DropInspectionResponse {
    pub suggested_name: String,
    pub candidates: Vec<DropPresetCandidate>,
}

#[derive(utoipa::ToSchema)]
pub struct DropArchiveUpload {
    #[schema(value_type = String, format = Binary)]
    pub file: String,
}

/// Inspect a source ZIP without creating a project or retaining the upload.
#[utoipa::path(
    post,
    path = "/drop/inspect",
    tag = "Projects",
    request_body(content = DropArchiveUpload, content_type = "multipart/form-data"),
    responses(
        (status = 200, description = "Detected deployable project roots", body = DropInspectionResponse),
        (status = 400, description = "Invalid or unsupported archive")
    ),
    security(("bearer_auth" = []))
)]
pub async fn inspect_drop_archive(
    RequireAuth(auth): RequireAuth,
    mut multipart: Multipart,
) -> Result<Json<DropInspectionResponse>, Problem> {
    permission_guard!(auth, ProjectsCreate);
    let inspection_permit = DropInspectionPermit::acquire()?;

    const MAX_ARCHIVE_BYTES: u64 = 500 * 1024 * 1024;
    let temporary = tempfile::tempdir().map_err(|error| {
        problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Upload Staging Failed")
            .with_detail(error.to_string())
    })?;
    let archive_path = temporary.path().join("drop-inspect.zip");
    let mut archive_received = false;
    let mut filename = None;
    while let Some(field) = multipart.next_field().await.map_err(|error| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Upload")
            .with_detail(format!("Failed to read multipart upload: {error}"))
    })? {
        if field.name() == Some("file") {
            filename = field.file_name().map(ToString::to_string);
            let mut output = tokio::fs::File::create(&archive_path)
                .await
                .map_err(|error| {
                    problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                        .with_title("Upload Staging Failed")
                        .with_detail(error.to_string())
                })?;
            let mut field = field;
            let mut size = 0u64;
            while let Some(chunk) = field.chunk().await.map_err(|error| {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Upload")
                    .with_detail(format!("Failed to read uploaded archive: {error}"))
            })? {
                size = size.saturating_add(chunk.len() as u64);
                if size > MAX_ARCHIVE_BYTES {
                    return Err(problemdetails::new(StatusCode::PAYLOAD_TOO_LARGE)
                        .with_title("Archive Too Large")
                        .with_detail("Drop archive exceeds 500 MiB"));
                }
                output.write_all(&chunk).await.map_err(|error| {
                    problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                        .with_title("Upload Staging Failed")
                        .with_detail(error.to_string())
                })?;
            }
            output.flush().await.map_err(|error| {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Upload Staging Failed")
                    .with_detail(error.to_string())
            })?;
            archive_received = true;
            break;
        }
    }

    if !archive_received {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Missing Archive")
            .with_detail("Expected a ZIP archive in multipart field 'file'"));
    }
    // Detection is CPU-bound and O(roots x files); it must run inside the same
    // `spawn_blocking` as the ZIP walk so it stays off the async runtime AND
    // stays covered by `inspection_permit`, which is released when this closure
    // returns.
    let candidates = tokio::task::spawn_blocking(move || {
        let _inspection_permit = inspection_permit;
        let manifests = inspect_zip_manifests(&archive_path)?;
        let mut candidates = temps_presets::detect_project_candidates(&manifests)
            .into_iter()
            .map(|candidate| drop_preset_candidate_from(&manifests, candidate))
            .collect::<Vec<_>>();
        // The response is rendered as a picker; an unbounded list is neither
        // useful to a human nor safe to serialise.
        candidates.truncate(MAX_DROP_CANDIDATES);
        Ok::<_, Problem>(candidates)
    })
    .await
    .map_err(|error| {
        problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Archive Inspection Failed")
            .with_detail(error.to_string())
    })??;

    if candidates.is_empty() {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("No Deployable Project Found")
            .with_detail(
                "The archive does not contain a supported project manifest or index.html",
            ));
    }

    let suggested_name = filename
        .as_deref()
        .and_then(|name| name.strip_suffix(".zip"))
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("drop-project")
        .to_string();

    Ok(Json(DropInspectionResponse {
        suggested_name,
        candidates,
    }))
}

/// Convert a detected project candidate into the response DTO for the
/// drop-inspection endpoint, resolving the compose file path alongside it.
fn drop_preset_candidate_from(
    manifests: &BTreeMap<String, String>,
    candidate: temps_presets::ProjectCandidate,
) -> DropPresetCandidate {
    let preset = candidate.catalog_slug().to_string();
    let compose_path = compose_path_for_candidate(manifests, &candidate);
    DropPresetCandidate {
        directory: candidate.path,
        preset,
        compose_path,
        label: candidate.preset.display_name().to_string(),
        confidence: candidate.confidence.to_string(),
        reason: candidate.reason,
        is_static: candidate.preset == temps_entities::preset::Preset::Static,
        dockerfile_path: candidate.dockerfile_path,
    }
}

fn compose_path_for_candidate(
    manifests: &BTreeMap<String, String>,
    candidate: &temps_presets::ProjectCandidate,
) -> Option<String> {
    if candidate.preset != temps_entities::preset::Preset::DockerCompose {
        return None;
    }

    temps_presets::docker_compose::COMPOSE_FILE_NAMES
        .iter()
        .find(|file_name| {
            let archive_path = if candidate.path == "." {
                (*file_name).to_string()
            } else {
                format!("{}/{file_name}", candidate.path.trim_end_matches('/'))
            };
            manifests.contains_key(&archive_path)
        })
        .map(|file_name| (*file_name).to_string())
}

fn inspect_zip_manifests(path: &std::path::Path) -> Result<BTreeMap<String, String>, Problem> {
    const MAX_FILES: usize = 20_000;
    const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
    /// Aggregate budget across every manifest we buffer. `MAX_FILES` x
    /// `MAX_MANIFEST_BYTES` is 20 GiB, and manifests compress ~1000:1, so a
    /// ~15 MB upload could otherwise pin gigabytes of `String` per request and
    /// OOM the whole binary on the 4 GB reference box.
    const MAX_TOTAL_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
    /// Independent cap on how many manifests we are willing to buffer at all.
    const MAX_MANIFESTS: usize = 512;
    /// Aggregate budget for the entry *paths* we retain as map keys. The ZIP
    /// central directory may legitimately be tens of MiB on its own.
    const MAX_TOTAL_PATH_BYTES: usize = 4 * 1024 * 1024;

    let mut file = std::fs::File::open(path).map_err(|error| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid ZIP Archive")
            .with_detail(format!("Could not read ZIP archive: {error}"))
    })?;
    temps_core::archive_security::validate_zip_metadata(&mut file).map_err(|error| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid ZIP Archive")
            .with_detail(error.to_string())
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid ZIP Archive")
            .with_detail(format!("Could not open ZIP archive: {error}"))
    })?;
    if archive.len() > MAX_FILES {
        return Err(problemdetails::new(StatusCode::PAYLOAD_TOO_LARGE)
            .with_title("Archive Has Too Many Files")
            .with_detail(format!("Archive contains more than {MAX_FILES} entries")));
    }

    let mut manifests = BTreeMap::new();
    let mut total_manifest_bytes: u64 = 0;
    let mut manifest_count: usize = 0;
    let mut total_path_bytes: usize = 0;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|error| {
            problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid ZIP Entry")
                .with_detail(format!("Could not inspect ZIP entry {index}: {error}"))
        })?;
        if entry.is_dir() {
            continue;
        }
        let path = entry.enclosed_name().ok_or_else(|| {
            problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Unsafe ZIP Entry")
                .with_detail(format!(
                    "Archive entry '{}' escapes the project root",
                    entry.name()
                ))
        })?;
        if path.components().any(|component| {
            let std::path::Component::Normal(value) = component else {
                return false;
            };
            let name = value.to_string_lossy();
            name == ".git"
                || name == "node_modules"
                || name == ".env"
                || name.starts_with(".env.")
                || name.ends_with(".pem")
                || name.ends_with(".key")
                || name == "credentials.json"
        }) {
            return Err(problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Sensitive ZIP Entry")
                .with_detail(format!(
                    "Archive entry '{}' must be excluded from Drop uploads",
                    entry.name()
                )));
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Unsupported ZIP Entry")
                .with_detail(format!("Symbolic link '{}' is not allowed", entry.name())));
        }
        let normalized = path.to_string_lossy().replace('\\', "/");
        let basename = normalized.rsplit('/').next().unwrap_or(&normalized);
        let should_read = matches!(
            basename,
            "package.json"
                | "requirements.txt"
                | "pyproject.toml"
                | "Cargo.toml"
                | "go.mod"
                | "pom.xml"
                | "build.gradle"
        ) || basename.ends_with(".csproj");
        total_path_bytes = total_path_bytes.saturating_add(normalized.len());
        if total_path_bytes > MAX_TOTAL_PATH_BYTES {
            return Err(problemdetails::new(StatusCode::PAYLOAD_TOO_LARGE)
                .with_title("Archive Manifest Too Large")
                .with_detail(format!(
                    "Combined entry paths exceed {MAX_TOTAL_PATH_BYTES} bytes"
                )));
        }
        let mut contents = String::new();
        if should_read {
            if entry.size() > MAX_MANIFEST_BYTES {
                return Err(problemdetails::new(StatusCode::PAYLOAD_TOO_LARGE)
                    .with_title("Manifest Is Too Large")
                    .with_detail(format!("Manifest '{normalized}' exceeds 1 MiB")));
            }
            manifest_count += 1;
            if manifest_count > MAX_MANIFESTS {
                return Err(problemdetails::new(StatusCode::PAYLOAD_TOO_LARGE)
                    .with_title("Too Many Manifests")
                    .with_detail(format!(
                        "Archive contains more than {MAX_MANIFESTS} project manifests"
                    )));
            }
            // Budget against bytes *actually read*, not the declared header
            // size, so a lying local header cannot get us to over-allocate.
            let remaining = MAX_TOTAL_MANIFEST_BYTES.saturating_sub(total_manifest_bytes);
            let read = entry
                .take(remaining.min(MAX_MANIFEST_BYTES) + 1)
                .read_to_string(&mut contents)
                .map_err(|error| {
                    problemdetails::new(StatusCode::BAD_REQUEST)
                        .with_title("Invalid Manifest")
                        .with_detail(format!("Could not read '{normalized}': {error}"))
                })? as u64;
            total_manifest_bytes = total_manifest_bytes.saturating_add(read);
            if total_manifest_bytes > MAX_TOTAL_MANIFEST_BYTES {
                return Err(problemdetails::new(StatusCode::PAYLOAD_TOO_LARGE)
                    .with_title("Archive Manifests Too Large")
                    .with_detail(format!(
                        "Combined project manifests exceed {} MiB",
                        MAX_TOTAL_MANIFEST_BYTES / (1024 * 1024)
                    )));
            }
        }
        manifests.insert(normalized, contents);
    }
    Ok(manifests)
}

/// Create a new project
#[utoipa::path(
    post,
    path = "/projects",
    tag = "Projects",
    request_body = CreateProjectRequest,
    responses(
        (status = 200, description = "Project created successfully", body = ProjectResponse),
        (status = 400, description = "Invalid input"),
        (status = 409, description = "Expected project slug is already in use"),
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
    let storage_service_claim_ids = if !project.storage_service_ids.is_empty() {
        permission_guard!(auth, ExternalServicesWrite);
        require_storage_services_access(state.as_ref(), &auth, &project.storage_service_ids).await?
    } else {
        Vec::new()
    };

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
        expected_slug: project.expected_slug,
        repo_name: project.repo_name,
        repo_owner: project.repo_owner,
        directory: project.directory,
        main_branch: project.main_branch,
        preset: project.preset,
        preset_config: project.preset_config,
        environment_variables: project.environment_variables,
        automatic_deploy: project.automatic_deploy.unwrap_or(false),
        storage_service_ids: project.storage_service_ids,
        storage_service_claim_ids,
        storage_service_claim_user_id: Some(auth.user_id()),
        is_public_repo: project.is_public_repo,
        git_url: project.git_url,
        git_provider_connection_id: project.git_provider_connection_id,
        exposed_port: project.exposed_port,
        cpu_request: None,
        cpu_limit: None,
        memory_request: None,
        memory_limit: None,
        source_type: project.source_type,
        template_slug: None,
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

/// Project ids to exclude from a listing for this caller.
///
/// Returns empty for instance administrators (never restricted by team
/// membership), for deployment tokens (already confined to their own
/// project by `project_scope_guard!`), and when no `ProjectAccessChecker`
/// is registered.
///
/// Fails the request on an infrastructure error rather than falling back
/// to an unfiltered list — a checker that can't answer must not silently
/// widen what a user sees.
pub(super) async fn resolve_hidden_projects(
    state: &Arc<AppState>,
    auth: &temps_auth::context::AuthContext,
) -> Result<Vec<i32>, Problem> {
    if auth.is_deployment_token()
        || auth.is_admin()
        || auth.has_role(&temps_auth::permissions::Role::PlatformAdmin)
    {
        return Ok(Vec::new());
    }
    let Some(checker) = state.project_access_checker.as_ref() else {
        return Ok(Vec::new());
    };
    // Fail closed, matching `project_permission_guard!`. Unreachable today
    // (deployment tokens are the only user-less auth source and are handled
    // above), but "hide nothing" on an unresolvable identity means "show
    // every project", so a future auth source must not land here silently.
    let Some(user_id) = auth.user_id_opt() else {
        tracing::error!("project list filtering: authenticated caller has no user id");
        return Err(
            temps_core::error_builder::ErrorBuilder::new(StatusCode::FORBIDDEN)
                .type_("https://temps.sh/probs/project-access-denied")
                .title("Project Access Denied")
                .detail("Could not resolve caller identity")
                .build(),
        );
    };
    match checker.hidden_project_ids(user_id).await {
        Ok(hidden) => Ok(hidden.unwrap_or_default()),
        Err(e) => {
            tracing::error!(
                user_id,
                error = %e,
                "ProjectAccessChecker infrastructure failure while filtering the project list"
            );
            Err(
                temps_core::error_builder::ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .type_("https://temps.sh/probs/project-access-check-failed")
                    .title("Project Access Check Failed")
                    .detail("Could not verify project access; please try again")
                    .build(),
            )
        }
    }
}

/// Get a list of all projects
#[utoipa::path(
    get,
    path = "/projects",
    tag = "Projects",
    params(
        ("page" = Option<i64>, Query, description = "Page number (1-based)"),
        ("per_page" = Option<i64>, Query, description = "Number of items per page (1-100)"),
        ("search" = Option<String>, Query, description = "Case-insensitive project name or slug filter")
    ),
    responses(
        (status = 200, description = "List of projects", body = PaginatedProjectList),
        (status = 400, description = "Invalid pagination parameters"),
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
    let search = params
        .search
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    // Per-resource guards keep a user out of a project they click on; this
    // keeps its name out of the list in the first place. Instance
    // administrators are exempt, matching `project_access_guard!`.
    let hidden = resolve_hidden_projects(&state, &auth).await?;

    let (projects, total) = state
        .project_service
        .get_projects_paginated_excluding_search(page, per_page, &hidden, search)
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
        expected_slug: None,
        repo_name: project.repo_name.clone(),
        repo_owner: project.repo_owner.clone(),
        directory: project.directory.clone(),
        main_branch: project.main_branch.clone(),
        preset: project.preset.clone(),
        preset_config: project.preset_config.clone(),
        environment_variables: project.environment_variables.clone(),
        automatic_deploy: project.automatic_deploy.unwrap_or(false),
        storage_service_ids: project.storage_service_ids.clone(),
        storage_service_claim_ids: Vec::new(),
        storage_service_claim_user_id: None,
        is_public_repo: None,               // Keep existing setting
        git_url: None,                      // Keep existing setting
        git_provider_connection_id: None,   // Keep existing setting
        exposed_port: project.exposed_port, // Keep existing or update if provided
        cpu_request: None,
        cpu_limit: None,
        memory_request: None,
        memory_limit: None,
        source_type: project.source_type, // Preserve source type
        template_slug: None,              // Template provenance is immutable
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
        compose_configuration_updated: None,
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
            compose_configuration_updated: None,
        },
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit_event).await {
        error!("Failed to create audit log: {:?}", e);
    }

    Ok(Json(ProjectResponse::map_from_project(updated)).into_response())
}

/// Opt a project in or out of accepting deployments from a source other than
/// its configured `source_type`.
///
/// This leaves `source_type` untouched — a Git project keeps its repository,
/// branch, webhook-driven auto-deploy and rollback rebuild-from-source — and
/// only changes whether the project will additionally accept an uploaded
/// source archive (`drop`). Docker images and static bundles are accepted by
/// every project regardless of this flag.
#[utoipa::path(
    patch,
    path = "/projects/{id}/alternate-sources",
    tag = "Projects",
    params(("id" = i32, Path, description = "Project ID")),
    request_body = SetAlternateSourcesRequest,
    responses(
        (status = 200, description = "Alternate-source policy updated", body = ProjectResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn set_alternate_sources(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(req): Json<SetAlternateSourcesRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, id);
    project_access_guard!(auth, id, state.project_access_checker);

    let updated = state
        .project_service
        .set_allow_alternate_sources(id, req.allow_alternate_sources)
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
            compose_configuration_updated: None,
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

    state.project_service.begin_project_deletion(id).await?;

    state
        .deployment_canceller
        .cancel_all_project_deployments(id)
        .await
        .map_err(|error| {
            error!(project_id = id, %error, "Failed to cancel project deployments");
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Project Deployment Cancellation Failed")
                .with_detail(format!(
                    "Failed to cancel active deployments for project {id}: {error}"
                ))
        })?;

    state
        .deployment_container_cleaner
        .cleanup_project_containers(id)
        .await
        .map_err(|error| {
            error!(project_id = id, %error, "Failed to clean up project containers");
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Project Container Cleanup Failed")
                .with_detail(error.to_string())
        })?;

    state
        .project_archive_cleaner
        .cleanup_project_archives(id)
        .await
        .map_err(|error| {
            error!(project_id = id, %error, "Failed to clean up project archives");
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Project Archive Cleanup Failed")
                .with_detail(format!(
                    "Failed to remove uploaded archives for project {id}: {error}"
                ))
        })?;

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

    // Capture the pre-update state only when a field that audits its previous
    // value is actually changing, so an unrelated settings save (attack mode,
    // preview envs, ...) doesn't pay for an extra read. The rename's previous
    // value deliberately does *not* come from here — the service reports it
    // from under the row lock, where it cannot race a concurrent rename.
    let previous_image_retention_hours = if settings.image_retention_hours.is_some() {
        match state.project_service.get_project(project_id).await {
            Ok(project) => project.image_retention_hours,
            Err(e) => {
                // A failed read and a genuinely-unset prior value both end up
                // `None` in the audit record below — that's an accepted gap
                // in what the double-Option type can express, but a *silent*
                // one would let a transient DB error read back as "there was
                // no prior retention window" during an incident review. Log
                // it so the ambiguity is at least visible operationally.
                warn!(
                    error = %e,
                    project_id,
                    "Could not read prior image_retention_hours before update; \
                     audit log will record it as unset rather than unknown"
                );
                None
            }
        }
    } else {
        None
    };

    let update = state
        .project_service
        .update_project_settings(project_id, settings.clone().into())
        .await
        .map_err(Problem::from)?;

    // Create audit event
    let audit_context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent,
    };

    // The service reports the rename it actually performed, under the row lock
    // that carried the write. Deriving this here from a pre-read instead would
    // race a concurrent rename and could pair one request's stale "before" with
    // another's persisted "after" — a transition that never happened. `name`
    // and `previous_name` are therefore set together or not at all.
    let updated_settings = ProjectSettingsUpdatedFields {
        cpu_request: None,
        cpu_limit: None,
        memory_request: None,
        memory_limit: None,
        performance_metrics_enabled: None,
        name: update.rename.as_ref().map(|rename| rename.to.clone()),
        previous_name: update.rename.map(|rename| rename.from),
        slug: settings.slug,
        compose_configuration_updated: settings.preset_config.as_ref().map(|_| true),
        image_retention_hours: settings.image_retention_hours,
        previous_image_retention_hours,
    };

    let audit_event = ProjectSettingsUpdatedAudit {
        context: audit_context,
        project_id: update.project.id,
        project_name: update.project.name.clone(),
        project_slug: update.project.slug.clone(),
        updated_settings,
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit_event).await {
        error!("Failed to create audit log: {:?}", e);
        // Continue with the operation even if audit logging fails
    }

    Ok(Json(ProjectResponse::map_from_project(update.project)))
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
    require_git_settings_permissions(&auth)?;
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
        compose_configuration_updated: settings.preset_config.as_ref().map(|_| true),
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

/// Updating connected-repository settings makes Temps use installation-wide
/// provider credentials for branch validation and webhook lifecycle calls.
/// Project write access alone must not grant that Git capability.
fn require_git_settings_permissions(auth: &AuthContext) -> Result<(), Problem> {
    permission_guard!(auth, GitConnectionsRead);
    permission_guard!(auth, GitRepositoriesRead);
    Ok(())
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
            compose_configuration_updated: None,
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
        .update_project_deployment_config(
            project_id,
            config.clone(),
            // An operator who can edit the instance-wide ceilings is not
            // meaningfully constrained by them.
            temps_core::CeilingEnforcement::from_has_settings_write(
                auth.has_permission(&temps_auth::Permission::SettingsWrite),
            ),
        )
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
    if config.cross_architecture_builds.is_some() {
        updated_fields.insert(
            "cross_architecture_builds".to_string(),
            "updated".to_string(),
        );
    }
    if config.request_timeout_seconds.is_some() {
        updated_fields.insert("request_timeout_seconds".to_string(), "updated".to_string());
    }
    if config.sse_idle_timeout_seconds.is_some() {
        updated_fields.insert(
            "sse_idle_timeout_seconds".to_string(),
            "updated".to_string(),
        );
    }
    if config.websocket_idle_timeout_seconds.is_some() {
        updated_fields.insert(
            "websocket_idle_timeout_seconds".to_string(),
            "updated".to_string(),
        );
    }
    if config.max_concurrent_connections.is_some() {
        updated_fields.insert(
            "max_concurrent_connections".to_string(),
            "updated".to_string(),
        );
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

/// Return the immutable service-template release applied to a project together
/// with catalog drift, missing requirements, and an available upgrade preview.
#[utoipa::path(
    get,
    path = "/projects/{project_id}/service-template",
    tag = "Projects",
    responses(
        (status = 200, description = "Applied service release and upgrade preview", body = ServiceTemplateInstanceResponse),
        (status = 400, description = "Project is not a service"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Project not found")
    ),
    params(("project_id" = i32, Path, description = "Project ID")),
    security(("bearer_auth" = []))
)]
pub async fn get_project_service_template(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let applied = state
        .project_service
        .get_applied_service_template(project_id)
        .await
        .map_err(Problem::from)?;
    let configured_names = production_environment_variable_names(
        &state
            .project_service
            .get_environment_variables(project_id)
            .await
            .map_err(Problem::from)?,
    );
    let linked_service_types = state
        .project_service
        .get_linked_service_types(project_id)
        .await
        .map_err(Problem::from)?;
    let (latest, catalog_error) = match state
        .template_service
        .get_service_template_instance(&applied.slug)
        .await
    {
        Ok(latest) => (Some(latest), None),
        Err(temps_core::templates::TemplateConfigError::NotFound(_)) => (
            None,
            Some(
                "This service family is no longer present in the active template catalog."
                    .to_string(),
            ),
        ),
        Err(error) => {
            warn!(project_id, template_slug = %applied.slug, %error, "Could not read the active service template catalog");
            (
                None,
                Some(
                    "The active service template catalog could not be read. Try again or ask an administrator to check the catalog configuration."
                        .to_string(),
                ),
            )
        }
    };
    let catalog_drift = latest.as_ref().is_some_and(|latest| {
        latest.version == applied.version && latest.template != applied.template
    });
    let upgrade_available = latest
        .as_ref()
        .is_some_and(|latest| latest.is_newer_than(&applied).unwrap_or(false));
    let changes = latest
        .as_ref()
        .map(|latest| service_template_changes(&applied.template, &latest.template))
        .unwrap_or_default();
    let required_configuration = latest
        .as_ref()
        .map(|latest| missing_required_template_configuration(&latest.template, &configured_names))
        .unwrap_or_default();
    let missing_services = latest
        .as_ref()
        .map(|latest| {
            latest
                .template
                .services
                .iter()
                .filter(|service| {
                    !linked_service_types.iter().any(|linked| {
                        temps_core::templates::managed_service_types_compatible(service, linked)
                    })
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();

    Ok(Json(ServiceTemplateInstanceResponse {
        project_id,
        applied,
        latest,
        catalog_error,
        upgrade_available,
        catalog_drift,
        changes,
        required_configuration,
        missing_services,
    }))
}

#[utoipa::path(
    post,
    path = "/projects/{project_id}/service-template/upgrade",
    tag = "Projects",
    request_body = UpgradeServiceTemplateRequest,
    responses(
        (status = 200, description = "Service template upgraded", body = ProjectResponse),
        (status = 400, description = "Invalid or stale upgrade"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Project or template not found")
    ),
    params(("project_id" = i32, Path, description = "Project ID")),
    security(("bearer_auth" = []))
)]
pub async fn upgrade_project_service_template(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<UpgradeServiceTemplateRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    permission_guard!(auth, EnvironmentsCreate);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let project = state
        .project_service
        .get_project(project_id)
        .await
        .map_err(Problem::from)?;
    let applied = state
        .project_service
        .get_applied_service_template(project_id)
        .await
        .map_err(Problem::from)?;
    let target = state
        .template_service
        .get_service_template_instance(&applied.slug)
        .await
        .map_err(|error| match error {
            temps_core::templates::TemplateConfigError::NotFound(_) => {
                temps_core::error_builder::not_found()
                    .title("Service Template Not Found")
                    .detail(error.to_string())
                    .build()
            }
            temps_core::templates::TemplateConfigError::IoError(_)
            | temps_core::templates::TemplateConfigError::ParseError(_)
            | temps_core::templates::TemplateConfigError::ValidationErrors(_) => {
                temps_core::error_builder::internal_server_error()
                    .title("Service Template Catalog Invalid")
                    .detail(error.to_string())
                    .build()
            }
        })?;
    let previous_template_version = applied.version.clone();
    let previous_template_image = applied.template.image.clone();
    let target_template_version = target.version.clone();
    let target_template_image = target.template.image.clone();
    if target.version != request.target_version {
        return Err(temps_core::error_builder::bad_request()
            .title("Template Upgrade Changed")
            .detail(format!(
                "Requested {}@{}, but the current catalog release is {}@{}. Refresh the preview before upgrading.",
                applied.slug, request.target_version, target.slug, target.version
            ))
            .build());
    }
    if target.version == applied.version && target.template != applied.template {
        return Err(temps_core::error_builder::bad_request()
            .title("Template Version Was Not Updated")
            .detail(format!(
                "Template '{}' changed without incrementing version {}. Publish a new template version before applying it.",
                target.slug, target.version
            ))
            .build());
    }

    let linked_service_types = state
        .project_service
        .get_linked_service_types(project_id)
        .await
        .map_err(Problem::from)?;
    let missing_services = target
        .template
        .services
        .iter()
        .filter(|service| {
            !linked_service_types.iter().any(|linked| {
                temps_core::templates::managed_service_types_compatible(service, linked)
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    if !missing_services.is_empty() {
        return Err(temps_core::error_builder::bad_request()
            .title("Managed Service Required")
            .detail(format!(
                "Link a {} service to this project before applying {}@{}",
                missing_services.join(", "),
                target.slug,
                target.version
            ))
            .build());
    }
    let configured_names = production_environment_variable_names(
        &state
            .project_service
            .get_environment_variables(project_id)
            .await
            .map_err(Problem::from)?,
    );
    let generated_app_url = if upgrade_needs_generated_app_url(
        &target.template,
        &request.environment_variables,
        &configured_names,
    ) {
        Some(canonical_template_app_url(state.as_ref(), &project.slug).await?)
    } else {
        None
    };
    let new_environment_variables = canonicalize_template_upgrade_environment_variables(
        &target.template,
        &request.environment_variables,
        &configured_names,
        generated_app_url.as_deref(),
    )
    .map_err(|error| {
        if matches!(&error, TemplateEnvironmentError::SecureGeneration { .. }) {
            error!(project_id, %error, "Secure template upgrade generation failed");
            temps_core::error_builder::internal_server_error()
                .title("Template Secret Generation Failed")
                .detail("Could not securely generate the required template credentials")
                .build()
        } else {
            temps_core::error_builder::bad_request()
                .title("Invalid Template Upgrade Configuration")
                .detail(error.to_string())
                .build()
        }
    })?;

    let updated_project = state
        .project_service
        .upgrade_service_template(
            project_id,
            target,
            new_environment_variables,
            temps_core::CeilingEnforcement::from_has_settings_write(
                auth.has_permission(&temps_auth::Permission::SettingsWrite),
            ),
        )
        .await
        .map_err(Problem::from)?;

    let audit_event = super::audit::DeploymentConfigUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.to_string()),
            user_agent: metadata.user_agent,
        },
        project_id: updated_project.id,
        project_name: updated_project.name.clone(),
        project_slug: updated_project.slug.clone(),
        updated_fields: [
            (
                "service_template_version".to_string(),
                format!("{previous_template_version} -> {target_template_version}"),
            ),
            (
                "service_template_image".to_string(),
                format!(
                    "{} -> {}",
                    previous_template_image.as_deref().unwrap_or("not set"),
                    target_template_image.as_deref().unwrap_or("not set")
                ),
            ),
        ]
        .into_iter()
        .collect(),
    };
    if let Err(error) = state.audit_service.create_audit_log(&audit_event).await {
        error!(project_id, %error, "Failed to create service upgrade audit log");
    }

    Ok(Json(ProjectResponse::map_from_project(updated_project)))
}

#[utoipa::path(
    patch,
    path = "/projects/{project_id}/service-runtime",
    tag = "Projects",
    request_body = UpdateServiceTemplateRuntimeRequest,
    responses(
        (status = 200, description = "Service runtime updated successfully", body = ProjectResponse),
        (status = 400, description = "Invalid service runtime"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    params(("project_id" = i32, Path, description = "Project ID")),
    security(("bearer_auth" = []))
)]
pub async fn update_service_template_runtime(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(runtime): Json<UpdateServiceTemplateRuntimeRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let current_project = state
        .project_service
        .get_project(project_id)
        .await
        .map_err(Problem::from)?;
    let previous_runtime = current_project
        .preset_config
        .as_ref()
        .and_then(|value| {
            serde_json::from_value::<temps_entities::preset::PresetConfig>(value.clone()).ok()
        })
        .and_then(|config| match config {
            temps_entities::preset::PresetConfig::Dockerfile(config) => config.image_runtime,
            _ => None,
        });
    let previous_image = previous_runtime
        .as_ref()
        .map(|runtime| runtime.image_ref.as_str())
        .unwrap_or("not set")
        .to_string();
    let previous_command_count = previous_runtime
        .as_ref()
        .and_then(|runtime| runtime.command.as_ref())
        .map_or(0, Vec::len);
    let requested_image = runtime.image_ref.clone();
    let requested_command_count = runtime.command.len();

    let updated_project = state
        .project_service
        .update_service_template_runtime(
            project_id,
            runtime,
            temps_core::CeilingEnforcement::from_has_settings_write(
                auth.has_permission(&temps_auth::Permission::SettingsWrite),
            ),
        )
        .await
        .map_err(Problem::from)?;

    let audit_context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent,
    };
    let updated_fields = [
        (
            "image_ref".to_string(),
            format!("{previous_image} -> {requested_image}"),
        ),
        (
            "command_argument_count".to_string(),
            format!("{previous_command_count} -> {requested_command_count}"),
        ),
        ("health_check_path".to_string(), "updated".to_string()),
        ("cpu_request".to_string(), "updated".to_string()),
        ("cpu_limit".to_string(), "updated".to_string()),
        ("memory_request".to_string(), "updated".to_string()),
        ("memory_limit".to_string(), "updated".to_string()),
        ("exposed_port".to_string(), "updated".to_string()),
    ]
    .into_iter()
    .collect();
    let audit_event = super::audit::DeploymentConfigUpdatedAudit {
        context: audit_context,
        project_id: updated_project.id,
        project_name: updated_project.name.clone(),
        project_slug: updated_project.slug.clone(),
        updated_fields,
    };
    if let Err(error) = state.audit_service.create_audit_log(&audit_event).await {
        error!(?error, "Failed to create service runtime audit log");
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

    // Same exclusion as the list — see `resolve_hidden_projects`.
    let hidden = resolve_hidden_projects(&state, &auth).await?;

    let statistics = state
        .project_service
        .get_project_statistics_excluding(&hidden)
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

            // Use the preset's own icon, NOT one derived from the slug.
            // Deriving it invented filenames that were never shipped —
            // `docker-compose` resolved to `/presets/docker-compose.svg`
            // (missing) instead of the `docker.svg` the preset declares, and
            // every `nixpacks-<lang>` slug pointed at a nonexistent file
            // rather than the language icon it maps to. Those 404s fell back
            // to the generic gear in the UI.
            let icon_url = preset.icon_url();

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
        ("featured" = Option<bool>, Query, description = "Only return featured templates"),
        ("kind" = Option<temps_core::templates::TemplateKind>, Query, description = "Filter by gallery: starter or service")
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
    let mut templates = state.template_service.list_templates().await;
    if let Some(kind) = query.kind {
        templates.retain(|template| template.kind == kind);
    }
    if query.featured == Some(true) {
        templates.retain(|template| template.is_featured);
    }
    if let Some(tag) = query.tag {
        templates.retain(|template| {
            template
                .tags
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(&tag))
        });
    }

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

fn project_created_from_template_telemetry_event(
    template_slug: &str,
    deploy_mode: &'static str,
    service_count: usize,
) -> temps_core::telemetry::TelemetryEvent {
    let safe_slug = temps_core::templates::telemetry_safe_template_slug(template_slug);
    temps_core::telemetry::TelemetryEvent::new(
        temps_core::telemetry::TelemetryEventKind::ProjectCreatedFromTemplate,
    )
    .with(
        "template_source",
        if safe_slug.is_some() {
            "bundled"
        } else {
            "custom"
        },
    )
    .with_opt("template_slug", safe_slug.map(str::to_string))
    .with("deploy_mode", deploy_mode)
    .with("service_count", service_count as i64)
}

fn image_deployment_dispatch_feedback(queued: bool) -> (Option<bool>, Option<String>) {
    if queued {
        (Some(true), None)
    } else {
        (
            Some(false),
            Some(
                "The project was created, but its initial deployment could not be queued. Open the project and select Deploy to retry."
                    .to_string(),
            ),
        )
    }
}

#[derive(Debug, PartialEq, Eq)]
enum TemplateServiceSelectionError {
    Missing(Vec<String>),
    Ambiguous(Vec<String>),
}

#[derive(Debug, PartialEq, Eq)]
struct EffectiveImageTemplateRuntime {
    image_ref: String,
    command: Option<Vec<String>>,
    cpu_request: Option<i32>,
    cpu_limit: Option<i32>,
    memory_request: Option<i32>,
    memory_limit: Option<i32>,
    exposed_port: Option<i32>,
    health_check_path: Option<String>,
}

fn image_template_preset_config(
    template: &temps_core::templates::ProjectTemplate,
    runtime: &EffectiveImageTemplateRuntime,
) -> Result<serde_json::Value, TemplateRuntimeOverrideError> {
    let mut config = match template.preset_config.as_ref() {
        Some(value) => temps_entities::preset::PresetConfig::parse_for_preset(
            &temps_entities::preset::Preset::Dockerfile,
            value,
        )
        .map_err(|reason| TemplateRuntimeOverrideError::InvalidImage { reason })?,
        None => temps_entities::preset::PresetConfig::default_for_preset(
            temps_entities::preset::Preset::Dockerfile,
        ),
    };
    let temps_entities::preset::PresetConfig::Dockerfile(config) = &mut config else {
        return Err(TemplateRuntimeOverrideError::InvalidImage {
            reason: "image templates must use the dockerfile runtime preset".to_string(),
        });
    };
    config.image_runtime = Some(temps_entities::preset::ImageRuntimeConfig {
        image_ref: runtime.image_ref.clone(),
        command: runtime.command.clone(),
        health_check_path: runtime.health_check_path.clone(),
    });
    serde_json::to_value(config).map_err(|error| TemplateRuntimeOverrideError::InvalidImage {
        reason: format!("could not store the selected runtime: {error}"),
    })
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
enum TemplateRuntimeOverrideError {
    #[error("Runtime overrides are available only for templates that deploy a prebuilt image")]
    NotAnImageTemplate,
    #[error("Invalid image reference: {reason}")]
    InvalidImage { reason: String },
    #[error("Invalid container command: {reason}")]
    InvalidCommand { reason: String },
    #[error("Invalid health-check path: {reason}")]
    InvalidHealthCheckPath { reason: String },
}

fn has_template_runtime_overrides(
    request: &super::templates::CreateProjectFromTemplateRequest,
) -> bool {
    request.image.is_some()
        || request.command.is_some()
        || request.cpu_request.is_some()
        || request.cpu_limit.is_some()
        || request.memory_request.is_some()
        || request.memory_limit.is_some()
        || request.exposed_port.is_some()
        || request.health_check_path.is_some()
}

fn is_pinned_sha256_image_reference(image: &str) -> bool {
    image.rsplit_once('@').is_some_and(|(name, digest)| {
        !name.trim().is_empty()
            && digest.strip_prefix("sha256:").is_some_and(|hash| {
                hash.len() == 64 && hash.chars().all(|character| character.is_ascii_hexdigit())
            })
    })
}

fn resolve_image_template_runtime(
    template: &temps_core::templates::ProjectTemplate,
    request: &super::templates::CreateProjectFromTemplateRequest,
) -> Result<Option<EffectiveImageTemplateRuntime>, TemplateRuntimeOverrideError> {
    let Some(template_image) = template.image.as_deref().filter(|image| !image.is_empty()) else {
        if has_template_runtime_overrides(request) {
            return Err(TemplateRuntimeOverrideError::NotAnImageTemplate);
        }
        return Ok(None);
    };

    let image_ref = request.image.as_deref().unwrap_or(template_image).trim();
    if image_ref.is_empty() {
        return Err(TemplateRuntimeOverrideError::InvalidImage {
            reason: "the image reference cannot be empty".to_string(),
        });
    }
    if image_ref.len() > 512 {
        return Err(TemplateRuntimeOverrideError::InvalidImage {
            reason: "the image reference cannot exceed 512 bytes".to_string(),
        });
    }
    if image_ref
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(TemplateRuntimeOverrideError::InvalidImage {
            reason: "the image reference cannot contain whitespace or control characters"
                .to_string(),
        });
    }
    if !is_pinned_sha256_image_reference(image_ref) {
        return Err(TemplateRuntimeOverrideError::InvalidImage {
            reason: "use an immutable image reference ending in @sha256:<64 hex characters>"
                .to_string(),
        });
    }

    let command = match request.command.as_ref() {
        Some(parts) if parts.is_empty() => None,
        Some(parts) => {
            if parts.len() > 64 {
                return Err(TemplateRuntimeOverrideError::InvalidCommand {
                    reason: "at most 64 arguments are supported".to_string(),
                });
            }
            let normalized = parts
                .iter()
                .map(|part| part.trim().to_string())
                .collect::<Vec<_>>();
            if normalized.iter().any(|part| {
                part.is_empty() || part.len() > 1_024 || part.chars().any(char::is_control)
            }) {
                return Err(TemplateRuntimeOverrideError::InvalidCommand {
                    reason: "arguments must be non-empty, at most 1024 bytes, and contain no control characters"
                        .to_string(),
                });
            }
            Some(normalized)
        }
        None => template.command.clone(),
    };

    let health_check_path = request
        .health_check_path
        .as_deref()
        .or(template.health_check_path.as_deref())
        .map(str::trim)
        .map(str::to_string);
    if let Some(path) = health_check_path.as_deref() {
        if path.len() > 2_048
            || !path.starts_with('/')
            || path.contains('@')
            || path.contains("://")
            || path.chars().any(char::is_control)
        {
            return Err(TemplateRuntimeOverrideError::InvalidHealthCheckPath {
                reason: "use a safe relative HTTP path starting with '/'".to_string(),
            });
        }
    }

    Ok(Some(EffectiveImageTemplateRuntime {
        image_ref: image_ref.to_string(),
        command,
        cpu_request: request.cpu_request.or_else(|| {
            template
                .resources
                .as_ref()
                .and_then(|resources| resources.cpu_request)
        }),
        cpu_limit: request.cpu_limit.or_else(|| {
            template
                .resources
                .as_ref()
                .and_then(|resources| resources.cpu_limit)
        }),
        memory_request: request.memory_request.or_else(|| {
            template
                .resources
                .as_ref()
                .and_then(|resources| resources.memory_request)
        }),
        memory_limit: request.memory_limit.or_else(|| {
            template
                .resources
                .as_ref()
                .and_then(|resources| resources.memory_limit)
        }),
        exposed_port: request.exposed_port.or(template.exposed_port),
        health_check_path,
    }))
}

fn validate_template_service_selection(
    required_services: &[String],
    selected_service_types: &[String],
) -> Result<(), TemplateServiceSelectionError> {
    let missing = required_services
        .iter()
        .filter(|required| {
            !selected_service_types.iter().any(|selected| {
                temps_core::templates::managed_service_types_compatible(required, selected)
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(TemplateServiceSelectionError::Missing(missing));
    }

    let ambiguous = required_services
        .iter()
        .filter(|required| {
            selected_service_types
                .iter()
                .filter(|selected| {
                    temps_core::templates::managed_service_types_compatible(required, selected)
                })
                .count()
                > 1
        })
        .cloned()
        .collect::<Vec<_>>();
    if !ambiguous.is_empty() {
        return Err(TemplateServiceSelectionError::Ambiguous(ambiguous));
    }

    Ok(())
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
enum TemplateEnvironmentError {
    #[error("Environment variable '{name}' is defined more than once")]
    Duplicate { name: String },
    #[error("Required environment variable '{name}' has no value")]
    MissingRequired { name: String },
    #[error("Environment variable '{name}' uses unsupported generator '{generator}'")]
    UnsupportedGenerator { name: String, generator: String },
    #[error("Could not securely generate environment variable '{name}': {reason}")]
    SecureGeneration { name: String, reason: String },
    #[error(
        "Environment variable '{name}' is already configured; edit it in project settings instead"
    )]
    AlreadyConfigured { name: String },
    #[error("Environment variable '{name}' is not declared by the target template")]
    Unknown { name: String },
}

fn require_template_creation_permissions(auth: &AuthContext) -> Result<(), Problem> {
    permission_guard!(auth, ProjectsCreate);
    permission_guard!(auth, DeploymentsCreate);
    Ok(())
}

fn template_environment_variable_is_secret(
    variable: &temps_core::templates::EnvVarTemplate,
) -> bool {
    variable.is_secret()
}

fn secure_template_value(
    variable_name: &str,
    generator: &str,
) -> Result<String, TemplateEnvironmentError> {
    let mut bytes = [0_u8; 32];
    rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut bytes).map_err(|error| {
        TemplateEnvironmentError::SecureGeneration {
            name: variable_name.to_string(),
            reason: error.to_string(),
        }
    })?;

    match generator {
        "random_secret" => Ok(URL_SAFE_NO_PAD.encode(bytes)),
        "random_hex_32" => Ok(hex::encode(bytes)),
        _ => Err(TemplateEnvironmentError::UnsupportedGenerator {
            name: variable_name.to_string(),
            generator: generator.to_string(),
        }),
    }
}

fn canonicalize_template_environment_variables(
    template: &temps_core::templates::ProjectTemplate,
    requested: &[super::templates::EnvVarInput],
    generated_app_url: Option<&str>,
) -> Result<Vec<CreateProjectEnvVar>, TemplateEnvironmentError> {
    let mut requested_by_name = BTreeMap::new();
    for variable in requested {
        if requested_by_name
            .insert(
                variable.name.clone(),
                (variable.value.clone(), variable.is_secret),
            )
            .is_some()
        {
            return Err(TemplateEnvironmentError::Duplicate {
                name: variable.name.clone(),
            });
        }
    }

    let mut resolved = Vec::with_capacity(template.env_vars.len() + requested_by_name.len());
    for variable in &template.env_vars {
        let requested_value = requested_by_name.remove(&variable.name);
        let explicitly_supplied = requested_value.is_some();
        let requested_secret = requested_value
            .as_ref()
            .is_some_and(|(_, is_secret)| *is_secret);
        let mut value = requested_value.map(|(value, _)| value);

        if value.as_deref().is_none_or(str::is_empty) {
            value = variable.default.clone();
        }
        if value.as_deref().is_none_or(str::is_empty) {
            value = match variable.default_generator.as_deref() {
                Some("app_url") => generated_app_url.map(str::to_string),
                Some(generator @ ("random_secret" | "random_hex_32")) => {
                    Some(secure_template_value(&variable.name, generator)?)
                }
                Some(generator) => {
                    return Err(TemplateEnvironmentError::UnsupportedGenerator {
                        name: variable.name.clone(),
                        generator: generator.to_string(),
                    });
                }
                None => None,
            };
        }

        if variable.required && value.as_deref().is_none_or(str::is_empty) {
            return Err(TemplateEnvironmentError::MissingRequired {
                name: variable.name.clone(),
            });
        }
        if let Some(value) = value {
            if !value.is_empty() || explicitly_supplied {
                resolved.push(CreateProjectEnvVar {
                    key: variable.name.clone(),
                    value,
                    is_secret: requested_secret
                        || template_environment_variable_is_secret(variable),
                });
            }
        }
    }

    resolved.extend(
        requested_by_name
            .into_iter()
            .map(|(key, (value, is_secret))| CreateProjectEnvVar {
                key,
                value,
                is_secret,
            }),
    );
    Ok(resolved)
}

fn canonicalize_template_upgrade_environment_variables(
    template: &temps_core::templates::ProjectTemplate,
    requested: &[super::templates::EnvVarInput],
    configured_names: &BTreeSet<String>,
    generated_app_url: Option<&str>,
) -> Result<Vec<CreateProjectEnvVar>, TemplateEnvironmentError> {
    let mut requested_by_name = BTreeMap::new();
    for variable in requested {
        if configured_names.contains(&variable.name) {
            return Err(TemplateEnvironmentError::AlreadyConfigured {
                name: variable.name.clone(),
            });
        }
        if requested_by_name
            .insert(
                variable.name.clone(),
                (variable.value.clone(), variable.is_secret),
            )
            .is_some()
        {
            return Err(TemplateEnvironmentError::Duplicate {
                name: variable.name.clone(),
            });
        }
    }

    let mut resolved = Vec::new();
    for variable in &template.env_vars {
        if configured_names.contains(&variable.name) {
            continue;
        }
        let requested_value = requested_by_name.remove(&variable.name);
        let explicitly_supplied = requested_value.is_some();
        let requested_secret = requested_value
            .as_ref()
            .is_some_and(|(_, is_secret)| *is_secret);
        let mut value = requested_value.map(|(value, _)| value);

        if value.as_deref().is_none_or(str::is_empty) {
            value = variable.default.clone();
        }
        if value.as_deref().is_none_or(str::is_empty) {
            value = match variable.default_generator.as_deref() {
                Some("app_url") => generated_app_url.map(str::to_string),
                Some(generator @ ("random_secret" | "random_hex_32")) => {
                    Some(secure_template_value(&variable.name, generator)?)
                }
                Some(generator) => {
                    return Err(TemplateEnvironmentError::UnsupportedGenerator {
                        name: variable.name.clone(),
                        generator: generator.to_string(),
                    });
                }
                None => None,
            };
        }

        if variable.required && value.as_deref().is_none_or(str::is_empty) {
            return Err(TemplateEnvironmentError::MissingRequired {
                name: variable.name.clone(),
            });
        }
        if let Some(value) = value {
            if !value.is_empty() || explicitly_supplied {
                resolved.push(CreateProjectEnvVar {
                    key: variable.name.clone(),
                    value,
                    is_secret: requested_secret || variable.is_secret(),
                });
            }
        }
    }

    if let Some((name, _)) = requested_by_name.into_iter().next() {
        return Err(TemplateEnvironmentError::Unknown { name });
    }
    Ok(resolved)
}

fn missing_required_template_configuration(
    template: &temps_core::templates::ProjectTemplate,
    configured_names: &BTreeSet<String>,
) -> Vec<temps_core::templates::EnvVarTemplate> {
    template
        .env_vars
        .iter()
        .filter(|variable| {
            variable.required
                && !configured_names.contains(&variable.name)
                && variable.default.as_deref().is_none_or(str::is_empty)
                && variable.default_generator.is_none()
        })
        .cloned()
        .collect()
}

fn upgrade_needs_generated_app_url(
    template: &temps_core::templates::ProjectTemplate,
    requested: &[super::templates::EnvVarInput],
    configured_names: &BTreeSet<String>,
) -> bool {
    template.env_vars.iter().any(|variable| {
        !configured_names.contains(&variable.name)
            && variable.default_generator.as_deref() == Some("app_url")
            && requested
                .iter()
                .find(|requested| requested.name == variable.name)
                .is_none_or(|requested| requested.value.is_empty())
            && variable.default.as_deref().is_none_or(str::is_empty)
    })
}

fn template_needs_generated_app_url(
    template: &temps_core::templates::ProjectTemplate,
    requested: &[super::templates::EnvVarInput],
) -> bool {
    template.env_vars.iter().any(|variable| {
        variable.default_generator.as_deref() == Some("app_url")
            && requested
                .iter()
                .find(|requested| requested.name == variable.name)
                .is_none_or(|requested| requested.value.is_empty())
            && variable.default.as_deref().is_none_or(str::is_empty)
    })
}

async fn canonical_template_app_url(
    state: &AppState,
    project_slug: &str,
) -> Result<String, Problem> {
    let settings = state.config_service.get_settings().await.map_err(|error| {
        error!(%error, "Failed to load hostname settings for native template");
        temps_core::error_builder::internal_server_error()
            .title("Template URL Generation Failed")
            .detail("Could not load the platform hostname settings")
            .build()
    })?;
    let preview_domain = if settings.preview_domain.trim().is_empty() {
        "localho.st"
    } else {
        settings.preview_domain.trim()
    };
    let strategy = state
        .public_hostname_resolver
        .strategy_for(preview_domain)
        .await;
    let (scheme, port) = settings
        .external_url
        .as_deref()
        .and_then(|value| url::Url::parse(value).ok())
        .map(|url| (url.scheme().to_string(), url.port()))
        .unwrap_or_else(|| ("http".to_string(), Some(state.config_service.proxy_port())));
    let hostname =
        strategy.environment_hostname(preview_domain, &format!("{project_slug}-production"));
    let port_suffix = port
        .filter(|port| !((scheme == "http" && *port == 80) || (scheme == "https" && *port == 443)))
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    Ok(format!("{scheme}://{hostname}{port_suffix}"))
}

/// Create a new project from a template
///
/// Image-backed service templates are created directly from their pinned image.
/// Source-backed starter templates can either use their public repository or
/// create a repository under the selected Git provider account.
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
    require_template_creation_permissions(&auth)?;
    let storage_service_claim_ids = if !request.storage_service_ids.is_empty() {
        permission_guard!(auth, ExternalServicesWrite);
        require_storage_services_access(state.as_ref(), &auth, &request.storage_service_ids).await?
    } else {
        Vec::new()
    };

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
    let service_template_instance = if template.kind == temps_core::templates::TemplateKind::Service
    {
        Some(
            state
                .template_service
                .get_service_template_instance(&request.template_slug)
                .await
                .map_err(|error| {
                    temps_core::error_builder::bad_request()
                        .title("Invalid Service Template")
                        .detail(error.to_string())
                        .build()
                })?,
        )
    } else {
        None
    };
    // Service projects need the actual local slug for future upgrades. The
    // telemetry path still applies its fixed bundled allowlist before emitting
    // this value, so operator-defined slugs remain private.
    let template_provenance = service_template_instance
        .as_ref()
        .map(|instance| instance.slug.clone())
        .unwrap_or_else(|| temps_core::templates::template_provenance(&template).to_string());
    let image_runtime = resolve_image_template_runtime(&template, &request).map_err(|error| {
        temps_core::error_builder::bad_request()
            .title("Invalid Template Runtime Configuration")
            .detail(error.to_string())
            .build()
    })?;
    let planned_project_slug = state
        .project_service
        .plan_project_slug(&request.project_name)
        .await
        .map_err(Problem::from)?;

    // The browser normally enforces this selection, but the API must fail
    // before creating a project when a required managed dependency is absent
    // or the caller supplied a service of the wrong type.
    let mut selected_service_types = Vec::with_capacity(request.storage_service_ids.len());
    for service_id in &request.storage_service_ids {
        let service = state
            .external_service_manager
            .get_service(*service_id)
            .await
            .map_err(|error| {
                error!(
                    service_id = *service_id,
                    template = %template.slug,
                    "Failed to resolve selected managed service: {error}"
                );
                temps_core::error_builder::internal_server_error()
                    .title("Managed Service Lookup Failed")
                    .detail(format!(
                        "Could not verify managed service {service_id} for template '{}': {error}",
                        template.name
                    ))
                    .build()
            })?;
        selected_service_types.push(service.service_type.to_ascii_lowercase());
    }
    if let Err(error) =
        validate_template_service_selection(&template.services, &selected_service_types)
    {
        let (title, detail) = match error {
            TemplateServiceSelectionError::Missing(services) => (
                "Managed Service Required",
                format!(
                    "Template '{}' requires a linked {} service before it can be deployed",
                    template.name,
                    services.join(", ")
                ),
            ),
            TemplateServiceSelectionError::Ambiguous(services) => (
                "Ambiguous Managed Service Selection",
                format!(
                    "Template '{}' accepts exactly one linked {} service",
                    template.name,
                    services.join(", ")
                ),
            ),
        };
        return Err(temps_core::error_builder::bad_request()
            .title(title)
            .detail(detail)
            .build());
    }

    // 2. Resolve template defaults and generators on the server. The client is
    //    an editor, not the authority for required values or secret handling.
    let generated_app_url =
        if template_needs_generated_app_url(&template, &request.environment_variables) {
            Some(canonical_template_app_url(state.as_ref(), &planned_project_slug).await?)
        } else {
            None
        };
    let env_vars = canonicalize_template_environment_variables(
        &template,
        &request.environment_variables,
        generated_app_url.as_deref(),
    )
    .map_err(|error| {
        if matches!(&error, TemplateEnvironmentError::SecureGeneration { .. }) {
            error!(%error, "Secure template environment generation failed");
            temps_core::error_builder::internal_server_error()
                .title("Template Secret Generation Failed")
                .detail("Could not securely generate the required template credentials")
                .build()
        } else {
            temps_core::error_builder::bad_request()
                .title("Invalid Template Environment")
                .detail(error.to_string())
                .build()
        }
    })?;
    let env_vars = (!env_vars.is_empty()).then_some(env_vars);

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
        Option<EffectiveImageTemplateRuntime>,
    ) = if let Some(runtime) = image_runtime {
        info!(
            "Deploying template {} from prebuilt image {} (image mode)",
            request.template_slug, runtime.image_ref
        );
        let preset_config = image_template_preset_config(&template, &runtime).map_err(|error| {
            temps_core::error_builder::bad_request()
                .title("Invalid Template Runtime Configuration")
                .detail(error.to_string())
                .build()
        })?;
        let req = crate::services::types::CreateProjectRequest {
            name: request.project_name.clone(),
            expected_slug: Some(planned_project_slug.clone()),
            // No Git source — the image is pulled from its registry.
            repo_name: None,
            repo_owner: None,
            directory: ".".to_string(),
            main_branch: template.git.r#ref.clone(),
            preset: template.preset.clone(),
            preset_config: Some(preset_config),
            environment_variables: env_vars,
            automatic_deploy: false,
            storage_service_ids: request.storage_service_ids.clone(),
            storage_service_claim_ids: storage_service_claim_ids.clone(),
            storage_service_claim_user_id: Some(auth.user_id()),
            is_public_repo: None,
            git_url: None,
            git_provider_connection_id: None,
            exposed_port: runtime.exposed_port,
            cpu_request: runtime.cpu_request,
            cpu_limit: runtime.cpu_limit,
            memory_request: runtime.memory_request,
            memory_limit: runtime.memory_limit,
            // docker_image source skips the build pipeline entirely; the deploy
            // is triggered explicitly below via Job::DeployImageRequested.
            source_type: SourceType::DockerImage,
            template_slug: Some(template_provenance.clone()),
        };
        // Surface the template's source repo as the response URL (the image ref
        // isn't a browsable URL); the message clarifies it deployed from an image.
        (req, template.git.url.clone(), "image", Some(runtime))
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
                        auth.user_id(),
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
                    expected_slug: Some(planned_project_slug.clone()),
                    repo_name: Some(new_repo.name.clone()),
                    repo_owner: Some(new_repo.owner.clone()),
                    directory: ".".to_string(),
                    main_branch: new_repo.default_branch.clone(),
                    preset: template.preset.clone(),
                    preset_config: template.preset_config.clone(),
                    environment_variables: env_vars,
                    automatic_deploy: request.automatic_deploy,
                    storage_service_ids: request.storage_service_ids.clone(),
                    storage_service_claim_ids: storage_service_claim_ids.clone(),
                    storage_service_claim_user_id: Some(auth.user_id()),
                    is_public_repo: Some(!new_repo.private),
                    git_url: Some(new_repo.clone_url.clone()),
                    git_provider_connection_id: Some(connection_id),
                    exposed_port: None,
                    cpu_request: None,
                    cpu_limit: None,
                    memory_request: None,
                    memory_limit: None,
                    source_type: SourceType::Git,
                    template_slug: Some(template_provenance.clone()),
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
                    expected_slug: Some(planned_project_slug.clone()),
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
                    storage_service_claim_ids: storage_service_claim_ids.clone(),
                    storage_service_claim_user_id: Some(auth.user_id()),
                    is_public_repo: Some(true),
                    git_url: Some(template.git.url.clone()),
                    git_provider_connection_id: None,
                    exposed_port: None,
                    cpu_request: None,
                    cpu_limit: None,
                    memory_request: None,
                    memory_limit: None,
                    source_type: SourceType::Git,
                    template_slug: Some(template_provenance.clone()),
                };
                (req, template.git.url.clone(), "public_repo")
            }
        };
        (create_request, repository_url, deploy_mode, None)
    };

    let project = if let Some(service_template) = service_template_instance {
        state
            .project_service
            .create_service_project(create_request, service_template)
            .await
    } else {
        state.project_service.create_project(create_request).await
    }
    .map_err(Problem::from)?;

    // 4. Image mode: docker_image projects don't auto-deploy on create (no Git
    //    push), so explicitly queue the image deploy. The deployments side
    //    resolves the target environment, pulls the image, and runs it — no
    //    build. Failure to enqueue is logged but doesn't fail project creation
    //    (the user can redeploy from the UI).
    let mut deployment_queued = None;
    let mut deployment_error = None;
    if let Some(runtime) = image_to_deploy {
        let deploy_job =
            temps_core::Job::DeployImageRequested(temps_core::DeployImageRequestedJob {
                project_id: project.id,
                target_environment_id: None,
                image_ref: runtime.image_ref.clone(),
                health_check_path: runtime.health_check_path,
                command: runtime.command,
            });
        if let Err(e) = state.project_service.queue_service.send(deploy_job).await {
            error!(
                "Failed to queue image deploy for project {} (image {}): {}",
                project.id, runtime.image_ref, e
            );
            (deployment_queued, deployment_error) = image_deployment_dispatch_feedback(false);
        } else {
            info!(
                "Queued image deploy for project {} from image {}",
                project.id, runtime.image_ref
            );
            (deployment_queued, deployment_error) = image_deployment_dispatch_feedback(true);
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
    state
        .telemetry
        .report(project_created_from_template_telemetry_event(
            &template_provenance,
            deploy_mode,
            request.storage_service_ids.len(),
        ));

    // 7. Return the response with the source/repository URL.
    let deploy_note = match deploy_mode {
        "image" if deployment_queued == Some(true) => {
            "Initial deployment queued from the template's prebuilt image (no build)."
        }
        "image" => {
            "Project created from the template's prebuilt image; deployment requires a retry."
        }
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
        deployment_queued,
        deployment_error,
    };

    Ok((StatusCode::CREATED, Json(response)))
}

#[cfg(test)]
mod tests {
    use super::{
        authorize_storage_service_scopes, canonicalize_template_environment_variables,
        canonicalize_template_upgrade_environment_variables, compose_path_for_candidate,
        drop_preset_candidate_from, image_deployment_dispatch_feedback,
        image_template_preset_config, missing_required_template_configuration,
        parse_owner_repo_from_git_url, production_environment_variable_names,
        project_created_from_template_telemetry_event, require_git_settings_permissions,
        require_template_creation_permissions, resolve_image_template_runtime,
        service_template_changes, validate_template_service_selection, DropPresetCandidate,
        TemplateEnvironmentError, TemplateRuntimeOverrideError, TemplateServiceSelectionError,
    };
    use axum::http::StatusCode;
    use chrono::Utc;
    use std::collections::{BTreeMap, BTreeSet};
    use temps_auth::{AuthContext, Permission};
    use temps_entities::users;
    use temps_providers::ExternalServiceProjectScope;

    fn custom_api_key(permissions: Vec<Permission>) -> AuthContext {
        let now = Utc::now();
        let user = users::Model {
            id: 42,
            name: "Test User".to_string(),
            email: "test@example.com".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        };

        AuthContext::new_api_key(user, None, Some(permissions), "test-key".to_string(), 1)
    }

    #[test]
    fn template_service_selection_requires_exactly_one_matching_dependency() {
        let required = vec!["postgres".to_string()];

        assert_eq!(
            validate_template_service_selection(&required, &[]),
            Err(TemplateServiceSelectionError::Missing(required.clone()))
        );
        assert!(validate_template_service_selection(&required, &["POSTGRES".to_string()]).is_ok());
        assert!(
            validate_template_service_selection(&["s3".to_string()], &["rustfs".to_string()])
                .is_ok()
        );
        assert_eq!(
            validate_template_service_selection(
                &required,
                &["postgres".to_string(), "postgres".to_string()]
            ),
            Err(TemplateServiceSelectionError::Ambiguous(required))
        );
    }

    #[test]
    fn service_template_change_preview_includes_bindings_and_input_defaults() {
        let applied = temps_core::templates::bundled_template_by_slug("keycloak")
            .expect("Keycloak should be bundled");
        let mut target = applied.clone();
        target
            .managed_service_bindings
            .get_mut("postgres")
            .expect("PostgreSQL bindings should exist")
            .insert("KC_DB_USERNAME".to_string(), "POSTGRES_ROLE".to_string());
        target
            .env_vars
            .iter_mut()
            .find(|variable| variable.name == "KC_HOSTNAME_STRICT")
            .expect("hostname input should exist")
            .default = Some("true".to_string());

        let changes = service_template_changes(&applied, &target);
        assert!(changes.iter().any(|change| {
            change.field == "managed_service_binding.postgres.KC_DB_USERNAME"
                && change.current.as_deref() == Some("POSTGRES_USER")
                && change.target.as_deref() == Some("POSTGRES_ROLE")
        }));
        assert!(changes.iter().any(|change| {
            change.field == "configuration.KC_HOSTNAME_STRICT"
                && change.current.as_deref() == Some("required, default=false")
                && change.target.as_deref() == Some("required, default=true")
        }));
    }

    #[test]
    fn template_creation_requires_project_and_deployment_permissions() {
        let projects_only = custom_api_key(vec![Permission::ProjectsCreate]);
        assert!(require_template_creation_permissions(&projects_only).is_err());

        let authorized = custom_api_key(vec![
            Permission::ProjectsCreate,
            Permission::DeploymentsCreate,
        ]);
        assert!(require_template_creation_permissions(&authorized).is_ok());
    }

    #[test]
    fn browserless_environment_is_complete_and_protects_its_token_server_side() {
        let template = temps_core::templates::bundled_template_by_slug("browserless")
            .expect("Browserless should be bundled");
        let resolved = canonicalize_template_environment_variables(
            &template,
            &[],
            Some("https://browserless-production.example.test"),
        )
        .expect("bundled Browserless variables should resolve");

        let token = resolved
            .iter()
            .find(|variable| variable.key == "TOKEN")
            .expect("TOKEN should be generated");
        assert!(token.is_secret);
        assert!(token.value.len() >= 42);
        assert_eq!(
            resolved
                .iter()
                .find(|variable| variable.key == "EXTERNAL")
                .map(|variable| variable.value.as_str()),
            Some("https://browserless-production.example.test")
        );
        assert_eq!(
            resolved
                .iter()
                .find(|variable| variable.key == "CONCURRENT")
                .map(|variable| variable.value.as_str()),
            Some("2")
        );
    }

    #[test]
    fn client_cannot_downgrade_a_template_password_to_regular_storage() {
        let template = temps_core::templates::bundled_template_by_slug("keycloak")
            .expect("Keycloak should be bundled");
        let requested = [super::super::templates::EnvVarInput {
            name: "KC_BOOTSTRAP_ADMIN_PASSWORD".to_string(),
            value: "operator-supplied".to_string(),
            is_secret: false,
        }];
        let resolved = canonicalize_template_environment_variables(&template, &requested, None)
            .expect("Keycloak defaults should resolve");

        let password = resolved
            .iter()
            .find(|variable| variable.key == "KC_BOOTSTRAP_ADMIN_PASSWORD")
            .expect("admin password should exist");
        assert_eq!(password.value, "operator-supplied");
        assert!(password.is_secret);
    }

    #[test]
    fn template_environment_rejects_duplicates_and_missing_required_values() {
        let template = temps_core::templates::bundled_template_by_slug("browserless")
            .expect("Browserless should be bundled");
        let duplicate = super::super::templates::EnvVarInput {
            name: "TOKEN".to_string(),
            value: "one".to_string(),
            is_secret: true,
        };
        assert!(matches!(
            canonicalize_template_environment_variables(
                &template,
                &[duplicate.clone(), duplicate],
                Some("https://browserless.example.test")
            ),
            Err(TemplateEnvironmentError::Duplicate { .. })
        ));
        assert!(matches!(
            canonicalize_template_environment_variables(&template, &[], None),
            Err(TemplateEnvironmentError::MissingRequired { name }) if name == "EXTERNAL"
        ));
    }

    fn image_template_request() -> super::super::templates::CreateProjectFromTemplateRequest {
        super::super::templates::CreateProjectFromTemplateRequest {
            template_slug: "keycloak".to_string(),
            project_name: "Identity".to_string(),
            git_provider_connection_id: None,
            repository_name: None,
            repository_owner: None,
            private: true,
            environment_variables: Vec::new(),
            storage_service_ids: Vec::new(),
            automatic_deploy: false,
            image: None,
            command: None,
            cpu_request: None,
            cpu_limit: None,
            memory_request: None,
            memory_limit: None,
            exposed_port: None,
            health_check_path: None,
        }
    }

    #[test]
    fn image_template_runtime_uses_curated_defaults() {
        let template = temps_core::templates::bundled_template_by_slug("keycloak")
            .expect("Keycloak should be bundled");

        let runtime = resolve_image_template_runtime(&template, &image_template_request())
            .expect("curated defaults should be valid")
            .expect("Keycloak should resolve to image mode");
        let expected_image = template.image.as_deref().expect("Keycloak image");

        assert_eq!(runtime.image_ref, expected_image);
        assert_eq!(runtime.command, Some(vec!["start".to_string()]));
        assert_eq!(runtime.cpu_request, Some(500_000));
        assert_eq!(runtime.cpu_limit, Some(1_000_000));
        assert_eq!(runtime.memory_request, Some(512));
        assert_eq!(runtime.memory_limit, Some(1_536));
        assert_eq!(runtime.exposed_port, Some(8080));
        assert_eq!(runtime.health_check_path.as_deref(), Some("/realms/master"));

        let stored = image_template_preset_config(&template, &runtime)
            .expect("resolved runtime should be persistable");
        assert_eq!(stored["imageRuntime"]["imageRef"], expected_image);
        assert_eq!(
            stored["imageRuntime"]["command"],
            serde_json::json!(["start"])
        );
        assert_eq!(stored["imageRuntime"]["healthCheckPath"], "/realms/master");
    }

    #[test]
    fn image_template_runtime_applies_user_overrides_and_can_clear_command() {
        let template = temps_core::templates::bundled_template_by_slug("keycloak")
            .expect("Keycloak should be bundled");
        let mut request = image_template_request();
        let override_image = format!("quay.io/keycloak/keycloak@sha256:{}", "a".repeat(64));
        request.image = Some(override_image.clone());
        request.command = Some(Vec::new());
        request.cpu_request = Some(750_000);
        request.cpu_limit = Some(1_500_000);
        request.memory_request = Some(768);
        request.memory_limit = Some(2_048);
        request.exposed_port = Some(9090);
        request.health_check_path = Some("/health/ready".to_string());

        let runtime = resolve_image_template_runtime(&template, &request)
            .expect("valid overrides should resolve")
            .expect("Keycloak should resolve to image mode");

        assert_eq!(runtime.image_ref, override_image);
        assert_eq!(runtime.command, None);
        assert_eq!(runtime.cpu_request, Some(750_000));
        assert_eq!(runtime.cpu_limit, Some(1_500_000));
        assert_eq!(runtime.memory_request, Some(768));
        assert_eq!(runtime.memory_limit, Some(2_048));
        assert_eq!(runtime.exposed_port, Some(9090));
        assert_eq!(runtime.health_check_path.as_deref(), Some("/health/ready"));
    }

    #[test]
    fn image_template_runtime_rejects_unsafe_health_paths() {
        let template = temps_core::templates::bundled_template_by_slug("keycloak")
            .expect("Keycloak should be bundled");
        let mut request = image_template_request();
        request.health_check_path = Some("https://example.test/ready".to_string());

        assert!(matches!(
            resolve_image_template_runtime(&template, &request),
            Err(TemplateRuntimeOverrideError::InvalidHealthCheckPath { .. })
        ));
    }

    #[test]
    fn image_template_runtime_rejects_mutable_image_tags() {
        let template = temps_core::templates::bundled_template_by_slug("keycloak")
            .expect("Keycloak should be bundled");
        let mut request = image_template_request();
        request.image = Some("quay.io/keycloak/keycloak:latest".to_string());

        assert!(matches!(
            resolve_image_template_runtime(&template, &request),
            Err(TemplateRuntimeOverrideError::InvalidImage { .. })
        ));
    }

    #[test]
    fn git_settings_require_connection_and_repository_permissions() {
        let projects_only = custom_api_key(vec![Permission::ProjectsWrite]);
        assert!(require_git_settings_permissions(&projects_only).is_err());

        let missing_repository_read = custom_api_key(vec![
            Permission::ProjectsWrite,
            Permission::GitConnectionsRead,
        ]);
        assert!(require_git_settings_permissions(&missing_repository_read).is_err());

        let authorized = custom_api_key(vec![
            Permission::ProjectsWrite,
            Permission::GitConnectionsRead,
            Permission::GitRepositoriesRead,
        ]);
        assert!(require_git_settings_permissions(&authorized).is_ok());
    }

    struct StorageAccessChecker {
        permissions: BTreeMap<i32, Option<Vec<String>>>,
        coarse_access: BTreeMap<i32, bool>,
    }

    #[temps_core::async_trait::async_trait]
    impl temps_core::ProjectAccessChecker for StorageAccessChecker {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(self
                .coarse_access
                .get(&project_id)
                .copied()
                .unwrap_or(false))
        }

        async fn user_can_access_projects(
            &self,
            _user_id: i32,
            project_ids: &[i32],
        ) -> Result<BTreeMap<i32, bool>, Box<dyn std::error::Error + Send + Sync>> {
            Ok(project_ids
                .iter()
                .filter_map(|project_id| {
                    self.coarse_access
                        .get(project_id)
                        .copied()
                        .map(|allowed| (*project_id, allowed))
                })
                .collect())
        }

        async fn effective_project_permissions_batch(
            &self,
            _user_id: i32,
            project_ids: &[i32],
        ) -> Result<BTreeMap<i32, Option<Vec<String>>>, Box<dyn std::error::Error + Send + Sync>>
        {
            Ok(project_ids
                .iter()
                .filter_map(|project_id| {
                    self.permissions
                        .get(project_id)
                        .cloned()
                        .map(|permissions| (*project_id, permissions))
                })
                .collect())
        }
    }

    fn service_scope(service_id: i32, project_ids: Vec<i32>) -> ExternalServiceProjectScope {
        ExternalServiceProjectScope {
            service_id,
            project_ids,
            created_by_user_id: None,
        }
    }

    #[tokio::test]
    async fn selected_database_requires_write_access_to_each_service_scope() {
        let auth = custom_api_key(vec![
            Permission::ProjectsCreate,
            Permission::ExternalServicesWrite,
        ]);
        let checker = StorageAccessChecker {
            permissions: BTreeMap::from([
                (7, Some(vec![Permission::ExternalServicesWrite.to_string()])),
                (8, Some(vec![Permission::ExternalServicesRead.to_string()])),
            ]),
            coarse_access: BTreeMap::new(),
        };

        let allowed =
            authorize_storage_service_scopes(&auth, Some(&checker), &[service_scope(1, vec![7])])
                .await;
        assert!(allowed.is_ok());

        let denied = authorize_storage_service_scopes(
            &auth,
            Some(&checker),
            &[service_scope(1, vec![7]), service_scope(2, vec![8])],
        )
        .await
        .expect_err("every selected database must be authorized");
        assert_eq!(denied.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn selected_unlinked_database_is_denied_when_project_auth_is_active() {
        let auth = custom_api_key(vec![
            Permission::ProjectsCreate,
            Permission::ExternalServicesWrite,
        ]);
        let checker = StorageAccessChecker {
            permissions: BTreeMap::new(),
            coarse_access: BTreeMap::new(),
        };

        let denied = authorize_storage_service_scopes(
            &auth,
            Some(&checker),
            &[service_scope(1, Vec::new())],
        )
        .await
        .expect_err("unlinked databases have no trustworthy tenant owner");

        assert_eq!(denied.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn creator_can_claim_a_new_unlinked_database_for_project_creation() {
        let auth = custom_api_key(vec![
            Permission::ProjectsCreate,
            Permission::ExternalServicesWrite,
        ]);
        let checker = StorageAccessChecker {
            permissions: BTreeMap::new(),
            coarse_access: BTreeMap::new(),
        };
        let owned_scope = ExternalServiceProjectScope {
            service_id: 1,
            project_ids: Vec::new(),
            created_by_user_id: Some(auth.user_id()),
        };

        let claims = authorize_storage_service_scopes(&auth, Some(&checker), &[owned_scope])
            .await
            .expect("creator should receive the one-time bootstrap claim");
        assert_eq!(claims, vec![1]);
    }

    #[tokio::test]
    async fn creator_cannot_bypass_permissions_after_database_is_linked() {
        let auth = custom_api_key(vec![
            Permission::ProjectsCreate,
            Permission::ExternalServicesWrite,
        ]);
        let checker = StorageAccessChecker {
            permissions: BTreeMap::from([(
                7,
                Some(vec![Permission::ExternalServicesRead.to_string()]),
            )]),
            coarse_access: BTreeMap::new(),
        };
        let linked_scope = ExternalServiceProjectScope {
            service_id: 1,
            project_ids: vec![7],
            created_by_user_id: Some(auth.user_id()),
        };

        let denied = authorize_storage_service_scopes(&auth, Some(&checker), &[linked_scope])
            .await
            .expect_err("creator marker must not bypass linked-project permissions");
        assert_eq!(denied.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn selected_database_coarse_access_fallback_and_oss_mode_are_supported() {
        let auth = custom_api_key(vec![
            Permission::ProjectsCreate,
            Permission::ExternalServicesWrite,
        ]);
        let checker = StorageAccessChecker {
            permissions: BTreeMap::from([(7, None)]),
            coarse_access: BTreeMap::from([(7, true)]),
        };
        let scopes = [service_scope(1, vec![7])];

        assert!(
            authorize_storage_service_scopes(&auth, Some(&checker), &scopes)
                .await
                .is_ok()
        );
        assert!(authorize_storage_service_scopes(&auth, None, &scopes)
            .await
            .is_ok());
    }

    #[test]
    fn project_creation_handlers_preflight_selected_database_permissions() {
        let source = include_str!("handlers.rs");
        for handler_name in ["create_project", "create_project_from_template"] {
            let start = source
                .find(&format!("pub async fn {handler_name}"))
                .expect("project creation handler exists");
            let tail = &source[start + 1..];
            let end = tail.find("pub async fn").unwrap_or(tail.len());
            let body = &source[start..start + 1 + end];

            assert!(body.contains("permission_guard!(auth, ExternalServicesWrite)"));
            assert!(body.contains("require_storage_services_access"));
        }

        let template_start = source
            .find("pub async fn create_project_from_template")
            .expect("template handler exists");
        let template_body = &source[template_start..];
        assert!(
            template_body
                .find("require_storage_services_access")
                .expect("authorization preflight")
                < template_body
                    .find("get_template(&request.template_slug)")
                    .expect("template lookup"),
            "database authorization must precede template/repository side effects"
        );
    }

    #[test]
    fn drop_compose_candidate_preserves_detected_modern_filename() {
        let manifests = BTreeMap::from([(
            "compose.yaml".to_string(),
            "services:\n  web:\n    image: nginx".to_string(),
        )]);
        let candidate = temps_presets::detect_project_candidates(&manifests)
            .into_iter()
            .next()
            .unwrap();

        assert_eq!(
            compose_path_for_candidate(&manifests, &candidate).as_deref(),
            Some("compose.yaml")
        );
    }

    #[test]
    fn drop_compose_candidate_returns_path_relative_to_nested_project() {
        let manifests = BTreeMap::from([(
            "apps/photos/compose.yml".to_string(),
            "services:\n  web:\n    image: nginx".to_string(),
        )]);
        let candidate = temps_presets::detect_project_candidates(&manifests)
            .into_iter()
            .next()
            .unwrap();

        assert_eq!(candidate.path, "apps/photos");
        assert_eq!(
            compose_path_for_candidate(&manifests, &candidate).as_deref(),
            Some("compose.yml")
        );
    }

    /// A bare Dockerfile in a `docker/`-named subdirectory, with something
    /// else at the repository root, must be rolled up into a repo-root
    /// `DropPresetCandidate` that records where the Dockerfile actually
    /// lives, so the build context defaults correctly.
    #[test]
    fn drop_preset_candidate_bare_dockerfile_in_docker_dir_roots_at_repo_root() {
        let manifests = BTreeMap::from([
            ("package.json".to_string(), "{}".to_string()),
            ("docker/Dockerfile".to_string(), "FROM scratch".to_string()),
        ]);
        let candidate = temps_presets::detect_project_candidates(&manifests)
            .into_iter()
            .find(|c| c.preset == temps_entities::preset::Preset::Dockerfile)
            .expect("dockerfile candidate should be detected");

        let drop_candidate = drop_preset_candidate_from(&manifests, candidate);

        assert_eq!(drop_candidate.directory, ".");
        assert_eq!(
            drop_candidate.dockerfile_path.as_deref(),
            Some("docker/Dockerfile")
        );
    }

    /// A genuine monorepo service directory (its own Dockerfile plus its own
    /// manifest, in a directory name that isn't a conventional Docker-tooling
    /// name) keeps today's behavior: its own root, `dockerfile_path: None`.
    #[test]
    fn drop_preset_candidate_service_dockerfile_keeps_own_root() {
        let manifests = BTreeMap::from([
            (
                "apps/api/Dockerfile".to_string(),
                "FROM scratch".to_string(),
            ),
            ("apps/api/package.json".to_string(), "{}".to_string()),
        ]);
        let candidate = temps_presets::detect_project_candidates(&manifests)
            .into_iter()
            .find(|c| c.preset == temps_entities::preset::Preset::Dockerfile)
            .expect("dockerfile candidate should be detected");

        let drop_candidate = drop_preset_candidate_from(&manifests, candidate);

        assert_eq!(drop_candidate.directory, "apps/api");
        assert_eq!(drop_candidate.dockerfile_path, None);
    }

    /// `dockerfile_path: None` must be omitted from the serialized response
    /// entirely (matching the existing `compose_path` convention), and a
    /// populated value must serialize under the camelCase key the frontend
    /// expects.
    #[test]
    fn drop_preset_candidate_dockerfile_path_serializes_camel_case_and_omits_when_none() {
        let without = DropPresetCandidate {
            directory: "apps/api".to_string(),
            preset: "dockerfile".to_string(),
            compose_path: None,
            label: "Dockerfile".to_string(),
            confidence: "high".to_string(),
            reason: "Dockerfile found".to_string(),
            is_static: false,
            dockerfile_path: None,
        };
        let json = serde_json::to_value(&without).unwrap();
        assert!(json.get("dockerfilePath").is_none());

        let with = DropPresetCandidate {
            dockerfile_path: Some("docker/Dockerfile".to_string()),
            ..without
        };
        let json = serde_json::to_value(&with).unwrap();
        assert_eq!(json["dockerfilePath"], "docker/Dockerfile");
    }

    #[test]
    fn template_creation_telemetry_omits_operator_defined_slug() {
        let private_slug = "customer-acme-private-ghp_secret123";
        let event = project_created_from_template_telemetry_event(private_slug, "image", 2);
        let serialized = serde_json::to_string(&event).expect("telemetry event serializes");

        assert_eq!(event.properties["template_source"], "custom");
        assert!(!event.properties.contains_key("template_slug"));
        assert!(!serialized.contains(private_slug));
        assert_eq!(event.properties["deploy_mode"], "image");
        assert_eq!(event.properties["service_count"], 2);
    }

    #[test]
    fn template_creation_telemetry_keeps_bundled_observability_slug() {
        let event =
            project_created_from_template_telemetry_event("observability-starter", "image", 1);

        assert_eq!(event.properties["template_source"], "bundled");
        assert_eq!(event.properties["template_slug"], "observability-starter");
    }

    #[test]
    fn failed_image_dispatch_reports_partial_success_with_retry_guidance() {
        let (queued, error) = image_deployment_dispatch_feedback(false);
        assert_eq!(queued, Some(false));
        assert!(error
            .as_deref()
            .is_some_and(|message| message.contains("select Deploy to retry")));

        let (queued, error) = image_deployment_dispatch_feedback(true);
        assert_eq!(queued, Some(true));
        assert!(error.is_none());
    }

    #[test]
    fn template_upgrade_only_accepts_new_declared_configuration() {
        let mut template = temps_core::templates::bundled_template_by_slug("browserless")
            .expect("browserless template must be bundled");
        template
            .env_vars
            .push(temps_core::templates::EnvVarTemplate {
                name: "NEW_API_KEY".to_string(),
                example: None,
                default: None,
                description: Some("New integration credential".to_string()),
                required: true,
                secret: true,
                default_generator: None,
            });
        let configured = BTreeSet::from(["TOKEN".to_string()]);

        let missing = missing_required_template_configuration(&template, &configured);
        assert_eq!(
            missing
                .iter()
                .map(|variable| variable.name.as_str())
                .collect::<Vec<_>>(),
            vec!["NEW_API_KEY"]
        );

        let resolved = canonicalize_template_upgrade_environment_variables(
            &template,
            &[crate::handlers::templates::EnvVarInput {
                name: "NEW_API_KEY".to_string(),
                value: "integration-secret".to_string(),
                is_secret: false,
            }],
            &configured,
            Some("https://browser.example.test"),
        )
        .expect("new declared configuration should resolve");
        assert!(resolved.iter().any(|variable| {
            variable.key == "NEW_API_KEY"
                && variable.value == "integration-secret"
                && variable.is_secret
        }));
        assert!(resolved.iter().any(|variable| variable.key == "EXTERNAL"));
    }

    #[test]
    fn template_upgrade_treats_empty_production_override_as_unconfigured() {
        let now = Utc::now();
        let variables = vec![
            crate::services::types::EnvVarWithEnvironments {
                id: 1,
                project_id: 7,
                key: "NEW_API_KEY".to_string(),
                value: "global-fallback".to_string(),
                has_value: true,
                created_at: now,
                updated_at: now,
                environments: Vec::new(),
            },
            crate::services::types::EnvVarWithEnvironments {
                id: 2,
                project_id: 7,
                key: "NEW_API_KEY".to_string(),
                value: "***".to_string(),
                has_value: false,
                created_at: now,
                updated_at: now,
                environments: vec![crate::services::types::EnvVarEnvironment {
                    id: 20,
                    name: "production".to_string(),
                }],
            },
        ];

        let configured = production_environment_variable_names(&variables);

        assert!(!configured.contains("NEW_API_KEY"));
        let serialized = serde_json::to_value(&variables[1]).expect("serialize env var response");
        assert!(serialized.get("has_value").is_none());
        assert_eq!(serialized["value"], "***");
    }

    #[test]
    fn template_upgrade_cannot_overwrite_existing_or_inject_unknown_configuration() {
        let template = temps_core::templates::bundled_template_by_slug("browserless")
            .expect("browserless template must be bundled");
        let configured = BTreeSet::from(["TOKEN".to_string()]);

        let overwrite = canonicalize_template_upgrade_environment_variables(
            &template,
            &[crate::handlers::templates::EnvVarInput {
                name: "TOKEN".to_string(),
                value: "replacement".to_string(),
                is_secret: true,
            }],
            &configured,
            None,
        );
        assert!(matches!(
            overwrite,
            Err(TemplateEnvironmentError::AlreadyConfigured { .. })
        ));

        let unknown = canonicalize_template_upgrade_environment_variables(
            &template,
            &[crate::handlers::templates::EnvVarInput {
                name: "UNDECLARED".to_string(),
                value: "value".to_string(),
                is_secret: false,
            }],
            &configured,
            Some("https://browser.example.test"),
        );
        assert!(matches!(
            unknown,
            Err(TemplateEnvironmentError::Unknown { .. })
        ));
    }

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
    fn service_template_upgrade_requires_project_and_environment_permissions() {
        let source = include_str!("handlers.rs");
        let fn_start = source
            .find("pub async fn upgrade_project_service_template")
            .expect("service template upgrade handler not found in source");
        let after_start = &source[fn_start + 1..];
        let next_fn_offset = after_start
            .find("pub async fn")
            .unwrap_or(after_start.len());
        let fn_body = &source[fn_start..fn_start + 1 + next_fn_offset];

        assert!(fn_body.contains("permission_guard!(auth, ProjectsWrite)"));
        assert!(fn_body.contains("permission_guard!(auth, EnvironmentsCreate)"));
        assert!(fn_body.contains("project_scope_guard!(auth, project_id)"));
        assert!(fn_body
            .contains("project_access_guard!(auth, project_id, state.project_access_checker)"));
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
