//! GitHub Actions-style workflow system
//!
//! This module provides a builder-based API for creating deployment pipelines
//! similar to GitHub Actions workflows, with jobs, dependencies, outputs, and artifacts.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum WorkflowError {
    #[error("Job execution failed: {0}")]
    JobExecutionFailed(String),

    #[error("Job validation failed: {0}")]
    JobValidationFailed(String),

    #[error("Dependency cycle detected: {0}")]
    DependencyCycleDetected(String),

    #[error("Job not found: {0}")]
    JobNotFound(String),

    #[error("Workflow was cancelled")]
    WorkflowCancelled,

    #[error("Build was cancelled")]
    BuildCancelled,

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("Other error: {0}")]
    Other(String),
}

/// Trait for writing logs in real-time during workflow execution
/// Each LogWriter is associated with a specific deployment stage and writes to its log file
#[async_trait]
pub trait LogWriter: Send + Sync {
    /// Write a log line to the stage's log file
    async fn write_log(&self, message: String) -> Result<(), WorkflowError>;

    /// Write multiple log lines at once
    async fn write_logs(&self, messages: Vec<String>) -> Result<(), WorkflowError> {
        for message in messages {
            self.write_log(message).await?;
        }
        Ok(())
    }

    /// Get the stage ID this log writer is associated with
    fn stage_id(&self) -> i32;
}

/// Status of a job execution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum JobStatus {
    /// Job is waiting to be executed (dependencies not ready)
    Pending,
    /// Job is waiting for dependencies to complete
    Waiting,
    /// Job is currently running
    Running,
    /// Job completed successfully
    Success,
    /// Job failed
    Failure,
    /// Job was cancelled
    Cancelled,
    /// Job was skipped due to conditions
    Skipped,
}

impl std::fmt::Display for JobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobStatus::Pending => write!(f, "pending"),
            JobStatus::Waiting => write!(f, "waiting"),
            JobStatus::Running => write!(f, "running"),
            JobStatus::Success => write!(f, "success"),
            JobStatus::Failure => write!(f, "failure"),
            JobStatus::Cancelled => write!(f, "cancelled"),
            JobStatus::Skipped => write!(f, "skipped"),
        }
    }
}

/// Context that flows between jobs in a workflow
pub struct WorkflowContext {
    /// Workflow run ID
    pub workflow_run_id: String,
    /// Deployment ID
    pub deployment_id: i32,
    /// Project ID
    pub project_id: i32,
    /// Environment ID
    pub environment_id: i32,
    /// Global context variables
    pub vars: HashMap<String, serde_json::Value>,
    /// Job outputs (job_id -> output_name -> value)
    pub outputs: HashMap<String, HashMap<String, serde_json::Value>>,
    /// Job artifacts (job_id -> artifact_name -> file_path)
    pub artifacts: HashMap<String, HashMap<String, PathBuf>>,
    /// Working directory for this workflow
    pub work_dir: Option<PathBuf>,
    /// Log writer for real-time logging to deployment stage log file (required)
    pub log_writer: Arc<dyn LogWriter>,
}

impl WorkflowContext {
    pub fn new(
        workflow_run_id: String,
        deployment_id: i32,
        project_id: i32,
        environment_id: i32,
        log_writer: Arc<dyn LogWriter>,
    ) -> Self {
        Self {
            workflow_run_id,
            deployment_id,
            project_id,
            environment_id,
            vars: HashMap::new(),
            outputs: HashMap::new(),
            artifacts: HashMap::new(),
            work_dir: None,
            log_writer,
        }
    }

    /// Write a log line to the stage's log file
    pub async fn log(&self, message: impl Into<String>) -> Result<(), WorkflowError> {
        self.log_writer.write_log(message.into()).await
    }

    /// Write multiple log lines to the stage's log file
    pub async fn log_lines(&self, messages: Vec<String>) -> Result<(), WorkflowError> {
        self.log_writer.write_logs(messages).await
    }

    /// Set a context variable
    pub fn set_var<T: Serialize>(&mut self, key: &str, value: T) -> Result<(), WorkflowError> {
        self.vars
            .insert(key.to_string(), serde_json::to_value(value)?);
        Ok(())
    }

    /// Get a context variable
    pub fn get_var<T: for<'de> Deserialize<'de>>(
        &self,
        key: &str,
    ) -> Result<Option<T>, WorkflowError> {
        if let Some(value) = self.vars.get(key) {
            Ok(Some(serde_json::from_value(value.clone())?))
        } else {
            Ok(None)
        }
    }

    /// Set a job output
    pub fn set_output<T: Serialize>(
        &mut self,
        job_id: &str,
        output_name: &str,
        value: T,
    ) -> Result<(), WorkflowError> {
        let job_outputs = self.outputs.entry(job_id.to_string()).or_default();
        job_outputs.insert(output_name.to_string(), serde_json::to_value(value)?);
        Ok(())
    }

    /// Get a job output
    pub fn get_output<T: for<'de> Deserialize<'de>>(
        &self,
        job_id: &str,
        output_name: &str,
    ) -> Result<Option<T>, WorkflowError> {
        if let Some(job_outputs) = self.outputs.get(job_id) {
            if let Some(value) = job_outputs.get(output_name) {
                return Ok(Some(serde_json::from_value(value.clone())?));
            }
        }
        Ok(None)
    }

    /// Set a job artifact
    pub fn set_artifact(&mut self, job_id: &str, artifact_name: &str, file_path: PathBuf) {
        let job_artifacts = self.artifacts.entry(job_id.to_string()).or_default();
        job_artifacts.insert(artifact_name.to_string(), file_path);
    }

    /// Get a job artifact
    pub fn get_artifact(&self, job_id: &str, artifact_name: &str) -> Option<&PathBuf> {
        self.artifacts.get(job_id)?.get(artifact_name)
    }
}

impl Clone for WorkflowContext {
    fn clone(&self) -> Self {
        Self {
            workflow_run_id: self.workflow_run_id.clone(),
            deployment_id: self.deployment_id,
            project_id: self.project_id,
            environment_id: self.environment_id,
            vars: self.vars.clone(),
            outputs: self.outputs.clone(),
            artifacts: self.artifacts.clone(),
            work_dir: self.work_dir.clone(),
            log_writer: Arc::clone(&self.log_writer),
        }
    }
}

impl std::fmt::Debug for WorkflowContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowContext")
            .field("workflow_run_id", &self.workflow_run_id)
            .field("deployment_id", &self.deployment_id)
            .field("project_id", &self.project_id)
            .field("environment_id", &self.environment_id)
            .field("vars", &self.vars)
            .field("outputs", &self.outputs)
            .field("artifacts", &self.artifacts)
            .field("work_dir", &self.work_dir)
            .field("log_writer", &"<LogWriter>")
            .finish()
    }
}

/// Result of job execution
#[derive(Debug, Clone)]
pub struct JobResult {
    /// Updated context after job execution
    pub context: WorkflowContext,
    /// Status of the job execution
    pub status: JobStatus,
    /// Optional message about the execution
    pub message: Option<String>,
    /// Log output from the job
    pub logs: Vec<String>,
}

impl JobResult {
    pub fn success(context: WorkflowContext) -> Self {
        Self {
            context,
            status: JobStatus::Success,
            message: None,
            logs: vec![],
        }
    }

    pub fn success_with_message(context: WorkflowContext, message: String) -> Self {
        Self {
            context,
            status: JobStatus::Success,
            message: Some(message),
            logs: vec![],
        }
    }

    pub fn success_with_logs(context: WorkflowContext, message: String, logs: Vec<String>) -> Self {
        Self {
            context,
            status: JobStatus::Success,
            message: Some(message),
            logs,
        }
    }

    pub fn failure(context: WorkflowContext, message: String) -> Self {
        Self {
            context,
            status: JobStatus::Failure,
            message: Some(message),
            logs: vec![],
        }
    }

    pub fn failure_with_logs(context: WorkflowContext, message: String, logs: Vec<String>) -> Self {
        Self {
            context,
            status: JobStatus::Failure,
            message: Some(message),
            logs,
        }
    }

    pub fn cancelled(context: WorkflowContext) -> Self {
        Self {
            context,
            status: JobStatus::Cancelled,
            message: Some("Job was cancelled".to_string()),
            logs: vec![],
        }
    }

    pub fn skipped(context: WorkflowContext, reason: String) -> Self {
        Self {
            context,
            status: JobStatus::Skipped,
            message: Some(reason),
            logs: vec![],
        }
    }
}

/// Trait for checking if a workflow should be cancelled
#[async_trait]
pub trait WorkflowCancellationProvider: Send + Sync {
    async fn is_cancelled(&self, workflow_run_id: &str) -> Result<bool, WorkflowError>;
}

/// Core trait that all workflow tasks must implement
#[async_trait]
pub trait WorkflowTask: Send + Sync + std::fmt::Debug {
    /// Unique identifier for this job instance
    fn job_id(&self) -> &str;

    /// Human-readable name of the job
    fn name(&self) -> &str;

    /// Description of what this job does
    fn description(&self) -> &str;

    /// Execute the job with the given context
    async fn execute(&self, context: WorkflowContext) -> Result<JobResult, WorkflowError>;

    /// Execute the job with cancellation support (default implementation)
    async fn execute_with_cancellation(
        &self,
        context: WorkflowContext,
        cancellation_provider: &dyn WorkflowCancellationProvider,
    ) -> Result<JobResult, WorkflowError> {
        // Check for cancellation before starting
        if cancellation_provider
            .is_cancelled(&context.workflow_run_id)
            .await?
        {
            // Write cancellation log immediately to persistent storage
            let cancel_msg = format!(
                "🛑 DEPLOYMENT CANCELLED: Job '{}' will not execute",
                self.name()
            );
            let _ = context.log(&cancel_msg).await;

            let mut result = JobResult::cancelled(context);
            result.logs.push(cancel_msg);
            return Ok(result);
        }

        // Execute the job
        let result = self.execute(context).await;

        // Check for cancellation after execution
        if let Ok(ref job_result) = result {
            if cancellation_provider
                .is_cancelled(&job_result.context.workflow_run_id)
                .await?
            {
                // Write cancellation log immediately to persistent storage
                let cancel_msg = format!(
                    "🛑 DEPLOYMENT CANCELLED: Job '{}' was cancelled after completion",
                    self.name()
                );
                let _ = job_result.context.log(&cancel_msg).await;

                let mut cancelled_result = JobResult::cancelled(job_result.context.clone());
                cancelled_result.logs.push(cancel_msg);
                return Ok(cancelled_result);
            }
        }

        result
    }

    /// Check if this job should be skipped based on context
    async fn should_skip(&self, _context: &WorkflowContext) -> Result<bool, WorkflowError> {
        Ok(false)
    }

    /// List of job IDs that must complete successfully before this job can run
    fn depends_on(&self) -> Vec<String> {
        vec![]
    }

    /// Validate that the context has everything needed for this job
    async fn validate_prerequisites(
        &self,
        _context: &WorkflowContext,
    ) -> Result<(), WorkflowError> {
        Ok(())
    }

    /// Cleanup resources if the job fails or is cancelled
    async fn cleanup(&self, _context: &WorkflowContext) -> Result<(), WorkflowError> {
        Ok(())
    }
}

/// Configuration for a job in a workflow
#[derive(Debug, Clone)]
pub struct JobConfig {
    /// The job implementation
    pub job: Arc<dyn WorkflowTask>,
    /// Whether this job is required for workflow success
    pub required: bool,
    /// Condition to evaluate before running this job
    pub condition: Option<String>,
    /// Override dependencies (if None, uses job.depends_on())
    pub dependencies_override: Option<Vec<String>>,
}

/// Workflow configuration
pub struct WorkflowConfig {
    /// Workflow run ID
    pub workflow_run_id: String,
    /// Deployment context
    pub deployment_id: i32,
    pub project_id: i32,
    pub environment_id: i32,
    /// Initial context variables
    pub initial_context: HashMap<String, serde_json::Value>,
    /// Jobs to execute
    pub jobs: Vec<JobConfig>,
    /// Whether to continue on job failures (for optional jobs)
    pub continue_on_failure: bool,
    /// Maximum number of parallel jobs
    pub max_parallel_jobs: usize,
    /// Log writer for workflow execution
    pub log_writer: Arc<dyn LogWriter>,
}

impl std::fmt::Debug for WorkflowConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowConfig")
            .field("workflow_run_id", &self.workflow_run_id)
            .field("deployment_id", &self.deployment_id)
            .field("project_id", &self.project_id)
            .field("environment_id", &self.environment_id)
            .field("initial_context", &self.initial_context)
            .field("jobs", &self.jobs.len())
            .field("continue_on_failure", &self.continue_on_failure)
            .field("max_parallel_jobs", &self.max_parallel_jobs)
            .field("log_writer", &"<LogWriter>")
            .finish()
    }
}

/// Builder for creating workflows
pub struct WorkflowBuilder {
    workflow_run_id: Option<String>,
    deployment_id: Option<i32>,
    project_id: Option<i32>,
    environment_id: Option<i32>,
    initial_context: HashMap<String, serde_json::Value>,
    jobs: Vec<JobConfig>,
    continue_on_failure: bool,
    max_parallel_jobs: usize,
    log_writer: Option<Arc<dyn LogWriter>>,
}

impl WorkflowBuilder {
    pub fn new() -> Self {
        Self {
            workflow_run_id: None,
            deployment_id: None,
            project_id: None,
            environment_id: None,
            initial_context: HashMap::new(),
            jobs: Vec::new(),
            continue_on_failure: true,
            max_parallel_jobs: 1, // Sequential by default
            log_writer: None,
        }
    }

    /// Set the log writer for workflow execution
    pub fn with_log_writer(mut self, log_writer: Arc<dyn LogWriter>) -> Self {
        self.log_writer = Some(log_writer);
        self
    }

    /// Set the workflow run ID
    pub fn with_workflow_run_id(mut self, workflow_run_id: String) -> Self {
        self.workflow_run_id = Some(workflow_run_id);
        self
    }

    /// Set the deployment context
    pub fn with_deployment_context(
        mut self,
        deployment_id: i32,
        project_id: i32,
        environment_id: i32,
    ) -> Self {
        self.deployment_id = Some(deployment_id);
        self.project_id = Some(project_id);
        self.environment_id = Some(environment_id);
        self
    }

    /// Add context variables
    pub fn with_context(mut self, context: HashMap<String, serde_json::Value>) -> Self {
        self.initial_context.extend(context);
        self
    }

    /// Add a context variable
    pub fn with_var<T: Serialize>(mut self, key: &str, value: T) -> Result<Self, WorkflowError> {
        self.initial_context
            .insert(key.to_string(), serde_json::to_value(value)?);
        Ok(self)
    }

    /// Add a required job
    pub fn with_job(mut self, job: Arc<dyn WorkflowTask>) -> Self {
        self.jobs.push(JobConfig {
            job,
            required: true,
            condition: None,
            dependencies_override: None,
        });
        self
    }

    /// Add an optional job
    pub fn with_optional_job(mut self, job: Arc<dyn WorkflowTask>) -> Self {
        self.jobs.push(JobConfig {
            job,
            required: false,
            condition: None,
            dependencies_override: None,
        });
        self
    }

    /// Add a conditional job
    pub fn with_conditional_job(mut self, job: Arc<dyn WorkflowTask>, condition: String) -> Self {
        self.jobs.push(JobConfig {
            job,
            required: false,
            condition: Some(condition),
            dependencies_override: None,
        });
        self
    }

    /// Add a job with custom dependencies (overrides job.depends_on())
    pub fn with_job_and_dependencies(
        mut self,
        job: Arc<dyn WorkflowTask>,
        dependencies: Vec<String>,
    ) -> Self {
        self.jobs.push(JobConfig {
            job,
            required: true,
            condition: None,
            dependencies_override: Some(dependencies),
        });
        self
    }

    /// Add multiple jobs
    pub fn with_jobs(mut self, jobs: Vec<Arc<dyn WorkflowTask>>) -> Self {
        for job in jobs {
            self.jobs.push(JobConfig {
                job,
                required: true,
                condition: None,
                dependencies_override: None,
            });
        }
        self
    }

    /// Set whether to continue on job failures
    pub fn continue_on_failure(mut self, continue_on_failure: bool) -> Self {
        self.continue_on_failure = continue_on_failure;
        self
    }

    /// Set maximum parallel jobs
    pub fn with_max_parallel_jobs(mut self, max_parallel_jobs: usize) -> Self {
        self.max_parallel_jobs = max_parallel_jobs;
        self
    }

    /// Build the workflow configuration
    pub fn build(self) -> Result<WorkflowConfig, WorkflowError> {
        let workflow_run_id = self.workflow_run_id.ok_or_else(|| {
            WorkflowError::JobValidationFailed("workflow_run_id is required".to_string())
        })?;

        let deployment_id = self.deployment_id.ok_or_else(|| {
            WorkflowError::JobValidationFailed("deployment_id is required".to_string())
        })?;

        let project_id = self.project_id.ok_or_else(|| {
            WorkflowError::JobValidationFailed("project_id is required".to_string())
        })?;

        let environment_id = self.environment_id.ok_or_else(|| {
            WorkflowError::JobValidationFailed("environment_id is required".to_string())
        })?;

        let log_writer = self.log_writer.ok_or_else(|| {
            WorkflowError::JobValidationFailed("log_writer is required".to_string())
        })?;

        Ok(WorkflowConfig {
            workflow_run_id,
            deployment_id,
            project_id,
            environment_id,
            initial_context: self.initial_context,
            jobs: self.jobs,
            continue_on_failure: self.continue_on_failure,
            max_parallel_jobs: self.max_parallel_jobs,
            log_writer,
        })
    }
}

impl Default for WorkflowBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct TestJob {
        id: String,
        name: String,
    }

    impl TestJob {
        fn new(id: &str, name: &str) -> Self {
            Self {
                id: id.to_string(),
                name: name.to_string(),
            }
        }
    }

    #[async_trait]
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

        async fn execute(&self, context: WorkflowContext) -> Result<JobResult, WorkflowError> {
            Ok(JobResult::success_with_message(
                context,
                format!("Executed job: {}", self.name),
            ))
        }
    }

    #[tokio::test]
    async fn test_workflow_builder() {
        let log_writer = Arc::new(MockLogWriter);
        let workflow = WorkflowBuilder::new()
            .with_workflow_run_id("test-run-123".to_string())
            .with_deployment_context(1, 1, 1)
            .with_log_writer(log_writer)
            .with_var("test_var", "test_value")
            .unwrap()
            .with_job(Arc::new(TestJob::new("job1", "First Job")))
            .with_job(Arc::new(TestJob::new("job2", "Second Job")))
            .continue_on_failure(true)
            .build()
            .unwrap();

        assert_eq!(workflow.workflow_run_id, "test-run-123");
        assert_eq!(workflow.jobs.len(), 2);
        assert_eq!(workflow.jobs[0].job.job_id(), "job1");
        assert_eq!(workflow.jobs[1].job.job_id(), "job2");
    }

    // Mock LogWriter for testing
    struct MockLogWriter;

    #[async_trait]
    impl LogWriter for MockLogWriter {
        async fn write_log(&self, _message: String) -> Result<(), WorkflowError> {
            Ok(())
        }

        fn stage_id(&self) -> i32 {
            1
        }
    }

    #[tokio::test]
    async fn test_workflow_context() {
        let log_writer = Arc::new(MockLogWriter);
        let mut context = WorkflowContext::new("test-run".to_string(), 1, 1, 1, log_writer);

        // Test context variables
        context.set_var("test_key", "test_value").unwrap();
        let value: Option<String> = context.get_var("test_key").unwrap();
        assert_eq!(value, Some("test_value".to_string()));

        // Test job outputs
        context.set_output("job1", "result", "success").unwrap();
        let output: Option<String> = context.get_output("job1", "result").unwrap();
        assert_eq!(output, Some("success".to_string()));

        // Test artifacts
        context.set_artifact(
            "job1",
            "build_artifact",
            PathBuf::from("/tmp/artifact.tar.gz"),
        );
        let artifact = context.get_artifact("job1", "build_artifact");
        assert_eq!(artifact, Some(&PathBuf::from("/tmp/artifact.tar.gz")));
    }
}
