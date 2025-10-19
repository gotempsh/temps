//! Stage system types
//!
//! Legacy stage system that works alongside the new workflow system.
//! These are kept for backward compatibility with existing stage-based deployments.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;
use crate::UtcDateTime;
/// Stage execution status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StageStatus {
    /// Stage is pending execution
    Pending,
    /// Stage is currently running
    Running,
    /// Stage completed successfully
    Success,
    /// Stage failed
    Failed,
    /// Stage was cancelled
    Cancelled,
    /// Stage was skipped
    Skipped,
}

impl fmt::Display for StageStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StageStatus::Pending => write!(f, "pending"),
            StageStatus::Running => write!(f, "running"),
            StageStatus::Success => write!(f, "success"),
            StageStatus::Failed => write!(f, "failed"),
            StageStatus::Cancelled => write!(f, "cancelled"),
            StageStatus::Skipped => write!(f, "skipped"),
        }
    }
}

/// Stage execution errors
#[derive(Error, Debug)]
pub enum StageError {
    #[error("Database error: {0}")]
    Database(String),

    #[error("Stage execution failed: {0}")]
    ExecutionFailed(String),

    #[error("Stage not found")]
    NotFound,

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Other error: {0}")]
    Other(String),
}

/// Stage execution information
#[derive(Debug, Clone)]
pub struct StageExecutionInfo {
    pub execution_id: i32,
    pub deployment_id: i32,
    pub pipeline_id: i32,
    pub stage_type: String,
    pub status: StageStatus,
    pub log_file_path: String,
    pub started_at: UtcDateTime,
    pub finished_at: Option<UtcDateTime>,
    pub error_message: Option<String>,
}

/// Trait for tracking stage executions
#[async_trait]
pub trait StageTracker: Send + Sync {
    /// Create a new stage execution record
    async fn create_stage_execution(
        &self,
        deployment_id: i32,
        pipeline_id: i32,
        stage_type: &str,
        status: StageStatus,
    ) -> Result<i32, StageError>;

    /// Update stage execution status
    async fn update_stage_execution(
        &self,
        execution_id: i32,
        status: StageStatus,
        error_message: Option<String>,
    ) -> Result<(), StageError>;

    /// Get stage execution information
    async fn get_stage_execution(
        &self,
        execution_id: i32,
    ) -> Result<StageExecutionInfo, StageError>;

    /// Add log lines to a stage execution
    async fn add_stage_logs(
        &self,
        execution_id: i32,
        logs: Vec<String>,
    ) -> Result<(), StageError>;

    /// Get all stage executions for a deployment
    async fn get_deployment_stages(
        &self,
        deployment_id: i32,
    ) -> Result<Vec<StageExecutionInfo>, StageError>;
}