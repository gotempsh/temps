//! Import plan types
//!
//! The import plan is the **contract between the system and the user**. It describes
//! exactly what will happen during migration, in what order, with what risks, and
//! what the user needs to do manually before/after each step.
//!
//! # Design principles
//!
//! 1. **Transparency**: Every action is described in human-readable terms.
//! 2. **Risk visibility**: Every item has a risk level and data-loss implications.
//! 3. **Stepped execution**: Migration runs as ordered steps with checkpoints.
//! 4. **User control**: Every service/domain/env-var can be individually skipped.
//! 5. **Auditability**: The plan is a complete record of what was intended.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::ToSchema;

// ---------------------------------------------------------------------------
// Top-level plan
// ---------------------------------------------------------------------------

/// Complete import plan describing all operations to onboard a workload.
///
/// The plan is generated from a snapshot and presented to the user for review
/// before any resources are created. Users can modify individual items
/// (skip services, change actions) before approving execution.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImportPlan {
    /// Plan version for compatibility tracking
    pub version: String,
    /// Source system this plan was generated from
    pub source: String,
    /// Source workload / project ID in the source system
    pub source_id: String,

    // -- Core configuration (always present) --
    /// Project configuration
    pub project: ProjectConfiguration,
    /// Environment configuration
    pub environment: EnvironmentConfiguration,
    /// Primary deployment configuration
    pub deployment: DeploymentConfiguration,

    // -- Extended configuration (for platform migrations) --
    /// Services to migrate (databases, caches, blob stores)
    ///
    /// Each service has an `action` field the user can change before execution.
    #[serde(default)]
    pub services: Vec<ServicePlan>,
    /// Custom domains to migrate
    #[serde(default)]
    pub domains: Vec<DomainPlan>,
    /// Additional deployments (workers, cron jobs, etc.)
    #[serde(default)]
    pub additional_deployments: Vec<DeploymentConfiguration>,

    // -- Migration execution plan --
    /// Ordered list of migration steps that will be executed.
    ///
    /// This is the human-readable execution plan. Each step describes what
    /// will happen, what risks are involved, and what the user should verify.
    /// Steps are executed in order. If a step fails, execution stops and
    /// already-created resources are reported for manual cleanup.
    #[serde(default)]
    pub steps: Vec<MigrationStep>,

    // -- Plan-level summary --
    /// Human-readable summary of the entire migration
    pub summary: MigrationSummary,
    /// Plan metadata
    pub metadata: PlanMetadata,

    // -- Cluster cost + rightsizing analysis (cluster-level sources only) --
    /// Cost, overprovisioning, and savings analysis. Populated by importers
    /// that can observe the whole source cluster (currently Kubernetes);
    /// `None` for container/platform imports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_analysis: Option<crate::cost::CostAnalysis>,
}

// ---------------------------------------------------------------------------
// Migration steps — the execution contract
// ---------------------------------------------------------------------------

/// A single step in the migration execution plan.
///
/// Steps are presented to the user before execution so they know exactly
/// what will happen. During execution, each step runs in order and reports
/// its outcome before proceeding to the next.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MigrationStep {
    /// Step number (1-based, for display)
    pub order: usize,
    /// Machine-readable step identifier (e.g., "create-project", "create-service-postgres")
    pub id: String,
    /// Human-readable title (e.g., "Create project 'my-app'")
    pub title: String,
    /// Detailed description of what this step does
    pub description: String,
    /// What kind of resource this step creates/modifies
    pub resource_type: StepResourceType,
    /// Risk level for this step
    pub risk: RiskLevel,
    /// Data implications — what could go wrong or what the user needs to know
    #[serde(default)]
    pub data_implications: Vec<DataImplication>,
    /// Things the user should verify BEFORE this step runs
    #[serde(default)]
    pub pre_conditions: Vec<String>,
    /// Things the user should verify AFTER this step completes
    #[serde(default)]
    pub post_conditions: Vec<String>,
    /// Whether this step can be skipped by the user
    pub skippable: bool,
    /// Whether the user has chosen to skip this step (set during review)
    #[serde(default)]
    pub skipped: bool,
    /// Whether this step is reversible (can be cleaned up on failure)
    pub reversible: bool,
    /// Estimated duration hint (e.g., "< 1 second", "10-30 seconds")
    pub estimated_duration: Option<String>,
}

/// What kind of resource a migration step operates on
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum StepResourceType {
    Project,
    Environment,
    Deployment,
    EnvironmentVariable,
    Service,
    Domain,
    GitLink,
    Other,
}

/// Risk level for a migration step
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    /// No risk — purely additive, no existing resources affected
    None,
    /// Low risk — creates new resources, easy to undo
    Low,
    /// Medium risk — modifies configuration, may require manual verification
    Medium,
    /// High risk — involves data, DNS changes, or irreversible operations
    High,
    /// Critical — potential data loss or service disruption
    Critical,
}

/// A specific data implication the user needs to understand
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DataImplication {
    /// Severity of this implication
    pub severity: DataImplicationSeverity,
    /// Human-readable description of what could happen
    pub message: String,
    /// What the user should do about it (if anything)
    pub recommended_action: Option<String>,
}

/// Severity of a data implication
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DataImplicationSeverity {
    /// Just informational — no action needed
    Info,
    /// User should be aware but migration is safe
    Warning,
    /// Data will NOT be migrated — user must act manually
    DataNotMigrated,
    /// Potential data loss if user doesn't take action first
    PotentialDataLoss,
}

// ---------------------------------------------------------------------------
// Migration summary — the TL;DR for the user
// ---------------------------------------------------------------------------

/// Human-readable summary of the entire migration plan
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MigrationSummary {
    /// One-line summary (e.g., "Migrate 'my-app' from Vercel with 1 database, 2 domains")
    pub headline: String,
    /// Overall risk assessment for the migration
    pub overall_risk: RiskLevel,
    /// Resource counts for quick overview
    pub resource_counts: ResourceCounts,
    /// Critical warnings that must be acknowledged before proceeding.
    /// These are the most important things the user needs to know.
    #[serde(default)]
    pub critical_warnings: Vec<String>,
    /// Manual actions the user must perform (before or after migration)
    #[serde(default)]
    pub manual_actions_required: Vec<ManualAction>,
    /// Features from the source platform that cannot be migrated
    #[serde(default)]
    pub unsupported_features: Vec<UnsupportedFeature>,
}

/// Quick count of resources involved in the migration
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct ResourceCounts {
    pub projects: usize,
    pub environments: usize,
    pub deployments: usize,
    pub environment_variables: usize,
    pub services: usize,
    pub domains: usize,
}

/// A manual action the user must perform outside of the automated migration
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ManualAction {
    /// When this action needs to happen
    pub timing: ManualActionTiming,
    /// Human-readable description
    pub description: String,
    /// Why this can't be automated
    pub reason: String,
}

/// When a manual action needs to happen relative to migration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ManualActionTiming {
    /// Must be done before starting the migration
    BeforeMigration,
    /// Must be done after the migration completes
    AfterMigration,
    /// Must be done within a time window after migration (e.g., DNS propagation)
    WithinHours,
}

/// A feature from the source platform that cannot be migrated
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UnsupportedFeature {
    /// Feature name (e.g., "Edge Middleware", "Serverless Functions", "Cron Jobs")
    pub feature: String,
    /// Why it can't be migrated
    pub reason: String,
    /// Suggested alternative in Temps (if any)
    pub alternative: Option<String>,
}

// ---------------------------------------------------------------------------
// Service plan — how to handle each attached service
// ---------------------------------------------------------------------------

/// Plan for migrating a single service (database, cache, etc.)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ServicePlan {
    /// Human-readable service name
    pub name: String,
    /// Service type (maps to temps-providers ServiceType)
    pub service_type: String,
    /// Service version to create (e.g., "16" for Postgres 16)
    pub version: Option<String>,
    /// Parameters for creating the service in Temps
    #[serde(default)]
    pub parameters: HashMap<String, serde_json::Value>,
    /// Environment variable key mappings: source_key -> temps_key
    ///
    /// For example, Vercel's `POSTGRES_URL` might map to Temps' `DATABASE_URL`.
    /// Both keys will be set during migration so the app works with either.
    #[serde(default)]
    pub env_var_mappings: HashMap<String, String>,
    /// What to do with this service
    pub action: ServiceAction,
    /// Human-readable explanation of what this action means
    pub action_description: String,
    /// Data implications specific to this service
    #[serde(default)]
    pub data_implications: Vec<DataImplication>,
}

/// What to do with a service during migration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceAction {
    /// Create a new managed service in Temps (empty — data NOT migrated)
    Create,
    /// Keep using the external connection string as-is (no new service created)
    ///
    /// The existing env vars pointing to the external service will be preserved.
    /// This is the safest option when you have data you don't want to risk.
    LinkExternal,
    /// Don't import this service at all
    Skip,
}

// ---------------------------------------------------------------------------
// Domain plan — how to handle each custom domain
// ---------------------------------------------------------------------------

/// Plan for migrating a single custom domain
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DomainPlan {
    /// Full domain name
    pub domain: String,
    /// Which environment to associate with ("production")
    pub environment: String,
    /// Redirect target (if this is a redirect domain)
    pub redirect_to: Option<String>,
    /// Redirect status code
    pub status_code: Option<i32>,
    /// What to do with this domain
    pub action: DomainAction,
    /// Human-readable explanation
    pub action_description: String,
    /// The temps-side address that replaces this domain when it is skipped.
    ///
    /// Source-generated domains (sslip.io / traefik.me / platform subdomains)
    /// embed the source server's IP and would keep pointing at the old
    /// machine — this tells the user where the app will be reachable on
    /// temps instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement: Option<String>,
}

/// What to do with a domain during migration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DomainAction {
    /// Register the domain in Temps (user must update DNS records manually)
    Import,
    /// Don't import this domain
    Skip,
}

// ---------------------------------------------------------------------------
// Project / Environment / Deployment configuration (existing, preserved)
// ---------------------------------------------------------------------------

/// Project-level configuration
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProjectConfiguration {
    /// Proposed project name
    pub name: String,
    /// Proposed slug (URL-safe identifier)
    pub slug: String,
    /// Project type
    pub project_type: ProjectType,
    /// Whether this is a web application
    pub is_web_app: bool,
}

/// Project type enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ProjectType {
    Static,
    Docker,
    Buildpack,
    /// Git-based deployment (most common for platform migrations)
    Git,
}

/// Environment-level configuration
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EnvironmentConfiguration {
    /// Environment name
    pub name: String,
    /// Proposed subdomain
    pub subdomain: String,
    /// Resource limits for environment
    pub resources: ResourceLimits,
}

/// Deployment-level configuration
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DeploymentConfiguration {
    /// Image to deploy
    pub image: String,
    /// Build configuration (if building from source)
    pub build: Option<BuildConfiguration>,
    /// Deployment strategy
    pub strategy: DeploymentStrategy,
    /// Environment variables
    pub env_vars: Vec<EnvironmentVariable>,
    /// Port mappings
    pub ports: Vec<PortMapping>,
    /// Volume mounts
    pub volumes: Vec<VolumeMount>,
    /// Network configuration
    pub network: NetworkConfiguration,
    /// Resource limits
    pub resources: ResourceLimits,
    /// Command override
    pub command: Option<Vec<String>>,
    /// Entrypoint override
    pub entrypoint: Option<Vec<String>>,
    /// Working directory
    pub working_dir: Option<String>,
    /// Health check configuration
    pub health_check: Option<HealthCheckConfiguration>,
    /// Where the application's source code lives, when the source platform
    /// builds from a git repository. Execution uses this to link the temps
    /// project to the same repository so the real deployment pipeline can
    /// clone and build it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<GitSourcePlan>,
}

/// Git repository the source platform deploys from
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GitSourcePlan {
    /// Repository owner (organization or user)
    pub owner: String,
    /// Repository name
    pub repo: String,
    /// Branch the source platform deploys
    pub branch: String,
    /// Full clone URL, e.g. `https://github.com/owner/repo.git`
    pub clone_url: Option<String>,
    /// True when the repository is public (no credentials on the source
    /// platform) — the project can then build without a git provider
    /// connection.
    pub is_public: bool,
}

/// Build configuration (for building images from source)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BuildConfiguration {
    /// Build context (Dockerfile path or buildpack)
    pub context: String,
    /// Dockerfile path (relative to context)
    pub dockerfile: Option<String>,
    /// Build arguments
    pub args: HashMap<String, String>,
    /// Target stage (for multi-stage builds)
    pub target: Option<String>,
}

/// Deployment strategy
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DeploymentStrategy {
    /// Replace existing deployment
    Replace,
    /// Blue-green deployment
    BlueGreen,
    /// Rolling update
    Rolling,
}

/// Heuristic: does this env var key look like it holds a secret value?
///
/// Importers that read real env values back from a source platform's API
/// (Dokploy, Coolify, CapRover, Portainer) must classify every variable
/// through this before putting it in a plan — an unclassified `is_secret:
/// false` default would carry passwords and API keys in plaintext into
/// `POST /imports/plan` and `GET /imports/{session_id}` responses.
///
/// Includes connection-string-shaped keys (`DATABASE_URL`, `REDIS_URL`,
/// `MONGO_URI`, `SMTP_DSN`, ...) since their *value* commonly embeds a
/// password even though the key itself isn't `PASSWORD`/`SECRET`/`TOKEN`.
/// This over-masks a few harmless keys too (e.g. `PUBLIC_APP_URL`) — an
/// acceptable trade since masking only affects what's echoed back in the
/// plan response, never the real value `execute()` deploys with.
pub fn looks_like_secret_env_key(key: &str) -> bool {
    let upper = key.to_uppercase();
    upper.contains("SECRET")
        || upper.contains("PASSWORD")
        || upper.contains("TOKEN")
        || upper.contains("KEY")
        || upper.contains("API_KEY")
        || upper.contains("_URL")
        || upper.contains("_URI")
        || upper.contains("DSN")
        || upper.contains("CONNECTION_STRING")
        || upper.contains("CONN_STRING")
}

/// Environment variable
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EnvironmentVariable {
    /// Variable name
    pub key: String,
    /// Variable value (may be redacted for secrets)
    pub value: String,
    /// Whether this is a secret (should be encrypted)
    pub is_secret: bool,
    /// Where this env var originates from (for traceability)
    #[serde(default)]
    pub source_description: Option<String>,
}

/// Port mapping
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PortMapping {
    /// Container port
    pub container_port: u16,
    /// Host port (optional - can be assigned dynamically)
    pub host_port: Option<u16>,
    /// Protocol (tcp, udp)
    pub protocol: Protocol,
    /// Whether this is the primary HTTP port
    pub is_primary: bool,
}

/// Network protocol
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
}

/// Volume mount in deployment
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VolumeMount {
    /// Source (volume name or path)
    pub source: String,
    /// Destination path in container
    pub destination: String,
    /// Read-only flag
    pub read_only: bool,
    /// Volume type
    #[serde(rename = "type")]
    pub volume_type: VolumeType,
}

/// Volume type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum VolumeType {
    /// Bind mount from host
    Bind,
    /// Named volume
    Volume,
    /// Temporary filesystem
    Tmpfs,
}

/// Network configuration
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NetworkConfiguration {
    /// Network mode
    pub mode: NetworkMode,
    /// Hostname
    pub hostname: Option<String>,
    /// DNS servers
    pub dns_servers: Vec<String>,
}

/// Network mode
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum NetworkMode {
    Bridge,
    Host,
    None,
    #[serde(untagged)]
    Custom(String),
}

/// Resource limits and requests
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResourceLimits {
    /// CPU limit (millicores)
    pub cpu_limit: Option<i32>,
    /// Memory limit (MB)
    pub memory_limit: Option<i32>,
    /// CPU request (millicores)
    pub cpu_request: Option<i32>,
    /// Memory request (MB)
    pub memory_request: Option<i32>,
}

/// Health check configuration
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HealthCheckConfiguration {
    /// HTTP path to check (if applicable)
    pub http_path: Option<String>,
    /// Port to check
    pub port: u16,
    /// Interval between checks (seconds)
    pub interval: u32,
    /// Timeout for each check (seconds)
    pub timeout: u32,
    /// Number of retries before marking unhealthy
    pub retries: u32,
}

/// Plan metadata
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlanMetadata {
    /// When the plan was generated
    #[serde(with = "chrono::serde::ts_seconds")]
    pub generated_at: chrono::DateTime<chrono::Utc>,
    /// Generator (importer) version
    pub generator_version: String,
    /// Estimated complexity (low, medium, high)
    pub complexity: PlanComplexity,
    /// Warnings detected during planning
    pub warnings: Vec<String>,
}

/// Plan complexity indicator
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum PlanComplexity {
    Low,
    Medium,
    High,
}

// ---------------------------------------------------------------------------
// Backward compatibility: ImportPlan still has source_container_id as alias
// ---------------------------------------------------------------------------

impl ImportPlan {
    /// Backward-compatible accessor for `source_id` (was `source_container_id`)
    pub fn source_container_id(&self) -> &str {
        &self.source_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_secret_env_key_matches_classic_secret_keys() {
        for key in [
            "SECRET",
            "APP_SECRET",
            "PASSWORD",
            "DB_PASSWORD",
            "TOKEN",
            "AUTH_TOKEN",
            "API_KEY",
            "SOME_KEY",
        ] {
            assert!(
                looks_like_secret_env_key(key),
                "{key} should be classified as secret"
            );
        }
    }

    #[test]
    fn looks_like_secret_env_key_matches_connection_string_shaped_keys() {
        // These commonly embed a password in the *value* even though the
        // key itself has no PASSWORD/SECRET/TOKEN substring.
        for key in [
            "DATABASE_URL",
            "REDIS_URL",
            "MONGO_URI",
            "RABBITMQ_URL",
            "AMQP_URL",
            "SMTP_DSN",
            "SENTRY_DSN",
            "CONNECTION_STRING",
            "DB_CONN_STRING",
        ] {
            assert!(
                looks_like_secret_env_key(key),
                "{key} should be classified as secret"
            );
        }
    }

    #[test]
    fn looks_like_secret_env_key_does_not_match_plain_config_keys() {
        for key in ["NODE_ENV", "PORT", "LOG_LEVEL", "FEATURE_CHECKOUT", "PLAN"] {
            assert!(
                !looks_like_secret_env_key(key),
                "{key} should not be classified as secret"
            );
        }
    }
}
