use sea_orm::{
    ActiveModelTrait, ColumnTrait, DbErr, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};
use serde::Serialize;
use slug::slugify;
use std::sync::Arc;
use temps_core::problemdetails::Problem;
use temps_core::{EnvironmentCreatedJob, EnvironmentDeletedJob, Job, JobQueue};
use temps_entities::{environment_domains, environments, projects};
use thiserror::Error;
use tracing::{info, warn};

#[derive(Error, Debug)]
pub enum EnvironmentError {
    #[error("Database connection error: {0}")]
    DatabaseConnectionError(String),

    #[error("Environment not found")]
    NotFound(String),

    #[error("Database error: {reason}")]
    DatabaseError { reason: String },

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error(
        "Branch '{branch}' is already used by environment '{env_name}' in project {project_id}"
    )]
    BranchAlreadyInUse {
        branch: String,
        env_name: String,
        project_id: i32,
    },

    #[error("Other error: {0}")]
    Other(String),
}

impl From<DbErr> for EnvironmentError {
    fn from(error: DbErr) -> Self {
        match error {
            DbErr::RecordNotFound(_) => EnvironmentError::NotFound(error.to_string()),
            _ => EnvironmentError::DatabaseError {
                reason: error.to_string(),
            },
        }
    }
}

impl From<EnvironmentError> for Problem {
    fn from(error: EnvironmentError) -> Self {
        match error {
            EnvironmentError::NotFound(msg) => {
                temps_core::error_builder::not_found().detail(msg).build()
            }
            EnvironmentError::InvalidInput(msg) => {
                temps_core::error_builder::bad_request().detail(msg).build()
            }
            EnvironmentError::DatabaseConnectionError(msg) => {
                temps_core::error_builder::internal_server_error()
                    .detail(msg)
                    .build()
            }
            EnvironmentError::DatabaseError { reason } => {
                temps_core::error_builder::internal_server_error()
                    .detail(reason)
                    .build()
            }
            EnvironmentError::BranchAlreadyInUse { .. } => temps_core::error_builder::bad_request()
                .title("Branch Already In Use")
                .detail(error.to_string())
                .build(),
            EnvironmentError::Other(msg) => temps_core::error_builder::internal_server_error()
                .detail(msg)
                .build(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct DomainEnvironment {
    pub id: i32,
    pub name: String,
    pub slug: String,
}

#[derive(Clone)]
pub struct EnvironmentService {
    db: Arc<temps_database::DbConnection>,
    config_service: Arc<temps_config::ConfigService>,
    queue_service: Option<Arc<dyn JobQueue>>,
}

impl EnvironmentService {
    pub fn new(
        db: Arc<temps_database::DbConnection>,
        config_service: Arc<temps_config::ConfigService>,
    ) -> Self {
        EnvironmentService {
            db,
            config_service,
            queue_service: None,
        }
    }

    pub fn with_queue_service(mut self, queue_service: Arc<dyn JobQueue>) -> Self {
        self.queue_service = Some(queue_service);
        self
    }

    pub async fn compute_environment_url(&self, environment_slug: &str) -> String {
        let settings = self.config_service.get_settings().await.unwrap_or_default();

        // Use external_url if configured, otherwise fall back to preview_domain
        let base_domain = settings.preview_domain.clone();

        // Determine protocol - use https if external_url is configured, otherwise http
        let protocol = if settings.external_url.is_some() {
            "https"
        } else {
            "http"
        };

        // Simple format: <scheme>://<slug>.<preview_domain>
        format!("{}://{}.{}", protocol, environment_slug, base_domain)
    }

    /// Compute the full FQDN for an environment (without protocol)
    pub async fn compute_environment_fqdn(&self, environment_slug: &str) -> String {
        let settings = self.config_service.get_settings().await.unwrap_or_default();
        let base_domain = settings.preview_domain.clone();
        format!("{}.{}", environment_slug, base_domain)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_environment(
        &self,
        project_id: i32,
        name: String,
        cpu_request: Option<i32>,
        cpu_limit: Option<i32>,
        memory_request: Option<i32>,
        memory_limit: Option<i32>,
        branch: String,
    ) -> anyhow::Result<environments::Model> {
        // Get the project slug
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| anyhow::anyhow!("Project not found"))?;

        // Check if a soft-deleted environment with this branch exists — restore it
        if let Some(deleted_env) = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::Branch.eq(&branch))
            .filter(environments::Column::DeletedAt.is_not_null())
            .one(self.db.as_ref())
            .await?
        {
            info!(
                "Restoring soft-deleted environment {} for branch '{}' in project {}",
                deleted_env.id, branch, project_id
            );
            let mut active_env: environments::ActiveModel = deleted_env.into();
            active_env.deleted_at = Set(None);
            active_env.updated_at = Set(chrono::Utc::now());
            active_env.current_deployment_id = Set(None);
            let restored = active_env.update(self.db.as_ref()).await?;
            return Ok(restored);
        }

        let env_slug = slugify(&name);

        // Create main_url using project_slug-env_slug format
        let main_url = format!("{}-{}", project.slug, env_slug);

        // Start a transaction for insert + domain creation
        let txn = self.db.begin().await?;

        // Create the new environment
        let new_environment = environments::ActiveModel {
            project_id: Set(project_id),
            name: Set(name),
            slug: Set(env_slug.clone()),
            subdomain: Set(main_url.clone()),
            host: Set("".to_string()),
            upstreams: Set(temps_entities::upstream_config::UpstreamList::new()),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            current_deployment_id: Set(None),
            deployment_config: Set(Some(temps_entities::deployment_config::DeploymentConfig {
                cpu_request,
                cpu_limit,
                memory_request,
                memory_limit,
                exposed_port: None,
                automatic_deploy: false,
                performance_metrics_enabled: false,
                session_recording_enabled: false,
                replicas: 1,
                security: None,
            })),
            branch: Set(Some(branch)),
            ..Default::default()
        };

        let environment = new_environment.insert(&txn).await?;

        // Create the environment domain with the stored identifier from main_url
        let new_domain = environment_domains::ActiveModel {
            environment_id: Set(environment.id),
            domain: Set(environment.subdomain.clone()),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        };

        new_domain.insert(&txn).await?;

        txn.commit().await?;

        // Emit EnvironmentCreated job
        if let Some(queue_service) = &self.queue_service {
            let env_created_job = Job::EnvironmentCreated(EnvironmentCreatedJob {
                environment_id: environment.id,
                environment_name: environment.name.clone(),
                project_id: environment.project_id,
                subdomain: environment.subdomain.clone(),
            });

            if let Err(e) = queue_service.send(env_created_job).await {
                warn!(
                    "Failed to emit EnvironmentCreated job for environment {}: {}",
                    environment.id, e
                );
            } else {
                info!(
                    "Emitted EnvironmentCreated job for environment {}",
                    environment.id
                );
            }
        }

        Ok(environment)
    }

    pub async fn get_environments(
        &self,
        project_id_p: i32,
    ) -> Result<Vec<environments::Model>, EnvironmentError> {
        let envs = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id_p))
            .filter(environments::Column::DeletedAt.is_null())
            .order_by_asc(environments::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        Ok(envs)
    }
    pub async fn get_project(
        &self,
        project_id_p: i32,
    ) -> Result<projects::Model, EnvironmentError> {
        let project = projects::Entity::find_by_id(project_id_p)
            .one(self.db.as_ref())
            .await?;

        project.ok_or(EnvironmentError::NotFound(format!(
            "Project {} not found",
            project_id_p
        )))
    }

    pub async fn get_environment(
        &self,
        project_id_p: i32,
        env_id: i32,
    ) -> Result<environments::Model, EnvironmentError> {
        let environment = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id_p))
            .filter(environments::Column::Id.eq(env_id))
            .filter(environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?;

        environment.ok_or(EnvironmentError::NotFound(format!(
            "Environment {:?} not found",
            env_id
        )))
    }

    pub async fn get_default_environment(
        &self,
        project_id_p: i32,
    ) -> Result<environments::Model, EnvironmentError> {
        let default_environment = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id_p))
            .filter(environments::Column::DeletedAt.is_null())
            .order_by_asc(environments::Column::CreatedAt)
            .one(self.db.as_ref())
            .await?;

        match default_environment {
            Some(env) => Ok(env),
            None => Err(EnvironmentError::NotFound(format!(
                "No environment found for project {}",
                project_id_p
            ))),
        }
    }

    pub async fn get_or_create_environment_for_branch(
        &self,
        project_id: i32,
        branch: &str,
    ) -> Result<environments::Model, EnvironmentError> {
        // First, get the project to check if this is the main branch
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| EnvironmentError::Other(e.to_string()))?
            .ok_or_else(|| EnvironmentError::Other("Project not found".to_string()))?;

        if project.main_branch == branch {
            // If it's the main branch, return the default (first) environment
            info!("Using default environment for main branch: {}", branch);
            return self.get_default_environment(project_id).await.map_err(|e| {
                EnvironmentError::Other(format!("Failed to get default environment: {}", e))
            });
        }

        // For non-main branches, find active environment for this branch
        let existing_env = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::Branch.eq(branch))
            .filter(environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await
            .map_err(|e| EnvironmentError::Other(e.to_string()))?;

        if let Some(env) = existing_env {
            info!(
                "Found existing preview environment for branch {}: {}",
                branch, env.id
            );
            return Ok(env);
        }

        // Check for a soft-deleted environment with this branch and restore it
        if let Some(deleted_env) = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::Branch.eq(branch))
            .filter(environments::Column::DeletedAt.is_not_null())
            .one(self.db.as_ref())
            .await
            .map_err(|e| EnvironmentError::Other(e.to_string()))?
        {
            info!(
                "Restoring soft-deleted environment {} for branch '{}'",
                deleted_env.id, branch
            );
            let mut active_env: environments::ActiveModel = deleted_env.into();
            active_env.deleted_at = Set(None);
            active_env.updated_at = Set(chrono::Utc::now());
            let restored = active_env
                .update(self.db.as_ref())
                .await
                .map_err(|e| EnvironmentError::Other(e.to_string()))?;
            return Ok(restored);
        }

        let env_name = "preview";
        info!("Creating new preview environment for branch: {}", branch);
        self.create_environment(
            project_id,
            env_name.to_string(),
            None,
            None,
            None,
            None,
            branch.to_string(),
        )
        .await
        .map_err(|e| EnvironmentError::Other(e.to_string()))
    }

    pub async fn create_new_environment(
        &self,
        project_id: i32,
        name: String,
        branch: String,
        replicas: Option<i32>,
    ) -> Result<environments::Model, EnvironmentError> {
        use sea_orm::TransactionTrait;

        // Verify project exists
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| EnvironmentError::Other(e.to_string()))?
            .ok_or_else(|| {
                EnvironmentError::NotFound(format!("Project {} not found", project_id))
            })?;

        // Check if an active environment with same name already exists
        let existing_env = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::Name.eq(&name))
            .filter(environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await
            .map_err(|e| EnvironmentError::Other(e.to_string()))?;

        if existing_env.is_some() {
            return Err(EnvironmentError::Other(
                "Environment with this name already exists".to_string(),
            ));
        }

        // Check if another active environment in the same project already tracks this branch
        // (applies to both restore and fresh creation paths)
        let branch_conflict = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::Branch.eq(&branch))
            .filter(environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await
            .map_err(|e| EnvironmentError::DatabaseError {
                reason: format!("Failed to check branch uniqueness: {}", e),
            })?;

        if let Some(conflict) = branch_conflict {
            return Err(EnvironmentError::BranchAlreadyInUse {
                branch,
                env_name: conflict.name,
                project_id,
            });
        }

        // Check if a soft-deleted environment with this name exists — restore it
        if let Some(deleted_env) = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::Name.eq(&name))
            .filter(environments::Column::DeletedAt.is_not_null())
            .one(self.db.as_ref())
            .await
            .map_err(|e| EnvironmentError::Other(e.to_string()))?
        {
            info!(
                "Restoring soft-deleted environment {} ('{}') in project {}",
                deleted_env.id, name, project_id
            );
            let mut active_env: environments::ActiveModel = deleted_env.into();
            active_env.deleted_at = Set(None);
            active_env.branch = Set(Some(branch));
            active_env.updated_at = Set(chrono::Utc::now());
            active_env.current_deployment_id = Set(None);
            if let Some(r) = replicas {
                active_env.deployment_config =
                    Set(Some(temps_entities::deployment_config::DeploymentConfig {
                        replicas: r,
                        ..Default::default()
                    }));
            }
            let restored = active_env
                .update(self.db.as_ref())
                .await
                .map_err(|e| EnvironmentError::Other(e.to_string()))?;
            return Ok(restored);
        }

        // Generate the environment identifier
        let env_slug = slugify(&name);

        // Create main_url using project_slug-env_slug format
        let main_url = format!("{}-{}", project.slug, env_slug);

        // Create the new environment
        let new_environment = environments::ActiveModel {
            project_id: Set(project_id),
            name: Set(name),
            slug: Set(env_slug.clone()),
            subdomain: Set(main_url.clone()),
            host: Set("".to_string()),
            upstreams: Set(temps_entities::upstream_config::UpstreamList::new()),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            current_deployment_id: Set(None),
            deployment_config: Set(replicas.map(|r| {
                temps_entities::deployment_config::DeploymentConfig {
                    replicas: r,
                    ..Default::default()
                }
            })),
            branch: Set(Some(branch)),
            ..Default::default()
        };

        let txn = self
            .db
            .begin()
            .await
            .map_err(|e| EnvironmentError::Other(e.to_string()))?;

        // Insert the environment
        let environment = new_environment
            .insert(&txn)
            .await
            .map_err(|e| EnvironmentError::Other(e.to_string()))?;

        // Create the environment domain with the stored identifier from main_url
        let new_domain = environment_domains::ActiveModel {
            environment_id: Set(environment.id),
            domain: Set(environment.subdomain.clone()),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        };

        new_domain
            .insert(&txn)
            .await
            .map_err(|e| EnvironmentError::Other(e.to_string()))?;

        txn.commit()
            .await
            .map_err(|e| EnvironmentError::Other(e.to_string()))?;

        Ok(environment)
    }

    pub async fn update_environment_settings(
        &self,
        project_id_param: i32,
        env_id: i32,
        settings: crate::handlers::UpdateEnvironmentSettingsRequest,
    ) -> Result<environments::Model, EnvironmentError> {
        // First get the environment to verify it exists and belongs to the project
        let environment = self.get_environment(project_id_param, env_id).await?;

        // Update the environment with new settings
        let mut active_model: environments::ActiveModel = environment.clone().into();

        // Update deployment config with new resource settings
        let mut deployment_config = environment.deployment_config.clone().unwrap_or_default();

        // Update only the fields that are provided
        if settings.cpu_request.is_some() {
            deployment_config.cpu_request = settings.cpu_request;
        }
        if settings.cpu_limit.is_some() {
            deployment_config.cpu_limit = settings.cpu_limit;
        }
        if settings.memory_request.is_some() {
            deployment_config.memory_request = settings.memory_request;
        }
        if settings.memory_limit.is_some() {
            deployment_config.memory_limit = settings.memory_limit;
        }
        if settings.exposed_port.is_some() {
            deployment_config.exposed_port = settings.exposed_port;
        }
        if let Some(replicas) = settings.replicas {
            deployment_config.replicas = replicas;
        }
        if let Some(automatic_deploy) = settings.automatic_deploy {
            deployment_config.automatic_deploy = automatic_deploy;
        }
        if let Some(performance_metrics_enabled) = settings.performance_metrics_enabled {
            deployment_config.performance_metrics_enabled = performance_metrics_enabled;
        }
        if let Some(session_recording_enabled) = settings.session_recording_enabled {
            deployment_config.session_recording_enabled = session_recording_enabled;
        }
        if let Some(security) = settings.security {
            deployment_config.security = Some(security);
        }

        // Validate the deployment config
        deployment_config.validate().map_err(|e| {
            EnvironmentError::InvalidInput(format!("Invalid deployment config: {}", e))
        })?;

        // If the branch is being changed, verify no other environment in the same project
        // already tracks it. We exclude the environment being updated from the check.
        if let Some(ref new_branch) = settings.branch {
            let branch_conflict = environments::Entity::find()
                .filter(environments::Column::ProjectId.eq(project_id_param))
                .filter(environments::Column::Branch.eq(new_branch.as_str()))
                .filter(environments::Column::Id.ne(env_id))
                .filter(environments::Column::DeletedAt.is_null())
                .one(self.db.as_ref())
                .await
                .map_err(|e| EnvironmentError::DatabaseError {
                    reason: format!("Failed to check branch uniqueness: {}", e),
                })?;

            if let Some(conflict) = branch_conflict {
                return Err(EnvironmentError::BranchAlreadyInUse {
                    branch: new_branch.clone(),
                    env_name: conflict.name,
                    project_id: project_id_param,
                });
            }
        }

        active_model.deployment_config = Set(Some(deployment_config));
        active_model.branch = Set(settings.branch);
        active_model.updated_at = Set(chrono::Utc::now());

        let updated_environment = active_model
            .update(self.db.as_ref())
            .await
            .map_err(|e| EnvironmentError::DatabaseConnectionError(e.to_string()))?;

        Ok(updated_environment)
    }

    pub async fn get_environment_domains(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<Vec<environment_domains::Model>, EnvironmentError> {
        // First verify that the environment belongs to the project and get it
        let environment = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::Id.eq(environment_id))
            .one(self.db.as_ref())
            .await?;

        let env = environment.ok_or_else(|| {
            EnvironmentError::NotFound(format!(">>> Environment {} not found", environment_id))
        })?;

        // Get all domains for this environment
        let all_domains = environment_domains::Entity::find()
            .filter(environment_domains::Column::EnvironmentId.eq(environment_id))
            .all(self.db.as_ref())
            .await?;

        // Filter out the default environment subdomain (which is auto-created and can't be removed)
        let custom_domains: Vec<environment_domains::Model> = all_domains
            .into_iter()
            .filter(|d| d.domain != env.subdomain)
            .collect();

        Ok(custom_domains)
    }

    pub async fn add_environment_domain(
        &self,
        project_id_p: i32,
        env_id: i32,
        domain: String,
    ) -> Result<environment_domains::Model, EnvironmentError> {
        let environment = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id_p))
            .filter(environments::Column::Id.eq(env_id))
            .one(self.db.as_ref())
            .await?;

        if let Some(env) = environment {
            let new_domain = environment_domains::ActiveModel {
                environment_id: Set(env.id),
                domain: Set(domain),
                created_at: Set(chrono::Utc::now()),
                ..Default::default()
            };

            let inserted_domain = new_domain.insert(self.db.as_ref()).await?;
            return Ok(inserted_domain);
        }

        Err(EnvironmentError::NotFound(format!(
            "Environment {} not found",
            env_id
        )))
    }

    pub async fn delete_environment_domain(
        &self,
        project_id_p: i32,
        env_id: i32,
        domain_id: i32,
    ) -> Result<(), EnvironmentError> {
        let environment = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id_p))
            .filter(environments::Column::Id.eq(env_id))
            .one(self.db.as_ref())
            .await?;

        if let Some(env) = environment {
            let domain_to_delete = environment_domains::Entity::find()
                .filter(environment_domains::Column::EnvironmentId.eq(env.id))
                .filter(environment_domains::Column::Id.eq(domain_id))
                .one(self.db.as_ref())
                .await?;

            if let Some(_domain) = domain_to_delete {
                environment_domains::Entity::delete_by_id(domain_id)
                    .exec(self.db.as_ref())
                    .await?;

                return Ok(());
            }
        }

        Err(EnvironmentError::NotFound(format!(
            "Environment {} not found",
            env_id
        )))
    }

    /// Soft-delete an environment by setting its `deleted_at` timestamp.
    ///
    /// The environment row is preserved for historical data (deployments, analytics)
    /// and can be restored if a new environment with the same name/branch is created.
    ///
    /// Prevents deletion of:
    /// - Production environments (name = "Production" case-insensitive)
    ///
    /// Note: Active deployments should be cancelled before calling this method
    pub async fn delete_environment(
        &self,
        project_id: i32,
        env_id: i32,
    ) -> Result<(), EnvironmentError> {
        // Get the environment (only non-deleted ones)
        let environment = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::Id.eq(env_id))
            .filter(environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                EnvironmentError::NotFound(format!("Environment {} not found", env_id))
            })?;

        // Prevent deletion of production environments
        if environment.name.to_lowercase() == "production" {
            return Err(EnvironmentError::InvalidInput(
                "Cannot delete production environment".to_string(),
            ));
        }

        // Emit EnvironmentDeleted job so subscribers can clean up
        if let Some(queue_service) = &self.queue_service {
            let env_deleted_job = Job::EnvironmentDeleted(EnvironmentDeletedJob {
                environment_id: env_id,
                environment_name: environment.name.clone(),
                project_id,
            });

            if let Err(e) = queue_service.send(env_deleted_job).await {
                warn!(
                    "Failed to emit EnvironmentDeleted job for environment {}: {}",
                    env_id, e
                );
            }
        }

        // Soft-delete: set deleted_at and clear current_deployment_id
        let mut active_env: environments::ActiveModel = environment.into();
        active_env.deleted_at = Set(Some(chrono::Utc::now()));
        active_env.current_deployment_id = Set(None);
        active_env.update(self.db.as_ref()).await?;

        info!(
            "Soft-deleted environment {} in project {}",
            env_id, project_id
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn make_service(db: sea_orm::DatabaseConnection) -> EnvironmentService {
        let server_config = temps_config::ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            "postgres://localhost/test".to_string(),
            None,
            None,
        )
        .unwrap();
        let config_service = Arc::new(temps_config::ConfigService::new(
            Arc::new(server_config),
            Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        ));
        EnvironmentService::new(Arc::new(db), config_service)
    }

    #[test]
    fn test_environment_error_display() {
        let error = EnvironmentError::NotFound("test".to_string());
        assert_eq!(error.to_string(), "Environment not found");

        let error = EnvironmentError::InvalidInput("invalid input".to_string());
        assert_eq!(error.to_string(), "Invalid input: invalid input");

        let error = EnvironmentError::Other("some error".to_string());
        assert_eq!(error.to_string(), "Other error: some error");
    }

    #[test]
    fn test_branch_already_in_use_error_display() {
        let error = EnvironmentError::BranchAlreadyInUse {
            branch: "main".to_string(),
            env_name: "production".to_string(),
            project_id: 42,
        };
        assert_eq!(
            error.to_string(),
            "Branch 'main' is already used by environment 'production' in project 42"
        );
    }

    #[test]
    fn test_branch_already_in_use_maps_to_bad_request() {
        let error = EnvironmentError::BranchAlreadyInUse {
            branch: "main".to_string(),
            env_name: "production".to_string(),
            project_id: 1,
        };
        let problem = Problem::from(error);
        assert_eq!(problem.status_code, axum::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_domain_environment_struct() {
        let domain_env = DomainEnvironment {
            id: 1,
            name: "production".to_string(),
            slug: "prod".to_string(),
        };

        assert_eq!(domain_env.id, 1);
        assert_eq!(domain_env.name, "production");
        assert_eq!(domain_env.slug, "prod");
    }

    #[test]
    fn test_environment_error_from_db_err() {
        let db_error = DbErr::RecordNotFound("test".to_string());
        let env_error = EnvironmentError::from(db_error);

        match env_error {
            EnvironmentError::NotFound(_) => {}
            _ => panic!("Expected NotFound error"),
        }
    }

    /// create_new_environment rejects a branch already tracked by another environment
    /// in the same project.
    #[tokio::test]
    async fn test_create_environment_rejects_duplicate_branch() {
        let existing_env = environments::Model {
            id: 1,
            name: "production".to_string(),
            slug: "production".to_string(),
            subdomain: "my-project-production".to_string(),
            branch: Some("main".to_string()),
            project_id: 10,
            host: "".to_string(),
            upstreams: temps_entities::upstream_config::UpstreamList::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            last_deployment: None,
            current_deployment_id: None,
            deleted_at: None,
            deployment_config: None,
            is_preview: false,
        };

        let project = temps_entities::projects::Model {
            id: 10,
            name: "My Project".to_string(),
            slug: "my-project".to_string(),
            repo_name: "repo".to_string(),
            repo_owner: "owner".to_string(),
            directory: ".".to_string(),
            main_branch: "main".to_string(),
            preset: temps_entities::preset::Preset::NextJs,
            preset_config: None,
            deployment_config: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            is_deleted: false,
            deleted_at: None,
            last_deployment: None,
            is_public_repo: true,
            git_url: None,
            git_provider_connection_id: None,
            attack_mode: false,
            enable_preview_environments: false,
            source_type: temps_entities::source_type::SourceType::Git,
        };

        // Query sequence:
        //   1. find project by id            → returns project
        //   2. find env by project_id+name   → returns empty (no name conflict)
        //   3. find env by project_id+branch → returns existing_env (branch conflict)
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![project]])
            .append_query_results(vec![Vec::<environments::Model>::new()])
            .append_query_results(vec![vec![existing_env]])
            .into_connection();

        let svc = make_service(db);
        let result = svc
            .create_new_environment(10, "staging".to_string(), "main".to_string(), None)
            .await;

        assert!(result.is_err());
        assert!(
            matches!(
                result.unwrap_err(),
                EnvironmentError::BranchAlreadyInUse {
                    branch,
                    env_name,
                    project_id: 10,
                } if branch == "main" && env_name == "production"
            ),
            "Expected BranchAlreadyInUse error"
        );
    }

    /// update_environment_settings rejects a branch already tracked by a different
    /// environment in the same project.
    #[tokio::test]
    async fn test_update_settings_rejects_duplicate_branch() {
        let current_env = environments::Model {
            id: 2,
            name: "staging".to_string(),
            slug: "staging".to_string(),
            subdomain: "my-project-staging".to_string(),
            branch: Some("develop".to_string()),
            project_id: 10,
            host: "".to_string(),
            upstreams: temps_entities::upstream_config::UpstreamList::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            last_deployment: None,
            current_deployment_id: None,
            deleted_at: None,
            deployment_config: None,
            is_preview: false,
        };

        let conflicting_env = environments::Model {
            id: 1,
            name: "production".to_string(),
            slug: "production".to_string(),
            subdomain: "my-project-production".to_string(),
            branch: Some("main".to_string()),
            project_id: 10,
            host: "".to_string(),
            upstreams: temps_entities::upstream_config::UpstreamList::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            last_deployment: None,
            current_deployment_id: None,
            deleted_at: None,
            deployment_config: None,
            is_preview: false,
        };

        // Query sequence:
        //   1. get_environment (find by project_id + env_id)            → returns current_env
        //   2. find env by project_id + branch (excluding env_id=2)     → returns conflicting_env
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![current_env]])
            .append_query_results(vec![vec![conflicting_env]])
            .into_connection();

        let svc = make_service(db);
        let result = svc
            .update_environment_settings(
                10,
                2,
                crate::handlers::UpdateEnvironmentSettingsRequest {
                    branch: Some("main".to_string()),
                    cpu_request: None,
                    cpu_limit: None,
                    memory_request: None,
                    memory_limit: None,
                    replicas: None,
                    exposed_port: None,
                    automatic_deploy: None,
                    performance_metrics_enabled: None,
                    session_recording_enabled: None,
                    security: None,
                },
            )
            .await;

        assert!(result.is_err());
        assert!(
            matches!(
                result.unwrap_err(),
                EnvironmentError::BranchAlreadyInUse {
                    branch,
                    env_name,
                    project_id: 10,
                } if branch == "main" && env_name == "production"
            ),
            "Expected BranchAlreadyInUse error"
        );
    }

    /// update_environment_settings allows updating other settings while keeping
    /// the same branch (self-reference must not trigger the conflict check).
    #[tokio::test]
    async fn test_update_settings_allows_same_branch_on_same_env() {
        let current_env = environments::Model {
            id: 1,
            name: "production".to_string(),
            slug: "production".to_string(),
            subdomain: "my-project-production".to_string(),
            branch: Some("main".to_string()),
            project_id: 10,
            host: "".to_string(),
            upstreams: temps_entities::upstream_config::UpstreamList::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            last_deployment: None,
            current_deployment_id: None,
            deleted_at: None,
            deployment_config: None,
            is_preview: false,
        };

        // Query sequence:
        //   1. get_environment                  → returns current_env
        //   2. branch conflict check (id != 1)  → returns empty (no other env uses "main")
        //   3. update                            → returns updated env
        let updated_env = environments::Model {
            branch: Some("main".to_string()),
            ..current_env.clone()
        };

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![current_env]])
            .append_query_results(vec![Vec::<environments::Model>::new()])
            .append_query_results(vec![vec![updated_env]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let svc = make_service(db);
        let result = svc
            .update_environment_settings(
                10,
                1,
                crate::handlers::UpdateEnvironmentSettingsRequest {
                    branch: Some("main".to_string()),
                    cpu_request: None,
                    cpu_limit: None,
                    memory_request: None,
                    memory_limit: None,
                    replicas: None,
                    exposed_port: None,
                    automatic_deploy: None,
                    performance_metrics_enabled: None,
                    session_recording_enabled: None,
                    security: None,
                },
            )
            .await;

        assert!(
            result.is_ok(),
            "Should allow keeping the same branch: {:?}",
            result.err()
        );
    }
}
