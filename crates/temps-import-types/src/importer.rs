//! Workload importer trait
//!
//! Defines the interface that all importer implementations must provide.
//! This is generic for all workload types: containers, serverless functions, static sites, etc.

use crate::{
    error::ImportResult,
    plan::ImportPlan,
    snapshot::{WorkloadDescriptor, WorkloadId, WorkloadSnapshot},
    validation::{ImportValidationRule, ValidationReport},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::ToSchema;

/// Import source identifier
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ImportSource {
    /// Docker Engine API (containers)
    Docker,
    /// Coolify platform (containers, static sites, databases)
    Coolify,
    /// Dokploy platform (containers, databases, applications)
    Dokploy,
    /// Vercel deployments (serverless functions, static sites, edge functions)
    Vercel,
    /// Netlify deployments (static sites, serverless functions)
    Netlify,
    /// Railway deployments (containers, databases, services)
    Railway,
    /// Render deployments (containers, static sites, services)
    Render,
    /// Fly.io deployments (containers, machines)
    Fly,
    /// Custom/other source
    Custom,
}

impl ImportSource {
    /// Get the string identifier for this source
    pub fn as_str(&self) -> &str {
        match self {
            ImportSource::Docker => "docker",
            ImportSource::Coolify => "coolify",
            ImportSource::Dokploy => "dokploy",
            ImportSource::Vercel => "vercel",
            ImportSource::Netlify => "netlify",
            ImportSource::Railway => "railway",
            ImportSource::Render => "render",
            ImportSource::Fly => "fly",
            ImportSource::Custom => "custom",
        }
    }

    /// Parse ImportSource from string
    pub fn from_str(s: &str) -> Result<Self, crate::ImportError> {
        match s.to_lowercase().as_str() {
            "docker" => Ok(ImportSource::Docker),
            "coolify" => Ok(ImportSource::Coolify),
            "dokploy" => Ok(ImportSource::Dokploy),
            "vercel" => Ok(ImportSource::Vercel),
            "netlify" => Ok(ImportSource::Netlify),
            "railway" => Ok(ImportSource::Railway),
            "render" => Ok(ImportSource::Render),
            "fly" => Ok(ImportSource::Fly),
            "custom" => Ok(ImportSource::Custom),
            _ => Err(crate::ImportError::SourceNotAccessible(format!(
                "Unknown import source: {}",
                s
            ))),
        }
    }
}

impl std::fmt::Display for ImportSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Selector for discovering workloads
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct ImportSelector {
    /// Filter by name pattern (glob/regex)
    pub name_pattern: Option<String>,
    /// Filter by status (running, stopped, deployed, etc.)
    pub status_filter: Option<Vec<String>>,
    /// Filter by labels/tags
    pub label_filter: Option<HashMap<String, String>>,
    /// Filter by workload type (container, function, static-site, etc.)
    pub workload_type_filter: Option<Vec<String>>,
    /// Limit number of results
    pub limit: Option<usize>,
}

/// Execution context for import operations
#[derive(Debug, Clone)]
pub struct ImportContext {
    /// Session ID for tracking
    pub session_id: String,
    /// User ID performing the import
    pub user_id: i32,
    /// Dry run mode (don't create resources)
    pub dry_run: bool,
    /// Project name for the import
    pub project_name: String,
    /// Preset to use for the project
    pub preset: String,
    /// Directory path
    pub directory: String,
    /// Main branch name
    pub main_branch: String,
    /// Git provider connection ID (required when importing with a repository)
    pub git_provider_connection_id: Option<i32>,
    /// Repository owner (required when importing with a repository)
    pub repo_owner: Option<String>,
    /// Repository name (required when importing with a repository)
    pub repo_name: Option<String>,
    /// Additional context data
    pub metadata: HashMap<String, String>,
}

/// Outcome of an import execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportOutcome {
    /// Session ID
    pub session_id: String,
    /// Whether execution succeeded
    pub success: bool,
    /// Created project ID (if any)
    pub project_id: Option<i32>,
    /// Created environment ID (if any)
    pub environment_id: Option<i32>,
    /// Created deployment ID (if any)
    pub deployment_id: Option<i32>,
    /// Warnings encountered during execution
    pub warnings: Vec<String>,
    /// Errors encountered (if failed)
    pub errors: Vec<String>,
    /// Resources created (for rollback)
    pub created_resources: Vec<CreatedResource>,
    /// Execution duration (seconds)
    pub duration_seconds: f64,
}

/// Resource created during import (for rollback)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatedResource {
    /// Resource type (project, environment, deployment, etc.)
    pub resource_type: String,
    /// Resource ID
    pub resource_id: i32,
    /// Resource name
    pub resource_name: String,
}

/// Service provider trait for importers to access Temps services
///
/// This trait allows importers to access necessary services (database, project service, etc.)
/// without tightly coupling to specific implementations.
#[async_trait]
pub trait ImportServiceProvider: Send + Sync {
    /// Get database connection
    fn db(&self) -> &sea_orm::DatabaseConnection;

    /// Get project service
    fn project_service(&self) -> &dyn std::any::Any;

    /// Get deployment service
    fn deployment_service(&self) -> &dyn std::any::Any;

    /// Get git provider manager
    fn git_provider_manager(&self) -> &dyn std::any::Any;
}

/// Workload importer trait
///
/// All importer implementations (Docker, Coolify, Vercel, etc.) must implement this trait.
/// This trait is generic and works for any workload type: containers, functions, static sites, etc.
#[async_trait]
pub trait WorkloadImporter: Send + Sync {
    /// Source system identifier
    fn source(&self) -> ImportSource;

    /// Human-readable name for this importer
    fn name(&self) -> &str;

    /// Version of this importer
    fn version(&self) -> &str;

    /// Check if the source is accessible and ready
    async fn health_check(&self) -> ImportResult<bool>;

    /// Discover workloads matching the selector
    ///
    /// Returns a list of brief descriptors for discovered workloads.
    async fn discover(&self, selector: ImportSelector) -> ImportResult<Vec<WorkloadDescriptor>>;

    /// Get detailed snapshot of a specific workload
    ///
    /// Returns complete configuration and state information.
    async fn describe(&self, workload_id: &WorkloadId) -> ImportResult<WorkloadSnapshot>;

    /// Generate an import plan from a workload snapshot
    ///
    /// Transforms source-specific configuration into normalized Temps configuration.
    fn generate_plan(&self, snapshot: WorkloadSnapshot) -> ImportResult<ImportPlan>;

    /// Get validation rules for this importer
    ///
    /// Returns source-specific validation rules to check before execution.
    fn validation_rules(&self) -> Vec<Box<dyn ImportValidationRule>>;

    /// Run validations on a plan
    ///
    /// Executes all validation rules and returns a report.
    fn validate(&self, snapshot: &WorkloadSnapshot, plan: &ImportPlan) -> ValidationReport {
        let mut report = ValidationReport::new();

        for rule in self.validation_rules() {
            let result = rule.validate(snapshot, plan);
            report.add_result(result);
        }

        report
    }

    /// Execute the import plan
    ///
    /// Creates projects, environments, and deployments in Temps.
    /// Each importer implementation is responsible for creating the necessary resources.
    async fn execute(
        &self,
        context: ImportContext,
        plan: ImportPlan,
        services: &dyn ImportServiceProvider,
    ) -> ImportResult<ImportOutcome>;

    /// Get capabilities/features supported by this importer
    fn capabilities(&self) -> ImporterCapabilities {
        ImporterCapabilities::default()
    }
}

/// Capabilities of an importer
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImporterCapabilities {
    /// Supports volume import
    pub supports_volumes: bool,
    /// Supports network configuration import
    pub supports_networks: bool,
    /// Supports health check import
    pub supports_health_checks: bool,
    /// Supports resource limits import
    pub supports_resource_limits: bool,
    /// Supports building from source
    pub supports_build: bool,
    /// Supports multi-container stacks
    pub supports_stacks: bool,
}
