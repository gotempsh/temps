// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Compose configuration checks outside a deployment: the "Run compose check"
//! action, the settings preview (through [`ComposeConfigurationChecker`]), the
//! host's Compose CLI status, and the one-time revalidation after an upgrade.
//!
//! Every check runs [`ComposeExecutor::check_report`], which is the deploy
//! job's own `check_configuration`, against the policy, variables and Compose
//! settings the deployment would load. None of it touches running containers.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::sea_query::{OnConflict, Query};
use sea_orm::{
    ColumnTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};
use serde::Serialize;
use temps_core::EncryptionService;
use temps_deployer::compose::{
    redact_variable_text, ComposeExecutor, ComposeSupport, MIN_COMPOSE_VERSION,
};
use temps_deployer::compose_interpolation;
use temps_entities::preset::{DockerComposeConfig, Preset, PresetConfig};
use temps_entities::{
    compose_security_policies, compose_validation_reports, deployment_jobs, deployments,
    env_var_environments, env_vars, environments, projects,
};
use temps_git::GitProviderManager;
use temps_presets::compose_check::{
    compose_local_references, ComposeCheckErrorClass, ComposeCheckReport, ComposeCheckRequest,
    ComposeConfigurationChecker, ComposeContextError, ComposeProjectContext,
    ComposeRepositorySource, COMPOSE_VALIDATION_REVISION,
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

/// Projects selected per revalidation query, so a pass never holds more than
/// one batch of IDs in memory.
const REVALIDATION_BATCH: u64 = 100;

/// Most files a check requests for `include`/`extends` (every request
/// counts, whether or not the file exists), and the total size read.
/// Resolution itself allows 128 files and 4 MiB.
const MAX_REFERENCE_FILES: usize = 64;
const MAX_REFERENCE_BYTES: usize = 4 * 1024 * 1024;
/// Most Compose documents examined for references, counting a document once
/// per distinct set of variables it is included with. Matches resolution.
const MAX_REFERENCE_DOCUMENTS: usize = 128;
/// Most bytes of variables the include environments built while looking for
/// references may hold together; matches resolution's include bound.
const MAX_REFERENCE_SCOPE_BYTES: usize = 16 * 1024 * 1024;

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

    /// The project variables a deployment of `environment` receives, plus
    /// the `HOST` default, selected exactly as the deployment selects them
    /// (environment-scoped rows over global ones, preview inheritance).
    /// Linked-service, secrets-binding and deployment-token variables are
    /// generated per deployment (some by provisioning resources) and are not
    /// loaded; a required `${VAR:?}` that only they could satisfy is reported
    /// rather than failed (see `defer_unknown_required`).
    async fn environment_variables(
        &self,
        project_id: i32,
        environment: &environments::Model,
    ) -> Result<HashMap<String, String>, ComposeCheckServiceError> {
        let mut variables = HashMap::from([("HOST".to_string(), "0.0.0.0".to_string())]);
        let rows = env_vars::Entity::find()
            .filter(env_vars::Column::ProjectId.eq(project_id))
            .all(self.db.as_ref())
            .await
            .map_err(Self::database(project_id))?;
        let ids: Vec<i32> = rows.iter().map(|row| row.id).collect();
        let links = if ids.is_empty() {
            Vec::new()
        } else {
            env_var_environments::Entity::find()
                .filter(env_var_environments::Column::EnvVarId.is_in(ids))
                .all(self.db.as_ref())
                .await
                .map_err(Self::database(project_id))?
        };
        let effective = super::env_resolver::select_effective_environment_variables(
            rows,
            &links,
            environment.id,
            environment.is_preview,
        );
        for row in effective.into_values() {
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
        let environment_variables = self.environment_variables(project_id, &environment).await?;
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

    /// Run a check. Returns the report and whether every referenced
    /// repository file could be read; a failure with unread files may be the
    /// provider's fault rather than the configuration's.
    async fn run(
        &self,
        project_id: i32,
        source: ComposeSource,
        context: ComposeProjectContext,
    ) -> Result<(ComposeCheckReport, bool), ComposeCheckServiceError> {
        let mut notes = Vec::new();
        let (compose_file, content, repository_env) = self
            .read_source(project_id, &source, &context, &mut notes)
            .await?;
        let repository = match source {
            ComposeSource::Repository {
                connection_id,
                owner,
                name,
                git_ref,
            } => Some(ComposeRepositorySource {
                connection_id,
                owner,
                name,
                git_ref,
                directory: context.directory.clone(),
            }),
            ComposeSource::Inline(_) => None,
        };
        let (mut report, complete) = self
            .check_with_references(ComposeCheckRequest {
                project_name: context.project_name,
                compose_file,
                content,
                compose_override: context.compose_override,
                excluded_services: context.excluded_services,
                policy: context.policy,
                environment: context.environment,
                repository_env,
                repository,
                reference_files: BTreeMap::new(),
            })
            .await;
        report.notes.extend(notes);
        Ok((report, complete))
    }

    /// Fetch the files the Compose file references (when it came from a
    /// repository) and run the deploy job's checks on the complete set.
    async fn check_with_references(
        &self,
        mut request: ComposeCheckRequest,
    ) -> (ComposeCheckReport, bool) {
        let mut fetched = ReferenceFiles::default();
        if let Some(repository) = &request.repository {
            if request.reference_files.is_empty() {
                fetched = self.reference_files(repository, &request).await;
                request.reference_files = std::mem::take(&mut fetched.files);
            }
        }
        let complete = fetched.inconclusive.is_empty();
        let mut report = self.executor.check_report(request).await;
        report.notes.extend(fetched.missing);
        report.notes.extend(fetched.inconclusive);
        (report, complete)
    }

    /// The files `include` and `extends` reference, transitively, read from
    /// the repository at the check's revision; see [`collect_reference_files`].
    async fn reference_files(
        &self,
        repository: &ComposeRepositorySource,
        request: &ComposeCheckRequest,
    ) -> ReferenceFiles {
        let manager = &self.git_provider_manager;
        collect_reference_files(request, |path| async move {
            let path = repository_path(&repository.directory, &path);
            manager
                .read_repository_text_file(
                    repository.connection_id,
                    &repository.owner,
                    &repository.name,
                    &path,
                    &repository.git_ref,
                )
                .await
                .map_err(|error| {
                    if error.is_file_not_found() {
                        ReferenceReadError::NotFound
                    } else {
                        ReferenceReadError::Unavailable(format!(
                            "'{path}' could not be read at '{}': {error}",
                            repository.git_ref
                        ))
                    }
                })
        })
        .await
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
        Ok(self.run(project_id, source, context).await?.0)
    }

    /// The current state of a project's default branch, as revalidation sees
    /// it, and whether every referenced file could be read.
    pub async fn check_project(
        &self,
        project_id: i32,
    ) -> Result<(ComposeCheckReport, bool), ComposeCheckServiceError> {
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

    /// Re-check every Compose project whose stored check is missing or
    /// predates [`COMPOSE_VALIDATION_REVISION`]. Sequential and spaced out:
    /// one provider read and at most one `docker compose config` at a time.
    /// Outdated projects are selected in the database in keyset batches of
    /// [`REVALIDATION_BATCH`], so memory is bounded by one batch.
    ///
    /// A result is stored only when the check actually ran on every file:
    /// a project whose source could not be read (provider outage, rate
    /// limit, no repository) keeps its previous report and is tried again at
    /// the next startup, rather than being recorded as passing.
    pub async fn revalidate_outdated_projects(&self) -> Result<usize, ComposeCheckServiceError> {
        let mut flagged = 0;
        let mut after = 0;
        loop {
            let current = Query::select()
                .column(compose_validation_reports::Column::ProjectId)
                .from(compose_validation_reports::Entity)
                .and_where(
                    compose_validation_reports::Column::Revision.gte(COMPOSE_VALIDATION_REVISION),
                )
                .to_owned();
            let batch: Vec<i32> = projects::Entity::find()
                .select_only()
                .column(projects::Column::Id)
                .filter(projects::Column::Preset.eq(Preset::DockerCompose))
                .filter(projects::Column::IsDeleted.eq(false))
                .filter(projects::Column::Id.gt(after))
                .filter(projects::Column::Id.not_in_subquery(current))
                .order_by_asc(projects::Column::Id)
                .limit(REVALIDATION_BATCH)
                .into_tuple()
                .all(self.db.as_ref())
                .await
                .map_err(Self::database(0))?;
            let Some(&last) = batch.last() else {
                break;
            };
            after = last;
            for project_id in batch {
                if self.revalidate_project(project_id).await {
                    flagged += 1;
                }
                tokio::time::sleep(REVALIDATION_SPACING).await;
            }
        }
        Ok(flagged)
    }

    /// Check one project and store the result when it is conclusive.
    /// Returns whether the project was flagged as failing.
    async fn revalidate_project(&self, project_id: i32) -> bool {
        let report = match self.check_project(project_id).await {
            Ok((report, complete)) if report.passed || complete => report,
            Ok((report, _)) => {
                warn!(
                    project_id,
                    "Compose revalidation was inconclusive because referenced files could not be read; it will be retried at the next startup: {}",
                    report.error.as_deref().unwrap_or("no detail")
                );
                return false;
            }
            Err(error) => {
                warn!(
                    project_id,
                    "Compose revalidation could not run and will be retried at the next startup: {error}"
                );
                return false;
            }
        };
        if !report.passed {
            warn!(
                project_id,
                error_class = report
                    .error_class
                    .map(|class| class.as_str())
                    .unwrap_or("unknown"),
                "Compose project would fail its next deployment under the current validation rules: {}",
                report.error.as_deref().unwrap_or("no detail")
            );
        }
        if let Err(error) = self
            .store_report(project_id, report.passed, report.error_class, report.error)
            .await
        {
            warn!(
                project_id,
                "Failed to store Compose revalidation result: {error}"
            );
        }
        !report.passed
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
/// Why a referenced repository file was not read.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ReferenceReadError {
    /// The provider answered that the file does not exist at the revision.
    NotFound,
    /// The provider did not answer conclusively (outage, rate limit,
    /// credentials); the check cannot tell whether the file exists.
    Unavailable(String),
}

/// Referenced files read for a check, keyed by path relative to the Compose
/// project directory.
#[derive(Debug, Default)]
struct ReferenceFiles {
    files: BTreeMap<String, String>,
    /// Required files the repository does not contain. The check then fails
    /// exactly as the deployment would, so the result is conclusive.
    missing: Vec<String>,
    /// Files that could not be read, or were not requested because a limit
    /// was reached. A failing result may then be the provider's fault.
    inconclusive: Vec<String>,
}

/// The variables a document is examined with, and their identity.
type ReferenceScope = (Arc<HashMap<String, String>>, u64);

/// A Compose document whose references are still to be examined.
struct PendingDocument {
    path: String,
    /// Directory its references resolve against.
    base: String,
    variables: Arc<HashMap<String, String>>,
    /// Identifies `variables`; `0` for the root document's.
    scope: u64,
}

/// Read every file `include` and `extends` pull in, transitively, the way
/// resolution reaches them: each document is interpolated before its paths
/// are read, with the root variables (repository `.env` overridden by the
/// project environment) plus, for included files, the include's own env
/// files layered underneath. Every request counts against
/// [`MAX_REFERENCE_FILES`], found or not, and the bytes read against
/// [`MAX_REFERENCE_BYTES`].
async fn collect_reference_files<F, Fut>(request: &ComposeCheckRequest, read: F) -> ReferenceFiles
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<String, ReferenceReadError>>,
{
    let mut root_variables = request
        .repository_env
        .as_deref()
        .and_then(|content| compose_interpolation::parse_dotenv(content).ok())
        .unwrap_or_default();
    root_variables.extend(
        request
            .environment
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    let base = request
        .compose_file
        .trim_start_matches("./")
        .rsplit_once('/')
        .map(|(parent, _)| parent.to_string())
        .unwrap_or_default();
    let mut reader = ReferenceReader {
        read,
        cache: HashMap::new(),
        bytes: 0,
        stopped: false,
        result: ReferenceFiles::default(),
    };
    // Values that may appear in a referenced path once it is interpolated;
    // every note is redacted for them before it reaches the report.
    let mut sensitive: HashSet<String> = root_variables.values().cloned().collect();
    let mut traversal = Traversal::default();
    traversal.enqueue(PendingDocument {
        path: String::new(),
        base,
        variables: Arc::new(root_variables),
        scope: 0,
    });
    // Include environments by including scope and env files, so a repeated
    // include reuses one map instead of parsing and holding another.
    let mut scopes: HashMap<(u64, Vec<String>), ReferenceScope> = HashMap::new();
    let mut scope_bytes = 0_usize;
    'documents: while let Some(document) = traversal.queue.pop_front() {
        let content = if document.path.is_empty() {
            Some(request.content.clone())
        } else {
            reader.read(document.path.clone(), false).await
        };
        let Some(content) = content else {
            continue;
        };
        // Paths may be interpolated (`${DIR:-stack}/shared.yaml`), so they
        // are read from the document as resolution sees it. Unknown required
        // variables are deferred, as in the check itself.
        let interpolated = compose_interpolation::interpolate_document_deferring_required(
            "Compose reference",
            &content,
            &document.variables,
        )
        .map(|interpolated| interpolated.content)
        .unwrap_or(content);
        let references = compose_local_references(&document.base, &interpolated);
        for file in references.extends {
            traversal.enqueue(PendingDocument {
                base: parent_path(&file),
                path: file,
                variables: document.variables.clone(),
                scope: document.scope,
            });
        }
        for include in references.includes {
            let key = (
                document.scope,
                include
                    .env_files
                    .iter()
                    .map(|file| file.path.clone())
                    .collect::<Vec<_>>(),
            );
            let (variables, scope) = match scopes.get(&key) {
                Some(cached) => cached.clone(),
                None => {
                    let mut loaded: HashMap<String, String> = HashMap::new();
                    for env_file in &include.env_files {
                        if let Some(content) =
                            reader.read(env_file.path.clone(), env_file.optional).await
                        {
                            match compose_interpolation::parse_dotenv_layered(
                                &content,
                                &document.variables,
                                std::mem::take(&mut loaded),
                            ) {
                                Ok(layered) => loaded = layered,
                                // Resolution reports the oversized file itself.
                                Err(_) => break,
                            }
                        }
                    }
                    let scoped = if loaded.is_empty() {
                        (document.variables.clone(), document.scope)
                    } else {
                        let mut entries: Vec<(&String, &String)> = loaded.iter().collect();
                        entries.sort();
                        let mut hasher = std::collections::hash_map::DefaultHasher::new();
                        std::hash::Hash::hash(&(document.scope, &entries), &mut hasher);
                        let scope = std::hash::Hasher::finish(&hasher);
                        let mut variables = HashMap::clone(&document.variables);
                        for (key, value) in loaded {
                            sensitive.insert(value.clone());
                            variables.entry(key).or_insert(value);
                        }
                        scope_bytes = scope_bytes.saturating_add(
                            variables
                                .iter()
                                .map(|(key, value)| key.len() + value.len())
                                .sum::<usize>(),
                        );
                        if scope_bytes > MAX_REFERENCE_SCOPE_BYTES {
                            reader.result.inconclusive.push(format!(
                                "Stopped looking for referenced Compose files: include environments exceed {MAX_REFERENCE_SCOPE_BYTES} bytes."
                            ));
                            break 'documents;
                        }
                        (Arc::new(variables), scope)
                    };
                    scopes.insert(key, scoped.clone());
                    scoped
                }
            };
            for path in include.paths {
                traversal.enqueue(PendingDocument {
                    base: include
                        .project_directory
                        .clone()
                        .unwrap_or_else(|| parent_path(&path)),
                    path,
                    variables: variables.clone(),
                    scope,
                });
            }
        }
    }
    let mut result = reader.result;
    if traversal.limited {
        result.inconclusive.push(format!(
            "Compose files were examined for references up to the limit of {MAX_REFERENCE_DOCUMENTS} documents; later references were not read."
        ));
    }
    // A note may name a path built from a variable value (`${TOKEN}/x.yaml`).
    let sensitive: Vec<String> = sensitive.into_iter().collect();
    for note in result
        .missing
        .iter_mut()
        .chain(result.inconclusive.iter_mut())
    {
        *note = redact_variable_text(note, &sensitive);
    }
    result
}

/// Documents waiting to be examined. Each (path, base, scope) is queued at
/// most once, and at most [`MAX_REFERENCE_DOCUMENTS`] are ever queued, so
/// repeating a reference never grows the queue.
#[derive(Default)]
struct Traversal {
    queue: VecDeque<PendingDocument>,
    queued: HashSet<(String, String, u64)>,
    /// Set when a document was refused because the limit was reached.
    limited: bool,
}

impl Traversal {
    fn enqueue(&mut self, document: PendingDocument) {
        let key = (document.path.clone(), document.base.clone(), document.scope);
        if self.queued.contains(&key) {
            return;
        }
        if self.queued.len() >= MAX_REFERENCE_DOCUMENTS {
            self.limited = true;
            return;
        }
        self.queued.insert(key);
        self.queue.push_back(document);
    }
}

/// Reads referenced files once each, within the request and byte limits,
/// and classifies what could not be read.
struct ReferenceReader<F> {
    read: F,
    /// Every path requested so far and its content; `None` when unread.
    cache: HashMap<String, Option<String>>,
    bytes: usize,
    stopped: bool,
    result: ReferenceFiles,
}

impl<F, Fut> ReferenceReader<F>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<String, ReferenceReadError>>,
{
    async fn read(&mut self, path: String, optional: bool) -> Option<String> {
        if let Some(cached) = self.cache.get(&path) {
            return cached.clone();
        }
        if self.stopped {
            return None;
        }
        if self.cache.len() >= MAX_REFERENCE_FILES || self.bytes > MAX_REFERENCE_BYTES {
            self.stopped = true;
            self.result.inconclusive.push(format!(
                "Stopped reading referenced Compose files after {} requests ({} bytes); '{}' and any later references were not read.",
                self.cache.len(),
                self.bytes,
                path
            ));
            return None;
        }
        let outcome = match (self.read)(path.clone()).await {
            Ok(content) => {
                self.bytes = self.bytes.saturating_add(content.len());
                self.result.files.insert(path.clone(), content.clone());
                Some(content)
            }
            Err(ReferenceReadError::NotFound) => {
                if !optional {
                    self.result.missing.push(format!(
                        "Referenced file '{path}' does not exist in the repository at this revision."
                    ));
                }
                None
            }
            Err(ReferenceReadError::Unavailable(reason)) => {
                self.result
                    .inconclusive
                    .push(format!("Referenced file {reason}"));
                None
            }
        };
        self.cache.insert(path, outcome.clone());
        outcome
    }
}

fn parent_path(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(parent, _)| parent.to_string())
        .unwrap_or_default()
}

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
        self.check_with_references(request).await.0
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

    /// A second handle on a mock connection, for reading its log afterwards.
    fn log_handle(db: &DatabaseConnection) -> DatabaseConnection {
        match db {
            DatabaseConnection::MockDatabaseConnection(mock) => {
                DatabaseConnection::MockDatabaseConnection(mock.clone())
            }
            _ => panic!("tests use a mock database"),
        }
    }

    /// SQL run on the mock connection behind `db`.
    fn statements(db: DatabaseConnection) -> Vec<String> {
        db.into_transaction_log()
            .into_iter()
            .flat_map(|transaction| {
                transaction
                    .statements()
                    .iter()
                    .map(|statement| statement.sql.clone())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    #[tokio::test]
    async fn revalidation_selects_outdated_projects_in_bounded_database_batches() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<projects::Model>::new()])
            .into_connection();
        let log = log_handle(&db);
        let service = service(db);
        assert_eq!(service.revalidate_outdated_projects().await.unwrap(), 0);
        let statements = statements(log);
        assert_eq!(statements.len(), 1);
        // The current-report set stays in the database (an anti-join), and
        // each query is a bounded keyset page.
        assert!(
            statements[0].contains("NOT IN (SELECT"),
            "{}",
            statements[0]
        );
        assert!(statements[0].contains("LIMIT"), "{}", statements[0]);
    }

    #[tokio::test]
    async fn a_project_that_cannot_be_checked_is_not_recorded_as_passing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<projects::Model>::new()])
            .into_connection();
        let log = log_handle(&db);
        let service = service(db);
        assert!(!service.revalidate_project(12).await);
        // Only the project lookup ran: nothing was written, so the project
        // stays outdated and is retried at the next startup.
        let statements = statements(log);
        assert_eq!(statements.len(), 1);
        assert!(statements[0].starts_with("SELECT"));
    }

    fn reference_request(content: &str, environment: &[(&str, &str)]) -> ComposeCheckRequest {
        ComposeCheckRequest {
            project_name: "temps-reference-test".to_string(),
            compose_file: "compose.yaml".to_string(),
            content: content.to_string(),
            compose_override: None,
            excluded_services: Vec::new(),
            policy: Default::default(),
            environment: environment
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
            repository_env: None,
            repository: None,
            reference_files: BTreeMap::new(),
        }
    }

    /// Collect against an in-memory repository, returning what was read and
    /// every path requested.
    async fn collect(
        request: &ComposeCheckRequest,
        repository: &[(&str, Result<&str, ReferenceReadError>)],
    ) -> (ReferenceFiles, Vec<String>) {
        let repository: HashMap<String, Result<String, ReferenceReadError>> = repository
            .iter()
            .map(|(path, outcome)| (path.to_string(), outcome.clone().map(str::to_string)))
            .collect();
        let requested = std::sync::Mutex::new(Vec::new());
        let files = collect_reference_files(request, |path| {
            requested.lock().unwrap().push(path.clone());
            let outcome = repository
                .get(&path)
                .cloned()
                .unwrap_or(Err(ReferenceReadError::NotFound));
            async move { outcome }
        })
        .await;
        (files, requested.into_inner().unwrap())
    }

    #[tokio::test]
    async fn references_are_read_after_interpolation_and_from_the_project_root() {
        let request = reference_request(
            "include:\n  - ${STACK:-stack}/shared.yaml\n  - path: ${TEAM}/team.yaml\n    project_directory: .\n",
            &[("TEAM", "teams")],
        );
        let (files, requested) = collect(
            &request,
            &[
                ("stack/shared.yaml", Ok("services: {}\n")),
                ("teams/team.yaml", Ok("include: ['${NESTED}/inner.yaml']\n")),
                (".env", Ok("NESTED=from-include-env\n")),
                ("from-include-env/inner.yaml", Ok("services: {}\n")),
            ],
        )
        .await;
        assert!(
            files.files.contains_key("stack/shared.yaml"),
            "{requested:?}"
        );
        // `project_directory: .` is the project root: its `.env` applies to
        // the included file and its references resolve from the root.
        assert!(files.files.contains_key("teams/team.yaml"), "{requested:?}");
        assert!(
            files.files.contains_key("from-include-env/inner.yaml"),
            "{requested:?}"
        );
        assert!(files.missing.is_empty() && files.inconclusive.is_empty());
    }

    #[tokio::test]
    async fn every_reference_request_counts_against_the_limit() {
        let includes: String = (0..5000)
            .map(|index| format!("  - missing{index:04}.yaml\n"))
            .collect();
        let request = reference_request(&format!("include:\n{includes}"), &[]);
        let (files, requested) = collect(&request, &[]).await;
        assert_eq!(requested.len(), MAX_REFERENCE_FILES);
        assert!(!files.inconclusive.is_empty());
        assert!(files.inconclusive[0].starts_with("Stopped reading"));
    }

    #[tokio::test]
    async fn missing_references_are_conclusive_and_outages_are_not() {
        let request = reference_request("include: [stack/shared.yaml]", &[]);
        let (files, _) = collect(&request, &[]).await;
        // The repository answered: the deployment would fail the same way.
        assert_eq!(files.missing.len(), 1, "{files:?}");
        assert!(files.missing[0].contains("stack/shared.yaml"));
        assert!(files.inconclusive.is_empty());
        // The include's implicit `.env` is optional, missing or not.
        let (files, _) = collect(
            &request,
            &[
                (
                    "stack/.env",
                    Err(ReferenceReadError::Unavailable(
                        "'stack/.env' timed out".to_string(),
                    )),
                ),
                ("stack/shared.yaml", Ok("services: {}\n")),
            ],
        )
        .await;
        assert!(files.missing.is_empty());
        // An outage leaves the result inconclusive, so it is not stored.
        assert_eq!(files.inconclusive.len(), 1, "{files:?}");
    }

    #[tokio::test]
    async fn notes_never_reveal_values_substituted_into_paths() {
        let request = reference_request(
            "include:\n  - ${TOKEN}/child.yaml\n  - ${TOKEN}/flaky.yaml\n  - path: nested.yaml\n    env_file: nested.env\n",
            &[("TOKEN", "s3cr3t-token")],
        );
        let (files, _) = collect(
            &request,
            &[
                (
                    "s3cr3t-token/flaky.yaml",
                    Err(ReferenceReadError::Unavailable(
                        "'s3cr3t-token/flaky.yaml' could not be read: timed out".to_string(),
                    )),
                ),
                ("nested.yaml", Ok("include: ['${HIDDEN}/inner.yaml']\n")),
                ("nested.env", Ok("HIDDEN=include-env-value\n")),
            ],
        )
        .await;
        let notes = format!("{:?} {:?}", files.missing, files.inconclusive);
        assert_eq!(files.missing.len(), 2, "{notes}");
        assert_eq!(files.inconclusive.len(), 1, "{notes}");
        assert!(!notes.contains("s3cr3t-token"), "{notes}");
        assert!(!notes.contains("include-env-value"), "{notes}");
        assert!(notes.contains("<redacted>/child.yaml"), "{notes}");
    }

    /// An env file whose values double on every line: ~2 MiB once parsed.
    fn doubling_dotenv() -> String {
        let mut doubling = String::from("A0=xxxxxxxxxxxxxxxx\n");
        for index in 1..=16 {
            doubling.push_str(&format!(
                "A{index}=${{A{}}}${{A{}}}\n",
                index - 1,
                index - 1
            ));
        }
        doubling
    }

    #[tokio::test]
    async fn repeated_includes_share_one_bounded_environment() {
        let doubling = doubling_dotenv();
        let repeated = format!("include:\n{}", "  - child/compose.yaml\n".repeat(3000));
        let request = reference_request(&repeated, &[]);
        let (files, requested) = collect(
            &request,
            &[
                ("child/compose.yaml", Ok("services: {}\n")),
                ("child/.env", Ok(doubling.as_str())),
            ],
        )
        .await;
        assert_eq!(requested, vec!["child/.env", "child/compose.yaml"]);
        assert!(files.inconclusive.is_empty(), "{:?}", files.inconclusive);

        // Distinct environments share one budget.
        let distinct: String = (0..10)
            .map(|index| format!("  - path: child/compose.yaml\n    env_file: child/{index}.env\n"))
            .collect();
        let request = reference_request(&format!("include:\n{distinct}"), &[]);
        let paths: Vec<String> = (0..10).map(|index| format!("child/{index}.env")).collect();
        let mut repository: Vec<(&str, Result<&str, ReferenceReadError>)> = paths
            .iter()
            .map(|path| (path.as_str(), Ok(doubling.as_str())))
            .collect();
        repository.push(("child/compose.yaml", Ok("services: {}\n")));
        let (files, _) = collect(&request, &repository).await;
        assert!(
            files
                .inconclusive
                .iter()
                .any(|note| note.contains("include environments exceed")),
            "{:?}",
            files.inconclusive
        );
    }

    #[tokio::test]
    async fn later_include_env_files_shape_nested_reference_paths() {
        let request = reference_request(
            "include:\n  - path: child.yaml\n    env_file: [first.env, second.env]\n",
            &[],
        );
        let (files, requested) = collect(
            &request,
            &[
                ("child.yaml", Ok("include: ['${DIR}/inner.yaml']\n")),
                ("first.env", Ok("BASE=layer\n")),
                ("second.env", Ok("DIR=${BASE}-two\n")),
                ("layer-two/inner.yaml", Ok("services: {}\n")),
            ],
        )
        .await;
        assert!(
            files.files.contains_key("layer-two/inner.yaml"),
            "{requested:?}"
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
