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

pub struct JobProcessorService {
    db: Arc<DbConnection>,
    job_receiver: Box<dyn JobReceiver>,
    queue: Arc<dyn JobQueue>,
    workflow_planner: Arc<WorkflowPlanner>,
    workflow_executor: Arc<WorkflowExecutionService>,
    git_provider_manager: Arc<temps_git::GitProviderManager>,
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
        }
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

                            // Spawn a task to handle the job asynchronously
                            tokio::spawn(async move {
                                debug!("Starting async processing for GitPushEvent job");
                                Self::process_git_push_event_job(
                                    workflow_planner,
                                    workflow_executor,
                                    db,
                                    git_provider_manager,
                                    queue,
                                    git_push_job,
                                )
                                .await;
                                debug!("Completed async processing for GitPushEvent job");
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

    async fn update_deployment_status(
        db: &DbConnection,
        deployment_id: i32,
        status: PipelineStatus,
    ) -> Result<(), JobProcessorError> {
        Self::update_deployment_status_with_message(db, deployment_id, status, None).await
    }

    async fn update_deployment_status_with_message(
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

    async fn process_git_push_event_job(
        workflow_planner: Arc<WorkflowPlanner>,
        workflow_executor: Arc<WorkflowExecutionService>,
        db: Arc<DbConnection>,
        git_provider_manager: Arc<temps_git::GitProviderManager>,
        queue: Arc<dyn JobQueue>,
        job: temps_core::GitPushEventJob,
    ) {
        process_git_push_event(
            workflow_planner,
            workflow_executor,
            db,
            git_provider_manager,
            queue,
            job,
        )
        .await;
    }
}

/// Find environment matching the branch, or create/use preview environment
async fn find_or_create_environment_for_branch(
    db: Arc<DbConnection>,
    project: &temps_entities::projects::Model,
    branch: Option<&str>,
) -> Result<temps_entities::environments::Model, String> {
    use temps_entities::environments;

    // If no branch specified, find first environment
    let Some(branch_name) = branch else {
        return environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project.id))
            .filter(environments::Column::DeletedAt.is_null())
            .one(db.as_ref())
            .await
            .map_err(|e| format!("Database error finding environment: {}", e))?
            .ok_or_else(|| "No environment found for project".to_string());
    };

    info!(
        "Looking for environment matching branch '{}' for project {}",
        branch_name, project.id
    );

    // Try to find environment with matching branch
    if let Some(matched_env) = environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(project.id))
        .filter(environments::Column::Branch.eq(branch_name))
        .filter(environments::Column::DeletedAt.is_null())
        .one(db.as_ref())
        .await
        .map_err(|e| format!("Database error finding branch environment: {}", e))?
    {
        info!(
            "Found environment '{}' matching branch '{}'",
            matched_env.name, branch_name
        );
        return Ok(matched_env);
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
            return Ok(existing_preview);
        }

        // Create new preview environment for this branch
        return create_preview_environment(db, project, branch_name, &slugified_branch).await;
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
        return Ok(preview_env);
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

    Ok(created_env)
}

/// Create a new preview environment for a specific branch
async fn create_preview_environment(
    db: Arc<DbConnection>,
    project: &temps_entities::projects::Model,
    branch_name: &str,
    slugified_branch: &str,
) -> Result<temps_entities::environments::Model, String> {
    use chrono::Utc;
    use temps_entities::{environments, upstream_config::UpstreamList};

    info!(
        "Creating preview environment '{}' for branch '{}' in project {}",
        slugified_branch, branch_name, project.id
    );

    let preview_env = environments::ActiveModel {
        name: Set(slugified_branch.to_string()),
        slug: Set(slugified_branch.to_string()),
        subdomain: Set(format!("{}-{}", project.slug, slugified_branch)),
        host: Set(String::new()),
        branch: Set(Some(branch_name.to_string())), // Link to specific branch (used for both deployment and tracking)
        project_id: Set(project.id),
        upstreams: Set(UpstreamList::default()),
        deployment_config: Set(None), // Inherits from project or template
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

// Extracted free function for testing
async fn process_git_push_event(
    workflow_planner: Arc<WorkflowPlanner>,
    workflow_executor: Arc<WorkflowExecutionService>,
    db: Arc<DbConnection>,
    git_provider_manager: Arc<temps_git::GitProviderManager>,
    queue: Arc<dyn JobQueue>,
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

    // Find environment matching the branch, or fallback to preview environment
    let environment =
        match find_or_create_environment_for_branch(db.clone(), &project, job.branch.as_deref())
            .await
        {
            Ok(env) => env,
            Err(e) => {
                error!(
                    "Failed to find or create environment for project {}: {}",
                    project.id, e
                );
                return;
            }
        };

    // Check for duplicate deployment (same project, environment, and commit)
    // This prevents duplicate deployments from being created if:
    // - Multiple webhook URLs are configured in GitHub (both /webhook/git/github/events and /webhook/source/github/events)
    // - GitHub sends duplicate webhooks
    // - Race condition between concurrent push events
    use sea_orm::{EntityTrait, PaginatorTrait, QueryOrder};
    let existing_deployment = deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(project.id))
        .filter(deployments::Column::EnvironmentId.eq(environment.id))
        .filter(deployments::Column::CommitSha.eq(&job.commit))
        .filter(deployments::Column::State.is_in(vec!["pending", "running", "deploying", "ready"]))
        .order_by_desc(deployments::Column::CreatedAt)
        .one(db.as_ref())
        .await;

    if let Ok(Some(existing)) = existing_deployment {
        info!(
            "Deployment already exists for project {} environment {} commit {} (deployment #{}, state: {}). Skipping duplicate.",
            project.id, environment.id, job.commit, existing.id, existing.state
        );
        return;
    }

    // Create deployment record directly (no more pipeline)
    // Note: Previous deployment teardown happens AFTER this deployment succeeds (zero-downtime)
    use chrono::Utc;

    // Get the next deployment number for this project
    let paginator = deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(project.id))
        .paginate(db.as_ref(), 1);

    let deployment_count = match paginator.num_items().await {
        Ok(count) => count,
        Err(e) => {
            error!(
                "Failed to count deployments for project {}: {}",
                project.id, e
            );
            return;
        }
    };
    let deployment_number = deployment_count + 1;

    // Fetch commit information from Git provider
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

    // Generate URL/slug based on environment type
    let env_slug = if environment.is_preview {
        // For preview deployments, include branch name in slug for unique URLs
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
        // For named environments (production, staging, etc.), use standard format
        format!("{}-{}", project.slug, deployment_number)
    };

    // Get the effective deployment configuration by merging project and environment configs
    let merged_config = if let Some(project_config) = &project.deployment_config {
        if let Some(env_config) = &environment.deployment_config {
            Some(project_config.merge(env_config))
        } else {
            Some(project_config.clone())
        }
    } else {
        environment.deployment_config.clone()
    };

    // Create deployment config snapshot
    // Note: Environment variables will be populated during workflow execution
    // We store an empty map here and the workflow will update it with actual values
    let deployment_config_snapshot =
        merged_config.map(|config| DeploymentConfigSnapshot::from_config(&config, HashMap::new()));

    // Create typed deployment metadata
    let deployment_metadata = DeploymentMetadata {
        git_push_event: Some(GitPushEvent {
            owner: job.owner.clone(),
            repo: job.repo.clone(),
            branch: job.branch.clone().unwrap_or_default(),
            commit: job.commit.clone(),
        }),
        ..Default::default()
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
        started_at: sea_orm::Set(None),
        finished_at: sea_orm::Set(None),
        context_vars: sea_orm::Set(Some(serde_json::json!({
            "trigger": "git_push",
            "source": "webhook"
        }))),
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
                "Failed to create deployment for project {}: {}",
                project.id, e
            );
            return;
        }
    };

    info!(
        "Created deployment {} for project {} from GitPushEvent",
        deployment.id, project.id
    );

    // Fire DeploymentCreated event to queue
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
    } else {
        debug!(
            "Sent DeploymentCreated event for deployment {}",
            deployment.id
        );
    }

    // Update project's last_deployment timestamp
    let mut active_project: temps_entities::projects::ActiveModel = project.clone().into();
    active_project.last_deployment = sea_orm::Set(Some(Utc::now()));
    if let Err(e) = active_project.update(db.as_ref()).await {
        error!(
            "Failed to update last_deployment for project {}: {}",
            project.id, e
        );
    } else {
        debug!(
            "Updated last_deployment timestamp for project {}",
            project.id
        );
    }

    // Create jobs for this deployment using the workflow planner
    let create_jobs_result = workflow_planner.create_deployment_jobs(deployment.id).await;
    let deployment_id = deployment.id; // Extract deployment_id before match

    // Handle result immediately
    match create_jobs_result {
        Ok(created_jobs) => {
            let job_count = created_jobs.len();
            info!(
                "Created {} jobs for deployment {} from GitPushEvent",
                job_count, deployment_id
            );

            // Update deployment status to Running before executing workflow
            match JobProcessorService::update_deployment_status(
                &db,
                deployment_id,
                PipelineStatus::Running,
            )
            .await
            {
                Ok(_) => info!("Updated deployment {} status to Running", deployment_id),
                Err(e) => {
                    error!("Failed to update deployment status to Running: {}", e);
                    return;
                }
            }

            // Execute the workflow
            info!("Executing workflow for deployment {}", deployment_id);
            match workflow_executor
                .execute_deployment_workflow(deployment_id)
                .await
            {
                Ok(_) => {
                    info!(
                        "Workflow execution completed for deployment {}",
                        deployment_id
                    );
                }
                Err(e) => {
                    let error_message = format!("{}", e);
                    error!(
                        "Workflow execution failed for deployment {}: {}",
                        deployment_id, error_message
                    );

                    // Mark deployment as failed with error message
                    if let Err(update_err) =
                        JobProcessorService::update_deployment_status_with_message(
                            &db,
                            deployment_id,
                            PipelineStatus::Failed,
                            Some(error_message),
                        )
                        .await
                    {
                        error!("Failed to update deployment status: {}", update_err);
                    } else {
                        debug!("Updated deployment {} status to failed", deployment_id);
                    }
                }
            }
        }
        Err(job_error) => {
            // Convert error to string immediately to avoid Send issues
            let error_message = format!("{}", job_error);
            // Drop the error explicitly before any await
            std::mem::drop(job_error);

            error!(
                "Failed to create jobs for deployment {}: {}",
                deployment_id, error_message
            );

            // Mark deployment as failed with error message
            if let Err(update_err) = JobProcessorService::update_deployment_status_with_message(
                &db,
                deployment_id,
                PipelineStatus::Failed,
                Some(error_message),
            )
            .await
            {
                error!("Failed to update deployment status: {}", update_err);
            } else {
                debug!("Updated deployment {} status to failed", deployment_id);
            }
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
            encryption_service,
            docker,
        ));

        let workflow_planner = Arc::new(WorkflowPlanner::new(
            db.clone(),
            Arc::new(LogService::new(std::env::temp_dir())),
            external_service_manager,
            config_service,
            dsn_service,
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

        // Verify jobs were created (nextjs project should create 5 jobs including configure_crons)
        let job_ids: Vec<String> = jobs.iter().map(|j| j.job_id.clone()).collect();
        assert_eq!(
            jobs.len(),
            5,
            "Expected 5 jobs but got {}: {:?}",
            jobs.len(),
            job_ids
        );

        // Verify all expected jobs are present
        assert!(job_ids.contains(&"download_repo".to_string()));
        assert!(job_ids.contains(&"build_image".to_string()));
        assert!(job_ids.contains(&"deploy_container".to_string()));
        assert!(job_ids.contains(&"mark_deployment_complete".to_string()));
        assert!(job_ids.contains(&"configure_crons".to_string()));

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
            encryption_service,
            docker,
        ));

        let workflow_planner = Arc::new(WorkflowPlanner::new(
            db.clone(),
            Arc::new(LogService::new(std::env::temp_dir())),
            external_service_manager,
            config_service,
            dsn_service,
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

        // Should create 3 jobs (no download_repo): build_image, deploy_container, mark_deployment_complete
        let job_ids: Vec<String> = jobs.iter().map(|j| j.job_id.clone()).collect();
        assert_eq!(
            jobs.len(),
            3,
            "Expected 3 jobs but got {}: {:?}",
            jobs.len(),
            job_ids
        );

        assert!(!job_ids.contains(&"download_repo".to_string()));
        assert!(job_ids.contains(&"build_image".to_string()));
        assert!(job_ids.contains(&"deploy_container".to_string()));
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
            encryption_service,
            docker,
        ));

        let workflow_planner = Arc::new(WorkflowPlanner::new(
            db.clone(),
            Arc::new(LogService::new(std::env::temp_dir())),
            external_service_manager,
            config_service,
            dsn_service,
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
        let found_env =
            find_or_create_environment_for_branch(db.clone(), &project, Some("main")).await?;

        assert_eq!(found_env.id, production_env.id);
        assert_eq!(found_env.name, "Production");
        assert_eq!(found_env.branch, Some("main".to_string()));

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
        let found_env =
            find_or_create_environment_for_branch(db.clone(), &project, Some("feature-auth"))
                .await?;

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
        let found_env =
            find_or_create_environment_for_branch(db.clone(), &project, Some("feature-xyz"))
                .await?;

        // Verify preview environment was created
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
        let env1 =
            find_or_create_environment_for_branch(db.clone(), &project, Some("feature-auth"))
                .await?;

        // Find environment for second feature branch (reuses preview)
        let env2 =
            find_or_create_environment_for_branch(db.clone(), &project, Some("feature-payments"))
                .await?;

        // Find environment for third feature branch (reuses preview)
        let env3 =
            find_or_create_environment_for_branch(db.clone(), &project, Some("bugfix-login"))
                .await?;

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
        let found_env = find_or_create_environment_for_branch(db.clone(), &project, None).await?;

        // Should return first environment (by database order)
        assert_eq!(found_env.id, env1.id);

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
        let found_env =
            find_or_create_environment_for_branch(db.clone(), &project, Some("feature-test"))
                .await?;

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
}
