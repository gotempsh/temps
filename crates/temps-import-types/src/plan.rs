//! Import plan types
//!
//! Normalized representation of what Temps will do to onboard a workload.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::ToSchema;

/// Complete import plan describing all operations to onboard a workload
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImportPlan {
    /// Plan version for compatibility tracking
    pub version: String,
    /// Source system this plan was generated from
    pub source: String,
    /// Source container ID
    pub source_container_id: String,
    /// Project configuration
    pub project: ProjectConfiguration,
    /// Environment configuration
    pub environment: EnvironmentConfiguration,
    /// Deployment configuration
    pub deployment: DeploymentConfiguration,
    /// Plan metadata
    pub metadata: PlanMetadata,
}

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

/// Environment variable
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EnvironmentVariable {
    /// Variable name
    pub key: String,
    /// Variable value (may be redacted for secrets)
    pub value: String,
    /// Whether this is a secret (should be encrypted)
    pub is_secret: bool,
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
