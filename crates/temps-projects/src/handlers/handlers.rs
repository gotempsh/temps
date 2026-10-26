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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use temps_auth::{
    permission_guard, project_access_guard, project_permission_guard, project_scope_guard,
};
use temps_auth::{AuthContext, Permission, RequireAuth};
use temps_core::RequestMetadata;
use tracing::{debug, error, info, warn};

use super::types::{
    ChangeProjectSourceRequest, CreateProjectRequest, PaginatedProjectList, PaginationParams,
    ProjectResponse, ProjectStatisticsResponse, ReinstallGithubWebhookResponse,
    ReinstallWebhookResponse, ServiceTemplateChangeKind, ServiceTemplateInstanceResponse,
    ServiceTemplateUpgradeChange, SetAlternateSourcesRequest, SetProjectHostingRequest,
    TriggerPipelinePayload, TriggerPipelineResponse, UpdateAutomaticDeployRequest,
    UpdateDeploymentConfigRequest, UpdateGitSettingsRequest, UpdateProjectSettingsRequest,
    UpdateServiceTemplateRuntimeRequest, UpgradeServiceTemplateRequest,
};
use crate::services::project::GroupFilter;
use crate::template_creation::{
    canonicalize_template_upgrade_environment_variables, missing_required_template_configuration,
    require_template_creation_permissions, resolve_image_template_runtime,
    upgrade_needs_generated_app_url, validate_template_service_selection, TemplateEnvironmentError,
    TemplateRuntimeOverrides, TemplateServiceSelectionError,
};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use temps_core::problemdetails;
use temps_core::problemdetails::Problem;
use tokio::io::AsyncWriteExt;

/// Record who tried to claim a slug reserved by the host Docker socket grant
/// (ADR 045).
///
/// The service refuses the write and logs what it knows; this adds the one
/// thing only the HTTP layer has — the principal. There is no audit event for
/// a write that did not happen, and inventing a `ProjectCreated` record for a
/// project that was never created would be worse than a log line: this is the
/// closest honest equivalent.
pub(crate) fn log_reserved_slug_refusal(
    error: &crate::services::types::ProjectError,
    user_id: i32,
    project_id: Option<i32>,
) {
    if let crate::services::types::ProjectError::DockerSocketSlugReserved { slug, change } = error {
        warn!(
            user_id,
            project_id = ?project_id,
            slug = %slug,
            change = ?change,
            "Rejected a non-admin attempt to move a project slug that is granted host Docker \
             access on this host (ADR 045)"
        );
    }
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    use axum::extract::DefaultBodyLimit;
    let custom_domain_routes = super::custom_domains::configure_routes();

    Router::new()
        .route(
            "/projects/{id}/compose-security",
            get(super::compose_security::get_compose_security)
                .put(super::compose_security::update_compose_security),
        )
        // Project CRUD routes
        .route("/projects/{id}", get(get_project))
        .route("/projects/by-slug/{slug}", get(get_project_by_slug))
        .route("/projects/{id}", put(update_project))
        .route("/projects/{id}/source", patch(change_project_source))
        .route(
            "/projects/{id}/alternate-sources",
            patch(set_alternate_sources),
        )
        .route("/projects/{id}/hosting", patch(set_project_hosting))
        .route("/projects/{id}", delete(delete_project))
        .route(
            "/projects/cloudflare-capability",
            get(get_cloudflare_project_capability),
        )
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
        .route(
            "/projects/{project_id}/github/reinstall-webhook",
            post(reinstall_github_webhook),
        )
        // Merge custom domain routes
        .merge(custom_domain_routes)
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

/// The definition of the service a release snapshot belongs to.
fn release_spec(
    instance: &temps_core::templates::ServiceTemplateInstance,
) -> &temps_core::templates::TemplateServiceSpec {
    instance
        .spec()
        .unwrap_or_else(|| instance.template.primary())
}

/// A catalog release, pointed at the same service of the app as `applied`.
fn same_service_release(
    release: temps_core::templates::ServiceTemplateInstance,
    applied: &temps_core::templates::ServiceTemplateInstance,
) -> temps_core::templates::ServiceTemplateInstance {
    match applied.service.clone() {
        Some(service) => release.for_service(service),
        None => release,
    }
}

/// Why a one-service app cannot be moved onto a release that has several
/// services through the single-service upgrade.
fn single_to_multi_service_upgrade_detail(
    project_id: i32,
    target: &temps_core::templates::ServiceTemplateInstance,
) -> String {
    format!(
        "Project {project_id} runs a single service of '{}', but release {}@{} has {} services that must be created together. It cannot be upgraded one service at a time: deploy {} as a whole app from the template catalog instead.",
        target.slug,
        target.slug,
        target.version,
        target.template.services.len(),
        target.template.name,
    )
}

/// What changes between two releases of the same service, comparing the
/// variables each release actually configures the service with
/// ([`temps_core::templates::ServiceTemplateInstance::service_variables`]):
/// a single-service template's shared variables are part of its service.
fn service_release_changes(
    applied: &temps_core::templates::ServiceTemplateInstance,
    target: &temps_core::templates::ServiceTemplateInstance,
) -> Vec<ServiceTemplateUpgradeChange> {
    template_spec_changes(
        release_spec(applied),
        &applied.service_variables(),
        release_spec(target),
        &target.service_variables(),
    )
}

#[cfg(test)]
fn service_template_changes(
    applied: &temps_core::templates::TemplateServiceSpec,
    target: &temps_core::templates::TemplateServiceSpec,
) -> Vec<ServiceTemplateUpgradeChange> {
    template_spec_changes(applied, &applied.env_vars, target, &target.env_vars)
}

fn template_spec_changes(
    applied: &temps_core::templates::TemplateServiceSpec,
    applied_variables: &[temps_core::templates::EnvVarTemplate],
    target: &temps_core::templates::TemplateServiceSpec,
    target_variables: &[temps_core::templates::EnvVarTemplate],
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

    let applied_inputs = applied_variables
        .iter()
        .map(|input| (input.name.as_str(), input))
        .collect::<BTreeMap<_, _>>();
    let target_inputs = target_variables
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

    let applied_services = applied.databases.iter().collect::<BTreeSet<_>>();
    let target_services = target.databases.iter().collect::<BTreeSet<_>>();
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
        .database_bindings
        .iter()
        .flat_map(|(service, bindings)| {
            bindings
                .iter()
                .map(move |(target, source)| ((service.as_str(), target.as_str()), source.as_str()))
        })
        .collect::<BTreeMap<_, _>>();
    let target_bindings = target
        .database_bindings
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
    crate::template_creation::authorize_storage_services(
        &state.external_service_manager,
        state.project_access_checker.as_deref(),
        auth,
        service_ids,
    )
    .await
}

async fn canonical_template_app_url(
    state: &AppState,
    project_slug: &str,
) -> Result<String, Problem> {
    crate::template_creation::template_app_url(
        &state.config_service,
        state.public_hostname_resolver.as_ref(),
        project_slug,
    )
    .await
}

#[derive(OpenApi)]
#[openapi(
    paths(
        super::compose_security::get_compose_security,
        super::compose_security::update_compose_security,
        create_project,
        get_cloudflare_project_capability,
        inspect_drop_archive,
        get_project,
        update_project,
        change_project_source,
        set_alternate_sources,
        set_project_hosting,
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
        reinstall_github_webhook,
        trigger_project_pipeline,
        get_project_statistics,
        list_presets,
        generate_preset_dockerfile,
        list_project_templates,
        get_project_template,
        list_project_template_tags,
        create_project_from_template,
        super::operations::list_operations,
    ),
    components(
        schemas(
            temps_core::repo_config::HealthCheckConfig,
            CreateProjectRequest,
            super::types::ProjectEnvVarInput,
            DropInspectionResponse,
            DropPresetCandidate,
            temps_core::drop_entries::DropSkippedPath,
            temps_core::drop_entries::DropSkipReason,
            ChangeProjectSourceRequest,
            SetAlternateSourcesRequest,
            SetProjectHostingRequest,
            temps_entities::types::Hosting,
            ProjectResponse,
            super::types::CloudflareProjectCapability,
            temps_core::docker_socket_grant::DockerSocketCapability,
            PaginatedProjectList,
            PaginationParams,
            UpdateProjectSettingsRequest,
            UpdateGitSettingsRequest,
            UpdateAutomaticDeployRequest,
            UpdateDeploymentConfigRequest,
            ReinstallWebhookResponse,
            ReinstallGithubWebhookResponse,
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
            super::operations::ListOperationsQuery,
            super::operations::OperationsListResponse,
            crate::services::operations::OperationEntry,
            crate::services::operations::OperationKind,
            crate::services::operations::OperationStatus,
            crate::services::operations::OperationStatusFilter,
        )
    ),
    tags(
        (name = "Projects", description = "Project management endpoints"),
        (name = "Presets", description = "Available deployment presets"),
        (name = "Templates", description = "Project template endpoints"),
        (name = "Operations", description = "Recent and in-flight operations across projects")
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
const MAX_DROP_PROJECT_ROOTS: usize = 50;
// Each root can offer its existing preset plus Ruby and PHP alternatives.
const MAX_DROP_CANDIDATES: usize = MAX_DROP_PROJECT_ROOTS * 3;

struct DropInspectionPermit;

impl DropInspectionPermit {
    fn acquire() -> Result<Self, Problem> {
        DROP_INSPECTIONS_IN_FLIGHT
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
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
    /// What the deployment will leave out of this archive: version control,
    /// installed dependencies, environment files that may hold secrets, OS
    /// metadata. Directories are listed once. At most 50 paths; see
    /// `skipped_file_count` for the total.
    #[serde(default)]
    pub skipped_entries: Vec<temps_core::drop_entries::DropSkippedPath>,
    /// Archive entries left out in total, including any not listed.
    #[serde(default)]
    pub skipped_file_count: u64,
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
    let (candidates, skipped) = tokio::task::spawn_blocking(move || {
        let _inspection_permit = inspection_permit;
        let inspection = inspect_zip_archive(&archive_path)?;
        Ok::<_, Problem>((
            drop_inspection_candidates(&inspection.manifests),
            inspection.skipped,
        ))
    })
    .await
    .map_err(|error| {
        problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Archive Inspection Failed")
            .with_detail(error.to_string())
    })??;

    if candidates.is_empty() {
        return Err(no_deployable_project_problem());
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
        skipped_entries: skipped.paths(),
        skipped_file_count: skipped.total_files(),
    }))
}

/// Problem type the console keys its "nothing to deploy" onboarding state on.
pub const DROP_NO_PROJECT_PROBLEM_TYPE: &str = "https://temps.sh/probs/drop-no-deployable-project";

/// Rejection for an archive with no recognisable project root.
///
/// Retrying detection on the same files cannot succeed, so the detail names
/// what Temps looks for and the one change that always works (a Dockerfile);
/// `supported_files` lets the console render that list without parsing prose.
fn no_deployable_project_problem() -> Problem {
    let signals = temps_presets::project_signal_names();
    problemdetails::new(StatusCode::BAD_REQUEST)
        .with_type(DROP_NO_PROJECT_PROBLEM_TYPE)
        .with_title("No Deployable Project Found")
        .with_detail(format!(
            "No deployable project was found in the archive. Temps looks for one of \
             {} at the archive root or in a folder up to 4 levels deep. Add a \
             Dockerfile to deploy any other application, or add the manifest \
             your language uses.",
            signals.join(", ")
        ))
        .with_value("supported_files", signals)
}

/// Bound the picker without letting one root's alternatives crowd out later
/// applications. Keep the first choice for each of the first 50 roots, then
/// fill the remaining bounded allowance with alternatives in detection order.
fn drop_inspection_candidates(manifests: &BTreeMap<String, String>) -> Vec<DropPresetCandidate> {
    let mut roots = BTreeSet::new();
    let mut primary = Vec::new();
    let mut alternatives = Vec::new();
    for candidate in temps_presets::detect_project_candidates(manifests) {
        if roots.contains(&candidate.path) {
            if alternatives.len() < MAX_DROP_CANDIDATES {
                alternatives.push(drop_preset_candidate_from(manifests, candidate));
            }
        } else if roots.len() < MAX_DROP_PROJECT_ROOTS {
            roots.insert(candidate.path.clone());
            primary.push(drop_preset_candidate_from(manifests, candidate));
        }
    }
    let remaining = MAX_DROP_CANDIDATES.saturating_sub(primary.len());
    primary.extend(alternatives.into_iter().take(remaining));
    primary
}

/// Convert a detected project candidate into the response DTO for the
/// drop-inspection endpoint, resolving the compose file path alongside it.
fn drop_preset_candidate_from(
    manifests: &BTreeMap<String, String>,
    candidate: temps_presets::ProjectCandidate,
) -> DropPresetCandidate {
    let preset = candidate.catalog_slug().to_string();
    let compose_path = compose_path_for_candidate(manifests, &candidate);
    let label = candidate.label().to_string();
    DropPresetCandidate {
        directory: candidate.path,
        preset,
        compose_path,
        label,
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

/// Rejection for an archive entry that is private material. Names the entry,
/// in `detail` and as `entry`, so the console can say which file to remove.
fn sensitive_zip_entry_problem(
    entry: &str,
    reason: temps_core::drop_entries::DropRefusalReason,
) -> Problem {
    problemdetails::new(StatusCode::BAD_REQUEST)
        .with_title("Sensitive ZIP Entry")
        .with_detail(format!(
            "Archive entry '{entry}' {reason}. Remove it from the archive and upload again: \
             Drop never accepts private keys or credentials. Environment files (.env, \
             .env.local), node_modules/ and .git/ do not need removing; they are left out \
             automatically, and templates such as .env.example are kept"
        ))
        .with_value("entry", entry)
}

/// Manifests of a Drop archive, for detection; see [`inspect_zip_archive`].
#[cfg(test)]
fn inspect_zip_manifests(path: &std::path::Path) -> Result<BTreeMap<String, String>, Problem> {
    inspect_zip_archive(path).map(|inspection| inspection.manifests)
}

/// What [`inspect_zip_archive`] read from a Drop archive.
struct DropZipInspection {
    /// Every kept entry's path; the contents of the manifests detection reads.
    manifests: BTreeMap<String, String>,
    /// Entries left out under the Drop rules, for the console's notice.
    skipped: temps_core::drop_entries::DropSkipReport,
}

fn inspect_zip_archive(path: &std::path::Path) -> Result<DropZipInspection, Problem> {
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
    let mut skipped = temps_core::drop_entries::DropSkipReport::default();
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
        // The decoded name is what `enclosed_name` and the classifier see
        // (legacy CP437 names included), so diagnostics must show the same
        // one. `ZipFile::name` is fallible since zip 9; only on an actual
        // decode error fall back to a lossy rendering of the raw bytes.
        let display_name = || match entry.name() {
            Ok(name) => name,
            Err(_) => String::from_utf8_lossy(entry.name_raw()),
        };
        let path = entry.enclosed_name().ok_or_else(|| {
            problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Unsafe ZIP Entry")
                .with_detail(format!(
                    "Archive entry '{}' escapes the project root",
                    display_name()
                ))
        })?;
        match temps_core::drop_entries::classify_drop_entry(&path) {
            temps_core::drop_entries::DropEntryDisposition::Keep => {}
            temps_core::drop_entries::DropEntryDisposition::Skip {
                reason,
                reported_path,
            } => {
                skipped.record(reason, reported_path);
                continue;
            }
            temps_core::drop_entries::DropEntryDisposition::Refuse { reason } => {
                return Err(sensitive_zip_entry_problem(&display_name(), reason));
            }
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Unsupported ZIP Entry")
                .with_detail(format!("Symbolic link '{}' is not allowed", display_name())));
        }
        let normalized = path.to_string_lossy().replace('\\', "/");
        let (directory, basename) = normalized.rsplit_once('/').unwrap_or((".", &normalized));
        let should_read = temps_presets::is_project_candidate_directory(directory)
            && (matches!(
                basename,
                "package.json"
                    | "Gemfile"
                    | "composer.json"
                    | "requirements.txt"
                    | "pyproject.toml"
                    | "Cargo.toml"
                    | "go.mod"
                    | "pom.xml"
                    | "build.gradle"
            ) || basename.ends_with(".csproj"));
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
    Ok(DropZipInspection { manifests, skipped })
}

/// Tell project creators whether Cloudflare is ready and what their new-project default is.
#[utoipa::path(
    get,
    path = "/projects/cloudflare-capability",
    tag = "Projects",
    responses((status = 200, body = super::types::CloudflareProjectCapability)),
    security(("bearer_auth" = []))
)]
pub async fn get_cloudflare_project_capability(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsCreate);
    Ok(Json(
        state
            .project_service
            .cloudflare_project_capability()
            .await?,
    ))
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
        (status = 403, description = "Insufficient permissions, or the slug is reserved for host Docker access and only an instance admin may claim it (ADR 045)"),
        (status = 409, description = "Expected project slug is already in use"),
        (status = 428, description = "The reserved slug requires a recently MFA-verified session; complete step-up verification and retry (ADR 045)"),
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
    super::compose_security::reject_legacy_sandbox_grants(project.preset_config.as_ref())?;
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
        cloudflare_enabled: project.cloudflare_enabled,
        delivery_provider: project.delivery_provider,
        name: project.name,
        expected_slug: project.expected_slug,
        repo_name: project.repo_name,
        repo_owner: project.repo_owner,
        directory: project.directory,
        main_branch: project.main_branch,
        preset: project.preset,
        preset_config: project.preset_config,
        environment_variables: project.environment_variables,
        // Temps never deploys a project whose app runs elsewhere.
        automatic_deploy: project.hosting == temps_entities::types::Hosting::Hosted
            && project.automatic_deploy.unwrap_or(false),
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
        hosting: project.hosting,
    };

    let new_project = state
        .project_service
        // ADR 045: only an instance admin may claim a slug this host grants
        // host Docker access to — including one derived from the name — and
        // only with a recently MFA-verified session.
        .create_project_as(
            project_req,
            &crate::services::types::SlugClaimCaller::from_request(
                &auth,
                state.sensitive_action_authorizer.as_ref(),
            ),
        )
        .await
        .inspect_err(|error| log_reserved_slug_refusal(error, auth.user_id(), None))
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
    hidden_project_ids_for_caller(state.project_access_checker.as_ref(), auth).await
}

/// [`resolve_hidden_projects`] for handlers that hold the optional checker
/// directly rather than the projects `AppState`. Same rules, same fail-closed
/// behaviour.
pub(crate) async fn hidden_project_ids_for_caller(
    project_access_checker: Option<&Arc<dyn temps_core::ProjectAccessChecker>>,
    auth: &temps_auth::context::AuthContext,
) -> Result<Vec<i32>, Problem> {
    if auth.is_deployment_token()
        || auth.is_admin()
        || auth.has_role(&temps_auth::permissions::Role::PlatformAdmin)
    {
        return Ok(Vec::new());
    }
    let Some(checker) = project_access_checker else {
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

/// Resolve the optional group filter from the two mutually exclusive query params.
/// Returns `Err(400)` when both are specified, `Ok(None)` when neither is.
fn build_group_filter(
    ungrouped: Option<bool>,
    project_group_id: Option<i32>,
) -> Result<Option<GroupFilter>, temps_core::problemdetails::Problem> {
    if ungrouped.unwrap_or(false) && project_group_id.is_some() {
        return Err(
            temps_core::error_builder::ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .type_("https://temps.sh/probs/invalid-filter")
                .title("Invalid Filter")
                .detail("`ungrouped` and `project_group_id` cannot both be specified")
                .build(),
        );
    }
    if ungrouped.unwrap_or(false) {
        Ok(Some(GroupFilter::Ungrouped))
    } else {
        Ok(project_group_id.map(GroupFilter::InGroup))
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
        ("search" = Option<String>, Query, description = "Case-insensitive project name or slug filter"),
        ("ungrouped" = Option<bool>, Query, description = "When true, return only services that are not a member of any project group. Mutually exclusive with project_group_id."),
        ("project_group_id" = Option<i32>, Query, description = "Return only services that are members of this project group. Mutually exclusive with ungrouped.")
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

    let group_filter = build_group_filter(params.ungrouped, params.project_group_id)?;

    // Per-resource guards keep a user out of a project they click on; this
    // keeps its name out of the list in the first place. Instance
    // administrators are exempt, matching `project_access_guard!`.
    let hidden = resolve_hidden_projects(&state, &auth).await?;

    let (projects, total) = state
        .project_service
        .get_projects_paginated_filtered(page, per_page, &hidden, search, group_filter)
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

    // ADR 045. Always attached on the detail response, granted or not: the
    // console renders a badge when granted and an onboarding state when not,
    // and a capability that is simply absent teaches the operator nothing.
    let docker_socket = state
        .project_service
        .docker_socket_capability(&project.slug)
        .await
        .map_err(Problem::from)?;

    let reachability_warning = state.project_service.reachability_warning(id).await;

    Ok(Json(
        ProjectResponse::map_from_project(project)
            .with_docker_socket(docker_socket)
            .with_reachability_warning(reachability_warning),
    ))
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

    // Same capability as `get_project` — the console uses whichever lookup it
    // has, and the two must not disagree about whether the badge shows.
    let docker_socket = state
        .project_service
        .docker_socket_capability(&project.slug)
        .await
        .map_err(Problem::from)?;

    let reachability_warning = state.project_service.reachability_warning(project.id).await;

    Ok(Json(
        ProjectResponse::map_from_project(project)
            .with_docker_socket(docker_socket)
            .with_reachability_warning(reachability_warning),
    )
    .into_response())
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
    super::compose_security::reject_legacy_sandbox_grants(project.preset_config.as_ref())?;
    project_permission_guard!(auth, ProjectsWrite, id, state.project_access_checker);
    project_scope_guard!(auth, id);

    let project_req = crate::services::types::CreateProjectRequest {
        cloudflare_enabled: None,
        delivery_provider: None,
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
        template_slug: None,              // Template provenance is immutable,
        hosting: Default::default(),
    };
    let updated_project = state
        .project_service
        .update_project(
            id,
            project_req,
            temps_core::docker_socket_grant::DeployCaller::from_instance_admin(
                auth.is_instance_admin(),
            ),
        )
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
        hosting: None,
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
        .set_source_type(
            id,
            req.source_type,
            temps_core::docker_socket_grant::DeployCaller::from_instance_admin(
                auth.is_instance_admin(),
            ),
        )
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
            hosting: None,
        },
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit_event).await {
        error!("Failed to create audit log: {:?}", e);
    }

    Ok(Json(ProjectResponse::map_from_project(updated)).into_response())
}

/// Set whether Temps hosts a project
///
/// `external` marks a project whose app runs elsewhere (Vercel, Netlify, a
/// VPS) and only sends Temps its analytics, errors and traces: Temps refuses
/// to deploy it, and deployments already running keep running until stopped.
/// `hosted` lets Temps deploy it again.
#[utoipa::path(
    patch,
    path = "/projects/{id}/hosting",
    tag = "Projects",
    params(("id" = i32, Path, description = "Project ID")),
    request_body = SetProjectHostingRequest,
    responses(
        (status = 200, description = "Hosting updated", body = ProjectResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn set_project_hosting(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(req): Json<SetProjectHostingRequest>,
) -> Result<impl IntoResponse, Problem> {
    // Deciding whether Temps deploys this project is a project-scoped write:
    // a user with the global permission but only a viewer grant on this
    // project must be refused, which membership alone would not do.
    project_permission_guard!(auth, ProjectsWrite, id, state.project_access_checker);
    project_scope_guard!(auth, id);

    let updated = state
        .project_service
        .set_hosting(
            id,
            req.hosting,
            temps_core::docker_socket_grant::DeployCaller::from_instance_admin(
                auth.is_instance_admin(),
            ),
        )
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
            hosting: Some(updated.hosting.to_string()),
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
        .set_allow_alternate_sources(
            id,
            req.allow_alternate_sources,
            temps_core::docker_socket_grant::DeployCaller::from_instance_admin(
                auth.is_instance_admin(),
            ),
        )
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
            hosting: None,
        },
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit_event).await {
        error!("Failed to create audit log: {:?}", e);
    }

    Ok(Json(ProjectResponse::map_from_project(updated)).into_response())
}

/// Tear a project down: fence it, stop everything serving for it, release its
/// resources and finally remove its rows.
///
/// Runs on a spawned task (see [`delete_project`]) so a client disconnect
/// cannot abort it between steps. Every step is safe to retry, so a failure
/// leaves the project fenced and a repeated `DELETE` finishes the job.
async fn run_project_teardown(
    state: Arc<AppState>,
    id: i32,
    project: crate::services::types::Project,
    audit_context: AuditContext,
) -> Result<(), Problem> {
    // Failures are logged here, inside the task: after a client disconnect
    // nobody awaits this result, so an unlogged error would leave the project
    // fenced with no trace of why the cleanup stopped.
    state
        .project_service
        .begin_project_deletion(id)
        .await
        .map_err(|error| {
            warn!(
                project_id = id,
                %error,
                "Project deletion did not start; project was not fenced"
            );
            Problem::from(error)
        })?;

    // Stop serving anything public for this project now: if cleanup below
    // fails, the project stays fenced but `ProjectDeleted` is never emitted.
    if let Some(hook) = &state.project_deletion_hook {
        hook.project_deletion_started(id).await;
    }

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
        .await
        .map_err(|error| {
            error!(
                project_id = id,
                project_name = %project.name,
                %error,
                "Failed to delete project rows; project stays fenced, retry the delete to finish it"
            );
            Problem::from(error)
        })?;

    info!(project_id = id, project_name = %project.name, "Project deleted");

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

    Ok(())
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
        (status = 409, description = "Project still has CDN delivery bindings; remove them first"),
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

    let audit_context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent,
    };

    // The teardown is a multi-step sequence (fence, cancel deployments, remove
    // containers, remove archives, delete rows) that must not stop half-way.
    // Axum drops the handler future when the client disconnects, which would
    // leave the project fenced (`is_deleted = true`) but still listed and
    // serving. Run it on its own task so it always completes; the handler only
    // awaits the result. Dropping the `JoinHandle` detaches the task rather
    // than cancelling it.
    let teardown = tokio::spawn(run_project_teardown(
        state.clone(),
        id,
        project,
        audit_context,
    ));

    match teardown.await {
        Ok(result) => result?,
        Err(join_error) => {
            error!(project_id = id, %join_error, "Project deletion task failed");
            return Err(problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Project Deletion Failed")
                .with_detail(format!(
                    "Project {id} deletion task did not complete: {join_error}. \
                     The project may be partially deleted; retry the delete to finish it."
                )));
        }
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
        (status = 403, description = "Forbidden, or the slug being claimed or given up is reserved for host Docker access and only an instance admin may move it (ADR 045)"),
        (status = 404, description = "Project not found"),
        (status = 428, description = "The reserved slug requires a recently MFA-verified session; complete step-up verification and retry (ADR 045)"),
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
    super::compose_security::reject_legacy_sandbox_grants(settings.preset_config.as_ref())?;
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
        .update_project_settings_as(
            project_id,
            settings.clone().into(),
            // ADR 045: renaming onto or off a granted slug is admin-only and
            // step-up verified. Every other settings change, including on an
            // already-granted project, is neither.
            &crate::services::types::SlugClaimCaller::from_request(
                &auth,
                state.sensitive_action_authorizer.as_ref(),
            ),
        )
        .await
        .inspect_err(|error| log_reserved_slug_refusal(error, auth.user_id(), Some(project_id)))
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
        log_ingest_lines_per_sec: settings.log_ingest_lines_per_sec,
        log_ingest_kib_per_sec: settings.log_ingest_kib_per_sec,
        serves_own_temps_api: settings.serves_own_temps_api,
        shield_mode: (settings.shield_mode.is_some() || settings.attack_mode.is_some())
            .then(|| temps_shield::ShieldMode::parse(&update.project.shield_mode))
            .flatten(),
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
        .update_automatic_deploy(
            project_id,
            request.automatic_deploy,
            temps_core::docker_socket_grant::DeployCaller::from_instance_admin(
                auth.is_instance_admin(),
            ),
        )
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
    super::compose_security::reject_legacy_sandbox_grants(settings.preset_config.as_ref())?;
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
            settings.pull_only_root_directory,
            // ADR 045: repointing a declared project's repository and pushing
            // runs the caller's source as host root, without any HTTP deploy
            // request for the deploy guard to refuse.
            temps_core::docker_socket_grant::DeployCaller::from_instance_admin(
                auth.is_instance_admin(),
            ),
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
        hosting: None,
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
            hosting: None,
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

/// Install or reinstall the GitHub repository webhook for a project
///
/// For projects on a GitHub connection that is not a GitHub App (personal
/// access token or OAuth). Removes the existing repository webhook (if any)
/// and registers a fresh one with a new signing secret. Use this when the
/// webhook could not be registered automatically (the token lacks webhook
/// permission) or was deleted on GitHub, and pushes have stopped deploying.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/github/reinstall-webhook",
    tag = "Projects",
    responses(
        (status = 200, description = "Webhook installed", body = ReinstallGithubWebhookResponse),
        (status = 400, description = "Project is not on a GitHub PAT/OAuth connection, or GitHub refused to create the webhook"),
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
pub async fn reinstall_github_webhook(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    info!("Reinstalling GitHub webhook for project: {}", project_id);

    let hook_id = state
        .project_service
        .reinstall_github_webhook(project_id)
        .await
        .map_err(|e| {
            error!(
                "Error reinstalling GitHub webhook for project {}: {:?}",
                project_id, e
            );
            Problem::from(e)
        })?;

    let audit_event = ProjectUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.to_string()),
            user_agent: metadata.user_agent,
        },
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
            hosting: None,
        },
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit_event).await {
        error!(
            "Failed to create audit log for GitHub webhook reinstall on project {}: {:?}",
            project_id, e
        );
    }

    Ok(Json(ReinstallGithubWebhookResponse {
        hook_id,
        message: "GitHub webhook installed successfully".to_string(),
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

    if config.health.is_some() && auth.is_instance_admin() {
        let project = state.project_service.get_project(project_id).await?;
        if temps_core::docker_socket_grant::process_grant().declares(&project.slug) {
            temps_auth::require_sensitive_action(
                state.sensitive_action_authorizer.as_ref(),
                &auth,
                temps_core::SensitiveAction::ChangeDockerSocketHealth { project_id },
            )
            .await?;
        }
    }

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
            temps_core::docker_socket_grant::DeployCaller::from_instance_admin(
                auth.is_instance_admin(),
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
    if config.build_location.is_some() {
        updated_fields.insert("build_location".to_string(), "updated".to_string());
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
    if config.health.is_some() {
        updated_fields.insert("health".to_string(), "updated".to_string());
    }
    if config.health_check_timeout_seconds.is_some() {
        updated_fields.insert(
            "health_check_timeout_seconds".to_string(),
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
    // Compare this service with the same service of the latest release.
    let latest = latest.map(|latest| same_service_release(latest, &applied));
    let (latest, catalog_error) = match latest {
        Some(latest) if latest.spec().is_none() => (
            None,
            Some(format!(
                "The latest {} release no longer has a '{}' service.",
                latest.slug,
                applied.service.as_deref().unwrap_or_default()
            )),
        ),
        latest => (latest, catalog_error),
    };
    // A one-service app cannot be moved onto a release that has several
    // services from here: that is the whole-app upgrade of a project.
    let (latest, catalog_error) = match latest {
        Some(latest)
            if latest.template.is_multi_service() && !applied.template.is_multi_service() =>
        {
            let detail = single_to_multi_service_upgrade_detail(project_id, &latest);
            (Some(latest), Some(detail))
        }
        other => (other, catalog_error),
    };
    let catalog_drift = latest.as_ref().is_some_and(|latest| {
        latest.version == applied.version && latest.template != applied.template
    });
    let upgrade_available = latest.as_ref().is_some_and(|latest| {
        !latest.template.is_multi_service() && latest.is_newer_than(&applied).unwrap_or(false)
    });
    let changes = latest
        .as_ref()
        .map(|latest| service_release_changes(&applied, latest))
        .unwrap_or_default();
    // The same variable set creation and the upgrade itself use, so a newly
    // introduced shared variable is reported as required configuration.
    let required_configuration = latest
        .as_ref()
        .map(|latest| {
            missing_required_template_configuration(&latest.service_variables(), &configured_names)
        })
        .unwrap_or_default();
    let missing_services = latest
        .as_ref()
        .map(|latest| {
            release_spec(latest)
                .databases
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
    if applied.template.is_multi_service() {
        return Err(temps_core::error_builder::conflict()
            .title("Upgrade The Whole App")
            .detail(format!(
                "This service is part of {}, which has several services that must stay on the same release. Upgrade {} from its project instead.",
                applied.template.name, applied.template.name
            ))
            .build());
    }
    if target.template.is_multi_service() {
        return Err(temps_core::error_builder::conflict()
            .title("Upgrade Needs The Whole App")
            .detail(single_to_multi_service_upgrade_detail(project_id, &target))
            .build());
    }
    let target = same_service_release(target, &applied);
    let Some(target_spec) = target.spec().cloned() else {
        return Err(temps_core::error_builder::bad_request()
            .title("Service Removed From Template")
            .detail(format!(
                "The latest {} release no longer has a '{}' service",
                target.slug,
                applied.service.as_deref().unwrap_or_default()
            ))
            .build());
    };
    let previous_template_version = applied.version.clone();
    let previous_template_image = release_spec(&applied).image.clone();
    let target_template_version = target.version.clone();
    let target_template_image = target_spec.image.clone();
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
    let missing_services = target_spec
        .databases
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
    // The variables this service is configured with: its own plus, for a
    // one-service template, the template's shared ones. Creation and
    // `upgrade_service_template` resolve exactly this set.
    let target_variables = target.service_variables();
    let generated_app_url = if upgrade_needs_generated_app_url(
        &target_variables,
        &request.environment_variables,
        &configured_names,
    ) {
        Some(canonical_template_app_url(state.as_ref(), &project.slug).await?)
    } else {
        None
    };
    let new_environment_variables = canonicalize_template_upgrade_environment_variables(
        &target_variables,
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
            // ADR 045: see `update_service_template_runtime` — this is the
            // other write that changes what a declared project runs.
            temps_core::docker_socket_grant::DeployCaller::from_instance_admin(
                auth.is_instance_admin(),
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
            // ADR 045: the persisted command this writes becomes the default
            // for every deploy that does not override it, so on a project this
            // control plane declares it decides what runs as host root — and
            // the deployment executing it may well be somebody else's.
            temps_core::docker_socket_grant::DeployCaller::from_instance_admin(
                auth.is_instance_admin(),
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
    if let Some(reason) = project.hosting.deploy_refusal(&project.name) {
        return Err(
            temps_core::error_builder::ErrorBuilder::new(StatusCode::CONFLICT)
                .type_("https://temps.sh/probs/project-hosted-externally")
                .title("Project Runs Outside Temps")
                .detail(reason)
                .build(),
        );
    }

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
        // ADR 045: deploying a project that holds host Docker access runs the
        // built image as host root, so it is admin-only regardless of who may
        // otherwise deploy this project.
        .trigger_pipeline_as(
            id,
            environment_id,
            payload.branch,
            payload.tag,
            payload.commit,
            temps_core::docker_socket_grant::DeployCaller::from_instance_admin(
                auth.is_instance_admin(),
            ),
            auth.has_permission(&Permission::GitRepositoriesRead)
                .then(|| auth.user_id_opt())
                .flatten(),
        )
        .await
        .map_err(|e| {
            let message = format!(
                "Error triggering pipeline for project {} environment {}: {:?}",
                id, environment_id, e
            );
            let problem = Problem::from(e);
            // A request the user can fix (no repository, unknown branch) is
            // not a server fault and must not be logged as one.
            if problem.status_code.is_server_error() {
                error!("{message}");
            } else {
                warn!("{message}");
            }
            problem
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
        has_completed_deployment: statistics.has_completed_deployment,
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
        (status = 403, description = "Insufficient permissions, or the slug is reserved for host Docker access and only an instance admin may claim it (ADR 045)"),
        (status = 404, description = "Template not found"),
        (status = 428, description = "The reserved slug requires a recently MFA-verified session; complete step-up verification and retry (ADR 045)"),
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
    if template.is_multi_service() {
        return Err(temps_core::error_builder::bad_request()
            .title("Template Has Several Services")
            .detail(format!(
                "'{}' creates {} services; create it with POST /templates/{}/instantiate, which puts them in one project",
                template.name,
                template.services.len(),
                template.slug
            ))
            .build());
    }
    let service = template.primary().clone();
    let source = service.source(&template.git);
    super::compose_security::reject_legacy_sandbox_grants(service.preset_config.as_ref())?;
    let release = if template.kind == temps_core::templates::TemplateKind::App {
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
    let template_provenance = release
        .as_ref()
        .map(|instance| instance.slug.clone())
        .unwrap_or_else(|| temps_core::templates::template_provenance(&template).to_string());
    let runtime_overrides = TemplateRuntimeOverrides::from(&request);
    resolve_image_template_runtime(&service, &runtime_overrides).map_err(|error| {
        temps_core::error_builder::bad_request()
            .title("Invalid Template Runtime Configuration")
            .detail(error.to_string())
            .build()
    })?;
    // A one-service template's shared variables belong to its only service.
    let variable_templates = template
        .shared_env_vars
        .iter()
        .chain(service.env_vars.iter())
        .cloned()
        .collect::<Vec<_>>();
    // Fork mode creates and populates a remote repository below, which cannot
    // be undone and would block a corrected retry under the same name. Reject
    // missing, duplicate or unknown variables before that side effect.
    crate::template_creation::validate_template_variables(
        &variable_templates,
        &request.environment_variables,
    )?;
    let planned_project_slug = state
        .project_service
        .plan_project_slug(&request.project_name)
        .await
        .map_err(Problem::from)?;

    // ADR 045: same claim rule as plain project creation — a template deploy
    // is another way to name a slug. Built here, ahead of the fork-mode
    // repository creation below, so it can pre-flight-check authorization
    // and step-up before that irreversible external side effect — and then
    // reused for the authoritative check at creation time.
    let slug_claim_caller = crate::services::types::SlugClaimCaller::from_request(
        &auth,
        state.sensitive_action_authorizer.as_ref(),
    );
    state
        .project_service
        .preflight_guard_reserved_slug(&planned_project_slug, &slug_claim_caller)
        .await
        .inspect_err(|error| log_reserved_slug_refusal(error, auth.user_id(), None))
        .map_err(Problem::from)?;

    // The browser normally enforces this selection, but the API must fail
    // before creating a project when a required managed dependency is absent
    // or the caller supplied a service of the wrong type.
    let mut selected_service_types = Vec::with_capacity(request.storage_service_ids.len());
    for service_id in &request.storage_service_ids {
        let selected = state
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
        selected_service_types.push(selected.service_type.to_ascii_lowercase());
    }
    if let Err(error) =
        validate_template_service_selection(&service.databases, &selected_service_types)
    {
        return Err(template_service_selection_problem(&template.name, error));
    }

    // 2. Resolve where the service builds from:
    //      * "image"       — the template carries a prebuilt image: create a
    //        docker_image project and pull/run the image (instant, no build).
    //        Wins over any Git connection — fastest activation path.
    //      * "fork"        — a Git connection is supplied: fork the template
    //        into the user's account and build from the fork.
    //      * "public_repo" — no connection: build straight from the template's
    //        public source repository.
    let (repository, repository_url, deploy_mode) = if service.is_image() {
        info!(
            "Deploying template {} from its prebuilt image (image mode)",
            request.template_slug
        );
        // The image ref isn't a browsable URL; surface the source repository.
        (None, source.url.clone(), "image")
    } else if let Some(connection_id) = request.git_provider_connection_id {
        // Fork mode requires a repository name to create under the account.
        let Some(repository_name) = request.repository_name.as_deref().filter(|s| !s.is_empty())
        else {
            return Err(temps_core::error_builder::bad_request()
                .title("Repository Name Required")
                .detail("repository_name is required when a Git provider connection is supplied")
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
                &source.url,
                &source.r#ref,
                source.path.as_deref(),
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
        // The template subfolder has been flattened into the fork root.
        (
            Some(crate::template_creation::RepositoryTarget {
                repo_owner: new_repo.owner.clone(),
                repo_name: new_repo.name.clone(),
                branch: new_repo.default_branch.clone(),
                git_url: Some(new_repo.clone_url.clone()),
                git_provider_connection_id: Some(connection_id),
                is_public: !new_repo.private,
                directory: ".".to_string(),
                automatic_deploy: request.automatic_deploy,
            }),
            new_repo.clone_url,
            "fork",
        )
    } else {
        // One-click public-repo mode: no fork, no Git account. The whole public
        // repo is cloned, so the build directory is the template's subfolder.
        info!(
            "Deploying template {} directly from public repo {} (one-click mode)",
            request.template_slug, source.url
        );
        (
            Some(crate::template_creation::public_repository_target(&source)),
            source.url.clone(),
            "public_repo",
        )
    };

    // 3. Create the project. A one-service template's shared variables belong
    //    to its only service.
    let caller = crate::template_creation::TemplateCreationCaller {
        auth: &auth,
        slug_claim: &slug_claim_caller,
        ip_address: Some(metadata.ip_address.to_string()),
        user_agent: metadata.user_agent.clone(),
    };
    let created = template_creator(state.as_ref())
        .create(
            crate::template_creation::TemplateServiceRequest {
                variable_templates,
                template: template.clone(),
                service: service.clone(),
                release,
                project_name: request.project_name.clone(),
                project_slug: planned_project_slug,
                provenance: Some(template_provenance),
                variables: request.environment_variables.clone(),
                storage_service_ids: request.storage_service_ids.clone(),
                storage_service_claim_ids,
                repository,
                runtime_overrides,
                defer_initial_deployment: false,
                instantiation_token: None,
                deploy_mode,
                cloudflare_enabled: request.cloudflare_enabled,
                delivery_provider: request.delivery_provider.clone(),
            },
            &caller,
        )
        .await?;

    // 4. Return the response with the source/repository URL.
    let deploy_note = match deploy_mode {
        "image" if created.deployment_queued == Some(true) => {
            "Initial deployment queued from the template's prebuilt image (no build)."
        }
        "image" => {
            "Project created from the template's prebuilt image; deployment requires a retry."
        }
        "fork" => "Repository created and initialized with template code.",
        _ => "Deployed directly from the template's public source repository.",
    };
    let response = super::templates::CreateProjectFromTemplateResponse {
        project_id: created.project.id,
        project_slug: created.project.slug,
        project_name: created.project.name,
        repository_url,
        template_slug: request.template_slug,
        message: format!(
            "Project created successfully from template '{}'. {} Services required: {:?}",
            template.name, deploy_note, template.databases
        ),
        deployment_queued: created.deployment_queued,
        deployment_error: created.deployment_error,
    };

    Ok((StatusCode::CREATED, Json(response)))
}

/// The creator used by both template endpoints.
fn template_creator(state: &AppState) -> crate::template_creation::TemplateServiceCreator {
    crate::template_creation::TemplateServiceCreator {
        project_service: state.project_service.clone(),
        config_service: state.config_service.clone(),
        public_hostname_resolver: state.public_hostname_resolver.clone(),
        audit_service: state.audit_service.clone(),
        telemetry: state.telemetry.clone(),
    }
}

fn template_service_selection_problem(
    template_name: &str,
    error: TemplateServiceSelectionError,
) -> Problem {
    let (title, detail) = match error {
        TemplateServiceSelectionError::Missing(services) => (
            "Managed Service Required",
            format!(
                "Template '{}' requires a linked {} service before it can be deployed",
                template_name,
                services.join(", ")
            ),
        ),
        TemplateServiceSelectionError::Ambiguous(services) => (
            "Ambiguous Managed Service Selection",
            format!(
                "Template '{}' accepts exactly one linked {} service",
                template_name,
                services.join(", ")
            ),
        ),
    };
    temps_core::error_builder::bad_request()
        .title(title)
        .detail(detail)
        .build()
}

#[cfg(test)]
mod tests {
    use super::{
        build_group_filter, canonicalize_template_upgrade_environment_variables,
        compose_path_for_candidate, drop_inspection_candidates, drop_preset_candidate_from,
        inspect_zip_archive, inspect_zip_manifests, missing_required_template_configuration,
        no_deployable_project_problem, production_environment_variable_names,
        require_git_settings_permissions, require_template_creation_permissions,
        resolve_image_template_runtime, service_template_changes,
        validate_template_service_selection, DropPresetCandidate, TemplateEnvironmentError,
        TemplateRuntimeOverrides, TemplateServiceSelectionError, DROP_NO_PROJECT_PROBLEM_TYPE,
        MAX_DROP_CANDIDATES,
    };
    use crate::services::project::GroupFilter;
    use crate::template_creation::{
        authorize_storage_service_scopes, canonicalize_template_environment_variables,
        image_deployment_dispatch_feedback, image_template_preset_config,
        parse_owner_repo_from_git_url, project_created_from_template_telemetry_event,
        TemplateRuntimeOverrideError,
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
            .expect("Keycloak should be bundled")
            .primary()
            .clone();
        let mut target = applied.clone();
        target
            .database_bindings
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

    fn shared_variable(
        name: &str,
        required: bool,
        generator: Option<&str>,
    ) -> temps_core::templates::EnvVarTemplate {
        temps_core::templates::EnvVarTemplate {
            name: name.to_string(),
            example: None,
            default: None,
            description: None,
            required,
            secret: false,
            default_generator: generator.map(str::to_string),
        }
    }

    fn keycloak_release() -> temps_core::templates::ServiceTemplateInstance {
        temps_core::templates::ServiceTemplateInstance::new(
            temps_core::templates::SERVICE_TEMPLATE_SCHEMA_VERSION,
            temps_core::templates::bundled_template_by_slug("keycloak")
                .expect("Keycloak should be bundled"),
        )
    }

    #[test]
    fn single_service_upgrade_resolves_newly_introduced_shared_variables() {
        let applied = keycloak_release();
        let mut target = applied.clone();
        target.version = "99.0.0".to_string();
        target.template.version = target.version.clone();
        target
            .template
            .shared_env_vars
            .push(shared_variable("NEW_SHARED_TOKEN", true, None));
        target.template.shared_env_vars.push(shared_variable(
            "NEW_SHARED_SECRET",
            true,
            Some("random_secret"),
        ));
        assert!(!target.template.is_multi_service());

        // The preview reports the shared variables the upgrade will ask for.
        let changes = super::service_release_changes(&applied, &target);
        assert!(
            changes
                .iter()
                .any(|change| change.field == "configuration.NEW_SHARED_TOKEN"),
            "{changes:?}"
        );
        let configured = BTreeSet::new();
        let required =
            missing_required_template_configuration(&target.service_variables(), &configured);
        assert!(required.iter().any(|v| v.name == "NEW_SHARED_TOKEN"));
        // A generator satisfies the requirement, so it is not asked for.
        assert!(!required.iter().any(|v| v.name == "NEW_SHARED_SECRET"));

        let supplied = [super::super::templates::EnvVarInput {
            name: "NEW_SHARED_TOKEN".to_string(),
            value: "operator-value".to_string(),
            is_secret: false,
        }];
        let variables = target.service_variables();

        // A supplied shared variable is accepted rather than rejected as
        // unknown, and defaults/generators are applied to the rest.
        let resolved = canonicalize_template_upgrade_environment_variables(
            &variables,
            &supplied,
            &BTreeSet::new(),
            None,
        )
        .expect("shared variables are part of the service's variables");
        let value_of = |name: &str| {
            resolved
                .iter()
                .find(|variable| variable.key == name)
                .map(|variable| variable.value.clone())
        };
        assert_eq!(
            value_of("NEW_SHARED_TOKEN").as_deref(),
            Some("operator-value")
        );
        assert!(value_of("NEW_SHARED_SECRET").is_some_and(|value| value.len() >= 32));

        // Omitting the required one fails required validation by name.
        assert!(matches!(
            canonicalize_template_upgrade_environment_variables(
                &variables,
                &[],
                &BTreeSet::new(),
                None,
            ),
            Err(TemplateEnvironmentError::MissingRequired { name }) if name == "NEW_SHARED_TOKEN"
        ));

        // The service-only set (what the endpoint used before) would have
        // rejected the same supplied value as unknown.
        assert!(matches!(
            canonicalize_template_upgrade_environment_variables(
                &target.spec().expect("service").env_vars,
                &supplied,
                &BTreeSet::new(),
                None,
            ),
            Err(TemplateEnvironmentError::Unknown { .. })
        ));
    }

    #[test]
    fn multi_service_releases_keep_shared_variables_on_the_project() {
        let mut release = keycloak_release();
        release
            .template
            .shared_env_vars
            .push(shared_variable("SHARED_ONLY", true, None));
        let mut second = release.template.services[0].clone();
        second.name = "worker".to_string();
        second.env_vars.clear();
        release.template.services.push(second);

        assert!(release.template.is_multi_service());
        assert!(!release
            .service_variables()
            .iter()
            .any(|variable| variable.name == "SHARED_ONLY"));
    }

    #[test]
    fn single_service_upgrade_to_a_multi_service_release_names_project_and_release() {
        let mut target = keycloak_release();
        target.version = "2.0.0".to_string();
        target.template.version = target.version.clone();
        let mut second = target.template.services[0].clone();
        second.name = "worker".to_string();
        target.template.services.push(second);

        let detail = super::single_to_multi_service_upgrade_detail(41, &target);
        assert!(detail.contains("Project 41"), "{detail}");
        assert!(detail.contains("keycloak@2.0.0"), "{detail}");
        assert!(detail.contains("whole app"), "{detail}");
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
            .expect("Browserless should be bundled")
            .primary()
            .clone();
        let resolved = canonicalize_template_environment_variables(
            &template.env_vars,
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
            .expect("Keycloak should be bundled")
            .primary()
            .clone();
        let requested = [super::super::templates::EnvVarInput {
            name: "KC_BOOTSTRAP_ADMIN_PASSWORD".to_string(),
            value: "operator-supplied".to_string(),
            is_secret: false,
        }];
        let resolved =
            canonicalize_template_environment_variables(&template.env_vars, &requested, None)
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
            .expect("Browserless should be bundled")
            .primary()
            .clone();
        let duplicate = super::super::templates::EnvVarInput {
            name: "TOKEN".to_string(),
            value: "one".to_string(),
            is_secret: true,
        };
        assert!(matches!(
            canonicalize_template_environment_variables(
                &template.env_vars,
                &[duplicate.clone(), duplicate],
                Some("https://browserless.example.test")
            ),
            Err(TemplateEnvironmentError::Duplicate { .. })
        ));
        assert!(matches!(
            canonicalize_template_environment_variables(&template.env_vars, &[], None),
            Err(TemplateEnvironmentError::MissingRequired { name }) if name == "EXTERNAL"
        ));
    }

    fn image_template_request() -> super::super::templates::CreateProjectFromTemplateRequest {
        super::super::templates::CreateProjectFromTemplateRequest {
            cloudflare_enabled: None,
            delivery_provider: None,
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
            .expect("Keycloak should be bundled")
            .primary()
            .clone();

        let runtime = resolve_image_template_runtime(
            &template,
            &TemplateRuntimeOverrides::from(&image_template_request()),
        )
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
            .expect("Keycloak should be bundled")
            .primary()
            .clone();
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

        let runtime =
            resolve_image_template_runtime(&template, &TemplateRuntimeOverrides::from(&request))
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
            .expect("Keycloak should be bundled")
            .primary()
            .clone();
        let mut request = image_template_request();
        request.health_check_path = Some("https://example.test/ready".to_string());

        assert!(matches!(
            resolve_image_template_runtime(&template, &TemplateRuntimeOverrides::from(&request)),
            Err(TemplateRuntimeOverrideError::InvalidHealthCheckPath { .. })
        ));
    }

    #[test]
    fn image_template_runtime_rejects_mutable_image_tags() {
        let template = temps_core::templates::bundled_template_by_slug("keycloak")
            .expect("Keycloak should be bundled")
            .primary()
            .clone();
        let mut request = image_template_request();
        request.image = Some("quay.io/keycloak/keycloak:latest".to_string());

        assert!(matches!(
            resolve_image_template_runtime(&template, &TemplateRuntimeOverrides::from(&request)),
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

    fn drop_test_zip(files: &[(&str, &str)]) -> tempfile::NamedTempFile {
        use std::io::Write;
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut zip = zip::ZipWriter::new(file.reopen().unwrap());
        for (name, contents) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(contents.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        file
    }

    #[test]
    fn drop_zip_inspection_reads_ruby_and_php_and_exposes_server_candidates() {
        for (manifest, content, slug, label) in [
            ("Gemfile", "gem 'rails'", "nixpacks-ruby", "Ruby"),
            (
                "apps/php/composer.json",
                r#"{"require":{"laravel/framework":"^12"}}"#,
                "nixpacks-php",
                "PHP",
            ),
        ] {
            let zip = drop_test_zip(&[(manifest, content)]);
            let manifests = inspect_zip_manifests(zip.path()).unwrap();
            assert_eq!(manifests[manifest], content);
            let candidate = temps_presets::detect_project_candidates(&manifests).remove(0);
            let response = drop_preset_candidate_from(&manifests, candidate);
            assert_eq!(response.preset, slug);
            assert_eq!(response.label, label);
            assert!(!response.is_static);
        }
        // Adding language manifests must not bypass the archive secret policy:
        // an environment file is never read or kept, and a key still refuses
        // the upload.
        let zip = drop_test_zip(&[("composer.json", "{}"), (".env", "APP_KEY=fixture-only")]);
        let manifests = inspect_zip_manifests(zip.path()).unwrap();
        assert!(!manifests.contains_key(".env"));
        let zip = drop_test_zip(&[("composer.json", "{}"), ("storage/oauth-private.key", "x")]);
        assert!(inspect_zip_manifests(zip.path()).is_err());
    }

    #[test]
    fn drop_offers_server_languages_that_ship_no_js_manifest() {
        // The file layouts of the official Deno, Phoenix, plain-PHP and
        // Vapor starters, zipped from the application root.
        for (files, slug, label) in [
            (
                vec![("main.ts", "Deno.serve(() => new Response('ok'));")],
                "nixpacks-deno",
                "Deno",
            ),
            (
                vec![
                    ("mix.exs", "defmodule Hello.MixProject do end"),
                    ("config/config.exs", "import Config"),
                    (
                        "lib/hello_web/router.ex",
                        "defmodule HelloWeb.Router do end",
                    ),
                ],
                "nixpacks-elixir",
                "Elixir",
            ),
            (
                vec![
                    ("index.php", "<?php echo 'ok';"),
                    ("nixpacks.toml", "[start]\ncmd = \"php -S 0.0.0.0:8000\""),
                ],
                "nixpacks-php",
                "PHP",
            ),
            (
                vec![
                    ("Package.swift", "// swift-tools-version:5.9"),
                    ("Sources/App/main.swift", "print(1)"),
                ],
                "nixpacks-swift",
                "Swift",
            ),
        ] {
            let zip = drop_test_zip(&files);
            let manifests = inspect_zip_manifests(zip.path()).unwrap();
            let candidates = drop_inspection_candidates(&manifests);
            assert_eq!(candidates.len(), 1, "{slug}: {candidates:?}");
            assert_eq!(candidates[0].directory, ".");
            assert_eq!(candidates[0].preset, slug);
            assert_eq!(candidates[0].label, label);
            assert!(
                !candidates[0].is_static,
                "{slug} must not be served as files"
            );
            // Recognising more files must not loosen the archive policy:
            // private keys still refuse the upload, and an environment file
            // is still never read or kept.
            let mut with_key = files.clone();
            with_key.push(("certs/server.key", "fixture-only"));
            assert!(inspect_zip_manifests(drop_test_zip(&with_key).path()).is_err());
            let mut with_env = files.clone();
            with_env.push((".env", "fixture-only"));
            let manifests = inspect_zip_manifests(drop_test_zip(&with_env).path()).unwrap();
            assert!(!manifests.contains_key(".env"));
        }
    }

    /// A Rust web app with a Dockerfile and a committed `.env.example` used to
    /// be refused as a whole ("Sensitive ZIP Entry ... '.env.example'").
    #[test]
    fn drop_keeps_environment_templates() {
        let zip = drop_test_zip(&[
            (".env.example", "DATABASE_URL=postgres://localhost/app"),
            (
                "Cargo.toml",
                "[package]\nname = \"app\"\nversion = \"0.1.0\"",
            ),
            ("Cargo.lock", "version = 3"),
            ("Dockerfile", "FROM rust:1\n"),
            ("src/main.rs", "fn main() {}"),
        ]);
        let inspection = inspect_zip_archive(zip.path()).unwrap();
        assert!(inspection.manifests.contains_key(".env.example"));
        assert!(inspection.skipped.is_empty());
        let candidates = drop_inspection_candidates(&inspection.manifests);
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.directory == "."),
            "{candidates:?}"
        );
    }

    /// Secrets and generated directories are left out and reported, the way
    /// a dropped folder already treats them, instead of refusing the ZIP.
    #[test]
    fn drop_leaves_out_env_files_and_generated_directories_and_reports_them() {
        let zip = drop_test_zip(&[
            ("package.json", "{\"name\":\"web\"}"),
            ("index.js", "console.log(1)"),
            (".env", "fixture-only"),
            ("config/.env.production", "fixture-only"),
            ("node_modules/left-pad/index.js", "module.exports = 1"),
            ("node_modules/left-pad/package.json", "{}"),
            (".git/config", "[core]"),
            ("node_modules/some-lib/test/fixture.pem", "fixture-only"),
        ]);
        let inspection = inspect_zip_archive(zip.path()).unwrap();
        for left_out in [".env", "config/.env.production", ".git/config"] {
            assert!(!inspection.manifests.contains_key(left_out), "{left_out}");
        }
        assert!(inspection
            .manifests
            .keys()
            .all(|path| !path.starts_with("node_modules/")));
        assert_eq!(inspection.skipped.total_files(), 6);
        let listed: Vec<(String, u64)> = inspection
            .skipped
            .paths()
            .into_iter()
            .map(|skipped| (skipped.path, skipped.files))
            .collect();
        assert_eq!(
            listed,
            vec![
                (".env".to_string(), 1),
                (".git/".to_string(), 1),
                ("config/.env.production".to_string(), 1),
                ("node_modules/".to_string(), 3),
            ]
        );
        assert_eq!(
            drop_inspection_candidates(&inspection.manifests)[0].directory,
            "."
        );
    }

    #[test]
    fn drop_refuses_private_material_and_names_the_entry() {
        let zip = drop_test_zip(&[("package.json", "{}"), ("certs/server.key", "fixture-only")]);
        let problem = inspect_zip_archive(zip.path()).err().unwrap();
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
        assert_eq!(problem.body["title"], "Sensitive ZIP Entry");
        assert_eq!(problem.body["entry"], "certs/server.key");
        let detail = problem.body["detail"].as_str().unwrap();
        assert!(detail.contains("'certs/server.key'"), "{detail}");
        assert!(detail.contains("private key"), "{detail}");
    }

    /// A legacy archive writes names in CP437 with no UTF-8 flag. zip decodes
    /// those names, so the classifier sees `café/server.key`; the diagnostic
    /// the console turns into "remove X from the archive" must say the same,
    /// not the replacement-character rendering of the raw bytes.
    #[test]
    fn drop_names_a_legacy_cp437_entry_with_its_decoded_name() {
        // `X` is patched to 0x82 (é in CP437) below. Same length, so offsets
        // and CRCs are untouched, and an ASCII name carries no UTF-8 flag.
        let zip = drop_test_zip(&[("package.json", "{}"), ("cafX/server.key", "fixture-only")]);
        let mut bytes = std::fs::read(zip.path()).unwrap();
        let mut patched = 0;
        while let Some(at) = bytes.windows(4).position(|window| window == b"cafX") {
            bytes[at + 3] = 0x82;
            patched += 1;
        }
        assert_eq!(patched, 2, "local header and central directory");
        std::fs::write(zip.path(), &bytes).unwrap();

        let problem = inspect_zip_archive(zip.path()).err().unwrap();
        assert_eq!(problem.body["title"], "Sensitive ZIP Entry");
        assert_eq!(problem.body["entry"], "caf\u{e9}/server.key");
        let detail = problem.body["detail"].as_str().unwrap();
        assert!(detail.contains("'caf\u{e9}/server.key'"), "{detail}");
    }

    #[test]
    fn drop_never_offers_a_static_deploy_that_would_publish_php_source() {
        let zip = drop_test_zip(&[
            ("index.html", "<!doctype html>"),
            ("api/index.php", "<?php echo getenv('DB_PASSWORD');"),
        ]);
        let manifests = inspect_zip_manifests(zip.path()).unwrap();
        let candidates = drop_inspection_candidates(&manifests);
        assert!(
            candidates.iter().all(|candidate| !candidate.is_static),
            "{candidates:?}"
        );
        assert_eq!(candidates[0].directory, ".");
        assert_eq!(candidates[0].preset, "nixpacks-php");
    }

    #[test]
    fn drop_without_a_project_says_what_to_add() {
        let zip = drop_test_zip(&[("notes.txt", "hello"), ("lib/util.ts", "export {}")]);
        let manifests = inspect_zip_manifests(zip.path()).unwrap();
        assert!(drop_inspection_candidates(&manifests).is_empty());

        let problem = no_deployable_project_problem();
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
        assert_eq!(
            problem.body.get("type").and_then(|value| value.as_str()),
            Some(DROP_NO_PROJECT_PROBLEM_TYPE)
        );
        let detail = problem.body["detail"].as_str().unwrap();
        assert!(detail.contains("Add a Dockerfile"), "{detail}");
        let supported: Vec<&str> = problem.body["supported_files"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|value| value.as_str())
            .collect();
        for expected in [
            "Dockerfile",
            "package.json",
            "mix.exs",
            "Package.swift",
            "deno.json",
            "index.php",
        ] {
            assert!(
                supported.contains(&expected),
                "{expected} missing: {supported:?}"
            );
        }
    }

    #[test]
    fn drop_zip_ignores_dependency_manifests_but_checks_their_paths_for_secrets() {
        let mut files = vec![("composer.json".to_string(), "{}".to_string())];
        for index in 0..513 {
            files.push((
                format!("vendor/package-{index}/composer.json"),
                "{}".to_string(),
            ));
        }
        // Neither an oversized dependency nor a deeply nested manifest is a
        // deployable root, so neither should consume the manifest-read budget.
        files.push((
            "vendor/large/composer.json".to_string(),
            "x".repeat(1024 * 1024 + 1),
        ));
        files.push((
            "a/b/c/d/e/composer.json".to_string(),
            "x".repeat(1024 * 1024 + 1),
        ));
        let borrowed: Vec<_> = files
            .iter()
            .map(|(name, contents)| (name.as_str(), contents.as_str()))
            .collect();
        let zip = drop_test_zip(&borrowed);
        let manifests = inspect_zip_manifests(zip.path()).unwrap();
        assert_eq!(manifests["composer.json"], "{}");
        assert!(manifests["vendor/large/composer.json"].is_empty());
        assert!(manifests["a/b/c/d/e/composer.json"].is_empty());
        let candidates = temps_presets::detect_project_candidates(&manifests);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].catalog_slug(), "nixpacks-php");
        let zip = drop_test_zip(&[
            ("composer.json", "{}"),
            ("vendor/private/.env", "fixture-only"),
            ("vendor/private/tls.key", "fixture-only"),
        ]);
        assert!(inspect_zip_manifests(zip.path()).is_err());
        // The size cap remains enforced for an actual application manifest.
        let oversized = "x".repeat(1024 * 1024 + 1);
        let zip = drop_test_zip(&[("composer.json", &oversized)]);
        assert!(inspect_zip_manifests(zip.path()).is_err());
    }

    #[test]
    fn drop_picker_preserves_roots_before_filling_alternative_preset_allowance() {
        for count in [26, 50, 51] {
            let mut manifests = BTreeMap::new();
            for index in 0..count {
                let root = format!("apps/app-{index:02}");
                manifests.insert(format!("{root}/composer.json"), "{}".to_string());
                manifests.insert(
                    format!("{root}/package.json"),
                    r#"{"devDependencies":{"vite":"7"}}"#.to_string(),
                );
                if count != 26 {
                    manifests.insert(format!("{root}/Gemfile"), "gem 'tooling'".to_string());
                }
            }
            let candidates = drop_inspection_candidates(&manifests);
            let roots: BTreeSet<_> = candidates
                .iter()
                .map(|candidate| candidate.directory.as_str())
                .collect();
            assert_eq!(roots.len(), count.min(50));
            assert_eq!(
                candidates.len(),
                if count == 26 { 52 } else { MAX_DROP_CANDIDATES }
            );
            for index in 0..count.min(50) {
                let root = format!("apps/app-{index:02}");
                assert!(candidates
                    .iter()
                    .any(|candidate| candidate.directory == root
                        && candidate.preset == "nixpacks-php"));
                assert!(candidates
                    .iter()
                    .any(|candidate| candidate.directory == root && candidate.preset == "vite"));
                if count != 26 {
                    assert!(candidates
                        .iter()
                        .any(|candidate| candidate.directory == root
                            && candidate.preset == "nixpacks-ruby"));
                }
            }
        }
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
            .expect("browserless template must be bundled")
            .primary()
            .clone();
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

        let missing = missing_required_template_configuration(&template.env_vars, &configured);
        assert_eq!(
            missing
                .iter()
                .map(|variable| variable.name.as_str())
                .collect::<Vec<_>>(),
            vec!["NEW_API_KEY"]
        );

        let resolved = canonicalize_template_upgrade_environment_variables(
            &template.env_vars,
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
            .expect("browserless template must be bundled")
            .primary()
            .clone();
        let configured = BTreeSet::from(["TOKEN".to_string()]);

        let overwrite = canonicalize_template_upgrade_environment_variables(
            &template.env_vars,
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
            &template.env_vars,
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

    // ── build_group_filter (R2.16) ────────────────────────────────────────────

    #[test]
    fn group_filter_none_when_neither_param_set() {
        assert!(
            build_group_filter(None, None).unwrap().is_none(),
            "no params => no filter"
        );
        assert!(
            build_group_filter(Some(false), None).unwrap().is_none(),
            "ungrouped=false => no filter"
        );
    }

    #[test]
    fn group_filter_ungrouped_when_flag_set() {
        let filter = build_group_filter(Some(true), None)
            .expect("ungrouped=true must be valid")
            .expect("must produce Some filter");
        assert!(
            matches!(filter, GroupFilter::Ungrouped),
            "ungrouped=true must produce GroupFilter::Ungrouped"
        );
    }

    #[test]
    fn group_filter_in_group_when_id_given() {
        let filter = build_group_filter(None, Some(7))
            .expect("project_group_id=7 must be valid")
            .expect("must produce Some filter");
        assert!(
            matches!(filter, GroupFilter::InGroup(7)),
            "project_group_id=7 must produce GroupFilter::InGroup(7)"
        );
    }

    #[test]
    fn group_filter_rejects_both_params() {
        let err =
            build_group_filter(Some(true), Some(7)).expect_err("both params must be rejected");
        assert_eq!(
            err.status_code,
            StatusCode::BAD_REQUEST,
            "mutual exclusion must produce 400 Bad Request"
        );
    }

    /// A client disconnect drops the handler future between teardown steps and
    /// leaves the project fenced but still listed and serving. The handler must
    /// therefore hand the whole sequence to a spawned task and only await it.
    #[test]
    fn delete_project_runs_teardown_on_a_detached_task() {
        let source = include_str!("handlers.rs");
        let start = source
            .find("pub async fn delete_project(")
            .expect("delete_project handler exists");
        let tail = &source[start..];
        let end = tail
            .find("/// Update project settings")
            .expect("end of delete_project handler");
        let handler = &tail[..end];

        assert!(
            handler.contains("tokio::spawn(run_project_teardown("),
            "teardown must run on a spawned task so a client disconnect cannot abort it"
        );
        for step in [
            "begin_project_deletion",
            "cancel_all_project_deployments",
            "cleanup_project_containers",
            "cleanup_project_archives",
            ".delete_project(",
        ] {
            assert!(
                !handler.contains(step),
                "`{step}` must run inside run_project_teardown, not in the request future"
            );
        }
    }

    #[tokio::test]
    async fn spawned_task_survives_the_awaiting_future_being_dropped() {
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let _ = done_tx.send(());
        });

        // Dropping the awaiting future is what a disconnect does to a handler.
        let waiter = tokio::spawn(async move {
            let _ = task.await;
        });
        waiter.abort();

        tokio::time::timeout(std::time::Duration::from_secs(2), done_rx)
            .await
            .expect("spawned work must finish")
            .expect("spawned work must signal completion");
    }
}
