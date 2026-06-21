use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder,
    Set, Statement, TransactionTrait,
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
            EnvironmentError::DatabaseConnectionError(_) => {
                // Log full details server-side, return generic message to client
                warn!("Database connection error: {}", error);
                temps_core::error_builder::internal_server_error()
                    .detail("A database error occurred while processing the request")
                    .build()
            }
            EnvironmentError::DatabaseError { .. } => {
                warn!("Database error: {}", error);
                temps_core::error_builder::internal_server_error()
                    .detail("A database error occurred while processing the request")
                    .build()
            }
            EnvironmentError::BranchAlreadyInUse { .. } => temps_core::error_builder::bad_request()
                .title("Branch Already In Use")
                .detail(error.to_string())
                .build(),
            EnvironmentError::Other(_) => {
                warn!("Environment error: {}", error);
                temps_core::error_builder::internal_server_error()
                    .detail("An internal error occurred while processing the request")
                    .build()
            }
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
        let settings = match self.config_service.get_settings().await {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    "Failed to load settings for URL computation, using defaults: {}",
                    e
                );
                Default::default()
            }
        };

        // Use external_url if configured, otherwise fall back to preview_domain
        let base_domain = settings.preview_domain.clone();

        // Determine protocol - use https if external_url is configured, otherwise http
        let protocol = if settings.external_url.is_some() {
            "https"
        } else {
            "http"
        };

        // Append the proxy port when it isn't the protocol's default — the
        // proxy listens on `ServerConfig.address` (e.g. `:8080`), so without
        // this the URL points at :80/:443 and is unreachable on a local /
        // non-standard-port instance. Only applies to the preview-domain path:
        // when `external_url` is set, that URL already encodes the real public
        // host/port (typically 443) and the internal proxy port is irrelevant.
        let port_suffix = self.port_suffix(protocol, settings.external_url.is_some());

        // <scheme>://<slug>.<preview_domain>[:port]
        format!(
            "{}://{}.{}{}",
            protocol, environment_slug, base_domain, port_suffix
        )
    }

    /// Returns `:<port>` when the proxy listens on a non-default port for the
    /// given scheme (i.e. not 80 for http, not 443 for https), otherwise an
    /// empty string. The port comes from the Rust proxy listener address via
    /// [`ConfigService::proxy_port`] — the single source of truth — rather than
    /// being parsed out of `preview_domain`.
    ///
    /// `external_url_set` short-circuits to no suffix: behind a reverse proxy /
    /// public domain the externally-visible port is whatever `external_url`
    /// uses, not the internal proxy listener port.
    fn port_suffix(&self, protocol: &str, external_url_set: bool) -> String {
        if external_url_set {
            return String::new();
        }
        let port = self.config_service.proxy_port();
        let is_default = (protocol == "http" && port == 80) || (protocol == "https" && port == 443);
        if is_default {
            String::new()
        } else {
            format!(":{}", port)
        }
    }

    /// Compute the full FQDN for an environment (without protocol)
    pub async fn compute_environment_fqdn(&self, environment_slug: &str) -> String {
        let settings = self.config_service.get_settings().await.unwrap_or_default();
        let base_domain = settings.preview_domain.clone();
        format!("{}.{}", environment_slug, base_domain)
    }

    /// Compute the URL for a user-supplied custom domain (verbatim host).
    /// Unlike `compute_environment_url`, this never appends `preview_domain` —
    /// the input is expected to already be a fully-qualified hostname.
    pub async fn compute_custom_domain_url(&self, domain: &str) -> String {
        let settings = self.config_service.get_settings().await.unwrap_or_default();
        let protocol = if settings.external_url.is_some() {
            "https"
        } else {
            "http"
        };
        let port_suffix = self.port_suffix(protocol, settings.external_url.is_some());
        format!("{}://{}{}", protocol, domain, port_suffix)
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

        let name = name.to_lowercase();
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
                ..Default::default()
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

        // Multiple environments can track the same branch (Vercel-like model:
        // e.g. "main" can deploy to both production and staging environments).

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
        let name = name.to_lowercase();
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

        // Update only the fields that are provided. These four use double-Option
        // semantics: `Some(inner)` means "apply" (inner `None` clears the column →
        // "no limit", inner `Some(n)` sets it); outer `None` means "leave unchanged".
        if let Some(cpu_request) = settings.cpu_request {
            deployment_config.cpu_request = cpu_request;
        }
        if let Some(cpu_limit) = settings.cpu_limit {
            deployment_config.cpu_limit = cpu_limit;
        }
        if let Some(memory_request) = settings.memory_request {
            deployment_config.memory_request = memory_request;
        }
        if let Some(memory_limit) = settings.memory_limit {
            deployment_config.memory_limit = memory_limit;
        }
        if settings.exposed_port.is_some() {
            deployment_config.exposed_port = settings.exposed_port;
        }
        if let Some(replicas) = settings.replicas {
            deployment_config.replicas = replicas;
        }
        if let Some(automatic_deploy) = settings.automatic_deploy {
            deployment_config.automatic_deploy = Some(automatic_deploy);
        }
        if let Some(performance_metrics_enabled) = settings.performance_metrics_enabled {
            deployment_config.performance_metrics_enabled = performance_metrics_enabled;
        }
        if let Some(session_recording_enabled) = settings.session_recording_enabled {
            deployment_config.session_recording_enabled = session_recording_enabled;
        }
        if let Some(mut security) = settings.security {
            // Preserve existing password_protection — it's managed separately via the `password` field
            if security.password_protection.is_none() {
                security.password_protection = deployment_config
                    .security
                    .as_ref()
                    .and_then(|s| s.password_protection.clone());
            }
            deployment_config.security = Some(security);
        }
        // Handle password protection: hash plaintext password with argon2
        if let Some(ref password) = settings.password {
            let mut security = deployment_config.security.clone().unwrap_or_default();
            if password.is_empty() {
                // Empty string removes password protection
                security.password_protection = None;
            } else {
                use argon2::password_hash::{rand_core::OsRng, SaltString};
                use argon2::{Argon2, PasswordHasher};
                let salt = SaltString::generate(&mut OsRng);
                let argon2 = Argon2::default();
                let hash = argon2
                    .hash_password(password.as_bytes(), &salt)
                    .map_err(|e| {
                        EnvironmentError::InvalidInput(format!(
                            "Failed to hash password for environment {}: {}",
                            env_id, e
                        ))
                    })?
                    .to_string();
                security.password_protection = Some(
                    temps_entities::deployment_config::PasswordProtectionConfig {
                        enabled: true,
                        password_hash: hash,
                    },
                );
            }
            deployment_config.security = Some(security);
        }
        if settings.target_nodes.is_some() {
            deployment_config.target_nodes = settings.target_nodes;
        }
        if settings.target_labels.is_some() {
            deployment_config.target_labels = settings.target_labels;
        }
        if let Some(anti_affinity) = settings.anti_affinity {
            deployment_config.anti_affinity = anti_affinity;
        }
        if let Some(on_demand) = settings.on_demand {
            deployment_config.on_demand = on_demand;
        }
        if let Some(idle_timeout_seconds) = settings.idle_timeout_seconds {
            deployment_config.idle_timeout_seconds = idle_timeout_seconds;
        }
        if let Some(wake_timeout_seconds) = settings.wake_timeout_seconds {
            deployment_config.wake_timeout_seconds = wake_timeout_seconds;
        }

        // Validate the deployment config
        deployment_config.validate().map_err(|e| {
            EnvironmentError::InvalidInput(format!("Invalid deployment config: {}", e))
        })?;

        // Multiple environments can track the same branch (Vercel-like model).

        active_model.deployment_config = Set(Some(deployment_config));
        active_model.branch = Set(settings.branch);
        if let Some(protected) = settings.protected {
            active_model.protected = Set(protected);
        }
        // attack_mode is a tri-state Option<Option<bool>> on the entity (not in
        // deployment_config). `Some(inner)` applies the change: inner `None`
        // clears the override (NULL → inherit project), inner `Some(b)` sets it.
        // Outer `None` leaves the column unchanged.
        if let Some(attack_mode) = settings.attack_mode {
            active_model.attack_mode = Set(attack_mode);
        }
        active_model.updated_at = Set(chrono::Utc::now());

        let updated_environment = active_model
            .update(self.db.as_ref())
            .await
            .map_err(|e| EnvironmentError::DatabaseConnectionError(e.to_string()))?;

        // When on-demand settings change, notify the proxy to reload routes so it
        // picks up sleeping-domain registrations and on-demand configs immediately.
        let on_demand_changed = settings.on_demand.is_some()
            || settings.idle_timeout_seconds.is_some()
            || settings.wake_timeout_seconds.is_some();
        if on_demand_changed {
            // "NOTIFY route_table_changes" is a fully hardcoded string — no
            // user-controlled data is interpolated. Statement::from_string is
            // safe here; PostgreSQL does not support parameterised NOTIFY.
            if let Err(e) = self
                .db
                .execute(sea_orm::Statement::from_string(
                    sea_orm::DatabaseBackend::Postgres,
                    "NOTIFY route_table_changes".to_string(),
                ))
                .await
            {
                tracing::error!(
                    error = %e,
                    environment_id = env_id,
                    "Failed to send route_table_changes NOTIFY after on-demand settings update"
                );
            }
        }

        Ok(updated_environment)
    }

    /// Rename the environment's auto-managed subdomain.
    ///
    /// Replaces both `environments.subdomain` and the matching row in
    /// `environment_domains` (the one created at environment-creation time)
    /// inside a single transaction. The old hostname stops resolving once
    /// the proxy reloads its route table.
    ///
    /// Returns `InvalidInput` if the slugified value is empty, exceeds the
    /// DNS label length limit, or collides with another environment in the
    /// same project.
    pub async fn update_environment_subdomain(
        &self,
        project_id: i32,
        env_id: i32,
        new_subdomain: String,
    ) -> Result<environments::Model, EnvironmentError> {
        let environment = self.get_environment(project_id, env_id).await?;

        let normalized = slugify(&new_subdomain);
        if normalized.is_empty() {
            return Err(EnvironmentError::InvalidInput(format!(
                "Subdomain '{}' is empty after normalization; use lowercase letters, digits, or hyphens",
                new_subdomain
            )));
        }
        if normalized.len() > 63 {
            return Err(EnvironmentError::InvalidInput(format!(
                "Subdomain '{}' is {} characters; DNS labels must be 63 characters or fewer",
                normalized,
                normalized.len()
            )));
        }

        if normalized == environment.subdomain {
            return Ok(environment);
        }

        // Reject collisions with any other environment in the same project.
        let conflict = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::Subdomain.eq(&normalized))
            .filter(environments::Column::Id.ne(env_id))
            .filter(environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?;
        if let Some(other) = conflict {
            return Err(EnvironmentError::InvalidInput(format!(
                "Subdomain '{}' is already used by environment '{}' in this project",
                normalized, other.name
            )));
        }

        let previous_subdomain = environment.subdomain.clone();

        let txn = self
            .db
            .begin()
            .await
            .map_err(|e| EnvironmentError::DatabaseConnectionError(e.to_string()))?;

        let mut active_model: environments::ActiveModel = environment.clone().into();
        active_model.subdomain = Set(normalized.clone());
        active_model.updated_at = Set(chrono::Utc::now());
        let updated = active_model
            .update(&txn)
            .await
            .map_err(|e| EnvironmentError::DatabaseConnectionError(e.to_string()))?;

        // Replace the auto-managed environment_domains row (the one whose
        // value matched the previous subdomain). Custom domains stay intact.
        let existing_domain = environment_domains::Entity::find()
            .filter(environment_domains::Column::EnvironmentId.eq(env_id))
            .filter(environment_domains::Column::Domain.eq(&previous_subdomain))
            .one(&txn)
            .await?;

        if let Some(existing) = existing_domain {
            let mut active_domain: environment_domains::ActiveModel = existing.into();
            active_domain.domain = Set(normalized.clone());
            active_domain
                .update(&txn)
                .await
                .map_err(|e| EnvironmentError::DatabaseConnectionError(e.to_string()))?;
        } else {
            // Defensive: if the auto row was previously deleted, recreate it
            // so the new subdomain still routes to this environment.
            let new_domain = environment_domains::ActiveModel {
                environment_id: Set(env_id),
                domain: Set(normalized.clone()),
                created_at: Set(chrono::Utc::now()),
                ..Default::default()
            };
            new_domain
                .insert(&txn)
                .await
                .map_err(|e| EnvironmentError::DatabaseConnectionError(e.to_string()))?;
        }

        txn.commit()
            .await
            .map_err(|e| EnvironmentError::DatabaseConnectionError(e.to_string()))?;

        if let Err(e) = self
            .db
            .execute(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "NOTIFY route_table_changes".to_string(),
            ))
            .await
        {
            tracing::error!(
                error = %e,
                environment_id = env_id,
                "Failed to send route_table_changes NOTIFY after subdomain rename"
            );
        }

        Ok(updated)
    }

    /// Set the sleeping state of an environment (for on-demand scale-to-zero).
    /// Uses atomic CAS (UPDATE WHERE) to prevent race conditions between
    /// concurrent API calls and proxy-initiated state transitions.
    /// Returns the updated environment model.
    pub async fn set_sleeping(
        &self,
        project_id: i32,
        env_id: i32,
        sleeping: bool,
    ) -> Result<environments::Model, EnvironmentError> {
        // First verify the environment exists, belongs to the project, and has on-demand enabled
        let environment = self.get_environment(project_id, env_id).await?;

        let on_demand = environment
            .deployment_config
            .as_ref()
            .map(|c| c.on_demand)
            .unwrap_or(false);

        if !on_demand {
            return Err(EnvironmentError::InvalidInput(format!(
                "Environment {} does not have on-demand mode enabled",
                env_id
            )));
        }

        // Already in the desired state
        if environment.sleeping == sleeping {
            return Ok(environment);
        }

        // Atomic CAS: only succeeds if state hasn't changed since we read it
        let result = self
            .db
            .execute(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                "UPDATE environments SET sleeping = $1, updated_at = NOW() WHERE id = $2 AND project_id = $3 AND sleeping = $4",
                [sleeping.into(), env_id.into(), project_id.into(), (!sleeping).into()],
            ))
            .await
            .map_err(|e| EnvironmentError::DatabaseConnectionError(e.to_string()))?;

        if result.rows_affected() == 0 {
            // Another caller already changed the state — re-read and return current
            return self.get_environment(project_id, env_id).await;
        }

        // Re-read the updated environment
        self.get_environment(project_id, env_id).await
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

    /// Multiple environments can track the same branch (Vercel-like model).
    /// create_new_environment should allow duplicate branches.
    #[tokio::test]
    async fn test_create_environment_allows_duplicate_branch() {
        // BranchAlreadyInUse check was removed to support multiple environments
        // tracking the same branch. This test verifies the error variant still
        // exists for backwards compatibility but is no longer triggered.
        let error = EnvironmentError::BranchAlreadyInUse {
            branch: "main".to_string(),
            env_name: "production".to_string(),
            project_id: 10,
        };
        assert!(error.to_string().contains("main"));
        assert!(error.to_string().contains("production"));
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
            protected: false,
            sleeping: false,
            attack_mode: None,
            last_activity_at: None,
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
            // 1. get_environment query
            .append_query_results(vec![vec![current_env]])
            // 2. update returns the updated model
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
                    target_nodes: None,
                    target_labels: None,
                    anti_affinity: None,
                    protected: None,
                    attack_mode: None,
                    on_demand: None,
                    idle_timeout_seconds: None,
                    wake_timeout_seconds: None,
                    password: None,
                },
            )
            .await;

        assert!(
            result.is_ok(),
            "Should allow keeping the same branch: {:?}",
            result.err()
        );
    }

    fn make_env_model(on_demand: bool, sleeping: bool) -> environments::Model {
        environments::Model {
            id: 1,
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
            current_deployment_id: Some(100),
            deleted_at: None,
            deployment_config: Some(temps_entities::deployment_config::DeploymentConfig {
                on_demand,
                idle_timeout_seconds: 300,
                wake_timeout_seconds: 30,
                ..Default::default()
            }),
            is_preview: false,
            protected: false,
            sleeping,
            attack_mode: None,
            last_activity_at: None,
        }
    }

    #[tokio::test]
    async fn test_set_sleeping_wakes_sleeping_environment() {
        let env = make_env_model(true, true);
        let woken = environments::Model {
            sleeping: false,
            ..env.clone()
        };

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // 1. get_environment (find by project_id + id)
            .append_query_results(vec![vec![env]])
            // 2. update returns the woken model
            .append_query_results(vec![vec![woken.clone()]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let svc = make_service(db);
        let result = svc.set_sleeping(10, 1, false).await;

        assert!(result.is_ok(), "Expected Ok, got {:?}", result.err());
        assert!(!result.unwrap().sleeping);
    }

    #[tokio::test]
    async fn test_set_sleeping_rejects_non_on_demand_environment() {
        let env = make_env_model(false, false);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![env]])
            .into_connection();

        let svc = make_service(db);
        let result = svc.set_sleeping(10, 1, true).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            EnvironmentError::InvalidInput(msg) => {
                assert!(
                    msg.contains("on-demand"),
                    "Error should mention on-demand: {}",
                    msg
                );
            }
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_set_sleeping_noop_when_already_in_desired_state() {
        let env = make_env_model(true, true);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Only the get_environment query — no update should happen
            .append_query_results(vec![vec![env.clone()]])
            .into_connection();

        let svc = make_service(db);
        let result = svc.set_sleeping(10, 1, true).await;

        assert!(result.is_ok());
        assert!(result.unwrap().sleeping, "Should still be sleeping");
    }

    #[tokio::test]
    async fn test_update_subdomain_rejects_empty_normalized_value() {
        let env = make_env_model(false, false);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![env]])
            .into_connection();
        let svc = make_service(db);

        let result = svc
            .update_environment_subdomain(10, 1, "!!!".to_string())
            .await;

        match result {
            Err(EnvironmentError::InvalidInput(msg)) => {
                assert!(
                    msg.contains("empty"),
                    "Error should mention empty normalization: {}",
                    msg
                );
            }
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_update_subdomain_rejects_too_long_label() {
        let env = make_env_model(false, false);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![env]])
            .into_connection();
        let svc = make_service(db);

        // 64 chars after slugify — exceeds DNS label limit.
        let too_long = "a".repeat(64);
        let result = svc.update_environment_subdomain(10, 1, too_long).await;

        match result {
            Err(EnvironmentError::InvalidInput(msg)) => {
                assert!(
                    msg.contains("63"),
                    "Error should mention DNS label limit: {}",
                    msg
                );
            }
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_update_subdomain_noop_when_unchanged() {
        let env = make_env_model(false, false);
        // env.subdomain is "my-project-staging" — slugifying that is identical
        let target = env.subdomain.clone();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Only the get_environment query — no conflict check or update
            .append_query_results(vec![vec![env.clone()]])
            .into_connection();
        let svc = make_service(db);

        let result = svc
            .update_environment_subdomain(10, 1, target.clone())
            .await;

        assert!(result.is_ok(), "Expected Ok, got {:?}", result.err());
        assert_eq!(result.unwrap().subdomain, target);
    }

    #[tokio::test]
    async fn test_update_subdomain_rejects_conflict_with_sibling() {
        let env = make_env_model(false, false);
        let conflict = environments::Model {
            id: 2,
            name: "production".to_string(),
            slug: "production".to_string(),
            subdomain: "myapp".to_string(),
            ..make_env_model(false, false)
        };

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // 1. get_environment
            .append_query_results(vec![vec![env]])
            // 2. conflict check returns the sibling env
            .append_query_results(vec![vec![conflict]])
            .into_connection();
        let svc = make_service(db);

        let result = svc
            .update_environment_subdomain(10, 1, "myapp".to_string())
            .await;

        match result {
            Err(EnvironmentError::InvalidInput(msg)) => {
                assert!(
                    msg.contains("already used"),
                    "Error should describe conflict: {}",
                    msg
                );
                assert!(
                    msg.contains("production"),
                    "Error should name the conflicting env: {}",
                    msg
                );
            }
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_set_sleeping_puts_environment_to_sleep() {
        let env = make_env_model(true, false);
        let sleeping = environments::Model {
            sleeping: true,
            ..env.clone()
        };

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![env]])
            .append_query_results(vec![vec![sleeping.clone()]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let svc = make_service(db);
        let result = svc.set_sleeping(10, 1, true).await;

        assert!(result.is_ok());
        assert!(result.unwrap().sleeping);
    }
}
