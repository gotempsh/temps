// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::services::workflow_execution_service::WorkflowExecutionService;
use crate::services::workflow_planner::WorkflowPlanner;
use sea_orm::{
    sea_query::{Expr, LockType, Query},
    ActiveModelTrait, ColumnTrait, Condition, DatabaseTransaction, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, Set, TransactionTrait,
};
use serde_json;
use std::collections::HashMap;
use std::sync::Arc;
use temps_core::{Job, JobQueue, JobReceiver};
use temps_database::DbConnection;
use temps_entities::{
    deployments, environments,
    prelude::{DeploymentConfigSnapshot, DeploymentMetadata, GitPushEvent},
    types::PipelineStatus,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{debug, error, info, trace, warn};

#[derive(Debug)]
pub enum JobProcessorError {
    QueueError(String),
    PipelineError(String),
    DatabaseError(String),
    FailoverRecoverySemaphoreClosed {
        project_id: i32,
        environment_id: Option<i32>,
        recovery_kind: &'static str,
        reason: String,
    },
    DeploymentCreationFailed {
        project_id: i32,
        environment_id: i32,
        operation: &'static str,
        reason: String,
    },
    Other(String),
}

impl std::fmt::Display for JobProcessorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobProcessorError::QueueError(msg) => write!(f, "Queue error: {}", msg),
            JobProcessorError::PipelineError(msg) => write!(f, "Pipeline error: {}", msg),
            JobProcessorError::DatabaseError(msg) => write!(f, "Database error: {}", msg),
            JobProcessorError::FailoverRecoverySemaphoreClosed {
                project_id,
                environment_id,
                recovery_kind,
                reason,
            } => write!(
                f,
                "Failover {recovery_kind} recovery for project {project_id}, environment {environment_id:?} could not acquire its deployment slot: {reason}"
            ),
            JobProcessorError::DeploymentCreationFailed {
                project_id,
                environment_id,
                operation,
                reason,
            } => write!(
                f,
                "Failed to {operation} deployment for project {project_id}, environment {environment_id}: {reason}"
            ),
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

enum DeploymentDuplicateKey {
    Commit(String),
    Image(String),
}

enum DeploymentCreationOutcome {
    Created {
        deployment: Box<deployments::Model>,
        cancellation_events: Vec<Job>,
    },
    Duplicate {
        deployment_id: i32,
        state: String,
    },
    StaleRecovery {
        source_deployment_id: i32,
        current_deployment_id: Option<i32>,
        newer_deployment_id: Option<i32>,
    },
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
    /// Failover recovery can fan out across many environments after a node
    /// outage. Keep those workflows strictly serial without throttling normal
    /// webhook, manual, or drain-triggered deployments.
    failover_recovery_semaphore: Arc<Semaphore>,
}

impl JobProcessorService {
    /// Acquire the dedicated failover slot when this job is an automatic node
    /// recovery. Ordinary jobs return immediately without touching the
    /// semaphore, so webhook, manual, and drain-triggered deploys retain their
    /// existing concurrency.
    async fn acquire_failover_recovery_permit(
        semaphore: Arc<Semaphore>,
        recovery_of_deployment_id: Option<i32>,
        project_id: i32,
        environment_id: Option<i32>,
        recovery_kind: &'static str,
    ) -> Result<Option<OwnedSemaphorePermit>, JobProcessorError> {
        if recovery_of_deployment_id.is_none() {
            return Ok(None);
        }

        info!(
            project_id,
            environment_id = ?environment_id,
            recovery_of_deployment_id = ?recovery_of_deployment_id,
            recovery_kind,
            "Failover recovery queued; waiting for the dedicated deployment slot"
        );

        let permit = semaphore.acquire_owned().await.map_err(|error| {
            JobProcessorError::FailoverRecoverySemaphoreClosed {
                project_id,
                environment_id,
                recovery_kind,
                reason: error.to_string(),
            }
        })?;

        info!(
            project_id,
            environment_id = ?environment_id,
            recovery_of_deployment_id = ?recovery_of_deployment_id,
            recovery_kind,
            "Failover recovery acquired the dedicated deployment slot"
        );

        Ok(Some(permit))
    }

    /// Serialize deployment generation changes on the environment row. Every
    /// caller, including ordinary webhooks and manual deploys, takes the same
    /// lock so the order in which jobs supersede each other is deterministic.
    async fn create_deployment_with_generation_fence(
        db: &DbConnection,
        project_id: i32,
        environment_id: i32,
        recovery_of_deployment_id: Option<i32>,
        duplicate_key: DeploymentDuplicateKey,
        mut new_deployment: deployments::ActiveModel,
    ) -> Result<DeploymentCreationOutcome, JobProcessorError> {
        let creation_error = |operation: &'static str, error: sea_orm::DbErr| {
            JobProcessorError::DeploymentCreationFailed {
                project_id,
                environment_id,
                operation,
                reason: error.to_string(),
            }
        };

        let transaction = db
            .begin()
            .await
            .map_err(|error| creation_error("begin transaction for", error))?;

        let environment = super::services::lock_environment_for_deployment_generation(
            &transaction,
            project_id,
            environment_id,
        )
        .await
        .map_err(|error| creation_error("lock environment before creating", error))?;

        if let Some(source_deployment_id) = recovery_of_deployment_id {
            let mut newer_deployment_id = None;
            if environment.current_deployment_id == Some(source_deployment_id) {
                let source = deployments::Entity::find_by_id(source_deployment_id)
                    .filter(deployments::Column::ProjectId.eq(project_id))
                    .filter(deployments::Column::EnvironmentId.eq(environment_id))
                    .one(&transaction)
                    .await
                    .map_err(|error| creation_error("load recovery source for", error))?;

                if let Some(source) = source {
                    newer_deployment_id = deployments::Entity::find()
                        .filter(deployments::Column::ProjectId.eq(project_id))
                        .filter(deployments::Column::EnvironmentId.eq(environment_id))
                        .filter(deployments::Column::State.is_in(vec![
                            "pending",
                            "running",
                            "deploying",
                            "built",
                            "ready",
                        ]))
                        .filter(
                            Condition::any()
                                .add(deployments::Column::CreatedAt.gt(source.created_at))
                                .add(
                                    Condition::all()
                                        .add(deployments::Column::CreatedAt.eq(source.created_at))
                                        .add(deployments::Column::Id.gt(source.id)),
                                ),
                        )
                        .order_by_desc(deployments::Column::CreatedAt)
                        .order_by_desc(deployments::Column::Id)
                        .one(&transaction)
                        .await
                        .map_err(|error| {
                            creation_error(
                                "check newer in-flight generations before recovering",
                                error,
                            )
                        })?
                        .map(|deployment| deployment.id);
                } else {
                    newer_deployment_id = Some(source_deployment_id);
                }
            }

            if environment.current_deployment_id != Some(source_deployment_id)
                || newer_deployment_id.is_some()
            {
                let current_deployment_id = environment.current_deployment_id;
                transaction.commit().await.map_err(|error| {
                    creation_error("finish stale recovery validation for", error)
                })?;
                return Ok(DeploymentCreationOutcome::StaleRecovery {
                    source_deployment_id,
                    current_deployment_id,
                    newer_deployment_id,
                });
            }
        }

        let duplicate_query = deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project_id))
            .filter(deployments::Column::EnvironmentId.eq(environment_id))
            .filter(deployments::Column::State.is_in(vec![
                "pending",
                "running",
                "deploying",
                "built",
                "ready",
            ]));
        let duplicate_query = if let Some(source_deployment_id) = recovery_of_deployment_id {
            duplicate_query.filter(deployments::Column::Id.ne(source_deployment_id))
        } else {
            duplicate_query
        };
        let duplicate_query = match duplicate_key {
            DeploymentDuplicateKey::Commit(commit) => {
                duplicate_query.filter(deployments::Column::CommitSha.eq(commit))
            }
            DeploymentDuplicateKey::Image(image) => {
                duplicate_query.filter(deployments::Column::ImageName.eq(image))
            }
        };
        if let Some(existing) = duplicate_query
            .order_by_desc(deployments::Column::CreatedAt)
            .order_by_desc(deployments::Column::Id)
            .one(&transaction)
            .await
            .map_err(|error| creation_error("check duplicate generations before creating", error))?
        {
            let outcome = DeploymentCreationOutcome::Duplicate {
                deployment_id: existing.id,
                state: existing.state,
            };
            transaction
                .commit()
                .await
                .map_err(|error| creation_error("finish duplicate validation for", error))?;
            return Ok(outcome);
        }

        let cancellation_events = cancel_in_flight_deployments(
            &transaction,
            project_id,
            environment_id,
            environment.current_deployment_id,
        )
        .await?;
        let generation_time = chrono::Utc::now();
        new_deployment.created_at = Set(generation_time);
        new_deployment.updated_at = Set(generation_time);
        let deployment = new_deployment
            .insert(&transaction)
            .await
            .map_err(|error| creation_error("insert", error))?;
        transaction
            .commit()
            .await
            .map_err(|error| creation_error("commit", error))?;

        Ok(DeploymentCreationOutcome::Created {
            deployment: Box::new(deployment),
            cancellation_events,
        })
    }

    async fn send_post_commit_events(queue: &Arc<dyn JobQueue>, events: Vec<Job>) {
        for event in events {
            if let Err(error) = queue.send(event).await {
                warn!(error = %error, "Failed to publish post-commit deployment event");
            }
        }
    }

    fn recovery_source(deployment: &deployments::Model) -> Option<i32> {
        deployment
            .context_vars
            .as_ref()
            .and_then(|context| context.get("recovery_of_deployment_id"))
            .and_then(serde_json::Value::as_i64)
            .and_then(|id| i32::try_from(id).ok())
    }

    /// Revalidate a gate-blocked recovery under the same environment lock used
    /// by deployment creation. A manual/webhook generation committed while the
    /// gate was blocked makes this recovery stale and cancels it before it can
    /// enter the workflow.
    async fn validate_gate_rechecked_recovery(
        db: &DbConnection,
        deployment: &deployments::Model,
        source_deployment_id: i32,
    ) -> Result<bool, JobProcessorError> {
        let project_id = deployment.project_id;
        let environment_id = deployment.environment_id;
        let creation_error = |operation: &'static str, error: sea_orm::DbErr| {
            JobProcessorError::DeploymentCreationFailed {
                project_id,
                environment_id,
                operation,
                reason: error.to_string(),
            }
        };
        let transaction = db
            .begin()
            .await
            .map_err(|error| creation_error("begin gate recheck transaction for", error))?;
        let environment = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_null())
            .lock(LockType::Update)
            .one(&transaction)
            .await
            .map_err(|error| creation_error("lock environment during gate recheck for", error))?;

        let newer_deployment_id = deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project_id))
            .filter(deployments::Column::EnvironmentId.eq(environment_id))
            .filter(deployments::Column::Id.ne(deployment.id))
            .filter(deployments::Column::State.is_in(vec![
                "pending",
                "running",
                "deploying",
                "built",
                "ready",
            ]))
            .filter(
                Condition::any()
                    .add(deployments::Column::CreatedAt.gt(deployment.created_at))
                    .add(
                        Condition::all()
                            .add(deployments::Column::CreatedAt.eq(deployment.created_at))
                            .add(deployments::Column::Id.gt(deployment.id)),
                    ),
            )
            .order_by_desc(deployments::Column::CreatedAt)
            .order_by_desc(deployments::Column::Id)
            .one(&transaction)
            .await
            .map_err(|error| {
                creation_error("check newer generations during gate recheck for", error)
            })?
            .map(|newer| newer.id);
        let current_deployment_id = environment.and_then(|env| env.current_deployment_id);
        let valid =
            current_deployment_id == Some(source_deployment_id) && newer_deployment_id.is_none();

        if !valid {
            deployments::Entity::update_many()
                .col_expr(deployments::Column::State, Expr::value("cancelled"))
                .col_expr(
                    deployments::Column::CancelledReason,
                    Expr::value("Failover recovery was superseded while awaiting deployment gate"),
                )
                .col_expr(
                    deployments::Column::FinishedAt,
                    Expr::value(chrono::Utc::now()),
                )
                .col_expr(
                    deployments::Column::UpdatedAt,
                    Expr::value(chrono::Utc::now()),
                )
                .filter(deployments::Column::Id.eq(deployment.id))
                .filter(deployments::Column::State.eq("pending"))
                .exec(&transaction)
                .await
                .map_err(|error| creation_error("cancel stale gate-blocked recovery for", error))?;
        }

        transaction
            .commit()
            .await
            .map_err(|error| creation_error("commit gate recheck validation for", error))?;

        if !valid {
            info!(
                project_id,
                environment_id,
                deployment_id = deployment.id,
                source_deployment_id,
                current_deployment_id = ?current_deployment_id,
                newer_deployment_id = ?newer_deployment_id,
                "Skipping stale gate-blocked failover recovery"
            );
        }
        Ok(valid)
    }

    async fn resolve_image_target_environments(
        db: &DbConnection,
        project_id: i32,
        target_environment_id: Option<i32>,
    ) -> Result<Vec<temps_entities::environments::Model>, sea_orm::DbErr> {
        let mut query = temps_entities::environments::Entity::find()
            .filter(temps_entities::environments::Column::ProjectId.eq(project_id))
            .filter(temps_entities::environments::Column::DeletedAt.is_null());
        query = if let Some(environment_id) = target_environment_id {
            query.filter(temps_entities::environments::Column::Id.eq(environment_id))
        } else {
            query.filter(temps_entities::environments::Column::IsPreview.eq(false))
        };
        query.all(db).await
    }

    /// Atomically admit a pending deployment only while both owners are active.
    pub(crate) async fn try_admit_deployment(
        db: &DbConnection,
        deployment_id: i32,
    ) -> Result<bool, JobProcessorError> {
        let active_projects = Query::select()
            .column(temps_entities::projects::Column::Id)
            .from(temps_entities::projects::Entity)
            .and_where(temps_entities::projects::Column::IsDeleted.eq(false))
            .to_owned();
        let active_environments = Query::select()
            .column(temps_entities::environments::Column::Id)
            .from(temps_entities::environments::Entity)
            .and_where(temps_entities::environments::Column::DeletedAt.is_null())
            .to_owned();
        let admitted_at = chrono::Utc::now();

        let admitted = deployments::Entity::update_many()
            .col_expr(deployments::Column::State, Expr::value("running"))
            .col_expr(deployments::Column::StartedAt, Expr::value(admitted_at))
            .col_expr(deployments::Column::UpdatedAt, Expr::value(admitted_at))
            .filter(deployments::Column::Id.eq(deployment_id))
            .filter(deployments::Column::State.eq("pending"))
            .filter(deployments::Column::ProjectId.in_subquery(active_projects))
            .filter(deployments::Column::EnvironmentId.in_subquery(active_environments))
            .exec(db)
            .await
            .map(|result| result.rows_affected == 1)
            .map_err(|error| JobProcessorError::DatabaseError(error.to_string()))?;

        if !admitted {
            // A deployment may be inserted after deletion took its cancellation
            // snapshot. Do not leave that denied row pending forever.
            deployments::Entity::update_many()
                .col_expr(deployments::Column::State, Expr::value("cancelled"))
                .col_expr(
                    deployments::Column::CancelledReason,
                    Expr::value("Deployment owner is being deleted"),
                )
                .col_expr(
                    deployments::Column::FinishedAt,
                    Expr::value(chrono::Utc::now()),
                )
                .col_expr(
                    deployments::Column::UpdatedAt,
                    Expr::value(chrono::Utc::now()),
                )
                .filter(deployments::Column::Id.eq(deployment_id))
                .filter(deployments::Column::State.eq("pending"))
                .exec(db)
                .await
                .map_err(|error| JobProcessorError::DatabaseError(error.to_string()))?;
        }

        Ok(admitted)
    }

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
            failover_recovery_semaphore: Arc::new(Semaphore::new(1)),
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
            failover_recovery_semaphore: Arc::new(Semaphore::new(1)),
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
                            let failover_recovery_semaphore =
                                Arc::clone(&self.failover_recovery_semaphore);
                            let recovery_of_deployment_id = git_push_job.recovery_of_deployment_id;
                            let recovery_project_id = git_push_job.project_id;
                            let recovery_environment_id = git_push_job.target_environment_id;

                            // Spawn a task to handle the job asynchronously
                            tokio::spawn(async move {
                                let recovery_permit = match Self::acquire_failover_recovery_permit(
                                    failover_recovery_semaphore,
                                    recovery_of_deployment_id,
                                    recovery_project_id,
                                    recovery_environment_id,
                                    "git",
                                )
                                .await
                                {
                                    Ok(permit) => permit,
                                    Err(error) => {
                                        error!(
                                            project_id = recovery_project_id,
                                            environment_id = ?recovery_environment_id,
                                            recovery_kind = "git",
                                            error = %error,
                                            "Failover recovery could not acquire the dedicated deployment slot"
                                        );
                                        return;
                                    }
                                };

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

                                if recovery_of_deployment_id.is_some() {
                                    info!(
                                        project_id = recovery_project_id,
                                        environment_id = ?recovery_environment_id,
                                        recovery_kind = "git",
                                        "Failover recovery finished; releasing the dedicated deployment slot"
                                    );
                                }
                                drop(recovery_permit);
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
                            let failover_recovery_semaphore =
                                Arc::clone(&self.failover_recovery_semaphore);
                            let recovery_of_deployment_id = image_job.recovery_of_deployment_id;
                            let recovery_project_id = image_job.project_id;
                            let recovery_environment_id = image_job.target_environment_id;

                            tokio::spawn(async move {
                                let recovery_permit = match Self::acquire_failover_recovery_permit(
                                    failover_recovery_semaphore,
                                    recovery_of_deployment_id,
                                    recovery_project_id,
                                    recovery_environment_id,
                                    "image",
                                )
                                .await
                                {
                                    Ok(permit) => permit,
                                    Err(error) => {
                                        error!(
                                            project_id = recovery_project_id,
                                            environment_id = ?recovery_environment_id,
                                            recovery_kind = "image",
                                            error = %error,
                                            "Failover recovery could not acquire the dedicated deployment slot"
                                        );
                                        return;
                                    }
                                };

                                Self::process_deploy_image_requested_job(
                                    workflow_planner,
                                    workflow_executor,
                                    db,
                                    queue,
                                    deployment_gate,
                                    image_job,
                                )
                                .await;

                                if recovery_of_deployment_id.is_some() {
                                    info!(
                                        project_id = recovery_project_id,
                                        environment_id = ?recovery_environment_id,
                                        recovery_kind = "image",
                                        "Failover recovery finished; releasing the dedicated deployment slot"
                                    );
                                }
                                drop(recovery_permit);
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
                            let failover_recovery_semaphore =
                                Arc::clone(&self.failover_recovery_semaphore);

                            tokio::spawn(async move {
                                Self::process_deployment_gate_recheck_job(
                                    db,
                                    workflow_executor,
                                    deployment_gate,
                                    failover_recovery_semaphore,
                                    recheck_job,
                                )
                                .await;
                            });
                        }
                        _ => {
                            // The queue is broadcast to several specialized
                            // processors. Seeing another processor's event is
                            // expected, not an unhandled system error.
                            trace!(
                                "Deployment processor skipped event owned by another subscriber: {}",
                                job
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
            .filter(temps_entities::projects::Column::IsDeleted.eq(false))
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

        // A drain/failover job names the exact affected environment. Legacy
        // project-wide template/import jobs continue targeting non-preview
        // environments.
        let environments = match Self::resolve_image_target_environments(
            db.as_ref(),
            job.project_id,
            job.target_environment_id,
        )
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
                "DeployImageRequested: project {} has no matching deployable environment (target_environment_id={:?})",
                job.project_id,
                job.target_environment_id
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
                command: job.command.clone(),
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

            let trigger_context = match job.recovery_of_deployment_id {
                Some(source_deployment_id) => serde_json::json!({
                    "trigger": "failover_recovery",
                    "source": "docker_image",
                    "recovery_of_deployment_id": source_deployment_id,
                }),
                None => serde_json::json!({
                    "trigger": "template_image",
                    "source": "docker_image"
                }),
            };
            let new_deployment = deployments::ActiveModel {
                project_id: Set(project.id),
                environment_id: Set(environment.id),
                slug: Set(deployment_slug),
                state: Set("pending".to_string()),
                metadata: Set(Some(metadata)),
                context_vars: Set(Some(trigger_context)),
                image_name: Set(Some(job.image_ref.clone())),
                deployment_config: Set(deployment_config_snapshot),
                created_at: Set(Utc::now()),
                updated_at: Set(Utc::now()),
                ..Default::default()
            };

            let (deployment, mut post_commit_events) =
                match Self::create_deployment_with_generation_fence(
                    db.as_ref(),
                    project.id,
                    environment.id,
                    job.recovery_of_deployment_id,
                    DeploymentDuplicateKey::Image(job.image_ref.clone()),
                    new_deployment,
                )
                .await
                {
                    Ok(DeploymentCreationOutcome::Created {
                        deployment,
                        cancellation_events,
                    }) => (*deployment, cancellation_events),
                    Ok(DeploymentCreationOutcome::Duplicate {
                        deployment_id,
                        state,
                    }) => {
                        info!(
                            project_id = project.id,
                            environment_id = environment.id,
                            deployment_id,
                            state,
                            image_ref = %job.image_ref,
                            "Image deployment already exists; skipping duplicate"
                        );
                        continue;
                    }
                    Ok(DeploymentCreationOutcome::StaleRecovery {
                        source_deployment_id,
                        current_deployment_id,
                        newer_deployment_id,
                    }) => {
                        info!(
                            project_id = project.id,
                            environment_id = environment.id,
                            source_deployment_id,
                            current_deployment_id = ?current_deployment_id,
                            newer_deployment_id = ?newer_deployment_id,
                            recovery_kind = "image",
                            "Skipping stale failover recovery generation"
                        );
                        continue;
                    }
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

            post_commit_events.push(Job::DeploymentCreated(temps_core::DeploymentCreatedJob {
                deployment_id: deployment.id,
                project_id: project.id,
                environment_id: environment.id,
                environment_name: environment.name.clone(),
                branch: None,
                commit_sha: None,
            }));
            Self::send_post_commit_events(&queue, post_commit_events).await;

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

        // This is the deployment admission boundary. Keep the pending-state
        // and owner-fence checks in the same UPDATE so a concurrent deletion
        // cannot resurrect a cancelled deployment after the gate returns.
        match Self::try_admit_deployment(db.as_ref(), deployment_id).await {
            Ok(true) => {}
            Ok(false) => {
                info!(
                    "Deployment {} was not admitted because it is no longer pending or its owner is being deleted",
                    deployment_id
                );
                return;
            }
            Err(e) => {
                error!(
                    "Failed to atomically admit deployment {}: {}",
                    deployment_id, e
                );
                return;
            }
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

            // Re-read the current deployment state before writing "failed".
            // execute_deployment_workflow already skips its own "failed" write
            // when the deployment is "stopped" (superseded by a concurrent
            // rollback), but this is a second write on the same error path
            // that must honour the same invariant. Without this guard,
            // "stopped" set by stop_environment_containers would be silently
            // overwritten here, making the deployment unavailable for
            // promote/rollback even though it was successfully superseded.
            let terminal_state = deployments::Entity::find_by_id(deployment_id)
                .one(db.as_ref())
                .await
                .ok()
                .flatten()
                .map(|deployment| deployment.state)
                .filter(|state| {
                    matches!(
                        state.as_str(),
                        "cancelled" | "stopped" | "completed" | "failed"
                    )
                });

            if let Some(state) = terminal_state {
                info!(
                    deployment_id,
                    state,
                    "Deployment already reached a terminal state; not overwriting it with failed"
                );
            } else if let Err(update_err) =
                JobProcessorService::update_deployment_status_with_message(
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
        failover_recovery_semaphore: Arc<Semaphore>,
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

        let recovery_of_deployment_id = Self::recovery_source(&deployment);
        let recovery_permit = match Self::acquire_failover_recovery_permit(
            failover_recovery_semaphore,
            recovery_of_deployment_id,
            deployment.project_id,
            Some(deployment.environment_id),
            "gate_recheck",
        )
        .await
        {
            Ok(permit) => permit,
            Err(error) => {
                error!(
                    project_id = deployment.project_id,
                    environment_id = deployment.environment_id,
                    deployment_id = deployment.id,
                    error = %error,
                    "Gate-rechecked failover recovery could not acquire its deployment slot"
                );
                return;
            }
        };

        if let Some(source_deployment_id) = recovery_of_deployment_id {
            match Self::validate_gate_rechecked_recovery(
                db.as_ref(),
                &deployment,
                source_deployment_id,
            )
            .await
            {
                Ok(true) => {}
                Ok(false) => return,
                Err(error) => {
                    error!(
                        project_id = deployment.project_id,
                        environment_id = deployment.environment_id,
                        deployment_id = deployment.id,
                        source_deployment_id,
                        error = %error,
                        "Failed to validate gate-rechecked failover recovery"
                    );
                    return;
                }
            }
        }

        Self::gate_check_then_run(
            &db,
            &workflow_executor,
            &deployment_gate,
            deployment.project_id,
            &environment.name,
            deployment.id,
        )
        .await;
        drop(recovery_permit);
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
            let deleted_preview_id = deleted_preview.id;
            let subdomain = deleted_preview.subdomain.clone();
            let txn = db
                .begin()
                .await
                .map_err(|error| format!("Failed to begin preview restore: {error}"))?;
            if environments::claim_subdomain(&txn, &subdomain, &[deleted_preview_id])
                .await
                .map_err(|error| format!("Failed to claim preview subdomain: {error}"))?
                .is_some()
            {
                return Err(format!("Preview subdomain '{subdomain}' is already in use"));
            }
            let mut active_env: environments::ActiveModel = deleted_preview.into();
            active_env.deleted_at = Set(None);
            active_env.updated_at = Set(chrono::Utc::now());
            active_env.current_deployment_id = Set(None);
            let restored = active_env
                .update(&txn)
                .await
                .map_err(|e| format!("Failed to restore preview environment: {}", e))?;
            txn.commit()
                .await
                .map_err(|error| format!("Failed to commit preview restore: {error}"))?;
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

    let subdomain = format!("{}-preview", project.slug).to_ascii_lowercase();
    let preview_env = environments::ActiveModel {
        name: Set("preview".to_string()),
        slug: Set("preview".to_string()),
        subdomain: Set(subdomain.clone()),
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

    let txn = db
        .begin()
        .await
        .map_err(|error| format!("Failed to begin preview creation: {error}"))?;
    if environments::claim_subdomain(&txn, &subdomain, &[])
        .await
        .map_err(|error| format!("Failed to claim preview subdomain: {error}"))?
        .is_some()
    {
        return Err(format!("Preview subdomain '{subdomain}' is already in use"));
    }
    let created_env = preview_env
        .insert(&txn)
        .await
        .map_err(|e| format!("Failed to create preview environment: {}", e))?;
    txn.commit()
        .await
        .map_err(|error| format!("Failed to commit preview creation: {error}"))?;

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

    let subdomain = format!("{}-{}", project.slug, slugified_branch).to_ascii_lowercase();
    let preview_env = environments::ActiveModel {
        name: Set(slugified_branch.to_string()),
        slug: Set(slugified_branch.to_string()),
        subdomain: Set(subdomain.clone()),
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

    let txn = db
        .begin()
        .await
        .map_err(|error| format!("Failed to begin preview creation: {error}"))?;
    if environments::claim_subdomain(&txn, &subdomain, &[])
        .await
        .map_err(|error| format!("Failed to claim preview subdomain: {error}"))?
        .is_some()
    {
        return Err(format!("Preview subdomain '{subdomain}' is already in use"));
    }
    let created_env = preview_env
        .insert(&txn)
        .await
        .map_err(|e| format!("Failed to create preview environment: {}", e))?;
    txn.commit()
        .await
        .map_err(|error| format!("Failed to commit preview creation: {error}"))?;

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

fn should_skip_git_push_for_auto_deploy(auto_deploy_enabled: bool, manual_trigger: bool) -> bool {
    !auto_deploy_enabled && !manual_trigger
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
        .filter(temps_entities::projects::Column::IsDeleted.eq(false))
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
    use sea_orm::{EntityTrait, PaginatorTrait};

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
        let auto_deploy_enabled = is_automatic_deploy_enabled(
            project.deployment_config.as_ref(),
            environment.deployment_config.as_ref(),
        );
        if should_skip_git_push_for_auto_deploy(auto_deploy_enabled, job.manual_trigger) {
            info!(
                "Skipping push event for project {} environment {} ({}): automatic_deploy is disabled",
                project.id, environment.id, environment.name
            );
            continue;
        } else if job.manual_trigger && !auto_deploy_enabled {
            info!(
                "Manual trigger for project {} environment {} ({}) — bypassing automatic_deploy=false",
                project.id, environment.id, environment.name
            );
        }

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
        let trigger_context = if let Some(source_deployment_id) = job.recovery_of_deployment_id {
            serde_json::json!({
                "trigger": "failover_recovery",
                "source": "git",
                "recovery_of_deployment_id": source_deployment_id,
            })
        } else if is_rollback {
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
            // Manual triggers (redeploy, import) carry no commit — an empty
            // string must not be stored, or checkout would use '' as a ref.
            commit_sha: sea_orm::Set((!job.commit.is_empty()).then(|| job.commit.clone())),
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
            upload_request_id: sea_orm::Set(None),
            created_at: sea_orm::Set(Utc::now()),
            updated_at: sea_orm::Set(Utc::now()),
        };

        let (deployment, mut post_commit_events) =
            match JobProcessorService::create_deployment_with_generation_fence(
                db.as_ref(),
                project.id,
                environment.id,
                job.recovery_of_deployment_id,
                DeploymentDuplicateKey::Commit(job.commit.clone()),
                new_deployment,
            )
            .await
            {
                Ok(DeploymentCreationOutcome::Created {
                    deployment,
                    cancellation_events,
                }) => (*deployment, cancellation_events),
                Ok(DeploymentCreationOutcome::Duplicate {
                    deployment_id,
                    state,
                }) => {
                    info!(
                        project_id = project.id,
                        environment_id = environment.id,
                        deployment_id,
                        state,
                        commit = %job.commit,
                        "Git deployment already exists; skipping duplicate"
                    );
                    continue;
                }
                Ok(DeploymentCreationOutcome::StaleRecovery {
                    source_deployment_id,
                    current_deployment_id,
                    newer_deployment_id,
                }) => {
                    info!(
                        project_id = project.id,
                        environment_id = environment.id,
                        source_deployment_id,
                        current_deployment_id = ?current_deployment_id,
                        newer_deployment_id = ?newer_deployment_id,
                        recovery_kind = "git",
                        "Skipping stale failover recovery generation"
                    );
                    continue;
                }
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

        post_commit_events.push(Job::DeploymentCreated(temps_core::DeploymentCreatedJob {
            deployment_id: deployment.id,
            project_id: project.id,
            environment_id: environment.id,
            environment_name: environment.name.clone(),
            branch: job.branch.clone(),
            commit_sha: (!job.commit.is_empty()).then(|| job.commit.clone()),
        }));
        JobProcessorService::send_post_commit_events(&queue, post_commit_events).await;

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
    transaction: &DatabaseTransaction,
    project_id: i32,
    environment_id: i32,
    current_deployment_id: Option<i32>,
) -> Result<Vec<Job>, JobProcessorError> {
    use temps_entities::deployment_jobs;
    use temps_entities::types::JobStatus;

    let in_flight_query = deployments::Entity::find()
        .filter(deployments::Column::EnvironmentId.eq(environment_id))
        .filter(deployments::Column::ProjectId.eq(project_id))
        .filter(deployments::Column::State.is_in(vec!["pending", "running", "deploying", "built"]));
    let in_flight_query = if let Some(current_deployment_id) = current_deployment_id {
        in_flight_query.filter(deployments::Column::Id.ne(current_deployment_id))
    } else {
        in_flight_query
    };
    let in_flight = in_flight_query.all(transaction).await.map_err(|error| {
        JobProcessorError::DeploymentCreationFailed {
            project_id,
            environment_id,
            operation: "query in-flight generations before superseding",
            reason: error.to_string(),
        }
    })?;

    if in_flight.is_empty() {
        return Ok(Vec::new());
    }

    info!(
        "Cancelling {} in-flight deployment(s) for environment {} (superseded by new push)",
        in_flight.len(),
        environment_id
    );

    let mut events = Vec::with_capacity(in_flight.len());
    for deployment in in_flight {
        let deployment_id = deployment.id;
        let environment_name = deployment.slug.clone();

        // Write cancellation message to any running job logs
        if let Ok(running_jobs) = deployment_jobs::Entity::find()
            .filter(deployment_jobs::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_jobs::Column::Status.eq(JobStatus::Running))
            .all(transaction)
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

        active.update(transaction).await.map_err(|error| {
            JobProcessorError::DeploymentCreationFailed {
                project_id,
                environment_id,
                operation: "cancel superseded generation before creating",
                reason: format!("deployment {deployment_id}: {error}"),
            }
        })?;

        info!(
            "Cancelled deployment {} (superseded) for environment {}",
            deployment_id, environment_id
        );

        // Fire DeploymentCancelled event so notification systems can react
        events.push(Job::DeploymentCancelled(
            temps_core::DeploymentCancelledJob {
                deployment_id,
                project_id,
                environment_id,
                environment_name: environment_name.clone(),
            },
        ));
    }

    Ok(events)
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

    #[tokio::test]
    async fn test_acquire_failover_recovery_permit_ordinary_job_bypasses_occupied_slot() {
        // Arrange: a recovery already owns the sole failover slot.
        let semaphore = Arc::new(Semaphore::new(1));
        let recovery_permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("failover semaphore must be open");

        // Act: an ordinary deployment must not wait for or consume that slot.
        let ordinary_permit = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            JobProcessorService::acquire_failover_recovery_permit(
                semaphore.clone(),
                None,
                42,
                Some(7),
                "git",
            ),
        )
        .await
        .expect("ordinary job must return immediately")
        .expect("ordinary job must not fail");

        // Assert
        assert!(ordinary_permit.is_none());
        assert_eq!(semaphore.available_permits(), 0);
        drop(recovery_permit);
    }

    #[tokio::test]
    async fn test_acquire_failover_recovery_permit_serializes_recovery_jobs() {
        // Arrange: the first recovery acquires the only slot.
        let semaphore = Arc::new(Semaphore::new(1));
        let first_permit = JobProcessorService::acquire_failover_recovery_permit(
            semaphore.clone(),
            Some(99),
            42,
            Some(7),
            "git",
        )
        .await
        .expect("first recovery acquisition must succeed")
        .expect("recovery job must receive a permit");

        let second_acquisition = JobProcessorService::acquire_failover_recovery_permit(
            semaphore.clone(),
            Some(99),
            43,
            Some(8),
            "image",
        );
        tokio::pin!(second_acquisition);

        // Act + Assert: the second recovery remains pending while the first
        // owns the slot.
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(50),
                &mut second_acquisition,
            )
            .await
            .is_err(),
            "a second recovery must wait for the dedicated slot"
        );

        // Act: releasing the first permit unblocks the queued recovery.
        drop(first_permit);
        let second_permit = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            &mut second_acquisition,
        )
        .await
        .expect("second recovery must unblock after the first permit drops")
        .expect("second recovery acquisition must succeed")
        .expect("second recovery must receive the permit");

        // Assert
        assert_eq!(semaphore.available_permits(), 0);
        drop(second_permit);
        assert_eq!(semaphore.available_permits(), 1);
    }

    #[tokio::test]
    async fn test_gate_recheck_recovers_source_marker_and_reacquires_recovery_slot() {
        // Arrange: gate-blocked recovery deployments persist their source id in
        // context_vars while waiting for a later DeploymentGateRecheck job.
        let deployment = deployments::Model {
            id: 100,
            project_id: 42,
            environment_id: 7,
            slug: "gate-blocked-recovery".to_string(),
            state: "pending".to_string(),
            metadata: None,
            deploying_at: None,
            ready_at: None,
            started_at: None,
            finished_at: None,
            context_vars: Some(serde_json::json!({"recovery_of_deployment_id": 99})),
            branch_ref: Some("main".to_string()),
            tag_ref: None,
            commit_sha: Some("abc123".to_string()),
            commit_message: None,
            commit_author: None,
            commit_json: None,
            cancelled_reason: None,
            static_dir_location: None,
            screenshot_location: None,
            image_name: None,
            deployment_config: None,
            promoted_from_deployment_id: None,
            upload_request_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let recovery_source = JobProcessorService::recovery_source(&deployment);
        assert_eq!(recovery_source, Some(99));

        let semaphore = Arc::new(Semaphore::new(1));
        let first_permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("first recovery owns the slot");
        let gate_recheck_acquisition = JobProcessorService::acquire_failover_recovery_permit(
            semaphore.clone(),
            recovery_source,
            deployment.project_id,
            Some(deployment.environment_id),
            "gate_recheck",
        );
        tokio::pin!(gate_recheck_acquisition);

        // Act + Assert: the recheck still behaves as recovery work and waits.
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(50),
            &mut gate_recheck_acquisition,
        )
        .await
        .is_err());

        drop(first_permit);
        let gate_recheck_permit = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            &mut gate_recheck_acquisition,
        )
        .await
        .expect("gate recheck must unblock when the recovery slot is released")
        .expect("gate recheck acquisition must succeed")
        .expect("gate recheck must reacquire the recovery permit");
        drop(gate_recheck_permit);
        assert_eq!(semaphore.available_permits(), 1);
    }

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

    async fn database_integration_tests_available() -> bool {
        std::env::var_os("TEMPS_TEST_DATABASE_URL").is_some()
            || tokio::process::Command::new("docker")
                .arg("info")
                .output()
                .await
                .map(|output| output.status.success())
                .unwrap_or(false)
    }

    fn generation_model(
        project_id: i32,
        environment_id: i32,
        slug: &str,
        state: &str,
        commit: &str,
        created_at: chrono::DateTime<Utc>,
    ) -> deployments::ActiveModel {
        deployments::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(environment_id),
            slug: Set(slug.to_string()),
            state: Set(state.to_string()),
            commit_sha: Set(Some(commit.to_string())),
            metadata: Set(Some(Default::default())),
            created_at: Set(created_at),
            updated_at: Set(created_at),
            ..Default::default()
        }
    }

    async fn set_current_deployment(db: &DbConnection, environment_id: i32, deployment_id: i32) {
        let environment = environments::Entity::find_by_id(environment_id)
            .one(db)
            .await
            .expect("query environment")
            .expect("environment exists");
        let mut active: environments::ActiveModel = environment.into();
        active.current_deployment_id = Set(Some(deployment_id));
        active.update(db).await.expect("set current deployment");
    }

    #[tokio::test]
    async fn test_create_deployment_stale_recovery_preserves_newer_in_flight_generation() {
        if !database_integration_tests_available().await {
            eprintln!("Docker unavailable; skipping stale recovery generation-fence test");
            return;
        }

        // Arrange
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("create test database");
        let db = test_db.connection_arc();
        let (project_id, environment_id) = setup_git_push_test_data(db.as_ref())
            .await
            .expect("seed project and environment");
        let now = Utc::now();
        let source = generation_model(
            project_id,
            environment_id,
            "source",
            "completed",
            "source-commit",
            now,
        )
        .insert(db.as_ref())
        .await
        .expect("insert recovery source");
        set_current_deployment(db.as_ref(), environment_id, source.id).await;
        let newer = generation_model(
            project_id,
            environment_id,
            "manual-newer",
            "pending",
            "manual-commit",
            now + chrono::Duration::seconds(1),
        )
        .insert(db.as_ref())
        .await
        .expect("insert newer manual generation");

        // Act
        let outcome = JobProcessorService::create_deployment_with_generation_fence(
            db.as_ref(),
            project_id,
            environment_id,
            Some(source.id),
            DeploymentDuplicateKey::Commit("recovery-commit".to_string()),
            generation_model(
                project_id,
                environment_id,
                "stale-recovery",
                "pending",
                "recovery-commit",
                now + chrono::Duration::seconds(2),
            ),
        )
        .await
        .expect("stale recovery validation must succeed");

        // Assert
        match outcome {
            DeploymentCreationOutcome::StaleRecovery {
                source_deployment_id,
                current_deployment_id,
                newer_deployment_id,
            } => {
                assert_eq!(source_deployment_id, source.id);
                assert_eq!(current_deployment_id, Some(source.id));
                assert_eq!(newer_deployment_id, Some(newer.id));
            }
            _ => panic!("newer in-flight work must make recovery stale"),
        }
        let rows = deployments::Entity::find()
            .filter(deployments::Column::EnvironmentId.eq(environment_id))
            .all(db.as_ref())
            .await
            .expect("reload deployment generations");
        assert_eq!(rows.len(), 2, "stale recovery must not create a row");
        assert_eq!(
            rows.iter().find(|row| row.id == source.id).unwrap().state,
            "completed"
        );
        assert_eq!(
            rows.iter().find(|row| row.id == newer.id).unwrap().state,
            "pending",
            "stale recovery must not cancel newer work"
        );
    }

    #[tokio::test]
    async fn test_recovery_with_source_commit_creates_new_generation_instead_of_duplicate() {
        if !database_integration_tests_available().await {
            eprintln!("Docker unavailable; skipping recovery commit self-duplicate test");
            return;
        }

        // Arrange: the recovery rebuilds the exact commit of the currently
        // routed running deployment.
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("create test database");
        let db = test_db.connection_arc();
        let (project_id, environment_id) = setup_git_push_test_data(db.as_ref())
            .await
            .expect("seed project and environment");
        let now = Utc::now();
        let source = generation_model(
            project_id,
            environment_id,
            "source",
            "running",
            "same-commit",
            now,
        )
        .insert(db.as_ref())
        .await
        .expect("insert routed recovery source");
        set_current_deployment(db.as_ref(), environment_id, source.id).await;

        // Act
        let outcome = JobProcessorService::create_deployment_with_generation_fence(
            db.as_ref(),
            project_id,
            environment_id,
            Some(source.id),
            DeploymentDuplicateKey::Commit("same-commit".to_string()),
            generation_model(
                project_id,
                environment_id,
                "recovery",
                "pending",
                "same-commit",
                now,
            ),
        )
        .await
        .expect("create recovery generation");

        // Assert
        let recovery = match outcome {
            DeploymentCreationOutcome::Created {
                deployment,
                cancellation_events,
            } => {
                assert!(cancellation_events.is_empty());
                deployment
            }
            DeploymentCreationOutcome::Duplicate { deployment_id, .. } => panic!(
                "routed source deployment {deployment_id} must not self-match duplicate detection"
            ),
            DeploymentCreationOutcome::StaleRecovery { .. } => {
                panic!("current recovery source must not be stale")
            }
        };
        assert_ne!(recovery.id, source.id);
        assert_eq!(recovery.state, "pending");
        assert_eq!(recovery.commit_sha.as_deref(), Some("same-commit"));
        let source = deployments::Entity::find_by_id(source.id)
            .one(db.as_ref())
            .await
            .expect("reload source")
            .expect("source exists");
        assert_eq!(source.state, "running");
    }

    #[tokio::test]
    async fn test_recovery_with_source_image_creates_new_generation_instead_of_duplicate() {
        if !database_integration_tests_available().await {
            eprintln!("Docker unavailable; skipping recovery image self-duplicate test");
            return;
        }

        // Arrange: image recovery intentionally deploys the exact immutable
        // image already associated with the currently routed source.
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("create test database");
        let db = test_db.connection_arc();
        let (project_id, environment_id) = setup_git_push_test_data(db.as_ref())
            .await
            .expect("seed project and environment");
        let now = Utc::now();
        let image_ref = "registry.example/app:immutable";
        let mut source_model = generation_model(
            project_id,
            environment_id,
            "source",
            "running",
            "source-commit",
            now,
        );
        source_model.image_name = Set(Some(image_ref.to_string()));
        let source = source_model
            .insert(db.as_ref())
            .await
            .expect("insert routed recovery source");
        set_current_deployment(db.as_ref(), environment_id, source.id).await;
        let mut recovery_model = generation_model(
            project_id,
            environment_id,
            "recovery",
            "pending",
            "recovery-commit",
            now,
        );
        recovery_model.image_name = Set(Some(image_ref.to_string()));

        // Act
        let outcome = JobProcessorService::create_deployment_with_generation_fence(
            db.as_ref(),
            project_id,
            environment_id,
            Some(source.id),
            DeploymentDuplicateKey::Image(image_ref.to_string()),
            recovery_model,
        )
        .await
        .expect("create image recovery generation");

        // Assert
        let recovery = match outcome {
            DeploymentCreationOutcome::Created {
                deployment,
                cancellation_events,
            } => {
                assert!(cancellation_events.is_empty());
                deployment
            }
            DeploymentCreationOutcome::Duplicate { deployment_id, .. } => panic!(
                "routed source deployment {deployment_id} must not self-match duplicate detection"
            ),
            DeploymentCreationOutcome::StaleRecovery { .. } => {
                panic!("current recovery source must not be stale")
            }
        };
        assert_ne!(recovery.id, source.id);
        assert_eq!(recovery.state, "pending");
        assert_eq!(recovery.image_name.as_deref(), Some(image_ref));
        let source = deployments::Entity::find_by_id(source.id)
            .one(db.as_ref())
            .await
            .expect("reload source")
            .expect("source exists");
        assert_eq!(source.state, "running");
    }

    #[tokio::test]
    async fn test_create_deployment_manual_generation_supersedes_recovery_normally() {
        if !database_integration_tests_available().await {
            eprintln!("Docker unavailable; skipping recovery then manual supersession test");
            return;
        }

        // Arrange
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("create test database");
        let db = test_db.connection_arc();
        let (project_id, environment_id) = setup_git_push_test_data(db.as_ref())
            .await
            .expect("seed project and environment");
        let now = Utc::now();
        let source = generation_model(
            project_id,
            environment_id,
            "source",
            "completed",
            "source-commit",
            now,
        )
        .insert(db.as_ref())
        .await
        .expect("insert recovery source");
        set_current_deployment(db.as_ref(), environment_id, source.id).await;

        let recovery = match JobProcessorService::create_deployment_with_generation_fence(
            db.as_ref(),
            project_id,
            environment_id,
            Some(source.id),
            DeploymentDuplicateKey::Commit("recovery-commit".to_string()),
            generation_model(
                project_id,
                environment_id,
                "recovery",
                "pending",
                "recovery-commit",
                now,
            ),
        )
        .await
        .expect("create recovery generation")
        {
            DeploymentCreationOutcome::Created { deployment, .. } => deployment,
            _ => panic!("first recovery must create a generation"),
        };

        // Act: ordinary/manual work participates in the normal newest-wins path.
        let manual = match JobProcessorService::create_deployment_with_generation_fence(
            db.as_ref(),
            project_id,
            environment_id,
            None,
            DeploymentDuplicateKey::Commit("manual-commit".to_string()),
            generation_model(
                project_id,
                environment_id,
                "manual",
                "pending",
                "manual-commit",
                now,
            ),
        )
        .await
        .expect("create manual generation")
        {
            DeploymentCreationOutcome::Created { deployment, .. } => deployment,
            _ => panic!("manual work must create a generation"),
        };

        // Assert
        let recovery = deployments::Entity::find_by_id(recovery.id)
            .one(db.as_ref())
            .await
            .expect("reload recovery")
            .expect("recovery row exists");
        assert_eq!(recovery.state, "cancelled");
        assert_eq!(
            recovery.cancelled_reason.as_deref(),
            Some("Superseded by a newer deployment for this environment")
        );
        let manual = deployments::Entity::find_by_id(manual.id)
            .one(db.as_ref())
            .await
            .expect("reload manual generation")
            .expect("manual generation exists");
        assert_eq!(manual.state, "pending");
    }

    #[tokio::test]
    async fn test_create_deployment_supersession_preserves_current_routed_generation() {
        if !database_integration_tests_available().await {
            eprintln!("Docker unavailable; skipping routed generation supersession test");
            return;
        }

        // Arrange: a running deployment is currently routed while an unrelated
        // pending generation is also in flight.
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("create test database");
        let db = test_db.connection_arc();
        let (project_id, environment_id) = setup_git_push_test_data(db.as_ref())
            .await
            .expect("seed project and environment");
        let now = Utc::now();
        let routed = generation_model(
            project_id,
            environment_id,
            "routed",
            "running",
            "routed-commit",
            now,
        )
        .insert(db.as_ref())
        .await
        .expect("insert routed deployment");
        set_current_deployment(db.as_ref(), environment_id, routed.id).await;
        let obsolete = generation_model(
            project_id,
            environment_id,
            "obsolete-pending",
            "pending",
            "obsolete-commit",
            now + chrono::Duration::seconds(1),
        )
        .insert(db.as_ref())
        .await
        .expect("insert obsolete pending deployment");

        // Act
        let outcome = JobProcessorService::create_deployment_with_generation_fence(
            db.as_ref(),
            project_id,
            environment_id,
            None,
            DeploymentDuplicateKey::Commit("new-manual-commit".to_string()),
            generation_model(
                project_id,
                environment_id,
                "new-manual",
                "pending",
                "new-manual-commit",
                now + chrono::Duration::seconds(2),
            ),
        )
        .await
        .expect("create newest generation");
        let created = match outcome {
            DeploymentCreationOutcome::Created {
                deployment,
                cancellation_events,
            } => {
                assert_eq!(cancellation_events.len(), 1);
                match &cancellation_events[0] {
                    Job::DeploymentCancelled(event) => {
                        assert_eq!(event.deployment_id, obsolete.id)
                    }
                    other => panic!("unexpected cancellation event: {other:?}"),
                }
                deployment
            }
            _ => panic!("ordinary work must create a generation"),
        };

        // Assert: supersession only cancels non-routed in-flight work.
        let routed = deployments::Entity::find_by_id(routed.id)
            .one(db.as_ref())
            .await
            .expect("reload routed deployment")
            .expect("routed deployment exists");
        assert_eq!(routed.state, "running");
        let obsolete = deployments::Entity::find_by_id(obsolete.id)
            .one(db.as_ref())
            .await
            .expect("reload obsolete deployment")
            .expect("obsolete deployment exists");
        assert_eq!(obsolete.state, "cancelled");
        assert_eq!(created.state, "pending");
    }

    #[tokio::test]
    async fn test_shared_generation_insert_makes_later_recovery_stale() {
        if !database_integration_tests_available().await {
            eprintln!("Docker unavailable; skipping shared generation insert ordering test");
            return;
        }

        // Arrange
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("create test database");
        let db = test_db.connection_arc();
        let (project_id, environment_id) = setup_git_push_test_data(db.as_ref())
            .await
            .expect("seed project and environment");
        let now = Utc::now();
        let source = generation_model(
            project_id,
            environment_id,
            "source",
            "completed",
            "source-commit",
            now,
        )
        .insert(db.as_ref())
        .await
        .expect("insert recovery source");
        set_current_deployment(db.as_ref(), environment_id, source.id).await;

        // Act: model a direct/manual or source-drop path using the universal
        // insertion helper before the delayed recovery reaches its fence.
        let direct = super::super::services::insert_deployment_with_generation_lock(
            db.as_ref(),
            project_id,
            environment_id,
            generation_model(
                project_id,
                environment_id,
                "direct-insert",
                "pending",
                "direct-commit",
                now,
            ),
        )
        .await
        .expect("insert direct generation under shared lock");
        let recovery = JobProcessorService::create_deployment_with_generation_fence(
            db.as_ref(),
            project_id,
            environment_id,
            Some(source.id),
            DeploymentDuplicateKey::Commit("recovery-commit".to_string()),
            generation_model(
                project_id,
                environment_id,
                "recovery",
                "pending",
                "recovery-commit",
                now,
            ),
        )
        .await
        .expect("validate delayed recovery");

        // Assert
        match recovery {
            DeploymentCreationOutcome::StaleRecovery {
                source_deployment_id,
                newer_deployment_id,
                ..
            } => {
                assert_eq!(source_deployment_id, source.id);
                assert_eq!(newer_deployment_id, Some(direct.id));
            }
            _ => panic!("direct generation must make delayed recovery stale"),
        }
        let direct = deployments::Entity::find_by_id(direct.id)
            .one(db.as_ref())
            .await
            .expect("reload direct generation")
            .expect("direct generation exists");
        assert_eq!(direct.state, "pending");
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

    #[tokio::test]
    async fn deployment_admission_is_atomic_with_owner_and_state_fences(
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !database_integration_tests_available().await {
            eprintln!("Docker unavailable; skipping deployment admission integration test");
            return Ok(());
        }

        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let (deployment_id, _) = setup_test_data(db.as_ref()).await?;

        assert!(JobProcessorService::try_admit_deployment(db.as_ref(), deployment_id).await?);

        let deployment = deployments::Entity::find_by_id(deployment_id)
            .one(db.as_ref())
            .await?
            .expect("deployment should exist");
        assert_eq!(deployment.state, "running");
        assert!(
            deployment.started_at.is_some(),
            "admitting a deployment must record when it started"
        );
        assert_eq!(
            deployment.started_at,
            Some(deployment.updated_at),
            "the running transition timestamps must come from the same atomic update"
        );

        let mut cancelled: deployments::ActiveModel = deployment.into();
        cancelled.state = Set("cancelled".to_string());
        let cancelled = cancelled.update(db.as_ref()).await?;
        assert!(
            !JobProcessorService::try_admit_deployment(db.as_ref(), cancelled.id).await?,
            "a gate recheck must not resurrect a cancelled deployment"
        );

        let pending = deployments::ActiveModel {
            project_id: Set(cancelled.project_id),
            environment_id: Set(cancelled.environment_id),
            slug: Set("owner-fenced-deployment".to_string()),
            state: Set("pending".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        let project = temps_entities::projects::Entity::find_by_id(cancelled.project_id)
            .one(db.as_ref())
            .await?
            .expect("project should exist");
        let mut deleting: temps_entities::projects::ActiveModel = project.into();
        deleting.is_deleted = Set(true);
        deleting.update(db.as_ref()).await?;

        assert!(
            !JobProcessorService::try_admit_deployment(db.as_ref(), pending.id).await?,
            "a deployment owned by a deleting project must not be admitted"
        );
        let denied = deployments::Entity::find_by_id(pending.id)
            .one(db.as_ref())
            .await?
            .expect("denied deployment should remain for history");
        assert_eq!(denied.state, "cancelled");
        assert_eq!(
            denied.cancelled_reason.as_deref(),
            Some("Deployment owner is being deleted")
        );
        assert!(
            denied.started_at.is_none(),
            "a deployment denied admission must not receive a start timestamp"
        );

        Ok(())
    }

    #[tokio::test]
    async fn image_deployment_target_is_scoped_to_one_environment(
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !database_integration_tests_available().await {
            eprintln!("Docker unavailable; skipping image target integration test");
            return Ok(());
        }

        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();
        let project = temps_entities::projects::ActiveModel {
            name: Set("Image Target Project".to_string()),
            slug: Set("image-target-project".to_string()),
            repo_owner: Set("temps-e2e".to_string()),
            repo_name: Set("image-target-project".to_string()),
            preset: Set(Preset::Dockerfile),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        let make_environment = |name: &str, slug: &str| temps_entities::environments::ActiveModel {
            project_id: Set(project.id),
            name: Set(name.to_string()),
            slug: Set(slug.to_string()),
            host: Set(format!("{slug}.example.com")),
            upstreams: Set(UpstreamList::default()),
            subdomain: Set(format!("{slug}.example.com")),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let production = make_environment("Production", "production")
            .insert(db.as_ref())
            .await?;
        let staging = make_environment("Staging", "staging")
            .insert(db.as_ref())
            .await?;

        let targeted = JobProcessorService::resolve_image_target_environments(
            db.as_ref(),
            project.id,
            Some(staging.id),
        )
        .await?;
        assert_eq!(targeted.len(), 1);
        assert_eq!(targeted[0].id, staging.id);

        let project_wide =
            JobProcessorService::resolve_image_target_environments(db.as_ref(), project.id, None)
                .await?;
        assert_eq!(project_wide.len(), 2);
        assert!(project_wide.iter().any(|env| env.id == production.id));
        assert!(project_wide.iter().any(|env| env.id == staging.id));
        Ok(())
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
            // Enable vulnerability scanning so the scan_vulnerabilities job is included
            // in the workflow plan (the assertion below checks for it by name).
            vulnerability_scanning_enabled: Set(true),
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
            recovery_of_deployment_id: None,
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

        // Verify jobs were created (nextjs project should create 10 jobs including
        // persist_static_assets, configure_crons, configure_metric_alerts,
        // configure_agents, scan_vulnerabilities, and capture_source_maps)
        let job_ids: Vec<String> = jobs.iter().map(|j| j.job_id.clone()).collect();
        assert_eq!(
            jobs.len(),
            10,
            "Expected 10 jobs but got {}: {:?}",
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
        assert!(job_ids.contains(&"configure_metric_alerts".to_string()));
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
            error_source_context_enabled: Set(false),
            error_source_root: Set(None),
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
            recovery_of_deployment_id: None,
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
    fn webhook_push_is_skipped_when_auto_deploy_disabled_even_for_first_deploy() {
        assert!(should_skip_git_push_for_auto_deploy(false, false));
    }

    #[test]
    fn manual_trigger_bypasses_auto_deploy_disabled() {
        assert!(!should_skip_git_push_for_auto_deploy(false, true));
    }

    #[test]
    fn auto_deploy_enabled_allows_webhook_push() {
        assert!(!should_skip_git_push_for_auto_deploy(true, false));
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
