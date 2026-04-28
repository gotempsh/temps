use crate::health_monitor::ExternalServiceHealthMonitor;
use crate::{ExternalServiceManager, QueryService};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use utoipa::ToSchema;

use temps_core::AuditLogger;

pub struct AppState {
    pub external_service_manager: Arc<ExternalServiceManager>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub query_service: Arc<QueryService>,
    /// Background health monitor. Present when the server wires it during
    /// startup (single-node control plane). `None` on workers or in tests
    /// where the loop isn't running.
    pub health_monitor: Option<Arc<ExternalServiceHealthMonitor>>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ServiceParameter {
    pub name: String,
    pub required: bool,
    pub encrypted: bool,
    pub description: String,
    pub default_value: Option<String>,
    pub validation_pattern: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choices: Option<Vec<String>>,
}

impl From<crate::externalsvc::ServiceParameter> for ServiceParameter {
    fn from(param: crate::externalsvc::ServiceParameter) -> Self {
        Self {
            name: param.name,
            required: param.required,
            encrypted: param.encrypted,
            description: param.description,
            default_value: param.default_value,
            validation_pattern: param.validation_pattern,
            choices: param.choices,
        }
    }
}

impl From<ServiceParameter> for crate::externalsvc::ServiceParameter {
    fn from(param: ServiceParameter) -> Self {
        Self {
            name: param.name,
            required: param.required,
            encrypted: param.encrypted,
            description: param.description,
            default_value: param.default_value,
            validation_pattern: param.validation_pattern,
            choices: param.choices,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ServiceTypeRoute {
    Mongodb,
    Postgres,
    Redis,
    /// S3-compatible object storage (RustFS-backed by default)
    S3,
    /// Temps KV service (Redis-backed key-value store)
    Kv,
    /// Temps Blob service (RustFS-backed object storage)
    Blob,
    /// RustFS S3-compatible object storage (standalone)
    Rustfs,
    /// MinIO S3-compatible object storage (deprecated, use S3/RustFS instead)
    Minio,
}

impl ServiceTypeRoute {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s.to_lowercase().as_str() {
            "mongodb" => Ok(ServiceTypeRoute::Mongodb),
            "postgres" => Ok(ServiceTypeRoute::Postgres),
            "redis" => Ok(ServiceTypeRoute::Redis),
            "s3" => Ok(ServiceTypeRoute::S3),
            "kv" => Ok(ServiceTypeRoute::Kv),
            "blob" => Ok(ServiceTypeRoute::Blob),
            "rustfs" => Ok(ServiceTypeRoute::Rustfs),
            "minio" => Ok(ServiceTypeRoute::Minio),
            _ => Err(anyhow::anyhow!("Invalid service type: {}", s)),
        }
    }

    /// Returns a Vec containing all available service types
    pub fn get_all() -> Vec<ServiceTypeRoute> {
        vec![
            ServiceTypeRoute::Mongodb,
            ServiceTypeRoute::Postgres,
            ServiceTypeRoute::Redis,
            ServiceTypeRoute::S3,
            ServiceTypeRoute::Kv,
            ServiceTypeRoute::Blob,
            ServiceTypeRoute::Rustfs,
            ServiceTypeRoute::Minio,
        ]
    }

    /// Returns a Vec containing string representations of all available service types
    pub fn get_all_strings() -> Vec<String> {
        Self::get_all()
            .into_iter()
            .map(|st| st.to_string())
            .collect()
    }
}
impl std::fmt::Display for ServiceTypeRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceTypeRoute::Mongodb => write!(f, "mongodb"),
            ServiceTypeRoute::Postgres => write!(f, "postgres"),
            ServiceTypeRoute::Redis => write!(f, "redis"),
            ServiceTypeRoute::S3 => write!(f, "s3"),
            ServiceTypeRoute::Kv => write!(f, "kv"),
            ServiceTypeRoute::Blob => write!(f, "blob"),
            ServiceTypeRoute::Rustfs => write!(f, "rustfs"),
            ServiceTypeRoute::Minio => write!(f, "minio"),
        }
    }
}

impl From<ServiceTypeRoute> for crate::externalsvc::ServiceType {
    #[allow(deprecated)]
    fn from(service_type: ServiceTypeRoute) -> Self {
        match service_type {
            ServiceTypeRoute::Mongodb => crate::externalsvc::ServiceType::Mongodb,
            ServiceTypeRoute::Postgres => crate::externalsvc::ServiceType::Postgres,
            ServiceTypeRoute::Redis => crate::externalsvc::ServiceType::Redis,
            ServiceTypeRoute::S3 => crate::externalsvc::ServiceType::S3,
            ServiceTypeRoute::Kv => crate::externalsvc::ServiceType::Kv,
            ServiceTypeRoute::Blob => crate::externalsvc::ServiceType::Blob,
            ServiceTypeRoute::Rustfs => crate::externalsvc::ServiceType::Rustfs,
            ServiceTypeRoute::Minio => crate::externalsvc::ServiceType::Minio,
        }
    }
}

impl From<crate::externalsvc::ServiceType> for ServiceTypeRoute {
    #[allow(deprecated)]
    fn from(service_type: crate::externalsvc::ServiceType) -> Self {
        match service_type {
            crate::externalsvc::ServiceType::Mongodb => ServiceTypeRoute::Mongodb,
            crate::externalsvc::ServiceType::Postgres => ServiceTypeRoute::Postgres,
            crate::externalsvc::ServiceType::Redis => ServiceTypeRoute::Redis,
            crate::externalsvc::ServiceType::S3 => ServiceTypeRoute::S3,
            crate::externalsvc::ServiceType::Kv => ServiceTypeRoute::Kv,
            crate::externalsvc::ServiceType::Blob => ServiceTypeRoute::Blob,
            crate::externalsvc::ServiceType::Rustfs => ServiceTypeRoute::Rustfs,
            crate::externalsvc::ServiceType::Minio => ServiceTypeRoute::Minio,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ExternalServiceInfo {
    pub id: i32,
    pub name: String,
    pub service_type: ServiceTypeRoute,
    pub version: Option<String>,
    pub status: String,
    pub connection_info: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// Node ID where the service runs. Null means control plane (local).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<i32>,
    /// Service topology: "standalone" (single container) or "cluster" (HA multi-member).
    #[schema(example = "standalone")]
    pub topology: String,
    /// Cluster members (empty for standalone services).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<ServiceMemberInfo>,
    /// Error message from failed initialization.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

/// Public info about a cluster member.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ServiceMemberInfo {
    pub id: i32,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<i32>,
    pub container_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<i32>,
    pub status: String,
    pub ordinal: i32,
    /// Container's IP on the `temps-overlay` multi-host network. Populated
    /// by the lifecycle hook (ADR-011 Phase 3); `None` on single-host
    /// clusters where the overlay isn't attached.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compute_ip: Option<String>,
    /// Last-attempted phase of the async `add_cluster_member` background
    /// task (e.g. `validating`, `provisioning_container`, `done`,
    /// `failed`). `None` for members not created through that flow —
    /// the UI falls back to the `status` column for those.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provisioning_step: Option<String>,
    /// Most recent provisioning failure message, when `status='failed'`.
    /// Set by the background task so the UI can show *why* the new
    /// replica didn't come up.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provisioning_error: Option<String>,
    /// Live FSM state from the pg_auto_failover monitor (`primary`,
    /// `secondary`, `catchingup`, `report_lsn`, …). `None` when the
    /// monitor is unreachable, the service is not a cluster, or the row
    /// is the monitor itself.
    ///
    /// **The UI must render the role badge from this field**, falling
    /// back to `role` only when `live_state` is null. `role` is now
    /// config-only (`monitor` or `replica`); flipping the badge to
    /// "primary" when the monitor elects a new one used to require a
    /// reconciler that lagged ~5s behind real failovers — and during
    /// that window the UI showed two primaries. `live_state` is read
    /// directly from the monitor on every list, so it can never lag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_state: Option<String>,
}

impl From<crate::services::ServiceMemberInfo> for ServiceMemberInfo {
    fn from(m: crate::services::ServiceMemberInfo) -> Self {
        Self {
            id: m.id,
            role: m.role,
            node_id: m.node_id,
            container_name: m.container_name,
            hostname: m.hostname,
            port: m.port,
            status: m.status,
            ordinal: m.ordinal,
            compute_ip: m.compute_ip,
            provisioning_step: m.provisioning_step,
            provisioning_error: m.provisioning_error,
            live_state: m.live_state,
        }
    }
}

/// Request body for adding a single member to a running cluster.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AddClusterMemberRequest {
    /// Member role. Currently only `replica` is accepted at runtime —
    /// monitor is a singleton, primary is elected by pg_auto_failover.
    #[schema(example = "replica")]
    pub role: String,
    /// Target worker node ID. Omit or null to run on the control plane.
    #[serde(default)]
    pub node_id: Option<i32>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct ServiceTypeInfo {
    #[schema(example = "postgres")]
    pub service_type: ServiceTypeRoute,
    #[schema(
        example = "[{\"name\": \"host\", \"required\": true, \"encrypted\": false, \"description\": \"Database host\"}]"
    )]
    pub parameters: Vec<ServiceParameter>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProviderMetadata {
    #[schema(example = "postgres")]
    pub service_type: ServiceTypeRoute,
    #[schema(example = "PostgreSQL")]
    pub display_name: String,
    #[schema(example = "Relational database management system")]
    pub description: String,
    #[schema(example = "https://cdn.simpleicons.org/postgresql")]
    pub icon_url: String,
    #[schema(example = "#336791")]
    pub color: String,
}

impl ProviderMetadata {
    pub fn get_all() -> Vec<Self> {
        vec![
            Self {
                service_type: ServiceTypeRoute::Mongodb,
                display_name: "MongoDB".to_string(),
                description: "NoSQL document database".to_string(),
                icon_url: "/providers/mongodb.svg".to_string(),
                color: "#47A248".to_string(),
            },
            Self {
                service_type: ServiceTypeRoute::Postgres,
                display_name: "PostgreSQL".to_string(),
                description: "Relational database management system".to_string(),
                icon_url: "/providers/postgresql.svg".to_string(),
                color: "#4169E1".to_string(),
            },
            Self {
                service_type: ServiceTypeRoute::Redis,
                display_name: "Redis".to_string(),
                description: "In-memory data structure store".to_string(),
                icon_url: "/providers/redis.svg".to_string(),
                color: "#DC382D".to_string(),
            },
            Self {
                service_type: ServiceTypeRoute::S3,
                display_name: "S3 / RustFS".to_string(),
                description: "S3-compatible object storage (RustFS)".to_string(),
                icon_url: "/providers/s3.svg".to_string(),
                color: "#C72E49".to_string(),
            },
            Self {
                service_type: ServiceTypeRoute::Minio,
                display_name: "MinIO (Deprecated)".to_string(),
                description: "S3-compatible object storage (deprecated, use S3/RustFS instead)"
                    .to_string(),
                icon_url: "/providers/s3.svg".to_string(),
                color: "#C72E49".to_string(),
            },
        ]
    }

    pub fn get_by_type(service_type: &ServiceTypeRoute) -> Option<Self> {
        Self::get_all()
            .into_iter()
            .find(|p| &p.service_type == service_type)
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ExternalServiceDetails {
    pub service: ExternalServiceInfo,
    pub parameter_schema: Option<serde_json::Value>,
    pub current_parameters: Option<HashMap<String, String>>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateExternalServiceRequest {
    pub name: String,
    pub service_type: ServiceTypeRoute,
    pub version: Option<String>,
    pub parameters: HashMap<String, serde_json::Value>,
    /// Target node ID for the service. Omit or null to run on the control plane.
    #[serde(default)]
    pub node_id: Option<i32>,
    /// Service topology: "standalone" (default) or "cluster" (HA multi-member).
    #[serde(default = "default_topology")]
    #[schema(example = "standalone")]
    pub topology: String,
    /// Cluster member specifications. Required when topology is "cluster".
    #[serde(default)]
    pub members: Vec<ClusterMemberRequest>,
}

fn default_topology() -> String {
    "standalone".to_string()
}

/// Request spec for a single cluster member.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ClusterMemberRequest {
    /// Service-type-specific role (e.g., "monitor", "primary", "replica")
    #[schema(example = "primary")]
    pub role: String,
    /// Target worker node ID. Omit or null to run on the control plane.
    #[serde(default)]
    pub node_id: Option<i32>,
}

/// Request body for retrying a failed cluster initialization.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RetryClusterRequest {
    /// Cluster member specifications (same format as create).
    /// If omitted, the original member configuration is reconstructed from
    /// the preserved service_members records.
    #[serde(default)]
    pub members: Vec<ClusterMemberRequest>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UpdateExternalServiceRequest {
    pub parameters: HashMap<String, serde_json::Value>,
    /// Docker image to use for the service (e.g., "gotempsh/postgres-walg:18-bookworm", "timescale/timescaledb-ha:pg18")
    /// When provided, the service will be recreated with the new image while preserving data
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docker_image: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UpgradeExternalServiceRequest {
    /// Docker image to upgrade to (e.g., "gotempsh/postgres-walg:18-bookworm")
    /// This will trigger pg_upgrade for PostgreSQL or equivalent upgrade procedures for other services
    #[schema(example = "gotempsh/postgres-walg:18-bookworm")]
    pub docker_image: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LinkServiceRequest {
    pub project_id: i32,
}

/// Available Docker container that can be imported as a service
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AvailableContainerInfo {
    /// Container ID or name
    #[schema(example = "abc123def456")]
    pub container_id: String,
    /// Container display name
    #[schema(example = "my-postgres")]
    pub container_name: String,
    /// Docker image name (e.g., "gotempsh/postgres-walg:18-bookworm")
    #[schema(example = "gotempsh/postgres-walg:18-bookworm")]
    pub image: String,
    /// Extracted version from image
    #[schema(example = "18")]
    pub version: String,
    /// Service type this container represents
    pub service_type: ServiceTypeRoute,
    /// Whether the container is currently running
    #[schema(example = true)]
    pub is_running: bool,
    /// Exposed ports (e.g., [5432] for PostgreSQL, [6379] for Redis)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub exposed_ports: Vec<u16>,
}

/// Request to import a Docker container as a managed service
#[derive(Debug, Deserialize, ToSchema)]
pub struct ImportExternalServiceRequest {
    /// Name to register the service as in Temps
    #[schema(example = "production-database")]
    pub name: String,
    /// Service type
    pub service_type: ServiceTypeRoute,
    /// Optional version override
    pub version: Option<String>,
    /// Service configuration parameters
    pub parameters: HashMap<String, serde_json::Value>,
    /// Container ID or name to import
    #[schema(example = "abc123def456")]
    pub container_id: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProjectInfo {
    pub id: i32,
    pub slug: String,
    #[schema(example = "2025-10-12T12:15:47.609192Z")]
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProjectServiceInfo {
    pub id: i32,
    pub project: ProjectInfo,
    pub service: ExternalServiceInfo,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ServiceHealthStatusEntryResponse {
    pub service_id: i32,
    /// "operational" | "degraded" | "down". `null` when the service has not
    /// been probed yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = "operational")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_checked_at: Option<String>,
    pub consecutive_failures: i32,
}

impl From<crate::services::ServiceHealthStatusEntry> for ServiceHealthStatusEntryResponse {
    fn from(e: crate::services::ServiceHealthStatusEntry) -> Self {
        Self {
            service_id: e.service_id,
            status: e.status,
            last_checked_at: e.last_checked_at,
            consecutive_failures: e.consecutive_failures,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ServiceHealthStatusBatchResponse {
    pub statuses: Vec<ServiceHealthStatusEntryResponse>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct HealthCheckEntryResponse {
    /// ISO 8601 timestamp of when the probe ran.
    #[schema(example = "2026-04-22T11:30:00Z")]
    pub checked_at: String,
    /// "operational" | "degraded" | "down"
    #[schema(example = "operational")]
    pub status: String,
    /// TCP connect latency in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_time_ms: Option<i32>,
    /// Present only when the probe failed or was degraded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ServiceHealthResponse {
    pub service_id: i32,
    /// Current health. `null` if the service has not been probed yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = "operational")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_checked_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Consecutive failed probes. Alert fires at 3.
    pub consecutive_failures: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_time_ms: Option<i32>,
    /// Uptime percentage over the last 24 hours (0.0 — 100.0).
    /// `null` when there is not enough history.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime_24h_percent: Option<f64>,
    /// Most recent checks, newest-first (capped at `limit`).
    pub recent_checks: Vec<HealthCheckEntryResponse>,
}

impl From<crate::services::ServiceHealthSnapshot> for ServiceHealthResponse {
    fn from(snap: crate::services::ServiceHealthSnapshot) -> Self {
        Self {
            service_id: snap.service_id,
            status: snap.status,
            last_checked_at: snap.last_checked_at,
            last_error: snap.last_error,
            consecutive_failures: snap.consecutive_failures,
            response_time_ms: snap.response_time_ms,
            uptime_24h_percent: snap.uptime_24h_percent,
            recent_checks: snap
                .recent_checks
                .into_iter()
                .map(|e| HealthCheckEntryResponse {
                    checked_at: e.checked_at,
                    status: e.status,
                    response_time_ms: e.response_time_ms,
                    error_message: e.error_message,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EnvironmentVariableInfo {
    pub name: String,
    pub value: String,
    /// Whether this variable contains sensitive data (passwords, keys, tokens)
    #[schema(example = false)]
    pub sensitive: bool,
}

/// One row in the cluster Members table — see `GET /external-services/{id}/cluster-health`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ClusterMemberHealthResponse {
    pub nodename: String,
    pub nodehost: String,
    pub nodeport: i32,
    /// What the node *last told the monitor* it was. Stale during outages.
    pub reported_state: String,
    /// What the monitor *wants* the node to be. Differs from
    /// `reported_state` mid-transition (failover, demotion, etc.).
    pub goal_state: String,
    /// pg_auto_failover liveness signal: `1` healthy, `0` unknown
    /// (no recent report), `-1` unhealthy.
    pub health: i32,
    /// Wall-clock seconds since the node last reported in.
    pub seconds_since_report: i64,
    pub candidate_priority: i32,
    pub replication_quorum: bool,
    /// `sync` / `quorum` / `async` for secondaries; `null` for the primary.
    pub sync_state: Option<String>,
    /// `replay_lag` from `pg_stat_replication`, in milliseconds.
    pub replay_lag_ms: Option<i64>,
}

impl From<crate::services::ClusterMemberHealth> for ClusterMemberHealthResponse {
    fn from(m: crate::services::ClusterMemberHealth) -> Self {
        Self {
            nodename: m.nodename,
            nodehost: m.nodehost,
            nodeport: m.nodeport,
            reported_state: m.reported_state,
            goal_state: m.goal_state,
            health: m.health,
            seconds_since_report: m.seconds_since_report,
            candidate_priority: m.candidate_priority,
            replication_quorum: m.replication_quorum,
            sync_state: m.sync_state,
            replay_lag_ms: m.replay_lag_ms,
        }
    }
}

/// Response body for `GET /external-services/{id}/cluster-health`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ClusterHealthReportResponse {
    /// ISO-8601 wall-clock when the report was generated.
    #[schema(example = "2025-10-12T12:15:47.609192Z")]
    pub checked_at: String,
    /// Round-trip to query the monitor (ms).
    pub monitor_response_ms: i64,
    pub members: Vec<ClusterMemberHealthResponse>,
    /// Set when the monitor itself was unreachable. UI shows a banner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor_error: Option<String>,
}

impl From<crate::services::ClusterHealthReport> for ClusterHealthReportResponse {
    fn from(r: crate::services::ClusterHealthReport) -> Self {
        Self {
            checked_at: r.checked_at.to_rfc3339(),
            monitor_response_ms: r.monitor_response_ms,
            members: r.members.into_iter().map(Into::into).collect(),
            monitor_error: r.monitor_error,
        }
    }
}
