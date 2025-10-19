//! Workload snapshot types
//!
//! Represents the current state and configuration of a workload in a source system.
//! A workload can be a container, serverless function, static site, etc.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::ToSchema;

/// Unique identifier for a workload in the source system
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
pub struct WorkloadId(pub String);

impl WorkloadId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WorkloadId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Brief descriptor for discovered workloads (used in listing)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkloadDescriptor {
    /// Unique ID in source system
    pub id: WorkloadId,
    /// Workload name (if any)
    pub name: Option<String>,
    /// Workload type (container, function, static-site, etc.)
    pub workload_type: WorkloadType,
    /// Current status
    pub status: WorkloadStatus,
    /// Image/build reference (for containers)
    pub image: Option<String>,
    /// Creation timestamp
    #[serde(with = "chrono::serde::ts_seconds_option")]
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Labels/tags from source system
    pub labels: HashMap<String, String>,
}

/// Detailed snapshot of a workload's configuration and state
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkloadSnapshot {
    /// Unique ID in source system
    pub id: WorkloadId,
    /// Workload name
    pub name: Option<String>,
    /// Workload type (container, function, static-site, etc.)
    pub workload_type: WorkloadType,
    /// Current status
    pub status: WorkloadStatus,
    /// Image name with tag/digest (for containers)
    pub image: Option<String>,
    /// Command override (if any, for containers)
    pub command: Option<Vec<String>>,
    /// Entrypoint override (if any, for containers)
    pub entrypoint: Option<Vec<String>>,
    /// Working directory
    pub working_dir: Option<String>,
    /// Environment variables
    pub env: HashMap<String, String>,
    /// Port mappings (workload_port -> host_port)
    pub ports: HashMap<u16, Option<u16>>,
    /// Volume mounts (source -> destination)
    pub volumes: Vec<VolumeMount>,
    /// Network configuration
    pub network: NetworkInfo,
    /// Resource limits
    pub resources: ResourceInfo,
    /// Labels/metadata from source
    pub labels: HashMap<String, String>,
    /// Health check configuration (if any) - stored as JSON for flexibility
    pub health_check: Option<serde_json::Value>,
    /// Restart policy (for containers)
    pub restart_policy: Option<RestartPolicy>,
    /// Creation timestamp
    #[serde(with = "chrono::serde::ts_seconds")]
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Source-specific metadata (JSON blob for extensibility)
    #[serde(default)]
    pub source_metadata: serde_json::Value,
}

/// Workload type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum WorkloadType {
    /// Container-based workload (Docker, Podman, etc.)
    Container,
    /// Serverless function (Lambda, Cloud Functions, etc.)
    Function,
    /// Static site (HTML/CSS/JS files)
    StaticSite,
    /// Server-side rendered application
    ServerSideApp,
    /// Background job/worker
    Worker,
    /// Database service
    Database,
    /// Message queue/broker
    MessageQueue,
    /// Cache service
    Cache,
    /// Other/unknown type
    Other,
}

/// Workload status in source system
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum WorkloadStatus {
    /// Active and running
    Running,
    /// Paused/suspended
    Paused,
    /// Stopped but can be started
    Stopped,
    /// Exited (terminated)
    Exited,
    /// Failed/error state
    Failed,
    /// Deployed (for serverless)
    Deployed,
    /// Building
    Building,
    /// Unknown/other status
    Unknown,
}

/// Volume mount information
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VolumeMount {
    /// Source (host path or volume name)
    pub source: String,
    /// Destination (container path)
    pub destination: String,
    /// Read-only flag
    pub read_only: bool,
    /// Volume type (bind, volume, tmpfs)
    #[serde(rename = "type")]
    pub volume_type: VolumeType,
}

/// Volume type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum VolumeType {
    Bind,
    Volume,
    Tmpfs,
}

/// Network configuration
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NetworkInfo {
    /// Network mode (bridge, host, none, custom)
    pub mode: crate::plan::NetworkMode,
    /// Networks the container is connected to
    pub networks: Vec<String>,
    /// Hostname
    pub hostname: Option<String>,
    /// Domain name
    pub domain_name: Option<String>,
}

/// Resource limits and requests
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct ResourceInfo {
    /// CPU limit (number of CPUs, e.g., 1.0 = 1 CPU)
    pub cpu_limit: Option<f64>,
    /// Memory limit (in bytes)
    pub memory_limit: Option<i64>,
    /// Memory reservation (in bytes)
    pub memory_reservation: Option<i64>,
    /// CPU shares (relative weight)
    pub cpu_shares: Option<i64>,
}

/// Health check configuration
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HealthCheckInfo {
    /// Command to run
    pub test: Vec<String>,
    /// Interval between checks (seconds)
    pub interval: u32,
    /// Timeout for each check (seconds)
    pub timeout: u32,
    /// Number of retries before marking unhealthy
    pub retries: u32,
    /// Start period (seconds)
    pub start_period: Option<u32>,
}

/// Restart policy
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    No,
    Always,
    OnFailure,
    UnlessStopped,
}
