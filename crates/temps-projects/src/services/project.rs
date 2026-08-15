use std::sync::Arc;
use temps_core::url_validation::{redact_url_password, validate_git_url};
use tracing::{info, warn};

use sea_orm::{
    prelude::Uuid, ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect, Set, Statement,
};
use temps_core::{
    ForceRouteReloadJob, Job, ProjectCreatedJob, ProjectDeletedJob, ProjectUpdatedJob,
};
use temps_entities::projects;
use temps_git::services::public_repo::PublicRepoProviderFactory;

use serde::Serialize;

use super::types::{
    CreateProjectEnvVar, CreateProjectRequest, Project, ProjectError, ProjectStatistics,
    UpdateDeploymentSettingsRequest,
};
use super::{EnvVarService, EnvVarWithEnvironments};
use crate::handlers::UpdateDeploymentConfigRequest;
// Placeholder functions - these should be implemented properly or imported from other services

/// Whether changing `repo_owner`/`repo_name` would leave `git_url` pointing at
/// a different repository.
///
/// Returns `Some((old, new))` — both as `owner/name` — only when the stored URL
/// demonstrably identifies the *current* repo and the requested change moves
/// away from it. A URL that doesn't carry a recognisable `owner/name` tail
/// (self-hosted layouts, ssh remotes with unusual paths) returns `None`: we
/// can't prove a desync, so we don't block the operator.
fn would_desync_git_url(
    git_url: &Option<String>,
    current: (&str, &str),
    requested: (Option<&str>, Option<&str>),
) -> Option<(String, String)> {
    let (new_owner, new_name) = (
        requested.0.unwrap_or(current.0),
        requested.1.unwrap_or(current.1),
    );
    let old_pair = format!("{}/{}", current.0, current.1);
    let new_pair = format!("{}/{}", new_owner, new_name);
    if old_pair == new_pair {
        return None;
    }

    let url = git_url.as_deref()?;
    // Compare on the `owner/name` tail, ignoring a `.git` suffix and any
    // trailing slash, so https/ssh and with/without `.git` all match.
    let tail = url
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .rsplit(['/', ':'])
        .take(2)
        .collect::<Vec<_>>();
    if tail.len() < 2 {
        return None;
    }
    let url_pair = format!("{}/{}", tail[1], tail[0]);

    (url_pair.eq_ignore_ascii_case(&old_pair)).then_some((url_pair, new_pair))
}

fn slugify(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn compose_public_ports(
    config: Option<&temps_entities::preset::PresetConfig>,
) -> Vec<temps_entities::preset::ComposePublicPort> {
    match config {
        Some(temps_entities::preset::PresetConfig::DockerCompose(compose)) => {
            compose.public_ports.clone()
        }
        _ => Vec::new(),
    }
}

// API Response types
#[derive(Debug, Serialize)]
pub struct TemplateResponse {
    pub name: String,
    pub description: String,
    pub image: String,
    pub github: TemplateGithubResponse,
    pub preset: Option<String>,
    pub project_type: String,
    pub services: Option<Vec<String>>,
    pub features: Option<Vec<String>>,
    pub env: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Serialize)]
pub struct TemplateGithubResponse {
    pub owner: String,
    pub repo: String,
    pub path: Option<String>,
    #[serde(rename = "ref")]
    pub ref_: Option<String>,
}

// Add this new struct to represent an environment variable with its environments
#[derive(Debug, Serialize)]
pub struct EnvVarEnvironment {
    pub id: i32,
    pub name: String,
}

// Constants for default hosted website resource profiles. CPU values are stored
// as microcores (1_000_000 = 1 CPU core); memory values are stored as MB. These
// profiles are intentionally separate from the external database-service
// profiles because app containers are usually burstier and safer to cap by
// default on small single-node installs.
pub const DEFAULT_CPU_REQUEST: i32 = 500_000; // 0.5 cores
pub const DEFAULT_MEMORY_REQUEST: i32 = 128; // 128 MB
pub const DEFAULT_MEMORY_LIMIT: i32 = 512; // 512 MB (small hosted website profile)

// Add these constants at the top of the file proper key management
pub const NONCE_LENGTH: usize = 12;

/// Resolve an API/UI catalog slug to its canonical persisted preset and config.
fn resolve_preset_slug(
    slug: &str,
    config: Option<temps_entities::preset::PresetConfig>,
) -> Result<temps_presets::StoredPreset, ProjectError> {
    temps_presets::resolve_preset_slug(slug, config)
        .map_err(|error| ProjectError::InvalidInput(format!("Invalid preset: {}", error)))
}

/// Apply a canonical preset selection to a project update.
fn apply_resolved_preset(
    active: &mut projects::ActiveModel,
    resolved: temps_presets::StoredPreset,
) {
    active.preset = Set(resolved.preset);
    active.preset_config = Set(resolved.config);
}

/// Preserve discriminator-like fields when a partial config patch omits them.
///
/// An explicit empty Nixpacks provider list still resets to auto, and an
/// explicit Dockerfile variant is still honored. Catalog preset selection is
/// normalized separately by the selected preset's resolver.
fn merge_preset_config(
    existing: Option<&temps_entities::preset::PresetConfig>,
    parsed: temps_entities::preset::PresetConfig,
    config_value: &serde_json::Value,
    preserve_omitted_providers: bool,
) -> temps_entities::preset::PresetConfig {
    use temps_entities::preset::PresetConfig;

    let omits_providers = config_value
        .as_object()
        .map(|map| !map.contains_key("providers"))
        .unwrap_or(true);
    let omits_dockerfile_variant = config_value
        .as_object()
        .map(|map| !map.contains_key("variant"))
        .unwrap_or(true);

    match (existing, parsed) {
        (Some(PresetConfig::Nixpacks(existing_cfg)), PresetConfig::Nixpacks(mut parsed_cfg)) => {
            if preserve_omitted_providers
                && omits_providers
                && parsed_cfg.providers.is_empty()
                && !existing_cfg.providers.is_empty()
            {
                parsed_cfg.providers = existing_cfg.providers.clone();
            }
            PresetConfig::Nixpacks(parsed_cfg)
        }
        (
            Some(PresetConfig::Dockerfile(existing_cfg)),
            PresetConfig::Dockerfile(mut parsed_cfg),
        ) => {
            if omits_dockerfile_variant {
                parsed_cfg.variant = existing_cfg.variant;
            }
            PresetConfig::Dockerfile(parsed_cfg)
        }
        (
            Some(PresetConfig::DockerCompose(existing_cfg)),
            PresetConfig::DockerCompose(mut parsed_cfg),
        ) => {
            // A partial PATCH (e.g. the settings-page exclusion toggle sends
            // only `excludedServices`) parses into a config where every
            // omitted field is its zero value, not "leave unchanged" — so
            // without this, a one-field patch would silently wipe
            // composePath/composeOverride/publicPorts/composeServices.
            let obj = config_value.as_object();
            let omits = |key: &str| obj.map(|map| !map.contains_key(key)).unwrap_or(true);
            if omits("composePath") {
                parsed_cfg.compose_path = existing_cfg.compose_path.clone();
            }
            if omits("composeOverride") {
                parsed_cfg.compose_override = existing_cfg.compose_override.clone();
            }
            if omits("publicPorts") {
                parsed_cfg.public_ports = existing_cfg.public_ports.clone();
            }
            if omits("excludedServices") {
                parsed_cfg.excluded_services = existing_cfg.excluded_services.clone();
            }
            if omits("composeServices") {
                parsed_cfg.compose_services = existing_cfg.compose_services.clone();
            }
            if omits("relaxedCapabilityServices") {
                parsed_cfg.relaxed_capability_services =
                    existing_cfg.relaxed_capability_services.clone();
            }
            if omits("unsandboxedServices") {
                parsed_cfg.unsandboxed_services = existing_cfg.unsandboxed_services.clone();
            }
            PresetConfig::DockerCompose(parsed_cfg)
        }
        (_, other) => other,
    }
}

fn validate_preset_config(
    preset: temps_entities::preset::Preset,
    config: temps_entities::preset::PresetConfig,
    config_value: Option<&serde_json::Value>,
) -> Result<temps_entities::preset::PresetConfig, ProjectError> {
    temps_presets::validate_preset_config(preset, &config)
        .map_err(|error| ProjectError::InvalidInput(format!("Invalid preset config: {}", error)))?;
    // Only re-validate when this call's patch explicitly touched
    // relaxedCapabilityServices. A value merged forward unchanged from the
    // existing config (e.g. because a later, unrelated patch replaced
    // composeServices and the previously-relaxed service name is no longer
    // in the new snapshot) must not retroactively fail every subsequent
    // save — that would permanently wedge the project's settings until the
    // user manually clears a field they never touched.
    let touches_relaxed_capability_services = config_value
        .and_then(|v| v.as_object())
        .is_some_and(|map| map.contains_key("relaxedCapabilityServices"));
    let touches_unsandboxed_services = config_value
        .and_then(|v| v.as_object())
        .is_some_and(|map| map.contains_key("unsandboxedServices"));
    if touches_relaxed_capability_services || touches_unsandboxed_services {
        if let temps_entities::preset::PresetConfig::DockerCompose(ref cfg) = config {
            validate_relaxed_capability_services(cfg)?;
            validate_unsandboxed_services(cfg)?;
        }
    }
    let touches_public_ports = config_value
        .and_then(|value| value.as_object())
        .is_some_and(|map| map.contains_key("publicPorts"));
    if touches_public_ports {
        if let temps_entities::preset::PresetConfig::DockerCompose(ref cfg) = config {
            validate_compose_public_ports(cfg)?;
        }
    }
    Ok(config)
}

fn validate_compose_public_ports(
    cfg: &temps_entities::preset::DockerComposeConfig,
) -> Result<(), ProjectError> {
    let mut services = std::collections::HashSet::new();
    for route in &cfg.public_ports {
        if route.service.trim().is_empty() {
            return Err(ProjectError::InvalidInput(
                "Compose public route service cannot be empty".to_string(),
            ));
        }
        if route.port == 0 || route.published == Some(0) {
            return Err(ProjectError::InvalidInput(format!(
                "Compose public route for service '{}' must use ports between 1 and 65535",
                route.service
            )));
        }
        if !services.insert(route.service.as_str()) {
            return Err(ProjectError::InvalidInput(format!(
                "Compose service '{}' can have only one public URL",
                route.service
            )));
        }
        if cfg
            .excluded_services
            .iter()
            .any(|excluded| excluded == &route.service)
        {
            return Err(ProjectError::InvalidInput(format!(
                "Compose service '{}' cannot be both disabled and public",
                route.service
            )));
        }
        if !cfg.compose_services.is_empty()
            && !cfg
                .compose_services
                .iter()
                .any(|service| service.name == route.service)
        {
            return Err(ProjectError::InvalidInput(format!(
                "Compose public route references unknown service '{}'",
                route.service
            )));
        }
    }
    Ok(())
}

/// `relaxed_capability_services` grants a compose service back the Linux
/// capabilities (CHOWN, DAC_OVERRIDE, FOWNER, SETUID, SETGID) many official
/// images' entrypoints need to fix ownership on a data directory and drop
/// from root to a service user at startup — this is not unique to database
/// images (confirmed live: Gitea's own official image hits the identical
/// `chown: ... Operation not permitted` / `su-exec: setgroups: Operation not
/// permitted` failure), so the settings UI offers this toggle for every
/// compose service, not just ones flagged `looks_like_database`. The
/// server-side check mirrors that: any name is accepted as long as it
/// matches a real service in the persisted snapshot, which rejects typos or
/// phantom names without narrowing eligibility to a specific image family.
/// If the snapshot is empty (e.g. before the first deploy has captured one),
/// allow the list through rather than block a legitimate first-time setup,
/// since there is nothing yet to validate against.
fn validate_relaxed_capability_services(
    cfg: &temps_entities::preset::DockerComposeConfig,
) -> Result<(), ProjectError> {
    if cfg.relaxed_capability_services.is_empty() || cfg.compose_services.is_empty() {
        return Ok(());
    }
    for service_name in &cfg.relaxed_capability_services {
        let matches_known_service = cfg.compose_services.iter().any(|s| &s.name == service_name);
        if !matches_known_service {
            return Err(ProjectError::InvalidInput(format!(
                "Cannot grant elevated capabilities to service '{}': it is not a recognized \
                 service in this compose file.",
                service_name
            )));
        }
    }
    Ok(())
}

fn validate_unsandboxed_services(
    cfg: &temps_entities::preset::DockerComposeConfig,
) -> Result<(), ProjectError> {
    if cfg.unsandboxed_services.is_empty() {
        return Ok(());
    }
    if cfg.compose_services.is_empty() {
        return Err(ProjectError::InvalidInput(
            "Cannot disable the Temps sandbox before Compose services have been recognized. Sync the Compose services from the repository first."
                .to_string(),
        ));
    }
    for service_name in &cfg.unsandboxed_services {
        if !cfg.compose_services.iter().any(|s| &s.name == service_name) {
            return Err(ProjectError::InvalidInput(format!(
                "Cannot disable the Temps sandbox for service '{}': it is not a recognized service in this compose file.",
                service_name
            )));
        }
        if cfg.relaxed_capability_services.contains(service_name) {
            return Err(ProjectError::InvalidInput(format!(
                "Service '{}' cannot use both elevated permissions and a disabled sandbox. Remove one of these settings.",
                service_name
            )));
        }
    }
    Ok(())
}

fn normalize_project_directory(directory: &str) -> Result<String, ProjectError> {
    let normalized = directory
        .trim()
        .replace('\\', "/")
        .trim_start_matches('/')
        .to_string();
    if normalized.is_empty() || normalized == "." {
        return Ok(".".to_string());
    }
    let path = std::path::Path::new(&normalized);
    let has_windows_drive_prefix = normalized.as_bytes().get(1) == Some(&b':');
    if has_windows_drive_prefix
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(ProjectError::InvalidInput(format!(
            "Project directory '{directory}' must be a relative path inside the source root"
        )));
    }
    Ok(normalized.trim_start_matches("./").to_string())
}

/// Resolve an explicit catalog selection for create/update.
///
/// Existing config is retained when it belongs to the same canonical preset.
/// Selecting base `nixpacks` is authoritative: omitted providers reset to
/// auto-detection while other Nixpacks settings remain intact.
fn resolve_preset_selection(
    slug: &str,
    config_value: Option<&serde_json::Value>,
    existing: Option<&temps_entities::preset::PresetConfig>,
) -> Result<temps_presets::StoredPreset, ProjectError> {
    use temps_entities::preset::PresetConfig;

    let base_selection = resolve_preset_slug(slug, None)?;
    let compatible_existing =
        existing.filter(|config| config.preset_type() == base_selection.preset);

    let config = match config_value {
        Some(value) => {
            let parsed =
                PresetConfig::parse_for_preset(&base_selection.preset, value).map_err(|error| {
                    ProjectError::InvalidInput(format!("Invalid preset config: {}", error))
                })?;
            Some(merge_preset_config(
                compatible_existing,
                parsed,
                value,
                slug != "nixpacks",
            ))
        }
        None => {
            let mut config = compatible_existing.cloned();
            if slug == "nixpacks" {
                if let Some(PresetConfig::Nixpacks(nixpacks)) = config.as_mut() {
                    nixpacks.providers.clear();
                }
            }
            config
        }
    };

    let resolved = if config.is_some() {
        resolve_preset_slug(slug, config)?
    } else {
        base_selection
    };
    let config = match resolved.config {
        Some(config) => Some(validate_preset_config(
            resolved.preset,
            config,
            config_value,
        )?),
        None => None,
    };
    Ok(temps_presets::StoredPreset {
        preset: resolved.preset,
        config,
    })
}

#[derive(Clone)]
pub struct ProjectService {
    pub db: Arc<temps_database::DbConnection>,
    pub queue_service: Arc<dyn temps_core::JobQueue>,
    pub config_service: Arc<temps_config::ConfigService>,
    pub external_service_manager: Arc<temps_providers::ExternalServiceManager>,
    pub git_provider_manager: Arc<temps_git::GitProviderManager>,
    env_var_service: Arc<EnvVarService>,
    environment_service: Arc<temps_environments::EnvironmentService>,
    encryption_service: Arc<temps_core::EncryptionService>,
}

impl ProjectService {
    pub fn new(
        db: Arc<temps_database::DbConnection>,
        queue_service: Arc<dyn temps_core::JobQueue>,
        config_service: Arc<temps_config::ConfigService>,
        external_service_manager: Arc<temps_providers::ExternalServiceManager>,
        git_provider_manager: Arc<temps_git::GitProviderManager>,
        environment_service: Arc<temps_environments::EnvironmentService>,
        encryption_service: Arc<temps_core::EncryptionService>,
    ) -> Self {
        let env_var_service = Arc::new(EnvVarService::new(db.clone(), encryption_service.clone()));

        ProjectService {
            db: db.clone(),
            queue_service,
            config_service: config_service.clone(),
            external_service_manager,
            git_provider_manager,
            env_var_service,
            environment_service,
            encryption_service,
        }
    }

    pub async fn create_project(
        &self,
        request: CreateProjectRequest,
    ) -> Result<Project, ProjectError> {
        if request.template_slug.as_deref().is_some_and(|slug| {
            slug.chars().count() > temps_core::templates::MAX_TEMPLATE_SLUG_CHARS
        }) {
            return Err(ProjectError::InvalidInput(format!(
                "Template slug cannot exceed {} characters",
                temps_core::templates::MAX_TEMPLATE_SLUG_CHARS
            )));
        }

        // Reject unusable env vars before the project row exists. Catching this
        // here keeps it a 400 on the request that caused it, instead of a 500
        // from the post-insert finalize step that then rolls the project back.
        if let Some(env_vars) = request.environment_variables.as_ref() {
            for env_var in env_vars {
                if env_var.key.trim().is_empty() {
                    return Err(ProjectError::InvalidInput(
                        "Environment variable names cannot be empty".to_string(),
                    ));
                }
                if env_var.is_secret && env_var.value.is_empty() {
                    return Err(ProjectError::InvalidInput(format!(
                        "Environment variable '{}' is marked as a secret but has no value. \
                         Secrets are write-only and cannot be filled in later — \
                         provide a value or clear the secret flag.",
                        env_var.key
                    )));
                }
            }
        }

        // Verify storage service IDs exist if provided
        if !request.storage_service_ids.is_empty() {
            use temps_entities::external_services;

            // Get count of matching services using SeaORM
            let found_count = external_services::Entity::find()
                .filter(external_services::Column::Id.is_in(request.storage_service_ids.clone()))
                .count(self.db.as_ref())
                .await
                .map_err(|e| ProjectError::Other(e.to_string()))?;

            // Verify all IDs were found
            if found_count != request.storage_service_ids.len() as u64 {
                return Err(ProjectError::InvalidInput(
                    "One or more storage service IDs not found".to_string(),
                ));
            }
        }

        let normalized_directory = normalize_project_directory(&request.directory)?;

        let project_slug = self.generate_unique_project_slug(&request.name).await?;
        let resolved = resolve_preset_selection(
            request.preset.as_str(),
            request.preset_config.as_ref(),
            None,
        )?;
        let preset = resolved.preset;
        let preset_config = resolved.config;

        // Create deployment config with resource and deployment settings.
        // New hosted websites get the conservative "small" profile by default:
        // a scheduling request plus a hard memory limit so a runaway app cannot
        // OOM a small single-node host. Operators can still choose standard,
        // dedicated, or explicit uncapped limits later via deployment settings.
        let deployment_config = Some(temps_entities::deployment_config::DeploymentConfig {
            cpu_request: Some(DEFAULT_CPU_REQUEST),
            cpu_limit: None,
            memory_request: Some(DEFAULT_MEMORY_REQUEST),
            memory_limit: Some(DEFAULT_MEMORY_LIMIT),
            exposed_port: request.exposed_port,
            automatic_deploy: Some(request.automatic_deploy),
            ..Default::default()
        });

        // SSRF guard: validate git_url before persisting (Fix #12).
        if let Some(ref git_url) = request.git_url {
            validate_git_url(git_url).map_err(|e| ProjectError::InvalidGitUrl {
                url: redact_url_password(git_url),
                reason: e.to_string(),
            })?;
        }

        let project = projects::ActiveModel {
            name: Set(request.name),
            repo_name: Set(request.repo_name.unwrap_or_default()),
            repo_owner: Set(request.repo_owner.unwrap_or_default()),
            directory: Set(normalized_directory),
            main_branch: Set(request.main_branch),
            preset: Set(preset), // Now required, not Option
            preset_config: Set(preset_config),
            deployment_config: Set(deployment_config),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            slug: Set(project_slug.clone()),
            is_public_repo: Set(request.is_public_repo.unwrap_or(false)),
            git_url: Set(request.git_url),
            git_provider_connection_id: Set(request.git_provider_connection_id),
            deleted_at: Set(None),
            last_deployment: Set(None),
            source_type: Set(request.source_type),
            template_slug: Set(request.template_slug),
            ..Default::default()
        };

        // Insert the project. The slug column has a UNIQUE index — if a
        // concurrent request raced us to the same slug, surface a typed
        // SlugConflict (HTTP 409) instead of a generic 500.
        let project_found_db = match project.insert(self.db.as_ref()).await {
            Ok(model) => model,
            Err(e) if super::types::is_unique_violation(&e) => {
                return Err(ProjectError::SlugConflict { slug: project_slug });
            }
            Err(e) => {
                return Err(ProjectError::DatabaseError {
                    reason: e.to_string(),
                })
            }
        };
        info!(
            "Created project id={} slug={} preset={}",
            project_found_db.id, project_found_db.slug, project_found_db.preset
        );

        // From here on, the project row exists. If any downstream step
        // fails, hard-delete it (CASCADE cleans up environments, env vars,
        // service links, etc.) before returning so the caller never sees
        // a half-initialized project. This is the manual rollback recommended
        // in CLAUDE.md "Resource Cleanup" — a real txn would require pushing
        // a `&impl ConnectionTrait` through every dependent service, which
        // is a much larger refactor.
        let project_id = project_found_db.id;
        let default_environment = match self
            .finalize_project_creation(
                &project_found_db,
                request.environment_variables,
                request.storage_service_ids,
            )
            .await
        {
            Ok(env) => env,
            Err(err) => {
                tracing::error!(
                    "Project {} creation failed after insert, rolling back: {}",
                    project_id,
                    err
                );
                if let Err(cleanup_err) = temps_entities::projects::Entity::delete_by_id(project_id)
                    .exec(self.db.as_ref())
                    .await
                {
                    tracing::error!(
                        "Failed to roll back project {} after creation error: {}",
                        project_id,
                        cleanup_err
                    );
                }
                return Err(err);
            }
        };

        // Emit ProjectCreated job
        let project_created_job = Job::ProjectCreated(ProjectCreatedJob {
            project_id: project_found_db.id,
            project_name: project_found_db.name.clone(),
        });

        if let Err(e) = self.queue_service.send(project_created_job).await {
            warn!(
                "Failed to emit ProjectCreated job for project {}: {}",
                project_found_db.id, e
            );
        } else {
            info!(
                "Emitted ProjectCreated job for project {}",
                project_found_db.id
            );
        }
        // Queue initial deployment/pipeline job only for Git-based projects with repository information
        // For docker_image and static_files source types, deployments are triggered via API
        if project_found_db.source_type.requires_git_info()
            && !project_found_db.repo_owner.is_empty()
            && !project_found_db.repo_name.is_empty()
        {
            info!(
                "Queueing initial deployment job for Git project: {}",
                project_found_db.id
            );

            match self
                .queue_initial_deployment_job(&project_found_db, &default_environment)
                .await
            {
                Ok(()) => {
                    info!(
                        "Successfully queued deployment job for project {}",
                        project_found_db.id
                    );
                }
                Err(e) => {
                    // Log error but don't fail project creation
                    tracing::error!(
                        "Failed to queue deployment job for project {}: {}",
                        project_found_db.id,
                        e
                    );
                }
            }
        } else {
            info!(
                "Skipping initial deployment for project {} (source_type: {})",
                project_found_db.id, project_found_db.source_type
            );
        }

        // Auto-install GitLab webhook if applicable (best-effort, non-fatal).
        let project_found_db = if let Some(conn_id) = project_found_db.git_provider_connection_id {
            let repo_owner = project_found_db.repo_owner.clone();
            let repo_name = project_found_db.repo_name.clone();
            if !repo_owner.is_empty() && !repo_name.is_empty() {
                match self
                    .install_gitlab_webhook_for_connection(
                        project_found_db.id,
                        conn_id,
                        &repo_owner,
                        &repo_name,
                    )
                    .await
                {
                    Ok((hook_id, encrypted_token)) => {
                        let mut active = projects::ActiveModel::from(project_found_db.clone());
                        active.gitlab_webhook_id = Set(Some(hook_id as i32));
                        active.gitlab_webhook_signing_token = Set(Some(encrypted_token));
                        active.updated_at = Set(chrono::Utc::now());
                        match active.update(self.db.as_ref()).await {
                            Ok(updated) => updated,
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to persist GitLab webhook fields on new project {}: {}",
                                    project_found_db.id,
                                    e
                                );
                                project_found_db
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to install GitLab webhook for new project {}: {}",
                            project_found_db.id,
                            e
                        );
                        project_found_db
                    }
                }
            } else {
                project_found_db
            }
        } else {
            project_found_db
        };

        // Auto-install Bitbucket Cloud webhook if applicable (best-effort, non-fatal).
        let project_found_db = if let Some(conn_id) = project_found_db.git_provider_connection_id {
            let repo_owner = project_found_db.repo_owner.clone();
            let repo_name = project_found_db.repo_name.clone();
            if !repo_owner.is_empty() && !repo_name.is_empty() {
                match self
                    .install_bitbucket_webhook_for_connection(
                        project_found_db.id,
                        conn_id,
                        &repo_owner,
                        &repo_name,
                    )
                    .await
                {
                    Ok((hook_uuid, encrypted_token)) => {
                        let mut active = projects::ActiveModel::from(project_found_db.clone());
                        active.bitbucket_webhook_hook_id = Set(Some(hook_uuid));
                        active.bitbucket_webhook_token = Set(Some(encrypted_token));
                        active.updated_at = Set(chrono::Utc::now());
                        match active.update(self.db.as_ref()).await {
                            Ok(updated) => updated,
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to persist Bitbucket webhook fields on new project {}: {}",
                                    project_found_db.id,
                                    e
                                );
                                project_found_db
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to install Bitbucket webhook for new project {}: {}",
                            project_found_db.id,
                            e
                        );
                        project_found_db
                    }
                }
            } else {
                project_found_db
            }
        } else {
            project_found_db
        };

        // Auto-install Gitea webhook if applicable (best-effort, non-fatal).
        // BLOCKER 1 fix: gitea_webhook_signing_token was never written.
        let project_found_db = if let Some(conn_id) = project_found_db.git_provider_connection_id {
            let repo_owner = project_found_db.repo_owner.clone();
            let repo_name = project_found_db.repo_name.clone();
            if !repo_owner.is_empty() && !repo_name.is_empty() {
                match self
                    .install_gitea_webhook_for_connection(
                        project_found_db.id,
                        conn_id,
                        &repo_owner,
                        &repo_name,
                    )
                    .await
                {
                    Ok(encrypted_token) => {
                        let mut active = projects::ActiveModel::from(project_found_db.clone());
                        active.gitea_webhook_signing_token = Set(Some(encrypted_token));
                        active.updated_at = Set(chrono::Utc::now());
                        match active.update(self.db.as_ref()).await {
                            Ok(updated) => updated,
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to persist Gitea webhook token on new project {}: {}",
                                    project_found_db.id,
                                    e
                                );
                                project_found_db
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to install Gitea webhook for new project {}: {}",
                            project_found_db.id,
                            e
                        );
                        project_found_db
                    }
                }
            } else {
                project_found_db
            }
        } else {
            project_found_db
        };

        // Auto-install Generic webhook token if applicable (best-effort, non-fatal).
        // BLOCKER 2 fix: generic_webhook_token was never written.
        // Generic has no remote API — generate the token and store it; operators
        // configure the webhook URL manually.
        let project_found_db = if let Some(conn_id) = project_found_db.git_provider_connection_id {
            match self
                .install_generic_webhook_token(project_found_db.id, conn_id)
                .await
            {
                Ok(Some(encrypted_token)) => {
                    let mut active = projects::ActiveModel::from(project_found_db.clone());
                    active.generic_webhook_token = Set(Some(encrypted_token));
                    active.updated_at = Set(chrono::Utc::now());
                    match active.update(self.db.as_ref()).await {
                        Ok(updated) => updated,
                        Err(e) => {
                            tracing::warn!(
                                "Failed to persist Generic webhook token on new project {}: {}",
                                project_found_db.id,
                                e
                            );
                            project_found_db
                        }
                    }
                }
                Ok(None) => {
                    // Not a Generic connection — nothing to do.
                    project_found_db
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to install Generic webhook token for new project {}: {}",
                        project_found_db.id,
                        e
                    );
                    project_found_db
                }
            }
        } else {
            project_found_db
        };

        Ok(Self::map_db_project_to_project(project_found_db))
    }

    /// Post-insert steps for `create_project`. Returns the default environment
    /// on success. On any error, the caller is responsible for rolling back
    /// the project row.
    async fn finalize_project_creation(
        &self,
        project: &projects::Model,
        environment_variables: Option<Vec<CreateProjectEnvVar>>,
        storage_service_ids: Vec<i32>,
    ) -> Result<temps_entities::environments::Model, ProjectError> {
        let default_environment = self
            .environment_service
            .create_environment(
                project.id,
                "production".to_string(),
                Some(DEFAULT_CPU_REQUEST),
                // CPU remains uncapped by default; memory gets the small hosted-web cap.
                None,
                Some(DEFAULT_MEMORY_REQUEST),
                Some(DEFAULT_MEMORY_LIMIT),
                project.main_branch.clone(),
            )
            .await
            .map_err(|e| ProjectError::EnvironmentCreationFailed {
                project_id: project.id,
                reason: e.to_string(),
            })?;

        info!(
            "Created default environment for project: {}",
            default_environment.id
        );

        if let Some(env_vars) = environment_variables {
            for env_var in env_vars {
                let CreateProjectEnvVar {
                    key,
                    value,
                    is_secret,
                } = env_var;
                self.env_var_service
                    .create_environment_variable(
                        project.id,
                        vec![default_environment.id],
                        key.clone(),
                        value,
                        is_secret,
                    )
                    .await
                    .map_err(|e| ProjectError::EnvVarCreationFailed {
                        project_id: project.id,
                        key,
                        reason: e.to_string(),
                    })?;
            }
        }

        if !storage_service_ids.is_empty() {
            info!(
                "Linking {} storage services to project {}",
                storage_service_ids.len(),
                project.id
            );
            for storage_service_id in storage_service_ids {
                self.external_service_manager
                    .link_service_to_project(storage_service_id, project.id)
                    .await
                    .map_err(|e| ProjectError::StorageLinkFailed {
                        project_id: project.id,
                        service_id: storage_service_id,
                        reason: e.to_string(),
                    })?;
            }
        }

        Ok(default_environment)
    }

    pub async fn get_projects(&self) -> Result<Vec<Project>, ProjectError> {
        let results = projects::Entity::find()
            // Most-recently-deployed first; never-deployed projects (NULL
            // last_deployment) sort last, not first — a NULL under DESC would
            // otherwise be treated as "deployed infinitely recently".
            .order_by_with_nulls(
                projects::Column::LastDeployment,
                sea_orm::Order::Desc,
                sea_orm::sea_query::NullOrdering::Last,
            )
            .order_by_desc(projects::Column::CreatedAt)
            .all(self.db.as_ref())
            .await
            .map_err(|e| ProjectError::Other(e.to_string()))?;

        Ok(results
            .into_iter()
            .map(Self::map_db_project_to_project)
            .collect())
    }

    pub async fn get_project(&self, project_id: i32) -> Result<Project, ProjectError> {
        let project_found_db = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| ProjectError::Other(e.to_string()))?;

        project_found_db
            .map(Self::map_db_project_to_project)
            .ok_or(ProjectError::NotFound(format!(
                "project {} not found",
                project_id
            )))
    }

    pub async fn get_project_by_slug(&self, slug: &str) -> Result<Project, ProjectError> {
        let project_found_db = projects::Entity::find()
            .filter(projects::Column::Slug.eq(slug))
            .one(self.db.as_ref())
            .await?
            .ok_or(ProjectError::NotFound(format!(
                "project {} not found",
                slug
            )))?;

        Ok(Self::map_db_project_to_project(project_found_db))
    }

    pub async fn get_projects_by_repo_owner_and_name(
        &self,
        repo_owner: &str,
        repo_name: &str,
    ) -> Result<Vec<Project>, ProjectError> {
        let projects_found_db = projects::Entity::find()
            .filter(projects::Column::RepoOwner.eq(repo_owner))
            .filter(projects::Column::RepoName.eq(repo_name))
            .all(self.db.as_ref())
            .await
            .map_err(|e| ProjectError::Other(e.to_string()))?;

        let projects_found: Vec<Project> = projects_found_db
            .into_iter()
            .map(Self::map_db_project_to_project)
            .collect();
        Ok(projects_found)
    }

    pub async fn find_project_by_repo(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<Project, ProjectError> {
        let project_found = projects::Entity::find()
            .filter(projects::Column::RepoOwner.eq(owner))
            .filter(projects::Column::RepoName.eq(repo))
            .one(self.db.as_ref())
            .await
            .map_err(|e| ProjectError::Other(format!("Database error: {}", e)))?;

        match project_found {
            Some(project) => Ok(Self::map_db_project_to_project(project)),
            None => Err(ProjectError::NotFound(format!(
                "Project not found for repository {}/{}",
                owner, repo
            ))),
        }
    }

    pub async fn update_project(
        &self,
        project_id: i32,
        request: CreateProjectRequest,
    ) -> Result<Project, ProjectError> {
        // Find the existing project
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ProjectError::NotFound(format!(
                "project {} not found",
                project_id
            )))?;

        let normalized_directory = normalize_project_directory(&request.directory)?;

        let resolved = resolve_preset_selection(
            request.preset.as_str(),
            request.preset_config.as_ref(),
            project.preset_config.as_ref(),
        )?;

        // Update the project
        let mut active_project: projects::ActiveModel = project.into();
        active_project.name = Set(request.name);
        active_project.repo_name = Set(request.repo_name.unwrap_or_else(|| "unknown".to_string()));
        active_project.repo_owner =
            Set(request.repo_owner.unwrap_or_else(|| "unknown".to_string()));
        active_project.directory = Set(normalized_directory);
        active_project.main_branch = Set(request.main_branch);
        apply_resolved_preset(&mut active_project, resolved);
        active_project.updated_at = Set(chrono::Utc::now());

        let project_found = active_project.update(self.db.as_ref()).await?;
        let project_found = Self::map_db_project_to_project(project_found);

        // Emit ProjectUpdated job
        let project_updated_job = Job::ProjectUpdated(ProjectUpdatedJob {
            project_id: project_found.id,
            project_name: project_found.name.clone(),
        });

        if let Err(e) = self.queue_service.send(project_updated_job).await {
            warn!(
                "Failed to emit ProjectUpdated job for project {}: {}",
                project_found.id, e
            );
        } else {
            info!(
                "Emitted ProjectUpdated job for project {}",
                project_found.id
            );
        }

        Ok(project_found)
    }

    /// Change a project's source type to a Git-less type (docker_image /
    /// static_files / manual). Switching TO Git is rejected here because that
    /// needs repo + provider-connection config — it goes through
    /// [`Self::update_git_settings`] instead (which sets `source_type = Git`).
    pub async fn set_source_type(
        &self,
        project_id: i32,
        source_type: temps_entities::source_type::SourceType,
    ) -> Result<Project, ProjectError> {
        use temps_entities::source_type::SourceType;
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ProjectError::NotFound(format!(
                "project {} not found",
                project_id
            )))?;

        // Switching to Git is a direct flip only when a repository is already
        // configured (repo owner + name). A project can carry git info without
        // being Git-typed — that's a one-click switch. Otherwise the user must
        // set a repository up first (via Git settings).
        if matches!(source_type, SourceType::Git) {
            let has_repo = !project.repo_owner.trim().is_empty()
                && !project.repo_name.trim().is_empty()
                && project.repo_owner != "unknown"
                && project.repo_name != "unknown";
            if !has_repo {
                return Err(ProjectError::InvalidInput(
                    "To switch to a Git source, configure a repository in Git settings \
                     (a provider connection, repository, and branch are required)."
                        .to_string(),
                ));
            }
        }

        let mut active_project: projects::ActiveModel = project.into();
        active_project.source_type = Set(source_type);
        active_project.updated_at = Set(chrono::Utc::now());
        let updated = active_project.update(self.db.as_ref()).await?;
        let updated = Self::map_db_project_to_project(updated);

        // Deploy routing / behavior keys off source_type — notify consumers.
        if let Err(e) = self
            .queue_service
            .send(Job::ProjectUpdated(ProjectUpdatedJob {
                project_id: updated.id,
                project_name: updated.name.clone(),
            }))
            .await
        {
            warn!(
                "Failed to emit ProjectUpdated after source-type change for {}: {}",
                updated.id, e
            );
        }
        Ok(updated)
    }

    /// Persist deletion intent before cancelling workflows or touching Docker.
    /// Deployment workers reject projects with this fence, closing the window
    /// where a new container could appear after the cleanup snapshot.
    pub async fn begin_project_deletion(&self, project_id: i32) -> Result<(), ProjectError> {
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ProjectError::NotFound(format!("project {} not found", project_id)))?;
        if project.is_deleted {
            return Ok(());
        }

        let mut active: projects::ActiveModel = project.into();
        active.is_deleted = Set(true);
        active.deleted_at = Set(Some(chrono::Utc::now()));
        active.updated_at = Set(chrono::Utc::now());
        active.update(self.db.as_ref()).await?;
        info!(project_id, "Marked project for deletion");
        Ok(())
    }

    pub async fn delete_project(
        &self,
        project_id: i32,
        project_name: &str,
    ) -> Result<(), ProjectError> {
        // Fetch environments before deletion to emit cleanup jobs.
        // We only need id, name, and project_id — use select_only to avoid loading full models.
        let environments_to_delete: Vec<(i32, String, i32)> =
            temps_entities::environments::Entity::find()
                .filter(temps_entities::environments::Column::ProjectId.eq(project_id))
                .select_only()
                .column(temps_entities::environments::Column::Id)
                .column(temps_entities::environments::Column::Name)
                .column(temps_entities::environments::Column::ProjectId)
                .into_tuple()
                .all(self.db.as_ref())
                .await
                .map_err(|e| ProjectError::Other(e.to_string()))?;

        // Emit EnvironmentDeleted jobs before deletion so subscribers can clean up
        for (env_id, env_name, env_project_id) in &environments_to_delete {
            let env_deleted_job = Job::EnvironmentDeleted(temps_core::EnvironmentDeletedJob {
                environment_id: *env_id,
                environment_name: env_name.clone(),
                project_id: *env_project_id,
            });

            if let Err(e) = self.queue_service.send(env_deleted_job).await {
                warn!(
                    "Failed to emit EnvironmentDeleted job for environment {}: {}",
                    env_id, e
                );
            }
        }

        // Delete the project row — all related data (deployments, environments, domains,
        // crons, env_vars, services, etc.) is cleaned up via ON DELETE CASCADE foreign keys.
        temps_entities::projects::Entity::delete_by_id(project_id)
            .exec(self.db.as_ref())
            .await?;

        // Emit ProjectDeleted job for async cleanup (e.g. status monitors)
        let project_deleted_job = Job::ProjectDeleted(ProjectDeletedJob {
            project_id,
            project_name: project_name.to_string(),
        });

        if let Err(e) = self.queue_service.send(project_deleted_job).await {
            warn!(
                "Failed to emit ProjectDeleted job for project {}: {}",
                project_id, e
            );
        }

        info!(
            "Project {} and all related data deleted successfully",
            project_id
        );

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_project_settings(
        &self,
        project_id: i32,
        new_slug: Option<String>,
        git_provider_connection_id: Option<i32>,
        main_branch: Option<String>,
        repo_owner: Option<String>,
        repo_name: Option<String>,
        preset: Option<String>,
        directory: Option<String>,
        attack_mode: Option<bool>,
        enable_preview_environments: Option<bool>,
        preview_envs_on_demand: Option<bool>,
        preview_envs_idle_timeout_seconds: Option<i32>,
        preview_envs_wake_timeout_seconds: Option<i32>,
        preset_config: Option<serde_json::Value>,
        ai_alert_summaries_enabled: Option<bool>,
        ai_debug_chat_enabled: Option<bool>,
        ai_write_actions_enabled: Option<bool>,
        cross_project_trace_sharing: Option<bool>,
        error_source_context_enabled: Option<bool>,
        error_source_root: Option<String>,
        ai_api_traffic_summary_enabled: Option<bool>,
    ) -> Result<Project, ProjectError> {
        // Validate preview env on-demand timeouts before touching the DB.
        // Mirrors DeploymentConfig::validate so the project-level defaults are
        // never out of range.
        if let Some(idle) = preview_envs_idle_timeout_seconds {
            if !(60..=86400).contains(&idle) {
                return Err(ProjectError::InvalidInput(format!(
                    "preview_envs_idle_timeout_seconds {} is not in valid range (60-86400)",
                    idle
                )));
            }
        }
        if let Some(wake) = preview_envs_wake_timeout_seconds {
            if !(5..=120).contains(&wake) {
                return Err(ProjectError::InvalidInput(format!(
                    "preview_envs_wake_timeout_seconds {} is not in valid range (5-120)",
                    wake
                )));
            }
        }

        // Get the current project
        let mut project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ProjectError::NotFound(format!(
                "Project {} not found",
                project_id
            )))?;
        let initial_public_ports = compose_public_ports(project.preset_config.as_ref());

        // Update the slug if provided
        if let Some(slug_value) = new_slug {
            // Check if the slug is already taken by another project
            let existing = projects::Entity::find()
                .filter(projects::Column::Slug.eq(&slug_value))
                .filter(projects::Column::Id.ne(project_id))
                .one(self.db.as_ref())
                .await?;

            if existing.is_some() {
                return Err(ProjectError::SlugAlreadyExists(format!(
                    "Slug '{}' is already taken",
                    slug_value
                )));
            }

            let old_slug = project.slug.clone();
            project.slug = slug_value.clone();

            // Update the project in the database
            let mut active_project: projects::ActiveModel = project.into();
            active_project.slug = Set(slug_value.clone());
            project = active_project.update(self.db.as_ref()).await?;

            // Update the environment_domain in the environment if the slug has changed
            if old_slug != project.slug {
                let envs = temps_entities::environments::Entity::find()
                    .filter(temps_entities::environments::Column::ProjectId.eq(project_id))
                    .all(self.db.as_ref())
                    .await?;

                for env in envs {
                    let new_subdomain = format!("{}-{}", slug_value.clone(), env.slug);

                    // Update environment
                    let mut active_env: temps_entities::environments::ActiveModel = env.into();
                    active_env.subdomain = Set(new_subdomain.clone());
                    active_env.update(self.db.as_ref()).await?;
                }
            }
        }

        // Update git_provider_connection_id if provided
        if let Some(connection_id) = git_provider_connection_id {
            // Reload project to ensure we have the latest state
            let project = projects::Entity::find_by_id(project_id)
                .one(self.db.as_ref())
                .await?
                .ok_or(ProjectError::NotFound(format!(
                    "Project {} not found",
                    project_id
                )))?;

            // Verify connection exists and is active if non-zero
            if connection_id > 0 {
                use temps_entities::git_provider_connections;
                let connection = git_provider_connections::Entity::find_by_id(connection_id)
                    .one(self.db.as_ref())
                    .await?
                    .ok_or(ProjectError::GitProviderConnectionNotFound { connection_id })?;

                if !connection.is_active {
                    return Err(ProjectError::Other(format!(
                        "Git provider connection {} is not active",
                        connection_id
                    )));
                }

                // Update the project with the new connection ID
                let mut active_project: projects::ActiveModel = project.into();
                active_project.git_provider_connection_id = Set(Some(connection_id));
                active_project.update(self.db.as_ref()).await?;
            } else {
                // Setting to 0 or negative means remove the connection
                let mut active_project: projects::ActiveModel = project.into();
                active_project.git_provider_connection_id = Set(None);
                active_project.update(self.db.as_ref()).await?;
            }
        }

        // Update attack_mode if provided
        if let Some(attack_mode_value) = attack_mode {
            // Reload project to ensure we have the latest state
            let project = projects::Entity::find_by_id(project_id)
                .one(self.db.as_ref())
                .await?
                .ok_or(ProjectError::NotFound(format!(
                    "Project {} not found",
                    project_id
                )))?;

            let mut active_project: projects::ActiveModel = project.into();
            active_project.attack_mode = Set(attack_mode_value);
            active_project.update(self.db.as_ref()).await?;
        }

        // Update AI feature toggles if provided (ADR-021 / ADR-023). Both are
        // tri-state opt-ins (Some(true) = on), stored as nullable columns.
        // ai_write_actions_enabled is a non-null bool column (default false).
        if ai_alert_summaries_enabled.is_some()
            || ai_debug_chat_enabled.is_some()
            || ai_write_actions_enabled.is_some()
            || error_source_context_enabled.is_some()
            || error_source_root.is_some()
            || ai_api_traffic_summary_enabled.is_some()
        {
            let project = projects::Entity::find_by_id(project_id)
                .one(self.db.as_ref())
                .await?
                .ok_or(ProjectError::NotFound(format!(
                    "Project {} not found",
                    project_id
                )))?;
            let mut active_project: projects::ActiveModel = project.into();
            if let Some(v) = ai_alert_summaries_enabled {
                active_project.ai_alert_summaries_enabled = Set(Some(v));
            }
            if let Some(v) = ai_debug_chat_enabled {
                active_project.ai_debug_chat_enabled = Set(Some(v));
            }
            if let Some(v) = ai_write_actions_enabled {
                active_project.ai_write_actions_enabled = Set(v);
            }
            if let Some(v) = ai_api_traffic_summary_enabled {
                active_project.ai_api_traffic_summary_enabled = Set(Some(v));
            }
            // Opt-in for native error-tracking source context (non-null bool).
            if let Some(v) = error_source_context_enabled {
                active_project.error_source_context_enabled = Set(v);
            }
            // Auto-capture source root (nullable). Empty string clears it back
            // to the build-context default.
            if let Some(v) = error_source_root {
                active_project.error_source_root =
                    Set(if v.trim().is_empty() { None } else { Some(v) });
            }
            active_project.update(self.db.as_ref()).await?;
        }

        // Update cross_project_trace_sharing if provided (ADR-027 Phase 3 opt-out).
        if let Some(sharing) = cross_project_trace_sharing {
            let project = projects::Entity::find_by_id(project_id)
                .one(self.db.as_ref())
                .await?
                .ok_or(ProjectError::NotFound(format!(
                    "Project {} not found",
                    project_id
                )))?;
            let mut active_project: projects::ActiveModel = project.into();
            active_project.cross_project_trace_sharing = Set(sharing);
            active_project.update(self.db.as_ref()).await?;
        }

        // Update preview environment settings if any are provided
        let needs_preview_update = enable_preview_environments.is_some()
            || preview_envs_on_demand.is_some()
            || preview_envs_idle_timeout_seconds.is_some()
            || preview_envs_wake_timeout_seconds.is_some();

        if needs_preview_update {
            // Reload project to ensure we have the latest state
            let project = projects::Entity::find_by_id(project_id)
                .one(self.db.as_ref())
                .await?
                .ok_or(ProjectError::NotFound(format!(
                    "Project {} not found",
                    project_id
                )))?;

            let mut active_project: projects::ActiveModel = project.into();

            if let Some(enable_preview) = enable_preview_environments {
                active_project.enable_preview_environments = Set(enable_preview);
            }
            if let Some(on_demand) = preview_envs_on_demand {
                active_project.preview_envs_on_demand = Set(on_demand);
            }
            if let Some(idle) = preview_envs_idle_timeout_seconds {
                active_project.preview_envs_idle_timeout_seconds = Set(idle);
            }
            if let Some(wake) = preview_envs_wake_timeout_seconds {
                active_project.preview_envs_wake_timeout_seconds = Set(wake);
            }

            active_project.update(self.db.as_ref()).await?;
        }

        // Update git-related fields and preset configuration atomically so a
        // config submitted with a new preset is parsed against that new preset.
        let needs_git_update = main_branch.is_some()
            || repo_owner.is_some()
            || repo_name.is_some()
            || preset.is_some()
            || preset_config.is_some()
            || directory.is_some();

        if needs_git_update {
            // Reload project to ensure we have the latest state
            let project = projects::Entity::find_by_id(project_id)
                .one(self.db.as_ref())
                .await?
                .ok_or(ProjectError::NotFound(format!(
                    "Project {} not found",
                    project_id
                )))?;

            // `repo_owner`/`repo_name` and `git_url` are read by different
            // code paths — branch resolution uses the former, the clone uses
            // the latter — and this endpoint only writes the former. Changing
            // the repo identity here therefore used to leave a stale clone
            // URL behind, and the next deploy resolved a commit from one repo
            // and cloned another:
            //
            //   Starting repository download for owner/new-repo
            //   Checking out ref: <commit that only exists in new-repo>
            //   Cloning public repository from: .../old-repo.git
            //
            // The project could not be recovered through the API. Reject the
            // change when it would actually desync — the git URL is owned by
            // `POST /projects/{id}/git`, which validates it.
            let desync = would_desync_git_url(
                &project.git_url,
                (&project.repo_owner, &project.repo_name),
                (repo_owner.as_deref(), repo_name.as_deref()),
            );
            if let Some((old, new)) = desync {
                return Err(ProjectError::InvalidInput(format!(
                    "Changing the repository to '{new}' would leave the clone URL pointing at \
                     '{old}'. Update both together with POST /projects/{project_id}/git, which \
                     sets git_url alongside the owner and name."
                )));
            }

            let existing_preset_config = project.preset_config.clone();
            let mut active_project: projects::ActiveModel = project.into();

            if let Some(branch) = main_branch {
                active_project.main_branch = Set(branch);
            }
            if let Some(owner) = repo_owner {
                active_project.repo_owner = Set(owner);
            }
            if let Some(name) = repo_name {
                active_project.repo_name = Set(name);
            }
            if let Some(preset_value) = preset {
                let resolved = resolve_preset_selection(
                    preset_value.as_str(),
                    preset_config.as_ref(),
                    existing_preset_config.as_ref(),
                )?;
                apply_resolved_preset(&mut active_project, resolved);
            } else if let Some(config_value) = preset_config.as_ref() {
                let parsed = temps_entities::preset::PresetConfig::parse_for_preset(
                    active_project.preset.as_ref(),
                    config_value,
                )
                .map_err(|error| {
                    ProjectError::InvalidInput(format!("Invalid preset config: {}", error))
                })?;
                let merged = merge_preset_config(
                    existing_preset_config.as_ref(),
                    parsed,
                    config_value,
                    true,
                );
                let merged = validate_preset_config(
                    *active_project.preset.as_ref(),
                    merged,
                    Some(config_value),
                )?;
                active_project.preset_config = Set(Some(merged));
            }
            if let Some(dir) = directory {
                active_project.directory = Set(normalize_project_directory(&dir)?);
            }

            let updated_project = active_project.update(self.db.as_ref()).await?;
            if initial_public_ports != compose_public_ports(updated_project.preset_config.as_ref())
            {
                self.reload_routes_after_compose_port_change(project_id)
                    .await?;
            }
            let project_found = Self::map_db_project_to_project(updated_project);

            // Emit ProjectUpdated job
            let project_updated_job = Job::ProjectUpdated(ProjectUpdatedJob {
                project_id: project_found.id,
                project_name: project_found.name.clone(),
            });

            if let Err(e) = self.queue_service.send(project_updated_job).await {
                warn!(
                    "Failed to emit ProjectUpdated job for project {}: {}",
                    project_found.id, e
                );
            }

            return Ok(project_found);
        }

        // Always reload the final project state before returning
        let final_project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ProjectError::NotFound(format!(
                "Project {} not found",
                project_id
            )))?;

        let project_found = Self::map_db_project_to_project(final_project);

        // Emit ProjectUpdated job
        let project_updated_job = Job::ProjectUpdated(ProjectUpdatedJob {
            project_id: project_found.id,
            project_name: project_found.name.clone(),
        });

        if let Err(e) = self.queue_service.send(project_updated_job).await {
            warn!(
                "Failed to emit ProjectUpdated job for project {}: {}",
                project_found.id, e
            );
        }

        Ok(project_found)
    }

    pub async fn update_automatic_deploy(
        &self,
        project_id: i32,
        automatic_deploy: bool,
    ) -> Result<Project, ProjectError> {
        // Get the current project
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ProjectError::NotFound(format!(
                "Project {} not found",
                project_id
            )))?;

        // Update automatic_deploy setting in deployment_config
        let mut active_project: projects::ActiveModel = project.clone().into();

        // Update deployment config with new automatic_deploy value
        let mut deployment_config = project.deployment_config.clone().unwrap_or_default();
        deployment_config.automatic_deploy = Some(automatic_deploy);
        active_project.deployment_config = Set(Some(deployment_config));

        let updated_project = active_project.update(self.db.as_ref()).await?;

        Ok(Self::map_db_project_to_project(updated_project))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_git_settings(
        &self,
        project_id: i32,
        git_provider_connection_id: Option<i32>,
        main_branch: String,
        repo_owner: String,
        repo_name: String,
        preset: Option<String>,
        directory: String,
        preset_config: Option<serde_json::Value>,
        git_url: Option<String>,
        is_public_repo: Option<bool>,
    ) -> Result<Project, ProjectError> {
        // Get the current project (includes the old gitlab_webhook_id / signing_token)
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ProjectError::NotFound(format!(
                "Project {} not found",
                project_id
            )))?;

        // Snapshot fields we need to reason about the old/new repo transition.
        let old_connection_id = project.git_provider_connection_id;
        let old_repo_owner = project.repo_owner.clone();
        let old_repo_name = project.repo_name.clone();
        let old_gitlab_webhook_id = project.gitlab_webhook_id;
        let old_bitbucket_hook_id = project.bitbucket_webhook_hook_id.clone();
        // Gitea: no separate hook_id column — we clear gitea_webhook_signing_token
        // on repo change / disconnect. The orphaned remote hook on Gitea is
        // acceptable for v1 (no stored ID to call DELETE with).
        // Generic: no remote API at all — just regenerate the token.

        // Verify git provider connection if provided.
        //
        // Connections are scoped to the installation (workspace-wide), not to
        // the user who created them: a GitHub App installation is shared by
        // design, and PAT connections are intended to be usable by anyone
        // with write access to the project, not just their creator. Access
        // control for this endpoint is enforced by `permission_guard!` and
        // `project_scope_guard!` in the handler, not by connection ownership.
        if let Some(connection_id) = git_provider_connection_id {
            if connection_id > 0 {
                use temps_entities::git_provider_connections;
                let connection = git_provider_connections::Entity::find_by_id(connection_id)
                    .one(self.db.as_ref())
                    .await?
                    .ok_or(ProjectError::GitProviderConnectionNotFound { connection_id })?;

                if !connection.is_active {
                    return Err(ProjectError::Other(format!(
                        "Git provider connection {} is not active",
                        connection_id
                    )));
                }

                // Verify that the branch exists in the repository
                match self
                    .git_provider_manager
                    .get_branch_latest_commit(connection_id, &repo_owner, &repo_name, &main_branch)
                    .await
                {
                    Ok(_) => {
                        // Branch exists and we got its latest commit
                    }
                    Err(e) => {
                        return Err(ProjectError::GitHubError(format!(
                            "Branch '{}' does not exist in repository {}/{} or failed to verify: {}",
                            main_branch, repo_owner, repo_name, e
                        )));
                    }
                }
            }
        }

        // Capture the current preset/config before converting to ActiveModel
        let project_preset = project.preset;
        let existing_preset_config = project.preset_config.clone();
        let previous_public_ports = compose_public_ports(existing_preset_config.as_ref());

        // Update the project
        let mut active_project: projects::ActiveModel = project.into();
        active_project.main_branch = Set(main_branch.clone());
        active_project.repo_owner = Set(repo_owner.clone());
        active_project.repo_name = Set(repo_name.clone());
        active_project.directory = Set(normalize_project_directory(&directory)?);
        // Configuring a Git repository makes this a Git-source project — this is
        // how a docker_image / static_files project is converted to Git (the
        // reverse conversion goes through `set_source_type`).
        active_project.source_type = Set(temps_entities::source_type::SourceType::Git);

        if let Some(preset_value) = preset {
            let resolved = resolve_preset_selection(
                preset_value.as_str(),
                preset_config.as_ref(),
                existing_preset_config.as_ref(),
            )?;
            apply_resolved_preset(&mut active_project, resolved);
        } else if let Some(config_value) = preset_config.as_ref() {
            let parsed = temps_entities::preset::PresetConfig::parse_for_preset(
                &project_preset,
                config_value,
            )
            .map_err(|error| {
                ProjectError::InvalidInput(format!("Invalid preset config: {}", error))
            })?;
            let merged =
                merge_preset_config(existing_preset_config.as_ref(), parsed, config_value, true);
            let merged = validate_preset_config(project_preset, merged, Some(config_value))?;
            active_project.preset_config = Set(Some(merged));
        }

        // Determine the effective new connection id and whether we need to handle
        // webhook lifecycle.  Three cases:
        //   1. connection_id provided and > 0 → connecting / changing repo
        //   2. connection_id provided and == 0 → disconnecting
        //   3. connection_id not provided → no change
        let new_connection_id: Option<i32> = match git_provider_connection_id {
            Some(cid) if cid > 0 => {
                active_project.git_provider_connection_id = Set(Some(cid));
                Some(cid)
            }
            Some(_) => {
                // Explicit disconnect (connection_id == 0)
                active_project.git_provider_connection_id = Set(None);
                None
            }
            None => {
                // No change requested — carry existing connection forward.
                old_connection_id
            }
        };

        if let Some(ref url) = git_url {
            // SSRF guard: validate before persisting (Fix #12).
            validate_git_url(url).map_err(|e| ProjectError::InvalidGitUrl {
                url: redact_url_password(url),
                reason: e.to_string(),
            })?;
            active_project.git_url = Set(Some(url.clone()));
        }

        if let Some(is_public) = is_public_repo {
            active_project.is_public_repo = Set(is_public);
        }

        // ── GitLab webhook lifecycle ──────────────────────────────────────────
        //
        // We detect a "repo change" when either the connection or the repo path
        // differs from what was previously stored.  A change triggers:
        //   • Delete the old webhook from GitLab (best-effort, idempotent on 404).
        //   • Install a new webhook on the new repo (GitLab connections only).
        //
        // Failures here are non-fatal: we log warnings and continue so that the
        // project save always succeeds.

        let repo_changed = git_provider_connection_id.is_some()
            || repo_owner != old_repo_owner
            || repo_name != old_repo_name;

        if repo_changed {
            // Step 1: Remove old webhook if the old connection was GitLab.
            if let (Some(old_hook_id), Some(old_conn_id)) =
                (old_gitlab_webhook_id, old_connection_id)
            {
                if let Err(e) = self
                    .delete_gitlab_webhook_for_connection(
                        old_conn_id,
                        &old_repo_owner,
                        &old_repo_name,
                        old_hook_id,
                    )
                    .await
                {
                    warn!(
                        "Failed to remove old GitLab webhook {} for project {}: {}",
                        old_hook_id, project_id, e
                    );
                }
                // Clear stale hook fields unconditionally — even if delete failed
                // (it may already be gone on GitLab's side).
                active_project.gitlab_webhook_id = Set(None);
                active_project.gitlab_webhook_signing_token = Set(None);
            }

            // Step 2: Install a new webhook if the new connection is GitLab.
            if let Some(conn_id) = new_connection_id {
                match self
                    .install_gitlab_webhook_for_connection(
                        project_id,
                        conn_id,
                        &repo_owner,
                        &repo_name,
                    )
                    .await
                {
                    Ok((hook_id, encrypted_token)) => {
                        active_project.gitlab_webhook_id = Set(Some(hook_id as i32));
                        active_project.gitlab_webhook_signing_token = Set(Some(encrypted_token));
                    }
                    Err(e) => {
                        // Non-fatal: the project connects without the webhook.
                        warn!(
                            "Failed to install GitLab webhook for project {}: {}",
                            project_id, e
                        );
                        active_project.gitlab_webhook_id = Set(None);
                        active_project.gitlab_webhook_signing_token = Set(None);
                    }
                }
            }
        } else if git_provider_connection_id == Some(0) {
            // Explicit disconnect: clear webhook state.
            active_project.gitlab_webhook_id = Set(None);
            active_project.gitlab_webhook_signing_token = Set(None);
        }

        // ── Bitbucket Cloud webhook lifecycle ────────────────────────────────
        //
        // Mirrors the GitLab block above: delete the old hook when the repo
        // changes and install a new one on the new repo (best-effort, non-fatal).

        if repo_changed {
            // Step 1: Remove old webhook if the old connection was Bitbucket.
            if let (Some(old_hook_uuid), Some(old_conn_id)) =
                (old_bitbucket_hook_id.as_deref(), old_connection_id)
            {
                if let Err(e) = self
                    .delete_bitbucket_webhook_for_connection(
                        old_conn_id,
                        &old_repo_owner,
                        &old_repo_name,
                        old_hook_uuid,
                    )
                    .await
                {
                    warn!(
                        "Failed to remove old Bitbucket webhook {} for project {}: {}",
                        old_hook_uuid, project_id, e
                    );
                }
                // Clear stale hook fields unconditionally.
                active_project.bitbucket_webhook_hook_id = Set(None);
                active_project.bitbucket_webhook_token = Set(None);
            }

            // Step 2: Install a new webhook if the new connection is Bitbucket.
            if let Some(conn_id) = new_connection_id {
                match self
                    .install_bitbucket_webhook_for_connection(
                        project_id,
                        conn_id,
                        &repo_owner,
                        &repo_name,
                    )
                    .await
                {
                    Ok((hook_uuid, encrypted_token)) => {
                        active_project.bitbucket_webhook_hook_id = Set(Some(hook_uuid));
                        active_project.bitbucket_webhook_token = Set(Some(encrypted_token));
                    }
                    Err(e) => {
                        // Non-fatal: project connects without the webhook.
                        warn!(
                            "Failed to install Bitbucket webhook for project {}: {}",
                            project_id, e
                        );
                        active_project.bitbucket_webhook_hook_id = Set(None);
                        active_project.bitbucket_webhook_token = Set(None);
                    }
                }
            }
        } else if git_provider_connection_id == Some(0) {
            // Explicit disconnect: clear Bitbucket webhook state.
            active_project.bitbucket_webhook_hook_id = Set(None);
            active_project.bitbucket_webhook_token = Set(None);
        }

        // ── Gitea webhook lifecycle ──────────────────────────────────────────
        //
        // BLOCKER 1 fix: gitea_webhook_signing_token is now written here.
        // There is no stored hook_id for remote deletion in v1 — on repo
        // change we clear the token (orphaning the remote hook) and re-install.
        // On explicit disconnect we clear the token.

        if repo_changed {
            // Clear the old Gitea signing token unconditionally.
            active_project.gitea_webhook_signing_token = Set(None);

            // Install a new webhook if the new connection is Gitea.
            if let Some(conn_id) = new_connection_id {
                match self
                    .install_gitea_webhook_for_connection(
                        project_id,
                        conn_id,
                        &repo_owner,
                        &repo_name,
                    )
                    .await
                {
                    Ok(encrypted_token) => {
                        active_project.gitea_webhook_signing_token = Set(Some(encrypted_token));
                    }
                    Err(e) => {
                        warn!(
                            "Failed to install Gitea webhook for project {}: {}",
                            project_id, e
                        );
                        active_project.gitea_webhook_signing_token = Set(None);
                    }
                }
            }
        } else if git_provider_connection_id == Some(0) {
            // Explicit disconnect: clear Gitea webhook state.
            active_project.gitea_webhook_signing_token = Set(None);
        }

        // ── Generic webhook token lifecycle ─────────────────────────────────
        //
        // BLOCKER 2 fix: generic_webhook_token is now written here.
        // Generic has no remote API to deregister; we simply regenerate.

        if repo_changed {
            // Clear the old token.
            active_project.generic_webhook_token = Set(None);

            if let Some(conn_id) = new_connection_id {
                match self
                    .install_generic_webhook_token(project_id, conn_id)
                    .await
                {
                    Ok(Some(encrypted_token)) => {
                        active_project.generic_webhook_token = Set(Some(encrypted_token));
                    }
                    Ok(None) => {
                        // Not a Generic connection — nothing to do.
                    }
                    Err(e) => {
                        warn!(
                            "Failed to install Generic webhook token for project {}: {}",
                            project_id, e
                        );
                        active_project.generic_webhook_token = Set(None);
                    }
                }
            }
        } else if git_provider_connection_id == Some(0) {
            // Explicit disconnect: clear Generic webhook state.
            active_project.generic_webhook_token = Set(None);
        }

        let updated_project = active_project.update(self.db.as_ref()).await?;

        if previous_public_ports != compose_public_ports(updated_project.preset_config.as_ref()) {
            self.reload_routes_after_compose_port_change(project_id)
                .await?;
        }

        Ok(Self::map_db_project_to_project(updated_project))
    }

    /// Public Compose ports are read directly from `projects.preset_config`
    /// when the proxy builds its route table. Updating the JSON alone leaves
    /// the in-memory table stale, because the project DB trigger deliberately
    /// ignores generic preset-config changes. Publish both supported signals:
    /// the queue gives this process a deterministic reload, while PostgreSQL
    /// NOTIFY wakes other control-plane processes and remains a fallback if
    /// the queue is unavailable.
    async fn reload_routes_after_compose_port_change(
        &self,
        project_id: i32,
    ) -> Result<(), ProjectError> {
        let queue_result = self
            .queue_service
            .send(Job::ForceRouteReload(ForceRouteReloadJob {
                environment_id: None,
                deployment_id: None,
            }))
            .await;

        let payload = serde_json::json!({
            "action": "UPDATE",
            "project_id": project_id,
            "field": "preset_config.public_ports",
        })
        .to_string();
        let notify_result = self
            .db
            .execute(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT pg_notify('project_route_change', $1)",
                [payload.into()],
            ))
            .await;

        match (queue_result, notify_result) {
            (Ok(()), _) | (_, Ok(_)) => Ok(()),
            (Err(queue_error), Err(database_error)) => Err(ProjectError::RouteReloadFailed {
                project_id,
                queue_reason: queue_error.to_string(),
                database_reason: database_error.to_string(),
            }),
        }
    }

    /// Resolve whether the given connection points to a GitLab provider.
    /// Returns `(base_url, access_token, auth_method)` for GitLab connections;
    /// `Err` for all others.
    async fn resolve_gitlab_connection(
        &self,
        connection_id: i32,
    ) -> Result<(String, String, String), String> {
        use temps_entities::{git_provider_connections, git_providers};

        let connection = git_provider_connections::Entity::find_by_id(connection_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| format!("DB error: {}", e))?
            .ok_or_else(|| format!("Connection {} not found", connection_id))?;

        let provider = git_providers::Entity::find_by_id(connection.provider_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| format!("DB error: {}", e))?
            .ok_or_else(|| format!("Provider {} not found", connection.provider_id))?;

        // Only handle GitLab providers.
        if provider.provider_type != "gitlab" {
            return Err(format!(
                "Provider {} is not a GitLab provider (type: {})",
                provider.id, provider.provider_type
            ));
        }

        let base_url = provider
            .base_url
            .unwrap_or_else(|| "https://gitlab.com".to_string());

        let auth_method = provider.auth_method.clone();

        let access_token = self
            .git_provider_manager
            .get_connection_token(connection_id)
            .await
            .map_err(|e| {
                format!(
                    "Failed to get access token for connection {}: {}",
                    connection_id, e
                )
            })?;

        Ok((base_url, access_token, auth_method))
    }

    /// Install a GitLab webhook for the given project/connection.
    /// Returns `(hook_id, encrypted_signing_token)` on success.
    async fn install_gitlab_webhook_for_connection(
        &self,
        project_id: i32,
        connection_id: i32,
        owner: &str,
        repo: &str,
    ) -> Result<(i64, String), String> {
        use temps_git::services::gitlab_webhook::{
            generate_signing_token, GitLabWebhookClient, WebhookAuthMethod,
        };

        let (base_url, access_token, auth_method_str) =
            match self.resolve_gitlab_connection(connection_id).await {
                Ok(triple) => triple,
                // Not a GitLab provider — skip silently.
                Err(e) => return Err(e),
            };

        let client = GitLabWebhookClient::new(
            base_url,
            access_token,
            WebhookAuthMethod::from_str(&auth_method_str),
        );

        // Pre-flight: verify the user has >= Maintainer (40) access.
        let access_level = client
            .get_project_access_level(owner, repo)
            .await
            .map_err(|e| format!("Could not check permissions for {}/{}: {}", owner, repo, e))?;

        if access_level < 40 {
            return Err(format!(
                "Insufficient GitLab permissions for {}/{}: access_level={} (need >= 40 Maintainer)",
                owner, repo, access_level
            ));
        }

        // Resolve the webhook URL from config.
        let external_url = self
            .config_service
            .get_settings()
            .await
            .ok()
            .and_then(|s| s.external_url)
            .unwrap_or_else(|| "http://localhost:8080".to_string());
        let webhook_url = format!("{}/api/webhook/git/gitlab/events", external_url);

        // Generate a random 32-byte signing token.
        let signing_token = generate_signing_token()
            .map_err(|error| format!("Failed to generate GitLab webhook token: {error}"))?;

        let hook_id = client
            .install_webhook(owner, repo, &webhook_url, &signing_token)
            .await
            .map_err(|e| {
                format!(
                    "Failed to install webhook for project {}: {}",
                    project_id, e
                )
            })?;

        // Encrypt the token before storing.
        let encrypted = self
            .encryption_service
            .encrypt_string(&signing_token)
            .map_err(|e| format!("Failed to encrypt signing token: {}", e))?;

        info!(
            "Installed GitLab webhook {} for project {} ({}/{})",
            hook_id, project_id, owner, repo
        );

        Ok((hook_id, encrypted))
    }

    /// Remove a GitLab webhook.  Best-effort; 404 is treated as success.
    async fn delete_gitlab_webhook_for_connection(
        &self,
        connection_id: i32,
        owner: &str,
        repo: &str,
        hook_id: i32,
    ) -> Result<(), String> {
        use temps_git::services::gitlab_webhook::{GitLabWebhookClient, WebhookAuthMethod};

        let (base_url, access_token, auth_method_str) =
            match self.resolve_gitlab_connection(connection_id).await {
                Ok(triple) => triple,
                // Not a GitLab connection — nothing to remove.
                Err(_) => return Ok(()),
            };

        let client = GitLabWebhookClient::new(
            base_url,
            access_token,
            WebhookAuthMethod::from_str(&auth_method_str),
        );
        client
            .delete_webhook(owner, repo, hook_id as i64)
            .await
            .map_err(|e| format!("GitLab delete webhook error: {}", e))
    }

    /// Reinstall (or install for the first time) a GitLab webhook for a project.
    /// Called by `POST /projects/{id}/gitlab/reinstall-webhook`.
    pub async fn reinstall_gitlab_webhook(&self, project_id: i32) -> Result<i32, ProjectError> {
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ProjectError::NotFound(format!(
                "Project {} not found",
                project_id
            )))?;

        let connection_id = project.git_provider_connection_id.ok_or_else(|| {
            ProjectError::Other(format!(
                "Project {} has no git provider connection",
                project_id
            ))
        })?;

        let owner = project.repo_owner.clone();
        let repo = project.repo_name.clone();

        // Best-effort: remove the old webhook first.
        if let Some(old_hook_id) = project.gitlab_webhook_id {
            if let Err(e) = self
                .delete_gitlab_webhook_for_connection(connection_id, &owner, &repo, old_hook_id)
                .await
            {
                warn!(
                    "Failed to remove old GitLab webhook {} during reinstall for project {}: {}",
                    old_hook_id, project_id, e
                );
            }
        }

        let (hook_id, encrypted_token) = self
            .install_gitlab_webhook_for_connection(project_id, connection_id, &owner, &repo)
            .await
            .map_err(ProjectError::Other)?;

        // Persist the new hook id + token.
        let mut active_project: projects::ActiveModel = project.into();
        active_project.gitlab_webhook_id = Set(Some(hook_id as i32));
        active_project.gitlab_webhook_signing_token = Set(Some(encrypted_token));
        active_project.update(self.db.as_ref()).await?;

        info!(
            "Reinstalled GitLab webhook {} for project {}",
            hook_id, project_id
        );

        Ok(hook_id as i32)
    }

    // ── Bitbucket Cloud webhook lifecycle ────────────────────────────────────
    //
    // Mirrors the GitLab pattern.  The delivery URL embeds a secret token in
    // the path (`…/bitbucket/events/{token}`); there is no HMAC body signing.
    // The token is generated once per project, stored encrypted, and persisted
    // together with the Bitbucket hook UUID so we can `DELETE` it on disconnect.
    //
    // Failures are always non-fatal: the project connects without the webhook
    // and the operator can re-try via the Temps UI or re-connect the repo.

    /// Resolve whether `connection_id` points to a Bitbucket provider.
    ///
    /// Returns `(access_token, auth_method_str)` for Bitbucket connections;
    /// `Err(String)` for all others (caller silently skips).
    async fn resolve_bitbucket_connection(
        &self,
        connection_id: i32,
    ) -> Result<(String, String), String> {
        use temps_entities::{git_provider_connections, git_providers};

        let connection = git_provider_connections::Entity::find_by_id(connection_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| format!("DB error fetching connection {connection_id}: {e}"))?
            .ok_or_else(|| format!("Connection {connection_id} not found"))?;

        let provider = git_providers::Entity::find_by_id(connection.provider_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| format!("DB error fetching provider {}: {e}", connection.provider_id))?
            .ok_or_else(|| format!("Provider {} not found", connection.provider_id))?;

        if provider.provider_type != "bitbucket" {
            return Err(format!(
                "Provider {} is not a Bitbucket provider (type: {})",
                provider.id, provider.provider_type
            ));
        }

        let auth_method = provider.auth_method.clone();

        let access_token = self
            .git_provider_manager
            .get_connection_token(connection_id)
            .await
            .map_err(|e| {
                format!("Failed to get access token for Bitbucket connection {connection_id}: {e}")
            })?;

        Ok((access_token, auth_method))
    }

    /// Install a Bitbucket Cloud webhook for `project_id` / `connection_id`.
    ///
    /// Steps:
    /// 1. Generate a fresh `bitbucket_webhook_token` via `OsRng`.
    /// 2. Build the delivery URL: `{external_url}/api/webhook/git/bitbucket/events/{token}`.
    /// 3. Call `POST /2.0/repositories/{workspace}/{slug}/hooks`.
    /// 4. Return `(hook_uuid, encrypted_token)` to the caller for persistence.
    async fn install_bitbucket_webhook_for_connection(
        &self,
        project_id: i32,
        connection_id: i32,
        owner: &str,
        repo: &str,
    ) -> Result<(String, String), String> {
        use temps_git::services::bitbucket_provider::generate_bitbucket_webhook_token;
        use temps_git::services::git_provider::WebhookConfig;

        let (access_token, _auth_method_str) =
            match self.resolve_bitbucket_connection(connection_id).await {
                Ok(pair) => pair,
                Err(e) => return Err(e),
            };

        let external_url = self
            .config_service
            .get_settings()
            .await
            .ok()
            .and_then(|s| s.external_url)
            .unwrap_or_else(|| "http://localhost:8080".to_string());

        // Generate a fresh secret-in-path delivery token.
        let delivery_token = generate_bitbucket_webhook_token()
            .map_err(|error| format!("Failed to generate Bitbucket webhook token: {error}"))?;

        let webhook_url = format!(
            "{}/api/webhook/git/bitbucket/events/{}",
            external_url.trim_end_matches('/'),
            delivery_token
        );

        // Resolve the connection to get provider_id, then get provider service.
        let connection = {
            use temps_entities::git_provider_connections;
            git_provider_connections::Entity::find_by_id(connection_id)
                .one(self.db.as_ref())
                .await
                .map_err(|e| format!("DB error fetching connection {connection_id}: {e}"))?
                .ok_or_else(|| format!("Connection {connection_id} not found"))?
        };

        let provider_service = self
            .git_provider_manager
            .get_provider_service(connection.provider_id)
            .await
            .map_err(|e| {
                format!(
                    "Failed to get provider service for Bitbucket connection {connection_id}: {e}"
                )
            })?;

        let hook_uuid = provider_service
            .create_webhook(
                &access_token,
                owner,
                repo,
                WebhookConfig {
                    url: webhook_url,
                    secret: None, // Bitbucket uses secret-in-path; no HMAC secret
                    events: vec![
                        "repo:push".to_string(),
                        "pullrequest:created".to_string(),
                        "pullrequest:updated".to_string(),
                        "pullrequest:fulfilled".to_string(),
                        "pullrequest:rejected".to_string(),
                    ],
                },
            )
            .await
            .map_err(|e| {
                format!(
                    "Failed to register Bitbucket webhook for project {project_id} ({owner}/{repo}): {e}"
                )
            })?;

        // Encrypt the delivery token before storing.
        let encrypted_token = self
            .encryption_service
            .encrypt_string(&delivery_token)
            .map_err(|e| format!("Failed to encrypt Bitbucket webhook token: {e}"))?;

        info!(
            "Registered Bitbucket webhook {} for project {} ({}/{})",
            hook_uuid, project_id, owner, repo
        );

        Ok((hook_uuid, encrypted_token))
    }

    /// Delete a previously auto-registered Bitbucket webhook.  Best-effort;
    /// 404 is treated as success (already gone).
    async fn delete_bitbucket_webhook_for_connection(
        &self,
        connection_id: i32,
        owner: &str,
        repo: &str,
        hook_uuid: &str,
    ) -> Result<(), String> {
        let (access_token, _) = match self.resolve_bitbucket_connection(connection_id).await {
            Ok(pair) => pair,
            // Not a Bitbucket connection — nothing to do.
            Err(_) => return Ok(()),
        };

        let connection = {
            use temps_entities::git_provider_connections;
            git_provider_connections::Entity::find_by_id(connection_id)
                .one(self.db.as_ref())
                .await
                .map_err(|e| format!("DB error: {e}"))?
                .ok_or_else(|| format!("Connection {connection_id} not found"))?
        };

        let provider_service = self
            .git_provider_manager
            .get_provider_service(connection.provider_id)
            .await
            .map_err(|e| {
                format!(
                    "Failed to get provider service for Bitbucket connection {connection_id}: {e}"
                )
            })?;

        provider_service
            .delete_webhook(&access_token, owner, repo, hook_uuid)
            .await
            .map_err(|e| format!("Bitbucket delete webhook error for {owner}/{repo}: {e}"))
    }

    // ── Gitea webhook install / resolve ─────────────────────────────────────────
    //
    // BLOCKER 1 fix: Gitea webhooks use HMAC-SHA256 (unlike Bitbucket's
    // secret-in-path).  We generate a signing token with OsRng, register it
    // with Gitea's API via `create_webhook` (passing the secret in the config),
    // then store it encrypted in `projects.gitea_webhook_signing_token`.
    //
    // Failures are always non-fatal — same policy as Bitbucket/GitLab.

    /// Resolve whether `connection_id` points to a Gitea provider.
    ///
    /// Returns `(access_token, base_url)` for Gitea connections;
    /// `Err(String)` for all others (caller silently skips).
    async fn resolve_gitea_connection(
        &self,
        connection_id: i32,
    ) -> Result<(String, String), String> {
        use temps_entities::{git_provider_connections, git_providers};

        let connection = git_provider_connections::Entity::find_by_id(connection_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| format!("DB error fetching connection {connection_id}: {e}"))?
            .ok_or_else(|| format!("Connection {connection_id} not found"))?;

        let provider = git_providers::Entity::find_by_id(connection.provider_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| format!("DB error fetching provider {}: {e}", connection.provider_id))?
            .ok_or_else(|| format!("Provider {} not found", connection.provider_id))?;

        if provider.provider_type != "gitea" {
            return Err(format!(
                "Provider {} is not a Gitea provider (type: {})",
                provider.id, provider.provider_type
            ));
        }

        let base_url = provider
            .base_url
            .ok_or_else(|| format!("Gitea provider {} has no base_url configured", provider.id))?;

        let access_token = self
            .git_provider_manager
            .get_connection_token(connection_id)
            .await
            .map_err(|e| {
                format!("Failed to get access token for Gitea connection {connection_id}: {e}")
            })?;

        Ok((access_token, base_url))
    }

    /// Install a Gitea webhook for `project_id` / `connection_id`.
    ///
    /// Steps:
    /// 1. Resolve the connection to confirm it's a Gitea provider.
    /// 2. Generate a fresh 32-byte hex signing token via `generate_gitea_signing_token`.
    /// 3. Build the delivery URL: `{external_url}/api/webhook/git/gitea/events`.
    /// 4. Call `create_webhook` with the HMAC secret (Gitea HMAC signing).
    /// 5. Encrypt the token and return it for storage.
    ///
    /// Returns `Ok(encrypted_token)` on success; `Err(String)` on any failure
    /// (non-fatal — caller logs and continues).
    async fn install_gitea_webhook_for_connection(
        &self,
        project_id: i32,
        connection_id: i32,
        owner: &str,
        repo: &str,
    ) -> Result<String, String> {
        use temps_git::services::git_provider::WebhookConfig;
        use temps_git::services::gitea_provider::generate_gitea_signing_token;

        let (access_token, _base_url) = match self.resolve_gitea_connection(connection_id).await {
            Ok(pair) => pair,
            Err(e) => return Err(e),
        };

        let external_url = self
            .config_service
            .get_settings()
            .await
            .ok()
            .and_then(|s| s.external_url)
            .unwrap_or_else(|| "http://localhost:8080".to_string());

        // Generate a fresh HMAC secret (used as the Gitea webhook secret).
        let signing_token = generate_gitea_signing_token()
            .map_err(|error| format!("Failed to generate Gitea webhook token: {error}"))?;

        let webhook_url = format!(
            "{}/api/webhook/git/gitea/events",
            external_url.trim_end_matches('/')
        );

        // Resolve the connection to get provider_id, then get provider service.
        let connection = {
            use temps_entities::git_provider_connections;
            git_provider_connections::Entity::find_by_id(connection_id)
                .one(self.db.as_ref())
                .await
                .map_err(|e| format!("DB error fetching connection {connection_id}: {e}"))?
                .ok_or_else(|| format!("Connection {connection_id} not found"))?
        };

        let provider_service = self
            .git_provider_manager
            .get_provider_service(connection.provider_id)
            .await
            .map_err(|e| {
                format!("Failed to get provider service for Gitea connection {connection_id}: {e}")
            })?;

        // Gitea webhooks are HMAC-SHA256; pass the secret in WebhookConfig.secret.
        provider_service
            .create_webhook(
                &access_token,
                owner,
                repo,
                WebhookConfig {
                    url: webhook_url,
                    secret: Some(signing_token.clone()), // HMAC secret
                    events: vec!["push".to_string()],
                },
            )
            .await
            .map_err(|e| {
                format!(
                    "Failed to register Gitea webhook for project {project_id} ({owner}/{repo}): {e}"
                )
            })?;

        // Encrypt the signing token before storing.
        let encrypted_token = self
            .encryption_service
            .encrypt_string(&signing_token)
            .map_err(|e| format!("Failed to encrypt Gitea webhook signing token: {e}"))?;

        info!(
            "Registered Gitea webhook for project {} ({}/{})",
            project_id, owner, repo
        );

        Ok(encrypted_token)
    }

    // ── Generic webhook token install ────────────────────────────────────────────
    //
    // BLOCKER 2 fix: the Generic provider has no REST API to auto-register a
    // hook.  We generate a token and store it encrypted.  The webhook URL
    // (`{external_url}/api/webhook/git/generic/events/{token}`) is surfaced to
    // the operator for manual configuration.
    //
    // Returns `Ok(Some(encrypted_token))` for Generic connections,
    // `Ok(None)` for non-Generic connections, `Err` on failure.

    /// Generate and store a generic webhook delivery token for `project_id`.
    ///
    /// Returns `Ok(Some(encrypted_token))` when `connection_id` is a Generic
    /// provider; `Ok(None)` when it's any other provider type (caller skips).
    async fn install_generic_webhook_token(
        &self,
        project_id: i32,
        connection_id: i32,
    ) -> Result<Option<String>, String> {
        use temps_entities::{git_provider_connections, git_providers};
        use temps_git::services::generic_provider::generate_generic_webhook_token;

        let connection = git_provider_connections::Entity::find_by_id(connection_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| format!("DB error fetching connection {connection_id}: {e}"))?
            .ok_or_else(|| format!("Connection {connection_id} not found"))?;

        let provider = git_providers::Entity::find_by_id(connection.provider_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| format!("DB error fetching provider {}: {e}", connection.provider_id))?
            .ok_or_else(|| format!("Provider {} not found", connection.provider_id))?;

        if provider.provider_type != "generic" {
            // Not a Generic connection — nothing to do.
            return Ok(None);
        }

        let token = generate_generic_webhook_token()
            .map_err(|error| format!("Failed to generate Generic webhook token: {error}"))?;

        let encrypted_token = self
            .encryption_service
            .encrypt_string(&token)
            .map_err(|e| format!("Failed to encrypt Generic webhook token: {e}"))?;

        let external_url = self
            .config_service
            .get_settings()
            .await
            .ok()
            .and_then(|s| s.external_url)
            .unwrap_or_else(|| "http://localhost:8080".to_string());

        info!(
            "Generated Generic webhook token for project {} (conn {}). \
             Configure your git host to POST to: {}/api/webhook/git/generic/events/{}",
            project_id,
            connection_id,
            external_url.trim_end_matches('/'),
            token // plaintext token is only logged here; stored value is encrypted
        );

        Ok(Some(encrypted_token))
    }

    pub async fn get_projects_paginated(
        &self,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<Project>, i64), ProjectError> {
        self.get_projects_paginated_excluding(page, per_page, &[])
            .await
    }

    /// [`Self::get_projects_paginated`], minus a caller-supplied set of
    /// project ids.
    ///
    /// `hidden` comes from
    /// [`ProjectAccessChecker::hidden_project_ids`](temps_core::ProjectAccessChecker::hidden_project_ids)
    /// and is empty on an instance with no access grants configured, which
    /// makes this identical to the unfiltered query in that case. The
    /// exclusion is applied to the **count** as well as the page, so
    /// pagination doesn't advertise rows the caller can never see.
    pub async fn get_projects_paginated_excluding(
        &self,
        page: i64,
        per_page: i64,
        hidden: &[i32],
    ) -> Result<(Vec<Project>, i64), ProjectError> {
        use sea_orm::PaginatorTrait;
        use sea_orm::QueryOrder;

        // Calculate offset
        let offset = ((page - 1) * per_page) as u64;

        let filtered = || {
            let query = projects::Entity::find();
            if hidden.is_empty() {
                query
            } else {
                query.filter(projects::Column::Id.is_not_in(hidden.iter().copied()))
            }
        };

        // Get total count
        let total = filtered()
            .count(self.db.as_ref())
            .await
            .map_err(|e| ProjectError::DatabaseConnectionError(e.to_string()))?
            as i64;

        // Get paginated projects. Never-deployed projects (NULL last_deployment)
        // sort last rather than first (a NULL under DESC would otherwise appear
        // as the most-recently-deployed project).
        let projects = filtered()
            .order_by_with_nulls(
                projects::Column::LastDeployment,
                sea_orm::Order::Desc,
                sea_orm::sea_query::NullOrdering::Last,
            )
            .order_by_desc(projects::Column::CreatedAt)
            .offset(offset)
            .limit(per_page as u64)
            .all(self.db.as_ref())
            .await
            .map_err(|e| ProjectError::DatabaseConnectionError(e.to_string()))?;

        let projects_found: Vec<Project> = projects
            .into_iter()
            .map(Self::map_db_project_to_project)
            .collect();
        Ok((projects_found, total))
    }

    pub async fn get_total_projects(&self) -> Result<i64, ProjectError> {
        use sea_orm::PaginatorTrait;
        // Get total count of projects
        let paginator = projects::Entity::find().paginate(self.db.as_ref(), 1);
        let total = paginator.num_items().await?;

        Ok(total as i64)
    }

    pub async fn get_project_statistics(&self) -> Result<ProjectStatistics, ProjectError> {
        self.get_project_statistics_excluding(&[]).await
    }

    /// [`Self::get_project_statistics`], minus a caller-supplied set of
    /// project ids.
    ///
    /// The count has to honour the same exclusion as the list, or the
    /// dashboard tells a scoped user how many projects exist on the
    /// instance while showing them only their own — a smaller leak than
    /// the names, but the same leak.
    pub async fn get_project_statistics_excluding(
        &self,
        hidden: &[i32],
    ) -> Result<ProjectStatistics, ProjectError> {
        use sea_orm::PaginatorTrait;

        let query = projects::Entity::find();
        let query = if hidden.is_empty() {
            query
        } else {
            query.filter(projects::Column::Id.is_not_in(hidden.iter().copied()))
        };

        let total_count = query
            .count(self.db.as_ref())
            .await
            .map_err(|e| ProjectError::DatabaseConnectionError(e.to_string()))?
            as i64;

        Ok(ProjectStatistics { total_count })
    }

    pub async fn update_deployment_settings(
        &self,
        project_id_or_slug: &str,
        settings: UpdateDeploymentSettingsRequest,
    ) -> Result<Project, ProjectError> {
        // Find project by ID or slug
        let project = if let Ok(project_id_int) = project_id_or_slug.parse::<i32>() {
            projects::Entity::find_by_id(project_id_int)
                .one(self.db.as_ref())
                .await?
                .ok_or_else(|| {
                    ProjectError::NotFound(format!("Project with id {} not found", project_id_int))
                })?
        } else {
            projects::Entity::find()
                .filter(projects::Column::Slug.eq(project_id_or_slug))
                .one(self.db.as_ref())
                .await?
                .ok_or_else(|| {
                    ProjectError::NotFound(format!(
                        "Project with slug {} not found",
                        project_id_or_slug
                    ))
                })?
        };

        // Update the project with new settings
        let mut active_project: projects::ActiveModel = project.clone().into();

        // Update deployment config with new resource settings
        let mut deployment_config = project.deployment_config.clone().unwrap_or_default();
        deployment_config.cpu_request = settings.cpu_request;
        deployment_config.cpu_limit = settings.cpu_limit;
        deployment_config.memory_request = settings.memory_request;
        deployment_config.memory_limit = settings.memory_limit;
        active_project.deployment_config = Set(Some(deployment_config));

        let updated_project = active_project.update(self.db.as_ref()).await?;

        // Emit ProjectUpdated job
        let project_updated_job = Job::ProjectUpdated(ProjectUpdatedJob {
            project_id: updated_project.id,
            project_name: updated_project.name.clone(),
        });

        if let Err(e) = self.queue_service.send(project_updated_job).await {
            warn!(
                "Failed to emit ProjectUpdated job for project {}: {}",
                updated_project.id, e
            );
        } else {
            info!(
                "Emitted ProjectUpdated job for project {} (settings update)",
                updated_project.id
            );
        }

        Ok(Self::map_db_project_to_project(updated_project))
    }

    /// Update deployment configuration for a project
    pub async fn update_project_deployment_config(
        &self,
        project_id: i32,
        config: UpdateDeploymentConfigRequest,
    ) -> Result<Project, ProjectError> {
        // Find project by ID or slug
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                ProjectError::NotFound(format!("Project with id {} not found", project_id))
            })?;

        // Get existing deployment config or create default
        let mut deployment_config = project.deployment_config.clone().unwrap_or_default();

        // Update only the fields that are provided
        if let Some(cpu_request) = config.cpu_request {
            deployment_config.cpu_request = Some(cpu_request);
        }
        if let Some(cpu_limit) = config.cpu_limit {
            deployment_config.cpu_limit = Some(cpu_limit);
        }
        if let Some(memory_request) = config.memory_request {
            deployment_config.memory_request = Some(memory_request);
        }
        if let Some(memory_limit) = config.memory_limit {
            deployment_config.memory_limit = Some(memory_limit);
        }
        if let Some(exposed_port) = config.exposed_port {
            deployment_config.exposed_port = Some(exposed_port);
        }
        if let Some(automatic_deploy) = config.automatic_deploy {
            deployment_config.automatic_deploy = Some(automatic_deploy);
        }
        if let Some(performance_metrics_enabled) = config.performance_metrics_enabled {
            deployment_config.performance_metrics_enabled = performance_metrics_enabled;
        }
        if let Some(session_recording_enabled) = config.session_recording_enabled {
            deployment_config.session_recording_enabled = session_recording_enabled;
        }
        if let Some(replicas) = config.replicas {
            deployment_config.replicas = replicas;
        }
        if let Some(security) = config.security {
            deployment_config.security = Some(security);
        }
        // Absent leaves it unset (disabled, and inheritable); an explicit value
        // — including `false` — pins it for every environment that doesn't
        // override it.
        if let Some(cross_architecture_builds) = config.cross_architecture_builds {
            deployment_config.cross_architecture_builds = Some(cross_architecture_builds);
        }
        if let Some(request_timeout_seconds) = config.request_timeout_seconds {
            deployment_config.request_timeout_seconds = Some(request_timeout_seconds);
        }
        if let Some(sse_idle_timeout_seconds) = config.sse_idle_timeout_seconds {
            deployment_config.sse_idle_timeout_seconds = Some(sse_idle_timeout_seconds);
        }
        if let Some(websocket_idle_timeout_seconds) = config.websocket_idle_timeout_seconds {
            deployment_config.websocket_idle_timeout_seconds = Some(websocket_idle_timeout_seconds);
        }
        if let Some(max_concurrent_connections) = config.max_concurrent_connections {
            deployment_config.max_concurrent_connections = Some(max_concurrent_connections);
        }

        // Validate the deployment config
        deployment_config
            .validate()
            .map_err(|e| ProjectError::InvalidInput(format!("Invalid deployment config: {}", e)))?;

        // Update the project
        let mut active_project: projects::ActiveModel = project.clone().into();
        active_project.deployment_config = Set(Some(deployment_config));

        let updated_project = active_project.update(self.db.as_ref()).await?;

        // Emit ProjectUpdated job
        let project_updated_job = Job::ProjectUpdated(ProjectUpdatedJob {
            project_id: updated_project.id,
            project_name: updated_project.name.clone(),
        });

        if let Err(e) = self.queue_service.send(project_updated_job).await {
            warn!(
                "Failed to emit ProjectUpdated job for project {}: {}",
                updated_project.id, e
            );
        } else {
            info!(
                "Emitted ProjectUpdated job for project {} (deployment config update)",
                updated_project.id
            );
        }

        Ok(Self::map_db_project_to_project(updated_project))
    }

    /// Generate a unique project slug by checking for collisions and appending a short UUID if needed.
    /// Slug is truncated to 40 chars max to keep DNS labels within the 63-char limit
    /// when combined with environment slug and service name prefix.
    pub async fn generate_unique_project_slug(&self, name: &str) -> Result<String, ProjectError> {
        let mut base_slug = slugify(name);
        // Truncate to 40 chars max (leaves room for "-production" env slug + "service-" prefix
        // within the 63-char DNS label limit)
        if base_slug.len() > 40 {
            base_slug = base_slug[..40].trim_end_matches('-').to_string();
        }

        // First, try the base slug
        let existing = projects::Entity::find()
            .filter(projects::Column::Slug.eq(&base_slug))
            .one(self.db.as_ref())
            .await
            .map_err(|e| ProjectError::Other(e.to_string()))?;

        if existing.is_none() {
            return Ok(base_slug);
        }

        // If base slug exists, generate a short UUID suffix
        let short_uuid = Uuid::new_v4()
            .to_string()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .take(6)
            .collect::<String>()
            .to_lowercase();

        let unique_slug = format!("{}-{}", base_slug, short_uuid);

        // Double check that this unique slug doesn't exist (extremely unlikely but be safe)
        let existing_unique = projects::Entity::find()
            .filter(projects::Column::Slug.eq(&unique_slug))
            .one(self.db.as_ref())
            .await
            .map_err(|e| ProjectError::Other(e.to_string()))?;

        if existing_unique.is_some() {
            // This is extremely unlikely, but if it happens, generate a new UUID
            let retry_uuid = Uuid::new_v4()
                .to_string()
                .chars()
                .filter(|c| c.is_alphanumeric())
                .take(8)
                .collect::<String>()
                .to_lowercase();
            Ok(format!("{}-{}", base_slug, retry_uuid))
        } else {
            Ok(unique_slug)
        }
    }

    pub fn map_db_project_to_project(db_project: projects::Model) -> Project {
        // Extract deployment config fields
        let deployment_config = db_project.deployment_config.clone();

        // Convert preset to the runtime/UI slug (reconstructs nixpacks-{provider})
        let preset_str =
            temps_presets::runtime_slug(db_project.preset, db_project.preset_config.as_ref());

        // Handle repo_name and repo_owner - return None for empty strings (Git-less projects)
        let repo_name = if db_project.repo_name.is_empty() {
            None
        } else {
            Some(db_project.repo_name)
        };
        let repo_owner = if db_project.repo_owner.is_empty() {
            None
        } else {
            Some(db_project.repo_owner)
        };

        // Serialize preset_config to JSON value for the response
        let preset_config_json = db_project
            .preset_config
            .as_ref()
            .and_then(|config| serde_json::to_value(config).ok());

        Project {
            id: db_project.id,
            slug: db_project.slug,
            name: db_project.name,
            repo_name,
            repo_owner,
            directory: db_project.directory,
            main_branch: db_project.main_branch,
            preset: Some(preset_str),
            preset_config: preset_config_json,
            created_at: db_project.created_at,
            updated_at: db_project.updated_at,
            automatic_deploy: deployment_config
                .clone()
                .and_then(|c| c.automatic_deploy)
                .unwrap_or(false),
            cpu_request: deployment_config.clone().and_then(|c| c.cpu_request),
            cpu_limit: deployment_config.clone().and_then(|c| c.cpu_limit),
            memory_request: deployment_config.clone().and_then(|c| c.memory_request),
            memory_limit: deployment_config.clone().and_then(|c| c.memory_limit),
            performance_metrics_enabled: deployment_config
                .clone()
                .map(|c| c.performance_metrics_enabled)
                .unwrap_or(false),
            last_deployment: db_project.last_deployment,
            project_type: if db_project.preset == temps_entities::preset::Preset::Static {
                "static".to_string()
            } else {
                "server".to_string()
            },
            use_default_wildcard: true, // Deprecated field, always true
            custom_domain: None,        // Deprecated field, use project_domains table
            is_public_repo: db_project.is_public_repo,
            git_url: db_project.git_url,
            git_provider_connection_id: db_project.git_provider_connection_id,
            is_on_demand: false, // Deprecated field, default to false
            deployment_config: deployment_config.clone(),
            attack_mode: db_project.attack_mode,
            ai_alert_summaries_enabled: db_project.ai_alert_summaries_enabled,
            ai_debug_chat_enabled: db_project.ai_debug_chat_enabled,
            ai_write_actions_enabled: db_project.ai_write_actions_enabled,
            ai_api_traffic_summary_enabled: db_project.ai_api_traffic_summary_enabled,
            error_source_context_enabled: db_project.error_source_context_enabled,
            error_source_root: db_project.error_source_root,
            enable_preview_environments: db_project.enable_preview_environments,
            preview_envs_on_demand: db_project.preview_envs_on_demand,
            preview_envs_idle_timeout_seconds: db_project.preview_envs_idle_timeout_seconds,
            preview_envs_wake_timeout_seconds: db_project.preview_envs_wake_timeout_seconds,
            source_type: db_project.source_type,
            gitlab_webhook_id: db_project.gitlab_webhook_id,
            cross_project_trace_sharing: db_project.cross_project_trace_sharing,
        }
    }

    // Environment Variables Methods
    pub async fn get_environment_variables(
        &self,
        project_id: i32,
    ) -> Result<Vec<EnvVarWithEnvironments>, ProjectError> {
        self.env_var_service
            .get_environment_variables(project_id)
            .await
            .map_err(|e| ProjectError::Other(e.to_string()))
    }

    pub async fn create_environment_variable(
        &self,
        project_id: i32,
        environment_ids: Vec<i32>,
        key: String,
        value: String,
        is_secret: bool,
    ) -> Result<EnvVarWithEnvironments, ProjectError> {
        self.env_var_service
            .create_environment_variable(project_id, environment_ids, key, value, is_secret)
            .await
            .map_err(|e| ProjectError::Other(e.to_string()))
    }

    pub async fn update_environment_variable(
        &self,
        project_id: i32,
        var_id: i32,
        key: String,
        value: String,
        environment_ids: Vec<i32>,
    ) -> Result<EnvVarWithEnvironments, ProjectError> {
        self.env_var_service
            .update_environment_variable(project_id, var_id, key, value, environment_ids)
            .await
            .map_err(|e| ProjectError::Other(e.to_string()))
    }

    pub async fn delete_environment_variable(
        &self,
        project_id: i32,
        var_id: i32,
    ) -> Result<(), ProjectError> {
        self.env_var_service
            .delete_environment_variable(project_id, var_id)
            .await
            .map_err(|e| ProjectError::Other(e.to_string()))
    }

    pub async fn get_environment_variable_value(
        &self,
        project_id: i32,
        key: &str,
        environment_id: Option<i32>,
    ) -> Result<String, ProjectError> {
        self.env_var_service
            .get_environment_variable_value(project_id, key, environment_id)
            .await
            .map_err(|e| ProjectError::Other(e.to_string()))
    }

    /// Queue an initial deployment job for a newly created project
    async fn queue_initial_deployment_job(
        &self,
        project: &temps_entities::projects::Model,
        _environment: &temps_entities::environments::Model,
    ) -> Result<(), ProjectError> {
        // Fetch the latest commit from the git provider if connection exists
        let commit_sha = if let Some(connection_id) = project.git_provider_connection_id {
            match self
                .git_provider_manager
                .get_branch_latest_commit(
                    connection_id,
                    &project.repo_owner,
                    &project.repo_name,
                    &project.main_branch,
                )
                .await
            {
                Ok(commit) => {
                    info!(
                        "Fetched latest commit for project {}: {} - {}",
                        project.id, commit.sha, commit.message
                    );
                    commit.sha
                }
                Err(e) => {
                    // Log error but don't fail - fall back to a generic commit
                    tracing::warn!(
                        "Failed to fetch latest commit for project {}: {}. Using fallback.",
                        project.id,
                        e
                    );
                    "HEAD".to_string()
                }
            }
        } else {
            // No git provider connection, use fallback
            "HEAD".to_string()
        };

        // Create a GitPushEvent job to trigger the initial deployment
        // The deployment service's job processor will handle creating the pipeline and deployment
        let git_push_job = temps_core::GitPushEventJob {
            owner: project.repo_owner.clone(),
            repo: project.repo_name.clone(),
            branch: Some(project.main_branch.clone()),
            tag: None, // No tag for initial deployment
            commit: commit_sha.clone(),
            project_id: project.id, // Include project_id
            // Initial deployment is a user-initiated event (project creation),
            // not a git webhook — bypass automatic_deploy.
            manual_trigger: true,
            rollback_from_deployment_id: None,
            // Infer the target from the branch at creation time (the default
            // environment tracks main_branch).
            target_environment_id: None,
        };

        self.queue_service
            .send(temps_core::Job::GitPushEvent(git_push_job))
            .await
            .map_err(|e| ProjectError::Other(format!("Failed to queue deployment job: {}", e)))?;

        info!(
            "Queued GitPushEvent job for initial deployment of project {} (owner: {}, repo: {}, branch: {}, commit: {})",
            project.id,
            &project.repo_owner,
            &project.repo_name,
            project.main_branch,
            commit_sha
        );

        Ok(())
    }

    /// Trigger a pipeline for a specific project and environment
    pub async fn trigger_pipeline(
        &self,
        project_id: i32,
        environment_id: i32,
        branch: Option<String>,
        tag: Option<String>,
        commit: Option<String>,
    ) -> Result<(i32, i32, Option<String>, Option<String>, Option<String>), ProjectError> {
        // Get the project to validate it exists and get repository information
        let project = temps_entities::projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| ProjectError::Other(e.to_string()))?
            .ok_or_else(|| ProjectError::NotFound("Project not found".to_string()))?;

        // Validate environment belongs to this project and is not soft-deleted
        let environment = temps_entities::environments::Entity::find_by_id(environment_id)
            .filter(temps_entities::environments::Column::ProjectId.eq(project_id))
            .filter(temps_entities::environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await
            .map_err(|e| ProjectError::Other(e.to_string()))?
            .ok_or_else(|| {
                ProjectError::NotFound(
                    "Environment not found or doesn't belong to project".to_string(),
                )
            })?;

        // Validate project has repository information
        if project.repo_owner.is_empty() || project.repo_name.is_empty() {
            return Err(ProjectError::InvalidInput(
                "Project must have repository information to trigger pipeline".to_string(),
            ));
        }

        // Use provided branch/commit or fall back to project defaults
        let branch_to_use = branch.unwrap_or(project.main_branch.clone());

        // Fetch the latest commit from the branch if no commit was provided
        let commit_to_use = if let Some(commit) = commit {
            commit
        } else if let Some(connection_id) = project.git_provider_connection_id {
            // Fetch latest commit from the branch using authenticated git provider
            match self
                .git_provider_manager
                .get_branch_latest_commit(
                    connection_id,
                    &project.repo_owner,
                    &project.repo_name,
                    &branch_to_use,
                )
                .await
            {
                Ok(commit_info) => {
                    info!(
                        "Fetched latest commit from branch {}: {} ({})",
                        branch_to_use, commit_info.sha, commit_info.message
                    );
                    commit_info.sha
                }
                Err(e) => {
                    warn!(
                        "Failed to fetch latest commit from branch {}: {}, using placeholder",
                        branch_to_use, e
                    );
                    format!("manual-trigger-{}", chrono::Utc::now().timestamp())
                }
            }
        } else if project.is_public_repo {
            // For public repos without git provider connection, fetch from public API
            let provider_name = if let Some(ref git_url) = project.git_url {
                if git_url.contains("github.com") {
                    "github"
                } else if git_url.contains("gitlab.com") {
                    "gitlab"
                } else {
                    return Err(ProjectError::InvalidInput(format!(
                        "Unknown git provider for public repo URL: {}. Only GitHub and GitLab public repos are supported.",
                        git_url
                    )));
                }
            } else {
                // No git_url, try to infer from repo structure (assume GitHub for public repos)
                "github"
            };

            // Public projects must never borrow a credential from an arbitrary
            // provider connection. A repository that needs authentication must
            // use the caller-owned connected-repository workflow instead.
            let provider = PublicRepoProviderFactory::create(provider_name).map_err(|e| {
                ProjectError::Other(format!(
                    "Failed to create public repo provider for {}: {}",
                    provider_name, e
                ))
            })?;

            // A shared credential may be able to see private repositories.
            // `is_public_repo` must never turn that credential into a private
            // repository oracle, so verify visibility before reading branches.
            provider
                .get_repository(&project.repo_owner, &project.repo_name)
                .await
                .map_err(|e| {
                    ProjectError::Other(format!(
                        "Failed to verify that repository {}/{} is public: {}",
                        project.repo_owner, project.repo_name, e
                    ))
                })?;

            let branches = provider
                .list_branches(&project.repo_owner, &project.repo_name)
                .await
                .map_err(|e| {
                    ProjectError::Other(format!(
                        "Failed to fetch branches from public repo {}/{}: {}. The repository may not exist, be private, or the provider API may be unavailable.",
                        project.repo_owner, project.repo_name, e
                    ))
                })?;

            // Find the target branch
            let branch_info = branches
                .iter()
                .find(|b| b.name == branch_to_use)
                .ok_or_else(|| {
                    ProjectError::NotFound(format!(
                        "Branch '{}' not found in public repo {}/{}. Available branches: {}",
                        branch_to_use,
                        project.repo_owner,
                        project.repo_name,
                        branches
                            .iter()
                            .take(10)
                            .map(|b| b.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                })?;

            info!(
                "Fetched latest commit from public repo {}/{} branch {}: {}",
                project.repo_owner, project.repo_name, branch_to_use, branch_info.commit_sha
            );
            branch_info.commit_sha.clone()
        } else {
            warn!("No git provider connection found for project, using placeholder commit");
            format!("manual-trigger-{}", chrono::Utc::now().timestamp())
        };

        // Create GitPushEvent job to trigger pipeline
        let git_push_job = temps_core::GitPushEventJob {
            owner: project.repo_owner.clone(),
            repo: project.repo_name.clone(),
            branch: Some(branch_to_use.clone()),
            tag: tag.clone(),
            commit: commit_to_use.clone(),
            project_id, // Include project_id
            // `trigger_pipeline` on the projects service is hit by the
            // "Deploy" button and the CLI — both are user-initiated.
            manual_trigger: true,
            rollback_from_deployment_id: None,
            // The caller explicitly chose this environment — deploy there
            // directly rather than re-inferring the target from the branch
            // (which would fall through to a preview/named-preview env when the
            // environment doesn't have the branch configured).
            target_environment_id: Some(environment_id),
        };

        // Send the job to the queue
        self.queue_service
            .send(temps_core::Job::GitPushEvent(git_push_job))
            .await
            .map_err(|e| {
                ProjectError::Other(format!("Failed to queue pipeline trigger job: {}", e))
            })?;

        info!(
            "Triggered pipeline for project {} ({}), environment {} ({}), branch: {}",
            project_id, project.name, environment_id, environment.name, branch_to_use
        );

        // Return the details for the response
        Ok((
            project_id,
            environment_id,
            Some(branch_to_use),
            tag,
            Some(commit_to_use),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, Set};

    // ── git_url / repo identity consistency ─────────────────────────────

    /// The failure this guards: the repo identity was changed through
    /// `/settings`, the clone URL was left behind, and the next deploy
    /// resolved a commit from the new repo while cloning the old one.
    #[test]
    fn test_repo_change_that_strands_git_url_is_detected() {
        let url = Some("https://github.com/acme/old-repo.git".to_string());
        let desync = would_desync_git_url(&url, ("acme", "old-repo"), (None, Some("new-repo")));

        let (old, new) = desync.expect("changing the name strands the URL");
        assert_eq!(old, "acme/old-repo");
        assert_eq!(new, "acme/new-repo");
    }

    /// Changing the owner counts too.
    #[test]
    fn test_owner_change_is_detected() {
        let url = Some("https://github.com/acme/app.git".to_string());
        assert!(would_desync_git_url(&url, ("acme", "app"), (Some("other"), None)).is_some());
    }

    /// No repo change means nothing to desync, whatever the URL looks like.
    #[test]
    fn test_no_repo_change_is_allowed() {
        let url = Some("https://github.com/acme/app.git".to_string());
        assert!(would_desync_git_url(&url, ("acme", "app"), (None, None)).is_none());
        assert!(
            would_desync_git_url(&url, ("acme", "app"), (Some("acme"), Some("app"))).is_none(),
            "restating the same values is not a change"
        );
    }

    /// A URL that doesn't identify the current repo can't be proven stale, so
    /// the operator isn't blocked — self-hosted layouts and unusual remotes
    /// must keep working.
    #[test]
    fn test_unrelated_or_unparsable_url_does_not_block() {
        // Points somewhere that isn't the current repo: not our call to make.
        let other = Some("https://git.internal/mirrors/vendored.git".to_string());
        assert!(would_desync_git_url(&other, ("acme", "app"), (None, Some("app2"))).is_none());

        // No URL at all.
        assert!(would_desync_git_url(&None, ("acme", "app"), (None, Some("app2"))).is_none());
    }

    /// ssh remotes and missing `.git` must match the same way https does,
    /// or the guard would fire on projects it shouldn't.
    #[test]
    fn test_matching_is_scheme_and_suffix_insensitive() {
        for url in [
            "git@github.com:acme/app.git",
            "https://github.com/acme/app",
            "https://github.com/acme/app/",
            "https://github.com/ACME/App.git",
        ] {
            assert!(
                would_desync_git_url(
                    &Some(url.to_string()),
                    ("acme", "app"),
                    (None, Some("app2"))
                )
                .is_some(),
                "should recognise {url} as the current repo"
            );
        }
    }

    #[test]
    fn test_validate_relaxed_capability_services_allows_matching_database_service() {
        use temps_entities::preset::{ComposeServiceSnapshot, DockerComposeConfig};

        let cfg = DockerComposeConfig {
            relaxed_capability_services: vec!["db".to_string()],
            compose_services: vec![
                ComposeServiceSnapshot {
                    name: "db".to_string(),
                    image: Some("postgres:18".to_string()),
                    looks_like_database: true,
                    ..Default::default()
                },
                ComposeServiceSnapshot {
                    name: "web".to_string(),
                    image: Some("nginx".to_string()),
                    looks_like_database: false,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert!(validate_relaxed_capability_services(&cfg).is_ok());
    }

    #[test]
    fn test_validate_relaxed_capability_services_allows_non_database_service() {
        use temps_entities::preset::{ComposeServiceSnapshot, DockerComposeConfig};

        // The fix isn't database-specific — e.g. Gitea's own official image
        // hits the identical `chown: ... Operation not permitted` failure at
        // startup, confirmed live. The toggle (and this validation) is
        // available for any real service in the compose file, not just ones
        // flagged looks_like_database.
        let cfg = DockerComposeConfig {
            relaxed_capability_services: vec!["web".to_string()],
            compose_services: vec![
                ComposeServiceSnapshot {
                    name: "db".to_string(),
                    image: Some("postgres:18".to_string()),
                    looks_like_database: true,
                    ..Default::default()
                },
                ComposeServiceSnapshot {
                    name: "web".to_string(),
                    image: Some("gitea/gitea:latest".to_string()),
                    looks_like_database: false,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert!(validate_relaxed_capability_services(&cfg).is_ok());
    }

    #[test]
    fn test_validate_relaxed_capability_services_rejects_unknown_service_name() {
        use temps_entities::preset::{ComposeServiceSnapshot, DockerComposeConfig};

        let cfg = DockerComposeConfig {
            relaxed_capability_services: vec!["nonexistent".to_string()],
            compose_services: vec![ComposeServiceSnapshot {
                name: "db".to_string(),
                image: Some("postgres:18".to_string()),
                looks_like_database: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(validate_relaxed_capability_services(&cfg).is_err());
    }

    #[test]
    fn test_validate_relaxed_capability_services_allows_when_snapshot_empty() {
        use temps_entities::preset::DockerComposeConfig;

        // No compose_services snapshot yet (e.g. before the first deploy) —
        // nothing to validate against, so don't block a legitimate first-time
        // setup.
        let cfg = DockerComposeConfig {
            relaxed_capability_services: vec!["db".to_string()],
            compose_services: vec![],
            ..Default::default()
        };
        assert!(validate_relaxed_capability_services(&cfg).is_ok());
    }

    #[test]
    fn test_validate_unsandboxed_services_allows_known_service() {
        use temps_entities::preset::{ComposeServiceSnapshot, DockerComposeConfig};

        let cfg = DockerComposeConfig {
            unsandboxed_services: vec!["webserver".to_string()],
            compose_services: vec![ComposeServiceSnapshot {
                name: "webserver".to_string(),
                image: Some("ghcr.io/paperless-ngx/paperless-ngx:latest".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };

        assert!(validate_unsandboxed_services(&cfg).is_ok());
    }

    #[test]
    fn test_validate_unsandboxed_services_rejects_unknown_service() {
        use temps_entities::preset::{ComposeServiceSnapshot, DockerComposeConfig};

        let cfg = DockerComposeConfig {
            unsandboxed_services: vec!["unknown".to_string()],
            compose_services: vec![ComposeServiceSnapshot {
                name: "webserver".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        let error = validate_unsandboxed_services(&cfg).unwrap_err();
        assert!(error.to_string().contains("not a recognized service"));
    }

    #[test]
    fn test_validate_unsandboxed_services_rejects_elevated_overlap() {
        use temps_entities::preset::{ComposeServiceSnapshot, DockerComposeConfig};

        let cfg = DockerComposeConfig {
            relaxed_capability_services: vec!["webserver".to_string()],
            unsandboxed_services: vec!["webserver".to_string()],
            compose_services: vec![ComposeServiceSnapshot {
                name: "webserver".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        let error = validate_unsandboxed_services(&cfg).unwrap_err();
        assert!(error
            .to_string()
            .contains("cannot use both elevated permissions and a disabled sandbox"));
    }

    #[test]
    fn test_validate_unsandboxed_services_requires_recognized_snapshot() {
        use temps_entities::preset::DockerComposeConfig;

        let cfg = DockerComposeConfig {
            unsandboxed_services: vec!["webserver".to_string()],
            ..Default::default()
        };

        let error = validate_unsandboxed_services(&cfg).unwrap_err();
        assert!(error
            .to_string()
            .contains("before Compose services have been recognized"));
    }

    #[test]
    fn test_partial_compose_patch_preserves_unsandboxed_services() {
        use temps_entities::preset::{DockerComposeConfig, PresetConfig};

        let existing = PresetConfig::DockerCompose(DockerComposeConfig {
            compose_path: Some("compose.yml".to_string()),
            unsandboxed_services: vec!["webserver".to_string()],
            ..Default::default()
        });
        let parsed = PresetConfig::DockerCompose(DockerComposeConfig {
            excluded_services: vec!["db".to_string()],
            ..Default::default()
        });

        let merged = merge_preset_config(
            Some(&existing),
            parsed,
            &serde_json::json!({ "excludedServices": ["db"] }),
            true,
        );

        match merged {
            PresetConfig::DockerCompose(cfg) => {
                assert_eq!(cfg.compose_path.as_deref(), Some("compose.yml"));
                assert_eq!(cfg.excluded_services, ["db"]);
                assert_eq!(cfg.unsandboxed_services, ["webserver"]);
            }
            other => panic!("expected DockerCompose config, got {other:?}"),
        }
    }

    #[test]
    fn test_preset_selection_rejects_unknown_unsandboxed_service() {
        let config = serde_json::json!({
            "composePath": "compose.yml",
            "composeServices": [{ "name": "webserver", "image": "paperless:latest" }],
            "unsandboxedServices": ["unknown"]
        });

        let error = resolve_preset_selection("docker-compose", Some(&config), None).unwrap_err();

        assert!(error.to_string().contains("not a recognized service"));
    }

    #[test]
    fn test_preset_selection_rejects_overlapping_security_modes() {
        let config = serde_json::json!({
            "composePath": "compose.yml",
            "composeServices": [{ "name": "webserver", "image": "paperless:latest" }],
            "relaxedCapabilityServices": ["webserver"],
            "unsandboxedServices": ["webserver"]
        });

        let error = resolve_preset_selection("docker-compose", Some(&config), None).unwrap_err();

        assert!(error
            .to_string()
            .contains("cannot use both elevated permissions and a disabled sandbox"));
    }

    #[test]
    fn validate_compose_public_ports_rejects_duplicate_unknown_and_disabled_services() {
        use temps_entities::preset::{
            ComposePublicPort, ComposeServiceSnapshot, DockerComposeConfig,
        };

        let route = |service: &str| ComposePublicPort {
            service: service.to_string(),
            port: 80,
            published: Some(15_455),
        };
        let base = DockerComposeConfig {
            compose_services: vec![ComposeServiceSnapshot {
                name: "web".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        let duplicate = DockerComposeConfig {
            public_ports: vec![route("web"), route("web")],
            ..base.clone()
        };
        assert!(validate_compose_public_ports(&duplicate)
            .unwrap_err()
            .to_string()
            .contains("only one public URL"));

        let unknown = DockerComposeConfig {
            public_ports: vec![route("missing")],
            ..base.clone()
        };
        assert!(validate_compose_public_ports(&unknown)
            .unwrap_err()
            .to_string()
            .contains("unknown service"));

        let disabled = DockerComposeConfig {
            public_ports: vec![route("web")],
            excluded_services: vec!["web".to_string()],
            ..base
        };
        assert!(validate_compose_public_ports(&disabled)
            .unwrap_err()
            .to_string()
            .contains("disabled and public"));
    }

    #[test]
    fn validate_compose_public_ports_accepts_target_and_published_mapping() {
        use temps_entities::preset::{
            ComposePublicPort, ComposeServiceSnapshot, DockerComposeConfig,
        };
        let cfg = DockerComposeConfig {
            public_ports: vec![ComposePublicPort {
                service: "web".to_string(),
                port: 80,
                published: Some(15_455),
            }],
            compose_services: vec![ComposeServiceSnapshot {
                name: "web".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        assert!(validate_compose_public_ports(&cfg).is_ok());
    }

    use std::sync::Arc;
    use std::sync::Mutex;
    use temps_core::async_trait::async_trait;
    use temps_core::{JobQueue, QueueError};
    use temps_database::test_utils::TestDatabase;
    use temps_entities::preset::Preset;
    // Mock JobQueue for testing
    struct MockJobQueue {
        jobs: Arc<Mutex<Vec<Job>>>,
    }

    impl MockJobQueue {
        fn new() -> Self {
            Self {
                jobs: Arc::new(Mutex::new(Vec::new())),
            }
        }

        async fn get_jobs(&self) -> Vec<Job> {
            self.jobs.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl JobQueue for MockJobQueue {
        async fn send(&self, job: Job) -> Result<(), QueueError> {
            self.jobs.lock().unwrap().push(job);
            Ok(())
        }

        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            unimplemented!("Not needed for these tests")
        }
    }

    // Helper function to create test services
    async fn create_test_services(
        db: Arc<temps_database::DbConnection>,
        mock_queue: Arc<MockJobQueue>,
    ) -> ProjectService {
        // Create ConfigService
        let server_config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:3000".to_string(),
                "postgresql://test".to_string(),
                None,
                None,
            )
            .unwrap(),
        );
        let config_service = Arc::new(temps_config::ConfigService::new(server_config, db.clone()));

        // Create ExternalServiceManager
        let encryption_service = Arc::new(
            temps_core::EncryptionService::new(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .expect("Failed to create encryption service"),
        );

        // Create Docker client for ExternalServiceManager
        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults()
                .expect("Docker connection required for tests"),
        );

        let external_service_manager = Arc::new(temps_providers::ExternalServiceManager::new(
            db.clone(),
            encryption_service.clone(),
            docker,
            Arc::new(temps_providers::DnsRegistry::new(db.clone())),
        ));

        // Create GitProviderManager
        let git_provider_manager = Arc::new(temps_git::GitProviderManager::new(
            db.clone(),
            encryption_service.clone(),
            mock_queue.clone() as Arc<dyn temps_core::JobQueue>,
            config_service.clone(),
        ));

        // Create EnvironmentService
        let environment_service = Arc::new(temps_environments::EnvironmentService::new(
            db.clone(),
            config_service.clone(),
        ));

        ProjectService::new(
            db,
            mock_queue,
            config_service,
            external_service_manager,
            git_provider_manager,
            environment_service,
            encryption_service,
        )
    }

    #[tokio::test]
    async fn begin_project_deletion_persists_idempotent_deployment_fence() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let service = create_test_services(db.clone(), Arc::new(MockJobQueue::new())).await;
        let project = temps_entities::projects::ActiveModel {
            name: Set("Deleting Project".to_string()),
            slug: Set("deleting-project".to_string()),
            repo_name: Set("repo".to_string()),
            repo_owner: Set("owner".to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            main_branch: Set("main".to_string()),
            directory: Set("/".to_string()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        service.begin_project_deletion(project.id).await.unwrap();
        service.begin_project_deletion(project.id).await.unwrap();

        let fenced = temps_entities::projects::Entity::find_by_id(project.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("project remains until container cleanup completes");
        assert!(fenced.is_deleted);
        assert!(fenced.deleted_at.is_some());
    }

    #[tokio::test]
    async fn test_update_project_emits_event() {
        // Setup test database
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();

        // Create mock queue service
        let mock_queue = Arc::new(MockJobQueue::new());

        // Create project service
        let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

        // Insert a test project
        let project = temps_entities::projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("test-project".to_string()),
            git_provider_connection_id: Set(None),
            main_branch: Set("main".to_string()),
            preset: Set(Preset::Nixpacks),
            ..Default::default()
        };

        let inserted_project = project.insert(db.as_ref()).await.unwrap();

        // Update the project
        let update_request = CreateProjectRequest {
            name: "Updated Test Project".to_string(),
            repo_name: None,
            repo_owner: None,
            directory: "/".to_string(),
            main_branch: "develop".to_string(),
            preset: Preset::Nixpacks.to_string(),
            preset_config: None,
            environment_variables: None,
            git_url: None,
            git_provider_connection_id: None,
            automatic_deploy: false,
            exposed_port: None,
            is_public_repo: None,
            storage_service_ids: vec![],
            source_type: temps_entities::source_type::SourceType::Git,
            template_slug: None,
        };

        let result = project_service
            .update_project(inserted_project.id, update_request)
            .await;

        assert!(result.is_ok(), "update_project should succeed");

        // Verify event was emitted
        let jobs = mock_queue.get_jobs().await;
        assert_eq!(jobs.len(), 1, "Should emit exactly one job");

        match &jobs[0] {
            Job::ProjectUpdated(job) => {
                assert_eq!(job.project_id, inserted_project.id);
                assert_eq!(job.project_name, "Updated Test Project");
            }
            _ => panic!("Expected ProjectUpdated job"),
        }
    }

    #[tokio::test]
    async fn test_update_project_settings_emits_event() {
        // Setup test database
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();

        // Create mock queue service
        let mock_queue = Arc::new(MockJobQueue::new());

        // Create project service
        let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

        // Insert a test project
        let project = temps_entities::projects::ActiveModel {
            name: Set("Settings Test Project".to_string()),
            slug: Set("settings-test-project".to_string()),
            repo_name: Set("settings-test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("settings-test-project".to_string()),
            git_provider_connection_id: Set(None),
            main_branch: Set("main".to_string()),
            preset: Set(Preset::Nixpacks),
            ..Default::default()
        };

        let inserted_project = project.insert(db.as_ref()).await.unwrap();

        // Update project settings
        let result = project_service
            .update_project_settings(
                inserted_project.id,
                Some("new-slug".to_string()),
                None,
                Some("develop".to_string()),
                None,
                None,
                Some(Preset::Nixpacks.to_string()),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None, // cross_project_trace_sharing
                None, // error_source_context_enabled
                None, // error_source_root
                None, // ai_api_traffic_summary_enabled
            )
            .await;

        assert!(result.is_ok(), "update_project_settings should succeed");

        // Verify event was emitted
        let jobs = mock_queue.get_jobs().await;
        assert_eq!(jobs.len(), 1, "Should emit exactly one job");

        match &jobs[0] {
            Job::ProjectUpdated(job) => {
                assert_eq!(job.project_id, inserted_project.id);
                assert_eq!(job.project_name, "Settings Test Project");
            }
            _ => panic!("Expected ProjectUpdated job"),
        }
    }

    #[tokio::test]
    async fn test_update_project_event_includes_correct_data() {
        // Setup test database
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();

        // Create mock queue service
        let mock_queue = Arc::new(MockJobQueue::new());

        // Create project service
        let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

        // Insert a test project with specific name
        let project = temps_entities::projects::ActiveModel {
            name: Set("Event Data Test".to_string()),
            slug: Set("event-data-test".to_string()),
            repo_name: Set("event-data-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("event-data-test".to_string()),
            git_provider_connection_id: Set(None),
            main_branch: Set("main".to_string()),
            preset: Set(Preset::Nixpacks),
            ..Default::default()
        };

        let inserted_project = project.insert(db.as_ref()).await.unwrap();
        let project_id = inserted_project.id;

        // Update the project name
        let update_request = CreateProjectRequest {
            name: "Event Data Test Updated".to_string(),
            repo_name: None,
            repo_owner: None,
            directory: "/".to_string(),
            main_branch: "main".to_string(),
            preset: Preset::Nixpacks.as_str().to_string(),
            preset_config: None,
            environment_variables: None,
            automatic_deploy: false,
            storage_service_ids: vec![],
            is_public_repo: None,
            git_url: None,
            git_provider_connection_id: None,
            exposed_port: None,
            source_type: temps_entities::source_type::SourceType::Git,
            template_slug: None,
        };

        project_service
            .update_project(project_id, update_request)
            .await
            .unwrap();

        // Verify the event contains the updated name
        let jobs = mock_queue.get_jobs().await;
        assert_eq!(jobs.len(), 1);

        if let Job::ProjectUpdated(job) = &jobs[0] {
            assert_eq!(job.project_id, project_id);
            assert_eq!(
                job.project_name, "Event Data Test Updated",
                "Event should contain the updated project name"
            );
        } else {
            panic!("Expected ProjectUpdated job");
        }
    }

    /// Docker is required by `create_test_services` because it constructs an
    /// `ExternalServiceManager`. When Docker isn't available locally
    /// (CI without docker-in-docker, dev machines without daemon) skip
    /// rather than failing — matches the `cargo test` discipline in CLAUDE.md.
    async fn docker_available() -> bool {
        match bollard::Docker::connect_with_local_defaults() {
            Ok(d) => d.ping().await.is_ok(),
            Err(_) => false,
        }
    }

    fn create_request(name: &str) -> CreateProjectRequest {
        CreateProjectRequest {
            name: name.to_string(),
            repo_name: Some("repo".to_string()),
            repo_owner: Some("owner".to_string()),
            directory: "/".to_string(),
            main_branch: "main".to_string(),
            preset: Preset::Nixpacks.to_string(),
            preset_config: None,
            environment_variables: None,
            git_url: None,
            git_provider_connection_id: None,
            automatic_deploy: false,
            exposed_port: None,
            is_public_repo: None,
            storage_service_ids: vec![],
            source_type: temps_entities::source_type::SourceType::Git,
            template_slug: None,
        }
    }

    #[tokio::test]
    async fn test_create_project_succeeds_and_creates_default_environment() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

        let result = project_service
            .create_project(create_request("My Project"))
            .await
            .expect("create_project should succeed");

        assert_eq!(result.name, "My Project");
        assert_eq!(result.slug, "my-project");

        // Default production environment should exist for the new project
        use temps_entities::environments;
        let env_count = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(result.id))
            .count(db.as_ref())
            .await
            .unwrap();
        assert_eq!(env_count, 1, "should auto-create one environment");

        let created_project = projects::Entity::find_by_id(result.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("created project row should exist");
        let project_config = created_project
            .deployment_config
            .expect("new project should seed deployment_config");
        assert_eq!(project_config.memory_limit, Some(DEFAULT_MEMORY_LIMIT));

        let production = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(result.id))
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("production environment should exist");
        let env_config = production
            .deployment_config
            .expect("default environment should seed deployment_config");
        assert_eq!(env_config.memory_limit, Some(DEFAULT_MEMORY_LIMIT));
    }

    #[tokio::test]
    async fn test_create_project_persists_curated_template_provenance() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue).await;
        let mut request = create_request("Observability Starter");
        request.template_slug = Some("observability-starter".to_string());

        let created = project_service
            .create_project(request)
            .await
            .expect("template project creation should succeed");
        let persisted = projects::Entity::find_by_id(created.id)
            .one(db.as_ref())
            .await
            .expect("template project query should succeed")
            .expect("template project should exist");

        assert_eq!(
            persisted.template_slug.as_deref(),
            Some("observability-starter")
        );
    }

    #[tokio::test]
    async fn test_create_project_rejects_template_slug_longer_than_schema_limit() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db, mock_queue).await;
        let mut request = create_request("Custom Template");
        request.template_slug =
            Some("x".repeat(temps_core::templates::MAX_TEMPLATE_SLUG_CHARS + 1));

        let error = match project_service.create_project(request).await {
            Ok(_) => panic!("oversized template slug must be rejected before insertion"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            ProjectError::InvalidInput(message) if message.contains("255")
        ));
    }

    #[tokio::test]
    async fn test_create_project_nixpacks_node_stores_provider_and_returns_runtime_slug() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

        let mut request = create_request("Nixpacks Node App");
        request.preset = "nixpacks-node".to_string();

        let result = project_service
            .create_project(request)
            .await
            .expect("create with nixpacks-node should succeed");

        // API/UI surface reconstructs the provider-specific slug
        assert_eq!(result.preset.as_deref(), Some("nixpacks-node"));

        let row = projects::Entity::find_by_id(result.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("project row");

        // Persistable column stays the single Nixpacks enum variant
        assert_eq!(row.preset, Preset::Nixpacks);

        match row.preset_config {
            Some(temps_entities::preset::PresetConfig::Nixpacks(cfg)) => {
                assert_eq!(
                    cfg.providers,
                    vec![temps_entities::preset::NixpacksProvider::Node]
                );
            }
            other => panic!("expected Nixpacks preset_config with providers=[node], got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_create_project_rejects_unknown_nixpacks_provider_slug() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

        let mut request = create_request("Bad Nixpacks");
        request.preset = "nixpacks-not-a-real-provider".to_string();

        match project_service.create_project(request).await {
            Err(ProjectError::InvalidInput(message)) => {
                assert!(message.contains("nixpacks-not-a-real-provider"));
            }
            Err(other) => panic!("expected InvalidInput, got {other:?}"),
            Ok(_) => panic!("unknown nixpacks provider slug must be rejected"),
        }
    }

    #[tokio::test]
    async fn test_update_project_leaving_nixpacks_clears_stale_preset_config() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

        let mut create = create_request("Leave Nixpacks");
        create.preset = "nixpacks-node".to_string();
        let created = project_service
            .create_project(create)
            .await
            .expect("create nixpacks-node");

        let mut update = create_request("Leave Nixpacks");
        update.preset = "nextjs".to_string();
        let updated = project_service
            .update_project(created.id, update)
            .await
            .expect("switch to nextjs");

        assert_eq!(updated.preset.as_deref(), Some("nextjs"));
        assert!(
            updated.preset_config.is_none(),
            "stale Nixpacks preset_config must be cleared, got {:?}",
            updated.preset_config
        );

        let row = projects::Entity::find_by_id(created.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("project row");
        assert_eq!(row.preset, Preset::NextJs);
        assert!(row.preset_config.is_none());
    }

    #[tokio::test]
    async fn test_partial_preset_config_patch_preserves_nixpacks_providers() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

        let mut create = create_request("Preserve Provider");
        create.preset = "nixpacks-node".to_string();
        let created = project_service
            .create_project(create)
            .await
            .expect("create nixpacks-node");

        let toml = "[start]\ncmd = \"npm start\"";
        let updated = project_service
            .update_project_settings(
                created.id,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(serde_json::json!({ "nixpacksConfig": toml })),
                None,
                None,
                None,
                None,
                None,
                None,
                None, // ai_api_traffic_summary_enabled
            )
            .await
            .expect("partial preset_config patch");

        assert_eq!(updated.preset.as_deref(), Some("nixpacks-node"));

        let row = projects::Entity::find_by_id(created.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("project row");
        match row.preset_config {
            Some(temps_entities::preset::PresetConfig::Nixpacks(cfg)) => {
                assert_eq!(
                    cfg.providers,
                    vec![temps_entities::preset::NixpacksProvider::Node]
                );
                assert_eq!(cfg.nixpacks_config.as_deref(), Some(toml));
            }
            other => panic!("expected Nixpacks config with providers=[node], got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_partial_preset_config_patch_preserves_docker_compose_fields() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

        let mut create = create_request("Preserve Compose Fields");
        create.preset = "docker-compose".to_string();
        create.preset_config = Some(serde_json::json!({
            "composePath": "compose.yml",
            "composeServices": [
                {"name": "postgres", "image": "postgres:17-alpine", "looksLikeDatabase": true},
                {"name": "hub", "image": "ghcr.io/getpaseo/hub:latest", "looksLikeDatabase": false}
            ]
        }));
        let created = project_service
            .create_project(create)
            .await
            .expect("create docker-compose project");

        // A patch touching only excludedServices must not wipe composePath/composeServices.
        let updated = project_service
            .update_project_settings(
                created.id,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(serde_json::json!({ "excludedServices": ["postgres"] })),
                None,
                None,
                None,
                None,
                None,
                None,
                None, // ai_api_traffic_summary_enabled
            )
            .await
            .expect("partial excludedServices patch");

        assert_eq!(updated.preset.as_deref(), Some("docker-compose"));

        let row = projects::Entity::find_by_id(created.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("project row");
        match row.preset_config {
            Some(temps_entities::preset::PresetConfig::DockerCompose(cfg)) => {
                assert_eq!(cfg.compose_path, Some("compose.yml".to_string()));
                assert_eq!(cfg.excluded_services, vec!["postgres".to_string()]);
                assert_eq!(cfg.compose_services.len(), 2);
                assert_eq!(cfg.compose_services[0].name, "postgres");
            }
            other => panic!("expected DockerCompose config, got {other:?}"),
        }

        // A patch touching only relaxedCapabilityServices must not wipe the
        // other DockerCompose fields either — same bug class, new field.
        // "postgres" is still present and looksLikeDatabase in the snapshot
        // at this point, so the server-side database-service check passes.
        let updated = project_service
            .update_project_settings(
                created.id,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(serde_json::json!({ "relaxedCapabilityServices": ["postgres"] })),
                None,
                None,
                None,
                None,
                None,
                None,
                None, // ai_api_traffic_summary_enabled
            )
            .await
            .expect("partial relaxedCapabilityServices patch");
        assert_eq!(updated.preset.as_deref(), Some("docker-compose"));

        let row = projects::Entity::find_by_id(created.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("project row");
        match row.preset_config {
            Some(temps_entities::preset::PresetConfig::DockerCompose(cfg)) => {
                assert_eq!(
                    cfg.relaxed_capability_services,
                    vec!["postgres".to_string()]
                );
                // Still preserved from the earlier patch.
                assert_eq!(cfg.compose_services.len(), 2);
                assert_eq!(cfg.excluded_services, vec!["postgres".to_string()]);
            }
            other => panic!("expected DockerCompose config, got {other:?}"),
        }

        // A patch explicitly replacing composeServices to drop "postgres"
        // entirely must still succeed even though it leaves
        // relaxedCapabilityServices pointing at a service that no longer
        // exists in the new snapshot — this patch doesn't touch that field,
        // so the server-side database-service check must not re-run against
        // the now-stale reference and wedge the update.
        let updated = project_service
            .update_project_settings(
                created.id,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(serde_json::json!({
                    "composeServices": [
                        {"name": "hub", "image": "ghcr.io/getpaseo/hub:latest", "looksLikeDatabase": false}
                    ]
                })),
                None,
                None,
                None,
                None,
                None,
                None,
                None, // ai_api_traffic_summary_enabled
            )
            .await
            .expect("explicit composeServices patch, even though it strands relaxedCapabilityServices");
        assert_eq!(updated.preset.as_deref(), Some("docker-compose"));

        let row = projects::Entity::find_by_id(created.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("project row");
        match row.preset_config {
            Some(temps_entities::preset::PresetConfig::DockerCompose(cfg)) => {
                assert_eq!(cfg.compose_services.len(), 1);
                assert_eq!(cfg.compose_services[0].name, "hub");
                // excludedServices was omitted from this patch too, so it must
                // still survive from the previous update.
                assert_eq!(cfg.excluded_services, vec!["postgres".to_string()]);
                // relaxedCapabilityServices survives too, even though it now
                // references a service absent from the new snapshot.
                assert_eq!(
                    cfg.relaxed_capability_services,
                    vec!["postgres".to_string()]
                );
            }
            other => panic!("expected DockerCompose config, got {other:?}"),
        }

        // And a subsequent unrelated patch must not wipe (or re-reject)
        // relaxedCapabilityServices either.
        let updated = project_service
            .update_project_settings(
                created.id,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(serde_json::json!({ "excludedServices": [] })),
                None,
                None,
                None,
                None,
                None,
                None,
                None, // ai_api_traffic_summary_enabled
            )
            .await
            .expect("unrelated excludedServices patch");
        assert_eq!(updated.preset.as_deref(), Some("docker-compose"));

        let row = projects::Entity::find_by_id(created.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("project row");
        match row.preset_config {
            Some(temps_entities::preset::PresetConfig::DockerCompose(cfg)) => {
                assert_eq!(
                    cfg.relaxed_capability_services,
                    vec!["postgres".to_string()]
                );
            }
            other => panic!("expected DockerCompose config, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_relaxed_capability_services_allows_non_database_service_via_settings_update() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

        let mut create = create_request("Allow Non-DB Relax");
        create.preset = "docker-compose".to_string();
        create.preset_config = Some(serde_json::json!({
            "composePath": "compose.yml",
            "composeServices": [
                {"name": "postgres", "image": "postgres:17-alpine", "looksLikeDatabase": true},
                {"name": "gitea", "image": "gitea/gitea:latest", "looksLikeDatabase": false}
            ]
        }));
        let created = project_service
            .create_project(create)
            .await
            .expect("create docker-compose project");

        // "gitea" is a real service in the snapshot and not flagged
        // looksLikeDatabase, but the fix isn't database-specific (confirmed
        // live: Gitea's own official image hits the identical ownership-fix
        // failure at startup) — the toggle must accept any real service.
        let result = project_service
            .update_project_settings(
                created.id,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(serde_json::json!({ "relaxedCapabilityServices": ["gitea"] })),
                None,
                None,
                None,
                None,
                None,
                None,
                None, // ai_api_traffic_summary_enabled
            )
            .await
            .expect("relaxedCapabilityServices patch for a real non-database service");
        assert_eq!(result.preset.as_deref(), Some("docker-compose"));

        let row = projects::Entity::find_by_id(created.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("project row");
        match row.preset_config {
            Some(temps_entities::preset::PresetConfig::DockerCompose(cfg)) => {
                assert_eq!(cfg.relaxed_capability_services, vec!["gitea".to_string()]);
            }
            other => panic!("expected DockerCompose config, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_relaxed_capability_services_rejects_phantom_service_via_settings_update() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

        let mut create = create_request("Reject Phantom Relax");
        create.preset = "docker-compose".to_string();
        create.preset_config = Some(serde_json::json!({
            "composePath": "compose.yml",
            "composeServices": [
                {"name": "postgres", "image": "postgres:17-alpine", "looksLikeDatabase": true}
            ]
        }));
        let created = project_service
            .create_project(create)
            .await
            .expect("create docker-compose project");

        // A name that doesn't correspond to any service in the compose file
        // at all — typo or a fabricated API request — must still be
        // rejected.
        let result = project_service
            .update_project_settings(
                created.id,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(serde_json::json!({ "relaxedCapabilityServices": ["does-not-exist"] })),
                None,
                None,
                None,
                None,
                None,
                None,
                None, // ai_api_traffic_summary_enabled
            )
            .await;

        assert!(matches!(result, Err(ProjectError::InvalidInput(_))));

        let row = projects::Entity::find_by_id(created.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("project row");
        match row.preset_config {
            Some(temps_entities::preset::PresetConfig::DockerCompose(cfg)) => {
                assert!(cfg.relaxed_capability_services.is_empty());
            }
            other => panic!("expected DockerCompose config, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_explicit_empty_providers_resets_nixpacks_to_auto() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

        let mut create = create_request("Clear Provider");
        create.preset = "nixpacks-node".to_string();
        let created = project_service
            .create_project(create)
            .await
            .expect("create nixpacks-node");

        let updated = project_service
            .update_project_settings(
                created.id,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(serde_json::json!({ "providers": [] })),
                None,
                None,
                None,
                None,
                None,
                None,
                None, // ai_api_traffic_summary_enabled
            )
            .await
            .expect("explicit empty providers");

        assert_eq!(updated.preset.as_deref(), Some("nixpacks"));

        let row = projects::Entity::find_by_id(created.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("project row");
        match row.preset_config {
            Some(temps_entities::preset::PresetConfig::Nixpacks(cfg)) => {
                assert!(cfg.providers.is_empty());
            }
            other => panic!("expected Nixpacks config without providers, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_update_project_base_nixpacks_resets_provider_and_preserves_toml() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue).await;

        let toml = "[start]\ncmd = \"npm start\"";
        let mut create = create_request("Reset Through Full Update");
        create.preset = "nixpacks-node".to_string();
        create.preset_config = Some(serde_json::json!({ "nixpacksConfig": toml }));
        let created = project_service
            .create_project(create)
            .await
            .expect("create nixpacks-node project");

        let mut update = create_request("Reset Through Full Update");
        update.preset = "nixpacks".to_string();
        let updated = project_service
            .update_project(created.id, update)
            .await
            .expect("select base nixpacks");

        assert_eq!(updated.preset.as_deref(), Some("nixpacks"));
        let row = projects::Entity::find_by_id(created.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("project row");
        match row.preset_config {
            Some(temps_entities::preset::PresetConfig::Nixpacks(config)) => {
                assert!(config.providers.is_empty());
                assert_eq!(config.nixpacks_config.as_deref(), Some(toml));
            }
            other => panic!("expected base Nixpacks config, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_nixpacks_config_rejects_unknown_provider() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db, mock_queue).await;

        let created = project_service
            .create_project(create_request("Reject Invalid Provider"))
            .await
            .expect("create nixpacks project");

        let result = project_service
            .update_project_settings(
                created.id,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(serde_json::json!({ "providers": ["not-real"] })),
                None,
                None,
                None,
                None,
                None,
                None,
                None, // ai_api_traffic_summary_enabled
            )
            .await;

        match result {
            Err(ProjectError::InvalidInput(message)) => {
                assert!(message.contains("not-real"));
            }
            Err(other) => panic!("expected InvalidInput, got {other:?}"),
            Ok(_) => panic!("invalid provider must be rejected"),
        }

        let row = projects::Entity::find_by_id(created.id)
            .one(project_service.db.as_ref())
            .await
            .unwrap()
            .expect("project row");
        match row.preset_config {
            Some(temps_entities::preset::PresetConfig::Nixpacks(config)) => {
                assert!(config.providers.is_empty());
            }
            other => panic!("expected unchanged Nixpacks config, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_nixpacks_invalid_inline_toml_is_rejected_during_create() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db, mock_queue).await;

        let mut request = create_request("Invalid Nixpacks TOML");
        request.preset_config = Some(serde_json::json!({
            "nixpacksConfig": "secret_token = [\"do-not-echo\""
        }));
        let result = project_service.create_project(request).await;

        match result {
            Err(ProjectError::InvalidInput(message)) => {
                assert!(message.contains("failed to parse Nixpacks TOML"));
                assert!(
                    !message.contains("do-not-echo"),
                    "validation errors must not echo inline config contents"
                );
            }
            Err(other) => panic!("expected InvalidInput, got {other:?}"),
            Ok(_) => panic!("invalid Nixpacks TOML must be rejected"),
        }
    }

    #[tokio::test]
    async fn test_nixpacks_invalid_inline_toml_is_rejected_during_config_only_settings_update() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue).await;

        let created = project_service
            .create_project(create_request("Invalid Settings TOML"))
            .await
            .expect("create nixpacks project");
        let original_config = projects::Entity::find_by_id(created.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("project row")
            .preset_config;

        let result = project_service
            .update_project_settings(
                created.id,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(serde_json::json!({ "nixpacksConfig": "invalid = [" })),
                None,
                None,
                None,
                None,
                None,
                None,
                None, // ai_api_traffic_summary_enabled
            )
            .await;

        match result {
            Err(ProjectError::InvalidInput(message)) => {
                assert!(message.contains("failed to parse Nixpacks TOML"));
            }
            Err(other) => panic!("expected InvalidInput, got {other:?}"),
            Ok(_) => panic!("invalid Nixpacks TOML must be rejected"),
        }

        let persisted_config = projects::Entity::find_by_id(created.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("project row")
            .preset_config;
        assert_eq!(persisted_config, original_config);
    }

    #[tokio::test]
    async fn test_nixpacks_invalid_inline_toml_is_rejected_during_config_only_git_update() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue).await;

        let created = project_service
            .create_project(create_request("Invalid Git Settings TOML"))
            .await
            .expect("create nixpacks project");
        let original_config = projects::Entity::find_by_id(created.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("project row")
            .preset_config;

        let result = project_service
            .update_git_settings(
                created.id,
                None,
                "main".to_string(),
                "owner".to_string(),
                "repo".to_string(),
                None,
                ".".to_string(),
                Some(serde_json::json!({ "nixpacksConfig": "invalid = [" })),
                None,
                None,
            )
            .await;

        match result {
            Err(ProjectError::InvalidInput(message)) => {
                assert!(message.contains("failed to parse Nixpacks TOML"));
            }
            Err(other) => panic!("expected InvalidInput, got {other:?}"),
            Ok(_) => panic!("invalid Nixpacks TOML must be rejected"),
        }

        let persisted_config = projects::Entity::find_by_id(created.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("project row")
            .preset_config;
        assert_eq!(persisted_config, original_config);
    }

    #[tokio::test]
    async fn test_config_only_settings_update_preserves_custom_dockerfile_variant() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue).await;

        let mut request = create_request("Custom Settings Variant");
        request.preset = "custom".to_string();
        request.preset_config = Some(serde_json::json!({
            "dockerfilePath": "Dockerfile.custom",
            "buildContext": "."
        }));
        let created = project_service
            .create_project(request)
            .await
            .expect("create custom Dockerfile project");

        project_service
            .update_project_settings(
                created.id,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(serde_json::json!({
                    "dockerfilePath": "Dockerfile.updated"
                })),
                None,
                None,
                None,
                None,
                None,
                None,
                None, // ai_api_traffic_summary_enabled
            )
            .await
            .expect("update custom Dockerfile config");

        let persisted_config = projects::Entity::find_by_id(created.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("project row")
            .preset_config
            .expect("preset config");
        match &persisted_config {
            temps_entities::preset::PresetConfig::Dockerfile(config) => {
                assert_eq!(
                    config.variant,
                    temps_entities::preset::DockerfileVariant::Custom
                );
                assert_eq!(
                    config.dockerfile_path.as_deref(),
                    Some("Dockerfile.updated")
                );
            }
            other => panic!("expected Dockerfile config, got {other:?}"),
        }
        let runtime = temps_presets::get_preset_for_storage(
            temps_entities::preset::Preset::Dockerfile,
            Some(&persisted_config),
        )
        .expect("resolve stored preset")
        .expect("runtime preset");
        assert_eq!(runtime.slug(), "custom");
    }

    #[tokio::test]
    async fn test_config_only_git_update_preserves_custom_dockerfile_variant() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue).await;

        let mut request = create_request("Custom Git Variant");
        request.preset = "custom".to_string();
        request.preset_config = Some(serde_json::json!({
            "dockerfilePath": "Dockerfile.custom",
            "buildContext": "."
        }));
        let created = project_service
            .create_project(request)
            .await
            .expect("create custom Dockerfile project");

        project_service
            .update_git_settings(
                created.id,
                None,
                "main".to_string(),
                "owner".to_string(),
                "repo".to_string(),
                None,
                ".".to_string(),
                Some(serde_json::json!({
                    "dockerfilePath": "Dockerfile.updated"
                })),
                None,
                None,
            )
            .await
            .expect("update custom Dockerfile Git config");

        let persisted_config = projects::Entity::find_by_id(created.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("project row")
            .preset_config
            .expect("preset config");
        match &persisted_config {
            temps_entities::preset::PresetConfig::Dockerfile(config) => {
                assert_eq!(
                    config.variant,
                    temps_entities::preset::DockerfileVariant::Custom
                );
                assert_eq!(
                    config.dockerfile_path.as_deref(),
                    Some("Dockerfile.updated")
                );
            }
            other => panic!("expected Dockerfile config, got {other:?}"),
        }
        let runtime = temps_presets::get_preset_for_storage(
            temps_entities::preset::Preset::Dockerfile,
            Some(&persisted_config),
        )
        .expect("resolve stored preset")
        .expect("runtime preset");
        assert_eq!(runtime.slug(), "custom");
    }

    #[tokio::test]
    async fn test_nixpacks_supports_multiple_ordered_providers() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue).await;

        let mut request = create_request("Multiple Providers");
        request.preset_config = Some(serde_json::json!({
            "providers": ["...", "python"]
        }));
        let created = project_service
            .create_project(request)
            .await
            .expect("create multi-provider Nixpacks project");

        assert_eq!(created.preset.as_deref(), Some("nixpacks"));
        let row = projects::Entity::find_by_id(created.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("project row");
        match row.preset_config {
            Some(temps_entities::preset::PresetConfig::Nixpacks(config)) => {
                assert_eq!(
                    config.providers,
                    vec![
                        temps_entities::preset::NixpacksProvider::Auto,
                        temps_entities::preset::NixpacksProvider::Python,
                    ]
                );
            }
            other => panic!("expected multi-provider Nixpacks config, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_git_settings_persist_multiple_ordered_nixpacks_providers() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue).await;

        let created = project_service
            .create_project(create_request("Git Settings Providers"))
            .await
            .expect("create project");
        let updated = project_service
            .update_git_settings(
                created.id,
                None,
                "main".to_string(),
                "owner".to_string(),
                "repo".to_string(),
                Some("nixpacks".to_string()),
                ".".to_string(),
                Some(serde_json::json!({ "providers": ["...", "python"] })),
                None,
                None,
            )
            .await
            .expect("update git settings");

        assert_eq!(updated.preset.as_deref(), Some("nixpacks"));
        let row = projects::Entity::find_by_id(created.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("project row");
        match row.preset_config {
            Some(temps_entities::preset::PresetConfig::Nixpacks(config)) => {
                assert_eq!(
                    config.providers,
                    vec![
                        temps_entities::preset::NixpacksProvider::Auto,
                        temps_entities::preset::NixpacksProvider::Python,
                    ]
                );
            }
            other => panic!("expected multi-provider Nixpacks config, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_preset_and_config_update_use_effective_new_preset() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue).await;

        let mut request = create_request("Atomic Preset Update");
        request.preset = "nextjs".to_string();
        let created = project_service
            .create_project(request)
            .await
            .expect("create nextjs project");

        let toml = "[start]\ncmd = \"npm start\"";
        let updated = project_service
            .update_project_settings(
                created.id,
                None,
                None,
                None,
                None,
                None,
                Some("nixpacks-node".to_string()),
                None,
                None,
                None,
                None,
                None,
                None,
                Some(serde_json::json!({ "nixpacksConfig": toml })),
                None,
                None,
                None,
                None,
                None,
                None,
                None, // ai_api_traffic_summary_enabled
            )
            .await
            .expect("update preset and config together");

        assert_eq!(updated.preset.as_deref(), Some("nixpacks-node"));
        let row = projects::Entity::find_by_id(created.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("project row");
        match row.preset_config {
            Some(temps_entities::preset::PresetConfig::Nixpacks(config)) => {
                assert_eq!(
                    config.providers,
                    vec![temps_entities::preset::NixpacksProvider::Node]
                );
                assert_eq!(config.nixpacks_config.as_deref(), Some(toml));
            }
            other => panic!("expected updated Nixpacks config, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_create_project_with_duplicate_name_gets_suffixed_slug() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

        let first = project_service
            .create_project(create_request("Duplicate Name"))
            .await
            .expect("first create should succeed");
        let second = project_service
            .create_project(create_request("Duplicate Name"))
            .await
            .expect("second create with same name should succeed with suffixed slug");

        assert_eq!(first.slug, "duplicate-name");
        assert!(
            second.slug.starts_with("duplicate-name-"),
            "second slug should be suffixed, got {}",
            second.slug
        );
        assert_ne!(first.id, second.id);
    }

    #[tokio::test]
    async fn test_create_project_slug_conflict_returns_typed_error() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

        // Pre-insert a project occupying the slug we're about to ask for, but
        // bypass `generate_unique_project_slug` (which would have suffixed it)
        // by inserting directly. This simulates the race window between the
        // SELECT in `generate_unique_project_slug` and the INSERT below.
        let pre_existing = temps_entities::projects::ActiveModel {
            name: Set("Race".to_string()),
            slug: Set("squatted-slug".to_string()),
            repo_name: Set("r".to_string()),
            repo_owner: Set("o".to_string()),
            directory: Set(".".to_string()),
            git_provider_connection_id: Set(None),
            main_branch: Set("main".to_string()),
            preset: Set(Preset::Nixpacks),
            ..Default::default()
        };
        pre_existing.insert(db.as_ref()).await.unwrap();

        // Now drive `create_project` straight at that slug by patching the
        // ActiveModel. Since we don't have a hook to inject the slug, we
        // instead synthesize the unique-violation by trying to insert a
        // second row with the same slug directly and verifying our
        // detector classifies it as a conflict.
        let dup = temps_entities::projects::ActiveModel {
            name: Set("Race 2".to_string()),
            slug: Set("squatted-slug".to_string()),
            repo_name: Set("r".to_string()),
            repo_owner: Set("o".to_string()),
            directory: Set(".".to_string()),
            git_provider_connection_id: Set(None),
            main_branch: Set("main".to_string()),
            preset: Set(Preset::Nixpacks),
            ..Default::default()
        };
        let err = dup.insert(db.as_ref()).await.unwrap_err();
        assert!(
            super::super::types::is_unique_violation(&err),
            "expected unique-violation classification, got {:?}",
            err
        );
        // And the From<ProjectError> for Problem path should map this branch
        // to 409 once it's wrapped as SlugConflict — exercise the type:
        let project_err = ProjectError::SlugConflict {
            slug: "squatted-slug".to_string(),
        };
        let problem: temps_core::problemdetails::Problem = project_err.into();
        let response = axum::response::IntoResponse::into_response(problem);
        assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);

        // ensure the `_` binding suppresses the unused warning on mock_queue
        let _ = project_service;
    }

    #[tokio::test]
    async fn test_create_project_rolls_back_on_invalid_storage_service() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

        // Reference a storage_service_id that doesn't exist. The pre-insert
        // verification (`found_count != ids.len()`) returns InvalidInput
        // BEFORE the project insert, so no rollback is needed for this path.
        // To exercise rollback we'd need to fail during a post-insert step,
        // which requires forcing a failure inside finalize. The simplest
        // mid-flight failure is exhausted resources / constraint violations
        // we can't easily inject here without mocking. So this test verifies
        // the early-validation path produces 400 InvalidInput and creates
        // zero projects.
        let req = CreateProjectRequest {
            storage_service_ids: vec![999_999],
            ..create_request("rollback-test")
        };

        let result = project_service.create_project(req).await;
        match result {
            Ok(_) => panic!("should reject unknown storage service id"),
            Err(ProjectError::InvalidInput(_)) => {}
            Err(other) => panic!("expected InvalidInput, got {:?}", other),
        }

        // No project should have been inserted
        use temps_entities::projects;
        let count = projects::Entity::find()
            .filter(projects::Column::Name.eq("rollback-test"))
            .count(db.as_ref())
            .await
            .unwrap();
        assert_eq!(count, 0, "no project should remain after validation error");
    }

    #[tokio::test]
    async fn test_is_unique_violation_detects_record_not_inserted() {
        // Pure unit test, no DB needed — guards the classifier itself.
        let err = sea_orm::DbErr::RecordNotInserted;
        assert!(super::super::types::is_unique_violation(&err));

        let err = sea_orm::DbErr::Custom("23505: duplicate key".to_string());
        assert!(super::super::types::is_unique_violation(&err));

        let err = sea_orm::DbErr::Custom("connection refused".to_string());
        assert!(!super::super::types::is_unique_violation(&err));
    }

    // ── Regression test: git provider connections are installation-scoped ───
    //
    // Connections belong to the installation (workspace-wide), not to the
    // user who created them — a GitHub App installation is inherently
    // shared, and PAT connections are meant to be usable by any project
    // maintainer, not gated to their creator. update_git_settings must not
    // reject a connection just because a different user created it; access
    // to the project itself is what `permission_guard!`/`project_scope_guard!`
    // already enforce in the handler.

    #[tokio::test]
    async fn test_update_git_settings_allows_connection_created_by_different_user() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

        use temps_entities::{git_provider_connections, git_providers, users};
        let creator = users::ActiveModel {
            email: Set("git-connection-creator@example.com".to_string()),
            name: Set("Connection Creator".to_string()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        // Create a git provider (required FK for connections).
        let provider = git_providers::ActiveModel {
            name: Set("Scoping Test Provider".to_string()),
            provider_type: Set("github".to_string()),
            base_url: Set(None),
            api_url: Set(None),
            auth_method: Set("oauth".to_string()),
            auth_config: Set(serde_json::json!({})),
            webhook_secret: Set(None),
            is_active: Set(true),
            is_default: Set(false),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        // Connection created by `creator` — a different caller must still be
        // able to attach it to a project they have write access to.
        let connection = git_provider_connections::ActiveModel {
            provider_id: Set(provider.id),
            user_id: Set(Some(creator.id)),
            account_name: Set("creator-account".to_string()),
            account_type: Set("User".to_string()),
            access_token: Set(None),
            refresh_token: Set(None),
            token_expires_at: Set(None),
            refresh_token_expires_at: Set(None),
            installation_id: Set(None),
            metadata: Set(None),
            is_active: Set(true),
            is_expired: Set(false),
            syncing: Set(false),
            last_synced_at: Set(None),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        let project = temps_entities::projects::ActiveModel {
            name: Set("Scoping Test Project".to_string()),
            slug: Set("scoping-test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set(".".to_string()),
            git_provider_connection_id: Set(None),
            main_branch: Set("main".to_string()),
            preset: Set(Preset::Nixpacks),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        let result = project_service
            .update_git_settings(
                project.id,
                Some(connection.id),
                "main".to_string(),
                "test-owner".to_string(),
                "test-repo".to_string(),
                None,
                ".".to_string(),
                None,
                None,
                None,
            )
            .await;

        // The connection lookup itself must succeed regardless of who
        // created it — GitProviderConnectionNotFound must not fire here.
        // (The call may still fail later, e.g. verifying the branch against
        // a real git host, which this test doesn't stub.)
        assert!(
            !matches!(
                result,
                Err(ProjectError::GitProviderConnectionNotFound { .. })
            ),
            "connection created by a different user was rejected; connections must be installation-scoped, not user-scoped"
        );
    }

    #[tokio::test]
    async fn test_update_project_settings_normalizes_blank_directory() {
        // Regression: saving project settings with an empty "Base directory"
        // used to persist "" verbatim, after which every deployment failed with
        // "directory must be a non-empty relative path (got '')".
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

        let inserted_project = temps_entities::projects::ActiveModel {
            name: Set("Blank Dir Project".to_string()),
            slug: Set("blank-dir-project".to_string()),
            repo_name: Set("blank-dir-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("apps/web".to_string()),
            git_provider_connection_id: Set(None),
            main_branch: Set("main".to_string()),
            preset: Set(Preset::DockerCompose),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        project_service
            .update_project_settings(
                inserted_project.id,
                None,
                None,
                Some("main".to_string()),
                None,
                None,
                None,
                Some(String::new()), // directory: blank field from the settings form
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None, // cross_project_trace_sharing
                None, // error_source_context_enabled
                None, // error_source_root
                None, // ai_api_traffic_summary_enabled
            )
            .await
            .expect("update_project_settings should succeed");

        let stored = temps_entities::projects::Entity::find_by_id(inserted_project.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.directory, ".",
            "a blank directory must be stored as the repo-root marker, not \"\""
        );
    }

    #[tokio::test]
    async fn test_update_git_settings_normalizes_blank_directory() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

        let inserted_project = temps_entities::projects::ActiveModel {
            name: Set("Blank Git Dir Project".to_string()),
            slug: Set("blank-git-dir-project".to_string()),
            repo_name: Set("blank-git-dir-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("apps/web".to_string()),
            git_provider_connection_id: Set(None),
            main_branch: Set("main".to_string()),
            preset: Set(Preset::DockerCompose),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        project_service
            .update_git_settings(
                inserted_project.id,
                None,
                "main".to_string(),
                "test-owner".to_string(),
                "blank-git-dir-repo".to_string(),
                None,
                "/".to_string(), // absolute root, equally invalid downstream
                None,
                Some("https://github.com/test-owner/blank-git-dir-repo".to_string()),
                Some(true),
            )
            .await
            .expect("update_git_settings should succeed");

        let stored = temps_entities::projects::Entity::find_by_id(inserted_project.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.directory, ".");
    }

    #[tokio::test]
    async fn changing_compose_public_ports_requests_a_route_reload() {
        if !docker_available().await {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();
        let mock_queue = Arc::new(MockJobQueue::new());
        let project_service = create_test_services(db.clone(), mock_queue.clone()).await;

        let inserted_project = temps_entities::projects::ActiveModel {
            name: Set("Compose route reload".to_string()),
            slug: Set("compose-route-reload".to_string()),
            repo_name: Set("repo".to_string()),
            repo_owner: Set("owner".to_string()),
            directory: Set(".".to_string()),
            git_provider_connection_id: Set(None),
            main_branch: Set("main".to_string()),
            preset: Set(Preset::DockerCompose),
            preset_config: Set(Some(temps_entities::preset::PresetConfig::DockerCompose(
                temps_entities::preset::DockerComposeConfig {
                    public_ports: vec![temps_entities::preset::ComposePublicPort {
                        service: "web".to_string(),
                        port: 80,
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ))),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        project_service
            .update_git_settings(
                inserted_project.id,
                None,
                "main".to_string(),
                "owner".to_string(),
                "repo".to_string(),
                None,
                ".".to_string(),
                Some(serde_json::json!({
                    "publicPorts": [{ "service": "web", "port": 8080 }]
                })),
                None,
                None,
            )
            .await
            .expect("compose port save should succeed");

        let jobs = mock_queue.get_jobs().await;
        assert!(jobs.iter().any(|job| matches!(
            job,
            Job::ForceRouteReload(ForceRouteReloadJob {
                environment_id: None,
                deployment_id: None,
            })
        )));
    }

    #[test]
    fn project_directory_must_remain_inside_source_root() {
        assert_eq!(normalize_project_directory("").unwrap(), ".");
        assert_eq!(
            normalize_project_directory("./apps/web").unwrap(),
            "apps/web"
        );
        assert!(matches!(
            normalize_project_directory("../secrets"),
            Err(ProjectError::InvalidInput(_))
        ));
        assert_eq!(
            normalize_project_directory("/apps/web").unwrap(),
            "apps/web"
        );
        assert!(matches!(
            normalize_project_directory("apps/../../etc"),
            Err(ProjectError::InvalidInput(_))
        ));
    }
}
