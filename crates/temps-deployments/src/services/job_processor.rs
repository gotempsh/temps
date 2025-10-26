use crate::services::workflow_execution_service::WorkflowExecutionService;
use crate::services::workflow_planner::WorkflowPlanner;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serde_json;
use std::collections::HashMap;
use std::sync::Arc;
use temps_core::{Job, JobReceiver};
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
    workflow_planner: Arc<WorkflowPlanner>,
    workflow_executor: Arc<WorkflowExecutionService>,
    git_provider_manager: Arc<temps_git::GitProviderManager>,
}

impl JobProcessorService {
    pub fn new(
        db: Arc<DbConnection>,
        job_receiver: Box<dyn JobReceiver>,
        workflow_executor: Arc<WorkflowExecutionService>,
        workflow_planner: Arc<WorkflowPlanner>,
        git_provider_manager: Arc<temps_git::GitProviderManager>,
    ) -> Self {
        Self {
            db,
            job_receiver,
            workflow_planner,
            workflow_executor,
            git_provider_manager,
        }
    }

    pub fn with_external_service_manager(
        db: Arc<DbConnection>,
        job_receiver: Box<dyn JobReceiver>,
        workflow_executor: Arc<WorkflowExecutionService>,
        workflow_planner: Arc<WorkflowPlanner>,
        git_provider_manager: Arc<temps_git::GitProviderManager>,
    ) -> Self {
        Self {
            db,
            job_receiver,
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

                            // Spawn a task to handle the job asynchronously
                            tokio::spawn(async move {
                                debug!("Starting async processing for GitPushEvent job");
                                Self::process_git_push_event_job(
                                    workflow_planner,
                                    workflow_executor,
                                    db,
                                    git_provider_manager,
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
        job: temps_core::GitPushEventJob,
    ) {
        process_git_push_event(
            workflow_planner,
            workflow_executor,
            db,
            git_provider_manager,
            job,
        )
        .await;
    }
}

// Extracted free function for testing
async fn process_git_push_event(
    workflow_planner: Arc<WorkflowPlanner>,
    workflow_executor: Arc<WorkflowExecutionService>,
    db: Arc<DbConnection>,
    git_provider_manager: Arc<temps_git::GitProviderManager>,
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

    // Find the default environment for this project (usually 'production' or the first one)
    let environment = match temps_entities::environments::Entity::find()
        .filter(temps_entities::environments::Column::ProjectId.eq(project.id))
        .one(db.as_ref())
        .await
    {
        Ok(Some(environment)) => environment,
        Ok(None) => {
            error!("No environment found for project {}", project.id);
            return;
        }
        Err(e) => {
            error!(
                "Database error while finding environment for project {}: {}",
                project.id, e
            );
            return;
        }
    };

    // Create deployment record directly (no more pipeline)
    // Note: Previous deployment teardown happens AFTER this deployment succeeds (zero-downtime)
    use chrono::Utc;

    // Get the next deployment number for this project
    use sea_orm::{EntityTrait, PaginatorTrait};
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

    // Generate URL as {project_slug}-{deployment_number}
    let env_slug = format!("{}-{}", project.slug, deployment_number);

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
            metadata: Set(None),
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
            metadata: Set(None),
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

        // Create project without git info
        use temps_entities::{environments, projects};
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project-no-git".to_string()),
            repo_owner: Set("test-owner".to_string()),
            repo_name: Set("test-repo".to_string()),
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
            metadata: Set(None),
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
            metadata: Set(None),
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
}
