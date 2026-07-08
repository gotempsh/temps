use crate::services::workflow_execution_service::WorkflowExecutionService;
use crate::services::workflow_planner::WorkflowPlanner;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serde_json;
use std::collections::HashMap;
use std::sync::Arc;
use temps_core::{Job, JobQueue, JobReceiver};
use temps_database::DbConnection;
use temps_entities::{
    deployments,
    prelude::{DeploymentConfigSnapshot, DeploymentMetadata, GitPushEvent},
    types::PipelineStatus,
};
use tracing::{debug, error, info, warn};

#[derive(Debug)]
pub enum JobProcessorError {
    QueueError(String),
    PipelineError(String),
    DatabaseError(String),
    Other(String),
}

impl std::fmt::Display for JobProcessorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobProcessorError::QueueError(msg) => write!(f, "Queue error: {}", msg),
            JobProcessorError::PipelineError(msg) => write!(f, "Pipeline error: {}", msg),
            JobProcessorError::DatabaseError(msg) => write!(f, "Database error: {}", msg),
            JobProcessorError::Other(msg) => write!(f, "Other error: {}", msg),
        }
    }
}

impl std::error::Error for JobProcessorError {}

/// Commit information fetched from Git provider
#[derive(Debug, Clone)]
struct CommitInfo {
    message: String,
    author: String,
    commit_json: serde_json::Value,
}

/// Shared slot for the optional [`temps_core::DeploymentGate`] — see the
/// `deployment_gate` field doc on [`JobProcessorService`] for why this is
/// a lock instead of a plain `Option`.
pub type DeploymentGateSlot = Arc<tokio::sync::RwLock<Option<Arc<dyn temps_core::DeploymentGate>>>>;

pub struct JobProcessorService {
    db: Arc<DbConnection>,
    job_receiver: Box<dyn JobReceiver>,
    queue: Arc<dyn JobQueue>,
    workflow_planner: Arc<WorkflowPlanner>,
    workflow_executor: Arc<WorkflowExecutionService>,
    git_provider_manager: Arc<temps_git::GitProviderManager>,
    /// Optional gate checked before a deployment transitions to `Running`
    /// (e.g. a plugin implementing manual approvals). Defaults to
    /// `None` — a no-op — so deploys never depend on it. See
    /// [`temps_core::DeploymentGate`].
    ///
    /// Held behind a shared lock rather than a plain `Option` because it
    /// must be settable *after* this service is constructed (and even
    /// after `run()` has been spawned): `DeploymentsPlugin::register_services`
    /// runs, and starts this processor, before later-registered plugins get
    /// a chance to register a gate. `DeploymentsPlugin::initialize_plugin_services`
    /// — which runs only after every plugin has registered — writes into
    /// the same slot via a clone taken before `run()` was spawned. `run()`'s
    /// dispatch loop re-reads it per job, so a gate registered after
    /// startup still protects every job dispatched from that point on.
    deployment_gate: DeploymentGateSlot,
}

impl JobProcessorService {
    pub fn new(
        db: Arc<DbConnection>,
        job_receiver: Box<dyn JobReceiver>,
        queue: Arc<dyn JobQueue>,
        workflow_executor: Arc<WorkflowExecutionService>,
        workflow_planner: Arc<WorkflowPlanner>,
        git_provider_manager: Arc<temps_git::GitProviderManager>,
    ) -> Self {
        Self {
            db,
            job_receiver,
            queue,
            workflow_planner,
            workflow_executor,
            git_provider_manager,
            deployment_gate: Arc::new(tokio::sync::RwLock::new(None)),
        }
    }

    pub fn with_external_service_manager(
        db: Arc<DbConnection>,
        job_receiver: Box<dyn JobReceiver>,
        queue: Arc<dyn JobQueue>,
        workflow_executor: Arc<WorkflowExecutionService>,
        workflow_planner: Arc<WorkflowPlanner>,
        git_provider_manager: Arc<temps_git::GitProviderManager>,
    ) -> Self {
        Self {
            db,
            job_receiver,
            queue,
            workflow_planner,
            workflow_executor,
            git_provider_manager,
            deployment_gate: Arc::new(tokio::sync::RwLock::new(None)),
        }
    }

    /// Returns a clone of the shared gate slot. Call this *before* moving
    /// `self` into the spawned `run()` task, so the caller retains a handle
    /// it can write into later (from `DeploymentsPlugin::initialize_plugin_services`,
    /// which runs after every plugin has registered its services). See the
    /// field doc on `deployment_gate` for why a direct
    /// `set_deployment_gate(&mut self, ...)` setter doesn't work here.
    pub fn deployment_gate_handle(&self) -> DeploymentGateSlot {
        self.deployment_gate.clone()
    }

    pub async fn run(&mut self) -> Result<(), JobProcessorError> {
        debug!("Starting job processor service for deployments");
        debug!("Job processor initialized and ready to receive jobs");

        loop {
            debug!("🎧 Waiting for next job...");
            match self.job_receiver.recv().await {
                Ok(job) => {
                    info!("Processing job: {}", job);
                    debug!(
                        "Job details received at: {}",
                        chrono::Utc::now().to_rfc3339()
                    );

                    match job {
                        Job::GitPushEvent(git_push_job) => {
                            debug!("🔥 Handling GitPushEvent job - owner: {}, repo: {}, branch: {:?}, tag: {:?}, commit: {}",
                                git_push_job.owner, git_push_job.repo, git_push_job.branch, git_push_job.tag, git_push_job.commit);
                            let workflow_planner = Arc::clone(&self.workflow_planner);
                            let workflow_executor = Arc::clone(&self.workflow_executor);
                            let db = Arc::clone(&self.db);
                            let git_provider_manager = Arc::clone(&self.git_provider_manager);
                            let queue = Arc::clone(&self.queue);
                            let deployment_gate = self.deployment_gate.read().await.clone();

                            // Spawn a task to handle the job asynchronously
                            tokio::spawn(async move {
                                debug!("Starting async processing for GitPushEvent job");
                                Self::process_git_push_event_job(
                                    workflow_planner,
                                    workflow_executor,
                                    db,
                                    git_provider_manager,
                                    queue,
                                    deployment_gate,
                                    git_push_job,
                                )
                                .await;
                                debug!("Completed async processing for GitPushEvent job");
                            });
                        }
                        Job::DeployImageRequested(image_job) => {
                            debug!(
                                "🔥 Handling DeployImageRequested job - project: {}, image: {}",
                                image_job.project_id, image_job.image_ref
                            );
                            let workflow_planner = Arc::clone(&self.workflow_planner);
                            let workflow_executor = Arc::clone(&self.workflow_executor);
                            let db = Arc::clone(&self.db);
                            let queue = Arc::clone(&self.queue);
                            let deployment_gate = self.deployment_gate.read().await.clone();

                            tokio::spawn(async move {
                                Self::process_deploy_image_requested_job(
                                    workflow_planner,
                                    workflow_executor,
                                    db,
                                    queue,
                                    deployment_gate,
                                    image_job,
                                )
                                .await;
                            });
                        }
                        Job::DeploymentGateRecheck(recheck_job) => {
                            debug!(
                                "🔥 Handling DeploymentGateRecheck job - deployment: {}",
                                recheck_job.deployment_id
                            );
                            let workflow_executor = Arc::clone(&self.workflow_executor);
                            let db = Arc::clone(&self.db);
                            let deployment_gate = self.deployment_gate.read().await.clone();

                            tokio::spawn(async move {
                                Self::process_deployment_gate_recheck_job(
                                    db,
                                    workflow_executor,
                                    deployment_gate,
                                    recheck_job,
                                )
                                .await;
                            });
                        }
                        _ => {
                            // Ignore jobs that aren't handled by this processor
                            info!("Ignoring unhandled job: {}", job);
                            debug!(
                                "Job type not handled by deployment processor: {}",
                                std::any::type_name_of_val(&job)
                            );
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to receive job: {}", e);
                    debug!("Queue error details: {:?}", e);
                    debug!("Stopping job processor due to queue error");
                    return Err(JobProcessorError::QueueError(e.to_string()));
                }
            }
        }
    }

    /// Process a `DeployImageRequested` job: deploy a prebuilt Docker image to
    /// the project's production environment(s) with **no build step**. Mirrors
    /// the `deploy_from_image` HTTP handler but is project-scoped and
    /// queue-driven — fired by the template one-click flow when a template
    /// carries a prebuilt image. The workflow planner sees `external_image_ref`
    /// in the deployment metadata and plans a pull+run pipeline (no
    /// download_repo / build_image).
    async fn process_deploy_image_requested_job(
        workflow_planner: Arc<WorkflowPlanner>,
        workflow_executor: Arc<WorkflowExecutionService>,
        db: Arc<DbConnection>,
        queue: Arc<dyn JobQueue>,
        deployment_gate: Option<Arc<dyn temps_core::DeploymentGate>>,
        job: temps_core::DeployImageRequestedJob,
    ) {
        use chrono::Utc;
        use sea_orm::PaginatorTrait;

        // Resolve the project.
        let project = match temps_entities::projects::Entity::find_by_id(job.project_id)
            .one(db.as_ref())
            .await
        {
            Ok(Some(p)) => p,
            Ok(None) => {
                error!("DeployImageRequested: project {} not found", job.project_id);
                return;
            }
            Err(e) => {
                error!(
                    "DeployImageRequested: db error loading project {}: {}",
                    job.project_id, e
                );
                return;
            }
        };

        // Target the project's non-preview (production) environment(s). A fresh
        // template project has exactly one.
        let environments = match temps_entities::environments::Entity::find()
            .filter(temps_entities::environments::Column::ProjectId.eq(job.project_id))
            .filter(temps_entities::environments::Column::DeletedAt.is_null())
            .filter(temps_entities::environments::Column::IsPreview.eq(false))
            .all(db.as_ref())
            .await
        {
            Ok(envs) => envs,
            Err(e) => {
                error!(
                    "DeployImageRequested: db error loading environments for project {}: {}",
                    job.project_id, e
                );
                return;
            }
        };

        if environments.is_empty() {
            error!(
                "DeployImageRequested: project {} has no deployable (non-preview) environment",
                job.project_id
            );
            return;
        }

        for environment in environments {
            let deployment_number = deployments::Entity::find()
                .filter(deployments::Column::ProjectId.eq(project.id))
                .count(db.as_ref())
                .await
                .unwrap_or(0)
                + 1;
            let deployment_slug = format!("{}-{}", project.slug, deployment_number);

            let metadata = DeploymentMetadata {
                external_image_ref: Some(job.image_ref.clone()),
                deployment_source_type: Some(temps_entities::source_type::SourceType::DockerImage),
                health_check_path: job.health_check_path.clone(),
                ..Default::default()
            };

            // Snapshot the merged project+env deployment config (port, resources)
            // so the planner/deployer resolve the routed container port.
            let merged_config = if let Some(project_config) = &project.deployment_config {
                if let Some(env_config) = &environment.deployment_config {
                    Some(project_config.merge(env_config))
                } else {
                    Some(project_config.clone())
                }
            } else {
                environment.deployment_config.clone()
            };
            let deployment_config_snapshot = merged_config
                .map(|config| DeploymentConfigSnapshot::from_config(&config, HashMap::new()));

            let new_deployment = deployments::ActiveModel {
                project_id: Set(project.id),
                environment_id: Set(environment.id),
                slug: Set(deployment_slug),
                state: Set("pending".to_string()),
                metadata: Set(Some(metadata)),
                context_vars: Set(Some(serde_json::json!({
                    "trigger": "template_image",
                    "source": "docker_image"
                }))),
                image_name: Set(Some(job.image_ref.clone())),
                deployment_config: Set(deployment_config_snapshot),
                created_at: Set(Utc::now()),
                updated_at: Set(Utc::now()),
                ..Default::default()
            };

            let deployment = match new_deployment.insert(db.as_ref()).await {
                Ok(d) => d,
                Err(e) => {
                    error!(
                        "DeployImageRequested: failed to create deployment for project {} env {}: {}",
                        project.id, environment.id, e
                    );
                    continue;
                }
            };

            info!(
                "Created deployment {} for project {} env {} from DeployImageRequested (image {})",
                deployment.id, project.id, environment.id, job.image_ref
            );

            let deployment_created_event =
                Job::DeploymentCreated(temps_core::DeploymentCreatedJob {
                    deployment_id: deployment.id,
                    project_id: project.id,
                    environment_id: environment.id,
                    environment_name: environment.name.clone(),
                    branch: None,
                    commit_sha: None,
                });
            if let Err(e) = queue.send(deployment_created_event).await {
                error!("Failed to send DeploymentCreated event: {}", e);
            }

            match workflow_planner.create_deployment_jobs(deployment.id).await {
                Ok(created_jobs) => {
                    info!(
                        "Created {} jobs for deployment {} from DeployImageRequested",
                        created_jobs.len(),
                        deployment.id
                    );

                    // Gate check (optional — no-op when no gate is registered),
                    // then transition to Running and execute the workflow.
                    Self::gate_check_then_run(
                        &db,
                        &workflow_executor,
                        &deployment_gate,
                        project.id,
                        &environment.name,
                        deployment.id,
                    )
                    .await;
                }
                Err(e) => {
                    error!(
                        "Failed to plan jobs for image deployment {}: {}",
                        deployment.id, e
                    );
                    if let Err(e2) = JobProcessorService::update_deployment_status_with_message(
                        &db,
                        deployment.id,
                        PipelineStatus::Failed,
                        Some(format!("Failed to plan image deployment: {}", e)),
                    )
                    .await
                    {
                        error!(
                            "Failed to mark image deployment {} failed: {}",
                            deployment.id, e2
                        );
                    }
                }
            }
        }
    }

    /// Fetch commit information from Git provider
    /// Returns CommitInfo struct with all commit details
    /// Falls back to generic message if commit info cannot be fetched
    async fn fetch_commit_info(
        git_provider_manager: &temps_git::GitProviderManager,
        project: &temps_entities::projects::Model,
        job: &temps_core::GitPushEventJob,
    ) -> Result<CommitInfo, anyhow::Error> {
        // Check if project has a git connection
        let connection_id = project
            .git_provider_connection_id
            .ok_or_else(|| anyhow::anyhow!("Project {} has no git connection", project.id))?;

        // Get repository API for cleaner operations
        let repo_api = git_provider_manager
            .get_repository_api(connection_id, &job.owner, &job.repo)
            .await?;

        // Fetch commit info using the repository API
        let commit = repo_api.get_commit_info(&job.commit).await?;

        // Create commit JSON
        let commit_json = serde_json::json!({
            "sha": commit.sha,
            "message": commit.message,
            "author": commit.author,
            "author_email": commit.author_email,
            "date": commit.date.to_rfc3339(),
        });

        Ok(CommitInfo {
            message: commit.message,
            author: commit.author,
            commit_json,
        })
    }

    pub async fn update_deployment_status(
        db: &DbConnection,
        deployment_id: i32,
        status: PipelineStatus,
    ) -> Result<(), JobProcessorError> {
        Self::update_deployment_status_with_message(db, deployment_id, status, None).await
    }

    pub async fn update_deployment_status_with_message(
        db: &DbConnection,
        deployment_id: i32,
        status: PipelineStatus,
        message: Option<String>,
    ) -> Result<(), JobProcessorError> {
        let deployment = deployments::Entity::find_by_id(deployment_id)
            .one(db)
            .await
            .map_err(|e| JobProcessorError::DatabaseError(e.to_string()))?
            .ok_or_else(|| {
                JobProcessorError::DatabaseError(format!("Deployment {} not found", deployment_id))
            })?;

        let mut deployment_active: deployments::ActiveModel = deployment.into();
        deployment_active.updated_at = Set(chrono::Utc::now());

        // Update state string field to match status enum
        let state_str = match status {
            PipelineStatus::Pending => "pending",
            PipelineStatus::Running => "running",
            PipelineStatus::Built => "built",
            PipelineStatus::Completed => "completed",
            PipelineStatus::Failed => "failed",
            PipelineStatus::Cancelled => "cancelled",
        };
        deployment_active.state = Set(state_str.to_string());

        // Set the error/cancellation message if provided
        if let Some(msg) = message {
            deployment_active.cancelled_reason = Set(Some(msg));
        }

        // Set started_at if running
        if status == PipelineStatus::Running {
            deployment_active.started_at = Set(Some(chrono::Utc::now()));
        }

        // Set finished_at if completed/failed/cancelled
        if matches!(
            status,
            PipelineStatus::Completed | PipelineStatus::Failed | PipelineStatus::Cancelled
        ) {
            deployment_active.finished_at = Set(Some(chrono::Utc::now()));
        }

        deployment_active
            .update(db)
            .await
            .map_err(|e| JobProcessorError::DatabaseError(e.to_string()))?;

        Ok(())
    }

    /// Check the optional [`temps_core::DeploymentGate`] and, if allowed,
    /// transition the deployment to `Running` and execute its workflow.
    ///
    /// If a gate blocks the deployment (or errors — fail-closed), this
    /// leaves the deployment in whatever status it already had. The jobs
    /// `create_deployment_jobs` already created are untouched, so a later
    /// [`temps_core::DeploymentGateRecheckJob`] can call this same helper
    /// again once conditions change, without recreating anything.
    ///
    /// `pub` — also called directly by the manual-deploy HTTP handlers
    /// (`handlers::remote_deployments::{deploy_from_image,
    /// deploy_from_image_upload, deploy_from_static}`) and by embedding
    /// binaries that create deployments in-process (e.g. vibetemps' Ship
    /// It), all of which run outside the job-queue dispatch loop and would
    /// otherwise skip the gate entirely.
    pub async fn gate_check_then_run(
        db: &Arc<DbConnection>,
        workflow_executor: &Arc<WorkflowExecutionService>,
        deployment_gate: &Option<Arc<dyn temps_core::DeploymentGate>>,
        project_id: i32,
        environment_name: &str,
        deployment_id: i32,
    ) {
        if let Some(gate) = deployment_gate {
            match gate
                .check(project_id, environment_name, &deployment_id.to_string())
                .await
            {
                Ok(temps_core::GateDecision::Allow) => {}
                Ok(temps_core::GateDecision::Block { reason }) => {
                    info!(
                        "Deployment {} blocked pending an external gate: {}",
                        deployment_id, reason
                    );
                    return;
                }
                Err(e) => {
                    // Fail-closed: a broken gate must never fail open.
                    error!(
                        "Deployment gate check errored for deployment {} — blocking (fail-closed): {}",
                        deployment_id, e
                    );
                    return;
                }
            }
        }

        if let Err(e) = JobProcessorService::update_deployment_status(
            db,
            deployment_id,
            PipelineStatus::Running,
        )
        .await
        {
            error!(
                "Failed to update deployment {} status to Running: {}",
                deployment_id, e
            );
            return;
        }
        info!("Updated deployment {} status to Running", deployment_id);

        info!("Executing workflow for deployment {}", deployment_id);
        if let Err(e) = workflow_executor
            .execute_deployment_workflow(deployment_id)
            .await
        {
            let error_message = format!("{}", e);
            error!(
                "Workflow execution failed for deployment {}: {}",
                deployment_id, error_message
            );
            if let Err(update_err) = JobProcessorService::update_deployment_status_with_message(
                db,
                deployment_id,
                PipelineStatus::Failed,
                Some(error_message),
            )
            .await
            {
                error!("Failed to update deployment status: {}", update_err);
            }
        } else {
            info!(
                "Workflow execution completed for deployment {}",
                deployment_id
            );
        }
    }

    /// Handle a [`temps_core::DeploymentGateRecheckJob`] — re-evaluate the
    /// gate for a deployment that a previous check blocked. Looks the
    /// deployment up to recover its project id and environment name (the
    /// recheck job carries only the deployment id, deliberately —
    /// gate-agnostic, no plugin-specific fields).
    ///
    /// No-ops (with a log line) if the deployment is no longer `Pending`
    /// (e.g. it was cancelled while waiting, or a race already processed
    /// it) — recheck jobs must never resurrect a deployment that moved on.
    async fn process_deployment_gate_recheck_job(
        db: Arc<DbConnection>,
        workflow_executor: Arc<WorkflowExecutionService>,
        deployment_gate: Option<Arc<dyn temps_core::DeploymentGate>>,
        job: temps_core::DeploymentGateRecheckJob,
    ) {
        let deployment = match deployments::Entity::find_by_id(job.deployment_id)
            .one(db.as_ref())
            .await
        {
            Ok(Some(d)) => d,
            Ok(None) => {
                error!(
                    "DeploymentGateRecheck: deployment {} not found",
                    job.deployment_id
                );
                return;
            }
            Err(e) => {
                error!(
                    "DeploymentGateRecheck: db error loading deployment {}: {}",
                    job.deployment_id, e
                );
                return;
            }
        };

        if deployment.state != "pending" {
            info!(
                "DeploymentGateRecheck: deployment {} is no longer pending (state={}), ignoring",
                deployment.id, deployment.state
            );
            return;
        }

        let environment =
            match temps_entities::environments::Entity::find_by_id(deployment.environment_id)
                .one(db.as_ref())
                .await
            {
                Ok(Some(e)) => e,
                Ok(None) => {
                    error!(
                        "DeploymentGateRecheck: environment {} for deployment {} not found",
                        deployment.environment_id, deployment.id
                    );
                    return;
                }
                Err(e) => {
                    error!(
                    "DeploymentGateRecheck: db error loading environment {} for deployment {}: {}",
                    deployment.environment_id, deployment.id, e
                );
                    return;
                }
            };

        Self::gate_check_then_run(
            &db,
            &workflow_executor,
            &deployment_gate,
            deployment.project_id,
            &environment.name,
            deployment.id,
        )
        .await;
    }

    async fn process_git_push_event_job(
        workflow_planner: Arc<WorkflowPlanner>,
        workflow_executor: Arc<WorkflowExecutionService>,
        db: Arc<DbConnection>,
        git_provider_manager: Arc<temps_git::GitProviderManager>,
        queue: Arc<dyn JobQueue>,
        deployment_gate: Option<Arc<dyn temps_core::DeploymentGate>>,
        job: temps_core::GitPushEventJob,
    ) {
        process_git_push_event(
            workflow_planner,
            workflow_executor,
            db,
            git_provider_manager,
            queue,
            deployment_gate,
            job,
        )
        .await;
    }
}

/// Resolve which environment(s) a `GitPushEvent` should deploy to.
///
/// When the job carries an explicit `target_environment_id` (a manual trigger
/// that named a specific environment — e.g. the AI's `trigger_project_pipeline`
/// with `environment_id`, or the "Deploy to this environment" action), deploy to
/// exactly that environment and SKIP branch → environment matching. This is what
/// makes "redeploy to production" actually target production instead of falling
/// through to whichever environment happens to track (or not track) the branch.
///
/// Otherwise fall back to [`find_environments_for_branch`] — the webhook-push
/// path that infers the target(s) from the branch.
async fn resolve_target_environments(
    db: Arc<DbConnection>,
    project: &temps_entities::projects::Model,
    job: &temps_core::GitPushEventJob,
) -> Result<Vec<temps_entities::environments::Model>, String> {
    use temps_entities::environments;

    if let Some(target_id) = job.target_environment_id {
        let env = environments::Entity::find_by_id(target_id)
            .filter(environments::Column::ProjectId.eq(project.id))
            .filter(environments::Column::DeletedAt.is_null())
            .one(db.as_ref())
            .await
            .map_err(|e| format!("Database error finding target environment {target_id}: {e}"))?
            .ok_or_else(|| {
                format!(
                    "Target environment {} not found or does not belong to project {}",
                    target_id, project.id
                )
            })?;
        info!(
            "Manual trigger targets environment {} ({}) directly — bypassing branch matching",
            env.id, env.name
        );
        return Ok(vec![env]);
    }

    find_environments_for_branch(db, project, job.branch.as_deref()).await
}

/// Find all environments to deploy to for a given branch push.
/// Returns every non-preview, non-protected environment tracking this branch
/// so each can independently apply its own `automatic_deploy` policy.
/// When no named environments match and preview environments are enabled,
/// creates/finds a per-branch preview environment and returns it as the sole
/// entry. Returns an empty Vec only when there are no matches and preview
/// environments are disabled.
async fn find_environments_for_branch(
    db: Arc<DbConnection>,
    project: &temps_entities::projects::Model,
    branch: Option<&str>,
) -> Result<Vec<temps_entities::environments::Model>, String> {
    use temps_entities::environments;

    // No branch → use the first environment (tag push, manual trigger without branch)
    let Some(branch_name) = branch else {
        let env = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project.id))
            .filter(environments::Column::DeletedAt.is_null())
            .one(db.as_ref())
            .await
            .map_err(|e| format!("Database error finding environment: {}", e))?
            .ok_or_else(|| "No environment found for project".to_string())?;
        return Ok(vec![env]);
    };

    info!(
        "Looking for environments matching branch '{}' for project {}",
        branch_name, project.id
    );

    // Find ALL non-preview, non-protected environments tracking this branch.
    // Protected environments receive only promoted deployments, never push events.
    // Each returned environment then applies its own automatic_deploy policy.
    let matched_envs = environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(project.id))
        .filter(environments::Column::Branch.eq(branch_name))
        .filter(environments::Column::IsPreview.eq(false))
        .filter(environments::Column::Protected.eq(false))
        .filter(environments::Column::DeletedAt.is_null())
        .all(db.as_ref())
        .await
        .map_err(|e| format!("Database error finding branch environments: {}", e))?;

    if !matched_envs.is_empty() {
        info!(
            "Found {} environment(s) matching branch '{}'",
            matched_envs.len(),
            branch_name
        );
        return Ok(matched_envs);
    }

    info!(
        "No environment matches branch '{}', checking preview environments",
        branch_name
    );

    // Check if preview environments are enabled for this project
    if project.enable_preview_environments {
        info!(
            "Preview environments enabled for project {}, creating/finding per-branch preview",
            project.id
        );

        // Slugify the branch name for use in environment name
        let slugified_branch = temps_core::slugify_branch_name(branch_name);

        // Try to find existing preview environment for this branch
        if let Some(existing_preview) = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project.id))
            .filter(environments::Column::IsPreview.eq(true))
            .filter(environments::Column::Branch.eq(branch_name))
            .filter(environments::Column::DeletedAt.is_null())
            .one(db.as_ref())
            .await
            .map_err(|e| format!("Database error finding preview environment: {}", e))?
        {
            info!(
                "Found existing preview environment '{}' for branch '{}'",
                existing_preview.name, branch_name
            );
            return Ok(vec![existing_preview]);
        }

        // Check if a soft-deleted preview environment exists for this branch — restore it
        if let Some(deleted_preview) = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project.id))
            .filter(environments::Column::Branch.eq(branch_name))
            .filter(environments::Column::DeletedAt.is_not_null())
            .one(db.as_ref())
            .await
            .map_err(|e| format!("Database error finding deleted preview environment: {}", e))?
        {
            info!(
                "Restoring soft-deleted preview environment {} for branch '{}'",
                deleted_preview.id, branch_name
            );
            let mut active_env: environments::ActiveModel = deleted_preview.into();
            active_env.deleted_at = Set(None);
            active_env.updated_at = Set(chrono::Utc::now());
            active_env.current_deployment_id = Set(None);
            let restored = active_env
                .update(db.as_ref())
                .await
                .map_err(|e| format!("Failed to restore preview environment: {}", e))?;
            return Ok(vec![restored]);
        }

        // Create new preview environment for this branch
        return create_preview_environment(db, project, branch_name, &slugified_branch)
            .await
            .map(|env| vec![env]);
    }

    // Preview environments not enabled, try to find generic preview environment (legacy behavior)
    info!(
        "Preview environments not enabled for project {}, looking for generic preview environment",
        project.id
    );

    if let Some(preview_env) = environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(project.id))
        .filter(environments::Column::Name.eq("preview"))
        .filter(environments::Column::DeletedAt.is_null())
        .one(db.as_ref())
        .await
        .map_err(|e| format!("Database error finding preview environment: {}", e))?
    {
        info!(
            "Using existing generic preview environment for branch '{}'",
            branch_name
        );
        return Ok(vec![preview_env]);
    }

    // No preview environment exists, create generic one (legacy behavior)
    info!(
        "Creating generic preview environment for project {}",
        project.id
    );

    use chrono::Utc;
    use temps_entities::upstream_config::UpstreamList;

    let preview_env = environments::ActiveModel {
        name: Set("preview".to_string()),
        slug: Set("preview".to_string()),
        subdomain: Set(format!("{}-preview", project.slug)),
        host: Set(String::new()),
        branch: Set(None), // No specific branch - matches all unmatched branches
        project_id: Set(project.id),
        upstreams: Set(UpstreamList::default()),
        deployment_config: Set(None), // Inherits from project
        current_deployment_id: Set(None),
        last_deployment: Set(None),
        is_preview: Set(false), // Legacy generic preview, not a per-branch preview
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        deleted_at: Set(None),
        ..Default::default()
    };

    let created_env = preview_env
        .insert(db.as_ref())
        .await
        .map_err(|e| format!("Failed to create preview environment: {}", e))?;

    info!(
        "Created generic preview environment '{}' for project {}",
        created_env.name, project.id
    );

    Ok(vec![created_env])
}

/// Create a new preview environment for a specific branch
async fn create_preview_environment(
    db: Arc<DbConnection>,
    project: &temps_entities::projects::Model,
    branch_name: &str,
    slugified_branch: &str,
) -> Result<temps_entities::environments::Model, String> {
    use chrono::Utc;
    use temps_entities::{
        deployment_config::DeploymentConfig, environments, upstream_config::UpstreamList,
    };

    info!(
        "Creating preview environment '{}' for branch '{}' in project {}",
        slugified_branch, branch_name, project.id
    );

    // When the project opts in to on-demand previews, seed the environment's
    // deployment_config with on_demand=true plus the project's idle/wake
    // timeouts so the preview scales to zero instead of running 24/7.
    // Other knobs (cpu, memory, replicas, security) stay None so the
    // inheritance chain (env → project → global defaults) still applies.
    let preview_deployment_config = if project.preview_envs_on_demand {
        Some(DeploymentConfig {
            on_demand: true,
            idle_timeout_seconds: project.preview_envs_idle_timeout_seconds,
            wake_timeout_seconds: project.preview_envs_wake_timeout_seconds,
            ..DeploymentConfig::default()
        })
    } else {
        None
    };

    let preview_env = environments::ActiveModel {
        name: Set(slugified_branch.to_string()),
        slug: Set(slugified_branch.to_string()),
        subdomain: Set(format!("{}-{}", project.slug, slugified_branch)),
        host: Set(String::new()),
        branch: Set(Some(branch_name.to_string())), // Link to specific branch (used for both deployment and tracking)
        project_id: Set(project.id),
        upstreams: Set(UpstreamList::default()),
        deployment_config: Set(preview_deployment_config),
        current_deployment_id: Set(None),
        last_deployment: Set(None),
        is_preview: Set(true),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        deleted_at: Set(None),
        ..Default::default()
    };

    let created_env = preview_env
        .insert(db.as_ref())
        .await
        .map_err(|e| format!("Failed to create preview environment: {}", e))?;

    info!(
        "Created preview environment '{}' (ID: {}) for branch '{}'",
        created_env.name, created_env.id, branch_name
    );

    // Copy environment variables marked for preview to the new preview environment
    info!(
        "Copying environment variables marked for preview to preview environment {}",
        created_env.id
    );

    if let Err(e) =
        copy_environment_variables_to_preview(db.clone(), created_env.id, project.id).await
    {
        error!(
            "Failed to copy environment variables to preview environment {}: {}",
            created_env.id, e
        );
        // Don't fail the preview environment creation, just log the error
    } else {
        info!(
            "Successfully copied environment variables to preview environment {}",
            created_env.id
        );
    }

    Ok(created_env)
}

/// Copy project environment variables marked for preview to a preview environment
/// Creates junction table entries linking env vars with include_in_preview=true to the new environment
async fn copy_environment_variables_to_preview(
    db: Arc<DbConnection>,
    preview_environment_id: i32,
    project_id: i32,
) -> Result<(), String> {
    use temps_entities::{env_var_environments, env_vars};

    // Find all environment variables for this project that are marked to include in preview
    let preview_env_vars = env_vars::Entity::find()
        .filter(env_vars::Column::ProjectId.eq(project_id))
        .filter(env_vars::Column::IncludeInPreview.eq(true))
        .all(db.as_ref())
        .await
        .map_err(|e| format!("Failed to query project environment variables: {}", e))?;

    if preview_env_vars.is_empty() {
        info!(
            "No environment variables marked for preview found in project {}",
            project_id
        );
        return Ok(());
    }

    info!(
        "Found {} environment variable(s) marked for preview in project {}",
        preview_env_vars.len(),
        project_id
    );

    // Create new env_var_environments entries for the preview environment
    let mut created_count = 0;
    let total_count = preview_env_vars.len();
    for env_var in preview_env_vars {
        let new_env_var_env = env_var_environments::ActiveModel {
            env_var_id: Set(env_var.id),
            environment_id: Set(preview_environment_id),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        };

        match new_env_var_env.insert(db.as_ref()).await {
            Ok(_) => {
                created_count += 1;
                debug!(
                    "Linked env var '{}' to preview environment {}",
                    env_var.key, preview_environment_id
                );
            }
            Err(e) => {
                error!(
                    "Failed to link env var '{}' to preview environment {}: {}",
                    env_var.key, preview_environment_id, e
                );
                // Continue copying other variables even if one fails
            }
        }
    }

    info!(
        "Successfully linked {}/{} environment variable(s) to preview environment {}",
        created_count, total_count, preview_environment_id
    );

    Ok(())
}

/// Returns true if a git push should auto-deploy given the project and
/// environment deployment configs. Environment config wins when present
/// (env can explicitly opt out even if project has auto-deploy on).
/// When both configs are absent the answer is false — auto-deploy is opt-in.
fn is_automatic_deploy_enabled(
    project_config: Option<&temps_entities::deployment_config::DeploymentConfig>,
    environment_config: Option<&temps_entities::deployment_config::DeploymentConfig>,
) -> bool {
    // Environment-level explicit value takes precedence; fall back to project then false.
    let effective = match (project_config, environment_config) {
        (_, Some(env_cfg)) => env_cfg
            .automatic_deploy
            .or_else(|| project_config.and_then(|p| p.automatic_deploy)),
        (Some(project_cfg), None) => project_cfg.automatic_deploy,
        (None, None) => None,
    };
    effective.unwrap_or(false)
}

// Extracted free function for testing
async fn process_git_push_event(
    workflow_planner: Arc<WorkflowPlanner>,
    workflow_executor: Arc<WorkflowExecutionService>,
    db: Arc<DbConnection>,
    git_provider_manager: Arc<temps_git::GitProviderManager>,
    queue: Arc<dyn JobQueue>,
    deployment_gate: Option<Arc<dyn temps_core::DeploymentGate>>,
    job: temps_core::GitPushEventJob,
) {
    info!(
        "🔥 Processing GitPushEvent job for owner: {}, repo: {}, branch: {:?}",
        job.owner, job.repo, job.branch
    );
    debug!(
        "GitPushEvent details - owner: {}, repo: {}, branch: {:?}, tag: {:?}, commit: {}",
        job.owner, job.repo, job.branch, job.tag, job.commit
    );

    // Find the project matching this git repository
    let project = match temps_entities::projects::Entity::find()
        .filter(temps_entities::projects::Column::Id.eq(job.project_id))
        .one(db.as_ref())
        .await
    {
        Ok(Some(project)) => project,
        Ok(None) => {
            warn!("No project found for repository {}/{}", job.owner, job.repo);
            return;
        }
        Err(e) => {
            error!(
                "Database error while finding project for {}/{}: {}",
                job.owner, job.repo, e
            );
            return;
        }
    };

    // Resolve the deploy target(s). A manual trigger that names an explicit
    // environment (`target_environment_id`) deploys to exactly that environment,
    // bypassing branch → environment matching. Otherwise (webhook push, or a
    // manual trigger with no explicit target) find every environment tracking
    // this branch; each is deployed independently per its own automatic_deploy
    // policy (env-wins semantics).
    let environments = match resolve_target_environments(db.clone(), &project, &job).await {
        Ok(envs) => envs,
        Err(e) => {
            error!(
                "Failed to resolve target environments for project {}: {}",
                project.id, e
            );
            return;
        }
    };

    if environments.is_empty() {
        info!(
            "No environments found for branch {:?} in project {}",
            job.branch, project.id
        );
        return;
    }

    use chrono::Utc;
    use sea_orm::{EntityTrait, PaginatorTrait, QueryOrder};

    // Fetch commit info once — it's the same for every environment receiving this push.
    let commit_info =
        match JobProcessorService::fetch_commit_info(&git_provider_manager, &project, &job).await {
            Ok(info) => {
                info!("Fetched commit info: {} by {}", info.message, info.author);
                Some(info)
            }
            Err(e) => {
                warn!("Failed to fetch commit info: {}, using fallback", e);
                None
            }
        };

    // Update project's last_deployment timestamp once for this push event.
    let mut active_project: temps_entities::projects::ActiveModel = project.clone().into();
    active_project.last_deployment = sea_orm::Set(Some(Utc::now()));
    if let Err(e) = active_project.update(db.as_ref()).await {
        error!(
            "Failed to update last_deployment for project {}: {}",
            project.id, e
        );
    }

    // Deploy to each environment that opts in. Each environment applies its own
    // automatic_deploy policy so the user can have one env auto-deploy on push
    // and another deploy on demand — even when both track the same branch.
    for environment in environments {
        // ── Auto-deploy gate ───────────────────────────────────────────────
        //
        // Env-wins semantics: if the environment has an explicit automatic_deploy
        // value it takes precedence over the project setting. When both are absent
        // the answer is false (opt-in, not opt-out). Manual triggers bypass this
        // gate entirely — the user clicked deploy, so they unambiguously want one.
        //
        // Exception: the FIRST deployment for an environment always runs even when
        // automatic_deploy=false, so a freshly-created opt-out env still boots.
        let auto_deploy_enabled = is_automatic_deploy_enabled(
            project.deployment_config.as_ref(),
            environment.deployment_config.as_ref(),
        );
        if !auto_deploy_enabled && !job.manual_trigger {
            let existing_count = match deployments::Entity::find()
                .filter(deployments::Column::EnvironmentId.eq(environment.id))
                .count(db.as_ref())
                .await
            {
                Ok(n) => n,
                Err(e) => {
                    error!(
                        "Failed to count existing deployments for environment {}: {}",
                        environment.id, e
                    );
                    continue;
                }
            };

            if existing_count > 0 {
                info!(
                    "Skipping push event for project {} environment {} ({}): automatic_deploy is disabled",
                    project.id, environment.id, environment.name
                );
                continue;
            }

            info!(
                "Allowing initial deployment for project {} environment {} ({}) despite automatic_deploy=false (no prior deployments)",
                project.id, environment.id, environment.name
            );
        } else if job.manual_trigger && !auto_deploy_enabled {
            info!(
                "Manual trigger for project {} environment {} ({}) — bypassing automatic_deploy=false",
                project.id, environment.id, environment.name
            );
        }

        // Check for duplicate deployment (same project, environment, and commit).
        let existing_deployment = deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project.id))
            .filter(deployments::Column::EnvironmentId.eq(environment.id))
            .filter(deployments::Column::CommitSha.eq(&job.commit))
            .filter(deployments::Column::State.is_in(vec![
                "pending",
                "running",
                "deploying",
                "ready",
            ]))
            .order_by_desc(deployments::Column::CreatedAt)
            .one(db.as_ref())
            .await;

        if let Ok(Some(existing)) = existing_deployment {
            info!(
                "Deployment already exists for project {} environment {} commit {} (deployment #{}, state: {}). Skipping duplicate.",
                project.id, environment.id, job.commit, existing.id, existing.state
            );
            continue;
        }

        // Cancel-on-supersede: newest push always wins.
        cancel_in_flight_deployments(&db, &queue, project.id, environment.id).await;

        // Get the next deployment number for this project.
        let deployment_count = match deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project.id))
            .paginate(db.as_ref(), 1)
            .num_items()
            .await
        {
            Ok(count) => count,
            Err(e) => {
                error!(
                    "Failed to count deployments for project {}: {}",
                    project.id, e
                );
                continue;
            }
        };
        let deployment_number = deployment_count + 1;

        let env_slug = if environment.is_preview {
            let sanitized_branch = job
                .branch
                .as_ref()
                .map(|b| b.replace(['/', '_', '.'], "-").to_lowercase())
                .unwrap_or_else(|| "unknown".to_string());
            format!(
                "{}-{}-{}",
                project.slug, sanitized_branch, deployment_number
            )
        } else {
            format!("{}-{}", project.slug, deployment_number)
        };

        let merged_config = if let Some(project_config) = &project.deployment_config {
            if let Some(env_config) = &environment.deployment_config {
                Some(project_config.merge(env_config))
            } else {
                Some(project_config.clone())
            }
        } else {
            environment.deployment_config.clone()
        };

        let deployment_config_snapshot = merged_config
            .map(|config| DeploymentConfigSnapshot::from_config(&config, HashMap::new()));

        // A rebuild-from-source rollback rides the same git-push pipeline but
        // must be recorded as a rollback so the UI/history reflect it.
        let is_rollback = job.rollback_from_deployment_id.is_some();
        let deployment_metadata = DeploymentMetadata {
            git_push_event: Some(GitPushEvent {
                owner: job.owner.clone(),
                repo: job.repo.clone(),
                branch: job.branch.clone().unwrap_or_default(),
                commit: job.commit.clone(),
            }),
            is_rollback,
            rolled_back_from_id: job.rollback_from_deployment_id,
            ..Default::default()
        };
        let trigger_context = if is_rollback {
            serde_json::json!({
                "trigger": "rollback",
                "source": "rebuild_from_source",
                "source_deployment_id": job.rollback_from_deployment_id,
            })
        } else {
            serde_json::json!({
                "trigger": "git_push",
                "source": "webhook"
            })
        };

        let new_deployment = deployments::ActiveModel {
            id: sea_orm::NotSet,
            project_id: sea_orm::Set(project.id),
            environment_id: sea_orm::Set(environment.id),
            slug: sea_orm::Set(env_slug),
            state: sea_orm::Set("pending".to_string()),
            metadata: sea_orm::Set(Some(deployment_metadata)),
            branch_ref: sea_orm::Set(job.branch.clone()),
            tag_ref: sea_orm::Set(job.tag.clone()),
            commit_sha: sea_orm::Set(Some(job.commit.clone())),
            commit_message: sea_orm::Set(commit_info.as_ref().map(|c| c.message.clone())),
            commit_author: sea_orm::Set(commit_info.as_ref().map(|c| c.author.clone())),
            promoted_from_deployment_id: sea_orm::Set(None),
            started_at: sea_orm::Set(None),
            finished_at: sea_orm::Set(None),
            context_vars: sea_orm::Set(Some(trigger_context)),
            deploying_at: sea_orm::Set(None),
            ready_at: sea_orm::Set(None),
            static_dir_location: sea_orm::Set(None),
            screenshot_location: sea_orm::Set(None),
            image_name: sea_orm::Set(None),
            cancelled_reason: sea_orm::Set(None),
            commit_json: sea_orm::Set(commit_info.as_ref().map(|c| c.commit_json.clone())),
            deployment_config: sea_orm::Set(deployment_config_snapshot),
            created_at: sea_orm::Set(Utc::now()),
            updated_at: sea_orm::Set(Utc::now()),
        };

        let deployment = match new_deployment.insert(db.as_ref()).await {
            Ok(deployment) => deployment,
            Err(e) => {
                error!(
                    "Failed to create deployment for project {} environment {}: {}",
                    project.id, environment.id, e
                );
                continue;
            }
        };

        info!(
            "Created deployment {} for project {} environment {} from GitPushEvent",
            deployment.id, project.id, environment.id
        );

        let deployment_created_event = Job::DeploymentCreated(temps_core::DeploymentCreatedJob {
            deployment_id: deployment.id,
            project_id: project.id,
            environment_id: environment.id,
            environment_name: environment.name.clone(),
            branch: job.branch.clone(),
            commit_sha: Some(job.commit.clone()),
        });
        if let Err(e) = queue.send(deployment_created_event).await {
            error!("Failed to send DeploymentCreated event: {}", e);
        }

        let create_jobs_result = workflow_planner.create_deployment_jobs(deployment.id).await;
        let deployment_id = deployment.id;

        match create_jobs_result {
            Ok(created_jobs) => {
                info!(
                    "Created {} jobs for deployment {} from GitPushEvent",
                    created_jobs.len(),
                    deployment_id
                );

                // Gate check (optional — no-op when no gate is registered),
                // then transition to Running and execute the workflow.
                JobProcessorService::gate_check_then_run(
                    &db,
                    &workflow_executor,
                    &deployment_gate,
                    project.id,
                    &environment.name,
                    deployment_id,
                )
                .await;
            }
            Err(job_error) => {
                let error_message = format!("{}", job_error);
                std::mem::drop(job_error);
                error!(
                    "Failed to create jobs for deployment {}: {}",
                    deployment_id, error_message
                );
                if let Err(update_err) = JobProcessorService::update_deployment_status_with_message(
                    &db,
                    deployment_id,
                    PipelineStatus::Failed,
                    Some(error_message),
                )
                .await
                {
                    error!("Failed to update deployment status: {}", update_err);
                }
            }
        }
    } // end for environment in environments
}

/// Cancel all in-flight deployments for the given environment.
///
/// This implements "cancel-on-supersede": when a new deployment is triggered,
/// any currently pending/running deployments for the same environment are
/// cancelled so the newest push always wins. Cancellation is cooperative —
/// the workflow executor checks `DatabaseCancellationProvider::is_cancelled()`
/// between job batches and stops.
async fn cancel_in_flight_deployments(
    db: &DbConnection,
    queue: &Arc<dyn JobQueue>,
    project_id: i32,
    environment_id: i32,
) {
    use temps_entities::deployment_jobs;
    use temps_entities::types::JobStatus;

    let in_flight = match deployments::Entity::find()
        .filter(deployments::Column::EnvironmentId.eq(environment_id))
        .filter(deployments::Column::ProjectId.eq(project_id))
        .filter(deployments::Column::State.is_in(vec!["pending", "running", "deploying", "built"]))
        .all(db)
        .await
    {
        Ok(deps) => deps,
        Err(e) => {
            error!(
                "Failed to query in-flight deployments for environment {}: {}",
                environment_id, e
            );
            return;
        }
    };

    if in_flight.is_empty() {
        return;
    }

    info!(
        "Cancelling {} in-flight deployment(s) for environment {} (superseded by new push)",
        in_flight.len(),
        environment_id
    );

    for deployment in in_flight {
        let deployment_id = deployment.id;
        let environment_name = deployment.slug.clone();

        // Write cancellation message to any running job logs
        if let Ok(running_jobs) = deployment_jobs::Entity::find()
            .filter(deployment_jobs::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_jobs::Column::Status.eq(JobStatus::Running))
            .all(db)
            .await
        {
            for job in &running_jobs {
                debug!(
                    "Writing supersede cancellation to job {} log {}",
                    job.name, job.log_id
                );
            }
        }

        // Mark the deployment as cancelled
        let mut active: deployments::ActiveModel = deployment.into();
        active.state = Set("cancelled".to_string());
        active.cancelled_reason = Set(Some(
            "Superseded by a newer deployment for this environment".to_string(),
        ));
        active.finished_at = Set(Some(chrono::Utc::now()));
        active.updated_at = Set(chrono::Utc::now());

        if let Err(e) = active.update(db).await {
            error!(
                "Failed to cancel superseded deployment {}: {}",
                deployment_id, e
            );
            continue;
        }

        info!(
            "Cancelled deployment {} (superseded) for environment {}",
            deployment_id, environment_id
        );

        // Fire DeploymentCancelled event so notification systems can react
        let event = Job::DeploymentCancelled(temps_core::DeploymentCancelledJob {
            deployment_id,
            project_id,
            environment_id,
            environment_name: environment_name.clone(),
        });
        if let Err(e) = queue.send(event).await {
            warn!(
                "Failed to send DeploymentCancelled event for deployment {}: {}",
                deployment_id, e
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::Utc;
    use mockall::mock;
    use sea_orm::{ActiveModelTrait, Set};
    use temps_core::QueueError;
    use temps_database::test_utils::TestDatabase;
    use temps_entities::preset::Preset;
    use temps_entities::upstream_config::UpstreamList;
    use temps_logs::LogService;

    fn create_test_config_service(db: Arc<DbConnection>) -> Arc<temps_config::ConfigService> {
        let server_config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:3000".to_string(),
                "postgresql://test".to_string(),
                None,
                Some("127.0.0.1:8000".to_string()),
            )
            .unwrap(),
        );
        Arc::new(temps_config::ConfigService::new(server_config, db))
    }

    fn create_test_dsn_service(db: Arc<DbConnection>) -> Arc<temps_error_tracking::DSNService> {
        Arc::new(temps_error_tracking::DSNService::new(db))
    }

    mock! {
        JobReceiver {}

        #[async_trait]
        impl JobReceiver for JobReceiver {
            async fn recv(&mut self) -> Result<Job, QueueError>;
        }
    }

    #[allow(dead_code)]
    async fn setup_test_data(db: &DbConnection) -> Result<(i32, i32), Box<dyn std::error::Error>> {
        // Create test project
        let project = temps_entities::projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_owner: Set("test-owner".to_string()),
            repo_name: Set("test-repo".to_string()),
            git_provider_connection_id: Set(Some(1)),
            preset: Set(Preset::NextJs),
            directory: Set("/".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            deleted_at: Set(None),
            is_deleted: Set(false),
            is_public_repo: Set(false),
            git_url: Set(None),
            main_branch: Set("main".to_string()),
            ..Default::default()
        };
        let project = project.insert(db).await?;

        // Create test environment
        let environment = temps_entities::environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Test Environment".to_string()),
            slug: Set("test".to_string()),
            host: Set("test.example.com".to_string()),
            upstreams: Set(UpstreamList::default()),
            current_deployment_id: Set(None),
            subdomain: Set("test.example.com".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let environment = environment.insert(db).await?;

        // Create test deployment (no pipeline needed in new system)
        let deployment = temps_entities::deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("test-deployment-123".to_string()),
            state: Set("pending".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let deployment = deployment.insert(db).await?;

        Ok((deployment.id, deployment.id))
    }

    async fn setup_git_push_test_data(
        db: &DbConnection,
    ) -> Result<(i32, i32), Box<dyn std::error::Error>> {
        // Create test project with git repo info
        let project = temps_entities::projects::ActiveModel {
            name: Set("Git Push Test Project".to_string()),
            slug: Set("git-push-test".to_string()),
            repo_owner: Set("test-owner".to_string()),
            repo_name: Set("test-repo".to_string()),
            git_provider_connection_id: Set(Some(1)),
            preset: Set(Preset::NextJs),
            directory: Set("/".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            deleted_at: Set(None),
            is_deleted: Set(false),
            is_public_repo: Set(false),
            git_url: Set(None),
            main_branch: Set("main".to_string()),
            ..Default::default()
        };
        let project = project.insert(db).await?;

        // Create test environment
        let environment = temps_entities::environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Production".to_string()),
            slug: Set("production".to_string()),
            host: Set("test-production.example.com".to_string()),
            upstreams: Set(UpstreamList::default()),
            current_deployment_id: Set(None),
            subdomain: Set("test-production.example.com".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let environment = environment.insert(db).await?;

        Ok((project.id, environment.id))
    }

    #[tokio::test]
    async fn test_git_push_event_job_missing_project() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Create GitPushEventJob for non-existent project
        let git_push_job = temps_core::GitPushEventJob {
            owner: "nonexistent-owner".to_string(),
            repo: "nonexistent-repo".to_string(),
            branch: Some("main".to_string()),
            tag: None,
            commit: "abc123".to_string(),
            project_id: 0,
            manual_trigger: false,
            rollback_from_deployment_id: None,
            target_environment_id: None,
        };

        // Try to find the project (should return None)
        let project = temps_entities::projects::Entity::find()
            .filter(temps_entities::projects::Column::RepoOwner.eq(&git_push_job.owner))
            .filter(temps_entities::projects::Column::RepoName.eq(&git_push_job.repo))
            .one(db.as_ref())
            .await?;

        assert!(project.is_none(), "Project should not exist");

        // Verify no deployments were created (no pipeline needed in new system)
        let deployments = temps_entities::deployments::Entity::find()
            .all(db.as_ref())
            .await?;

        assert_eq!(deployments.len(), 0);

        Ok(())
    }

    #[tokio::test]
    async fn test_git_push_event_job_missing_environment() -> Result<(), Box<dyn std::error::Error>>
    {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Create project without environment
        use chrono::Utc;
        use sea_orm::Set;

        let project = temps_entities::projects::ActiveModel {
            name: Set("Project Without Environment".to_string()),
            slug: Set("no-env-project".to_string()),
            repo_owner: Set("test-owner".to_string()),
            repo_name: Set("no-env-repo".to_string()),
            git_provider_connection_id: Set(Some(1)),
            preset: Set(Preset::NextJs),
            directory: Set("/".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            deleted_at: Set(None),
            is_deleted: Set(false),
            is_public_repo: Set(false),
            git_url: Set(None),
            main_branch: Set("main".to_string()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await?;

        // Verify no environment exists for this project
        let environment = temps_entities::environments::Entity::find()
            .filter(temps_entities::environments::Column::ProjectId.eq(project.id))
            .one(db.as_ref())
            .await?;

        assert!(environment.is_none(), "Environment should not exist");

        Ok(())
    }

    #[tokio::test]
    async fn test_workflow_planner_integration() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let config_service = create_test_config_service(db.clone());
        let dsn_service = create_test_dsn_service(db.clone());

        // Create ExternalServiceManager with minimal setup
        let encryption_service = Arc::new(
            temps_core::EncryptionService::new(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .expect("Failed to create encryption service"),
        );
        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults().expect("Docker required for tests"),
        );
        let external_service_manager = Arc::new(temps_providers::ExternalServiceManager::new(
            db.clone(),
            encryption_service.clone(),
            docker,
            Arc::new(temps_providers::DnsRegistry::new(db.clone())),
        ));

        let workflow_planner = Arc::new(WorkflowPlanner::new(
            db.clone(),
            Arc::new(LogService::new(std::env::temp_dir())),
            external_service_manager,
            config_service,
            dsn_service,
            encryption_service.clone(),
        ));

        // Create test project, environment, and deployment
        let (project_id, environment_id) = setup_git_push_test_data(db.as_ref()).await?;

        // Create deployment
        let deployment = deployments::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(environment_id),
            slug: Set("test-deployment".to_string()),
            state: Set("pending".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let deployment = deployment.insert(db.as_ref()).await?;

        // Test workflow planner creates jobs
        let jobs = workflow_planner
            .create_deployment_jobs(deployment.id)
            .await?;

        // Verify jobs were created (nextjs project should create 9 jobs including
        // persist_static_assets, configure_crons, configure_agents, scan_vulnerabilities, and capture_source_maps)
        let job_ids: Vec<String> = jobs.iter().map(|j| j.job_id.clone()).collect();
        assert_eq!(
            jobs.len(),
            9,
            "Expected 9 jobs but got {}: {:?}",
            jobs.len(),
            job_ids
        );

        // Verify all expected jobs are present
        assert!(job_ids.contains(&"download_repo".to_string()));
        assert!(job_ids.contains(&"build_image".to_string()));
        assert!(job_ids.contains(&"deploy_container".to_string()));
        assert!(job_ids.contains(&"persist_static_assets".to_string()));
        assert!(job_ids.contains(&"mark_deployment_complete".to_string()));
        assert!(job_ids.contains(&"configure_crons".to_string()));
        assert!(job_ids.contains(&"scan_vulnerabilities".to_string()));
        assert!(job_ids.contains(&"capture_source_maps".to_string()));

        // Verify all jobs are in pending state
        for job in &jobs {
            assert_eq!(job.status, temps_entities::types::JobStatus::Pending);
            assert_eq!(job.deployment_id, deployment.id);
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_deployment_without_git_info() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let config_service = create_test_config_service(db.clone());
        let dsn_service = create_test_dsn_service(db.clone());

        // Create ExternalServiceManager with minimal setup
        let encryption_service = Arc::new(
            temps_core::EncryptionService::new(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .expect("Failed to create encryption service"),
        );
        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults().expect("Docker required for tests"),
        );
        let external_service_manager = Arc::new(temps_providers::ExternalServiceManager::new(
            db.clone(),
            encryption_service.clone(),
            docker,
            Arc::new(temps_providers::DnsRegistry::new(db.clone())),
        ));

        let workflow_planner = Arc::new(WorkflowPlanner::new(
            db.clone(),
            Arc::new(LogService::new(std::env::temp_dir())),
            external_service_manager,
            config_service,
            dsn_service,
            encryption_service.clone(),
        ));

        // Create project without git info (empty repo_owner and repo_name)
        use temps_entities::{environments, projects};
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project-no-git".to_string()),
            repo_owner: Set("".to_string()), // Empty - no git info
            repo_name: Set("".to_string()),  // Empty - no git info
            main_branch: Set("main".to_string()),

            git_provider_connection_id: Set(None),
            preset: Set(Preset::NextJs),
            directory: Set("/".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await?;

        // Create environment
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Production".to_string()),
            slug: Set("production".to_string()),
            host: Set("test.example.com".to_string()),
            upstreams: Set(UpstreamList::default()),
            subdomain: Set("test.example.com".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let environment = environment.insert(db.as_ref()).await?;

        // Create deployment
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("test-deployment".to_string()),
            state: Set("pending".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let deployment = deployment.insert(db.as_ref()).await?;

        // Create jobs - should skip download_repo
        let jobs = workflow_planner
            .create_deployment_jobs(deployment.id)
            .await?;

        // Should create 4 jobs (no download_repo): build_image, deploy_container, persist_static_assets, mark_deployment_complete
        let job_ids: Vec<String> = jobs.iter().map(|j| j.job_id.clone()).collect();
        assert_eq!(
            jobs.len(),
            4,
            "Expected 4 jobs but got {}: {:?}",
            jobs.len(),
            job_ids
        );

        assert!(!job_ids.contains(&"download_repo".to_string()));
        assert!(job_ids.contains(&"build_image".to_string()));
        assert!(job_ids.contains(&"deploy_container".to_string()));
        assert!(job_ids.contains(&"persist_static_assets".to_string()));
        assert!(job_ids.contains(&"mark_deployment_complete".to_string()));

        Ok(())
    }

    #[tokio::test]
    async fn test_job_status_transitions() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let config_service = create_test_config_service(db.clone());
        let dsn_service = create_test_dsn_service(db.clone());

        // Create ExternalServiceManager with minimal setup
        let encryption_service = Arc::new(
            temps_core::EncryptionService::new(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .expect("Failed to create encryption service"),
        );
        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults().expect("Docker required for tests"),
        );
        let external_service_manager = Arc::new(temps_providers::ExternalServiceManager::new(
            db.clone(),
            encryption_service.clone(),
            docker,
            Arc::new(temps_providers::DnsRegistry::new(db.clone())),
        ));

        let workflow_planner = Arc::new(WorkflowPlanner::new(
            db.clone(),
            Arc::new(LogService::new(std::env::temp_dir())),
            external_service_manager,
            config_service,
            dsn_service,
            encryption_service.clone(),
        ));

        // Create test setup
        let (project_id, environment_id) = setup_git_push_test_data(db.as_ref()).await?;

        let deployment = deployments::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(environment_id),
            slug: Set("test-deployment".to_string()),
            state: Set("pending".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let deployment = deployment.insert(db.as_ref()).await?;

        // Create jobs
        let jobs = workflow_planner
            .create_deployment_jobs(deployment.id)
            .await?;

        // Verify all jobs start as Pending
        use temps_entities::types::JobStatus;
        for job in &jobs {
            assert_eq!(job.status, JobStatus::Pending);
        }

        // Test updating job status
        let first_job = &jobs[0];
        let mut updated_job: temps_entities::deployment_jobs::ActiveModel =
            first_job.clone().into();
        updated_job.status = Set(JobStatus::Running);
        let updated_job = updated_job.update(db.as_ref()).await?;

        assert_eq!(updated_job.status, JobStatus::Running);

        Ok(())
    }

    /// Test that a branch with an exact environment match uses that environment
    #[tokio::test]
    async fn test_find_environment_with_exact_branch_match(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Create test project
        let project = temps_entities::projects::ActiveModel {
            name: Set("Branch Match Test".to_string()),
            slug: Set("branch-match-test".to_string()),
            repo_owner: Set("test-owner".to_string()),
            repo_name: Set("test-repo".to_string()),
            git_provider_connection_id: Set(Some(1)),
            preset: Set(Preset::NextJs),
            directory: Set("/".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            deleted_at: Set(None),
            is_deleted: Set(false),
            is_public_repo: Set(false),
            git_url: Set(None),
            main_branch: Set("main".to_string()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await?;

        // Create environment with specific branch
        let production_env = temps_entities::environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Production".to_string()),
            slug: Set("production".to_string()),
            host: Set("production.example.com".to_string()),
            branch: Set(Some("main".to_string())), // Matches "main" branch
            upstreams: Set(UpstreamList::default()),
            subdomain: Set("production.example.com".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let production_env = production_env.insert(db.as_ref()).await?;

        // Test finding environment for "main" branch
        let found_envs = find_environments_for_branch(db.clone(), &project, Some("main")).await?;

        assert_eq!(found_envs.len(), 1, "exactly one env matches branch 'main'");
        let found_env = &found_envs[0];
        assert_eq!(found_env.id, production_env.id);
        assert_eq!(found_env.name, "Production");
        assert_eq!(found_env.branch, Some("main".to_string()));

        Ok(())
    }

    /// A manual trigger that names an explicit `target_environment_id` deploys to
    /// exactly that environment, bypassing branch matching. This reproduces the
    /// temps-sre-demo bug: neither environment had a branch configured, so a
    /// branch-based resolve would miss "production" entirely and fall through to
    /// the env named "preview" — but an explicit target must still hit production.
    #[tokio::test]
    async fn test_resolve_target_environments_honors_explicit_target(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        let project = temps_entities::projects::ActiveModel {
            name: Set("Target Test".to_string()),
            slug: Set("target-test".to_string()),
            repo_owner: Set("test-owner".to_string()),
            repo_name: Set("test-repo".to_string()),
            preset: Set(Preset::NextJs),
            directory: Set("/".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            is_public_repo: Set(false),
            main_branch: Set("main".to_string()),
            // Preview envs disabled — the legacy "named preview" fallback path.
            enable_preview_environments: Set(false),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await?;

        // Production: NO branch configured (like the real temps-sre-demo).
        let production = temps_entities::environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            host: Set("prod.example.com".to_string()),
            branch: Set(None),
            upstreams: Set(UpstreamList::default()),
            subdomain: Set("prod.example.com".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let production = production.insert(db.as_ref()).await?;

        // An env literally named "preview" — what the branch fallback would pick.
        let preview = temps_entities::environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("preview".to_string()),
            slug: Set("preview".to_string()),
            host: Set("preview.example.com".to_string()),
            branch: Set(None),
            upstreams: Set(UpstreamList::default()),
            subdomain: Set("preview.example.com".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let preview = preview.insert(db.as_ref()).await?;

        let base_job = |target: Option<i32>| temps_core::GitPushEventJob {
            owner: "test-owner".to_string(),
            repo: "test-repo".to_string(),
            branch: Some("main".to_string()),
            tag: None,
            commit: "abc123".to_string(),
            project_id: project.id,
            manual_trigger: true,
            rollback_from_deployment_id: None,
            target_environment_id: target,
        };

        // Explicit target → exactly production, despite no branch match.
        let job = base_job(Some(production.id));
        let envs = resolve_target_environments(db.clone(), &project, &job).await?;
        assert_eq!(envs.len(), 1, "explicit target yields exactly one env");
        assert_eq!(
            envs[0].id, production.id,
            "explicit target_environment_id must win over branch matching"
        );

        // No explicit target → branch fallback picks the env named "preview"
        // (the pre-fix behaviour), proving the target is what redirects it.
        let job = base_job(None);
        let envs = resolve_target_environments(db.clone(), &project, &job).await?;
        assert_eq!(envs.len(), 1);
        assert_eq!(
            envs[0].id, preview.id,
            "without a target, the branch fallback lands on the named-preview env"
        );

        Ok(())
    }

    /// Test that a branch without a match uses existing preview environment
    #[tokio::test]
    async fn test_find_environment_uses_existing_preview() -> Result<(), Box<dyn std::error::Error>>
    {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Create test project
        let project = temps_entities::projects::ActiveModel {
            name: Set("Existing Preview Test".to_string()),
            slug: Set("existing-preview-test".to_string()),
            repo_owner: Set("test-owner".to_string()),
            repo_name: Set("test-repo".to_string()),
            git_provider_connection_id: Set(Some(1)),
            preset: Set(Preset::NextJs),
            directory: Set("/".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            deleted_at: Set(None),
            is_deleted: Set(false),
            is_public_repo: Set(false),
            git_url: Set(None),
            main_branch: Set("main".to_string()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await?;

        // Create production environment with branch
        let _production_env = temps_entities::environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Production".to_string()),
            slug: Set("production".to_string()),
            host: Set("production.example.com".to_string()),
            branch: Set(Some("main".to_string())),
            upstreams: Set(UpstreamList::default()),
            subdomain: Set("production.example.com".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let _production_env = _production_env.insert(db.as_ref()).await?;

        // Create preview environment
        let preview_env = temps_entities::environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("preview".to_string()),
            slug: Set("preview".to_string()),
            host: Set(String::new()),
            branch: Set(None), // No specific branch
            upstreams: Set(UpstreamList::default()),
            subdomain: Set("existing-preview-test-preview".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let preview_env = preview_env.insert(db.as_ref()).await?;

        // Test finding environment for "feature-auth" branch (no exact match)
        let found_envs =
            find_environments_for_branch(db.clone(), &project, Some("feature-auth")).await?;

        assert_eq!(found_envs.len(), 1, "falls back to the single preview env");
        let found_env = &found_envs[0];
        assert_eq!(found_env.id, preview_env.id);
        assert_eq!(found_env.name, "preview");
        assert_eq!(found_env.branch, None); // Preview has no specific branch

        Ok(())
    }

    /// Test that preview environment is auto-created when it doesn't exist
    #[tokio::test]
    async fn test_find_environment_creates_preview_when_missing(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Create test project
        let project = temps_entities::projects::ActiveModel {
            name: Set("Auto Create Preview Test".to_string()),
            slug: Set("auto-create-preview-test".to_string()),
            repo_owner: Set("test-owner".to_string()),
            repo_name: Set("test-repo".to_string()),
            git_provider_connection_id: Set(Some(1)),
            preset: Set(Preset::NextJs),
            directory: Set("/".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            deleted_at: Set(None),
            is_deleted: Set(false),
            is_public_repo: Set(false),
            git_url: Set(None),
            main_branch: Set("main".to_string()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await?;

        // Create only production environment (no preview)
        let _production_env = temps_entities::environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Production".to_string()),
            slug: Set("production".to_string()),
            host: Set("production.example.com".to_string()),
            branch: Set(Some("main".to_string())),
            upstreams: Set(UpstreamList::default()),
            subdomain: Set("production.example.com".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let _production_env = _production_env.insert(db.as_ref()).await?;

        // Verify no preview environment exists
        let preview_before = temps_entities::environments::Entity::find()
            .filter(temps_entities::environments::Column::ProjectId.eq(project.id))
            .filter(temps_entities::environments::Column::Name.eq("preview"))
            .one(db.as_ref())
            .await?;
        assert!(preview_before.is_none(), "Preview should not exist yet");

        // Test finding environment for "feature-xyz" branch (should create preview)
        let found_envs =
            find_environments_for_branch(db.clone(), &project, Some("feature-xyz")).await?;

        // Verify preview environment was created
        assert_eq!(found_envs.len(), 1, "creates one generic preview env");
        let found_env = &found_envs[0];
        assert_eq!(found_env.name, "preview");
        assert_eq!(found_env.slug, "preview");
        assert_eq!(found_env.subdomain, "auto-create-preview-test-preview");
        assert_eq!(found_env.host, "");
        assert_eq!(found_env.branch, None); // No specific branch
        assert_eq!(found_env.project_id, project.id);

        // Verify preview environment persisted in database
        let preview_after = temps_entities::environments::Entity::find()
            .filter(temps_entities::environments::Column::ProjectId.eq(project.id))
            .filter(temps_entities::environments::Column::Name.eq("preview"))
            .one(db.as_ref())
            .await?;
        assert!(preview_after.is_some(), "Preview should exist now");

        Ok(())
    }

    /// Test that multiple branches without matches all use the same preview environment
    #[tokio::test]
    async fn test_multiple_branches_share_preview_environment(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Create test project
        let project = temps_entities::projects::ActiveModel {
            name: Set("Multi Branch Preview Test".to_string()),
            slug: Set("multi-branch-preview-test".to_string()),
            repo_owner: Set("test-owner".to_string()),
            repo_name: Set("test-repo".to_string()),
            git_provider_connection_id: Set(Some(1)),
            preset: Set(Preset::NextJs),
            directory: Set("/".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            deleted_at: Set(None),
            is_deleted: Set(false),
            is_public_repo: Set(false),
            git_url: Set(None),
            main_branch: Set("main".to_string()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await?;

        // Create production environment
        let _production_env = temps_entities::environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Production".to_string()),
            slug: Set("production".to_string()),
            host: Set("production.example.com".to_string()),
            branch: Set(Some("main".to_string())),
            upstreams: Set(UpstreamList::default()),
            subdomain: Set("production.example.com".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let _production_env = _production_env.insert(db.as_ref()).await?;

        // Find environment for first feature branch (creates preview)
        let envs1 =
            find_environments_for_branch(db.clone(), &project, Some("feature-auth")).await?;

        // Find environment for second feature branch (reuses preview)
        let envs2 =
            find_environments_for_branch(db.clone(), &project, Some("feature-payments")).await?;

        // Find environment for third feature branch (reuses preview)
        let envs3 =
            find_environments_for_branch(db.clone(), &project, Some("bugfix-login")).await?;

        // Each call returns exactly the one shared preview environment
        assert_eq!(envs1.len(), 1);
        assert_eq!(envs2.len(), 1);
        assert_eq!(envs3.len(), 1);
        let (env1, env2, env3) = (&envs1[0], &envs2[0], &envs3[0]);

        // All three should return the same preview environment
        assert_eq!(env1.id, env2.id);
        assert_eq!(env2.id, env3.id);
        assert_eq!(env1.name, "preview");

        // Verify only one preview environment was created
        let all_preview_envs = temps_entities::environments::Entity::find()
            .filter(temps_entities::environments::Column::ProjectId.eq(project.id))
            .filter(temps_entities::environments::Column::Name.eq("preview"))
            .all(db.as_ref())
            .await?;
        assert_eq!(
            all_preview_envs.len(),
            1,
            "Should only have one preview environment"
        );

        Ok(())
    }

    /// Test that when no branch is provided, first environment is used
    #[tokio::test]
    async fn test_find_environment_no_branch_uses_first() -> Result<(), Box<dyn std::error::Error>>
    {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Create test project
        let project = temps_entities::projects::ActiveModel {
            name: Set("No Branch Test".to_string()),
            slug: Set("no-branch-test".to_string()),
            repo_owner: Set("test-owner".to_string()),
            repo_name: Set("test-repo".to_string()),
            git_provider_connection_id: Set(Some(1)),
            preset: Set(Preset::NextJs),
            directory: Set("/".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            deleted_at: Set(None),
            is_deleted: Set(false),
            is_public_repo: Set(false),
            git_url: Set(None),
            main_branch: Set("main".to_string()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await?;

        // Create multiple environments
        let env1 = temps_entities::environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Production".to_string()),
            slug: Set("production".to_string()),
            host: Set("production.example.com".to_string()),
            branch: Set(Some("main".to_string())),
            upstreams: Set(UpstreamList::default()),
            subdomain: Set("production.example.com".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let env1 = env1.insert(db.as_ref()).await?;

        let _env2 = temps_entities::environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Staging".to_string()),
            slug: Set("staging".to_string()),
            host: Set("staging.example.com".to_string()),
            branch: Set(Some("develop".to_string())),
            upstreams: Set(UpstreamList::default()),
            subdomain: Set("staging.example.com".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let _env2 = _env2.insert(db.as_ref()).await?;

        // Test finding environment with no branch specified
        let found_envs = find_environments_for_branch(db.clone(), &project, None).await?;

        // Should return the first environment (by database order)
        assert_eq!(found_envs.len(), 1, "no branch → single first env");
        assert_eq!(found_envs[0].id, env1.id);

        Ok(())
    }

    /// Test that deleted environments are ignored
    #[tokio::test]
    async fn test_find_environment_ignores_deleted_environments(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        // Create test project
        let project = temps_entities::projects::ActiveModel {
            name: Set("Deleted Env Test".to_string()),
            slug: Set("deleted-env-test".to_string()),
            repo_owner: Set("test-owner".to_string()),
            repo_name: Set("test-repo".to_string()),
            git_provider_connection_id: Set(Some(1)),
            preset: Set(Preset::NextJs),
            directory: Set("/".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            deleted_at: Set(None),
            is_deleted: Set(false),
            is_public_repo: Set(false),
            git_url: Set(None),
            main_branch: Set("main".to_string()),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await?;

        // Create deleted preview environment
        let deleted_preview = temps_entities::environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("preview".to_string()),
            slug: Set("preview".to_string()),
            host: Set(String::new()),
            branch: Set(None),
            upstreams: Set(UpstreamList::default()),
            subdomain: Set("deleted-env-test-preview".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            deleted_at: Set(Some(Utc::now())), // Mark as deleted
            ..Default::default()
        };
        let _deleted_preview = deleted_preview.insert(db.as_ref()).await?;

        // Create active production environment
        let _production_env = temps_entities::environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Production".to_string()),
            slug: Set("production".to_string()),
            host: Set("production.example.com".to_string()),
            branch: Set(Some("main".to_string())),
            upstreams: Set(UpstreamList::default()),
            subdomain: Set("production.example.com".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            deleted_at: Set(None),
            ..Default::default()
        };
        let _production_env = _production_env.insert(db.as_ref()).await?;

        // Test finding environment for feature branch
        // Should create NEW preview (ignore deleted one)
        let found_envs =
            find_environments_for_branch(db.clone(), &project, Some("feature-test")).await?;

        assert_eq!(found_envs.len(), 1, "creates one fresh preview env");
        let found_env = &found_envs[0];
        assert_eq!(found_env.name, "preview");
        assert!(
            found_env.deleted_at.is_none(),
            "Preview should not be deleted"
        );

        // Verify two preview environments exist (one deleted, one active)
        let all_preview_envs = temps_entities::environments::Entity::find()
            .filter(temps_entities::environments::Column::ProjectId.eq(project.id))
            .filter(temps_entities::environments::Column::Name.eq("preview"))
            .all(db.as_ref())
            .await?;
        assert_eq!(
            all_preview_envs.len(),
            2,
            "Should have two preview environments (one deleted, one active)"
        );

        Ok(())
    }

    fn cfg_with_auto_deploy(value: bool) -> temps_entities::deployment_config::DeploymentConfig {
        temps_entities::deployment_config::DeploymentConfig {
            automatic_deploy: Some(value),
            ..Default::default()
        }
    }

    #[test]
    fn auto_deploy_disabled_when_both_configs_missing() {
        assert!(!is_automatic_deploy_enabled(None, None));
    }

    #[test]
    fn auto_deploy_disabled_when_project_off_and_env_missing() {
        let project_cfg = cfg_with_auto_deploy(false);
        assert!(!is_automatic_deploy_enabled(Some(&project_cfg), None));
    }

    #[test]
    fn auto_deploy_enabled_when_project_on_and_env_missing() {
        let project_cfg = cfg_with_auto_deploy(true);
        assert!(is_automatic_deploy_enabled(Some(&project_cfg), None));
    }

    #[test]
    fn auto_deploy_disabled_when_both_sides_off() {
        let project_cfg = cfg_with_auto_deploy(false);
        let env_cfg = cfg_with_auto_deploy(false);
        assert!(!is_automatic_deploy_enabled(
            Some(&project_cfg),
            Some(&env_cfg)
        ));
    }

    #[test]
    fn auto_deploy_enabled_when_env_overrides_project_off() {
        let project_cfg = cfg_with_auto_deploy(false);
        let env_cfg = cfg_with_auto_deploy(true);
        assert!(is_automatic_deploy_enabled(
            Some(&project_cfg),
            Some(&env_cfg)
        ));
    }

    #[test]
    fn auto_deploy_enabled_when_only_env_set_on() {
        let env_cfg = cfg_with_auto_deploy(true);
        assert!(is_automatic_deploy_enabled(None, Some(&env_cfg)));
    }

    #[test]
    fn auto_deploy_env_false_overrides_project_true() {
        // Env explicitly opts out even though project is on — env wins.
        let project_cfg = cfg_with_auto_deploy(true);
        let env_cfg = cfg_with_auto_deploy(false);
        assert!(!is_automatic_deploy_enabled(
            Some(&project_cfg),
            Some(&env_cfg)
        ));
    }

    #[test]
    fn auto_deploy_env_none_inherits_project_true() {
        // Env has no explicit setting — inherits project's true.
        let project_cfg = cfg_with_auto_deploy(true);
        let env_cfg = temps_entities::deployment_config::DeploymentConfig {
            automatic_deploy: None,
            ..Default::default()
        };
        assert!(is_automatic_deploy_enabled(
            Some(&project_cfg),
            Some(&env_cfg)
        ));
    }
}
