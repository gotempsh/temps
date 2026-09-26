// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use std::sync::Arc;
use temps_core::{JobStatus as CoreJobStatus, JobTracker, WorkflowError};
use temps_database::DbConnection;
use temps_entities::{
    deployment_jobs, prelude::DeploymentJobs, types::JobStatus as EntityJobStatus,
};
use temps_logs::LogService;
use tracing::{debug, warn};

/// Tracks job execution status in the deployment_jobs table
pub struct DeploymentJobTracker {
    db: Arc<DbConnection>,
    deployment_id: i32,
    log_service: Arc<LogService>,
}

impl DeploymentJobTracker {
    pub fn new(db: Arc<DbConnection>, deployment_id: i32, log_service: Arc<LogService>) -> Self {
        Self {
            db,
            deployment_id,
            log_service,
        }
    }

    /// Archive a job's log to the configured backend (S3, when
    /// `TEMPS_LOG_STORAGE_BACKEND=s3`) now that it has reached a terminal
    /// state, freeing the local scratch file. Best-effort: an archival
    /// failure must never fail the job/deployment that already finished --
    /// the job's outcome and the log's storage location are independent
    /// concerns, and a build the user is waiting on should not be reported
    /// as failed because of a transient S3 hiccup after the fact.
    ///
    /// Fire-and-forget: spawned onto its own task rather than awaited on the
    /// caller's stack. `update_job_status`/`create_job_execution` sit on the
    /// workflow executor's critical path -- it can't schedule a dependent
    /// job batch or report the deployment as complete until they return --
    /// so a slow or momentarily unavailable S3 endpoint must never stall
    /// them, even with `LogService::archive_log`'s own bounded
    /// timeout/retry, whose worst case (several attempts, each up to 30s,
    /// with backoff between) is still far too long to hold up scheduling.
    /// If the process exits before the spawned task completes (e.g. a
    /// restart raced with a job finishing), the archive step simply never
    /// ran: the local scratch file is left in place rather than orphaned or
    /// deleted, and `LogService::get_log_content`'s local-file-first read
    /// path keeps serving it exactly as it would have before this feature
    /// existed -- nothing is lost, that one job's log just permanently stays
    /// on local disk (today's default behavior) instead of also landing in
    /// the bucket. This is a known, documented trade-off of the
    /// fire-and-forget design, not a failure mode: it trades a rare,
    /// bounded amount of local disk for never blocking the workflow on S3.
    fn archive_job_log(&self, log_id: String) {
        let log_service = self.log_service.clone();
        let deployment_id = self.deployment_id;
        tokio::spawn(async move {
            if let Err(e) = log_service.archive_log(&log_id).await {
                warn!(
                    deployment_id,
                    log_id = %log_id,
                    error = %e,
                    "Failed to archive job log to configured backend; log remains on local disk"
                );
            }
        });
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

        // Capture before `job` is consumed below -- needed for archival if
        // this call already lands the job in a terminal state (e.g.
        // `WorkflowExecutor::persist_terminal_status` creating a job record
        // that failed prerequisite validation without ever running).
        let log_id = job.log_id.clone();

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

        if matches!(
            status,
            CoreJobStatus::Success
                | CoreJobStatus::Failure
                | CoreJobStatus::Cancelled
                | CoreJobStatus::Skipped
        ) {
            self.archive_job_log(log_id);
        }

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

        // Captured before `job` is consumed below -- this is the job whose
        // log gets archived once we know the status update below reaches a
        // terminal state.
        let log_id = job.log_id.clone();

        let mut active_job: deployment_jobs::ActiveModel = job.into();
        active_job.status = Set(Self::convert_status(status.clone()));

        // Set timestamps based on status
        let is_terminal = matches!(
            status,
            CoreJobStatus::Success
                | CoreJobStatus::Failure
                | CoreJobStatus::Cancelled
                | CoreJobStatus::Skipped
        );
        match status {
            CoreJobStatus::Running => {
                let now = chrono::Utc::now();
                active_job.started_at = Set(Some(now));
                debug!("Job {} started at {}", job_execution_id, now);
            }
            CoreJobStatus::Success
            | CoreJobStatus::Failure
            | CoreJobStatus::Cancelled
            | CoreJobStatus::Skipped => {
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

        // Archive the job's log now that its terminal status is durably
        // recorded (only once a job has actually finished running is it
        // safe to stop treating the local file as a live scratch file --
        // see `LogService::archive_log`'s doc comment). This is the hook
        // that keeps local disk bounded to currently-running jobs.
        if is_terminal {
            self.archive_job_log(log_id);
        }

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

    /// Cancel every still-`Pending` job for this deployment in one bulk
    /// UPDATE. Deliberately does not archive these jobs' logs: a `Pending`
    /// job has by definition never entered `Running` (that transition only
    /// happens via `create_job_execution`/`update_job_status` above), so it
    /// never called `LogService::log_*` and has no local log file to
    /// archive. `archive_log` would be a correct no-op here too, but calling
    /// it would mean an extra per-row S3-config check (and, if this bulk
    /// path is ever used for a large fan-out of pending jobs, N archive
    /// calls) for something that structurally cannot have written a log.
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
    use std::time::Duration;
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{
        deployments, environments, preset::Preset, projects, upstream_config::UpstreamList,
    };

    /// Polls `check` every 10ms until it returns `Some`, up to `timeout`.
    ///
    /// `DeploymentJobTracker::archive_job_log` is fire-and-forget
    /// (`tokio::spawn`, deliberately not awaited on the workflow's critical
    /// path -- see its doc comment), so tests asserting on its effects
    /// (an upload landing in a mock archive, a local file disappearing)
    /// can't rely on those effects being visible the instant the tracker
    /// call returns. This gives the spawned task a bounded window to run
    /// instead, mirroring the polling pattern `temps-logs`' own tests use
    /// for tail-stream convergence.
    async fn wait_for<T>(timeout: Duration, mut check: impl FnMut() -> Option<T>) -> Option<T> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(value) = check() {
                return Some(value);
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// A `LogService` with no archive backend configured, matching the
    /// default (filesystem-only) production behavior. None of these tests
    /// exercise archival directly -- that's covered in `temps-logs`'
    /// `file_logs` tests -- they only need a real `LogService` to satisfy
    /// `DeploymentJobTracker::new`'s constructor.
    fn test_log_service() -> Arc<LogService> {
        Arc::new(LogService::new(std::env::temp_dir()))
    }

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
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
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

        let tracker = DeploymentJobTracker::new(db.clone(), deployment_id, test_log_service());

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

        let tracker = DeploymentJobTracker::new(db.clone(), deployment_id, test_log_service());

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

        let tracker = DeploymentJobTracker::new(db.clone(), deployment_id, test_log_service());

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

        let tracker = DeploymentJobTracker::new(db.clone(), deployment_id, test_log_service());

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

    /// Regression test for #477: an optional job that never ran (failed
    /// prerequisites) must leave a terminal row behind — not stay Pending — with the
    /// reason and a finish time, so the UI stops showing it as in-progress.
    #[tokio::test]
    async fn test_prerequisite_failure_leaves_job_failed_not_pending(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        let (deployment_id, _environment_id) = create_test_deployment(&db).await?;
        let screenshot_job_id = create_test_job(
            &db,
            deployment_id,
            "take_screenshot",
            false,
            EntityJobStatus::Pending,
        )
        .await?;

        let tracker = DeploymentJobTracker::new(db.clone(), deployment_id, test_log_service());

        // This is what WorkflowExecutor::persist_terminal_status does when
        // validate_prerequisites fails.
        let execution_id = tracker
            .create_job_execution(
                "deployment-workflow",
                "take_screenshot",
                CoreJobStatus::Failure,
            )
            .await?;
        assert_eq!(execution_id, screenshot_job_id);

        tracker
            .update_job_status(
                execution_id,
                CoreJobStatus::Failure,
                Some(
                    "Screenshot provider 'local-headless-chrome' is not available: \
                      Chrome browser error: libnss3.so: cannot open shared object file"
                        .to_string(),
                ),
            )
            .await?;

        let job = DeploymentJobs::find_by_id(screenshot_job_id)
            .one(db.as_ref())
            .await?
            .ok_or("screenshot job should exist")?;

        assert_eq!(job.status, EntityJobStatus::Failure);
        assert!(
            job.finished_at.is_some(),
            "a job that reached a terminal state must have finished_at set"
        );
        assert!(job.error_message.unwrap_or_default().contains("libnss3.so"));

        Ok(())
    }

    /// A skipped job must also be terminal, including `finished_at`.
    #[tokio::test]
    async fn test_skipped_job_records_finished_at() -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        let (deployment_id, _environment_id) = create_test_deployment(&db).await?;
        let job_id =
            create_test_job(&db, deployment_id, "crons", false, EntityJobStatus::Pending).await?;

        let tracker = DeploymentJobTracker::new(db.clone(), deployment_id, test_log_service());
        tracker
            .update_job_status(
                job_id,
                CoreJobStatus::Skipped,
                Some("Not configured".to_string()),
            )
            .await?;

        let job = DeploymentJobs::find_by_id(job_id)
            .one(db.as_ref())
            .await?
            .ok_or("job should exist")?;

        assert_eq!(job.status, EntityJobStatus::Skipped);
        assert!(
            job.finished_at.is_some(),
            "skipped jobs are terminal and must record finished_at"
        );

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

        let tracker = DeploymentJobTracker::new(db.clone(), deployment_id, test_log_service());

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

    /// Records every `upload_log` call so tests can assert the
    /// completion-hook wiring (not `LogService`'s own archival logic, which
    /// is covered by `temps-logs`' own tests) actually fires exactly once
    /// per terminal transition, with the right key.
    #[derive(Default)]
    struct RecordingArchive {
        uploaded_keys: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl temps_logs::LogArchiveStorage for RecordingArchive {
        async fn upload_log(
            &self,
            key: &str,
            _data: Vec<u8>,
        ) -> Result<(), temps_logs::LogArchiveStorageError> {
            self.uploaded_keys
                .lock()
                .expect("recording archive lock poisoned")
                .push(key.to_string());
            Ok(())
        }

        async fn download_log(
            &self,
            key: &str,
        ) -> Result<Vec<u8>, temps_logs::LogArchiveStorageError> {
            Err(temps_logs::LogArchiveStorageError::NotFound {
                bucket: "recording-archive".to_string(),
                key: key.to_string(),
            })
        }
    }

    #[tokio::test]
    async fn test_update_job_status_terminal_archives_log_when_backend_configured(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        let (deployment_id, _environment_id) = create_test_deployment(&db).await?;
        let job_id =
            create_test_job(&db, deployment_id, "build", true, EntityJobStatus::Pending).await?;

        let temp_dir = tempfile::TempDir::new()?;
        let archive = std::sync::Arc::new(RecordingArchive::default());
        let log_service = std::sync::Arc::new(LogService::with_archive(
            temp_dir.path().to_path_buf(),
            Some(archive.clone() as std::sync::Arc<dyn temps_logs::LogArchiveStorage>),
        ));

        // `create_test_job`'s log_id ("test-log-build") deliberately has no
        // `/` and no extension, which is fine for the other tests in this
        // file but doesn't reflect real `deployment_jobs.log_id` values
        // (always `{project}/{env}/{date-path}/deployment-{id}-job-{job}.log`,
        // see `workflow_planner.rs`) and would exercise the dormant
        // `LogService` path-resolution quirk documented on
        // `realistic_log_id` in `temps-logs`' `file_logs.rs` tests. Give
        // this job a realistic log_id instead so the test exercises what
        // production actually does.
        let job = DeploymentJobs::find_by_id(job_id)
            .one(db.as_ref())
            .await?
            .unwrap();
        let mut active_job: deployment_jobs::ActiveModel = job.into();
        active_job.log_id =
            Set("proj/prod/2026/09/18/12/00/deployment-1-job-build.log".to_string());
        let job = active_job.update(db.as_ref()).await?;

        // The job actually "runs" and writes a log line, exactly like a real
        // deployment job would via DeploymentStageLogWriter, so there is a
        // local file for the completion hook to archive.
        log_service
            .log_info(&job.log_id, "Building image...")
            .await?;

        let tracker = DeploymentJobTracker::new(db.clone(), deployment_id, log_service.clone());

        tracker
            .update_job_status(job_id, CoreJobStatus::Success, None)
            .await?;

        // Archival is fire-and-forget (spawned onto its own task so a slow
        // S3 call can never block workflow progression -- see
        // `DeploymentJobTracker::archive_job_log`), so give the spawned task
        // a bounded window to actually run before asserting on its effects.
        let uploaded = wait_for(Duration::from_secs(2), || {
            let uploaded = archive.uploaded_keys.lock().unwrap().clone();
            (!uploaded.is_empty()).then_some(uploaded)
        })
        .await
        .expect("archive upload did not complete within 2s of update_job_status returning");
        assert_eq!(
            uploaded.len(),
            1,
            "expected exactly one archive upload for the completed job"
        );
        assert!(
            uploaded[0].ends_with(&job.log_id),
            "archived key '{}' should be derived from the job's log_id '{}'",
            uploaded[0],
            job.log_id
        );

        // The completion hook must also have deleted the local scratch file
        // once the upload succeeded -- also part of the same spawned task,
        // so give it the same bounded window rather than a hard assertion.
        let deleted = wait_for(Duration::from_secs(2), || {
            (!log_service.get_log_path(&job.log_id).exists()).then_some(())
        })
        .await;
        assert!(
            deleted.is_some(),
            "local scratch file was not removed within 2s of the archive upload"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_update_job_status_non_terminal_does_not_archive(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = TestDatabase::with_migrations().await?;
        let db = test_db.connection_arc();

        let (deployment_id, _environment_id) = create_test_deployment(&db).await?;
        let job_id =
            create_test_job(&db, deployment_id, "build", true, EntityJobStatus::Pending).await?;

        let temp_dir = tempfile::TempDir::new()?;
        let archive = std::sync::Arc::new(RecordingArchive::default());
        let log_service = std::sync::Arc::new(LogService::with_archive(
            temp_dir.path().to_path_buf(),
            Some(archive.clone() as std::sync::Arc<dyn temps_logs::LogArchiveStorage>),
        ));

        let tracker = DeploymentJobTracker::new(db.clone(), deployment_id, log_service);

        // Transitioning to Running (non-terminal) must never trigger archival
        // -- the job is still writing to its log file.
        tracker
            .update_job_status(job_id, CoreJobStatus::Running, None)
            .await?;

        assert!(archive.uploaded_keys.lock().unwrap().is_empty());

        Ok(())
    }
}
