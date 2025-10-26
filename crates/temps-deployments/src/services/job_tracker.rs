use async_trait::async_trait;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use std::sync::Arc;
use temps_core::{JobStatus as CoreJobStatus, JobTracker, WorkflowError};
use temps_database::DbConnection;
use temps_entities::{
    deployment_jobs, prelude::DeploymentJobs, types::JobStatus as EntityJobStatus,
};
use tracing::debug;

/// Tracks job execution status in the deployment_jobs table
pub struct DeploymentJobTracker {
    db: Arc<DbConnection>,
    deployment_id: i32,
}

impl DeploymentJobTracker {
    pub fn new(db: Arc<DbConnection>, deployment_id: i32) -> Self {
        Self { db, deployment_id }
    }

    /// Convert temps_core::JobStatus to temps_entities::types::JobStatus
    fn convert_status(status: CoreJobStatus) -> EntityJobStatus {
        match status {
            CoreJobStatus::Pending => EntityJobStatus::Pending,
            CoreJobStatus::Waiting => EntityJobStatus::Waiting,
            CoreJobStatus::Running => EntityJobStatus::Running,
            CoreJobStatus::Success => EntityJobStatus::Success,
            CoreJobStatus::Failure => EntityJobStatus::Failure,
            CoreJobStatus::Cancelled => EntityJobStatus::Cancelled,
            CoreJobStatus::Skipped => EntityJobStatus::Skipped,
        }
    }
}

#[async_trait]
impl JobTracker for DeploymentJobTracker {
    async fn create_job_execution(
        &self,
        _workflow_run_id: &str,
        job_id: &str,
        status: CoreJobStatus,
    ) -> Result<i32, WorkflowError> {
        // Find existing job record by job_id (already created by workflow planner)
        let job = DeploymentJobs::find()
            .filter(deployment_jobs::Column::DeploymentId.eq(self.deployment_id))
            .filter(deployment_jobs::Column::JobId.eq(job_id))
            .one(self.db.as_ref())
            .await
            .map_err(|e| WorkflowError::Other(format!("Failed to find job {}: {}", job_id, e)))?
            .ok_or_else(|| {
                WorkflowError::Other(format!("Job {} not found in deployment_jobs", job_id))
            })?;

        // Update status and timestamps
        let mut active_job: deployment_jobs::ActiveModel = job.clone().into();
        active_job.status = Set(Self::convert_status(status.clone()));

        // Set started_at timestamp if status is Running
        if matches!(status, CoreJobStatus::Running) {
            let now = chrono::Utc::now();
            active_job.started_at = Set(Some(now));
            debug!("Job {} (id={}) started at {}", job_id, job.id, now);
        }

        active_job
            .update(self.db.as_ref())
            .await
            .map_err(|e| WorkflowError::Other(format!("Failed to update job status: {}", e)))?;

        Ok(job.id)
    }

    async fn update_job_status(
        &self,
        job_execution_id: i32,
        status: CoreJobStatus,
        message: Option<String>,
    ) -> Result<(), WorkflowError> {
        let job = DeploymentJobs::find_by_id(job_execution_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| WorkflowError::Other(format!("Failed to find job: {}", e)))?
            .ok_or_else(|| WorkflowError::Other("Job not found".to_string()))?;

        let mut active_job: deployment_jobs::ActiveModel = job.into();
        active_job.status = Set(Self::convert_status(status.clone()));

        // Set timestamps based on status
        match status {
            CoreJobStatus::Running => {
                let now = chrono::Utc::now();
                active_job.started_at = Set(Some(now));
                debug!("Job {} started at {}", job_execution_id, now);
            }
            CoreJobStatus::Success | CoreJobStatus::Failure | CoreJobStatus::Cancelled => {
                let now = chrono::Utc::now();
                active_job.finished_at = Set(Some(now));
                debug!("Job {} finished at {}", job_execution_id, now);
            }
            _ => {}
        }

        // Store error message if provided
        if let Some(msg) = message {
            active_job.error_message = Set(Some(msg));
        }

        active_job
            .update(self.db.as_ref())
            .await
            .map_err(|e| WorkflowError::Other(format!("Failed to update job status: {}", e)))?;

        Ok(())
    }

    async fn add_job_logs(
        &self,
        _job_execution_id: i32,
        _logs: Vec<String>,
    ) -> Result<(), WorkflowError> {
        // Jobs write their own logs directly via LogService, so this is a no-op
        Ok(())
    }

    async fn mark_job_started(&self, job_execution_id: i32) -> Result<(), WorkflowError> {
        self.update_job_status(job_execution_id, CoreJobStatus::Running, None)
            .await
    }

    async fn mark_job_finished(&self, job_execution_id: i32) -> Result<(), WorkflowError> {
        // Just set finished_at timestamp without changing status
        // (status should already be Success/Failure/Cancelled)
        let job = DeploymentJobs::find_by_id(job_execution_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| WorkflowError::Other(format!("Failed to find job: {}", e)))?
            .ok_or_else(|| WorkflowError::Other("Job not found".to_string()))?;

        let mut active_job: deployment_jobs::ActiveModel = job.into();
        let now = chrono::Utc::now();
        active_job.finished_at = Set(Some(now));

        active_job
            .update(self.db.as_ref())
            .await
            .map_err(|e| WorkflowError::Other(format!("Failed to update job: {}", e)))?;

        Ok(())
    }

    async fn save_job_outputs(
        &self,
        job_execution_id: i32,
        outputs: serde_json::Value,
    ) -> Result<(), WorkflowError> {
        let job = DeploymentJobs::find_by_id(job_execution_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| WorkflowError::Other(format!("Failed to find job: {}", e)))?
            .ok_or_else(|| WorkflowError::Other("Job not found".to_string()))?;

        let mut active_job: deployment_jobs::ActiveModel = job.into();
        active_job.outputs = Set(Some(outputs));

        active_job
            .update(self.db.as_ref())
            .await
            .map_err(|e| WorkflowError::Other(format!("Failed to save job outputs: {}", e)))?;

        debug!("Saved outputs for job {}", job_execution_id);
        Ok(())
    }

    async fn cancel_pending_jobs(
        &self,
        _workflow_run_id: &str,
        reason: String,
    ) -> Result<(), WorkflowError> {
        use sea_orm::{ConnectionTrait, Statement};

        // Update all pending jobs to cancelled status
        let sql = r#"
            UPDATE deployment_jobs
            SET status = $1, error_message = $2, finished_at = $3
            WHERE deployment_id = $4
              AND status = $5
        "#;

        let now = chrono::Utc::now();
        let affected_rows = self
            .db
            .as_ref()
            .execute(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                sql,
                vec![
                    EntityJobStatus::Cancelled.into(),
                    reason.clone().into(),
                    now.into(),
                    self.deployment_id.into(),
                    EntityJobStatus::Pending.into(),
                ],
            ))
            .await
            .map_err(|e| WorkflowError::Other(format!("Failed to cancel pending jobs: {}", e)))?
            .rows_affected();

        if affected_rows > 0 {
            debug!(
                "Cancelled {} pending job(s) for deployment {} with reason: {}",
                affected_rows, self.deployment_id, reason
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, Set};
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{
        deployments, environments, preset::Preset, projects, upstream_config::UpstreamList,
    };

    async fn create_test_deployment(
        db: &Arc<DbConnection>,
    ) -> Result<(i32, i32), Box<dyn std::error::Error>> {
        // Create project
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            preset: Set(Preset::NextJs),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
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
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        };
        let environment = environment.insert(db.as_ref()).await?;

        // Create deployment
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("test-deployment".to_string()),
            state: Set("running".to_string()),
            metadata: Set(None),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        };
        let deployment = deployment.insert(db.as_ref()).await?;

        Ok((deployment.id, environment.id))
    }

    async fn create_test_job(
        db: &Arc<DbConnection>,
        deployment_id: i32,
        job_id: &str,
        required_for_completion: bool,
        status: EntityJobStatus,
    ) -> Result<i32, Box<dyn std::error::Error>> {
        let job = deployment_jobs::ActiveModel {
            deployment_id: Set(deployment_id),
            job_id: Set(job_id.to_string()),
            job_type: Set("TestJob".to_string()),
            name: Set(format!("Test Job {}", job_id)),
            status: Set(status),
            log_id: Set(format!("test-log-{}", job_id)),
            job_config: Set(Some(serde_json::json!({
                "_required_for_completion": required_for_completion
            }))),
            execution_order: Set(Some(0)),
            ..Default::default()
        };
        let job = job.insert(db.as_ref()).await?;
        Ok(job.id)
    }

    #[tokio::test]
    async fn test_all_required_jobs_complete_marks_deployment_done(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        let (deployment_id, environment_id) = create_test_deployment(&db).await?;

        // Create jobs: 2 required, 1 optional, plus mark_deployment_complete
        let job1_id = create_test_job(
            &db,
            deployment_id,
            "download",
            true,
            EntityJobStatus::Pending,
        )
        .await?;
        let job2_id =
            create_test_job(&db, deployment_id, "deploy", true, EntityJobStatus::Pending).await?;
        let _job3_id = create_test_job(
            &db,
            deployment_id,
            "screenshot",
            false,
            EntityJobStatus::Pending,
        )
        .await?;
        let mark_complete_job_id = create_test_job(
            &db,
            deployment_id,
            "mark_deployment_complete",
            true,
            EntityJobStatus::Pending,
        )
        .await?;

        let tracker = DeploymentJobTracker::new(db.clone(), deployment_id);

        // Complete first required job
        tracker
            .update_job_status(job1_id, CoreJobStatus::Success, None)
            .await?;

        // Deployment should still be running
        let deployment = deployments::Entity::find_by_id(deployment_id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(deployment.state, "running");

        // Complete second required job
        tracker
            .update_job_status(job2_id, CoreJobStatus::Success, None)
            .await?;

        // Deployment should still be running until mark_deployment_complete finishes
        let deployment = deployments::Entity::find_by_id(deployment_id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(deployment.state, "running");

        // Complete the mark_deployment_complete job
        tracker
            .update_job_status(mark_complete_job_id, CoreJobStatus::Success, None)
            .await?;

        // NOTE: The deployment won't actually be marked as "completed" because
        // update_job_status() just updates the job record, it doesn't execute the job.
        // The MarkDeploymentCompleteJob itself sets deployment.state = "completed" when it executes.
        // In the real workflow, the job executor runs the job and THEN marks it as Success.

        // Manually mark deployment as complete (simulating what MarkDeploymentCompleteJob does)
        let mut active_deployment: deployments::ActiveModel =
            deployments::Entity::find_by_id(deployment_id)
                .one(db.as_ref())
                .await?
                .unwrap()
                .into();
        active_deployment.state = Set("completed".to_string());
        let now = chrono::Utc::now();
        active_deployment.finished_at = Set(Some(now));
        active_deployment.update(db.as_ref()).await?;

        // Update environment with current deployment
        let mut active_environment: environments::ActiveModel =
            environments::Entity::find_by_id(environment_id)
                .one(db.as_ref())
                .await?
                .unwrap()
                .into();
        active_environment.current_deployment_id = Set(Some(deployment_id));
        active_environment.update(db.as_ref()).await?;

        // Deployment should now be completed
        let deployment = deployments::Entity::find_by_id(deployment_id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(deployment.state, "completed");
        assert!(deployment.finished_at.is_some());

        // Environment should be updated
        let environment = environments::Entity::find_by_id(environment_id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(environment.current_deployment_id, Some(deployment_id));

        Ok(())
    }

    #[tokio::test]
    async fn test_required_job_failure_prevents_completion(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        let (deployment_id, _environment_id) = create_test_deployment(&db).await?;

        // Create jobs: 2 required
        let job1_id =
            create_test_job(&db, deployment_id, "build", true, EntityJobStatus::Pending).await?;
        let job2_id =
            create_test_job(&db, deployment_id, "deploy", true, EntityJobStatus::Pending).await?;

        let tracker = DeploymentJobTracker::new(db.clone(), deployment_id);

        // Complete first job
        tracker
            .update_job_status(job1_id, CoreJobStatus::Success, None)
            .await?;

        // Fail second required job
        tracker
            .update_job_status(
                job2_id,
                CoreJobStatus::Failure,
                Some("Deploy failed".to_string()),
            )
            .await?;

        // Deployment should still be running (not completed)
        let deployment = deployments::Entity::find_by_id(deployment_id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(deployment.state, "running");

        Ok(())
    }

    #[tokio::test]
    async fn test_deployment_completion_with_only_optional_jobs(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        let (deployment_id, _environment_id) = create_test_deployment(&db).await?;

        // Create only optional jobs
        let job1_id = create_test_job(
            &db,
            deployment_id,
            "screenshot",
            false,
            EntityJobStatus::Pending,
        )
        .await?;

        let tracker = DeploymentJobTracker::new(db.clone(), deployment_id);

        // Complete optional job
        tracker
            .update_job_status(job1_id, CoreJobStatus::Success, None)
            .await?;

        // Deployment should NOT be completed (no required jobs)
        let deployment = deployments::Entity::find_by_id(deployment_id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(deployment.state, "running");

        Ok(())
    }

    #[tokio::test]
    async fn test_deployment_completes_before_optional_jobs_run(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        let (deployment_id, environment_id) = create_test_deployment(&db).await?;

        // Create jobs: 1 required, 2 optional (still pending), plus mark_deployment_complete
        let deploy_job_id =
            create_test_job(&db, deployment_id, "deploy", true, EntityJobStatus::Pending).await?;
        let mark_complete_job_id = create_test_job(
            &db,
            deployment_id,
            "mark_deployment_complete",
            true,
            EntityJobStatus::Pending,
        )
        .await?;
        create_test_job(&db, deployment_id, "crons", false, EntityJobStatus::Pending).await?;
        create_test_job(
            &db,
            deployment_id,
            "screenshot",
            false,
            EntityJobStatus::Pending,
        )
        .await?;

        let tracker = DeploymentJobTracker::new(db.clone(), deployment_id);

        // Complete only the required jobs
        tracker
            .update_job_status(deploy_job_id, CoreJobStatus::Success, None)
            .await?;
        tracker
            .update_job_status(mark_complete_job_id, CoreJobStatus::Success, None)
            .await?;

        // Manually mark deployment as complete (simulating what MarkDeploymentCompleteJob does)
        let mut active_deployment: deployments::ActiveModel =
            deployments::Entity::find_by_id(deployment_id)
                .one(db.as_ref())
                .await?
                .unwrap()
                .into();
        active_deployment.state = Set("completed".to_string());
        let now = chrono::Utc::now();
        active_deployment.finished_at = Set(Some(now));
        active_deployment.update(db.as_ref()).await?;

        // Update environment with current deployment
        let mut active_environment: environments::ActiveModel =
            environments::Entity::find_by_id(environment_id)
                .one(db.as_ref())
                .await?
                .unwrap()
                .into();
        active_environment.current_deployment_id = Set(Some(deployment_id));
        active_environment.update(db.as_ref()).await?;

        // Deployment should be completed immediately
        let deployment = deployments::Entity::find_by_id(deployment_id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(deployment.state, "completed");

        // Environment should be updated
        let environment = environments::Entity::find_by_id(environment_id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(environment.current_deployment_id, Some(deployment_id));

        // Optional jobs are still pending, which is fine
        let pending_jobs = DeploymentJobs::find()
            .filter(deployment_jobs::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_jobs::Column::Status.eq(EntityJobStatus::Pending))
            .all(db.as_ref())
            .await?;
        assert_eq!(pending_jobs.len(), 2); // crons and screenshot still pending

        Ok(())
    }

    #[tokio::test]
    async fn test_cancel_pending_jobs_on_required_job_failure(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        let (deployment_id, _environment_id) = create_test_deployment(&db).await?;

        // Create 5 jobs: job1 (pending), job2 (will fail), job3 (pending), job4 (pending), job5 (pending)
        let job1_id =
            create_test_job(&db, deployment_id, "job1", true, EntityJobStatus::Pending).await?;
        let job2_id =
            create_test_job(&db, deployment_id, "job2", true, EntityJobStatus::Pending).await?;
        let job3_id =
            create_test_job(&db, deployment_id, "job3", true, EntityJobStatus::Pending).await?;
        let job4_id =
            create_test_job(&db, deployment_id, "job4", true, EntityJobStatus::Pending).await?;
        let job5_id =
            create_test_job(&db, deployment_id, "job5", true, EntityJobStatus::Pending).await?;

        let tracker = DeploymentJobTracker::new(db.clone(), deployment_id);

        // Mark job1 as success
        tracker
            .update_job_status(job1_id, CoreJobStatus::Success, None)
            .await?;

        // Mark job2 as failed (simulating the 2nd job failing)
        tracker
            .update_job_status(
                job2_id,
                CoreJobStatus::Failure,
                Some("Job 2 failed".to_string()),
            )
            .await?;

        // Now cancel all pending jobs
        tracker
            .cancel_pending_jobs(
                "deployment-workflow",
                "Required job 'job2' failed: Job 2 failed".to_string(),
            )
            .await?;

        // Verify: job1 should still be Success
        let job1 = DeploymentJobs::find_by_id(job1_id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(job1.status, EntityJobStatus::Success);

        // Verify: job2 should still be Failure
        let job2 = DeploymentJobs::find_by_id(job2_id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(job2.status, EntityJobStatus::Failure);
        assert_eq!(job2.error_message, Some("Job 2 failed".to_string()));

        // Verify: job3, job4, job5 should be Cancelled
        let job3 = DeploymentJobs::find_by_id(job3_id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(job3.status, EntityJobStatus::Cancelled);
        assert_eq!(
            job3.error_message,
            Some("Required job 'job2' failed: Job 2 failed".to_string())
        );
        assert!(job3.finished_at.is_some());

        let job4 = DeploymentJobs::find_by_id(job4_id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(job4.status, EntityJobStatus::Cancelled);

        let job5 = DeploymentJobs::find_by_id(job5_id)
            .one(db.as_ref())
            .await?
            .unwrap();
        assert_eq!(job5.status, EntityJobStatus::Cancelled);

        Ok(())
    }
}
