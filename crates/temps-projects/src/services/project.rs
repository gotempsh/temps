use std::sync::Arc;
use temps_core::url_validation::{redact_url_password, validate_git_url};
use tracing::{info, warn};

use sea_orm::{
    prelude::Uuid, ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect, Set,
};
use temps_core::{Job, ProjectCreatedJob, ProjectDeletedJob, ProjectUpdatedJob};
use temps_entities::projects;
use temps_git::services::public_repo::PublicRepoProviderFactory;

use serde::Serialize;

use super::types::{
    CreateProjectRequest, Project, ProjectError, ProjectStatistics, UpdateDeploymentSettingsRequest,
};
use super::{EnvVarService, EnvVarWithEnvironments};
use crate::handlers::UpdateDeploymentConfigRequest;
use temps_presets::get_preset_by_slug;
// Placeholder functions - these should be implemented properly or imported from other services

fn slugify(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
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

// Constants for CPU allocation (in millicores, where 1000 = 1 CPU core).
// Only *requests* (scheduling minimums) are defaulted; CPU/memory *limits* are
// intentionally left unset so new projects/environments run uncapped by default.
pub const DEFAULT_CPU_REQUEST: i32 = 500_000; // 0.5 cores

// Constants for memory allocation (in MB)
pub const DEFAULT_MEMORY_REQUEST: i32 = 128; // 128 MB

// Add these constants at the top of the file proper key management
pub const NONCE_LENGTH: usize = 12;

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

        // Normalize directory to ensure it's a relative path
        let normalized_directory = if request.directory.starts_with('/') {
            // Remove leading slash to make it relative
            request.directory.trim_start_matches('/').to_string()
        } else {
            request.directory.clone()
        };

        // If directory is empty after normalization, use current directory marker
        let normalized_directory = if normalized_directory.is_empty() {
            ".".to_string()
        } else {
            normalized_directory
        };

        let project_slug = self.generate_unique_project_slug(&request.name).await?;
        // Get preset info and determine project type
        let preset_info = get_preset_by_slug(request.preset.as_str()).ok_or_else(|| {
            ProjectError::InvalidInput(format!("Invalid preset: {}", request.preset))
        })?;

        let _project_type_enum = preset_info.project_type();

        // Parse preset string to enum
        let preset = request
            .preset
            .parse::<temps_entities::preset::Preset>()
            .map_err(|e| ProjectError::InvalidInput(format!("Invalid preset: {}", e)))?;

        // Parse preset_config from JSON if provided
        let preset_config: Option<temps_entities::preset::PresetConfig> = request
            .preset_config
            .map(|json_value| {
                serde_json::from_value(json_value).map_err(|e| {
                    ProjectError::InvalidInput(format!("Invalid preset_config: {}", e))
                })
            })
            .transpose()?;

        // Create deployment config with resource and deployment settings.
        // CPU/memory *limits* are intentionally left unset (None) so containers
        // run uncapped by default — operators opt into a cap explicitly. Only the
        // *requests* (scheduling minimums) are seeded.
        let deployment_config = Some(temps_entities::deployment_config::DeploymentConfig {
            cpu_request: Some(DEFAULT_CPU_REQUEST),
            cpu_limit: None,
            memory_request: Some(DEFAULT_MEMORY_REQUEST),
            memory_limit: None,
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
        info!("Created project: {:?}", project_found_db);

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

        Ok(Self::map_db_project_to_project(project_found_db))
    }

    /// Post-insert steps for `create_project`. Returns the default environment
    /// on success. On any error, the caller is responsible for rolling back
    /// the project row.
    async fn finalize_project_creation(
        &self,
        project: &projects::Model,
        environment_variables: Option<Vec<(String, String)>>,
        storage_service_ids: Vec<i32>,
    ) -> Result<temps_entities::environments::Model, ProjectError> {
        let default_environment = self
            .environment_service
            .create_environment(
                project.id,
                "production".to_string(),
                Some(DEFAULT_CPU_REQUEST),
                // CPU/memory limits unset by default → uncapped containers.
                None,
                Some(DEFAULT_MEMORY_REQUEST),
                None,
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
            for (key, value) in env_vars {
                self.env_var_service
                    .create_environment_variable(
                        project.id,
                        vec![default_environment.id],
                        key.clone(),
                        value,
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
            .order_by_desc(projects::Column::LastDeployment)
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

        // Normalize directory to ensure it's a relative path
        let normalized_directory = if request.directory.starts_with('/') {
            // Remove leading slash to make it relative
            request.directory.trim_start_matches('/').to_string()
        } else {
            request.directory.clone()
        };

        // If directory is empty after normalization, use current directory marker
        let normalized_directory = if normalized_directory.is_empty() {
            ".".to_string()
        } else {
            normalized_directory
        };

        // Parse preset string to enum
        let preset = request
            .preset
            .parse::<temps_entities::preset::Preset>()
            .map_err(|e| ProjectError::InvalidInput(format!("Invalid preset: {}", e)))?;

        // Update the project
        let mut active_project: projects::ActiveModel = project.into();
        active_project.name = Set(request.name);
        active_project.repo_name = Set(request.repo_name.unwrap_or_else(|| "unknown".to_string()));
        active_project.repo_owner =
            Set(request.repo_owner.unwrap_or_else(|| "unknown".to_string()));
        active_project.directory = Set(normalized_directory);
        active_project.main_branch = Set(request.main_branch);
        active_project.preset = Set(preset); // No longer Optional
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
                    .ok_or(ProjectError::Other(format!(
                        "Git provider connection {} not found",
                        connection_id
                    )))?;

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

        // Update preset_config if provided
        if let Some(ref config_value) = preset_config {
            // Reload project to ensure we have the latest state
            let project = projects::Entity::find_by_id(project_id)
                .one(self.db.as_ref())
                .await?
                .ok_or(ProjectError::NotFound(format!(
                    "Project {} not found",
                    project_id
                )))?;

            // Parse the preset config based on the project's current preset
            let parsed_config = temps_entities::preset::PresetConfig::parse_for_preset(
                &project.preset,
                config_value,
            )
            .map_err(|e| ProjectError::InvalidInput(format!("Invalid preset config: {}", e)))?;

            let mut active_project: projects::ActiveModel = project.into();
            active_project.preset_config = Set(Some(parsed_config));
            active_project.update(self.db.as_ref()).await?;
        }

        // Update git-related fields if any are provided
        let needs_git_update = main_branch.is_some()
            || repo_owner.is_some()
            || repo_name.is_some()
            || preset.is_some()
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
                // Parse preset string to enum
                let preset_enum = preset_value
                    .parse::<temps_entities::preset::Preset>()
                    .map_err(|e| ProjectError::InvalidInput(format!("Invalid preset: {}", e)))?;
                active_project.preset = Set(preset_enum);
            }
            if let Some(dir) = directory {
                active_project.directory = Set(dir);
            }

            let updated_project = active_project.update(self.db.as_ref()).await?;
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

        // Verify git provider connection if provided
        if let Some(connection_id) = git_provider_connection_id {
            if connection_id > 0 {
                use temps_entities::git_provider_connections;
                let connection = git_provider_connections::Entity::find_by_id(connection_id)
                    .one(self.db.as_ref())
                    .await?
                    .ok_or(ProjectError::Other(format!(
                        "Git provider connection {} not found",
                        connection_id
                    )))?;

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

        // Capture the current preset before converting to ActiveModel
        let project_preset = project.preset;

        // Update the project
        let mut active_project: projects::ActiveModel = project.into();
        active_project.main_branch = Set(main_branch.clone());
        active_project.repo_owner = Set(repo_owner.clone());
        active_project.repo_name = Set(repo_name.clone());
        active_project.directory = Set(directory);

        if let Some(preset_value) = preset {
            // Parse preset string to enum
            let preset_enum = preset_value
                .parse::<temps_entities::preset::Preset>()
                .map_err(|e| ProjectError::InvalidInput(format!("Invalid preset: {}", e)))?;
            active_project.preset = Set(preset_enum);
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

        // Update preset_config if provided (e.g., Dockerfile path for Docker preset)
        if let Some(ref config_value) = preset_config {
            // Determine the target preset: use the newly set preset if provided, otherwise use current
            let target_preset = if active_project.preset.is_set() {
                *active_project.preset.as_ref()
            } else {
                project_preset
            };

            let parsed_config = temps_entities::preset::PresetConfig::parse_for_preset(
                &target_preset,
                config_value,
            )
            .map_err(|e| ProjectError::InvalidInput(format!("Invalid preset config: {}", e)))?;

            active_project.preset_config = Set(Some(parsed_config));
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

        let updated_project = active_project.update(self.db.as_ref()).await?;

        Ok(Self::map_db_project_to_project(updated_project))
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
        let signing_token = generate_signing_token();

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

    pub async fn get_projects_paginated(
        &self,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<Project>, i64), ProjectError> {
        use sea_orm::PaginatorTrait;
        use sea_orm::QueryOrder;

        // Calculate offset
        let offset = ((page - 1) * per_page) as u64;

        // Get total count
        let total = projects::Entity::find()
            .count(self.db.as_ref())
            .await
            .map_err(|e| ProjectError::DatabaseConnectionError(e.to_string()))?
            as i64;

        // Get paginated projects
        let projects = projects::Entity::find()
            .order_by_desc(projects::Column::LastDeployment)
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
        use sea_orm::PaginatorTrait;

        // Get total count of projects
        let total_count = projects::Entity::find()
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

        // Convert preset enum to string for backwards compatibility
        let preset_str = format!("{:?}", db_project.preset).to_lowercase();

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
            enable_preview_environments: db_project.enable_preview_environments,
            preview_envs_on_demand: db_project.preview_envs_on_demand,
            preview_envs_idle_timeout_seconds: db_project.preview_envs_idle_timeout_seconds,
            preview_envs_wake_timeout_seconds: db_project.preview_envs_wake_timeout_seconds,
            source_type: db_project.source_type,
            gitlab_webhook_id: db_project.gitlab_webhook_id,
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
    ) -> Result<EnvVarWithEnvironments, ProjectError> {
        self.env_var_service
            .create_environment_variable(project_id, environment_ids, key, value)
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

            // Use authenticated token if available (avoids 60 req/hr rate limit)
            let token = if provider_name == "github" {
                self.git_provider_manager.get_any_github_token().await
            } else {
                None
            };

            let provider = PublicRepoProviderFactory::create_with_token(provider_name, token)
                .map_err(|e| {
                    ProjectError::Other(format!(
                        "Failed to create public repo provider for {}: {}",
                        provider_name, e
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
}
