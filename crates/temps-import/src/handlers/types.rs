//! Request and response types for import handlers

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_import_types::{
    ImportPlan, ImportSelector, ImportSource, ValidationReport, WorkloadDescriptor, WorkloadId,
};
use utoipa::ToSchema;

use crate::services::ImportOrchestrator;

/// Application state for handlers
pub struct AppState {
    pub import_orchestrator: Arc<ImportOrchestrator>,
}

/// Information about an import source
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImportSourceInfo {
    /// Source identifier
    pub source: ImportSource,
    /// Human-readable name
    pub name: String,
    /// Source version
    pub version: String,
    /// Whether the source is currently available
    pub available: bool,
    /// Capabilities
    pub capabilities: ImportSourceCapabilities,
}

/// Source capabilities
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImportSourceCapabilities {
    pub supports_volumes: bool,
    pub supports_networks: bool,
    pub supports_health_checks: bool,
    pub supports_resource_limits: bool,
    pub supports_build: bool,
}

/// Request to discover workloads
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct DiscoverRequest {
    /// Source to discover from
    pub source: ImportSource,
    /// Optional selector to filter workloads
    #[serde(default)]
    pub selector: ImportSelector,
}

/// Response with discovered workloads
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DiscoverResponse {
    /// Discovered workloads
    pub workloads: Vec<WorkloadDescriptor>,
}

/// Request to create an import plan
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreatePlanRequest {
    /// Source to import from
    pub source: ImportSource,
    /// Workload ID to import
    pub workload_id: WorkloadId,
    /// Optional repository ID to associate with the import
    /// If provided, preset will be detected from the repository
    pub repository_id: Option<i32>,
}

/// Response with created plan
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CreatePlanResponse {
    /// Session ID for tracking
    pub session_id: String,
    /// Generated import plan
    pub plan: ImportPlan,
    /// Validation report
    pub validation: ValidationReport,
    /// Whether the plan can be executed
    pub can_execute: bool,
}

/// Request to execute an import
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ExecuteImportRequest {
    /// Session ID from plan creation
    pub session_id: String,
    /// Project name to use (overrides the name from the plan)
    #[schema(example = "my-app")]
    pub project_name: String,
    /// Preset to use for the project (e.g., "nextjs", "express", "docker")
    pub preset: String,
    /// Project directory
    #[schema(example = ".")]
    pub directory: String,
    /// Main branch name
    #[schema(example = "main")]
    pub main_branch: String,
    /// Dry run mode (don't create resources)
    pub dry_run: Option<bool>,
}

/// Response from import execution
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ExecuteImportResponse {
    /// Session ID
    pub session_id: String,
    /// Execution status
    pub status: ImportExecutionStatus,
    /// Created project ID (if completed)
    pub project_id: Option<i32>,
    /// Created environment ID (if completed)
    pub environment_id: Option<i32>,
    /// Created deployment ID (if completed)
    pub deployment_id: Option<i32>,
}

/// Import execution status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ImportExecutionStatus {
    /// Import is pending
    Pending,
    /// Import is in progress
    InProgress,
    /// Import completed successfully
    Completed,
    /// Import failed
    Failed,
}

/// Response with import status
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportStatusResponse {
    /// Session ID
    pub session_id: String,
    /// Current status
    pub status: ImportExecutionStatus,
    /// Import plan
    pub plan: Option<ImportPlan>,
    /// Validation report
    pub validation: Option<ValidationReport>,
    /// Created project ID
    pub project_id: Option<i32>,
    /// Created environment ID
    pub environment_id: Option<i32>,
    /// Created deployment ID
    pub deployment_id: Option<i32>,
    /// Errors (if any)
    pub errors: Vec<String>,
    /// Warnings (if any)
    pub warnings: Vec<String>,
    /// Created at timestamp
    #[serde(with = "chrono::serde::ts_seconds")]
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Updated at timestamp
    #[serde(with = "chrono::serde::ts_seconds")]
    pub updated_at: chrono::DateTime<chrono::Utc>,
}
