// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Import orchestrator service
//!
//! Coordinates import operations across multiple sources

use sea_orm::{DatabaseConnection, EntityTrait};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use temps_core::url_validation::validate_external_url;
use temps_import_types::{
    ImportCredentials, ImportPlan, ImportSelector, ImportSource, ValidationReport,
    WorkloadDescriptor, WorkloadId, WorkloadImporter,
};
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::{ImportServiceError, ImportServiceResult};
use crate::handlers::types::{
    CreatePlanResponse, ExecuteImportResponse, ImportExecutionStatus, ImportSourceInfo,
    ImportStatusResponse,
};

/// Stored import session
#[derive(Debug, Clone)]
struct ImportSession {
    #[allow(dead_code)]
    session_id: String,
    user_id: i32,
    plan: ImportPlan,
    validation: ValidationReport,
    #[allow(dead_code)]
    repository_id: Option<i32>,
    git_provider_connection_id: Option<i32>,
    repo_owner: Option<String>,
    repo_name: Option<String>,
    credentials: ImportCredentials,
    created_at: chrono::DateTime<chrono::Utc>,
}

/// Shared by every operation that reads or acts on a session
/// (`execute_import`, `get_status`): a session's plan carries the source
/// platform's env vars, so leaking one cross-user is a real credential
/// exposure, not just a privacy nicety. Returning `SessionNotFound` rather
/// than a distinct "forbidden" error deliberately avoids confirming to a
/// non-owner that the session id exists at all.
fn check_session_ownership(
    session: &ImportSession,
    user_id: i32,
    session_id: &str,
) -> ImportServiceResult<()> {
    if session.user_id != user_id {
        warn!(
            "User {} attempted to access session {} owned by user {}",
            user_id, session_id, session.user_id
        );
        return Err(ImportServiceError::SessionNotFound(session_id.to_string()));
    }
    Ok(())
}

/// Text to substitute for any secret value returned to a client. Matches the
/// `***` convention used everywhere else in the codebase (API keys, tokens).
const MASKED_SECRET: &str = "***";

/// How long an import session (plan + real, unmasked source credentials —
/// including a full kubeconfig with its private key, for Kubernetes) is
/// kept in memory before being pruned. A session only needs to survive long
/// enough for the user to review the plan and click execute; there's no
/// reason to hold a live credential set indefinitely for a plan nobody ever
/// executed.
const SESSION_TTL: chrono::Duration = chrono::Duration::hours(1);

/// Redact secrets out of a plan before it leaves the process as an HTTP
/// response — `CreatePlanResponse`/`ImportStatusResponse` return the whole
/// plan, and a plan built from a real platform carries the source
/// database's connection URL (with its password) and every captured env
/// var, `is_secret` ones included. The plan kept in the in-memory session
/// (used to actually run the import) is a separate clone and is untouched.
fn mask_plan_secrets(plan: &ImportPlan) -> ImportPlan {
    let mut masked = plan.clone();

    for service in &mut masked.services {
        let raw_source_url = service
            .parameters
            .get(super::resource_executor::SOURCE_URL_PARAM)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        if let Some(source_url) = service
            .parameters
            .get_mut(super::resource_executor::SOURCE_URL_PARAM)
        {
            *source_url = serde_json::json!(MASKED_SECRET);
        }

        // Every importer's dump_command() embeds this same URL verbatim
        // into a copy-pasteable pg_dump/mysqldump/mongodump command in
        // recommended_action — redact it there too, or the password leaks
        // right back out through this sibling field.
        if let Some(raw_url) = raw_source_url {
            for implication in &mut service.data_implications {
                if let Some(action) = &mut implication.recommended_action {
                    if action.contains(raw_url.as_str()) {
                        *action = action.replace(raw_url.as_str(), MASKED_SECRET);
                    }
                }
            }
        }
    }

    let mask_env_vars = |deployment: &mut temps_import_types::plan::DeploymentConfiguration| {
        for env_var in &mut deployment.env_vars {
            if env_var.is_secret {
                env_var.value = MASKED_SECRET.to_string();
            }
        }
    };
    mask_env_vars(&mut masked.deployment);
    for deployment in &mut masked.additional_deployments {
        mask_env_vars(deployment);
    }

    masked
}

/// Import orchestrator coordinating all import operations
pub struct ImportOrchestrator {
    db: Arc<DatabaseConnection>,
    importers: HashMap<ImportSource, Arc<dyn WorkloadImporter>>,
    git_provider_manager: Arc<temps_git::GitProviderManager>,
    project_service: Arc<temps_projects::ProjectService>,
    deployment_service: Arc<temps_deployments::DeploymentService>,
    /// Executes the platform-generic plan resources (services, data
    /// population, domains) around the importer's own execute.
    resource_executor: Arc<super::ResourceExecutor>,
    /// Settings access for the preview domain used in env-var rewriting.
    config_service: Arc<temps_config::ConfigService>,
    /// In-memory session storage (will be replaced with database storage later)
    sessions: Arc<RwLock<HashMap<String, ImportSession>>>,
}

/// Implementation of ImportServiceProvider for ImportOrchestrator
impl temps_import_types::ImportServiceProvider for ImportOrchestrator {
    fn db(&self) -> &DatabaseConnection {
        &self.db
    }

    fn project_service(&self) -> &dyn std::any::Any {
        self.project_service.as_ref()
    }

    fn deployment_service(&self) -> &dyn std::any::Any {
        self.deployment_service.as_ref()
    }

    fn git_provider_manager(&self) -> &dyn std::any::Any {
        self.git_provider_manager.as_ref()
    }
}

impl ImportOrchestrator {
    /// Create a new import orchestrator with required services
    pub fn new(
        db: Arc<DatabaseConnection>,
        git_provider_manager: Arc<temps_git::GitProviderManager>,
        project_service: Arc<temps_projects::ProjectService>,
        deployment_service: Arc<temps_deployments::DeploymentService>,
        resource_executor: Arc<super::ResourceExecutor>,
        config_service: Arc<temps_config::ConfigService>,
    ) -> Self {
        Self {
            db,
            importers: HashMap::new(),
            git_provider_manager,
            project_service,
            deployment_service,
            resource_executor,
            config_service,
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register an importer for a source
    pub fn register_importer(&mut self, importer: Arc<dyn WorkloadImporter>) {
        let source = importer.source();
        info!("Registering importer for source: {}", source);
        self.importers.insert(source, importer);
    }

    /// Read-lock the session map. A `.unwrap()` here would poison the lock on
    /// panic and wedge every subsequent import call until the process
    /// restarts — return a typed error instead so a single bad session can't
    /// take down the whole import subsystem.
    fn sessions_read(
        &self,
    ) -> ImportServiceResult<std::sync::RwLockReadGuard<'_, HashMap<String, ImportSession>>> {
        self.sessions.read().map_err(|_| {
            ImportServiceError::Internal(
                "import session store lock was poisoned by a prior panic".to_string(),
            )
        })
    }

    /// Write-locked counterpart of [`Self::sessions_read`].
    fn sessions_write(
        &self,
    ) -> ImportServiceResult<std::sync::RwLockWriteGuard<'_, HashMap<String, ImportSession>>> {
        self.sessions.write().map_err(|_| {
            ImportServiceError::Internal(
                "import session store lock was poisoned by a prior panic".to_string(),
            )
        })
    }

    /// Drop every session older than [`SESSION_TTL`] — a plan the user never
    /// executed (or a real-time browser tab they closed) must not keep its
    /// source credentials resident in memory forever. Piggybacks on
    /// `create_plan`'s own write-lock rather than a background task: import
    /// volume is low control-plane traffic, so an O(live sessions) sweep on
    /// every new plan is cheap and needs no extra lifecycle to manage.
    fn prune_expired_sessions(
        sessions: &mut std::sync::RwLockWriteGuard<'_, HashMap<String, ImportSession>>,
    ) {
        let now = chrono::Utc::now();
        let before = sessions.len();
        sessions.retain(|_, session| now - session.created_at < SESSION_TTL);
        let pruned = before - sessions.len();
        if pruned > 0 {
            info!(
                "Pruned {} expired import session(s) older than {}h",
                pruned,
                SESSION_TTL.num_hours()
            );
        }
    }

    /// Clear the real, unmasked source credentials (for Kubernetes, a full
    /// kubeconfig including its client private key) from a stored session
    /// once `execute_import` has read them to build its execution context.
    /// Nothing downstream of that one read site -- the importers' own
    /// `execute()`, the resource executor, the deployment verifier, or a
    /// later `get_status` call -- ever reads them again, including on a
    /// retry after a failed real run (the session is deliberately kept
    /// around for exactly that). So there's no reason to leave them
    /// resident for the rest of `SESSION_TTL`; a no-op if the session is
    /// already gone (e.g. a concurrent successful-run eviction raced this).
    fn clear_session_credentials(
        sessions: &mut std::sync::RwLockWriteGuard<'_, HashMap<String, ImportSession>>,
        session_id: &str,
    ) {
        if let Some(stored) = sessions.get_mut(session_id) {
            stored.credentials = temps_import_types::ImportCredentials::default();
        }
    }

    /// Get an importer for a source
    fn get_importer(
        &self,
        source: ImportSource,
    ) -> ImportServiceResult<&Arc<dyn WorkloadImporter>> {
        self.importers
            .get(&source)
            .ok_or_else(|| ImportServiceError::SourceNotAvailable(source.to_string()))
    }

    /// List available import sources
    pub async fn list_sources(&self) -> ImportServiceResult<Vec<ImportSourceInfo>> {
        let mut sources = Vec::new();

        for (source, importer) in &self.importers {
            let available = importer.health_check().await.unwrap_or(false);
            let capabilities = importer.capabilities();

            sources.push(ImportSourceInfo {
                source: *source,
                name: importer.name().to_string(),
                version: importer.version().to_string(),
                available,
                capabilities: crate::handlers::types::ImportSourceCapabilities {
                    supports_volumes: capabilities.supports_volumes,
                    supports_networks: capabilities.supports_networks,
                    supports_health_checks: capabilities.supports_health_checks,
                    supports_resource_limits: capabilities.supports_resource_limits,
                    supports_build: capabilities.supports_build,
                    supports_services: capabilities.supports_services,
                    supports_domains: capabilities.supports_domains,
                    supports_project_snapshot: capabilities.supports_project_snapshot,
                    supports_cost_analysis: capabilities.supports_cost_analysis,
                    requires_credentials: source.requires_credentials(),
                },
            });
        }

        Ok(sources)
    }

    /// Discover workloads from a source
    pub async fn discover(
        &self,
        source: ImportSource,
        credentials: &ImportCredentials,
        selector: ImportSelector,
    ) -> ImportServiceResult<Vec<WorkloadDescriptor>> {
        debug!("Discovering workloads from source: {}", source);

        // SSRF guard (Fix #16): validate base_url before forwarding credentials
        // to the importer, which would make outbound HTTP requests.
        if let Some(ref url) = credentials.base_url {
            validate_external_url(url).map_err(|e| ImportServiceError::InvalidBaseUrl {
                url: url.clone(),
                reason: e.to_string(),
            })?;
        }

        let importer = self.get_importer(source)?;
        let workloads = importer.discover(credentials, selector).await?;

        info!("Discovered {} workloads from {}", workloads.len(), source);
        Ok(workloads)
    }

    /// Create an import plan
    pub async fn create_plan(
        &self,
        user_id: i32,
        source: ImportSource,
        workload_id: WorkloadId,
        credentials: &ImportCredentials,
        repository_id: Option<i32>,
    ) -> ImportServiceResult<CreatePlanResponse> {
        debug!(
            "Creating import plan for workload: {} from source: {} (repository: {:?})",
            workload_id, source, repository_id
        );

        // SSRF guard (Fix #16): validate base_url before forwarding credentials
        // to the importer, which would make outbound HTTP requests.
        if let Some(ref url) = credentials.base_url {
            validate_external_url(url).map_err(|e| ImportServiceError::InvalidBaseUrl {
                url: url.clone(),
                reason: e.to_string(),
            })?;
        }

        let importer = self.get_importer(source)?;

        // Get detailed snapshot and generate the plan. Importers that support
        // project-level snapshots (platform/cluster migrations) go through
        // describe_project/generate_project_plan so attached services, domains,
        // and cost analysis make it into the plan; workload-level importers
        // (Docker) keep the original describe/generate_plan path.
        let (snapshot, mut plan) = if importer.capabilities().supports_project_snapshot {
            let project_snapshot = importer.describe_project(credentials, &workload_id).await?;
            let plan = importer.generate_project_plan(project_snapshot.clone())?;
            (project_snapshot.primary_workload, plan)
        } else {
            let snapshot = importer.describe(credentials, &workload_id).await?;
            let plan = importer.generate_plan(snapshot.clone())?;
            (snapshot, plan)
        };

        // Skipped domains are IP-tied (sslip.io & friends) and would keep
        // pointing at the source machine — show the user the temps address
        // that replaces them, right in the reviewable plan.
        match self.config_service.get_settings().await {
            Ok(settings) => {
                let preview_host = super::resource_executor::preview_hostname(
                    &settings.preview_domain,
                    &plan.environment.subdomain,
                );
                super::resource_executor::annotate_skipped_domains(&mut plan, &preview_host);
            }
            Err(e) => {
                warn!(
                    "Could not load settings to annotate replacement domains in the plan: {}",
                    e
                );
            }
        }

        // Fetch repository information if repository ID is provided
        let (git_provider_connection_id, repo_owner, repo_name) = if let Some(repo_id) =
            repository_id
        {
            debug!("Fetching repository {} information", repo_id);

            // Fetch repository to get git_provider_connection_id, owner, and name
            use temps_entities::repositories;
            let repository = repositories::Entity::find_by_id(repo_id)
                .one(self.db.as_ref())
                .await
                .map_err(|e| {
                    ImportServiceError::Internal(format!("Failed to fetch repository: {}", e))
                })?
                .ok_or_else(|| {
                    ImportServiceError::Validation(format!("Repository {} not found", repo_id))
                })?;

            // Repository always has git_provider_connection_id now (no longer Optional)
            let git_provider_conn_id = repository.git_provider_connection_id;

            let owner = repository.owner.clone();
            let name = repository.name.clone();

            debug!(
                "Repository {} has git_provider_connection_id: {}, owner: {}, name: {}",
                repo_id, git_provider_conn_id, owner, name
            );

            // Detect preset from repository
            match self
                .git_provider_manager
                .calculate_repository_preset_live(repo_id, None)
                .await
            {
                Ok(preset_info) => {
                    // Find root preset (path == "./")
                    if let Some(root_preset) = preset_info.presets.iter().find(|p| p.path == "./") {
                        info!(
                            "Detected preset '{}' for repository {}",
                            root_preset.preset, repo_id
                        );

                        // Update plan with preset information
                        // Note: We'll add a preset field to BuildConfiguration
                        if plan.deployment.build.is_none() {
                            use temps_import_types::plan::BuildConfiguration;
                            plan.deployment.build = Some(BuildConfiguration {
                                context: ".".to_string(),
                                dockerfile: None,
                                args: std::collections::HashMap::new(),
                                target: None,
                            });
                        }

                        // Add preset as metadata in build args
                        if let Some(ref mut build) = plan.deployment.build {
                            build
                                .args
                                .insert("DETECTED_PRESET".to_string(), root_preset.preset.clone());
                        }

                        // Add warning if Docker image will be replaced by build
                        plan.metadata.warnings.push(format!(
                            "Repository preset detected: {}. Consider using buildpack deployment instead of Docker image",
                            root_preset.preset_label
                        ));
                    } else {
                        debug!("No preset detected for repository {}", repo_id);
                        plan.metadata.warnings.push(
                            "No buildpack preset detected in repository. Will use Docker image deployment".to_string()
                        );
                    }
                }
                Err(e) => {
                    info!("Failed to detect preset for repository {}: {}", repo_id, e);
                    plan.metadata.warnings.push(format!(
                        "Failed to detect repository preset: {}. Will use Docker image deployment",
                        e
                    ));
                }
            }

            (Some(git_provider_conn_id), Some(owner), Some(name))
        } else {
            (None, None, None)
        };

        // Run validations
        let validation = importer.validate(&snapshot, &plan);

        // Generate session ID
        let session_id = Uuid::new_v4().to_string();

        // Store session in memory (will be replaced with database storage later)
        let session = ImportSession {
            session_id: session_id.clone(),
            user_id,
            plan: plan.clone(),
            validation: validation.clone(),
            repository_id,
            git_provider_connection_id,
            repo_owner,
            repo_name,
            credentials: credentials.clone(),
            created_at: chrono::Utc::now(),
        };

        {
            let mut sessions = self.sessions_write()?;
            Self::prune_expired_sessions(&mut sessions);
            sessions.insert(session_id.clone(), session);
        }

        info!(
            "Created import plan for session: {} (can_execute: {})",
            session_id,
            validation.can_proceed()
        );

        // The session keeps the real, unmasked plan above (it's what
        // execute_import reads to actually connect to the source database
        // and inject env vars) — the response the client sees is a
        // separately masked copy.
        Ok(CreatePlanResponse {
            session_id,
            plan: mask_plan_secrets(&plan),
            validation: validation.clone(),
            can_execute: validation.can_proceed(),
        })
    }

    /// Execute an import
    #[allow(clippy::too_many_arguments)]
    pub async fn execute_import(
        &self,
        user_id: i32,
        session_id: String,
        project_name: String,
        preset: String,
        directory: String,
        main_branch: String,
        dry_run: bool,
    ) -> ImportServiceResult<ExecuteImportResponse> {
        info!(
            "Executing import for session: {} (dry_run: {})",
            session_id, dry_run
        );

        // Retrieve session from memory
        let session = {
            let sessions = self.sessions_read()?;
            sessions
                .get(&session_id)
                .cloned()
                .ok_or_else(|| ImportServiceError::SessionNotFound(session_id.clone()))?
        };

        // Verify user owns this session
        check_session_ownership(&session, user_id, &session_id)?;

        // See clear_session_credentials's doc comment for the full rationale.
        match self.sessions_write() {
            Ok(mut sessions) => Self::clear_session_credentials(&mut sessions, &session_id),
            Err(e) => warn!(
                "Could not clear credentials from import session {} after use: {} — they will still expire via SESSION_TTL",
                session_id, e
            ),
        }

        // Check if validation passed
        if !session.validation.can_proceed() {
            warn!(
                "Session {} has validation errors, cannot execute",
                session_id
            );
            return Err(ImportServiceError::ValidationFailed);
        }

        // Get the importer for this session's plan source
        let source = ImportSource::from_str(&session.plan.source)?;
        let importer = self.get_importer(source)?;

        // Create execution context (keep the branch around — the context is
        // moved into the importer, but the deploy phase needs it too)
        let deploy_branch = main_branch.clone();
        let context = temps_import_types::ImportContext {
            session_id: session_id.clone(),
            user_id,
            dry_run,
            project_name,
            preset,
            directory,
            main_branch,
            git_provider_connection_id: session.git_provider_connection_id,
            repo_owner: session.repo_owner,
            repo_name: session.repo_name,
            credentials: session.credentials,
            metadata: std::collections::HashMap::new(),
        };

        let mut plan = session.plan.clone();
        let mut pre_steps: Vec<temps_import_types::StepResult> = Vec::new();

        // Phase 1 (real runs only): create managed services BEFORE the
        // project so env vars can point at them from the first deployment.
        let created_services = if dry_run {
            Vec::new()
        } else {
            let (steps, created, _resources) = self
                .resource_executor
                .create_services(&plan, &context.project_name)
                .await;
            pre_steps.extend(steps);

            // Phase 2: rewrite env vars — source database URLs point at the
            // new managed services, source-generated domains point at the
            // environment's preview hostname.
            match self.config_service.get_settings().await {
                Ok(settings) => {
                    let preview_host = super::resource_executor::preview_hostname(
                        &settings.preview_domain,
                        &plan.environment.subdomain,
                    );
                    let rewrites = super::resource_executor::build_env_rewrites(
                        &plan,
                        &created,
                        &preview_host,
                    );
                    let changed =
                        super::resource_executor::apply_env_rewrites(&mut plan, &rewrites);
                    if changed > 0 {
                        pre_steps.push(temps_import_types::StepResult {
                            step_id: "rewrite-env-vars".to_string(),
                            step_title: "Rewrite environment variables for temps".to_string(),
                            success: true,
                            skipped: false,
                            message: format!(
                                "Rewrote {} environment variable(s): source database URLs now point at the new managed service(s) and source-generated domains at '{}'",
                                changed, preview_host
                            ),
                            created_resources: vec![],
                            duration_seconds: 0.0,
                        });
                    }
                }
                Err(e) => {
                    warn!(
                        "Could not load settings for env-var rewriting during import {}: {} — imported values are unchanged",
                        session_id, e
                    );
                }
            }
            created
        };

        // Delegate project/environment/deployment creation to the importer
        let mut outcome = importer
            .execute(
                context,
                plan.clone(),
                self as &dyn temps_import_types::ImportServiceProvider,
            )
            .await
            .map_err(|e| ImportServiceError::ExecutionFailed(e.to_string()))?;

        // Phase 3 (real runs only, once the environment exists): custom
        // domains and data population.
        if !dry_run && outcome.success {
            if let (Some(project_id), Some(environment_id)) =
                (outcome.project_id, outcome.environment_id)
            {
                let (domain_steps, domain_resources) = self
                    .resource_executor
                    .create_domains(&plan, project_id, environment_id)
                    .await;
                outcome.step_results.extend(domain_steps);
                outcome.created_resources.extend(domain_resources);
            }
            let populate_steps = self
                .resource_executor
                .populate_services(&created_services)
                .await;
            outcome.step_results.extend(populate_steps);
        }

        // Phase 4 (real runs only): actually deploy the imported project and
        // verify it answers. Creating rows is not a migration — until the app
        // has been built, started, and responded, nothing has moved.
        let mut operational = false;
        if !dry_run && outcome.success {
            if let (Some(project_id), Some(environment_id)) =
                (outcome.project_id, outcome.environment_id)
            {
                let app_url = self.environment_url(environment_id).await;
                let verifier = super::DeploymentVerifier::new(
                    self.db.clone(),
                    self.deployment_service.clone(),
                );
                let verification = verifier
                    .deploy_and_verify(
                        project_id,
                        environment_id,
                        Some(
                            plan.deployment
                                .git
                                .as_ref()
                                .map(|g| g.branch.clone())
                                .filter(|b| !b.is_empty())
                                .unwrap_or_else(|| deploy_branch.clone()),
                        ),
                        app_url,
                        outcome.deployment_id,
                    )
                    .await;
                operational = verification.operational;
                outcome.step_results.extend(verification.steps);
                outcome.warnings.extend(verification.warnings);
                // The pipeline's deployment is the real one; the importer's
                // placeholder row is only a record of what was imported.
                if let Some(deployment_id) = verification.deployment_id {
                    outcome.deployment_id = Some(deployment_id);
                }
            }
        }

        // Pre-execution steps (services, env rewrite) come first in the report
        let mut step_results = pre_steps;
        step_results.extend(outcome.step_results);

        // An import only counts as completed when the application is actually
        // running. Everything else is reported as failed so the user is never
        // told a migration succeeded while the app is down — the created
        // resources are listed in the steps either way.
        let status = if outcome.success && (dry_run || operational) {
            ImportExecutionStatus::Completed
        } else {
            ImportExecutionStatus::Failed
        };

        // A completed real import has no further use for its source
        // credentials — evict it now rather than waiting out SESSION_TTL.
        // Failed real runs and dry runs are left in place: a failure may be
        // worth retrying against the same reviewed plan, and a dry run is
        // explicitly a rehearsal for a real execute call that follows.
        if !dry_run && status == ImportExecutionStatus::Completed {
            match self.sessions_write() {
                Ok(mut sessions) => {
                    sessions.remove(&session_id);
                }
                Err(e) => warn!(
                    "Could not evict completed import session {}: {} — it will still expire via SESSION_TTL",
                    session_id, e
                ),
            }
        }

        Ok(ExecuteImportResponse {
            session_id: outcome.session_id,
            status,
            project_id: outcome.project_id,
            environment_id: outcome.environment_id,
            deployment_id: outcome.deployment_id,
            step_results,
        })
    }

    /// The address the imported app should answer on, used for the post-deploy
    /// check. Always the environment's own preview URL: it is served by this
    /// proxy immediately, while imported custom domains only resolve here
    /// after the user cuts DNS over — probing those would report a healthy
    /// app as down.
    async fn environment_url(&self, environment_id: i32) -> Option<String> {
        let environment = temps_entities::environments::Entity::find_by_id(environment_id)
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten()?;
        self.deployment_service
            .compute_environment_url(&environment.subdomain)
            .await
            .ok()
    }

    /// Get import status
    pub async fn get_status(
        &self,
        user_id: i32,
        session_id: &str,
    ) -> ImportServiceResult<ImportStatusResponse> {
        debug!("Getting status for import session: {}", session_id);

        // Retrieve session from memory
        let session = {
            let sessions = self.sessions_read()?;
            sessions
                .get(session_id)
                .cloned()
                .ok_or_else(|| ImportServiceError::SessionNotFound(session_id.to_string()))?
        };

        // Verify user owns this session — the plan it carries includes the
        // source platform's env vars, so leaking it cross-user is a real
        // credential exposure, not just a privacy nicety.
        check_session_ownership(&session, user_id, session_id)?;

        // Extract errors and warnings from validation
        let errors: Vec<String> = session
            .validation
            .results
            .iter()
            .filter(|r| !r.passed && r.level == temps_import_types::ValidationLevel::Critical)
            .map(|r| r.message.clone())
            .collect();

        let warnings: Vec<String> = session
            .validation
            .results
            .iter()
            .filter(|r| r.level == temps_import_types::ValidationLevel::Warning)
            .map(|r| r.message.clone())
            .collect();

        Ok(ImportStatusResponse {
            session_id: session_id.to_string(),
            status: ImportExecutionStatus::Pending,
            plan: Some(mask_plan_secrets(&session.plan)),
            validation: Some(session.validation),
            project_id: None,
            environment_id: None,
            deployment_id: None,
            errors,
            warnings,
            created_at: session.created_at,
            updated_at: session.created_at, // Same as created_at since we don't track updates yet
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_import_types::plan::*;
    use temps_import_types::validation::*;
    use temps_import_types::{
        ImportSelector, ImportSource, ImportValidationRule, WorkloadDescriptor, WorkloadId,
        WorkloadImporter, WorkloadSnapshot,
    };

    #[allow(dead_code)]
    fn create_test_db() -> Arc<DatabaseConnection> {
        // For unit tests, we create a mock database
        use sea_orm::{DatabaseBackend, MockDatabase};
        Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection())
    }

    // NOTE: Unit tests for ImportOrchestrator are limited because it now requires
    // fully initialized services (ProjectService, DeploymentService, GitProviderManager).
    // These services have complex dependencies that are difficult to mock in unit tests.
    // Full functionality tests should be done as integration tests with a real database.

    fn create_test_plan() -> temps_import_types::ImportPlan {
        temps_import_types::ImportPlan {
            version: "1.0".to_string(),
            source: "docker".to_string(),
            source_id: "abc123".to_string(),
            project: ProjectConfiguration {
                name: "test-project".to_string(),
                slug: "test-project".to_string(),
                project_type: ProjectType::Docker,
                is_web_app: true,
            },
            environment: EnvironmentConfiguration {
                name: "production".to_string(),
                subdomain: "test-project".to_string(),
                resources: ResourceLimits {
                    cpu_limit: Some(1000),
                    memory_limit: Some(512),
                    cpu_request: Some(500),
                    memory_request: Some(256),
                },
            },
            deployment: DeploymentConfiguration {
                image: "nginx:latest".to_string(),
                build: None,
                strategy: DeploymentStrategy::Replace,
                env_vars: vec![],
                ports: vec![],
                volumes: vec![],
                network: NetworkConfiguration {
                    mode: NetworkMode::Bridge,
                    hostname: None,
                    dns_servers: vec![],
                },
                resources: ResourceLimits {
                    cpu_limit: Some(1000),
                    memory_limit: Some(512),
                    cpu_request: Some(500),
                    memory_request: Some(256),
                },
                command: None,
                entrypoint: None,
                working_dir: None,
                health_check: None,
                git: None,
            },
            services: vec![],
            domains: vec![],
            additional_deployments: vec![],
            steps: vec![],
            summary: temps_import_types::MigrationSummary {
                headline: "Test import".to_string(),
                overall_risk: temps_import_types::RiskLevel::Low,
                resource_counts: temps_import_types::ResourceCounts::default(),
                critical_warnings: vec![],
                manual_actions_required: vec![],
                unsupported_features: vec![],
            },
            metadata: PlanMetadata {
                generated_at: chrono::Utc::now(),
                generator_version: "1.0".to_string(),
                complexity: PlanComplexity::Low,
                warnings: vec![],
            },
            cost_analysis: None,
        }
    }

    fn create_test_validation(passed: bool) -> temps_import_types::ValidationReport {
        temps_import_types::ValidationReport {
            results: vec![],
            overall_status: if passed {
                ValidationStatus::Passed
            } else {
                ValidationStatus::Failed
            },
            summary: ValidationSummary {
                total_count: 0,
                passed_count: 0,
                failed_count: 0,
                error_count: 0,
                info_count: 0,
                warning_count: 0,
                critical_count: 0,
            },
        }
    }

    fn test_session(user_id: i32) -> ImportSession {
        ImportSession {
            session_id: "sess-1".to_string(),
            user_id,
            plan: create_test_plan(),
            validation: create_test_validation(true),
            repository_id: None,
            git_provider_connection_id: None,
            repo_owner: None,
            repo_name: None,
            credentials: temps_import_types::ImportCredentials::default(),
            created_at: chrono::Utc::now(),
        }
    }

    // Pure — no orchestrator/DB/service construction needed, unlike the
    // integration-shaped tests below that require full service wiring.
    #[test]
    fn check_session_ownership_allows_the_owner() {
        let session = test_session(7);
        assert!(check_session_ownership(&session, 7, "sess-1").is_ok());
    }

    #[test]
    fn check_session_ownership_rejects_a_different_user() {
        let session = test_session(7);
        let result = check_session_ownership(&session, 8, "sess-1");
        assert!(
            matches!(result, Err(ImportServiceError::SessionNotFound(id)) if id == "sess-1"),
            "a non-owner must see the same 'not found' error a truly missing \
             session would produce — never a distinct 'forbidden' response \
             that would confirm the session id exists"
        );
    }

    /// Regression test: import sessions hold the real, unmasked source
    /// credentials (a full kubeconfig with its private key, for
    /// Kubernetes) and used to have no eviction at all — a plan nobody ever
    /// executed stayed resident in memory for the life of the process.
    /// `prune_expired_sessions` bounds that to `SESSION_TTL`.
    #[test]
    fn prune_expired_sessions_drops_only_stale_entries() {
        let mut fresh = test_session(1);
        fresh.session_id = "fresh".to_string();
        fresh.created_at = chrono::Utc::now();

        let mut stale = test_session(1);
        stale.session_id = "stale".to_string();
        stale.created_at = chrono::Utc::now() - (SESSION_TTL + chrono::Duration::minutes(1));

        let lock = std::sync::RwLock::new(HashMap::from([
            ("fresh".to_string(), fresh),
            ("stale".to_string(), stale),
        ]));
        {
            let mut sessions = lock.write().unwrap();
            ImportOrchestrator::prune_expired_sessions(&mut sessions);
        }

        let sessions = lock.read().unwrap();
        assert!(
            sessions.contains_key("fresh"),
            "a session within the TTL must survive pruning"
        );
        assert!(
            !sessions.contains_key("stale"),
            "a session older than SESSION_TTL must be evicted, credentials and all"
        );
    }

    /// Regression test: execute_import used to read a session's real,
    /// unmasked credentials once and then leave them sitting in the stored
    /// session for the rest of SESSION_TTL (up to an hour), even though
    /// nothing ever reads them again -- including on a retry after a failed
    /// real run, which keeps the session around for exactly that.
    #[test]
    fn clear_session_credentials_zeroes_credentials_but_keeps_the_rest_of_the_session() {
        let mut session = test_session(7);
        session.credentials = temps_import_types::ImportCredentials {
            token: Some("real-token".to_string()),
            team_id: None,
            base_url: Some("https://coolify.example.com".to_string()),
            extra: HashMap::from([("username".to_string(), "admin".to_string())]),
        };
        let lock = std::sync::RwLock::new(HashMap::from([("sess-1".to_string(), session)]));

        {
            let mut sessions = lock.write().unwrap();
            ImportOrchestrator::clear_session_credentials(&mut sessions, "sess-1");
        }

        let sessions = lock.read().unwrap();
        let stored = sessions.get("sess-1").expect("session must still exist");
        assert!(
            stored.credentials.token.is_none()
                && stored.credentials.base_url.is_none()
                && stored.credentials.extra.is_empty(),
            "credentials must be cleared after use, got {:?}",
            stored.credentials
        );
        assert_eq!(
            stored.user_id, 7,
            "clearing credentials must not disturb the rest of the session \
             (get_status and a retry after failure still need plan/validation/ownership)"
        );
    }

    /// A missing session id must be a safe no-op, not a panic -- the caller
    /// (execute_import) has already handled SessionNotFound by this point,
    /// but a concurrent eviction could still race this call.
    #[test]
    fn clear_session_credentials_is_a_noop_for_a_missing_session() {
        let lock = std::sync::RwLock::new(HashMap::<String, ImportSession>::new());
        let mut sessions = lock.write().unwrap();
        ImportOrchestrator::clear_session_credentials(&mut sessions, "nonexistent");
        assert!(sessions.is_empty());
    }

    #[test]
    fn mask_plan_secrets_redacts_source_url_and_secret_env_vars_only() {
        let mut plan = create_test_plan();
        plan.services.push(ServicePlan {
            name: "lab-db".to_string(),
            service_type: "postgres".to_string(),
            version: Some("16".to_string()),
            parameters: HashMap::from([
                (
                    crate::services::resource_executor::SOURCE_URL_PARAM.to_string(),
                    serde_json::json!("postgres://lab:hunter2@1.2.3.4:5432/labdb"),
                ),
                ("database".to_string(), serde_json::json!("labdb")),
            ]),
            env_var_mappings: HashMap::new(),
            action: ServiceAction::Create,
            action_description: "Create a new managed service".to_string(),
            data_implications: vec![DataImplication {
                severity: DataImplicationSeverity::Warning,
                message: "Database 'lab-db' contains data that will be copied automatically"
                    .to_string(),
                recommended_action: Some(format!(
                    "pg_dump --no-owner --format=custom \"{}\" -f lab-db.dump",
                    "postgres://lab:hunter2@1.2.3.4:5432/labdb"
                )),
            }],
        });
        plan.deployment.env_vars = vec![
            EnvironmentVariable {
                key: "DATABASE_URL".to_string(),
                value: "postgres://lab:hunter2@1.2.3.4:5432/labdb".to_string(),
                is_secret: true,
                source_description: None,
            },
            EnvironmentVariable {
                key: "PUBLIC_APP_NAME".to_string(),
                value: "lab-shop".to_string(),
                is_secret: false,
                source_description: None,
            },
        ];

        let masked = mask_plan_secrets(&plan);

        let masked_param = masked.services[0]
            .parameters
            .get(crate::services::resource_executor::SOURCE_URL_PARAM)
            .unwrap();
        assert_eq!(masked_param, &serde_json::json!(MASKED_SECRET));
        assert_eq!(
            masked.services[0].parameters.get("database").unwrap(),
            &serde_json::json!("labdb"),
            "non-secret parameters must pass through unchanged"
        );
        assert_eq!(masked.deployment.env_vars[0].value, MASKED_SECRET);
        assert_eq!(
            masked.deployment.env_vars[1].value, "lab-shop",
            "non-secret env vars must pass through unchanged"
        );

        let masked_action = masked.services[0].data_implications[0]
            .recommended_action
            .as_ref()
            .unwrap();
        assert!(
            !masked_action.contains("hunter2"),
            "recommended_action must not leak the source DB password: {}",
            masked_action
        );
        assert_eq!(
            masked_action,
            &format!(
                "pg_dump --no-owner --format=custom \"{}\" -f lab-db.dump",
                MASKED_SECRET
            )
        );

        // The original, unmasked plan (what execute_import actually uses)
        // must be untouched — masking must only ever operate on a copy.
        assert_eq!(
            plan.deployment.env_vars[0].value,
            "postgres://lab:hunter2@1.2.3.4:5432/labdb"
        );
        assert!(plan.services[0].data_implications[0]
            .recommended_action
            .as_ref()
            .unwrap()
            .contains("hunter2"));
    }

    // Tests commented out - they require full service initialization which is complex in unit tests.
    // These should be converted to integration tests with proper database and service setup.

    /*
    #[tokio::test]
    async fn test_list_sources_returns_empty_when_no_importers_registered() {
        // Requires full orchestrator with services
    }

    #[tokio::test]
    async fn test_execute_import_returns_error_for_nonexistent_session() {
        // Requires full orchestrator with services
    }

    #[tokio::test]
    async fn test_execute_import_dry_run_returns_completed_status() {
        // Requires full orchestrator with services
    }

    #[tokio::test]
    async fn test_get_status_returns_error_for_nonexistent_session() {
        // Requires full orchestrator with services
    }
    */

    // Basic test that doesn't require full orchestrator - just tests data structures
    #[tokio::test]
    async fn test_create_test_plan_structure() {
        // Arrange & Act
        let plan = create_test_plan();

        // Assert - verify plan structure is correct
        assert_eq!(plan.project.name, "test-project");
        assert_eq!(plan.deployment.image, "nginx:latest");
        assert!(plan.project.is_web_app);
    }

    #[tokio::test]
    async fn test_create_test_validation_passed() {
        // Arrange & Act
        let validation = create_test_validation(true);

        // Assert
        assert_eq!(validation.overall_status, ValidationStatus::Passed);
    }

    #[tokio::test]
    async fn test_create_test_validation_failed() {
        // Arrange & Act
        let validation = create_test_validation(false);

        // Assert
        assert_eq!(validation.overall_status, ValidationStatus::Failed);
    }

    #[tokio::test]
    async fn test_import_context_includes_git_provider_connection_id() {
        // Arrange
        let git_provider_connection_id = Some(42);

        // Act
        let context = temps_import_types::ImportContext {
            session_id: "test-session".to_string(),
            user_id: 1,
            dry_run: false,
            project_name: "test-project".to_string(),
            preset: "nodejs".to_string(),
            directory: ".".to_string(),
            main_branch: "main".to_string(),
            git_provider_connection_id,
            repo_owner: Some("test-owner".to_string()),
            repo_name: Some("test-repo".to_string()),
            credentials: temps_import_types::ImportCredentials::none(),
            metadata: std::collections::HashMap::new(),
        };

        // Assert
        assert_eq!(context.git_provider_connection_id, Some(42));
        assert_eq!(context.repo_owner, Some("test-owner".to_string()));
        assert_eq!(context.repo_name, Some("test-repo".to_string()));
        assert_eq!(context.session_id, "test-session");
        assert_eq!(context.user_id, 1);
        assert_eq!(context.project_name, "test-project");
    }

    #[tokio::test]
    async fn test_import_session_stores_git_provider_connection_id() {
        // Arrange & Act
        let session = ImportSession {
            session_id: "test-session".to_string(),
            user_id: 1,
            plan: create_test_plan(),
            validation: create_test_validation(true),
            repository_id: Some(10),
            git_provider_connection_id: Some(42),
            repo_owner: Some("test-owner".to_string()),
            repo_name: Some("test-repo".to_string()),
            credentials: ImportCredentials::none(),
            created_at: chrono::Utc::now(),
        };

        // Assert
        assert_eq!(session.git_provider_connection_id, Some(42));
        assert_eq!(session.repository_id, Some(10));
        assert_eq!(session.repo_owner, Some("test-owner".to_string()));
        assert_eq!(session.repo_name, Some("test-repo".to_string()));
        assert_eq!(session.user_id, 1);
    }

    // NOTE: The following tests require full orchestrator initialization with real services,
    // which have complex dependencies. They should be rewritten as integration tests.
    /*
    #[tokio::test]
    async fn test_execute_import_delegates_to_importer() {
        // Arrange
        let db = create_test_db();
        let git_provider_manager = Arc::new(create_mock_git_provider_manager());
        let project_service = Arc::new(create_mock_project_service_for_orchestrator());
        let deployment_service = Arc::new(create_mock_deployment_service());

        let mut orchestrator = ImportOrchestrator::new(
            db.clone(),
            git_provider_manager,
            project_service,
            deployment_service,
        );

        // Register a mock importer
        let mock_importer = Arc::new(MockWorkloadImporter::new());
        orchestrator.register_importer(mock_importer.clone());

        // Create a session
        let session_id = "test-execute-session".to_string();
        let plan = create_test_plan();
        let validation = create_test_validation(true);
        let session = ImportSession {
            session_id: session_id.clone(),
            user_id: 1,
            plan: plan.clone(),
            validation,
            created_at: chrono::Utc::now(),
        };

        {
            let mut sessions = orchestrator.sessions.write().unwrap();
            sessions.insert(session_id.clone(), session);
        }

        // Act
        let result = orchestrator.execute_import(
            1,
            session_id.clone(),
            "test-project".to_string(),
            "nodejs".to_string(),
            ".".to_string(),
            "main".to_string(),
            false,
        ).await;

        // Assert
        assert!(result.is_ok(), "Execute should delegate to importer successfully");
        let response = result.unwrap();
        assert_eq!(response.session_id, session_id);
        assert_eq!(response.status, ImportExecutionStatus::Completed);
    }

    #[tokio::test]
    async fn test_execute_import_validates_session_ownership() {
        // Arrange
        let db = create_test_db();
        let git_provider_manager = Arc::new(create_mock_git_provider_manager());
        let project_service = Arc::new(create_mock_project_service_for_orchestrator());
        let deployment_service = Arc::new(create_mock_deployment_service());

        let orchestrator = ImportOrchestrator::new(
            db.clone(),
            git_provider_manager,
            project_service,
            deployment_service,
        );

        // Create a session owned by user 1
        let session_id = "ownership-test-session".to_string();
        let plan = create_test_plan();
        let validation = create_test_validation(true);
        let session = ImportSession {
            session_id: session_id.clone(),
            user_id: 1,
            plan: plan.clone(),
            validation,
            created_at: chrono::Utc::now(),
        };

        {
            let mut sessions = orchestrator.sessions.write().unwrap();
            sessions.insert(session_id.clone(), session);
        }

        // Act - Try to execute as user 2
        let result = orchestrator.execute_import(
            2, // Different user
            session_id.clone(),
            "test-project".to_string(),
            "nodejs".to_string(),
            ".".to_string(),
            "main".to_string(),
            false,
        ).await;

        // Assert
        assert!(result.is_err(), "Should reject session access by wrong user");
        match result {
            Err(ImportServiceError::SessionNotFound(_)) => {
                // Expected error
            }
            _ => panic!("Should return SessionNotFound error"),
        }
    }

    #[tokio::test]
    async fn test_execute_import_rejects_invalid_validation() {
        // Arrange
        let db = create_test_db();
        let git_provider_manager = Arc::new(create_mock_git_provider_manager());
        let project_service = Arc::new(create_mock_project_service_for_orchestrator());
        let deployment_service = Arc::new(create_mock_deployment_service());

        let orchestrator = ImportOrchestrator::new(
            db.clone(),
            git_provider_manager,
            project_service,
            deployment_service,
        );

        // Create a session with failed validation
        let session_id = "invalid-validation-session".to_string();
        let plan = create_test_plan();
        let validation = create_test_validation(false); // Failed validation
        let session = ImportSession {
            session_id: session_id.clone(),
            user_id: 1,
            plan: plan.clone(),
            validation,
            created_at: chrono::Utc::now(),
        };

        {
            let mut sessions = orchestrator.sessions.write().unwrap();
            sessions.insert(session_id.clone(), session);
        }

        // Act
        let result = orchestrator.execute_import(
            1,
            session_id.clone(),
            "test-project".to_string(),
            "nodejs".to_string(),
            ".".to_string(),
            "main".to_string(),
            false,
        ).await;

        // Assert
        assert!(result.is_err(), "Should reject execution with failed validation");
        match result {
            Err(ImportServiceError::ValidationFailed) => {
                // Expected error
            }
            _ => panic!("Should return ValidationFailed error"),
        }
    }

    #[tokio::test]
    async fn test_orchestrator_implements_service_provider_trait() {
        // Arrange
        let db = create_test_db();
        let git_provider_manager = Arc::new(create_mock_git_provider_manager());
        let project_service = Arc::new(create_mock_project_service_for_orchestrator());
        let deployment_service = Arc::new(create_mock_deployment_service());

        let orchestrator = ImportOrchestrator::new(
            db.clone(),
            git_provider_manager.clone(),
            project_service.clone(),
            deployment_service.clone(),
        );

        // Act & Assert - Test that orchestrator implements ImportServiceProvider
        let service_provider: &dyn temps_import_types::ImportServiceProvider = &orchestrator;

        // Verify db access
        let _ = service_provider.db();

        // Verify service access
        let _ = service_provider.project_service();
        let _ = service_provider.deployment_service();
        let _ = service_provider.git_provider_manager();
    }
    */

    // Mock implementations for testing (kept for potential future integration tests)

    /// Mock Git Provider Manager
    #[allow(dead_code)]
    struct MockGitProviderManager;

    #[allow(dead_code)]
    fn create_mock_git_provider_manager() -> MockGitProviderManager {
        MockGitProviderManager
    }

    /// Mock Project Service for orchestrator tests
    #[allow(dead_code)]
    struct MockProjectServiceForOrchestrator;

    #[allow(dead_code)]
    fn create_mock_project_service_for_orchestrator() -> MockProjectServiceForOrchestrator {
        MockProjectServiceForOrchestrator
    }

    /// Mock Deployment Service
    #[allow(dead_code)]
    struct MockDeploymentService;

    #[allow(dead_code)]
    fn create_mock_deployment_service() -> MockDeploymentService {
        MockDeploymentService
    }

    /// Mock Workload Importer for testing
    #[allow(dead_code)]
    struct MockWorkloadImporter {
        source: ImportSource,
    }

    #[allow(dead_code)]
    impl MockWorkloadImporter {
        fn new() -> Self {
            Self {
                source: ImportSource::Docker,
            }
        }
    }

    #[async_trait::async_trait]
    impl WorkloadImporter for MockWorkloadImporter {
        fn source(&self) -> ImportSource {
            self.source
        }

        fn name(&self) -> &str {
            "Mock Importer"
        }

        fn version(&self) -> &str {
            "1.0.0-test"
        }

        async fn health_check(&self) -> temps_import_types::ImportResult<bool> {
            Ok(true)
        }

        async fn discover(
            &self,
            _credentials: &ImportCredentials,
            _selector: ImportSelector,
        ) -> temps_import_types::ImportResult<Vec<WorkloadDescriptor>> {
            Ok(vec![])
        }

        async fn describe(
            &self,
            _credentials: &ImportCredentials,
            _workload_id: &WorkloadId,
        ) -> temps_import_types::ImportResult<WorkloadSnapshot> {
            Err(temps_import_types::ImportError::Internal(
                "Mock importer".to_string(),
            ))
        }

        fn generate_plan(
            &self,
            _snapshot: WorkloadSnapshot,
        ) -> temps_import_types::ImportResult<ImportPlan> {
            Ok(create_test_plan())
        }

        fn validation_rules(&self) -> Vec<Box<dyn ImportValidationRule>> {
            vec![]
        }

        async fn execute(
            &self,
            context: temps_import_types::ImportContext,
            _plan: ImportPlan,
            _services: &dyn temps_import_types::ImportServiceProvider,
        ) -> temps_import_types::ImportResult<temps_import_types::ImportOutcome> {
            // Mock successful execution
            Ok(temps_import_types::ImportOutcome {
                session_id: context.session_id,
                success: true,
                project_id: Some(1),
                environment_id: Some(1),
                deployment_id: Some(1),
                warnings: vec![],
                errors: vec![],
                created_resources: vec![],
                step_results: vec![],
                duration_seconds: 0.1,
            })
        }

        fn capabilities(&self) -> temps_import_types::ImporterCapabilities {
            temps_import_types::ImporterCapabilities::default()
        }
    }

    /*
    #[tokio::test]
    async fn test_get_status_returns_session_details() {
        // Arrange
        let orchestrator = create_test_orchestrator();

        // Create a fake session with validation results
        let session_id = Uuid::new_v4().to_string();
        let validation = temps_import_types::ValidationReport {
            results: vec![
                temps_import_types::ValidationResult {
                    rule_id: "test.warning".to_string(),
                    rule_name: "Test Warning".to_string(),
                    level: temps_import_types::ValidationLevel::Warning,
                    passed: true,
                    message: "This is a warning".to_string(),
                    remediation: None,
                    affected_resources: vec![],
                },
            ],
            overall_status: ValidationStatus::PassedWithWarnings,
            summary: ValidationSummary {
                total_count: 1,
                passed_count: 1,
                failed_count: 0,
                error_count: 0,
                info_count: 0,
                warning_count: 1,
                critical_count: 0,
            },
        };

        let session = ImportSession {
            session_id: session_id.clone(),
            user_id: 1,
            plan: create_test_plan(),
            validation,
            created_at: chrono::Utc::now(),
        };

        {
            let mut sessions = orchestrator.sessions.write().unwrap();
            sessions.insert(session_id.clone(), session.clone());
        }

        // Act
        let result = orchestrator.get_status(1, &session_id).await;

        // Assert
        assert!(result.is_ok(), "Should return status for existing session");
        let status = result.unwrap();
        assert_eq!(status.session_id, session_id);
        assert!(status.plan.is_some(), "Should include plan");
        assert!(status.validation.is_some(), "Should include validation");
        assert_eq!(status.warnings.len(), 1, "Should extract warnings from validation");
        assert_eq!(status.warnings[0], "This is a warning");
    }
    */

    // -----------------------------------------------------------------------
    // Fix #16 — ImportCredentials base_url SSRF validation
    //
    // These tests bypass the full orchestrator construction (which requires
    // real DB + service wiring) and call validate_external_url directly,
    // mirroring the check inserted at the top of discover() and create_plan().
    // -----------------------------------------------------------------------

    #[test]
    fn test_import_base_url_cloud_metadata_rejected() {
        let credentials = ImportCredentials {
            base_url: Some("http://169.254.169.254".to_string()),
            ..Default::default()
        };
        if let Some(ref url) = credentials.base_url {
            let result = validate_external_url(url);
            assert!(
                result.is_err(),
                "cloud metadata base_url must be rejected before reaching importer"
            );
        }
    }

    #[test]
    fn test_import_base_url_localhost_rejected() {
        let credentials = ImportCredentials {
            base_url: Some("http://localhost".to_string()),
            ..Default::default()
        };
        if let Some(ref url) = credentials.base_url {
            let result = validate_external_url(url);
            assert!(result.is_err(), "localhost base_url must be rejected");
        }
    }

    #[test]
    fn test_import_base_url_private_ip_rejected() {
        let credentials = ImportCredentials {
            base_url: Some("http://10.0.0.1".to_string()),
            ..Default::default()
        };
        if let Some(ref url) = credentials.base_url {
            let result = validate_external_url(url);
            assert!(result.is_err(), "private IP base_url must be rejected");
        }
    }

    #[test]
    fn test_import_base_url_public_accepted() {
        let credentials = ImportCredentials {
            base_url: Some("https://coolify.example.com".to_string()),
            ..Default::default()
        };
        if let Some(ref url) = credentials.base_url {
            let result = validate_external_url(url);
            assert!(
                result.is_ok(),
                "public HTTPS base_url must be accepted; got: {:?}",
                result
            );
        }
    }

    #[test]
    fn test_import_base_url_none_is_allowed() {
        let credentials = ImportCredentials {
            base_url: None,
            ..Default::default()
        };
        // No validation should occur when base_url is absent.
        assert!(credentials.base_url.is_none());
    }
}
