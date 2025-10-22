//! Mark Deployment Complete Job
//!
//! A synthetic job that marks the deployment as complete and updates the environment.
//! This job runs after all core deployment jobs (download, build, deploy) succeed.
//! Optional jobs (screenshots, crons) depend on this job, ensuring the deployment
//! is live before they run.

use async_trait::async_trait;
use sea_orm::{ActiveModelTrait, EntityTrait, Set};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::{JobResult, UtcDateTime, WorkflowContext, WorkflowError, WorkflowTask};
use temps_database::DbConnection;
use temps_entities::{deployment_containers, deployments, environments};
use temps_logs::LogService;
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
    pub fn new(job_id: String, deployment_id: i32, db: Arc<DbConnection>) -> Self {
        Self {
            job_id,
            deployment_id,
            db,
            log_id: None,
            log_service: None,
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
        if let (Some(ref log_id), Some(ref log_service)) = (&self.log_id, &self.log_service) {
            log_service
                .append_to_log(log_id, &format!("{}\n", message))
                .await
                .map_err(|e| WorkflowError::Other(format!("Failed to write log: {}", e)))?;
        }
        Ok(())
    }

    /// Mark deployment as complete and update environment
    /// Also updates deployment with workflow outputs (image info, container info)
    async fn mark_complete(
        &self,
        context: &WorkflowContext,
    ) -> Result<MarkCompleteOutput, WorkflowError> {
        self.log("🎯 Marking deployment as complete...".to_string())
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
        let mut active_deployment: deployments::ActiveModel = deployment.into();

        // Extract image info from build job output
        if let Ok(Some(image_tag)) = context.get_output::<String>("build_image", "image_tag") {
            debug!("Setting deployment image_name to: {}", image_tag);
            active_deployment.image_name = Set(Some(image_tag));
        }

        // Extract container info from deploy job output and create deployment_container record
        if let Ok(Some(container_id)) =
            context.get_output::<String>("deploy_container", "container_id")
        {
            let container_name = context
                .get_output::<String>("deploy_container", "container_name")
                .ok()
                .flatten()
                .unwrap_or_else(|| format!("container-{}", self.deployment_id));

            let container_port = context
                .get_output::<i32>("deploy_container", "container_port")
                .ok()
                .flatten()
                .unwrap_or(8080);

            let host_port = context
                .get_output::<i32>("deploy_container", "host_port")
                .ok()
                .flatten();

            let now = chrono::Utc::now();

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
                "✅ Created deployment_container record for container {}",
                container_id
            );
            self.log(format!("✅ Container {} registered", container_id))
                .await?;
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

        info!("✅ Deployment {} marked as complete", self.deployment_id);
        self.log(format!(
            "✅ Deployment {} status updated to Completed",
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
            .update(self.db.as_ref())
            .await
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to update environment: {}", e))
            })?;

        info!(
            "✅ Environment {} current_deployment_id updated to {}",
            environment_id, self.deployment_id
        );
        self.log(format!(
            "✅ Environment {} now points to deployment {}",
            environment_id, self.deployment_id
        ))
        .await?;

        self.log("🎉 Deployment is now LIVE and ready for traffic!".to_string())
            .await?;

        Ok(MarkCompleteOutput {
            completed_at: now,
            environment_id,
        })
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
            "🚀 Marking deployment {} as complete",
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

        self.log("✅ Deployment marked as complete successfully".to_string())
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
}

impl MarkDeploymentCompleteJobBuilder {
    pub fn new() -> Self {
        Self {
            job_id: None,
            deployment_id: None,
            db: None,
            log_id: None,
            log_service: None,
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

        let mut job = MarkDeploymentCompleteJob::new(job_id, deployment_id, db);

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
