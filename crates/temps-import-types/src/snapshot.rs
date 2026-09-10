// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Workload snapshot types
//!
//! Represents the current state and configuration of a workload in a source system.
//! A workload can be a container, serverless function, static site, etc.
//!
//! # Snapshot hierarchy
//!
//! For platform migrations (Vercel, Railway, Coolify, etc.), the top-level type is
//! [`ProjectSnapshot`] which contains a primary workload, optional additional workloads,
//! attached services, custom domains, and git repository information.
//!
//! For simpler container-level imports (Docker), [`WorkloadSnapshot`] is used directly.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::ToSchema;

// ---------------------------------------------------------------------------
// Workload identification
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Project-level snapshot (for platform migrations)
// ---------------------------------------------------------------------------

/// A full project snapshot from the source platform.
///
/// This is the "what exists today" representation. It captures everything
/// the source platform knows about a project so the plan generator can
/// produce a detailed, reviewable migration plan.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProjectSnapshot {
    /// Source-platform project ID
    pub id: WorkloadId,
    /// Human-readable project name
    pub name: String,
    /// The primary workload (web app, API, etc.)
    pub primary_workload: WorkloadSnapshot,
    /// Additional workloads (workers, cron jobs, etc.)
    #[serde(default)]
    pub additional_workloads: Vec<WorkloadSnapshot>,
    /// Attached services (databases, caches, blob stores)
    #[serde(default)]
    pub services: Vec<ServiceSnapshot>,
    /// Custom domains configured on the source platform
    #[serde(default)]
    pub domains: Vec<DomainSnapshot>,
    /// Git repository information (if linked)
    pub git_info: Option<GitInfo>,
    /// Source platform framework detection (e.g., "nextjs", "remix")
    pub detected_framework: Option<String>,
    /// Platform-specific metadata (raw JSON for debugging / audit trail)
    #[serde(default)]
    pub source_metadata: serde_json::Value,
}

/// Git repository information from the source platform
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GitInfo {
    /// Provider name: "github", "gitlab", "bitbucket"
    pub provider: String,
    /// Repository owner / organization
    pub owner: String,
    /// Repository name
    pub repo: String,
    /// Default / production branch
    pub default_branch: String,
    /// Clone URL (HTTPS)
    pub clone_url: Option<String>,
}

// ---------------------------------------------------------------------------
// Service snapshots (databases, caches, blob stores, etc.)
// ---------------------------------------------------------------------------

/// Snapshot of a managed service attached to a project on the source platform.
///
/// Examples: Vercel Postgres, Railway Postgres, Coolify Redis, etc.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ServiceSnapshot {
    /// Source-platform service ID
    pub id: String,
    /// Human-readable service name
    pub name: String,
    /// Service type
    pub service_type: SnapshotServiceType,
    /// Service version (e.g., "16" for Postgres 16)
    pub version: Option<String>,
    /// Connection string / URL (if accessible)
    ///
    /// This is the *current* connection string on the source platform.
    /// It will NOT work after migration unless the user keeps the external service.
    pub connection_url: Option<String>,
    /// Environment variables this service injects into the project
    pub env_vars: HashMap<String, String>,
    /// Whether the service contains user data that cannot be auto-migrated.
    ///
    /// When `true`, the plan generator MUST produce a data-loss warning.
    pub has_data: bool,
    /// Estimated data size in bytes (if known). Helps users assess migration risk.
    pub data_size_bytes: Option<u64>,
    /// Platform-specific metadata
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// Service type as discovered on the source platform
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SnapshotServiceType {
    Postgres,
    Mysql,
    Redis,
    #[serde(rename = "mongodb")]
    MongoDB,
    /// S3-compatible object storage
    S3,
    /// Key-value store (Vercel KV, Upstash, etc.)
    Kv,
    /// Blob storage (Vercel Blob, etc.)
    Blob,
    /// Other / unknown service type
    Other(String),
}

impl std::fmt::Display for SnapshotServiceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SnapshotServiceType::Postgres => write!(f, "PostgreSQL"),
            SnapshotServiceType::Mysql => write!(f, "MySQL"),
            SnapshotServiceType::Redis => write!(f, "Redis"),
            SnapshotServiceType::MongoDB => write!(f, "MongoDB"),
            SnapshotServiceType::S3 => write!(f, "S3"),
            SnapshotServiceType::Kv => write!(f, "Key-Value Store"),
            SnapshotServiceType::Blob => write!(f, "Blob Storage"),
            SnapshotServiceType::Other(name) => write!(f, "{}", name),
        }
    }
}

// ---------------------------------------------------------------------------
// Domain snapshots
// ---------------------------------------------------------------------------

/// Snapshot of a custom domain configured on the source platform
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DomainSnapshot {
    /// Full domain name (e.g., "www.example.com")
    pub domain: String,
    /// Whether this is the apex domain (e.g., "example.com" vs "www.example.com")
    pub is_apex: bool,
    /// Redirect target (if this domain redirects to another)
    pub redirect_to: Option<String>,
    /// Redirect HTTP status code (301, 302, 307, 308)
    pub redirect_status_code: Option<i32>,
    /// Which environment this domain is associated with ("production", "preview", etc.)
    pub environment: Option<String>,
    /// Whether the domain is verified/active on the source platform
    pub verified: bool,
}

// ---------------------------------------------------------------------------
// Workload-level snapshot (individual container / function / app)
// ---------------------------------------------------------------------------

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
    /// Scheduled job / cron
    CronJob,
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
