// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Compose configuration checks outside a deployment: the "Run compose check"
//! action, the settings preview (through [`ComposeConfigurationChecker`]), the
//! host's Compose CLI status, and the one-time revalidation after an upgrade.
//!
//! Every check runs [`ComposeExecutor::check_report`], which is the deploy
//! job's own `check_configuration`, against the policy, variables and Compose
//! settings the deployment would load. None of it touches running containers.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ColumnTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};
use serde::Serialize;
use temps_core::EncryptionService;
use temps_deployer::compose::{ComposeExecutor, ComposeSupport, MIN_COMPOSE_VERSION};
use temps_entities::preset::{DockerComposeConfig, Preset, PresetConfig};
use temps_entities::{
    compose_security_policies, compose_validation_reports, deployment_jobs, deployments,
    env_var_environments, env_vars, environments, projects,
};
use temps_git::GitProviderManager;
use temps_presets::compose_check::{
    ComposeCheckErrorClass, ComposeCheckReport, ComposeCheckRequest, ComposeConfigurationChecker,
    ComposeContextError, ComposeProjectContext, COMPOSE_VALIDATION_REVISION,
};
use thiserror::Error;
use tracing::{debug, info, warn};
use utoipa::ToSchema;

/// File names tried, in order, when a project has no configured Compose path
/// (the same list and order the deploy job uses).
const STANDARD_COMPOSE_FILE_NAMES: [&str; 4] = [
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
];

/// Delay between projects during post-upgrade revalidation, so the pass never
/// competes with real deployments for the provider API or the Compose CLI.
const REVALIDATION_SPACING: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Error, Debug)]
pub enum ComposeCheckServiceError {
    #[error("Project {project_id} not found")]
    ProjectNotFound { project_id: i32 },
    #[error("Project {project_id} is not a Docker Compose project")]
    NotComposeProject { project_id: i32 },
    #[error("Environment {environment_id} not found in project {project_id}")]
    EnvironmentNotFound {
        project_id: i32,
        environment_id: i32,
    },
    #[error("Project {project_id} has no environment to check against")]
    NoEnvironment { project_id: i32 },
    #[error("Deployment {deployment_id} not found in project {project_id}")]
    DeploymentNotFound { project_id: i32, deployment_id: i32 },
    #[error(
        "Failed to decrypt environment variable '{key}' (id={env_var_id}) for project {project_id}: {reason}"
    )]
    Decrypt {
        project_id: i32,
        env_var_id: i32,
        key: String,
        reason: String,
    },
    #[error("Cannot read the Compose file for project {project_id}: {reason}")]
    SourceUnavailable { project_id: i32, reason: String },
    #[error("Database error while checking Compose project {project_id}: {source}")]
    Database {
        project_id: i32,
        #[source]
        source: DbErr,
    },
}

impl From<ComposeCheckServiceError> for ComposeContextError {
    fn from(error: ComposeCheckServiceError) -> Self {
        match error {
            ComposeCheckServiceError::ProjectNotFound { project_id } => {
                ComposeContextError::ProjectNotFound { project_id }
            }
            ComposeCheckServiceError::NotComposeProject { project_id } => {
                ComposeContextError::NotComposeProject { project_id }
            }
            ComposeCheckServiceError::EnvironmentNotFound {
                project_id,
                environment_id,
            } => ComposeContextError::EnvironmentNotFound {
                project_id,
                environment_id,
            },
            ComposeCheckServiceError::NoEnvironment { project_id }
            | ComposeCheckServiceError::DeploymentNotFound { project_id, .. }
            | ComposeCheckServiceError::Decrypt { project_id, .. }
            | ComposeCheckServiceError::SourceUnavailable { project_id, .. }
            | ComposeCheckServiceError::Database { project_id, .. } => ComposeContextError::Load {
                project_id,
                reason: error.to_string(),
            },
        }
    }
}

/// The host's Docker Compose CLI as probed at startup.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ComposeHostStatusResponse {
    /// False until the startup probe finishes.
    pub probed: bool,
    /// Whether the Compose CLI ran.
    pub available: bool,
    /// Whether every `docker compose config` option Temps needs is present.
    pub supported: bool,
    pub version: Option<String>,
    pub minimum_version: String,
    pub missing_flags: Vec<String>,
    /// Operator-facing explanation when not supported.
    pub problem: Option<String>,
    pub checked_at: Option<DateTime<Utc>>,
}

/// The last stored Compose check for a project.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ComposeValidationReportResponse {
    pub project_id: i32,
    pub passed: bool,
    pub error_class: Option<ComposeCheckErrorClass>,
    pub error: Option<String>,
    /// True when the check ran under an older validation revision than this
    /// server's and has not been repeated yet.
    pub outdated: bool,
    pub checked_at: DateTime<Utc>,
}

impl From<compose_validation_reports::Model> for ComposeValidationReportResponse {
    fn from(model: compose_validation_reports::Model) -> Self {
        Self {
            project_id: model.project_id,
            passed: model.passed,
            error_class: model
                .error_class
                .as_deref()
                .and_then(ComposeCheckErrorClass::parse),
            error: model.error,
            outdated: model.revision < COMPOSE_VALIDATION_REVISION,
            checked_at: model.checked_at,
        }
    }
}

/// Where a check reads the Compose file from.
enum ComposeSource {
    Repository {
        connection_id: i32,
        owner: String,
        name: String,
        git_ref: String,
    },
    Inline(String),
}

pub struct ComposeCheckService {
    db: Arc<DatabaseConnection>,
    encryption_service: Arc<EncryptionService>,
    git_provider_manager: Arc<GitProviderManager>,
    executor: Arc<ComposeExecutor>,
    /// Written once by the startup probe; read by the status endpoint.
    host_status: RwLock<Option<(ComposeSupport, DateTime<Utc>)>>,
}

impl ComposeCheckService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption_service: Arc<EncryptionService>,
        git_provider_manager: Arc<GitProviderManager>,
        executor: Arc<ComposeExecutor>,
    ) -> Self {
        Self {
            db,
            encryption_service,
            git_provider_manager,
            executor,
            host_status: RwLock::new(None),
        }
    }

    /// Probe the Compose CLI and remember the result. Logs a warning when
    /// Compose deployments that need resolution would fail on this host.
    pub async fn probe_host(&self) -> ComposeSupport {
        let support = ComposeExecutor::probe_compose_support().await;
        match &support.problem {
            Some(problem) => warn!(
                compose_version = support.version.as_deref().unwrap_or("unknown"),
                missing_flags = ?support.missing_flags,
                minimum_version = MIN_COMPOSE_VERSION,
                "Docker Compose on this host is not fully supported: {problem}"
            ),
            None => info!(
                compose_version = support.version.as_deref().unwrap_or("unknown"),
                "Docker Compose CLI supports every option Temps uses"
            ),
        }
        match self.host_status.write() {
            Ok(mut status) => *status = Some((support.clone(), Utc::now())),
            Err(poisoned) => *poisoned.into_inner() = Some((support.clone(), Utc::now())),
        }
        support
    }

    pub fn host_status(&self) -> ComposeHostStatusResponse {
        let status = match self.host_status.read() {
            Ok(status) => status.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        match status {
            Some((support, checked_at)) => ComposeHostStatusResponse {
                probed: true,
                available: support.available,
                supported: support.supported(),
                version: support.version,
                minimum_version: MIN_COMPOSE_VERSION.to_string(),
                missing_flags: support.missing_flags,
                problem: support.problem,
                checked_at: Some(checked_at),
            },
            None => ComposeHostStatusResponse {
                probed: false,
                available: false,
                supported: false,
                version: None,
                minimum_version: MIN_COMPOSE_VERSION.to_string(),
                missing_flags: Vec::new(),
                problem: None,
                checked_at: None,
            },
        }
    }

    fn database(project_id: i32) -> impl Fn(DbErr) -> ComposeCheckServiceError {
        move |source| ComposeCheckServiceError::Database { project_id, source }
    }

    async fn compose_project(
        &self,
        project_id: i32,
    ) -> Result<(projects::Model, DockerComposeConfig), ComposeCheckServiceError> {
        let project = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.db.as_ref())
            .await
            .map_err(Self::database(project_id))?
            .ok_or(ComposeCheckServiceError::ProjectNotFound { project_id })?;
        if project.preset != Preset::DockerCompose {
            return Err(ComposeCheckServiceError::NotComposeProject { project_id });
        }
        let config = match &project.preset_config {
            Some(PresetConfig::DockerCompose(config)) => config.clone(),
            _ => DockerComposeConfig::default(),
        };
        Ok((project, config))
    }

    async fn environment(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<environments::Model, ComposeCheckServiceError> {
        let query = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_null());
        match environment_id {
            Some(environment_id) => query
                .filter(environments::Column::Id.eq(environment_id))
                .one(self.db.as_ref())
                .await
                .map_err(Self::database(project_id))?
                .ok_or(ComposeCheckServiceError::EnvironmentNotFound {
                    project_id,
                    environment_id,
                }),
            // The oldest non-preview environment: the one created with the
            // project, which is its production environment.
            None => query
                .filter(environments::Column::IsPreview.eq(false))
                .order_by_asc(environments::Column::Id)
                .one(self.db.as_ref())
                .await
                .map_err(Self::database(project_id))?
                .ok_or(ComposeCheckServiceError::NoEnvironment { project_id }),
        }
    }

    /// The project variables a deployment of `environment_id` receives, plus
    /// the `HOST` default. Linked-service and deployment-token variables are
    /// generated per deployment and are not included.
    async fn environment_variables(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<HashMap<String, String>, ComposeCheckServiceError> {
        let mut variables = HashMap::from([("HOST".to_string(), "0.0.0.0".to_string())]);
        let rows = env_vars::Entity::find()
            .inner_join(env_var_environments::Entity)
            .filter(env_var_environments::Column::EnvironmentId.eq(environment_id))
            .filter(env_vars::Column::ProjectId.eq(project_id))
            .distinct()
            .all(self.db.as_ref())
            .await
            .map_err(Self::database(project_id))?;
        for row in rows {
            let value = if row.is_encrypted {
                self.encryption_service
                    .decrypt_string(&row.value)
                    .map_err(|error| ComposeCheckServiceError::Decrypt {
                        project_id,
                        env_var_id: row.id,
                        key: row.key.clone(),
                        reason: error.to_string(),
                    })?
            } else {
                row.value
            };
            variables.insert(row.key, value);
        }
        Ok(variables)
    }

    async fn context(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<(projects::Model, ComposeProjectContext), ComposeCheckServiceError> {
        let (project, config) = self.compose_project(project_id).await?;
        let environment = self.environment(project_id, environment_id).await?;
        let policy = compose_security_policies::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .map_err(Self::database(project_id))?
            .map(|row| row.policy)
            .unwrap_or_default();
        let environment_variables = self
            .environment_variables(project_id, environment.id)
            .await?;
        let context = ComposeProjectContext {
            project_id,
            environment_id: Some(environment.id),
            project_name: format!("temps-{}-{}", project_id, environment.id),
            directory: normalize_directory(&project.directory),
            policy,
            environment: environment_variables,
            compose_path: config.compose_path.clone(),
            compose_override: config.compose_override.clone(),
            excluded_services: config.excluded_services.clone(),
        };
        Ok((project, context))
    }

    /// Read the Compose file (trying the standard names when no path is
    /// configured) and, best-effort, the repository `.env` beside it.
    async fn read_source(
        &self,
        project_id: i32,
        source: &ComposeSource,
        context: &ComposeProjectContext,
        notes: &mut Vec<String>,
    ) -> Result<(String, String, Option<String>), ComposeCheckServiceError> {
        let (connection_id, owner, name, git_ref) = match source {
            ComposeSource::Inline(content) => {
                let file = context
                    .compose_path
                    .clone()
                    .unwrap_or_else(|| "docker-compose.yml".to_string());
                return Ok((file, content.clone(), None));
            }
            ComposeSource::Repository {
                connection_id,
                owner,
                name,
                git_ref,
            } => (*connection_id, owner, name, git_ref),
        };
        let candidates: Vec<String> = match &context.compose_path {
            Some(path) => vec![path.clone()],
            None => STANDARD_COMPOSE_FILE_NAMES
                .iter()
                .map(|name| name.to_string())
                .collect(),
        };
        let mut last_error = None;
        let mut found = None;
        for candidate in candidates {
            let path = repository_path(&context.directory, &candidate);
            match self
                .git_provider_manager
                .read_repository_text_file(connection_id, owner, name, &path, git_ref)
                .await
            {
                Ok(content) => {
                    found = Some((candidate, content));
                    break;
                }
                Err(error) => last_error = Some(format!("'{path}': {error}")),
            }
        }
        let Some((compose_file, content)) = found else {
            return Err(ComposeCheckServiceError::SourceUnavailable {
                project_id,
                reason: format!(
                    "no Compose file could be read at '{git_ref}' ({})",
                    last_error.unwrap_or_else(|| "no candidate paths".to_string())
                ),
            });
        };
        let env_path = repository_path(&context.directory, ".env");
        let repository_env = match self
            .git_provider_manager
            .read_repository_text_file(connection_id, owner, name, &env_path, git_ref)
            .await
        {
            Ok(content) => Some(content),
            Err(error) => {
                debug!(
                    project_id,
                    "Repository .env not read for the Compose check: {error}"
                );
                notes.push(
                    "No repository .env was read at this revision; variables are resolved from the environment's variables and defaults only.".to_string(),
                );
                None
            }
        };
        Ok((compose_file, content, repository_env))
    }

    async fn run(
        &self,
        project_id: i32,
        source: ComposeSource,
        context: ComposeProjectContext,
    ) -> Result<ComposeCheckReport, ComposeCheckServiceError> {
        let mut notes = Vec::new();
        let (compose_file, content, repository_env) = self
            .read_source(project_id, &source, &context, &mut notes)
            .await?;
        let mut report = self
            .executor
            .check_report(ComposeCheckRequest {
                project_name: context.project_name,
                compose_file,
                content,
                compose_override: context.compose_override,
                excluded_services: context.excluded_services,
                policy: context.policy,
                environment: context.environment,
                repository_env,
            })
            .await;
        notes.push(
            "Variables from linked services and the deployment token are generated per deployment and are not part of this check.".to_string(),
        );
        report.notes.extend(notes);
        Ok(report)
    }

    /// "Run compose check" for a deployment: the deployment's commit (or its
    /// inline Compose content), with the project's current settings, policy
    /// and the deployment environment's variables.
    pub async fn check_deployment(
        &self,
        project_id: i32,
        deployment_id: i32,
    ) -> Result<ComposeCheckReport, ComposeCheckServiceError> {
        let deployment = deployments::Entity::find_by_id(deployment_id)
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await
            .map_err(Self::database(project_id))?
            .ok_or(ComposeCheckServiceError::DeploymentNotFound {
                project_id,
                deployment_id,
            })?;
        let (project, context) = self
            .context(project_id, Some(deployment.environment_id))
            .await?;
        let source = match self.inline_content(project_id, deployment_id).await? {
            Some(content) => ComposeSource::Inline(content),
            None => {
                let git_ref = [
                    deployment.commit_sha.as_deref(),
                    deployment.tag_ref.as_deref(),
                    deployment.branch_ref.as_deref(),
                ]
                .into_iter()
                .flatten()
                .map(str::trim)
                .find(|value| !value.is_empty() && !value.eq_ignore_ascii_case("HEAD"))
                .unwrap_or(project.main_branch.as_str())
                .to_string();
                self.repository_source(&project, git_ref)?
            }
        };
        self.run(project_id, source, context).await
    }

    /// The current state of a project's default branch, as revalidation sees it.
    pub async fn check_project(
        &self,
        project_id: i32,
    ) -> Result<ComposeCheckReport, ComposeCheckServiceError> {
        let (project, context) = self.context(project_id, None).await?;
        let latest_inline = match deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project_id))
            .order_by_desc(deployments::Column::Id)
            .one(self.db.as_ref())
            .await
            .map_err(Self::database(project_id))?
        {
            Some(deployment) if project.git_provider_connection_id.is_none() => {
                self.inline_content(project_id, deployment.id).await?
            }
            _ => None,
        };
        let source = match latest_inline {
            Some(content) => ComposeSource::Inline(content),
            None => self.repository_source(&project, project.main_branch.clone())?,
        };
        self.run(project_id, source, context).await
    }

    fn repository_source(
        &self,
        project: &projects::Model,
        git_ref: String,
    ) -> Result<ComposeSource, ComposeCheckServiceError> {
        let connection_id = project.git_provider_connection_id.ok_or_else(|| {
            ComposeCheckServiceError::SourceUnavailable {
                project_id: project.id,
                reason: "the project has no connected Git provider and no inline Compose content; the check reads the Compose file through a provider connection".to_string(),
            }
        })?;
        Ok(ComposeSource::Repository {
            connection_id,
            owner: project.repo_owner.clone(),
            name: project.repo_name.clone(),
            git_ref,
        })
    }

    /// Inline Compose content recorded on a deployment's deploy job, for
    /// projects deployed without a repository.
    async fn inline_content(
        &self,
        project_id: i32,
        deployment_id: i32,
    ) -> Result<Option<String>, ComposeCheckServiceError> {
        let job = deployment_jobs::Entity::find()
            .filter(deployment_jobs::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_jobs::Column::JobType.eq("DeployComposeJob"))
            .one(self.db.as_ref())
            .await
            .map_err(Self::database(project_id))?;
        Ok(job.and_then(|job| job.job_config).and_then(|config| {
            config
                .get("compose_content")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        }))
    }

    pub async fn latest_report(
        &self,
        project_id: i32,
    ) -> Result<Option<ComposeValidationReportResponse>, ComposeCheckServiceError> {
        Ok(compose_validation_reports::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .map_err(Self::database(project_id))?
            .map(ComposeValidationReportResponse::from))
    }

    pub async fn store_report(
        &self,
        project_id: i32,
        passed: bool,
        error_class: Option<ComposeCheckErrorClass>,
        error: Option<String>,
    ) -> Result<(), ComposeCheckServiceError> {
        store_validation_report(self.db.as_ref(), project_id, passed, error_class, error).await
    }

    /// Re-check, once, every Compose project whose stored check is missing or
    /// predates [`COMPOSE_VALIDATION_REVISION`]. Sequential and spaced out:
    /// one provider read and at most one `docker compose config` at a time.
    /// O(outdated projects); after a pass every row is current, so later
    /// restarts select nothing.
    pub async fn revalidate_outdated_projects(&self) -> Result<usize, ComposeCheckServiceError> {
        let current: Vec<i32> = compose_validation_reports::Entity::find()
            .select_only()
            .column(compose_validation_reports::Column::ProjectId)
            .filter(compose_validation_reports::Column::Revision.gte(COMPOSE_VALIDATION_REVISION))
            .into_tuple()
            .all(self.db.as_ref())
            .await
            .map_err(Self::database(0))?;
        let projects: Vec<i32> = projects::Entity::find()
            .select_only()
            .column(projects::Column::Id)
            .filter(projects::Column::Preset.eq(Preset::DockerCompose))
            .filter(projects::Column::IsDeleted.eq(false))
            .filter(projects::Column::Id.is_not_in(current))
            .order_by_asc(projects::Column::Id)
            .into_tuple()
            .all(self.db.as_ref())
            .await
            .map_err(Self::database(0))?;
        let mut flagged = 0;
        for project_id in projects {
            let (passed, class, error) = match self.check_project(project_id).await {
                Ok(report) => (report.passed, report.error_class, report.error),
                // A project the check cannot read is not flagged as failing
                // validation; it is recorded so the pass does not repeat.
                Err(error) => {
                    warn!(project_id, "Compose revalidation could not run: {error}");
                    (true, None, None)
                }
            };
            if !passed {
                flagged += 1;
                warn!(
                    project_id,
                    error_class = class.map(|class| class.as_str()).unwrap_or("unknown"),
                    "Compose project would fail its next deployment under the current validation rules: {}",
                    error.as_deref().unwrap_or("no detail")
                );
            }
            if let Err(error) = self.store_report(project_id, passed, class, error).await {
                warn!(
                    project_id,
                    "Failed to store Compose revalidation result: {error}"
                );
            }
            tokio::time::sleep(REVALIDATION_SPACING).await;
        }
        Ok(flagged)
    }
}

/// Upsert a project's latest Compose check under the current revision.
pub async fn store_validation_report(
    db: &DatabaseConnection,
    project_id: i32,
    passed: bool,
    error_class: Option<ComposeCheckErrorClass>,
    error: Option<String>,
) -> Result<(), ComposeCheckServiceError> {
    compose_validation_reports::Entity::insert(compose_validation_reports::ActiveModel {
        project_id: Set(project_id),
        revision: Set(COMPOSE_VALIDATION_REVISION),
        passed: Set(passed),
        error_class: Set(error_class.map(|class| class.as_str().to_string())),
        error: Set(error),
        checked_at: Set(Utc::now()),
    })
    .on_conflict(
        OnConflict::column(compose_validation_reports::Column::ProjectId)
            .update_columns([
                compose_validation_reports::Column::Revision,
                compose_validation_reports::Column::Passed,
                compose_validation_reports::Column::ErrorClass,
                compose_validation_reports::Column::Error,
                compose_validation_reports::Column::CheckedAt,
            ])
            .to_owned(),
    )
    .exec(db)
    .await
    .map_err(|source| ComposeCheckServiceError::Database { project_id, source })?;
    Ok(())
}

/// `""`, `"/"` and `"./"` all mean the repository root, as in the deploy job.
fn normalize_directory(directory: &str) -> String {
    let directory = directory.trim().trim_start_matches("./").trim_matches('/');
    if directory.is_empty() || directory == "." {
        ".".to_string()
    } else {
        directory.to_string()
    }
}

/// A repository-relative path for `file` inside the project directory.
fn repository_path(directory: &str, file: &str) -> String {
    let file = file.trim_start_matches("./");
    if directory == "." {
        file.to_string()
    } else {
        format!("{directory}/{file}")
    }
}

#[async_trait]
impl ComposeConfigurationChecker for ComposeCheckService {
    async fn check(&self, request: ComposeCheckRequest) -> ComposeCheckReport {
        self.executor.check_report(request).await
    }

    async fn load_project_context(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<ComposeProjectContext, ComposeContextError> {
        self.context(project_id, environment_id)
            .await
            .map(|(_, context)| context)
            .map_err(ComposeContextError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    struct NoopQueue;

    #[async_trait]
    impl temps_core::JobQueue for NoopQueue {
        async fn send(&self, _job: temps_core::Job) -> Result<(), temps_core::QueueError> {
            Ok(())
        }
        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            panic!("subscribe is not used by the Compose check service")
        }
    }

    fn service(db: DatabaseConnection) -> ComposeCheckService {
        let db = Arc::new(db);
        let encryption = Arc::new(
            EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .expect("valid test key"),
        );
        let server_config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:3000".to_string(),
                "postgresql://test".to_string(),
                None,
                Some("127.0.0.1:3001".to_string()),
            )
            .expect("valid test server config"),
        );
        let config = Arc::new(temps_config::ConfigService::new(server_config, db.clone()));
        let git = Arc::new(GitProviderManager::new(
            db.clone(),
            encryption.clone(),
            Arc::new(NoopQueue),
            config,
        ));
        let executor = Arc::new(ComposeExecutor::new_with_handle(
            Arc::new(temps_core::DockerHandle::disabled(
                temps_core::PROFILE_CONTROL_PLANE,
                temps_core::CONTROL_PLANE_DOCKER_REASON,
            )),
            std::env::temp_dir(),
        ));
        ComposeCheckService::new(db, encryption, git, executor)
    }

    #[test]
    fn directories_and_paths_match_the_deploy_job() {
        for root in ["", "/", ".", "./", " ./ "] {
            assert_eq!(normalize_directory(root), ".");
        }
        assert_eq!(normalize_directory("/apps/web/"), "apps/web");
        assert_eq!(repository_path(".", "compose.yaml"), "compose.yaml");
        assert_eq!(repository_path("apps/web", "./.env"), "apps/web/.env");
    }

    #[test]
    fn stored_reports_flag_older_revisions() {
        let report = ComposeValidationReportResponse::from(compose_validation_reports::Model {
            project_id: 7,
            revision: COMPOSE_VALIDATION_REVISION - 1,
            passed: false,
            error_class: Some("policy_violation".to_string()),
            error: Some("privileged".to_string()),
            checked_at: Utc::now(),
        });
        assert!(report.outdated);
        assert_eq!(
            report.error_class,
            Some(ComposeCheckErrorClass::PolicyViolation)
        );
    }

    #[test]
    fn host_status_is_unprobed_until_the_probe_runs() {
        let service = service(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let status = service.host_status();
        assert!(!status.probed);
        assert!(!status.supported);
        assert_eq!(status.minimum_version, MIN_COMPOSE_VERSION);
    }

    #[tokio::test]
    async fn missing_projects_and_non_compose_projects_are_typed_errors() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<projects::Model>::new()])
            .into_connection();
        let error = service(db).check_project(41).await.unwrap_err();
        assert!(matches!(
            error,
            ComposeCheckServiceError::ProjectNotFound { project_id: 41 }
        ));
        assert_eq!(
            ComposeContextError::from(error),
            ComposeContextError::ProjectNotFound { project_id: 41 }
        );
    }

    #[tokio::test]
    async fn a_deployment_from_another_project_is_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<deployments::Model>::new()])
            .into_connection();
        let error = service(db).check_deployment(3, 99).await.unwrap_err();
        assert!(matches!(
            error,
            ComposeCheckServiceError::DeploymentNotFound {
                project_id: 3,
                deployment_id: 99
            }
        ));
        assert!(error.to_string().contains("Deployment 99"));
    }
}
