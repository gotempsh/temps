use std::sync::Arc;
use tracing::{info, warn};

use sea_orm::{
    prelude::Uuid, sea_query::Expr, ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use temps_core::{Job, ProjectCreatedJob, ProjectDeletedJob, ProjectUpdatedJob};
use temps_entities::projects;

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

// Constants for CPU allocation (in millicores, where 1000 = 1 CPU core)
pub const DEFAULT_CPU_REQUEST: i32 = 500_000; // 0.5 cores
pub const DEFAULT_CPU_LIMIT: i32 = 2_000_000; // 2 cores

// Constants for memory allocation (in MB)
pub const DEFAULT_MEMORY_REQUEST: i32 = 128; // 128 MB
pub const DEFAULT_MEMORY_LIMIT: i32 = 512; // 512 MB

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
}

impl ProjectService {
    pub fn new(
        db: Arc<temps_database::DbConnection>,
        queue_service: Arc<dyn temps_core::JobQueue>,
        config_service: Arc<temps_config::ConfigService>,
        external_service_manager: Arc<temps_providers::ExternalServiceManager>,
        git_provider_manager: Arc<temps_git::GitProviderManager>,
        environment_service: Arc<temps_environments::EnvironmentService>,
    ) -> Self {
        let env_var_service = Arc::new(EnvVarService::new(db.clone()));

        ProjectService {
            db: db.clone(),
            queue_service,
            config_service: config_service.clone(),
            external_service_manager,
            git_provider_manager,
            env_var_service,
            environment_service,
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

        // Create deployment config with resource and deployment settings
        let deployment_config = Some(temps_entities::deployment_config::DeploymentConfig {
            cpu_request: Some(DEFAULT_CPU_REQUEST),
            cpu_limit: Some(DEFAULT_CPU_LIMIT),
            memory_request: Some(DEFAULT_MEMORY_REQUEST),
            memory_limit: Some(DEFAULT_MEMORY_LIMIT),
            exposed_port: request.exposed_port,
            automatic_deploy: request.automatic_deploy,
            performance_metrics_enabled: false, // Default to false
            session_recording_enabled: false,
            replicas: 1, // Default replicas
        });

        let project = projects::ActiveModel {
            name: Set(request.name),
            repo_name: Set(request.repo_name.unwrap_or_else(|| "unknown".to_string())),
            repo_owner: Set(request.repo_owner.unwrap_or_else(|| "unknown".to_string())),
            directory: Set(normalized_directory),
            main_branch: Set(request.main_branch),
            preset: Set(preset), // Now required, not Option
            preset_config: Set(preset_config),
            deployment_config: Set(deployment_config),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            slug: Set(project_slug),
            is_public_repo: Set(request.is_public_repo.unwrap_or(false)),
            git_url: Set(request.git_url),
            git_provider_connection_id: Set(request.git_provider_connection_id),
            deleted_at: Set(None),
            last_deployment: Set(None),
            ..Default::default()
        };

        // Start a transaction to ensure all operations succeed or fail together
        // Insert the project
        let project_found_db = project
            .insert(self.db.as_ref())
            .await
            .map_err(|e| ProjectError::Other(e.to_string()))?;
        info!("Created project: {:?}", project_found_db);

        // Create default production environment
        let default_environment = self
            .environment_service
            .create_environment(
                project_found_db.id,
                "production".to_string(),
                Some(DEFAULT_CPU_REQUEST),
                Some(DEFAULT_CPU_LIMIT),
                Some(DEFAULT_MEMORY_REQUEST),
                Some(DEFAULT_MEMORY_LIMIT),
                project_found_db.main_branch.clone(),
            )
            .await
            .map_err(|e| {
                ProjectError::Other(format!("Failed to create default environment: {}", e))
            })?;

        info!(
            "Created default environment for project: {}",
            default_environment.id
        );

        // Create environment variables if provided and link them to the default environment
        if let Some(env_vars) = request.environment_variables {
            for (key, value) in env_vars {
                self.env_var_service
                    .create_environment_variable(
                        project_found_db.id,
                        vec![default_environment.id], // Link to the newly created environment
                        key,
                        value,
                    )
                    .await
                    .map_err(|e| {
                        ProjectError::Other(format!("Failed to create environment variable: {}", e))
                    })?;
            }
        }

        // Create storage services
        // Create storage services if any are specified
        if !request.storage_service_ids.is_empty() {
            info!(
                "Creating {} storage services for project {}",
                request.storage_service_ids.len(),
                project_found_db.id
            );

            for storage_service_id in request.storage_service_ids {
                self.external_service_manager
                    .link_service_to_project(storage_service_id, project_found_db.id)
                    .await
                    .map_err(|e| {
                        ProjectError::Other(format!("Failed to create storage service: {}", e))
                    })?;
            }
        }

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
        // Queue initial deployment/pipeline job if project has repository information
        if !project_found_db.repo_owner.is_empty() && !project_found_db.repo_name.is_empty() {
            info!(
                "Queueing initial deployment job for project: {}",
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
        }

        Ok(Self::map_db_project_to_project(project_found_db))
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

    pub async fn delete_project(&self, project_id: i32) -> Result<(), ProjectError> {
        // Get project info before deletion for the event
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| ProjectError::Other(e.to_string()))?;
        if project.is_none() {
            return Err(ProjectError::NotFound(format!(
                "project {} not found",
                project_id
            )));
        }
        // Teardown all deployments for this project before deletion
        if let Some(project_info) = &project {
            info!(
                "Tearing down all deployments for project {}",
                project_info.id
            );
            let deployments = temps_entities::deployments::Entity::find()
                .filter(temps_entities::deployments::Column::ProjectId.eq(project_id))
                .all(self.db.as_ref())
                .await
                .map_err(|e| ProjectError::Other(e.to_string()))?;

            // Extract deployment IDs for bulk operations instead of individual teardowns
            let deployment_ids: Vec<i32> = deployments.iter().map(|d| d.id).collect();

            // Bulk teardown operations - update all deployment states to "cancelled" in one query
            if !deployment_ids.is_empty() {
                use temps_entities::deployments;
                deployments::Entity::update_many()
                    .col_expr(deployments::Column::State, Expr::value("cancelled"))
                    .col_expr(
                        deployments::Column::UpdatedAt,
                        Expr::current_timestamp().into(),
                    )
                    .filter(deployments::Column::Id.is_in(deployment_ids))
                    .exec(self.db.as_ref())
                    .await
                    .map_err(|e| {
                        ProjectError::Other(format!("Failed to bulk cancel deployments: {}", e))
                    })?;

                info!(
                    "Bulk cancelled {} deployments for project deletion",
                    deployments.len()
                );
            }
        }
        let txn = self.db.begin().await?;

        use temps_entities::{
            crons, deployment_domains, deployments, env_var_environments, env_vars,
            environment_domains, environments, project_custom_domains, project_services, projects,
        };

        // NOTE: We're NOT deleting analytics_events, visitor, traces, logs, request_logs, or performance_metrics
        // These are kept for historical/audit purposes and can be very large tables
        info!("Keeping analytics data, visitor data, traces, and logs for historical purposes");

        info!("deleting deployment domains");
        // Get all deployment IDs for this project first
        let deployment_ids: Vec<i32> = deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project_id))
            .all(&txn)
            .await?
            .into_iter()
            .map(|d| d.id)
            .collect();

        // Delete deployment_domains for these deployments
        deployment_domains::Entity::delete_many()
            .filter(deployment_domains::Column::DeploymentId.is_in(deployment_ids.clone()))
            .exec(&txn)
            .await?;

        info!("deleting environment domains");
        // Get all environment IDs for this project first
        let environment_ids: Vec<i32> = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .all(&txn)
            .await?
            .into_iter()
            .map(|e| e.id)
            .collect();

        // Delete environment_domains for these environments
        environment_domains::Entity::delete_many()
            .filter(environment_domains::Column::EnvironmentId.is_in(environment_ids.clone()))
            .exec(&txn)
            .await?;

        // Delete cron jobs for this project
        info!("Deleting cron jobs");
        crons::Entity::delete_many()
            .filter(crons::Column::ProjectId.eq(project_id))
            .exec(&txn)
            .await?;

        // Delete project services
        info!("Deleting project services");
        project_services::Entity::delete_many()
            .filter(project_services::Column::ProjectId.eq(project_id))
            .exec(&txn)
            .await?;

        info!("deleting env_var_environments");
        // Delete env_var_environments for these environments
        env_var_environments::Entity::delete_many()
            .filter(env_var_environments::Column::EnvironmentId.is_in(environment_ids))
            .exec(&txn)
            .await?;

        info!("deleting env_vars");
        // Delete environment variables
        env_vars::Entity::delete_many()
            .filter(env_vars::Column::ProjectId.eq(project_id))
            .exec(&txn)
            .await?;

        info!("deleting deployments");
        // Delete all deployments for this project
        deployments::Entity::delete_many()
            .filter(deployments::Column::ProjectId.eq(project_id))
            .exec(&txn)
            .await?;

        info!("deleting environments");
        // Get all environments before deletion to emit events
        let environments_to_delete = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .all(&txn)
            .await?;

        // Emit EnvironmentDeleted jobs for each environment
        for env in &environments_to_delete {
            let env_deleted_job = Job::EnvironmentDeleted(temps_core::EnvironmentDeletedJob {
                environment_id: env.id,
                environment_name: env.name.clone(),
                project_id: env.project_id,
            });

            if let Err(e) = self.queue_service.send(env_deleted_job).await {
                warn!(
                    "Failed to emit EnvironmentDeleted job for environment {}: {}",
                    env.id, e
                );
            } else {
                info!("Emitted EnvironmentDeleted job for environment {}", env.id);
            }
        }

        // Delete all environments for this project
        environments::Entity::delete_many()
            .filter(environments::Column::ProjectId.eq(project_id))
            .exec(&txn)
            .await?;

        info!("deleting custom domains");
        // Delete all custom domains for this project
        project_custom_domains::Entity::delete_many()
            .filter(project_custom_domains::Column::ProjectId.eq(project_id))
            .exec(&txn)
            .await?;

        info!("Hard deleting project from database");
        // Actually delete the project row from the database
        projects::Entity::delete_by_id(project_id)
            .exec(&txn)
            .await?;

        txn.commit().await?;

        // Emit ProjectDeleted job
        if let Some(project_data) = project {
            let project_deleted_job = Job::ProjectDeleted(ProjectDeletedJob {
                project_id: project_data.id,
                project_name: project_data.name.clone(),
            });

            if let Err(e) = self.queue_service.send(project_deleted_job).await {
                warn!(
                    "Failed to emit ProjectDeleted job for project {}: {}",
                    project_id, e
                );
            } else {
                info!("Emitted ProjectDeleted job for project {}", project_id);
            }
        }

        info!(
            "Project and all related data deleted successfully for project_id: {}",
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
    ) -> Result<Project, ProjectError> {
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
        deployment_config.automatic_deploy = automatic_deploy;
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
    ) -> Result<Project, ProjectError> {
        // Get the current project
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ProjectError::NotFound(format!(
                "Project {} not found",
                project_id
            )))?;

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

        // Update the project
        let mut active_project: projects::ActiveModel = project.into();
        active_project.main_branch = Set(main_branch);
        active_project.repo_owner = Set(repo_owner);
        active_project.repo_name = Set(repo_name);
        active_project.directory = Set(directory);

        if let Some(preset_value) = preset {
            // Parse preset string to enum
            let preset_enum = preset_value
                .parse::<temps_entities::preset::Preset>()
                .map_err(|e| ProjectError::InvalidInput(format!("Invalid preset: {}", e)))?;
            active_project.preset = Set(preset_enum);
        }

        if let Some(connection_id) = git_provider_connection_id {
            if connection_id > 0 {
                active_project.git_provider_connection_id = Set(Some(connection_id));
            } else {
                active_project.git_provider_connection_id = Set(None);
            }
        }

        let updated_project = active_project.update(self.db.as_ref()).await?;

        Ok(Self::map_db_project_to_project(updated_project))
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
            deployment_config.automatic_deploy = automatic_deploy;
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

    /// Generate a unique project slug by checking for collisions and appending a short UUID if needed
    pub async fn generate_unique_project_slug(&self, name: &str) -> Result<String, ProjectError> {
        let base_slug = slugify(name);

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

        Project {
            id: db_project.id,
            slug: db_project.slug,
            name: db_project.name,
            repo_name: Some(db_project.repo_name),
            repo_owner: Some(db_project.repo_owner),
            directory: db_project.directory,
            main_branch: db_project.main_branch,
            preset: Some(preset_str),
            created_at: db_project.created_at,
            updated_at: db_project.updated_at,
            automatic_deploy: deployment_config
                .clone()
                .map(|c| c.automatic_deploy)
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

        // Validate environment belongs to this project
        let environment = temps_entities::environments::Entity::find_by_id(environment_id)
            .filter(temps_entities::environments::Column::ProjectId.eq(project_id))
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
            // Fetch latest commit from the branch
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
}
