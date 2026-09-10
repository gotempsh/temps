// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Workload importer trait
//!
//! Defines the interface that all importer implementations must provide.
//! This is generic for all workload types: containers, serverless functions, static sites, etc.
//!
//! # Credential handling
//!
//! Platform importers (Vercel, Railway, Coolify, etc.) require API credentials
//! to access the source system. The [`ImportCredentials`] type provides a
//! standard way to pass these. Local importers (Docker) can ignore credentials.
//!
//! # Two description modes
//!
//! - [`WorkloadImporter::describe`] — returns a single [`WorkloadSnapshot`] (containers).
//! - [`WorkloadImporter::describe_project`] — returns a full [`ProjectSnapshot`] including
//!   services, domains, git info (platform migrations). Has a default implementation that
//!   wraps `describe()` for backward compatibility.

use crate::{
    error::ImportResult,
    plan::ImportPlan,
    snapshot::{ProjectSnapshot, WorkloadDescriptor, WorkloadId, WorkloadSnapshot},
    validation::{ImportValidationRule, ValidationReport},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::ToSchema;

// ---------------------------------------------------------------------------
// Import source
// ---------------------------------------------------------------------------

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
    /// Kubernetes clusters (Deployments, StatefulSets, CronJobs via kubeconfig)
    Kubernetes,
    /// CapRover platform (apps, one-click databases)
    Caprover,
    /// Portainer (Docker environments, compose stacks, standalone containers)
    Portainer,
    /// Kamal (imported from its config/deploy.yml — there is no control plane)
    Kamal,
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
            ImportSource::Kubernetes => "kubernetes",
            ImportSource::Caprover => "caprover",
            ImportSource::Portainer => "portainer",
            ImportSource::Kamal => "kamal",
            ImportSource::Custom => "custom",
        }
    }

    /// Whether this source requires API credentials (token, base URL, etc.)
    pub fn requires_credentials(&self) -> bool {
        !matches!(self, ImportSource::Docker)
    }
}

impl std::fmt::Display for ImportSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for ImportSource {
    type Err = crate::ImportError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "docker" => Ok(ImportSource::Docker),
            "coolify" => Ok(ImportSource::Coolify),
            "dokploy" => Ok(ImportSource::Dokploy),
            "vercel" => Ok(ImportSource::Vercel),
            "netlify" => Ok(ImportSource::Netlify),
            "railway" => Ok(ImportSource::Railway),
            "render" => Ok(ImportSource::Render),
            "fly" => Ok(ImportSource::Fly),
            "kubernetes" => Ok(ImportSource::Kubernetes),
            "caprover" => Ok(ImportSource::Caprover),
            "portainer" => Ok(ImportSource::Portainer),
            "kamal" => Ok(ImportSource::Kamal),
            "custom" => Ok(ImportSource::Custom),
            _ => Err(crate::ImportError::SourceNotAccessible(format!(
                "Unknown import source: {}",
                s
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

/// Platform-specific credentials for accessing the source system.
///
/// For platforms like Vercel and Railway, this contains the API token.
/// For self-hosted platforms like Coolify and Dokploy, this also contains
/// the `base_url` of the instance.
///
/// Local importers (Docker) can use `ImportCredentials::none()`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct ImportCredentials {
    /// API token / bearer token for the source platform
    pub token: Option<String>,
    /// Team or organization ID (for platforms with team scoping like Vercel)
    pub team_id: Option<String>,
    /// Base URL override (for self-hosted platforms like Coolify, Dokploy)
    ///
    /// Example: `https://coolify.example.com`
    pub base_url: Option<String>,
    /// Additional platform-specific parameters
    #[serde(default)]
    pub extra: HashMap<String, String>,
}

impl ImportCredentials {
    /// Create empty credentials (for local importers like Docker)
    pub fn none() -> Self {
        Self::default()
    }

    /// Create credentials with just a token (for cloud platforms)
    pub fn with_token(token: impl Into<String>) -> Self {
        Self {
            token: Some(token.into()),
            ..Default::default()
        }
    }

    /// Create credentials for self-hosted platforms
    pub fn with_token_and_url(token: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            token: Some(token.into()),
            base_url: Some(base_url.into()),
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Selector
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Execution context
// ---------------------------------------------------------------------------

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
    /// Credentials used for this import (so the importer can make API calls during execution)
    pub credentials: ImportCredentials,
    /// Additional context data
    pub metadata: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// Outcome
// ---------------------------------------------------------------------------

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
    /// Resources created (for rollback / audit)
    pub created_resources: Vec<CreatedResource>,
    /// Per-step results (in execution order)
    pub step_results: Vec<StepResult>,
    /// Execution duration (seconds)
    pub duration_seconds: f64,
}

/// Result of executing a single migration step
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StepResult {
    /// Step ID (matches `MigrationStep.id`)
    pub step_id: String,
    /// Step title (for display)
    pub step_title: String,
    /// Whether this step succeeded
    pub success: bool,
    /// Whether this step was skipped
    pub skipped: bool,
    /// Human-readable message about what happened
    pub message: String,
    /// Resources created by this step
    pub created_resources: Vec<CreatedResource>,
    /// Duration of this step
    pub duration_seconds: f64,
}

/// Resource created during import (for rollback / audit)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreatedResource {
    /// Resource type (project, environment, deployment, service, domain, etc.)
    pub resource_type: String,
    /// Resource ID
    pub resource_id: i32,
    /// Resource name
    pub resource_name: String,
}

// ---------------------------------------------------------------------------
// Service provider trait
// ---------------------------------------------------------------------------

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

    /// Get external service manager (for creating databases, caches, etc.)
    fn external_service_manager(&self) -> Option<&dyn std::any::Any> {
        None
    }

    /// Get custom domain service (for creating domains)
    fn custom_domain_service(&self) -> Option<&dyn std::any::Any> {
        None
    }
}

// ---------------------------------------------------------------------------
// Core importer trait
// ---------------------------------------------------------------------------

/// Workload importer trait
///
/// All importer implementations (Docker, Coolify, Vercel, etc.) must implement this trait.
/// This trait is generic and works for any workload type: containers, functions, static sites, etc.
///
/// # Credential flow
///
/// Platform importers should validate credentials in `validate_credentials()` before
/// any discovery or description calls. The `health_check()` method checks general
/// source availability (e.g., "is Docker daemon running?"), while `validate_credentials()`
/// checks whether the provided API token is valid.
///
/// # Description flow
///
/// There are two description methods:
/// - `describe()` — single workload (containers, simple imports)
/// - `describe_project()` — full project with services, domains, git info (platform migrations)
///
/// `describe_project()` has a default implementation that wraps `describe()` so existing
/// importers don't need to change.
#[async_trait]
pub trait WorkloadImporter: Send + Sync {
    /// Source system identifier
    fn source(&self) -> ImportSource;

    /// Human-readable name for this importer
    fn name(&self) -> &str;

    /// Version of this importer
    fn version(&self) -> &str;

    /// Check if the source is accessible and ready (e.g., Docker daemon is running)
    async fn health_check(&self) -> ImportResult<bool>;

    /// Validate platform credentials before making API calls.
    ///
    /// Returns `Ok(true)` if credentials are valid, `Ok(false)` if invalid,
    /// or `Err` on network/connectivity issues.
    ///
    /// Default implementation returns `Ok(true)` for importers that don't need credentials.
    async fn validate_credentials(
        &self,
        _credentials: &ImportCredentials,
    ) -> ImportResult<CredentialValidation> {
        Ok(CredentialValidation {
            valid: true,
            account_name: None,
            message: None,
        })
    }

    /// Discover workloads matching the selector.
    ///
    /// Returns a list of brief descriptors for discovered workloads.
    /// Platform importers use `credentials` to authenticate API calls.
    async fn discover(
        &self,
        credentials: &ImportCredentials,
        selector: ImportSelector,
    ) -> ImportResult<Vec<WorkloadDescriptor>>;

    /// Get detailed snapshot of a single workload.
    ///
    /// Returns complete configuration and state information for a single
    /// container/function/app. For richer project-level snapshots, use
    /// `describe_project()` instead.
    async fn describe(
        &self,
        credentials: &ImportCredentials,
        workload_id: &WorkloadId,
    ) -> ImportResult<WorkloadSnapshot>;

    /// Get a full project-level snapshot including services, domains, and git info.
    ///
    /// This is the preferred method for platform migrations. The default implementation
    /// wraps `describe()` into a minimal `ProjectSnapshot` for backward compatibility.
    async fn describe_project(
        &self,
        credentials: &ImportCredentials,
        workload_id: &WorkloadId,
    ) -> ImportResult<ProjectSnapshot> {
        let workload = self.describe(credentials, workload_id).await?;
        let name = workload
            .name
            .clone()
            .unwrap_or_else(|| workload_id.as_str().to_string());
        Ok(ProjectSnapshot {
            id: workload_id.clone(),
            name,
            primary_workload: workload,
            additional_workloads: vec![],
            services: vec![],
            domains: vec![],
            git_info: None,
            detected_framework: None,
            source_metadata: serde_json::Value::Null,
        })
    }

    /// Generate an import plan from a project snapshot.
    ///
    /// Transforms source-specific configuration into a normalized Temps import plan
    /// with migration steps, risk assessments, and data implications.
    ///
    /// The plan includes human-readable descriptions of every action so the user
    /// can review and approve before execution.
    fn generate_plan(&self, snapshot: WorkloadSnapshot) -> ImportResult<ImportPlan>;

    /// Generate a full migration plan from a project snapshot.
    ///
    /// This is the preferred method for platform migrations. The default implementation
    /// delegates to `generate_plan()` using only the primary workload.
    fn generate_project_plan(&self, snapshot: ProjectSnapshot) -> ImportResult<ImportPlan> {
        self.generate_plan(snapshot.primary_workload)
    }

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

    /// Execute the import plan.
    ///
    /// Creates projects, environments, and deployments in Temps.
    /// Each importer implementation is responsible for creating the necessary resources.
    ///
    /// Execution follows the `plan.steps` in order. If a step fails, execution stops
    /// and the outcome includes all resources created up to that point.
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

// ---------------------------------------------------------------------------
// Credential validation result
// ---------------------------------------------------------------------------

/// Result of credential validation
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CredentialValidation {
    /// Whether the credentials are valid
    pub valid: bool,
    /// Account or team name (for display, e.g., "my-team" on Vercel)
    pub account_name: Option<String>,
    /// Human-readable message (e.g., "Token has read-only access")
    pub message: Option<String>,
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

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
    /// Supports service migration (databases, caches, etc.)
    pub supports_services: bool,
    /// Supports custom domain migration
    pub supports_domains: bool,
    /// Supports full project-level snapshots (describe_project)
    pub supports_project_snapshot: bool,
    /// Supports cluster cost + overprovisioning analysis (CostAnalysis in plan)
    pub supports_cost_analysis: bool,
}
