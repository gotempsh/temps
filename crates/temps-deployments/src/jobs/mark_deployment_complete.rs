//! Mark Deployment Complete Job
//!
//! A synthetic job that marks the deployment as complete and updates the environment.
//! This job runs after all core deployment jobs (download, build, deploy) succeed.
//! Optional jobs (screenshots, crons) depend on this job, ensuring the deployment
//! is live before they run.

use async_trait::async_trait;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::{
    Job, JobQueue, JobResult, UtcDateTime, WorkflowContext, WorkflowError, WorkflowTask,
};
use temps_database::DbConnection;
use temps_entities::{deployment_containers, deployments, environments};
use temps_logs::{LogLevel, LogService};
use tracing::{debug, info};

/// Output from MarkDeploymentCompleteJob
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkCompleteOutput {
    pub completed_at: UtcDateTime,
    pub environment_id: i32,
}

/// Job that marks a deployment as complete and updates the environment
pub struct MarkDeploymentCompleteJob {
    job_id: String,
    deployment_id: i32,
    db: Arc<DbConnection>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
    container_deployer: Arc<dyn temps_deployer::ContainerDeployer>,
    queue: Arc<dyn JobQueue>,
}

impl std::fmt::Debug for MarkDeploymentCompleteJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MarkDeploymentCompleteJob")
            .field("job_id", &self.job_id)
            .field("deployment_id", &self.deployment_id)
            .finish()
    }
}

impl MarkDeploymentCompleteJob {
    pub fn new(
        job_id: String,
        deployment_id: i32,
        db: Arc<DbConnection>,
        container_deployer: Arc<dyn temps_deployer::ContainerDeployer>,
        queue: Arc<dyn JobQueue>,
    ) -> Self {
        Self {
            job_id,
            deployment_id,
            db,
            log_id: None,
            log_service: None,
            container_deployer,
            queue,
        }
    }

    pub fn with_log_id(mut self, log_id: String) -> Self {
        self.log_id = Some(log_id);
        self
    }

    pub fn with_log_service(mut self, log_service: Arc<LogService>) -> Self {
        self.log_service = Some(log_service);
        self
    }

    /// Write log message to job-specific log file
    async fn log(&self, message: String) -> Result<(), WorkflowError> {
        // Detect log level from message content/emojis
        let level = Self::detect_log_level(&message);

        if let (Some(ref log_id), Some(ref log_service)) = (&self.log_id, &self.log_service) {
            log_service
                .append_structured_log(log_id, level, message.clone())
                .await
                .map_err(|e| WorkflowError::Other(format!("Failed to write log: {}", e)))?;
        }
        Ok(())
    }

    /// Detect log level from message content
    fn detect_log_level(message: &str) -> LogLevel {
        if message.contains("✅")
            || message.contains("🎉")
            || message.contains("Complete")
            || message.contains("success")
        {
            LogLevel::Success
        } else if message.contains("❌")
            || message.contains("⚠️")
            || message.contains("Failed")
            || message.contains("Error")
            || message.contains("error")
        {
            LogLevel::Error
        } else if message.contains("⏳")
            || message.contains("🔄")
            || message.contains("🛑")
            || message.contains("Waiting")
            || message.contains("warning")
            || message.contains("Checking")
            || message.contains("Cancelling")
        {
            LogLevel::Warning
        } else {
            LogLevel::Info
        }
    }

    /// Mark deployment as complete and update environment
    /// Also updates deployment with workflow outputs (image info, container info)
    async fn mark_complete(
        &self,
        context: &WorkflowContext,
    ) -> Result<MarkCompleteOutput, WorkflowError> {
        self.log("Marking deployment as complete...".to_string())
            .await?;

        // Get deployment
        let deployment = deployments::Entity::find_by_id(self.deployment_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to find deployment: {}", e))
            })?
            .ok_or_else(|| {
                WorkflowError::JobExecutionFailed(format!(
                    "Deployment {} not found",
                    self.deployment_id
                ))
            })?;

        let environment_id = deployment.environment_id;

        // Update deployment with workflow outputs
        let mut active_deployment: deployments::ActiveModel = deployment.clone().into();

        // Extract image info from build job output
        if let Ok(Some(image_tag)) = context.get_output::<String>("build_image", "image_tag") {
            debug!("Setting deployment image_name to: {}", image_tag);
            active_deployment.image_name = Set(Some(image_tag));
        }

        // Extract static_dir_location from deploy_static job output
        if let Ok(Some(static_dir)) =
            context.get_output::<String>("deploy_static", "static_dir_location")
        {
            debug!("Setting deployment static_dir_location to: {}", static_dir);
            self.log(format!("📁 Static files location: {}", static_dir))
                .await?;
            active_deployment.static_dir_location = Set(Some(static_dir));
        }

        // Extract container info from deploy job output and create deployment_container records
        // Try to get container_ids array first (for multi-replica deployments)
        let container_ids = context
            .get_output::<Vec<String>>("deploy_container", "container_ids")
            .ok()
            .flatten()
            .or_else(|| {
                // Fallback to single container_id for backward compatibility
                context
                    .get_output::<String>("deploy_container", "container_id")
                    .ok()
                    .flatten()
                    .map(|id| vec![id])
            });

        let host_ports = context
            .get_output::<Vec<u16>>("deploy_container", "host_ports")
            .ok()
            .flatten()
            .or_else(|| {
                // Fallback to single host_port for backward compatibility
                context
                    .get_output::<u16>("deploy_container", "host_port")
                    .ok()
                    .flatten()
                    .map(|port| vec![port])
            });

        if let Some(container_ids) = container_ids {
            let now = chrono::Utc::now();
            let container_port = context
                .get_output::<i32>("deploy_container", "container_port")
                .ok()
                .flatten()
                .unwrap_or(8080);

            // Create a deployment_container record for each container
            for (index, container_id) in container_ids.iter().enumerate() {
                let container_name = if container_ids.len() > 1 {
                    // Multi-replica: append index to name
                    context
                        .get_output::<String>("deploy_container", "container_name")
                        .ok()
                        .flatten()
                        .map(|name| format!("{}-{}", name, index + 1))
                        .unwrap_or_else(|| {
                            format!("container-{}-{}", self.deployment_id, index + 1)
                        })
                } else {
                    // Single replica: use original name
                    context
                        .get_output::<String>("deploy_container", "container_name")
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| format!("container-{}", self.deployment_id))
                };

                let host_port = host_ports
                    .as_ref()
                    .and_then(|ports| ports.get(index).map(|&p| p as i32));

                // Create deployment_container record
                let deployment_container = deployment_containers::ActiveModel {
                    deployment_id: Set(self.deployment_id),
                    container_id: Set(container_id.clone()),
                    container_name: Set(container_name.clone()),
                    container_port: Set(container_port),
                    host_port: Set(host_port),
                    image_name: Set(match &active_deployment.image_name {
                        sea_orm::ActiveValue::Set(v) => v.clone(),
                        sea_orm::ActiveValue::Unchanged(v) => v.clone(),
                        _ => None,
                    }),
                    status: Set(Some("running".to_string())),
                    created_at: Set(now),
                    deployed_at: Set(now),
                    ready_at: Set(Some(now)),
                    deleted_at: Set(None),
                    ..Default::default()
                };

                deployment_container
                    .insert(self.db.as_ref())
                    .await
                    .map_err(|e| {
                        WorkflowError::JobExecutionFailed(format!(
                            "Failed to create deployment_container: {}",
                            e
                        ))
                    })?;

                info!(
                    "Created deployment_container record for container {} (replica {}/{})",
                    container_id,
                    index + 1,
                    container_ids.len()
                );
                self.log(format!(
                    "Container {} registered (replica {}/{})",
                    container_id,
                    index + 1,
                    container_ids.len()
                ))
                .await?;
            }
        }

        // Update deployment status to completed
        active_deployment.state = Set("completed".to_string());
        let now = chrono::Utc::now();
        active_deployment.finished_at = Set(Some(now));
        active_deployment.updated_at = Set(now);

        active_deployment
            .update(self.db.as_ref())
            .await
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to update deployment: {}", e))
            })?;

        info!("Deployment {} marked as complete", self.deployment_id);
        self.log(format!(
            "Deployment {} status updated to Completed",
            self.deployment_id
        ))
        .await?;

        // Update environment's current_deployment_id
        let environment = environments::Entity::find_by_id(environment_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to find environment: {}", e))
            })?
            .ok_or_else(|| {
                WorkflowError::JobExecutionFailed(format!(
                    "Environment {} not found",
                    environment_id
                ))
            })?;

        let mut active_environment: environments::ActiveModel = environment.into();
        active_environment.current_deployment_id = Set(Some(self.deployment_id));

        active_environment
            .clone()
            .update(self.db.as_ref())
            .await
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to update environment: {}", e))
            })?;

        info!(
            "Environment {} current_deployment_id updated to {}",
            environment_id, self.deployment_id
        );
        self.log(format!(
            "Environment {} now points to deployment {}",
            environment_id, self.deployment_id
        ))
        .await?;

        self.log("Deployment is now LIVE and ready for traffic!".to_string())
            .await?;

        // Emit DeploymentSucceeded event
        // Get deployment URL from environment
        let url = if !active_environment.host.as_ref().is_empty() {
            Some(format!("https://{}", active_environment.host.as_ref()))
        } else {
            None
        };

        let event = Job::DeploymentSucceeded(temps_core::DeploymentSucceededJob {
            deployment_id: self.deployment_id,
            project_id: deployment.project_id,
            environment_id,
            environment_name: active_environment.name.as_ref().clone(),
            commit_sha: deployment.commit_sha.clone(),
            url,
        });

        if let Err(e) = self.queue.send(event).await {
            self.log(format!("Failed to send DeploymentSucceeded event: {}", e))
                .await?;
        } else {
            debug!(
                "Sent DeploymentSucceeded event for deployment {}",
                self.deployment_id
            );
        }

        // Cancel and teardown all previous deployments for this environment
        self.cancel_previous_deployments(environment_id).await;

        Ok(MarkCompleteOutput {
            completed_at: now,
            environment_id,
        })
    }

    /// Teardown all running/pending deployments for the same environment
    /// This ensures only one active deployment per environment
    /// Note: Deployment state is NOT changed - the is_current flag indicates which deployment is active
    async fn cancel_previous_deployments(&self, environment_id: i32) {
        use sea_orm::Set;

        self.log("Checking for previous deployments to teardown...".to_string())
            .await
            .ok();

        // Find all running or pending deployments for this environment (excluding the new one)
        // Note: "failed" deployments are intentionally excluded to preserve error history
        let previous_deployments = match deployments::Entity::find()
            .filter(deployments::Column::EnvironmentId.eq(environment_id))
            .filter(deployments::Column::Id.ne(self.deployment_id))
            .filter(deployments::Column::State.is_in(vec![
                "pending",
                "running",
                "built",
                "completed",
            ]))
            .all(self.db.as_ref())
            .await
        {
            Ok(deps) => deps,
            Err(e) => {
                self.log(format!("Failed to fetch previous deployments: {}", e))
                    .await
                    .ok();
                return;
            }
        };

        if previous_deployments.is_empty() {
            self.log("No previous deployments to teardown".to_string())
                .await
                .ok();
            return;
        }

        self.log(format!(
            "Found {} previous deployment(s) to teardown",
            previous_deployments.len()
        ))
        .await
        .ok();

        for deployment in previous_deployments {
            let deployment_id = deployment.id;
            self.log(format!(
                "Tearing down deployment {} (state: {})",
                deployment_id, deployment.state
            ))
            .await
            .ok();

            // Stop all containers for this deployment
            let containers = match deployment_containers::Entity::find()
                .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
                .filter(deployment_containers::Column::DeletedAt.is_null())
                .all(self.db.as_ref())
                .await
            {
                Ok(containers) => containers,
                Err(e) => {
                    self.log(format!(
                        "Failed to fetch containers for deployment {}: {}",
                        deployment_id, e
                    ))
                    .await
                    .ok();
                    continue;
                }
            };

            for container in containers {
                let container_id = container.container_id.clone();

                // Stop container first
                match self.container_deployer.stop_container(&container_id).await {
                    Ok(_) => {
                        self.log(format!("Stopped container {}", container_id))
                            .await
                            .ok();
                    }
                    Err(e) => {
                        self.log(format!("Failed to stop container {}: {}", container_id, e))
                            .await
                            .ok();
                    }
                }

                // Remove container from Docker
                match self
                    .container_deployer
                    .remove_container(&container_id)
                    .await
                {
                    Ok(_) => {
                        self.log(format!("Removed container {}", container_id))
                            .await
                            .ok();
                    }
                    Err(e) => {
                        self.log(format!(
                            "Failed to remove container {}: {}",
                            container_id, e
                        ))
                        .await
                        .ok();
                    }
                }

                // Mark container as deleted in database
                let mut active_container: deployment_containers::ActiveModel = container.into();
                active_container.deleted_at = Set(Some(chrono::Utc::now()));
                active_container.status = Set(Some("removed".to_string()));
                if let Err(e) = active_container.update(self.db.as_ref()).await {
                    self.log(format!("Failed to update container status: {}", e))
                        .await
                        .ok();
                }
            }

            self.log(format!(
                "Torn down deployment {} - containers stopped and removed",
                deployment_id
            ))
            .await
            .ok();
        }

        self.log("All previous deployments torn down successfully".to_string())
            .await
            .ok();
    }
}

#[async_trait]
impl WorkflowTask for MarkDeploymentCompleteJob {
    fn job_id(&self) -> &str {
        &self.job_id
    }

    fn name(&self) -> &str {
        "Mark Deployment Complete"
    }

    fn description(&self) -> &str {
        "Marks the deployment as complete and updates environment routing"
    }

    fn depends_on(&self) -> Vec<String> {
        // This job depends on all core deployment jobs being complete
        // Dependencies are set by the workflow planner
        vec![]
    }

    async fn execute(&self, mut context: WorkflowContext) -> Result<JobResult, WorkflowError> {
        self.log(format!(
            "Marking deployment {} as complete",
            self.deployment_id
        ))
        .await?;

        let output = self.mark_complete(&context).await?;

        // Set job outputs
        context.set_output(
            &self.job_id,
            "completed_at",
            output.completed_at.timestamp(),
        )?;
        context.set_output(&self.job_id, "environment_id", output.environment_id)?;
        context.set_output(&self.job_id, "deployment_id", self.deployment_id)?;

        self.log("Deployment marked as complete successfully".to_string())
            .await?;

        Ok(JobResult::success(context))
    }

    async fn validate_prerequisites(
        &self,
        _context: &WorkflowContext,
    ) -> Result<(), WorkflowError> {
        if self.job_id.is_empty() {
            return Err(WorkflowError::JobValidationFailed(
                "job_id cannot be empty".to_string(),
            ));
        }
        if self.deployment_id <= 0 {
            return Err(WorkflowError::JobValidationFailed(
                "deployment_id must be positive".to_string(),
            ));
        }
        Ok(())
    }

    async fn cleanup(&self, _context: &WorkflowContext) -> Result<(), WorkflowError> {
        Ok(())
    }
}

/// Builder for MarkDeploymentCompleteJob
pub struct MarkDeploymentCompleteJobBuilder {
    job_id: Option<String>,
    deployment_id: Option<i32>,
    db: Option<Arc<DbConnection>>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
    container_deployer: Option<Arc<dyn temps_deployer::ContainerDeployer>>,
    queue: Option<Arc<dyn JobQueue>>,
}

impl MarkDeploymentCompleteJobBuilder {
    pub fn new() -> Self {
        Self {
            job_id: None,
            deployment_id: None,
            db: None,
            log_id: None,
            log_service: None,
            container_deployer: None,
            queue: None,
        }
    }

    pub fn job_id(mut self, job_id: String) -> Self {
        self.job_id = Some(job_id);
        self
    }

    pub fn deployment_id(mut self, deployment_id: i32) -> Self {
        self.deployment_id = Some(deployment_id);
        self
    }

    pub fn db(mut self, db: Arc<DbConnection>) -> Self {
        self.db = Some(db);
        self
    }

    pub fn log_id(mut self, log_id: String) -> Self {
        self.log_id = Some(log_id);
        self
    }

    pub fn log_service(mut self, log_service: Arc<LogService>) -> Self {
        self.log_service = Some(log_service);
        self
    }

    pub fn container_deployer(
        mut self,
        container_deployer: Arc<dyn temps_deployer::ContainerDeployer>,
    ) -> Self {
        self.container_deployer = Some(container_deployer);
        self
    }

    pub fn queue(mut self, queue: Arc<dyn JobQueue>) -> Self {
        self.queue = Some(queue);
        self
    }

    pub fn build(self) -> Result<MarkDeploymentCompleteJob, WorkflowError> {
        let job_id = self
            .job_id
            .unwrap_or_else(|| "mark_deployment_complete".to_string());
        let deployment_id = self.deployment_id.ok_or_else(|| {
            WorkflowError::JobValidationFailed("deployment_id is required".to_string())
        })?;
        let db = self.db.ok_or_else(|| {
            WorkflowError::JobValidationFailed("db connection is required".to_string())
        })?;
        let container_deployer = self.container_deployer.ok_or_else(|| {
            WorkflowError::JobValidationFailed("container_deployer is required".to_string())
        })?;
        let queue = self
            .queue
            .ok_or_else(|| WorkflowError::JobValidationFailed("queue is required".to_string()))?;

        let mut job =
            MarkDeploymentCompleteJob::new(job_id, deployment_id, db, container_deployer, queue);

        if let Some(log_id) = self.log_id {
            job = job.with_log_id(log_id);
        }
        if let Some(log_service) = self.log_service {
            job = job.with_log_service(log_service);
        }

        Ok(job)
    }
}

impl Default for MarkDeploymentCompleteJobBuilder {
    fn default() -> Self {
        Self::new()
    }
}
