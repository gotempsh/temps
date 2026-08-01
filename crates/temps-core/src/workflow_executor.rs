//! Workflow Executor
//!
//! Executes workflows with job dependencies, parallel execution, and proper error handling.

use crate::workflow::{
    JobConfig, JobResult, JobStatus, WorkflowCancellationProvider, WorkflowConfig, WorkflowContext,
    WorkflowError,
};
use futures::future::join_all;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::{debug, error, info, warn};

/// Workflow executor that handles job dependencies and parallel execution
pub struct WorkflowExecutor {
    /// Optional job tracker for persistence
    job_tracker: Option<Arc<dyn JobTracker>>,
}

/// Trait for tracking job executions (similar to StageTracker)
#[async_trait::async_trait]
pub trait JobTracker: Send + Sync {
    /// Create a new job execution record
    async fn create_job_execution(
        &self,
        workflow_run_id: &str,
        job_id: &str,
        status: JobStatus,
    ) -> Result<i32, WorkflowError>; // Returns job execution record ID

    /// Update job status
    async fn update_job_status(
        &self,
        job_execution_id: i32,
        status: JobStatus,
        message: Option<String>,
    ) -> Result<(), WorkflowError>;

    /// Add logs to a job execution
    async fn add_job_logs(
        &self,
        job_execution_id: i32,
        logs: Vec<String>,
    ) -> Result<(), WorkflowError>;

    /// Mark job as started
    async fn mark_job_started(&self, job_execution_id: i32) -> Result<(), WorkflowError>;

    /// Mark job as finished
    async fn mark_job_finished(&self, job_execution_id: i32) -> Result<(), WorkflowError>;

    /// Save job outputs
    async fn save_job_outputs(
        &self,
        job_execution_id: i32,
        outputs: serde_json::Value,
    ) -> Result<(), WorkflowError>;

    /// Cancel all pending jobs in the workflow
    async fn cancel_pending_jobs(
        &self,
        workflow_run_id: &str,
        reason: String,
    ) -> Result<(), WorkflowError>;
}

/// Job execution state
#[derive(Debug, Clone)]
struct JobExecutionState {
    job_config: JobConfig,
    status: JobStatus,
    dependencies: Vec<String>,
    dependents: Vec<String>,
    execution_id: Option<i32>,
    result: Option<JobResult>,
}

impl WorkflowExecutor {
    pub fn new(job_tracker: Option<Arc<dyn JobTracker>>) -> Self {
        Self { job_tracker }
    }

    fn is_cancellation_error(error: &WorkflowError) -> bool {
        matches!(
            error,
            WorkflowError::WorkflowCancelled | WorkflowError::BuildCancelled
        )
    }

    /// Execute a workflow with proper dependency resolution and parallel execution
    pub async fn execute_workflow(
        &self,
        config: WorkflowConfig,
        cancellation_provider: Arc<dyn WorkflowCancellationProvider>,
    ) -> Result<WorkflowContext, WorkflowError> {
        info!(
            "🚀 Starting workflow execution: {} with {} jobs",
            config.workflow_run_id,
            config.jobs.len()
        );

        // Initialize context with log writer
        let mut context = WorkflowContext::new(
            config.workflow_run_id.clone(),
            config.deployment_id,
            config.project_id,
            config.environment_id,
            Arc::clone(&config.log_writer),
        );
        context.vars = config.initial_context;

        // Build dependency graph and validate
        let mut job_states = self.build_job_dependency_graph(&config.jobs)?;
        self.validate_dependency_graph(&job_states)?;

        // Calculate execution order
        let execution_order = self.calculate_execution_order(&job_states)?;
        debug!("📋 Job execution order: {:?}", execution_order);

        // Create a semaphore to limit parallel execution
        let semaphore = Arc::new(Semaphore::new(config.max_parallel_jobs));

        // Execute jobs in dependency order
        for batch in execution_order {
            // Check for cancellation
            if cancellation_provider
                .is_cancelled(&config.workflow_run_id)
                .await?
            {
                warn!("🚫 Workflow {} was cancelled", config.workflow_run_id);

                self.cancel_remaining_pending(
                    &config.workflow_run_id,
                    "Workflow cancelled by user".to_string(),
                )
                .await;

                return Err(WorkflowError::WorkflowCancelled);
            }

            // Execute all jobs in this batch in parallel
            let batch_results = match self
                .execute_job_batch(
                    batch,
                    &mut job_states,
                    &mut context,
                    &semaphore,
                    cancellation_provider.clone(),
                    config.continue_on_failure,
                )
                .await
            {
                Ok(results) => results,
                Err(e) => {
                    // Aborting mid-batch skips the per-result handling below, which is
                    // what would normally cancel the rest of the workflow. Without this,
                    // every job that never got to run keeps the planner's `Pending`
                    // status forever and the UI polls it as still in progress.
                    self.cancel_remaining_pending(
                        &config.workflow_run_id,
                        format!("Workflow aborted: {}", e),
                    )
                    .await;
                    return Err(e);
                }
            };

            // Check if any required jobs failed
            for (job_id, result) in batch_results {
                if let Some(job_state) = job_states.get_mut(&job_id) {
                    job_state.result = Some(result.clone());
                    job_state.status = result.status.clone();

                    // Merge outputs from job result into context
                    // (don't overwrite — parallel jobs each have their own outputs)
                    for (job_key, job_outputs) in result.context.outputs {
                        context
                            .outputs
                            .entry(job_key)
                            .or_default()
                            .extend(job_outputs);
                    }
                    for (job_key, job_artifacts) in result.context.artifacts {
                        context
                            .artifacts
                            .entry(job_key)
                            .or_default()
                            .extend(job_artifacts);
                    }
                    for (key, value) in result.context.vars {
                        context.vars.insert(key, value);
                    }

                    // Update job tracker with completion status
                    if let Some(ref tracker) = self.job_tracker {
                        if let Some(execution_id) = job_state.execution_id {
                            let update_result = match result.status {
                                JobStatus::Success => {
                                    tracker
                                        .update_job_status(execution_id, JobStatus::Success, None)
                                        .await
                                }
                                JobStatus::Failure => {
                                    let error_msg = result
                                        .message
                                        .clone()
                                        .unwrap_or_else(|| "Job failed".to_string());
                                    tracker
                                        .update_job_status(
                                            execution_id,
                                            JobStatus::Failure,
                                            Some(error_msg),
                                        )
                                        .await
                                }
                                _ => {
                                    tracker
                                        .update_job_status(
                                            execution_id,
                                            result.status.clone(),
                                            result.message.clone(),
                                        )
                                        .await
                                }
                            };

                            if let Err(e) = update_result {
                                error!(
                                    "Failed to update job {} status to {:?}: {}",
                                    job_id, result.status, e
                                );
                            } else {
                                debug!("✅ Updated job {} status to {:?}", job_id, result.status);
                            }

                            // Save job outputs if the job succeeded
                            if matches!(result.status, JobStatus::Success) {
                                // Get the job's outputs from context and serialize them
                                if let Some(job_outputs) = context.outputs.get(&job_id) {
                                    let outputs_json = serde_json::to_value(job_outputs)
                                        .unwrap_or_else(|_| serde_json::json!({}));

                                    if let Err(e) =
                                        tracker.save_job_outputs(execution_id, outputs_json).await
                                    {
                                        error!("Failed to save outputs for job {}: {}", job_id, e);
                                    } else {
                                        debug!("✅ Saved outputs for job {}", job_id);
                                    }
                                }
                            }
                        } else {
                            warn!("Job {} has no execution_id, cannot update status", job_id);
                        }
                    }

                    // Check if required job failed
                    if job_state.job_config.required && result.status == JobStatus::Failure {
                        if !config.continue_on_failure {
                            error!("Required job '{}' failed, stopping workflow", job_id);

                            // Cancel all pending jobs before failing the workflow
                            if let Some(ref tracker) = self.job_tracker {
                                let cancel_reason = format!(
                                    "Required job '{}' failed: {}",
                                    job_id,
                                    result
                                        .message
                                        .as_ref()
                                        .unwrap_or(&"Unknown error".to_string())
                                );

                                if let Err(e) = tracker
                                    .cancel_pending_jobs(&config.workflow_run_id, cancel_reason)
                                    .await
                                {
                                    error!("Failed to cancel pending jobs: {}", e);
                                } else {
                                    info!("Cancelled all pending jobs due to required job failure");
                                }
                            }

                            return Err(WorkflowError::JobExecutionFailed(format!(
                                "Required job '{}' failed: {:?}",
                                job_id, result.message
                            )));
                        } else {
                            warn!("⚠️ Required job '{}' failed, but continuing due to continue_on_failure=true", job_id);
                        }
                    }
                }
            }
        }

        info!(
            "🎉 Workflow {} completed successfully",
            config.workflow_run_id
        );
        Ok(context)
    }

    /// Build dependency graph from job configs
    fn build_job_dependency_graph(
        &self,
        jobs: &[JobConfig],
    ) -> Result<HashMap<String, JobExecutionState>, WorkflowError> {
        let mut job_states = HashMap::new();

        // First pass: create all job states
        for job_config in jobs {
            let job_id = job_config.job.job_id().to_string();
            // Use dependencies_override if provided, otherwise use job.depends_on()
            let dependencies = job_config
                .dependencies_override
                .clone()
                .unwrap_or_else(|| job_config.job.depends_on());

            job_states.insert(
                job_id.clone(),
                JobExecutionState {
                    job_config: job_config.clone(),
                    status: JobStatus::Pending,
                    dependencies: dependencies.clone(),
                    dependents: Vec::new(),
                    execution_id: None,
                    result: None,
                },
            );
        }

        // Second pass: build reverse dependencies (dependents)
        let job_ids: Vec<String> = job_states.keys().cloned().collect();
        for job_id in &job_ids {
            let dependencies = job_states[job_id].dependencies.clone();
            for dep_job_id in dependencies {
                if let Some(dep_state) = job_states.get_mut(&dep_job_id) {
                    dep_state.dependents.push(job_id.clone());
                } else {
                    return Err(WorkflowError::JobNotFound(format!(
                        "Job '{}' depends on '{}' which doesn't exist",
                        job_id, dep_job_id
                    )));
                }
            }
        }

        Ok(job_states)
    }

    /// Validate that there are no dependency cycles
    fn validate_dependency_graph(
        &self,
        job_states: &HashMap<String, JobExecutionState>,
    ) -> Result<(), WorkflowError> {
        let mut visited = HashSet::new();
        let mut rec_stack = HashSet::new();

        for job_id in job_states.keys() {
            if !visited.contains(job_id)
                && Self::has_cycle(job_id, job_states, &mut visited, &mut rec_stack)?
            {
                return Err(WorkflowError::DependencyCycleDetected(format!(
                    "Dependency cycle detected involving job '{}'",
                    job_id
                )));
            }
        }

        Ok(())
    }

    /// Check for cycles using DFS
    fn has_cycle(
        job_id: &str,
        job_states: &HashMap<String, JobExecutionState>,
        visited: &mut HashSet<String>,
        rec_stack: &mut HashSet<String>,
    ) -> Result<bool, WorkflowError> {
        visited.insert(job_id.to_string());
        rec_stack.insert(job_id.to_string());

        if let Some(job_state) = job_states.get(job_id) {
            for dep_id in &job_state.dependencies {
                if !visited.contains(dep_id) {
                    if Self::has_cycle(dep_id, job_states, visited, rec_stack)? {
                        return Ok(true);
                    }
                } else if rec_stack.contains(dep_id) {
                    return Ok(true);
                }
            }
        }

        rec_stack.remove(job_id);
        Ok(false)
    }

    /// Calculate execution order respecting dependencies (topological sort)
    fn calculate_execution_order(
        &self,
        job_states: &HashMap<String, JobExecutionState>,
    ) -> Result<Vec<Vec<String>>, WorkflowError> {
        let mut in_degree: HashMap<String, usize> = HashMap::new();
        let mut order = Vec::new();

        // Calculate in-degree for each job
        for (job_id, job_state) in job_states {
            in_degree.insert(job_id.clone(), job_state.dependencies.len());
        }

        // Start with jobs that have no dependencies
        let mut queue: VecDeque<String> = job_states
            .keys()
            .filter(|job_id| in_degree[*job_id] == 0)
            .cloned()
            .collect();

        // Process jobs level by level
        while !queue.is_empty() {
            let current_batch: Vec<String> = queue.drain(..).collect();
            order.push(current_batch.clone());

            // Reduce in-degree for dependents
            for job_id in &current_batch {
                if let Some(job_state) = job_states.get(job_id) {
                    for dependent_id in &job_state.dependents {
                        if let Some(current_in_degree) = in_degree.get_mut(dependent_id) {
                            *current_in_degree -= 1;
                            if *current_in_degree == 0 {
                                queue.push_back(dependent_id.clone());
                            }
                        }
                    }
                }
            }
        }

        // Check if all jobs were processed (no cycles)
        let total_jobs_processed: usize = order.iter().map(|batch| batch.len()).sum();
        if total_jobs_processed != job_states.len() {
            return Err(WorkflowError::DependencyCycleDetected(
                "Unable to resolve all dependencies - cycle detected".to_string(),
            ));
        }

        Ok(order)
    }

    /// Move every still-`Pending` job in this workflow to `Cancelled`.
    ///
    /// Any path that leaves `execute_workflow` early must call this: jobs that
    /// never ran keep the status the planner created them with, and a row left at
    /// `Pending` reads as "still running" forever in the UI. Tracker failures are
    /// logged and swallowed — cancellation bookkeeping must not mask the original
    /// error that caused the abort.
    async fn cancel_remaining_pending(&self, workflow_run_id: &str, reason: String) {
        let Some(ref tracker) = self.job_tracker else {
            return;
        };

        warn!(
            "🧹 Cancelling all pending jobs in workflow {}",
            workflow_run_id
        );
        if let Err(e) = tracker.cancel_pending_jobs(workflow_run_id, reason).await {
            error!("Failed to cancel pending jobs: {}", e);
        }
    }

    /// Record a terminal status for a job that never entered execution.
    ///
    /// Jobs that are skipped or fail prerequisite validation never reach the
    /// batch-result handling in [`execute_workflow`](Self::execute_workflow), so
    /// without this their tracker row keeps whatever status the planner created it
    /// with (`Pending`) indefinitely. Tracker failures are logged and swallowed —
    /// they must not take down the surrounding workflow.
    async fn persist_terminal_status(
        &self,
        context: &WorkflowContext,
        job_id: &str,
        status: JobStatus,
        message: Option<String>,
    ) {
        let Some(ref tracker) = self.job_tracker else {
            return;
        };

        let execution_id = match tracker
            .create_job_execution(&context.workflow_run_id, job_id, status.clone())
            .await
        {
            Ok(id) => id,
            Err(e) => {
                error!(
                    "Failed to record terminal status {:?} for job '{}': {}",
                    status, job_id, e
                );
                return;
            }
        };

        if let Err(e) = tracker
            .update_job_status(execution_id, status.clone(), message)
            .await
        {
            error!(
                "Failed to update job '{}' (execution {}) to {:?}: {}",
                job_id, execution_id, status, e
            );
        } else {
            debug!(
                "✅ Recorded terminal status {:?} for job '{}'",
                status, job_id
            );
        }
    }

    /// Execute a batch of jobs in parallel
    async fn execute_job_batch(
        &self,
        batch: Vec<String>,
        job_states: &mut HashMap<String, JobExecutionState>,
        context: &mut WorkflowContext,
        semaphore: &Arc<Semaphore>,
        cancellation_provider: Arc<dyn WorkflowCancellationProvider>,
        continue_on_failure: bool,
    ) -> Result<Vec<(String, JobResult)>, WorkflowError> {
        info!("▶️ Executing job batch: {:?}", batch);

        let mut tasks = Vec::new();

        for job_id in batch {
            if let Some(job_state) = job_states.get_mut(&job_id) {
                // Check if job should be skipped
                if job_state.job_config.job.should_skip(context).await? {
                    info!("⏭️ Skipping job: {}", job_id);
                    job_state.status = JobStatus::Skipped;
                    // A job that never enters the batch results below must still reach a
                    // terminal state in the tracker, or its pre-created row stays Pending
                    // forever and the UI shows the step as still running.
                    //
                    // No message: `should_skip` is a deliberate opt-out (feature disabled,
                    // nothing to do), not a failure, and the executor doesn't know the
                    // job-specific reason — inventing one would put a fabricated string in
                    // the `error_message` column for a job that never errored.
                    self.persist_terminal_status(context, &job_id, JobStatus::Skipped, None)
                        .await;
                    continue;
                }

                // Validate prerequisites
                if let Err(e) = job_state
                    .job_config
                    .job
                    .validate_prerequisites(context)
                    .await
                {
                    let error_msg = format!("Prerequisites not met for job '{}': {}", job_id, e);
                    error!("{}", error_msg);

                    // Failed prerequisites are a real failure, not a silent skip — record
                    // them as Failure with the reason so the operator can act on it. This
                    // applies to optional jobs too: they are optional for *deployment
                    // completion*, not exempt from reporting why they didn't run.
                    job_state.status = JobStatus::Failure;
                    self.persist_terminal_status(
                        context,
                        &job_id,
                        JobStatus::Failure,
                        Some(error_msg.clone()),
                    )
                    .await;

                    if let Err(log_err) = context.log(&error_msg).await {
                        error!(
                            "Failed to log prerequisite failure for '{}': {}",
                            job_id, log_err
                        );
                    }

                    if job_state.job_config.required && !continue_on_failure {
                        return Err(e);
                    } else {
                        warn!(
                            "Not running job '{}' due to failed prerequisites: {}",
                            job_id, e
                        );
                        continue;
                    }
                }

                // Create job execution record if tracker is available
                if let Some(ref tracker) = self.job_tracker {
                    let execution_id = tracker
                        .create_job_execution(&context.workflow_run_id, &job_id, JobStatus::Running)
                        .await?;
                    job_state.execution_id = Some(execution_id);
                }

                // Clone necessary data for the async task
                let job = job_state.job_config.job.clone();
                let context_clone = context.clone();
                let semaphore_clone = semaphore.clone();
                let job_id_clone = job_id.clone();
                let tracker_clone = self.job_tracker.clone();

                // Spawn async task for job execution
                let cancellation_provider_clone = cancellation_provider.clone();
                let task = tokio::spawn(async move {
                    // Acquire semaphore permit
                    let _permit = semaphore_clone.acquire().await.expect("Semaphore closed");

                    info!("🏃 Starting job: {} ({})", job.name(), job_id_clone);

                    // Mark job as started if tracker is available
                    if let Some(ref _tracker) = tracker_clone {
                        // Note: We'd need the execution_id here, but it's complex to pass
                        // In practice, this would be handled differently
                    }

                    // Create a separate context clone for error handling
                    let error_context = context_clone.clone();

                    // Execute the job
                    let result = job
                        .execute_with_cancellation(
                            context_clone,
                            cancellation_provider_clone.as_ref(),
                        )
                        .await;

                    match result {
                        Ok(job_result) => {
                            info!(
                                "✅ Job '{}' completed with status: {}",
                                job_id_clone, job_result.status
                            );

                            // Add logs to tracker if available
                            if let Some(ref _tracker) = tracker_clone {
                                // Note: Same issue with execution_id
                                if !job_result.logs.is_empty() {
                                    let _ = _tracker.add_job_logs(0, job_result.logs.clone()).await;
                                    // Would use real execution_id
                                }
                            }

                            // Call cleanup if job was cancelled or failed
                            if matches!(
                                job_result.status,
                                crate::JobStatus::Failure | crate::JobStatus::Cancelled
                            ) {
                                warn!(
                                    "🧹 Calling cleanup for job '{}' due to {:?} status",
                                    job_id_clone, job_result.status
                                );
                                if let Err(cleanup_err) = job.cleanup(&job_result.context).await {
                                    error!(
                                        "Failed to cleanup job '{}': {}",
                                        job_id_clone, cleanup_err
                                    );
                                }
                            }

                            (job_id_clone, job_result)
                        }
                        Err(e) => {
                            let is_cancellation = Self::is_cancellation_error(&e);
                            let error_msg = if is_cancellation {
                                format!("Job cancelled: {}", e)
                            } else {
                                format!("Job failed: {}", e)
                            };
                            if is_cancellation {
                                warn!("Job '{}' cancelled: {}", job_id_clone, e);
                            } else {
                                error!("Job '{}' failed: {}", job_id_clone, e);
                            }

                            // Log the error to the job's context so it appears in the job logs
                            if let Err(log_err) = error_context.log(&error_msg).await {
                                error!(
                                    "Failed to log error for job '{}': {}",
                                    job_id_clone, log_err
                                );
                            }

                            // Call cleanup on job failure or cancellation
                            warn!("Calling cleanup for stopped job '{}'", job_id_clone);
                            if let Err(cleanup_err) = job.cleanup(&error_context).await {
                                error!("Failed to cleanup job '{}': {}", job_id_clone, cleanup_err);
                            }

                            (
                                job_id_clone,
                                if is_cancellation {
                                    JobResult::cancelled(error_context)
                                } else {
                                    JobResult::failure(error_context, e.to_string())
                                },
                            )
                        }
                    }
                });

                tasks.push(task);
            }
        }

        // Wait for all tasks to complete
        let results = join_all(tasks).await;
        let mut job_results = Vec::new();

        for result in results {
            match result {
                Ok((job_id, job_result)) => {
                    job_results.push((job_id, job_result));
                }
                Err(e) => {
                    error!("Task join error: {}", e);
                    return Err(WorkflowError::Other(format!(
                        "Task execution failed: {}",
                        e
                    )));
                }
            }
        }

        Ok(job_results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::WorkflowBuilder;
    use crate::workflow::WorkflowTask;
    use crate::LogWriter;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct TestJob {
        id: String,
        name: String,
        dependencies: Vec<String>,
        execution_order: Arc<AtomicUsize>,
        global_counter: Arc<AtomicUsize>,
    }

    impl TestJob {
        fn new(
            id: &str,
            name: &str,
            dependencies: Vec<String>,
            execution_order: Arc<AtomicUsize>,
            global_counter: Arc<AtomicUsize>,
        ) -> Self {
            Self {
                id: id.to_string(),
                name: name.to_string(),
                dependencies,
                execution_order,
                global_counter,
            }
        }
    }

    #[async_trait::async_trait]
    impl WorkflowTask for TestJob {
        fn job_id(&self) -> &str {
            &self.id
        }

        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "A test job"
        }

        async fn execute(&self, mut context: WorkflowContext) -> Result<JobResult, WorkflowError> {
            // Record execution order
            let order = self.global_counter.fetch_add(1, Ordering::SeqCst);
            self.execution_order.store(order, Ordering::SeqCst);

            // Set job output
            context.set_output(&self.id, "execution_order", order)?;
            context.set_output(&self.id, "result", "success")?;

            Ok(JobResult::success_with_message(
                context,
                format!("Executed job: {} (order: {})", self.name, order),
            ))
        }

        fn depends_on(&self) -> Vec<String> {
            self.dependencies.clone()
        }
    }

    struct TestCancellationProvider {
        cancelled: bool,
    }

    #[test]
    fn test_cancellation_errors_are_not_failures() {
        assert!(WorkflowExecutor::is_cancellation_error(
            &WorkflowError::WorkflowCancelled
        ));
        assert!(WorkflowExecutor::is_cancellation_error(
            &WorkflowError::BuildCancelled
        ));
        assert!(!WorkflowExecutor::is_cancellation_error(
            &WorkflowError::JobExecutionFailed("Build failed".to_string())
        ));
    }
    #[async_trait::async_trait]
    impl WorkflowCancellationProvider for TestCancellationProvider {
        async fn is_cancelled(&self, _workflow_run_id: &str) -> Result<bool, WorkflowError> {
            Ok(self.cancelled)
        }
    }

    // Mock LogWriter for testing
    struct MockLogWriter;

    #[async_trait::async_trait]
    impl LogWriter for MockLogWriter {
        async fn write_log(&self, _message: String) -> Result<(), WorkflowError> {
            Ok(())
        }

        fn stage_id(&self) -> i32 {
            1
        }
    }

    #[tokio::test]
    async fn test_workflow_execution_order() {
        let global_counter = Arc::new(AtomicUsize::new(0));

        // Create jobs with dependencies: job1 -> job2 -> job3
        let job1_order = Arc::new(AtomicUsize::new(999));
        let job2_order = Arc::new(AtomicUsize::new(999));
        let job3_order = Arc::new(AtomicUsize::new(999));

        let job1 = Arc::new(TestJob::new(
            "job1",
            "First Job",
            vec![],
            job1_order.clone(),
            global_counter.clone(),
        ));
        let job2 = Arc::new(TestJob::new(
            "job2",
            "Second Job",
            vec!["job1".to_string()],
            job2_order.clone(),
            global_counter.clone(),
        ));
        let job3 = Arc::new(TestJob::new(
            "job3",
            "Third Job",
            vec!["job2".to_string()],
            job3_order.clone(),
            global_counter.clone(),
        ));

        let log_writer = Arc::new(MockLogWriter);
        let config = WorkflowBuilder::new()
            .with_workflow_run_id("test-workflow".to_string())
            .with_deployment_context(1, 1, 1)
            .with_log_writer(log_writer)
            .with_jobs(vec![job3.clone(), job1.clone(), job2.clone()]) // Intentionally out of order
            .build()
            .unwrap();

        let executor = WorkflowExecutor::new(None);
        let cancellation_provider = Arc::new(TestCancellationProvider { cancelled: false });

        let result = executor
            .execute_workflow(config, cancellation_provider)
            .await;
        assert!(result.is_ok());

        let context = result.unwrap();

        // Verify execution order
        assert!(job1_order.load(Ordering::SeqCst) < job2_order.load(Ordering::SeqCst));
        assert!(job2_order.load(Ordering::SeqCst) < job3_order.load(Ordering::SeqCst));

        // Verify job outputs
        let job1_result: Option<String> = context.get_output("job1", "result").unwrap();
        assert_eq!(job1_result, Some("success".to_string()));

        let job3_order_output: Option<usize> =
            context.get_output("job3", "execution_order").unwrap();
        assert_eq!(job3_order_output, Some(2)); // Should be the third job executed (0-indexed)
    }

    #[tokio::test]
    async fn test_dependency_cycle_detection() {
        let global_counter = Arc::new(AtomicUsize::new(0));

        // Create jobs with circular dependency: job1 -> job2 -> job3 -> job1
        let job1 = Arc::new(TestJob::new(
            "job1",
            "First Job",
            vec!["job3".to_string()],
            Arc::new(AtomicUsize::new(0)),
            global_counter.clone(),
        ));
        let job2 = Arc::new(TestJob::new(
            "job2",
            "Second Job",
            vec!["job1".to_string()],
            Arc::new(AtomicUsize::new(0)),
            global_counter.clone(),
        ));
        let job3 = Arc::new(TestJob::new(
            "job3",
            "Third Job",
            vec!["job2".to_string()],
            Arc::new(AtomicUsize::new(0)),
            global_counter.clone(),
        ));

        let log_writer = Arc::new(MockLogWriter);
        let config = WorkflowBuilder::new()
            .with_workflow_run_id("test-workflow".to_string())
            .with_deployment_context(1, 1, 1)
            .with_log_writer(log_writer)
            .with_jobs(vec![job1, job2, job3])
            .build()
            .unwrap();

        let executor = WorkflowExecutor::new(None);
        let cancellation_provider = Arc::new(TestCancellationProvider { cancelled: false });

        let result = executor
            .execute_workflow(config, cancellation_provider)
            .await;
        assert!(result.is_err());

        if let Err(WorkflowError::DependencyCycleDetected(_)) = result {
            // Expected error
        } else {
            panic!("Expected DependencyCycleDetected error");
        }
    }

    /// A job that never runs: either its prerequisites fail or it opts out via
    /// `should_skip`. Mirrors `TakeScreenshotJob` when Chrome is missing.
    #[derive(Debug)]
    struct NonRunningJob {
        id: String,
        skip: bool,
        prerequisite_error: Option<String>,
        executed: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl WorkflowTask for NonRunningJob {
        fn job_id(&self) -> &str {
            &self.id
        }

        fn name(&self) -> &str {
            "Non-running job"
        }

        fn description(&self) -> &str {
            "A job that is skipped or fails prerequisite validation"
        }

        async fn execute(&self, context: WorkflowContext) -> Result<JobResult, WorkflowError> {
            self.executed.fetch_add(1, Ordering::SeqCst);
            Ok(JobResult::success(context))
        }

        async fn should_skip(&self, _context: &WorkflowContext) -> Result<bool, WorkflowError> {
            Ok(self.skip)
        }

        async fn validate_prerequisites(
            &self,
            _context: &WorkflowContext,
        ) -> Result<(), WorkflowError> {
            match &self.prerequisite_error {
                Some(msg) => Err(WorkflowError::JobValidationFailed(msg.clone())),
                None => Ok(()),
            }
        }

        fn depends_on(&self) -> Vec<String> {
            Vec::new()
        }
    }

    /// Records every status the executor persists, keyed by job id.
    #[derive(Default)]
    struct RecordingJobTracker {
        statuses: std::sync::Mutex<Vec<(String, JobStatus, Option<String>)>>,
        ids: std::sync::Mutex<HashMap<i32, String>>,
        cancellations: std::sync::Mutex<Vec<String>>,
    }

    impl RecordingJobTracker {
        /// Reasons passed to `cancel_pending_jobs` — i.e. the sweeps that would
        /// move leftover Pending rows to Cancelled.
        fn cancellations(&self) -> Vec<String> {
            self.cancellations
                .lock()
                .expect("cancellations lock poisoned")
                .clone()
        }

        fn statuses_for(&self, job_id: &str) -> Vec<(JobStatus, Option<String>)> {
            self.statuses
                .lock()
                .expect("statuses lock poisoned")
                .iter()
                .filter(|(id, _, _)| id == job_id)
                .map(|(_, status, msg)| (status.clone(), msg.clone()))
                .collect()
        }

        /// The status the deployment_jobs row would be left with.
        fn final_status(&self, job_id: &str) -> Option<(JobStatus, Option<String>)> {
            self.statuses_for(job_id).last().cloned()
        }
    }

    #[async_trait::async_trait]
    impl JobTracker for RecordingJobTracker {
        async fn create_job_execution(
            &self,
            _workflow_run_id: &str,
            job_id: &str,
            status: JobStatus,
        ) -> Result<i32, WorkflowError> {
            let mut ids = self.ids.lock().expect("ids lock poisoned");
            let execution_id = ids.len() as i32 + 1;
            ids.insert(execution_id, job_id.to_string());
            self.statuses.lock().expect("statuses lock poisoned").push((
                job_id.to_string(),
                status,
                None,
            ));
            Ok(execution_id)
        }

        async fn update_job_status(
            &self,
            job_execution_id: i32,
            status: JobStatus,
            message: Option<String>,
        ) -> Result<(), WorkflowError> {
            let job_id = self
                .ids
                .lock()
                .expect("ids lock poisoned")
                .get(&job_execution_id)
                .cloned()
                .ok_or_else(|| {
                    WorkflowError::Other(format!("Unknown execution id {}", job_execution_id))
                })?;
            self.statuses
                .lock()
                .expect("statuses lock poisoned")
                .push((job_id, status, message));
            Ok(())
        }

        async fn add_job_logs(&self, _id: i32, _logs: Vec<String>) -> Result<(), WorkflowError> {
            Ok(())
        }

        async fn mark_job_started(&self, _id: i32) -> Result<(), WorkflowError> {
            Ok(())
        }

        async fn mark_job_finished(&self, _id: i32) -> Result<(), WorkflowError> {
            Ok(())
        }

        async fn save_job_outputs(
            &self,
            _id: i32,
            _outputs: serde_json::Value,
        ) -> Result<(), WorkflowError> {
            Ok(())
        }

        async fn cancel_pending_jobs(
            &self,
            _workflow_run_id: &str,
            reason: String,
        ) -> Result<(), WorkflowError> {
            self.cancellations
                .lock()
                .expect("cancellations lock poisoned")
                .push(reason);
            Ok(())
        }
    }

    async fn run_with_tracker(
        job: Arc<dyn WorkflowTask>,
        required: bool,
        continue_on_failure: bool,
    ) -> (
        Arc<RecordingJobTracker>,
        Result<WorkflowContext, WorkflowError>,
    ) {
        let tracker = Arc::new(RecordingJobTracker::default());
        let config = WorkflowBuilder::new()
            .with_workflow_run_id("test-workflow".to_string())
            .with_deployment_context(1, 1, 1)
            .with_log_writer(Arc::new(MockLogWriter))
            .with_job_config(job, vec![], required)
            .continue_on_failure(continue_on_failure)
            .build()
            .expect("workflow config should build");

        let executor = WorkflowExecutor::new(Some(tracker.clone()));
        let result = executor
            .execute_workflow(
                config,
                Arc::new(TestCancellationProvider { cancelled: false }),
            )
            .await;

        (tracker, result)
    }

    /// Regression test for #477: an optional job whose prerequisites fail (e.g.
    /// `take_screenshot` on a server with no working Chrome) must be persisted as
    /// Failure with the reason — previously only an in-memory status was set, so
    /// the deployment_jobs row stayed Pending forever and the UI showed the step
    /// as perpetually "generating".
    #[tokio::test]
    async fn test_optional_job_failing_prerequisites_is_persisted_as_failure() {
        let executed = Arc::new(AtomicUsize::new(0));
        let job = Arc::new(NonRunningJob {
            id: "take_screenshot".to_string(),
            skip: false,
            prerequisite_error: Some(
                "Screenshot provider 'local-headless-chrome' is not available: \
                 Chrome browser error: Failed to launch Chrome browser"
                    .to_string(),
            ),
            executed: executed.clone(),
        });

        let (tracker, result) = run_with_tracker(job, false, true).await;

        // The optional job must not fail the workflow...
        assert!(
            result.is_ok(),
            "optional job should not fail the workflow: {:?}",
            result.err()
        );
        assert_eq!(executed.load(Ordering::SeqCst), 0, "job must not execute");

        // ...but it must reach a terminal, non-Pending state in the tracker.
        let (status, message) = tracker
            .final_status("take_screenshot")
            .expect("prerequisite failure must be persisted, not left Pending");
        assert_eq!(status, JobStatus::Failure);

        let message = message.expect("failure must carry a reason");
        assert!(
            message.contains("Failed to launch Chrome browser"),
            "reason must reach the user, got: {}",
            message
        );
    }

    /// A required job that fails prerequisites still fails the workflow, but must
    /// also leave a Failure row behind rather than a Pending one.
    #[tokio::test]
    async fn test_required_job_failing_prerequisites_is_persisted_before_erroring() {
        let job = Arc::new(NonRunningJob {
            id: "deploy".to_string(),
            skip: false,
            prerequisite_error: Some("docker daemon unreachable".to_string()),
            executed: Arc::new(AtomicUsize::new(0)),
        });

        let (tracker, result) = run_with_tracker(job, true, false).await;

        assert!(matches!(result, Err(WorkflowError::JobValidationFailed(_))));

        let (status, message) = tracker
            .final_status("deploy")
            .expect("prerequisite failure must be persisted, not left Pending");
        assert_eq!(status, JobStatus::Failure);
        assert!(message
            .unwrap_or_default()
            .contains("docker daemon unreachable"));
    }

    /// Aborting out of a batch must sweep the jobs that never got to run.
    ///
    /// The early `return Err` on a required job's failed prerequisites bypasses the
    /// per-result handling that normally cancels the rest of the workflow, so every
    /// downstream job kept the planner's `Pending` status forever — the same stuck
    /// state as #477, one branch over.
    #[tokio::test]
    async fn test_aborting_batch_cancels_jobs_that_never_ran() {
        let downstream_executed = Arc::new(AtomicUsize::new(0));

        let failing = Arc::new(NonRunningJob {
            id: "deploy".to_string(),
            skip: false,
            prerequisite_error: Some("docker daemon unreachable".to_string()),
            executed: Arc::new(AtomicUsize::new(0)),
        });
        let downstream = Arc::new(NonRunningJob {
            id: "take_screenshot".to_string(),
            skip: false,
            prerequisite_error: None,
            executed: downstream_executed.clone(),
        });

        let tracker = Arc::new(RecordingJobTracker::default());
        let config = WorkflowBuilder::new()
            .with_workflow_run_id("test-workflow".to_string())
            .with_deployment_context(1, 1, 1)
            .with_log_writer(Arc::new(MockLogWriter))
            .with_job_config(failing, vec![], true)
            .with_job_config(downstream, vec!["deploy".to_string()], false)
            .continue_on_failure(false)
            .build()
            .expect("workflow config should build");

        let executor = WorkflowExecutor::new(Some(tracker.clone()));
        let result = executor
            .execute_workflow(
                config,
                Arc::new(TestCancellationProvider { cancelled: false }),
            )
            .await;

        assert!(
            result.is_err(),
            "required job failure must abort the workflow"
        );
        assert_eq!(
            downstream_executed.load(Ordering::SeqCst),
            0,
            "downstream job must not run"
        );

        // take_screenshot never reached the executor, so only the sweep can move it
        // off Pending.
        assert!(
            tracker.final_status("take_screenshot").is_none(),
            "sanity: the downstream job should have no status of its own"
        );
        let cancellations = tracker.cancellations();
        assert_eq!(
            cancellations.len(),
            1,
            "aborting the workflow must cancel jobs left Pending, got: {:?}",
            cancellations
        );
        assert!(
            cancellations[0].contains("docker daemon unreachable"),
            "cancellation reason should explain the abort, got: {}",
            cancellations[0]
        );
    }

    /// A job that opts out via `should_skip` must be persisted as Skipped.
    #[tokio::test]
    async fn test_skipped_job_is_persisted_as_skipped() {
        let executed = Arc::new(AtomicUsize::new(0));
        let job = Arc::new(NonRunningJob {
            id: "configure_crons".to_string(),
            skip: true,
            prerequisite_error: None,
            executed: executed.clone(),
        });

        let (tracker, result) = run_with_tracker(job, false, true).await;

        assert!(result.is_ok());
        assert_eq!(executed.load(Ordering::SeqCst), 0, "job must not execute");

        let (status, message) = tracker
            .final_status("configure_crons")
            .expect("skipped job must be persisted, not left Pending");
        assert_eq!(status, JobStatus::Skipped);
        assert!(
            message.is_none(),
            "an intentional skip is not an error and the executor doesn't know the \
             job's reason — it must not fabricate one, got: {:?}",
            message
        );
    }
}
