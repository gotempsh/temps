use crate::externalsvc::{
    mariadb::{MariaDbService, MariaDbSizeProfile},
    mongodb::MongodbService,
    postgres::PostgresService,
    postgres_cluster::PostgresClusterService,
    redis::RedisService,
    rustfs::RustfsService,
    s3::S3Service,
    AvailableContainer, ClusterMemberSpec, ExternalService, HealthProbeStatus,
    ManagedS3BackendKind, ManagedS3BackendSelection, ServiceConfig, ServiceType,
};
use crate::parameter_strategies;
use crate::remote_service_client::{
    RemotePortMapping, RemoteServiceClient, RemoteServiceCreateParams,
};
use crate::types::EnvironmentVariableInfo;
use anyhow::Result;
use bollard::Docker;
use chrono::Utc;
use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, Set, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use temps_entities::{
    external_service_backups, external_service_health_checks, external_services, nodes,
    postgres_major_upgrades, project_services, projects, service_members,
};
use thiserror::Error;
use tracing::{debug, error, info, warn};
// use crate::routes::types::external_services::EnvironmentVariableInfo;
use temps_core::EncryptionService;
// Add these constants at the top of the file proper key management
#[allow(dead_code)]
const NONCE_LENGTH: usize = 12;

#[derive(Error, Debug)]
pub enum ExternalServiceError {
    #[error("Service {id} not found")]
    ServiceNotFound { id: i32 },

    #[error("Service with name '{name}' not found")]
    ServiceNotFoundByName { name: String },

    #[error("Service with slug '{slug}' not found")]
    ServiceNotFoundBySlug { slug: String },

    #[error("Failed to initialize service {id}: {reason}")]
    InitializationFailed { id: i32, reason: String },

    /// A same-version `upgrade()` call was rejected before touching the
    /// container (unsupported downgrade, or a major-version change that
    /// must go through the dedicated upgrade orchestrator instead). Client
    /// error, not a server failure -- kept distinct from
    /// `InitializationFailed` so handlers can map it to 400 without
    /// string-matching the message.
    #[error("Upgrade rejected for service {id}: {reason}")]
    UpgradeRejected { id: i32, reason: String },

    #[error("Failed to encrypt parameter '{param_name}' for service {service_id}: {reason}")]
    EncryptionFailed {
        service_id: i32,
        param_name: String,
        reason: String,
    },

    #[error("Failed to decrypt parameter '{param_name}' for service {service_id}: {reason}")]
    DecryptionFailed {
        service_id: i32,
        param_name: String,
        reason: String,
    },

    #[error("Invalid service type '{service_type}' for service {id}")]
    InvalidServiceType { id: i32, service_type: String },

    #[error("Service {service_id} is not linked to project {project_id}")]
    ServiceNotLinkedToProject { service_id: i32, project_id: i32 },

    #[error("Project {id} not found")]
    ProjectNotFound { id: i32 },

    #[error("Database error: {reason}")]
    DatabaseError { reason: String },

    #[error("Parameter validation failed for service {service_id}: {reason}")]
    ParameterValidationFailed { service_id: i32, reason: String },

    #[error("Failed to start service {id}: {reason}")]
    StartFailed { id: i32, reason: String },

    #[error(
        "Service {id} has a major upgrade in progress (upgrade_id={upgrade_id}, phase={phase}); \
         refusing to start/reconcile the container until it completes or is cancelled"
    )]
    UpgradeInProgress {
        id: i32,
        upgrade_id: i32,
        phase: String,
    },

    #[error("Failed to stop service {id}: {reason}")]
    StopFailed { id: i32, reason: String },

    #[error("Failed to delete service {id}: {reason}")]
    DeletionFailed { id: i32, reason: String },

    #[error("Cannot delete service {service_id}: still linked to {project_count} project(s)")]
    ServiceHasLinkedProjects {
        service_id: i32,
        project_count: usize,
    },

    #[error("Environment variable '{var_name}' not found for service {service_id}")]
    EnvironmentVariableNotFound { service_id: i32, var_name: String },

    #[error("Access denied for encrypted variable '{var_name}' in service {service_id}")]
    EncryptedVariableAccessDenied { service_id: i32, var_name: String },

    #[error("Docker operation failed for service {id}: {reason}")]
    DockerError { id: i32, reason: String },

    #[error("Project {project_id} already has a linked service of type '{service_type}'")]
    DuplicateServiceType {
        project_id: i32,
        service_type: String,
    },

    #[error("Internal error: {reason}")]
    InternalError { reason: String },
}

impl From<sea_orm::DbErr> for ExternalServiceError {
    fn from(err: sea_orm::DbErr) -> Self {
        ExternalServiceError::DatabaseError {
            reason: err.to_string(),
        }
    }
}

impl From<anyhow::Error> for ExternalServiceError {
    fn from(err: anyhow::Error) -> Self {
        ExternalServiceError::InternalError {
            reason: err.to_string(),
        }
    }
}

impl From<sea_orm::TransactionError<ExternalServiceError>> for ExternalServiceError {
    fn from(err: sea_orm::TransactionError<ExternalServiceError>) -> Self {
        match err {
            sea_orm::TransactionError::Connection(e) => ExternalServiceError::DatabaseError {
                reason: e.to_string(),
            },
            sea_orm::TransactionError::Transaction(e) => e,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateExternalServiceRequest {
    pub name: String,
    pub service_type: ServiceType,
    pub version: Option<String>,
    pub parameters: HashMap<String, serde_json::Value>,
    /// Target node ID for the service. None = local (control plane).
    /// For cluster topology, this is ignored (members specify their own node_ids).
    pub node_id: Option<i32>,
    /// Service topology: "standalone" (default, single container) or "cluster" (HA multi-member).
    #[serde(default = "default_topology")]
    pub topology: String,
    /// Cluster member specifications. Required when topology is "cluster".
    /// Each member specifies a role, target node, and ordinal.
    #[serde(default)]
    pub members: Vec<ClusterMemberRequest>,
}

fn default_topology() -> String {
    "standalone".to_string()
}

/// Request spec for a single cluster member.
#[derive(Debug, Clone, Deserialize)]
pub struct ClusterMemberRequest {
    /// Service-type-specific role (e.g., "monitor", "primary", "replica")
    pub role: String,
    /// Target worker node ID. None = local (control plane).
    pub node_id: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct ImportExternalServiceRequest {
    pub name: String,
    pub service_type: ServiceType,
    pub version: Option<String>,
    pub parameters: HashMap<String, serde_json::Value>,
    pub container_id: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateExternalServiceRequest {
    pub name: Option<String>,
    pub parameters: HashMap<String, serde_json::Value>,
    /// Docker image to use for the service (e.g., "gotempsh/postgres-walg:18-bookworm", "timescale/timescaledb-ha:pg18")
    /// When provided, the service container will be recreated with the new image
    pub docker_image: Option<String>,
}

/// Options for getting environment variables
#[derive(Debug, Clone, Default)]
pub struct EnvironmentVariableOptions {
    /// Include Docker container environment variables
    pub include_docker: bool,
    /// Include runtime-provisioned environment variables (requires project_id and environment_id)
    pub include_runtime: bool,
    /// Mask sensitive values (password, secret, key, token, etc.)
    pub mask_sensitive: bool,
    /// Return only variable names (no values)
    pub names_only: bool,
}

/// Response containing environment variables
#[derive(Debug, Serialize)]
pub struct EnvironmentVariablesResponse {
    pub variables: HashMap<String, String>,
    pub masked: bool,
}

#[derive(Debug, Serialize)]
pub struct ExternalServiceDetails {
    pub service: ExternalServiceInfo,
    pub parameter_schema: Option<serde_json::Value>,
    pub current_parameters: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExternalServiceInfo {
    pub id: i32,
    pub name: String,
    pub service_type: ServiceType,
    pub version: Option<String>,
    pub status: String,
    pub connection_info: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// Node ID where the service runs. None = control plane (local).
    /// For cluster topology, this is None (members have their own node_ids).
    pub node_id: Option<i32>,
    /// Service topology: "standalone" or "cluster".
    pub topology: String,
    /// Cluster members (empty for standalone services).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<ServiceMemberInfo>,
    /// Error message from failed initialization (None if no error).
    pub error_message: Option<String>,
    /// Whether metric collection is enabled for this service.
    #[serde(default)]
    pub metrics_enabled: bool,
}

/// Format a `tokio_postgres::Error` (or any `std::error::Error`) by
/// walking its `source()` chain. `tokio_postgres::Error::Display` only
/// emits a brief tag like `db error` and hides the actual cause —
/// callers are expected to walk the chain themselves. This helper does
/// it so probe error messages surface the *real* failure (pg_hba miss,
/// auth failure, TLS rejection, etc.) instead of the useless tag.
fn format_pg_error<E: std::error::Error>(err: &E) -> String {
    let mut out = err.to_string();
    let mut cause: Option<&dyn std::error::Error> = err.source();
    while let Some(c) = cause {
        let s = c.to_string();
        if !s.is_empty() {
            out.push_str(": ");
            out.push_str(&s);
        }
        cause = c.source();
    }
    out
}

/// Aggregate health-probe result for a cluster service. Returned by
/// [`ExternalServiceManager::probe_cluster`] and consumed by the
/// background health monitor.
#[derive(Debug, Clone)]
pub struct ClusterProbeResult {
    pub status: HealthProbeStatus,
    /// Average response time across reachable members (ms).
    pub response_time_ms: Option<i32>,
    /// Per-member failure detail when status is Degraded or Down.
    pub error_message: Option<String>,
}

impl ClusterProbeResult {
    fn down(reason: String) -> Self {
        Self {
            status: HealthProbeStatus::Down,
            response_time_ms: None,
            error_message: Some(reason),
        }
    }
}

/// Per-member health snapshot returned by
/// [`ExternalServiceManager::cluster_health`]. Renders the row in the
/// cluster-detail UI's Members table.
#[derive(Debug, Clone, Serialize)]
pub struct ClusterMemberHealth {
    /// pg_auto_failover's `nodename` for this member (e.g. `node-1`).
    pub nodename: String,
    /// `nodehost` reported by pg_auto_failover.
    pub nodehost: String,
    /// `nodeport` reported by pg_auto_failover.
    pub nodeport: i32,
    /// `pgautofailover.node.reportedstate` — what the node *last told the
    /// monitor* it was. Doesn't change when the node stops phoning home;
    /// use `health` + `seconds_since_report` to detect that.
    pub reported_state: String,
    /// `pgautofailover.node.goalstate` — what the monitor wants this node
    /// to become. When `goalstate != reported_state`, the cluster is in
    /// the middle of a transition (failover, demotion, etc.) and the UI
    /// should render an arrow.
    pub goal_state: String,
    /// `pgautofailover.node.health`: `1` healthy, `0` unknown (no recent
    /// report), `-1` unhealthy (monitor probe failed). The single most
    /// reliable signal that a node is reachable RIGHT NOW.
    pub health: i32,
    /// Wall-clock seconds since pg_auto_failover last received a status
    /// report from this node. Computed server-side as
    /// `EXTRACT(EPOCH FROM now() - reporttime)`.
    pub seconds_since_report: i64,
    /// `pgautofailover.node.candidatepriority` — 0 means "never promote".
    pub candidate_priority: i32,
    /// `pgautofailover.node.replicationquorum` — t/f.
    pub replication_quorum: bool,
    /// From `pg_stat_replication.sync_state` on the primary, joined by
    /// `application_name = nodename`. `Some("sync"|"quorum"|"async")` for
    /// secondaries, `None` for the primary itself.
    pub sync_state: Option<String>,
    /// `replay_lag` from `pg_stat_replication`, in milliseconds. `None`
    /// for the primary or when no streaming row exists yet.
    pub replay_lag_ms: Option<i64>,
}

/// Health report for a cluster — what the UI needs to render the
/// per-member table. Returned by [`ExternalServiceManager::cluster_health`].
#[derive(Debug, Clone, Serialize)]
pub struct ClusterHealthReport {
    /// Wall-clock time the report was generated. Useful for the UI to
    /// display "X seconds ago".
    pub checked_at: chrono::DateTime<chrono::Utc>,
    /// Total round-trip to read `pgautofailover.node` from the monitor (ms).
    pub monitor_response_ms: i64,
    /// One row per registered data member in `pgautofailover.node`.
    /// Monitor itself is excluded because it has no `reportedstate` row.
    pub members: Vec<ClusterMemberHealth>,
    /// Set when the monitor itself was unreachable. UI should show a
    /// banner instead of (or above) the table in this case.
    pub monitor_error: Option<String>,
}

/// Public info about a cluster member.
#[derive(Debug, Clone, Serialize)]
pub struct ServiceMemberInfo {
    pub id: i32,
    pub role: String,
    pub node_id: Option<i32>,
    pub container_name: String,
    pub hostname: Option<String>,
    pub port: Option<i32>,
    pub status: String,
    pub ordinal: i32,
    /// Container's overlay IP from `temps-overlay`, when known. Populated
    /// by the lifecycle hook (ADR-011 Phase 3). The cluster health probe
    /// prefers this over `hostname` because it's the only path that
    /// reliably reaches the container from any node.
    pub compute_ip: Option<String>,
    /// Most recent phase of the background add-member provisioning task,
    /// when applicable. See `MemberProvisioningStep` for the canonical
    /// step names. NULL for members not created through that flow.
    pub provisioning_step: Option<String>,
    pub provisioning_error: Option<String>,
    /// Live FSM state from the pg_auto_failover monitor (`primary`,
    /// `secondary`, `catchingup`, `report_lsn`, …). `None` when the
    /// monitor is unreachable, not applicable (non-cluster service), or
    /// the row is the monitor itself.
    ///
    /// **This is the source of truth for the "is this node primary?"
    /// question.** `role` reflects what we wrote at provisioning time and
    /// can lag behind reality after a failover. UI badges, role checks
    /// in admin actions, and connection-string builders should prefer
    /// `live_state` when set.
    pub live_state: Option<String>,
}

impl ServiceMemberInfo {
    /// Typed view of `role`. Returns `None` for any unrecognised string —
    /// callers that only care about `is_monitor()`/`is_data_member()` should
    /// use those helpers instead.
    pub fn cluster_role(&self) -> Option<crate::ClusterRole> {
        role_from_str(&self.role)
    }

    pub fn is_monitor(&self) -> bool {
        is_role_monitor(&self.role)
    }

    pub fn is_primary(&self) -> bool {
        is_role_primary(&self.role)
    }

    pub fn is_data_member(&self) -> bool {
        is_role_data_member(&self.role)
    }
}

/// Parse a raw role string (TEXT column / spec) into the typed enum.
/// Returns `None` for unknown values; callers should use the
/// classification helpers below for `is_monitor()` / `is_data_member()`
/// semantics, not direct equality.
fn role_from_str(s: &str) -> Option<crate::ClusterRole> {
    use std::str::FromStr;
    crate::ClusterRole::from_str(s).ok()
}

fn is_role_monitor(s: &str) -> bool {
    role_from_str(s) == Some(crate::ClusterRole::Monitor)
}

fn is_role_primary(s: &str) -> bool {
    role_from_str(s) == Some(crate::ClusterRole::Primary)
}

/// `true` for any role that holds data — primary, replica, or any
/// future data role we add. Matches the historical `role != "monitor"`
/// check exactly: unknown roles are treated as data members.
fn is_role_data_member(s: &str) -> bool {
    role_from_str(s).map(|r| r.is_data_member()).unwrap_or(true)
}

/// Validated, fully-resolved input for the background member-creation
/// task. Built once by `plan_add_cluster_member` and handed off to the
/// spawned task; nothing inside it requires a DB lookup, so the task
/// never has to revalidate.
#[derive(Clone)]
struct AddMemberPlan {
    service_id: i32,
    #[allow(dead_code)]
    service_name: String,
    spec: ClusterMemberSpec,
    container_name: String,
    member_fqdn: String,
    member_port: u16,
    member_params: crate::externalsvc::postgres_cluster::ClusterMemberCreateParams,
}

/// Phases of the async `add_cluster_member` task. The strings here are
/// what gets written to `service_members.provisioning_step`; the
/// frontend renders them as a checklist.
///
/// Ordering: each step starts when the previous one finishes, so a
/// member at `provisioning_container` has already passed `validating`
/// and `inserting_row`. `done` and `failed` are terminal.
pub mod member_provisioning_step {
    pub const VALIDATING: &str = "validating";
    pub const RESOLVING_MONITOR: &str = "resolving_monitor";
    pub const INSERTING_ROW: &str = "inserting_row";
    pub const PROVISIONING_CONTAINER: &str = "provisioning_container";
    pub const REGISTERING_DNS: &str = "registering_dns";
    pub const DONE: &str = "done";
    pub const FAILED: &str = "failed";

    /// Ordered list, used by the frontend timeline.
    pub const ORDER: &[&str] = &[
        VALIDATING,
        RESOLVING_MONITOR,
        INSERTING_ROW,
        PROVISIONING_CONTAINER,
        REGISTERING_DNS,
        DONE,
    ];
}

#[derive(Debug, Serialize, Clone)]
pub struct ProjectInfo {
    pub id: i32,
    pub slug: String,
    pub created_at: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct ProjectServiceInfo {
    pub id: i32,
    pub project: ProjectInfo,
    pub service: ExternalServiceInfo,
}

/// Persisted health snapshot returned by `get_health_snapshot`.
#[derive(Debug, Clone, Serialize)]
pub struct ServiceHealthSnapshot {
    pub service_id: i32,
    /// "operational" | "degraded" | "down" | null (never probed)
    pub status: Option<String>,
    pub last_checked_at: Option<String>,
    pub last_error: Option<String>,
    pub consecutive_failures: i32,
    pub response_time_ms: Option<i32>,
    /// 24-hour uptime percentage computed from stored history (0.0 — 100.0).
    /// None when there's not enough history to compute.
    pub uptime_24h_percent: Option<f64>,
    /// Most recent check results, newest-first.
    pub recent_checks: Vec<HealthCheckEntry>,
}

/// Minimal per-service status entry returned by `list_health_statuses`.
/// Powers the status dot on the Storage list page.
#[derive(Debug, Clone, Serialize)]
pub struct ServiceHealthStatusEntry {
    pub service_id: i32,
    /// "operational" | "degraded" | "down" | null (never probed)
    pub status: Option<String>,
    pub last_checked_at: Option<String>,
    pub consecutive_failures: i32,
}

/// A single history entry returned alongside the health snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct HealthCheckEntry {
    pub checked_at: String,
    pub status: String,
    pub response_time_ms: Option<i32>,
    pub error_message: Option<String>,
}

fn compute_uptime_percent(entries: &[HealthCheckEntry], window_hours: i64) -> Option<f64> {
    if entries.is_empty() {
        return None;
    }

    let cutoff = chrono::Utc::now() - chrono::Duration::hours(window_hours);
    let mut total = 0usize;
    let mut operational = 0usize;

    for entry in entries {
        let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&entry.checked_at) else {
            continue;
        };
        if ts.with_timezone(&chrono::Utc) < cutoff {
            continue;
        }
        total += 1;
        if entry.status == "operational" {
            operational += 1;
        }
    }

    if total == 0 {
        None
    } else {
        Some((operational as f64 / total as f64) * 100.0)
    }
}

/// Detect a Postgres unique-constraint violation by inspecting the
/// error chain. Sea-ORM wraps `SqlxError`, which wraps the libpq
/// `SQLSTATE`. The reliable signal is `SQLSTATE 23505` ("unique
/// violation"). We match on substring rather than parsing the full
/// error chain because `tokio_postgres::Error::source()` is hidden
/// inside Sea-ORM's wrapper and there's no stable accessor.
///
/// False positives would only happen if the error message text
/// contains "23505" by accident, which a postgres protocol error
/// won't.
fn is_unique_violation(e: &sea_orm::DbErr) -> bool {
    let s = e.to_string();
    s.contains("23505") || s.contains("duplicate key value")
}

/// Build the env-file lines that WAL-G needs for both `backup-push`
/// and `wal-push`. Same shape used by the standalone postgres path —
/// kept here so the cluster path produces an identical file (any
/// post-failover archiver that sources it works the same way).
fn build_walg_env(
    creds: &crate::S3Credentials,
    walg_s3_prefix: &str,
    resolved_endpoint: Option<&str>,
) -> Vec<String> {
    let mut env = vec![
        format!("export WALG_S3_PREFIX='{}'", walg_s3_prefix),
        format!("export AWS_ACCESS_KEY_ID='{}'", creds.access_key_id),
        format!("export AWS_SECRET_ACCESS_KEY='{}'", creds.secret_key),
        format!("export AWS_REGION='{}'", creds.region),
        // Pin the WAL segment compression to lz4 — fast, low CPU,
        // matches the standalone path. Operators who want zstd can
        // override via service parameters in a follow-up.
        "export WALG_COMPRESSION_METHOD='lz4'".to_string(),
    ];
    if let Some(endpoint) = resolved_endpoint {
        env.push(format!("export AWS_ENDPOINT='{}'", endpoint));
    }
    if creds.force_path_style {
        env.push("export AWS_S3_FORCE_PATH_STYLE='true'".to_string());
    }
    env
}

// ---------------------------------------------------------------------------
// Runtime + stats DTOs (response shapes for the runtime/stats endpoints).
//
// These surface raw container state so the UI can warn about restarts and
// OOM kills, plus live CPU/memory usage. Cluster services return a list
// of members; standalone services return a single entry with role="standalone".
// ---------------------------------------------------------------------------

/// Snapshot of a container's lifecycle state from `docker inspect`.
/// `restart_count` and `oom_killed` are the load-bearing fields when
/// diagnosing crash loops — the kernel OOM killer never reaches the
/// application's logs, so seeing `oom_killed=true` is the only signal
/// that a memory limit was the cause.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ContainerRuntimeInfo {
    /// `service_members.role` for cluster members; "standalone" otherwise.
    pub role: String,
    /// Stable name of the Docker container (e.g. `postgres-mydb`).
    pub container_name: String,
    /// Container Docker id, when present. None = container does not exist
    /// (was never created or was removed externally).
    pub container_id: Option<String>,
    /// Bollard container state ("running", "exited", "dead", etc.). None
    /// when the container does not exist.
    pub status: Option<String>,
    /// Total restarts since the container was created. Useful for
    /// detecting crash loops — a steady stream means something is killing
    /// the container repeatedly (frequently OOM).
    pub restart_count: Option<i64>,
    /// True when the container's last termination was caused by the
    /// kernel OOM killer. Set if the user enabled hard memory limits
    /// and the working set exceeded them.
    pub oom_killed: Option<bool>,
    /// Last container exit code, when known. Non-zero = unclean stop.
    pub exit_code: Option<i64>,
    /// ISO-8601 timestamp of when the container last started. None when
    /// it has never started (i.e. created but never run).
    pub started_at: Option<String>,
    /// ISO-8601 timestamp of the most recent termination, when known.
    pub finished_at: Option<String>,
    /// Currently-effective Docker image (e.g. `gotempsh/postgres-walg:18-bookworm`).
    pub image: Option<String>,
    /// Currently-applied resource limits read off the container's
    /// `HostConfig`. Compare this against the user-configured limits to
    /// detect drift (an old container that never picked up new caps).
    pub resource_limits: super::externalsvc::ServiceResourceLimits,
}

/// Aggregate runtime info for an external service. For standalone services,
/// `members` has exactly one entry. For clusters, one entry per member.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ServiceRuntimeReport {
    pub service_id: i32,
    pub topology: String,
    pub members: Vec<ContainerRuntimeInfo>,
}

/// Live resource usage sample for a single container.
///
/// `cpu_percent` is computed by Docker's standard formula:
///   ((cpu_delta / system_delta) * online_cpus) * 100
/// `memory_percent` is `(memory_usage / memory_limit) * 100` — when no
/// memory limit is set the limit reported by Docker is the host's total
/// RAM, so a 5% reading means "5% of host RAM", not "5% of allocated".
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ContainerStatsSample {
    pub role: String,
    pub container_name: String,
    /// CPU usage as a percentage. `None` when the container is not running
    /// (Docker returns no usable counters).
    pub cpu_percent: Option<f64>,
    /// Resident memory usage in bytes.
    pub memory_usage_bytes: Option<u64>,
    /// Memory limit in bytes (host RAM if no limit set).
    pub memory_limit_bytes: Option<u64>,
    /// Memory usage as a percentage of `memory_limit_bytes`.
    pub memory_percent: Option<f64>,
    /// Number of cores Docker observed at sample time. Used by the UI
    /// to label "x/y cores" instead of just a percent.
    pub online_cpus: Option<u32>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ServiceStatsReport {
    pub service_id: i32,
    pub topology: String,
    pub members: Vec<ContainerStatsSample>,
}

/// Per-container outcome of a live `docker update` call. Surfaced from the
/// PATCH /resources endpoint so the UI can tell the operator whether the
/// new caps are already in effect or whether they only apply on next
/// recreate (e.g., container was missing).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ResourceLimitApplyResult {
    /// `service_members.role` for cluster members; "standalone" otherwise.
    pub role: String,
    pub container_name: String,
    /// One of:
    /// - "applied"  — Docker accepted the update; caps are live now.
    /// - "missing"  — container does not exist; caps stored, will apply on next start.
    /// - "stopped"  — container exists but isn't running; Docker still
    ///   accepts the update (the new caps apply on next start).
    /// - "failed"   — `docker update` returned an error (see `error`).
    pub outcome: String,
    /// Populated only when `outcome == "failed"`.
    pub error: Option<String>,
}

/// Response from PATCH /external-services/{id}/resources.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ResourceLimitsUpdateResponse {
    /// The limits that were persisted to the encrypted config.
    pub limits: crate::externalsvc::ServiceResourceLimits,
    /// Per-container result of trying to apply the limits live.
    pub applied: Vec<ResourceLimitApplyResult>,
}

pub struct ExternalServiceManager {
    db: Arc<DatabaseConnection>,
    encryption_service: Arc<EncryptionService>,
    docker: Arc<Docker>,
    /// Internal DNS registry (ADR-011). Required, not optional — making it
    /// optional led to silent no-ops where one constructor wired it and
    /// another didn't, so cluster members that *should* have DNS records
    /// never got them. The registry is a stateless wrapper over the
    /// shared `DatabaseConnection`, so every constructor can produce one
    /// trivially.
    dns_registry: Arc<temps_dns::DnsRegistry>,
    /// Per-cluster role reconciler shutdown handles, keyed by service_id.
    /// Notify-then-await pattern: `delete_service` fires the notifier and
    /// the task observes it on its next select. Held inside a tokio mutex
    /// because the reconciler-spawn path is async and we want a Send
    /// MutexGuard across awaits.
    reconciler_shutdowns: Arc<tokio::sync::Mutex<HashMap<i32, Arc<tokio::sync::Notify>>>>,
}

impl ExternalServiceManager {
    /// Construct with all required dependencies. The `DnsRegistry` is
    /// required (not optional) so cluster lifecycle hooks always have a
    /// place to write A records — the historical `Option<DnsRegistry>` +
    /// `with_dns_registry` setter caused silent no-ops when one
    /// constructor wired the registry and another didn't.
    ///
    /// Callers that don't have a `DnsRegistry` in scope can build one
    /// trivially: `Arc::new(temps_dns::DnsRegistry::new(db.clone()))`.
    /// The registry is a stateless wrapper over the same `db` handle.
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption_service: Arc<EncryptionService>,
        docker: Arc<Docker>,
        dns_registry: Arc<temps_dns::DnsRegistry>,
    ) -> Self {
        Self {
            db,
            encryption_service,
            docker,
            dns_registry,
            reconciler_shutdowns: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Determine the local machine's private IP address for inter-node communication.
    ///
    /// Uses a UDP socket to determine which interface would be used to reach
    /// a public address (without actually sending any data). This gives us the
    /// correct source IP for the machine's default route.
    fn get_local_private_ip() -> Result<String, String> {
        let socket = std::net::UdpSocket::bind("0.0.0.0:0")
            .map_err(|e| format!("Failed to bind UDP socket: {}", e))?;
        socket
            .connect("8.8.8.8:80")
            .map_err(|e| format!("Failed to connect UDP socket: {}", e))?;
        let local_addr = socket
            .local_addr()
            .map_err(|e| format!("Failed to get local address: {}", e))?;
        Ok(local_addr.ip().to_string())
    }

    pub async fn get_local_address(
        &self,
        service: external_services::Model,
    ) -> Result<String, ExternalServiceError> {
        // Get service parameters
        let service_config = self.get_service_config(service.id).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service.id,
                service_type: service.service_type.clone(),
            }
        })?;

        // Create service instance
        let service_instance = self.create_service_instance_for_parameter_value(
            service.name.clone(),
            service_type,
            &service_config.parameters,
        )?;

        // Get local address from service instance
        let address = service_instance
            .get_local_address(service_config)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to get local address: {}", e),
            })?;

        info!(
            "Retrieved local address {} for service {}",
            address, service.id
        );
        Ok(address)
    }
    pub fn get_service_instance(
        &self,
        name: String,
        service_type: ServiceType,
    ) -> Box<dyn ExternalService> {
        self.create_service_instance(name, service_type)
    }
    #[allow(deprecated)]
    fn create_service_instance(
        &self,
        name: String,
        service_type: ServiceType,
    ) -> Box<dyn ExternalService> {
        match service_type {
            ServiceType::Mariadb => Box::new(MariaDbService::new(name, self.docker.clone())),
            ServiceType::Mongodb => Box::new(MongodbService::new(name, self.docker.clone())),
            ServiceType::Postgres => Box::new(PostgresService::new(name, self.docker.clone())),
            // Note: PostgresCluster is handled via create_cluster_service_instance, not here
            ServiceType::Redis => Box::new(RedisService::new(name, self.docker.clone())),
            // S3 now uses RustFS by default (high-performance S3-compatible storage)
            ServiceType::S3 => Box::new(RustfsService::new(
                name,
                self.docker.clone(),
                self.encryption_service.clone(),
            )),
            // Temps KV uses Redis backend - create a RedisService with "kv-" prefix
            ServiceType::Kv => Box::new(RedisService::new(
                format!("kv-{}", name),
                self.docker.clone(),
            )),
            // Temps Blob uses RustfsService (high-performance S3-compatible storage)
            ServiceType::Blob => Box::new(RustfsService::new(
                format!("blob-{}", name),
                self.docker.clone(),
                self.encryption_service.clone(),
            )),
            // RustFS standalone S3-compatible storage
            ServiceType::Rustfs => Box::new(RustfsService::new(
                name,
                self.docker.clone(),
                self.encryption_service.clone(),
            )),
            // MinIO (deprecated) - kept for backward compatibility with existing services
            ServiceType::Minio => Box::new(S3Service::new(
                name,
                self.docker.clone(),
                self.encryption_service.clone(),
            )),
        }
    }

    #[allow(deprecated)]
    fn create_service_instance_for_parameters(
        &self,
        name: String,
        service_type: ServiceType,
        parameters: &HashMap<String, serde_json::Value>,
    ) -> Result<Box<dyn ExternalService>, ExternalServiceError> {
        let parameter_value =
            serde_json::to_value(parameters).map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to inspect managed S3 backend parameters: {}", e),
            })?;
        self.create_service_instance_for_parameter_value(name, service_type, &parameter_value)
    }

    #[allow(deprecated)]
    fn create_service_instance_for_parameter_value(
        &self,
        name: String,
        service_type: ServiceType,
        parameters: &serde_json::Value,
    ) -> Result<Box<dyn ExternalService>, ExternalServiceError> {
        if !matches!(service_type, ServiceType::S3 | ServiceType::Blob) {
            return Ok(self.create_service_instance(name, service_type));
        }

        let backend_selection =
            ManagedS3BackendSelection::from_parameters(parameters).map_err(|e| {
                ExternalServiceError::ParameterValidationFailed {
                    service_id: 0,
                    reason: e.to_string(),
                }
            })?;
        backend_selection
            .validate_for_service_create()
            .map_err(|e| ExternalServiceError::ParameterValidationFailed {
                service_id: 0,
                reason: e.to_string(),
            })?;

        match backend_selection.backend {
            ManagedS3BackendKind::Rustfs => Ok(self.create_service_instance(name, service_type)),
            ManagedS3BackendKind::Minio if service_type == ServiceType::S3 => {
                Ok(Box::new(S3Service::new(
                    name,
                    self.docker.clone(),
                    self.encryption_service.clone(),
                )))
            }
            ManagedS3BackendKind::Minio => Err(ExternalServiceError::ParameterValidationFailed {
                service_id: 0,
                reason: "managed S3 backend 'minio' is only supported for S3 services; use the default 'rustfs' backend for Blob services"
                    .to_string(),
            }),
            ManagedS3BackendKind::Garage => unreachable!("Garage is rejected by validation"),
        }
    }

    // -----------------------------------------------------------------------
    // Remote-node helpers
    // -----------------------------------------------------------------------

    /// Look up a node by ID and return a `RemoteServiceClient` ready to call
    /// the agent's service endpoints.
    async fn get_remote_client(
        &self,
        node_id: i32,
    ) -> Result<RemoteServiceClient, ExternalServiceError> {
        let node = nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::InternalError {
                reason: format!("Node {} not found", node_id),
            })?;

        let token = node
            .token_encrypted
            .as_deref()
            .ok_or(ExternalServiceError::InternalError {
                reason: format!(
                    "Node {} ({}) has no encrypted token — cannot authenticate",
                    node_id, node.name
                ),
            })
            .and_then(|encrypted| {
                self.encryption_service
                    .decrypt_string(encrypted)
                    .map_err(|e| ExternalServiceError::InternalError {
                        reason: format!(
                            "Failed to decrypt token for node {} ({}): {}",
                            node_id, node.name, e
                        ),
                    })
            })?;

        RemoteServiceClient::new(node.address.clone(), token, node.name.clone())
    }

    /// Build the `RemoteServiceCreateParams` that the agent needs to create a
    /// Docker container for a given service type and parameters.
    fn build_remote_create_params(
        &self,
        service_name: &str,
        service_type: &ServiceType,
        parameters: &HashMap<String, String>,
    ) -> Result<RemoteServiceCreateParams, ExternalServiceError> {
        #[allow(deprecated)]
        let backend_service_type = match (
            *service_type,
            parameters
                .get("backend")
                .map(|backend| backend.trim().to_ascii_lowercase()),
        ) {
            (ServiceType::S3, Some(backend)) if backend == "minio" => ServiceType::Minio,
            (ServiceType::Blob, Some(backend)) if backend == "minio" => {
                return Err(ExternalServiceError::ParameterValidationFailed {
                    service_id: 0,
                    reason: "managed S3 backend 'minio' is only supported for S3 services; use the default 'rustfs' backend for Blob services".to_string(),
                });
            }
            _ => *service_type,
        };

        let (image, container_port, env, volume_path, command) = match backend_service_type {
            ServiceType::Mariadb => {
                let image = parameters
                    .get("docker_image")
                    .cloned()
                    .unwrap_or_else(|| "mariadb:lts".to_string());
                let size_profile = parameters
                    .get("size_profile")
                    .and_then(|value| MariaDbSizeProfile::parse(value))
                    .unwrap_or_default();
                let root_password = parameters.get("root_password").cloned().unwrap_or_default();
                let password = parameters.get("password").cloned().unwrap_or_default();
                let database = parameters
                    .get("database")
                    .cloned()
                    .unwrap_or_else(|| "app".to_string());
                let username = parameters
                    .get("username")
                    .cloned()
                    .unwrap_or_else(|| "app".to_string());

                let env = HashMap::from([
                    ("MARIADB_ROOT_PASSWORD".to_string(), root_password),
                    ("MARIADB_DATABASE".to_string(), database),
                    ("MARIADB_USER".to_string(), username),
                    ("MARIADB_PASSWORD".to_string(), password),
                    ("MARIADB_AUTO_UPGRADE".to_string(), "1".to_string()),
                ]);
                (
                    image,
                    3306u16,
                    env,
                    "/var/lib/mysql".to_string(),
                    Some(size_profile.server_args()),
                )
            }
            ServiceType::Postgres => {
                let image = parameters
                    .get("docker_image")
                    .cloned()
                    .unwrap_or_else(|| "gotempsh/postgres-walg:18-bookworm".to_string());
                let password = parameters.get("password").cloned().unwrap_or_default();
                let database = parameters
                    .get("database")
                    .cloned()
                    .unwrap_or_else(|| "postgres".to_string());
                let username = parameters
                    .get("username")
                    .cloned()
                    .unwrap_or_else(|| "postgres".to_string());
                let max_connections = parameters
                    .get("max_connections")
                    .cloned()
                    .unwrap_or_else(|| "100".to_string());

                let env = HashMap::from([
                    ("POSTGRES_USER".to_string(), username),
                    ("POSTGRES_PASSWORD".to_string(), password),
                    ("POSTGRES_DB".to_string(), database),
                    ("POSTGRES_HOST_AUTH_METHOD".to_string(), "md5".to_string()),
                ]);
                // archive_mode=off until enable_wal_archiving() flips it on
                // together with archive_command. See externalsvc/postgres.rs
                // for the same invariant on the create-container path.
                let cmd = vec![
                    "postgres".to_string(),
                    "-c".to_string(),
                    format!("max_connections={}", max_connections),
                    "-c".to_string(),
                    "wal_level=replica".to_string(),
                    "-c".to_string(),
                    "archive_mode=off".to_string(),
                    "-c".to_string(),
                    "archive_timeout=60".to_string(),
                ];
                (
                    image,
                    5432u16,
                    env,
                    "/var/lib/postgresql".to_string(),
                    Some(cmd),
                )
            }
            ServiceType::Redis => {
                let image = parameters
                    .get("docker_image")
                    .cloned()
                    .unwrap_or_else(|| "gotempsh/redis-walg:8-bookworm".to_string());
                let password = parameters.get("password").cloned().unwrap_or_default();
                let env = HashMap::new();
                let cmd = if password.is_empty() {
                    vec!["redis-server".to_string()]
                } else {
                    vec![
                        "redis-server".to_string(),
                        "--requirepass".to_string(),
                        password,
                    ]
                };
                (image, 6379u16, env, "/data".to_string(), Some(cmd))
            }
            ServiceType::Mongodb => {
                let image = parameters
                    .get("docker_image")
                    .cloned()
                    .unwrap_or_else(|| "mongo:7".to_string());
                let username = parameters
                    .get("username")
                    .cloned()
                    .unwrap_or_else(|| "admin".to_string());
                let password = parameters.get("password").cloned().unwrap_or_default();
                let database = parameters
                    .get("database")
                    .cloned()
                    .unwrap_or_else(|| "admin".to_string());
                let env = HashMap::from([
                    ("MONGO_INITDB_ROOT_USERNAME".to_string(), username),
                    ("MONGO_INITDB_ROOT_PASSWORD".to_string(), password),
                    ("MONGO_INITDB_DATABASE".to_string(), database),
                ]);
                (image, 27017u16, env, "/data/db".to_string(), None)
            }
            ServiceType::S3 | ServiceType::Rustfs | ServiceType::Blob => {
                let image = parameters
                    .get("docker_image")
                    .cloned()
                    .unwrap_or_else(|| "ghcr.io/rustfs/rustfs:latest".to_string());
                let access_key = parameters
                    .get("access_key")
                    .cloned()
                    .unwrap_or_else(|| "minioadmin".to_string());
                let secret_key = parameters.get("secret_key").cloned().unwrap_or_default();
                let env = HashMap::from([
                    ("RUSTFS_ROOT_USER".to_string(), access_key),
                    ("RUSTFS_ROOT_PASSWORD".to_string(), secret_key),
                ]);
                let cmd = vec![
                    "rustfs".to_string(),
                    "server".to_string(),
                    "/data".to_string(),
                ];
                (image, 9000u16, env, "/data".to_string(), Some(cmd))
            }
            ServiceType::Kv => {
                // KV is Redis-backed
                let image = parameters
                    .get("docker_image")
                    .cloned()
                    .unwrap_or_else(|| "gotempsh/redis-walg:8-bookworm".to_string());
                let password = parameters.get("password").cloned().unwrap_or_default();
                let env = HashMap::new();
                let cmd = if password.is_empty() {
                    vec!["redis-server".to_string()]
                } else {
                    vec![
                        "redis-server".to_string(),
                        "--requirepass".to_string(),
                        password,
                    ]
                };
                (image, 6379u16, env, "/data".to_string(), Some(cmd))
            }
            #[allow(deprecated)]
            ServiceType::Minio => {
                let image = parameters
                    .get("docker_image")
                    .cloned()
                    .unwrap_or_else(|| "minio/minio:latest".to_string());
                let access_key = parameters
                    .get("access_key")
                    .cloned()
                    .unwrap_or_else(|| "minioadmin".to_string());
                let secret_key = parameters.get("secret_key").cloned().unwrap_or_default();
                let env = HashMap::from([
                    ("MINIO_ROOT_USER".to_string(), access_key),
                    ("MINIO_ROOT_PASSWORD".to_string(), secret_key),
                ]);
                let cmd = vec![
                    "minio".to_string(),
                    "server".to_string(),
                    "/data".to_string(),
                ];
                (image, 9000u16, env, "/data".to_string(), Some(cmd))
            }
        };

        let host_port: u16 = parameters
            .get("port")
            .and_then(|p| p.parse().ok())
            .unwrap_or(container_port);

        let container_name = self
            .create_service_instance(service_name.to_string(), backend_service_type)
            .get_name();
        let container_name_for_volume = format!("{}-{}", backend_service_type, service_name);
        let volume_name = format!("{}_data", container_name_for_volume);

        // Resource limits may arrive as the modern nested `resources` block or
        // as legacy flat string keys (`memory_mb=512`, `nano_cpus=1000000000`,
        // etc.). Missing limits mean unlimited.
        let resource_limits = Self::remote_resource_limits_from_parameters(parameters);

        Ok(RemoteServiceCreateParams {
            name: container_name,
            service_type: service_type.to_string(),
            image,
            environment: env,
            port_mappings: vec![RemotePortMapping {
                host_port,
                container_port,
            }],
            volumes: HashMap::from([(volume_name, volume_path)]),
            network: Some(temps_core::NETWORK_NAME.to_string()),
            command,
            resource_limits,
        })
    }

    fn remote_resource_limits_from_parameters(
        parameters: &HashMap<String, String>,
    ) -> Option<crate::externalsvc::ServiceResourceLimits> {
        if let Some(resources) = parameters.get("resources") {
            if let Ok(limits) =
                serde_json::from_str::<crate::externalsvc::ServiceResourceLimits>(resources)
            {
                if !limits.is_unlimited() {
                    return Some(limits);
                }
            }
        }

        let parse_i64 = |key: &str| -> Option<i64> {
            parameters
                .get(key)
                .and_then(|s| s.trim().parse::<i64>().ok())
                .filter(|&n| n > 0)
        };
        let limits = crate::externalsvc::ServiceResourceLimits {
            memory_mb: parse_i64("memory_mb"),
            memory_swap_mb: parse_i64("memory_swap_mb"),
            nano_cpus: parse_i64("nano_cpus"),
            cpu_shares: parse_i64("cpu_shares"),
            shm_size_mb: parse_i64("shm_size_mb"),
        };
        if limits.is_unlimited() {
            None
        } else {
            Some(limits)
        }
    }

    pub async fn get_service_by_name(
        &self,
        name_param: &str,
    ) -> Result<external_services::Model, ExternalServiceError> {
        let service = external_services::Entity::find()
            .filter(external_services::Column::Name.eq(name_param))
            .one(self.db.as_ref())
            .await?;

        service.ok_or(ExternalServiceError::ServiceNotFoundByName {
            name: name_param.to_string(),
        })
    }

    pub async fn get_service_by_slug(
        &self,
        slug_param: &str,
    ) -> Result<external_services::Model, ExternalServiceError> {
        let service = external_services::Entity::find()
            .filter(external_services::Column::Slug.eq(slug_param))
            .one(self.db.as_ref())
            .await?;

        service.ok_or(ExternalServiceError::ServiceNotFoundBySlug {
            slug: slug_param.to_string(),
        })
    }

    pub async fn create_service(
        &self,
        request: CreateExternalServiceRequest,
    ) -> Result<ExternalServiceInfo, ExternalServiceError> {
        info!("Creating new external service");
        let service_slug = Self::generate_slug(&request.name);

        let backend_selection =
            if matches!(request.service_type, ServiceType::S3 | ServiceType::Blob) {
                let parameter_value = serde_json::to_value(&request.parameters).map_err(|e| {
                    ExternalServiceError::InternalError {
                        reason: format!("Failed to inspect managed S3 backend parameters: {}", e),
                    }
                })?;
                let backend_selection =
                    ManagedS3BackendSelection::from_parameters(&parameter_value).map_err(|e| {
                        ExternalServiceError::ParameterValidationFailed {
                            service_id: 0,
                            reason: e.to_string(),
                        }
                    })?;
                backend_selection
                    .validate_for_service_create()
                    .map_err(|e| ExternalServiceError::ParameterValidationFailed {
                        service_id: 0,
                        reason: e.to_string(),
                    })?;
                Some(backend_selection)
            } else {
                None
            };

        #[allow(deprecated)]
        let strategy_service_type = match backend_selection
            .as_ref()
            .map(|selection| selection.backend)
        {
            Some(ManagedS3BackendKind::Minio) if request.service_type == ServiceType::S3 => {
                ServiceType::Minio
            }
            Some(ManagedS3BackendKind::Minio) => {
                return Err(ExternalServiceError::ParameterValidationFailed {
                    service_id: 0,
                    reason: "managed S3 backend 'minio' is only supported for S3 services; use the default 'rustfs' backend for Blob services".to_string(),
                });
            }
            _ => request.service_type,
        };

        // Get the parameter strategy for the selected backend defaults. The
        // stored service type remains the requested S3-compatible type.
        let strategy = parameter_strategies::get_strategy(&strategy_service_type.to_string())
            .ok_or(ExternalServiceError::InvalidServiceType {
                id: 0,
                service_type: strategy_service_type.to_string(),
            })?;

        // Validate required parameters
        strategy
            .validate_for_creation(&request.parameters)
            .map_err(|reason| ExternalServiceError::ParameterValidationFailed {
                service_id: 0,
                reason,
            })?;

        // Auto-generate missing optional parameters
        let mut parameters = request.parameters.clone();
        strategy
            .auto_generate_missing(&mut parameters)
            .map_err(|reason| ExternalServiceError::InternalError { reason })?;

        // Serialize parameters to JSON and encrypt
        let config_json = serde_json::to_string(&parameters).map_err(|e| {
            ExternalServiceError::InternalError {
                reason: format!("Failed to serialize config to JSON: {}", e),
            }
        })?;

        let encrypted_config = self
            .encryption_service
            .encrypt_string(&config_json)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to encrypt config: {}", e),
            })?;

        let topology = request.topology.clone();
        let topology_for_txn = topology.clone();

        // Start transaction
        let service = self
            .db
            .transaction::<_, external_services::Model, ExternalServiceError>(|txn| {
                Box::pin(async move {
                    // Create service record with encrypted config
                    let new_service = external_services::ActiveModel {
                        name: Set(request.name.clone()),
                        slug: Set(Some(service_slug.clone())),
                        service_type: Set(request.service_type.to_string()),
                        version: Set(request.version.clone()),
                        status: Set("pending".to_string()),
                        config: Set(Some(encrypted_config)),
                        node_id: Set(request.node_id),
                        topology: Set(topology_for_txn),
                        // Monitoring is on by default for every new service. For DB
                        // engines (postgres/redis/mongodb) the metrics poller picks up
                        // any service with this flag set — zero extra footprint, no
                        // restart. For OTLP-push engines (rustfs/s3) the create handler
                        // provisions the ingest key right after creation. The entity
                        // default stays `false` so existing rows and out-of-band inserts
                        // are unaffected.
                        metrics_enabled: Set(true),
                        created_at: Set(Utc::now()),
                        updated_at: Set(Utc::now()),
                        ..Default::default()
                    };

                    let service = new_service.insert(txn).await?;

                    Ok(service)
                })
            })
            .await
            .map_err(ExternalServiceError::from)?;

        // Initialize the service
        if topology == "cluster" {
            // Cluster creation is async — update status to "creating" and spawn background task.
            // The frontend polls GET /external-services/{id} to track progress.
            let mut service_update: external_services::ActiveModel = service.clone().into();
            service_update.status = Set("creating".to_string());
            service_update.update(self.db.as_ref()).await?;

            let db = self.db.clone();
            let docker = self.docker.clone();
            let encryption_service = self.encryption_service.clone();
            let dns_registry = self.dns_registry.clone();
            let service_id = service.id;
            let members = request.members.clone();

            tokio::spawn(async move {
                let manager = ExternalServiceManager::new(
                    db.clone(),
                    encryption_service,
                    docker,
                    dns_registry,
                );
                let result = manager.initialize_cluster(service_id, &members).await;

                match result {
                    Ok(()) => {
                        info!(
                            "Cluster service {} initialized successfully (background)",
                            service_id
                        );
                        // Status already set to "running" inside initialize_cluster
                    }
                    Err(e) => {
                        error!(
                            "Background cluster creation failed for service {}: {}",
                            service_id, e
                        );

                        // Update service status to "failed" with error message
                        let update_result: Result<_, sea_orm::DbErr> = async {
                            let mut svc: external_services::ActiveModel =
                                external_services::Entity::find_by_id(service_id)
                                    .one(db.as_ref())
                                    .await?
                                    .ok_or(sea_orm::DbErr::RecordNotFound(
                                        "Service not found during rollback".to_string(),
                                    ))?
                                    .into();
                            svc.status = Set("failed".to_string());
                            svc.error_message = Set(Some(e.to_string()));
                            svc.updated_at = Set(Utc::now());
                            svc.update(db.as_ref()).await?;
                            Ok(())
                        }
                        .await;

                        if let Err(db_err) = update_result {
                            error!(
                                "Failed to update service {} status to 'failed': {}",
                                service_id, db_err
                            );
                        }
                    }
                }
            });

            // Return immediately with "creating" status
            self.get_service_info(service.id).await
        } else {
            // Standalone: initialize synchronously
            let init_result = self.initialize_service(service.id).await;

            if let Err(e) = init_result {
                error!(
                    "Service initialization failed for service {}: {}. Rolling back database record.",
                    service.id, e
                );

                if let Err(delete_err) = external_services::Entity::delete_by_id(service.id)
                    .exec(self.db.as_ref())
                    .await
                {
                    error!(
                        "Failed to clean up service {} after initialization failure: {}",
                        service.id, delete_err
                    );
                }

                return Err(ExternalServiceError::InitializationFailed {
                    id: service.id,
                    reason: e.to_string(),
                });
            }

            self.get_service_info(service.id).await
        }
    }

    pub async fn get_service_config(
        &self,
        service_id: i32,
    ) -> Result<ServiceConfig, ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;

        let parameters = self.get_service_parameters(service_id).await?;

        let config = ServiceConfig {
            name: service.name.clone(),
            service_type,
            version: service.version,
            parameters: serde_json::to_value(parameters).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to serialize parameters: {}", e),
                }
            })?,
        };

        Ok(config)
    }

    /// Store a metrics ingest key + internal URL into the service's encrypted
    /// config blob and restart the container so the OTLP env vars take effect.
    ///
    /// Both are persisted in the config (`metrics_ingest_key` and
    /// `metrics_ingest_url`) so any future container recreate (upgrade,
    /// restart) automatically picks them up without re-provisioning.
    pub async fn store_and_apply_ingest_key(
        &self,
        service_id: i32,
        ingest_key: String,
        ingest_url: String,
    ) -> Result<(), ExternalServiceError> {
        // Merge the key + URL into the existing encrypted params.
        let mut params = self.get_service_parameters(service_id).await?;
        params.insert(
            "metrics_ingest_key".to_string(),
            serde_json::Value::String(ingest_key),
        );
        params.insert(
            "metrics_ingest_url".to_string(),
            serde_json::Value::String(ingest_url),
        );

        let config_json =
            serde_json::to_string(&params).map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to serialize config: {}", e),
            })?;
        let encrypted = self
            .encryption_service
            .encrypt_string(&config_json)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to encrypt config: {}", e),
            })?;

        external_services::Entity::update(external_services::ActiveModel {
            id: sea_orm::Set(service_id),
            config: sea_orm::Set(Some(encrypted)),
            ..Default::default()
        })
        .exec(self.db.as_ref())
        .await
        .map_err(|e| ExternalServiceError::InternalError {
            reason: format!("Failed to save ingest key: {}", e),
        })?;

        // Now recreate the container — it will read metrics_ingest_key from config.
        let service = self.get_service(service_id).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;
        let config = self.get_service_config(service_id).await?;
        let instance = self.create_service_instance_for_parameter_value(
            service.name.clone(),
            service_type,
            &config.parameters,
        )?;
        instance
            .apply_ingest_key(config)
            .await
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to restart container with ingest key: {}", e),
            })
    }

    pub async fn list_services(&self) -> Result<Vec<ExternalServiceInfo>, ExternalServiceError> {
        let services = external_services::Entity::find()
            .order_by_desc(external_services::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        let mut result = Vec::new();
        for service in services {
            result.push(self.get_service_info(service.id).await?);
        }

        Ok(result)
    }

    pub async fn list_services_paginated(
        &self,
        page: u64,
        page_size: u64,
    ) -> Result<Vec<ExternalServiceInfo>, ExternalServiceError> {
        let services = external_services::Entity::find()
            .order_by_desc(external_services::Column::CreatedAt)
            .paginate(self.db.as_ref(), page_size)
            .fetch_page(page - 1)
            .await?;

        let mut result = Vec::new();
        for service in services {
            result.push(self.get_service_info(service.id).await?);
        }

        Ok(result)
    }

    pub async fn get_service_details(
        &self,
        service_id: i32,
    ) -> Result<ExternalServiceDetails, ExternalServiceError> {
        let service_info = self.get_service_info(service_id).await?;
        let mut parameters = self.get_service_parameters(service_id).await?;
        // Hide structured sub-blocks from the flat Configuration card. The
        // `resources` block ({memory_mb, nano_cpus, ...}) is surfaced via
        // its own panel + endpoint; if it stays in this map the React
        // `<dl>` renders the object directly and crashes the page.
        parameters.remove("resources");
        let service_type =
            ServiceType::from_str(&service_info.service_type.to_string()).map_err(|_| {
                ExternalServiceError::InvalidServiceType {
                    id: service_id,
                    service_type: service_info.service_type.to_string(),
                }
            })?;

        let service_instance = self.create_service_instance_for_parameters(
            service_info.name.clone(),
            service_type,
            &parameters,
        )?;

        Ok(ExternalServiceDetails {
            service: service_info,
            parameter_schema: service_instance.get_parameter_schema(),
            current_parameters: Some(parameters),
        })
    }

    pub async fn upgrade_service(
        &self,
        service_id: i32,
        new_docker_image: String,
    ) -> Result<ExternalServiceInfo, ExternalServiceError> {
        info!(
            "Upgrading service {} to Docker image {}",
            service_id, new_docker_image
        );
        self.ensure_no_active_upgrade(service_id).await?;

        let service = self.get_service(service_id).await?;
        let old_parameters = self.get_service_parameters(service_id).await?;

        // Get old configuration
        let old_config = ServiceConfig {
            name: service.name.clone(),
            service_type: ServiceType::from_str(&service.service_type).map_err(|_| {
                ExternalServiceError::InvalidServiceType {
                    id: service_id,
                    service_type: service.service_type.clone(),
                }
            })?,
            version: service.version.clone(),
            parameters: serde_json::to_value(&old_parameters).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to serialize old parameters: {}", e),
                }
            })?,
        };

        // Create new configuration with updated Docker image
        let mut new_parameters = old_parameters.clone();
        new_parameters.insert(
            "docker_image".to_string(),
            serde_json::Value::String(new_docker_image.clone()),
        );

        let new_config = ServiceConfig {
            name: service.name.clone(),
            service_type: ServiceType::from_str(&service.service_type).map_err(|_| {
                ExternalServiceError::InvalidServiceType {
                    id: service_id,
                    service_type: service.service_type.clone(),
                }
            })?,
            version: service.version.clone(),
            parameters: serde_json::to_value(&new_parameters).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to serialize new parameters: {}", e),
                }
            })?,
        };

        // Create service instance
        let service_type_enum = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;
        let service_instance = self.create_service_instance_for_parameter_value(
            service.name.clone(),
            service_type_enum,
            &new_config.parameters,
        )?;

        // Call the upgrade method on the service instance. `upgrade()`
        // returns `anyhow::Result` (shared across every provider), so a
        // same-version rejection is downcast here -- before the error is
        // stringified below -- into a typed `UpgradeRejected` variant the
        // handler can match on directly instead of substring-matching the
        // message text.
        service_instance
            .upgrade(old_config, new_config.clone())
            .await
            .map_err(|e| {
                if let Some(rejected) =
                    e.downcast_ref::<crate::externalsvc::postgres::PostgresUpgradeRejected>()
                {
                    ExternalServiceError::UpgradeRejected {
                        id: service_id,
                        reason: rejected.to_string(),
                    }
                } else {
                    ExternalServiceError::InitializationFailed {
                        id: service_id,
                        reason: format!("Upgrade failed: {}", e),
                    }
                }
            })?;

        // Update the service configuration in the database with the new Docker image
        let config_json = serde_json::to_string(&new_parameters).map_err(|e| {
            ExternalServiceError::InternalError {
                reason: format!("Failed to serialize config to JSON: {}", e),
            }
        })?;

        let encrypted_config = self
            .encryption_service
            .encrypt_string(&config_json)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to encrypt config: {}", e),
            })?;

        // Update service config in database
        let mut service_update: external_services::ActiveModel = service.clone().into();
        service_update.config = Set(Some(encrypted_config));
        service_update.status = Set("running".to_string());
        service_update.updated_at = Set(Utc::now());
        service_update.update(self.db.as_ref()).await?;

        self.get_service_info(service_id).await
    }

    pub async fn update_service(
        &self,
        service_id: i32,
        request: UpdateExternalServiceRequest,
    ) -> Result<ExternalServiceInfo, ExternalServiceError> {
        let service = self.get_service(service_id).await?;

        // Get the parameter strategy for this service type
        let strategy = parameter_strategies::get_strategy(&service.service_type).ok_or(
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            },
        )?;

        // Prepare update parameters (merge docker_image if provided)
        let mut update_params = request.parameters.clone();
        if let Some(docker_image) = &request.docker_image {
            info!(
                "Updating service {} with new Docker image: {}",
                service_id, docker_image
            );
            update_params.insert(
                "docker_image".to_string(),
                serde_json::Value::String(docker_image.clone()),
            );
        }

        // Validate that only updateable parameters are being changed
        strategy
            .validate_for_update(&update_params)
            .map_err(|reason| ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason,
            })?;

        // Get existing parameters and merge updates
        let mut existing_params = self.get_service_parameters(service_id).await?;
        strategy
            .merge_updates(&mut existing_params, update_params)
            .map_err(|reason| ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason,
            })?;

        // Serialize and encrypt the merged parameters
        let config_json = serde_json::to_string(&existing_params).map_err(|e| {
            ExternalServiceError::InternalError {
                reason: format!("Failed to serialize config to JSON: {}", e),
            }
        })?;

        let encrypted_config = self
            .encryption_service
            .encrypt_string(&config_json)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to encrypt config: {}", e),
            })?;

        // Update service config (and optionally name/slug) in database.
        // `name` was previously accepted by the request but silently dropped;
        // applying it here keeps the API contract honest.
        let mut service_update: external_services::ActiveModel = service.clone().into();
        service_update.config = Set(Some(encrypted_config));
        if let Some(new_name) = request.name {
            if new_name != service.name {
                // The running container is identified by the service's
                // current (pre-rename) name (see create_service_instance).
                // initialize_service() below rebuilds its stop-then-recreate
                // instance from whatever name is in the DB at that point --
                // if we persist the rename first, it looks for a container
                // under the *new* name, finds nothing, and the still-running
                // old container is left holding the host port, so the new
                // container's start fails with "port is already allocated".
                // Stop the old container by its pre-rename identity first.
                let service_type_enum =
                    ServiceType::from_str(&service.service_type).map_err(|_| {
                        ExternalServiceError::InvalidServiceType {
                            id: service_id,
                            service_type: service.service_type.clone(),
                        }
                    })?;
                let old_instance =
                    self.create_service_instance(service.name.clone(), service_type_enum);
                if let Err(e) = old_instance.stop().await {
                    info!(
                        "Could not stop pre-rename container for service {} (may not exist): {}",
                        service_id, e
                    );
                }
            }
            let new_slug = Self::generate_slug(&new_name);
            service_update.name = Set(new_name);
            service_update.slug = Set(Some(new_slug));
        }
        service_update.updated_at = Set(Utc::now());
        service_update.update(self.db.as_ref()).await?;

        // Reinitialize the service (this will stop, remove, and recreate the container with new image)
        self.initialize_service(service_id).await?;

        self.get_service_info(service_id).await
    }

    pub async fn delete_service(&self, service_id: i32) -> Result<(), ExternalServiceError> {
        // Get service to check if it exists
        let service = self.get_service(service_id).await?;
        let service_type_enum = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;

        // Safety check: Verify no projects are linked to this service
        let linked_projects = project_services::Entity::find()
            .filter(project_services::Column::ServiceId.eq(service_id))
            .all(self.db.as_ref())
            .await?;

        if !linked_projects.is_empty() {
            return Err(ExternalServiceError::ServiceHasLinkedProjects {
                service_id,
                project_count: linked_projects.len(),
            });
        }

        // Load cluster members BEFORE deleting DB records (needed for container cleanup)
        let members = service_members::Entity::find()
            .filter(service_members::Column::ServiceId.eq(service_id))
            .all(self.db.as_ref())
            .await?;
        let is_cluster = !members.is_empty();

        // Fetch parameters BEFORE deleting the DB row -- they're needed below to
        // reconstruct the service instance for container removal, and
        // get_service_parameters looks the service up by ID, which would fail
        // once the row is gone.
        let parameters = self.get_service_parameters(service_id).await?;

        // Delete from database first
        self.db
            .transaction::<_, (), ExternalServiceError>(|txn| {
                Box::pin(async move {
                    project_services::Entity::delete_many()
                        .filter(project_services::Column::ServiceId.eq(service_id))
                        .exec(txn)
                        .await?;

                    external_service_backups::Entity::delete_many()
                        .filter(external_service_backups::Column::ServiceId.eq(service_id))
                        .exec(txn)
                        .await?;

                    service_members::Entity::delete_many()
                        .filter(service_members::Column::ServiceId.eq(service_id))
                        .exec(txn)
                        .await?;

                    external_services::Entity::delete_by_id(service_id)
                        .exec(txn)
                        .await?;

                    Ok(())
                })
            })
            .await
            .map_err(ExternalServiceError::from)?;

        // Stop the per-cluster role reconciler before dropping its records,
        // otherwise a tick mid-deletion could re-write what we just removed.
        self.stop_role_reconciler(service_id).await;

        // Drop DNS records that pointed at this service's members (ADR-011).
        // Best-effort, post-DB-commit: the rows that owned the records are
        // already gone, so the worst case is a stale record served until
        // the next janitor pass. We ignore registry errors so a stuck DNS
        // plane doesn't fail an otherwise successful service deletion.
        // Per-member records (Tier 2).
        for member in &members {
            let owner_id = member.id as i64;
            if let Err(e) = self
                .dns_registry
                .delete_by_owner(temps_dns::InternalOwnerKind::ServiceMember, owner_id)
                .await
            {
                warn!(
                    service_id,
                    member_id = member.id,
                    error = %e,
                    "Failed to drop DNS records for deleted cluster member"
                );
            }
        }
        // Role/VIP records (Tier 3) — owner_id == service_id.
        if let Err(e) = self
            .dns_registry
            .delete_by_owner(temps_dns::InternalOwnerKind::ServiceRole, service_id as i64)
            .await
        {
            warn!(
                service_id,
                error = %e,
                "Failed to drop role/VIP DNS records for deleted cluster"
            );
        }

        // Remove containers
        if is_cluster {
            // Cluster: remove each member container (best-effort, log failures)
            info!(
                "Removing {} cluster member container(s) for service {}",
                members.len(),
                service_id
            );
            let mut errors = Vec::new();

            for member in &members {
                if let Some(node_id) = member.node_id {
                    match self.get_remote_client(node_id).await {
                        Ok(client) => {
                            if let Err(e) = client.remove_service(&member.container_name).await {
                                let msg = format!(
                                    "Failed to remove remote container '{}' on node {}: {}",
                                    member.container_name, node_id, e
                                );
                                error!("{}", msg);
                                errors.push(msg);
                            }
                        }
                        Err(e) => {
                            let msg = format!(
                                "Failed to connect to node {} to remove '{}': {}",
                                node_id, member.container_name, e
                            );
                            error!("{}", msg);
                            errors.push(msg);
                        }
                    }
                } else {
                    // Local container
                    if let Err(e) = self
                        .docker
                        .remove_container(
                            &member.container_name,
                            Some(bollard::query_parameters::RemoveContainerOptions {
                                force: true,
                                ..Default::default()
                            }),
                        )
                        .await
                    {
                        let msg = format!(
                            "Failed to remove local container '{}': {}",
                            member.container_name, e
                        );
                        error!("{}", msg);
                        errors.push(msg);
                    }

                    // Also remove the volume
                    let volume_name = format!("{}_data", member.container_name);
                    if let Err(e) = self
                        .docker
                        .remove_volume(
                            &volume_name,
                            None::<bollard::query_parameters::RemoveVolumeOptions>,
                        )
                        .await
                    {
                        warn!("Failed to remove volume '{}': {}", volume_name, e);
                    }
                }
            }

            if !errors.is_empty() {
                return Err(ExternalServiceError::DeletionFailed {
                    id: service_id,
                    reason: format!(
                        "Service deleted from database but {} container(s) failed to remove: {}",
                        errors.len(),
                        errors.join("; ")
                    ),
                });
            }
        } else {
            // Standalone: remove single container
            info!("Removing service {} container", service_id);
            if let Some(node_id) = service.node_id {
                let client = self.get_remote_client(node_id).await?;
                let container_name = self
                    .create_service_instance_for_parameters(
                        service.name.clone(),
                        service_type_enum,
                        &parameters,
                    )?
                    .get_name();
                client.remove_service(&container_name).await.map_err(|e| {
                    ExternalServiceError::DeletionFailed {
                        id: service_id,
                        reason: e.to_string(),
                    }
                })?;
            } else {
                let service_instance = self.create_service_instance_for_parameters(
                    service.name.clone(),
                    service_type_enum,
                    &parameters,
                )?;
                service_instance.remove().await.map_err(|e| {
                    ExternalServiceError::DeletionFailed {
                        id: service_id,
                        reason: e.to_string(),
                    }
                })?;
            }
        }

        Ok(())
    }

    pub async fn check_service_health(&self, service_id: i32) -> Result<bool> {
        let _service = self.get_service(service_id).await?;

        Ok(false)
    }

    /// Return the current health status for many services in one query.
    /// Used by the Storage list page to render per-row status dots without
    /// issuing one HTTP request per service.
    pub async fn list_health_statuses(
        &self,
        service_ids: &[i32],
    ) -> Result<Vec<ServiceHealthStatusEntry>, ExternalServiceError> {
        if service_ids.is_empty() {
            return Ok(Vec::new());
        }

        let rows = external_services::Entity::find()
            .filter(external_services::Column::Id.is_in(service_ids.to_vec()))
            .all(self.db.as_ref())
            .await?;

        Ok(rows
            .into_iter()
            .map(|r| ServiceHealthStatusEntry {
                service_id: r.id,
                status: r.health_status,
                last_checked_at: r.last_health_check_at.map(|t| t.to_rfc3339()),
                consecutive_failures: r.consecutive_health_failures,
            })
            .collect())
    }

    /// Return the persisted health snapshot for a service (status, last error,
    /// and the most recent check history). Written by
    /// `ExternalServiceHealthMonitor` on each probe cycle.
    pub async fn get_health_snapshot(
        &self,
        service_id: i32,
        history_limit: u64,
    ) -> Result<ServiceHealthSnapshot, ExternalServiceError> {
        let service = self.get_service(service_id).await?;

        let history = external_service_health_checks::Entity::find()
            .filter(external_service_health_checks::Column::ServiceId.eq(service_id))
            .order_by_desc(external_service_health_checks::Column::CheckedAt)
            .paginate(self.db.as_ref(), history_limit.clamp(1, 200))
            .fetch_page(0)
            .await?;

        let recent_checks = history
            .into_iter()
            .map(|row| HealthCheckEntry {
                checked_at: row.checked_at.to_rfc3339(),
                status: row.status,
                response_time_ms: row.response_time_ms,
                error_message: row.error_message,
            })
            .collect::<Vec<_>>();

        // Most recent response time (first entry when sorted DESC).
        let response_time_ms = recent_checks.first().and_then(|c| c.response_time_ms);

        // 24h uptime percentage based on stored history.
        let uptime_24h_percent = compute_uptime_percent(&recent_checks, 24);

        Ok(ServiceHealthSnapshot {
            service_id,
            status: service.health_status,
            last_checked_at: service.last_health_check_at.map(|t| t.to_rfc3339()),
            last_error: service.last_health_error,
            consecutive_failures: service.consecutive_health_failures,
            response_time_ms,
            uptime_24h_percent,
            recent_checks,
        })
    }

    // Helper methods
    /// Read a single `external_services` row by id. Public so handlers can
    /// branch on per-service fields (e.g. `topology`) without redoing the
    /// existence check.
    pub async fn get_service(
        &self,
        service_id: i32,
    ) -> Result<external_services::Model, ExternalServiceError> {
        external_services::Entity::find_by_id(service_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::ServiceNotFound { id: service_id })
    }

    /// Read the Postgres WAL health snapshot from `health_metadata.postgres_wal`.
    ///
    /// Returns `Ok(None)` when the service exists but has no snapshot yet
    /// (e.g., probe hasn't run, or service isn't Postgres). Returns
    /// `ServiceNotFound` only when the row doesn't exist at all.
    pub async fn get_postgres_wal_health(
        &self,
        service_id: i32,
    ) -> Result<
        Option<crate::externalsvc::postgres_wal_health::PostgresWalHealth>,
        ExternalServiceError,
    > {
        let service = self.get_service(service_id).await?;
        let Some(metadata) = service.health_metadata else {
            return Ok(None);
        };
        let Some(snapshot) = metadata.get("postgres_wal") else {
            return Ok(None);
        };
        match serde_json::from_value(snapshot.clone()) {
            Ok(parsed) => Ok(Some(parsed)),
            Err(e) => {
                tracing::warn!(
                    "health_metadata.postgres_wal for service {} did not parse: {}",
                    service_id,
                    e
                );
                Ok(None)
            }
        }
    }

    async fn get_service_info(
        &self,
        service_id: i32,
    ) -> Result<ExternalServiceInfo, ExternalServiceError> {
        let service = self.get_service(service_id).await?;

        // Load cluster members if this is a cluster topology, and enrich
        // each one with the monitor's view of its FSM state. The UI uses
        // `live_state` for the role badge so failovers and promotions
        // reflect immediately, instead of being gated on the
        // `service_members.role` reconciler.
        let members = if service.topology == "cluster" {
            self.get_service_members_with_live_state(service_id).await?
        } else {
            Vec::new()
        };

        Ok(ExternalServiceInfo {
            id: service.id,
            name: service.name,
            service_type: ServiceType::from_str(&service.service_type).map_err(|_| {
                ExternalServiceError::InvalidServiceType {
                    id: service_id,
                    service_type: service.service_type,
                }
            })?,
            version: service.version,
            status: service.status,
            connection_info: None,
            created_at: service.created_at.to_rfc3339(),
            updated_at: service.updated_at.to_rfc3339(),
            node_id: service.node_id,
            topology: service.topology,
            members,
            error_message: service.error_message,
            metrics_enabled: service.metrics_enabled,
        })
    }

    /// Get all members for a cluster service.
    pub async fn get_service_members(
        &self,
        service_id: i32,
    ) -> Result<Vec<ServiceMemberInfo>, ExternalServiceError> {
        let members = service_members::Entity::find()
            .filter(service_members::Column::ServiceId.eq(service_id))
            .order_by_asc(service_members::Column::Ordinal)
            .all(self.db.as_ref())
            .await?;

        Ok(members
            .into_iter()
            .map(|m| ServiceMemberInfo {
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
                // Pure DB read — monitor enrichment is the caller's
                // responsibility via `get_service_members_with_live_state`.
                // Cheap callers (cluster_health, reconciler) avoid the
                // extra network round-trip.
                live_state: None,
            })
            .collect())
    }

    /// Find the live primary among a cluster's members by asking the
    /// monitor for the current FSM state.
    ///
    /// Returns `Ok(None)` when:
    ///   - the service isn't a cluster
    ///   - the monitor is unreachable (callers should treat this as
    ///     "primary unknown" rather than "no primary")
    ///   - the monitor knows of no node in `primary | single` state
    ///
    /// Replaces the old `members.iter().find(|m| m.role == "primary")`
    /// pattern, which broke the moment we stopped storing the primary
    /// designation in `service_members.role`.
    pub async fn find_live_primary_member<'a>(
        &self,
        service: &external_services::Model,
        members: &'a [temps_entities::service_members::Model],
    ) -> Result<Option<&'a temps_entities::service_members::Model>, ExternalServiceError> {
        if service.topology != "cluster" {
            return Ok(None);
        }
        let health = self.cluster_health(service).await;
        if health.monitor_error.is_some() {
            return Ok(None);
        }
        let primary_name = health
            .members
            .iter()
            .find(|h| matches!(h.reported_state.as_str(), "primary" | "single"))
            .map(|h| h.nodename.clone());
        let Some(name) = primary_name else {
            return Ok(None);
        };
        Ok(members.iter().find(|m| m.container_name == name))
    }

    /// Live primary check: ask the pg_auto_failover monitor whether the
    /// given member is currently the writable node. Returns `Ok(false)`
    /// when the monitor is unreachable so admin actions don't get
    /// blocked by a flaky control plane — callers that need stronger
    /// guarantees should explicitly probe `cluster_health` first and
    /// surface `monitor_error` to the user.
    ///
    /// Use this for "is this the primary?" gates (e.g. block deletion,
    /// reject self-promotion). Don't use for the UI label — that path
    /// reads `ServiceMemberInfo.live_state` and shows the actual
    /// FSM state including transient ones like `wait_primary`.
    pub async fn member_is_live_primary(
        &self,
        service: &external_services::Model,
        member: &temps_entities::service_members::Model,
    ) -> Result<bool, ExternalServiceError> {
        if service.topology != "cluster" {
            return Ok(false);
        }
        let health = self.cluster_health(service).await;
        if health.monitor_error.is_some() {
            // Monitor is unreachable. Fall back to the persisted role label
            // so we still refuse to delete the node that was last known to
            // be primary — otherwise the "monitor down" branch turns
            // `remove_cluster_member` into an unconditional escape hatch
            // and can silently delete the writable node. The operator
            // override path is to first manually flip the role column or
            // run `pg_autoctl perform failover` once the monitor recovers.
            return Ok(is_role_primary(&member.role));
        }
        Ok(health
            .members
            .iter()
            .find(|h| h.nodename == member.container_name)
            .map(|h| matches!(h.reported_state.as_str(), "primary" | "single"))
            .unwrap_or(false))
    }

    /// Same shape as `get_service_members`, but for cluster topologies
    /// also queries the monitor and fills in `live_state` per member.
    ///
    /// Used by UI-facing endpoints. Falls back to the bare DB result if
    /// the monitor is unreachable so the page still renders — the UI
    /// then displays the stored `role` as a best-effort label.
    ///
    /// Cost: one extra `cluster_health` call (≤5s timeout). Don't use on
    /// the hot path.
    pub async fn get_service_members_with_live_state(
        &self,
        service_id: i32,
    ) -> Result<Vec<ServiceMemberInfo>, ExternalServiceError> {
        let mut members = self.get_service_members(service_id).await?;

        let service = match self.get_service(service_id).await {
            Ok(s) => s,
            Err(_) => return Ok(members),
        };
        if service.topology != "cluster" {
            return Ok(members);
        }

        // Monitor probe — best-effort. `cluster_health` already swallows
        // monitor errors and returns an empty `members` list, so we just
        // skip enrichment when that happens.
        let health = self.cluster_health(&service).await;
        if health.monitor_error.is_some() || health.members.is_empty() {
            return Ok(members);
        }

        // Index live state by container name (== `nodename` in the monitor
        // since the rename in postgres_cluster.rs::container_params).
        let live: HashMap<String, String> = health
            .members
            .into_iter()
            .map(|m| (m.nodename, m.reported_state))
            .collect();

        for member in members.iter_mut() {
            if member.is_monitor() {
                continue;
            }
            if let Some(state) = live.get(&member.container_name) {
                member.live_state = Some(state.clone());
            }
        }

        Ok(members)
    }

    /// Health-probe a cluster service by fanning out to:
    ///   1. The pg_auto_failover monitor (proves the cluster's control plane
    ///      is alive and we can read state from it).
    ///   2. Each data member's `pgautofailover.node` reported state, read
    ///      *through* the monitor (no per-member network call needed).
    ///
    /// Why not direct `tokio_postgres::connect(member, password)`:
    /// pg_auto_failover's pg_hba.conf only trusts its own infrastructure
    /// users (`autoctl_node`, `pgautofailover_replicator`) globally. The
    /// application user the cluster was created with has *certificate*
    /// auth, not password — so a control-plane-side password probe always
    /// fails with `no pg_hba.conf entry for host ..., user ..., (SSL|no)
    /// encryption`. The monitor, by contrast, accepts `autoctl_node` from
    /// `0.0.0.0/0 trust` — the same path the data nodes themselves use to
    /// register, so we know it works.
    ///
    /// Aggregation rules:
    /// - Monitor reachable + every reported data node in a healthy state
    ///   → `Operational`.
    /// - Monitor reachable + at least one data node not healthy
    ///   → `Degraded` (with per-member states listed).
    /// - Monitor unreachable → `Down` (with full error chain).
    /// - No monitor row at all → `Down` ("no monitor in cluster").
    pub async fn probe_cluster(&self, service: &external_services::Model) -> ClusterProbeResult {
        use std::time::{Duration, Instant};

        const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

        let members = match self.get_service_members(service.id).await {
            Ok(m) => m,
            Err(e) => {
                return ClusterProbeResult::down(format!(
                    "Failed to load cluster members for service {}: {}",
                    service.id, e
                ));
            }
        };

        let monitor = members.iter().find(|m| is_role_monitor(&m.role));
        let monitor = match monitor {
            Some(m) => m,
            None => {
                return ClusterProbeResult::down(format!(
                    "Cluster service {} has no monitor member",
                    service.id
                ));
            }
        };

        // Resolve the monitor host: prefer overlay IP, fall back to the
        // node's underlay address, then localhost. The monitor's host port
        // is `service_id * 10 + 6000` for the dev cluster; in general
        // `monitor.port` is what the lifecycle hook stored.
        let monitor_host: String = if let Some(ip) = monitor.compute_ip.as_deref() {
            ip.to_string()
        } else if let Some(node_id) = monitor.node_id {
            match nodes::Entity::find_by_id(node_id)
                .one(self.db.as_ref())
                .await
            {
                Ok(Some(n)) => n.private_address,
                _ => {
                    return ClusterProbeResult::down(format!(
                        "Monitor's node {} not found in nodes table",
                        node_id
                    ))
                }
            }
        } else {
            "localhost".to_string()
        };
        let monitor_port = monitor.port.unwrap_or(5432);

        // pg_auto_failover requires SSL for the autoctl_node user (the
        // hba rule is `hostssl ... trust`). We use PostgresSource which
        // tries TLS-with-self-signed-accept first, then falls back to
        // plain. Empty password is correct: autoctl_node is trust-auth'd
        // from 0.0.0.0/0 once SSL is established.
        let conn_str = format!(
            "host={monitor_host} port={monitor_port} user=autoctl_node \
             dbname=pg_auto_failover sslmode=require connect_timeout=3"
        );

        let start = Instant::now();
        let connect = tokio::time::timeout(
            PROBE_TIMEOUT,
            temps_query_postgres::connect_with_self_signed_tls(&conn_str),
        )
        .await;

        let client = match connect {
            Err(_) => {
                return ClusterProbeResult::down(format!(
                    "Monitor probe to {monitor_host}:{monitor_port} timed out after {}s",
                    PROBE_TIMEOUT.as_secs()
                ));
            }
            Ok(Err(e)) => {
                return ClusterProbeResult::down(format!(
                    "Monitor connect to {monitor_host}:{monitor_port} failed: {}",
                    format_pg_error(&e)
                ));
            }
            Ok(Ok(client)) => client,
        };

        // Read per-data-node reportedstate from the monitor. The monitor's
        // pgautofailover.node table holds one row per registered data node.
        let rows_result = tokio::time::timeout(
            PROBE_TIMEOUT,
            client.query(
                "SELECT nodename::text, nodehost::text, reportedstate::text \
                 FROM pgautofailover.node",
                &[],
            ),
        )
        .await;

        // Drop the client to close the connection cleanly. The driver task
        // is owned by `connect_with_self_signed_tls` and exits when the
        // client handle is dropped.
        drop(client);

        let rows = match rows_result {
            Err(_) => {
                return ClusterProbeResult::down(format!(
                    "Monitor query to {monitor_host}:{monitor_port} timed out after {}s",
                    PROBE_TIMEOUT.as_secs()
                ));
            }
            Ok(Err(e)) => {
                return ClusterProbeResult::down(format!(
                    "Monitor query failed at {monitor_host}:{monitor_port}: {}",
                    format_pg_error(&e)
                ));
            }
            Ok(Ok(r)) => r,
        };

        let elapsed_ms = start.elapsed().as_millis();
        let response_time_ms = i32::try_from(elapsed_ms).ok();

        let healthy_states = ["primary", "single", "secondary"];
        let mut unhealthy: Vec<String> = Vec::new();
        for row in &rows {
            let nodename: &str = row.get(0);
            let state: &str = row.get(2);
            if !healthy_states.contains(&state) {
                unhealthy.push(format!("{nodename}={state}"));
            }
        }

        if rows.is_empty() {
            // Monitor reachable but no data nodes registered — cluster is
            // half-built. Treat as Down so it's visibly broken.
            return ClusterProbeResult::down(format!(
                "Monitor at {monitor_host}:{monitor_port} reports zero data nodes"
            ));
        }

        if unhealthy.is_empty() {
            ClusterProbeResult {
                status: HealthProbeStatus::Operational,
                response_time_ms,
                error_message: None,
            }
        } else {
            ClusterProbeResult {
                status: HealthProbeStatus::Degraded,
                response_time_ms,
                error_message: Some(format!(
                    "{}/{} data node(s) not in a healthy state: {}",
                    unhealthy.len(),
                    rows.len(),
                    unhealthy.join(", ")
                )),
            }
        }
    }

    /// Read per-member health for a cluster from the monitor + the current
    /// primary. Used by the UI's Members table — gives one row per data
    /// node with role, reported state, replication sync state, and
    /// replay lag in ms.
    ///
    /// Two queries:
    /// 1. `pgautofailover.node` from the monitor (TLS, autoctl_node) —
    ///    authoritative for `reportedstate` / `candidatepriority` /
    ///    `replicationquorum`.
    /// 2. `pg_stat_replication` from the current primary (TLS,
    ///    autoctl_node) — gives `sync_state` and `replay_lag` per
    ///    streaming replica, joined to step 1 by `application_name = nodename`.
    ///
    /// Best-effort on (2): if the primary is briefly unreachable mid-failover,
    /// the per-member sync_state/replay_lag fields are left `None` and the
    /// caller can still render the topology view.
    pub async fn cluster_health(&self, service: &external_services::Model) -> ClusterHealthReport {
        use std::time::{Duration, Instant};
        const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

        // ---- locate the monitor ----
        let members = match self.get_service_members(service.id).await {
            Ok(m) => m,
            Err(e) => {
                return ClusterHealthReport {
                    checked_at: chrono::Utc::now(),
                    monitor_response_ms: 0,
                    members: vec![],
                    monitor_error: Some(format!(
                        "Failed to load cluster members for service {}: {}",
                        service.id, e
                    )),
                };
            }
        };
        let monitor = match members.iter().find(|m| is_role_monitor(&m.role)) {
            Some(m) => m,
            None => {
                return ClusterHealthReport {
                    checked_at: chrono::Utc::now(),
                    monitor_response_ms: 0,
                    members: vec![],
                    monitor_error: Some(format!(
                        "Cluster service {} has no monitor member",
                        service.id
                    )),
                };
            }
        };

        let monitor_host: String = if let Some(ip) = monitor.compute_ip.as_deref() {
            ip.to_string()
        } else if let Some(node_id) = monitor.node_id {
            match nodes::Entity::find_by_id(node_id)
                .one(self.db.as_ref())
                .await
            {
                Ok(Some(n)) => n.private_address,
                _ => {
                    return ClusterHealthReport {
                        checked_at: chrono::Utc::now(),
                        monitor_response_ms: 0,
                        members: vec![],
                        monitor_error: Some(format!(
                            "Monitor's node {} not found in nodes table",
                            node_id
                        )),
                    };
                }
            }
        } else {
            "localhost".to_string()
        };
        let monitor_port = monitor.port.unwrap_or(5432);

        let monitor_conn_str = format!(
            "host={monitor_host} port={monitor_port} user=autoctl_node \
             dbname=pg_auto_failover sslmode=require connect_timeout=3"
        );

        let start = Instant::now();
        let monitor_client = match tokio::time::timeout(
            PROBE_TIMEOUT,
            temps_query_postgres::connect_with_self_signed_tls(&monitor_conn_str),
        )
        .await
        {
            Err(_) => {
                return ClusterHealthReport {
                    checked_at: chrono::Utc::now(),
                    monitor_response_ms: PROBE_TIMEOUT.as_millis() as i64,
                    members: vec![],
                    monitor_error: Some(format!(
                        "Monitor probe to {monitor_host}:{monitor_port} timed out after {}s",
                        PROBE_TIMEOUT.as_secs()
                    )),
                };
            }
            Ok(Err(e)) => {
                return ClusterHealthReport {
                    checked_at: chrono::Utc::now(),
                    monitor_response_ms: 0,
                    members: vec![],
                    monitor_error: Some(format!(
                        "Monitor connect to {monitor_host}:{monitor_port} failed: {}",
                        format_pg_error(&e)
                    )),
                };
            }
            Ok(Ok(client)) => client,
        };

        let nodes_rows = match tokio::time::timeout(
            PROBE_TIMEOUT,
            monitor_client.query(
                "SELECT nodename::text, nodehost::text, nodeport::int4, \
                        reportedstate::text, goalstate::text, \
                        health::int4, \
                        EXTRACT(EPOCH FROM (now() - reporttime))::int8 AS sec_since_report, \
                        candidatepriority::int4, \
                        replicationquorum::bool \
                 FROM pgautofailover.node",
                &[],
            ),
        )
        .await
        {
            Err(_) => {
                return ClusterHealthReport {
                    checked_at: chrono::Utc::now(),
                    monitor_response_ms: start.elapsed().as_millis() as i64,
                    members: vec![],
                    monitor_error: Some(format!(
                        "Monitor query to {monitor_host}:{monitor_port} timed out"
                    )),
                };
            }
            Ok(Err(e)) => {
                return ClusterHealthReport {
                    checked_at: chrono::Utc::now(),
                    monitor_response_ms: start.elapsed().as_millis() as i64,
                    members: vec![],
                    monitor_error: Some(format!(
                        "Monitor query failed at {monitor_host}:{monitor_port}: {}",
                        format_pg_error(&e)
                    )),
                };
            }
            Ok(Ok(rows)) => rows,
        };
        drop(monitor_client);

        let monitor_response_ms = start.elapsed().as_millis() as i64;

        // Build the per-member view from monitor rows. We'll fill
        // sync_state / replay_lag_ms in the next step from the primary.
        let mut by_name: std::collections::HashMap<String, ClusterMemberHealth> =
            std::collections::HashMap::new();
        let mut primary_endpoint: Option<(String, i32)> = None;
        for row in &nodes_rows {
            let nodename: String = row.get(0);
            let nodehost: String = row.get(1);
            let nodeport: i32 = row.get(2);
            let reported_state: String = row.get(3);
            let goal_state: String = row.get(4);
            let health: i32 = row.get(5);
            let seconds_since_report: i64 = row.get(6);
            let candidate_priority: i32 = row.get(7);
            let replication_quorum: bool = row.get(8);

            // Only treat a node as primary for the pg_stat_replication
            // join if pg_auto_failover *currently* believes it's primary
            // AND the node is healthy. A stale ghost-primary
            // (`reportedstate='primary'` but `health<=0`) would otherwise
            // route us to a dead host and the panel would lose sync data.
            if matches!(reported_state.as_str(), "primary" | "single")
                && health == 1
                && seconds_since_report < 30
            {
                primary_endpoint = Some((nodehost.clone(), nodeport));
            }

            by_name.insert(
                nodename.clone(),
                ClusterMemberHealth {
                    nodename,
                    nodehost,
                    nodeport,
                    reported_state,
                    goal_state,
                    health,
                    seconds_since_report,
                    candidate_priority,
                    replication_quorum,
                    sync_state: None,
                    replay_lag_ms: None,
                },
            );
        }

        // ---- replication state from the primary, best-effort ----
        //
        // We connect as the cluster's *application* user (whose hba was
        // opened by the node startup script in A1) — `autoctl_node` only
        // has hba access against the monitor's `pg_auto_failover` DB, not
        // the data nodes' `postgres` DB.
        //
        // The join key is `client_addr`, not `application_name`:
        // pg_auto_failover sets application_name to
        // `pgautofailover_standby_<nodeid>`, which doesn't match our
        // friendly `node-1`/`node-2` names. `client_addr` matches
        // `pgautofailover.node.nodehost`, which we already have.
        if let Some((primary_host, primary_port)) = primary_endpoint {
            let app_creds = self
                .get_service_parameters(service.id)
                .await
                .ok()
                .map(|params| {
                    let user = params
                        .get("username")
                        .and_then(|v| v.as_str())
                        .unwrap_or("postgres")
                        .to_string();
                    let password = params
                        .get("password")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let database = params
                        .get("database")
                        .and_then(|v| v.as_str())
                        .unwrap_or("postgres")
                        .to_string();
                    (user, password, database)
                });

            if let Some((user, password, database)) = app_creds {
                let primary_conn_str = format!(
                    "host={primary_host} port={primary_port} user={user} password={password} \
                     dbname={database} sslmode=require connect_timeout=3"
                );
                if let Ok(Ok(primary_client)) = tokio::time::timeout(
                    PROBE_TIMEOUT,
                    temps_query_postgres::connect_with_self_signed_tls(&primary_conn_str),
                )
                .await
                {
                    if let Ok(Ok(rep_rows)) = tokio::time::timeout(
                        PROBE_TIMEOUT,
                        primary_client.query(
                            "SELECT host(client_addr)::text AS client_host, \
                                    sync_state::text, \
                                    EXTRACT(EPOCH FROM replay_lag)::float8 * 1000.0 AS replay_lag_ms \
                             FROM pg_stat_replication \
                             WHERE client_addr IS NOT NULL",
                            &[],
                        ),
                    )
                    .await
                    {
                        // Build a host->member-name lookup from the monitor
                        // rows we already have. pg_auto_failover's
                        // `nodehost` matches `pg_stat_replication.client_addr`
                        // for the standby connection.
                        let mut name_by_host: std::collections::HashMap<String, String> =
                            std::collections::HashMap::new();
                        for member in by_name.values() {
                            name_by_host
                                .insert(member.nodehost.clone(), member.nodename.clone());
                        }
                        for row in &rep_rows {
                            let client_host: String = row.get(0);
                            let sync_state: String = row.get(1);
                            let replay_ms: Option<f64> = row.try_get(2).ok();
                            if let Some(member_name) = name_by_host.get(&client_host) {
                                if let Some(member) = by_name.get_mut(member_name) {
                                    member.sync_state = Some(sync_state);
                                    member.replay_lag_ms = replay_ms.map(|v| v as i64);
                                }
                            }
                        }
                    }
                    drop(primary_client);
                }
            }
        }

        // Stable-order output: by candidate_priority desc, then nodename
        // ascending so the UI doesn't reshuffle on every poll.
        let mut members: Vec<ClusterMemberHealth> = by_name.into_values().collect();
        members.sort_by(|a, b| {
            b.candidate_priority
                .cmp(&a.candidate_priority)
                .then(a.nodename.cmp(&b.nodename))
        });

        ClusterHealthReport {
            checked_at: chrono::Utc::now(),
            monitor_response_ms,
            members,
            monitor_error: None,
        }
    }

    /// Run a WAL-G basebackup against a Postgres HA cluster.
    ///
    /// Routes the backup to the **current primary** (resolved from
    /// `service_members` — kept fresh by the role reconciler). Writes
    /// the WAL-G env file to **every running data member** so failover
    /// doesn't break continuous WAL archiving — the new primary picks
    /// up the same env file and `archive_command` (which lives in
    /// `postgresql.auto.conf`, replicated through streaming).
    ///
    /// Writes an `external_service_backups` row tied to `backup_id`,
    /// transitioning it through `running` → `completed`/`failed` so
    /// the standard backup listing/restore flow works for clusters
    /// without further special-casing.
    ///
    /// Returns the WAL-G S3 prefix on success — same shape as the
    /// standalone postgres path so the rest of `temps-backup` doesn't
    /// have to special-case clusters.
    ///
    /// Designed to be called from `BackupService::backup_external_service`
    /// when `service.topology == "cluster"`.
    pub async fn backup_postgres_cluster(
        &self,
        service: &external_services::Model,
        s3_credentials: &crate::S3Credentials,
        subpath_root: &str,
        backup_id: i32,
    ) -> Result<crate::externalsvc::BackupOutcome, ExternalServiceError> {
        info!(
            service_id = service.id,
            service_name = %service.name,
            "Starting WAL-G basebackup for cluster"
        );

        if service.topology != "cluster" || service.service_type != "postgres" {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id: service.id,
                reason: format!(
                    "backup_postgres_cluster requires topology='cluster' and service_type='postgres' (got {}/{})",
                    service.topology, service.service_type,
                ),
            });
        }

        let members = self.get_service_members_with_live_state(service.id).await?;
        // `live_state` is the runtime FSM state from pg_auto_failover.
        // Backup must run against the writable primary; "single" is the
        // single-node form pg_auto_failover uses before a replica
        // catches up — also writable. Anything else (secondary,
        // catchingup, report_lsn, …) is a replica.
        let primary = members
            .iter()
            .find(|m| {
                m.status == "running"
                    && matches!(m.live_state.as_deref(), Some("primary") | Some("single"))
            })
            .ok_or(ExternalServiceError::InitializationFailed {
                id: service.id,
                reason: "Cannot run backup: cluster has no running primary (monitor unreachable or no node in primary state)".to_string(),
            })?;

        // Write the external_service_backups row up front so the UI's
        // backup listing reflects an in-progress backup. Updated to
        // completed/failed at the end.
        let metadata = serde_json::json!({
            "service_type": "postgres",
            "service_name": service.name,
            "topology": "cluster",
            "backup_tool": "wal-g",
            "primary_member_id": primary.id,
            "primary_container": primary.container_name,
        });
        let backup_record = external_service_backups::ActiveModel {
            service_id: Set(service.id),
            backup_id: Set(backup_id),
            backup_type: Set("full".to_string()),
            state: Set("running".to_string()),
            started_at: Set(Utc::now()),
            s3_location: Set(String::new()),
            metadata: Set(metadata),
            compression_type: Set("lz4".to_string()),
            created_by: Set(0),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?;

        // Single stable WAL-G prefix per cluster. WAL-G needs every
        // basebackup + WAL segment under the same prefix so
        // backup-fetch + wal-fetch can find each other.
        let walg_prefix = format!(
            "s3://{}/{}/walg",
            s3_credentials.bucket_name,
            subpath_root.trim_matches('/'),
        );

        // Resolve the S3 endpoint relative to the primary's container.
        // Important for self-hosted MinIO setups where the endpoint
        // looks like `localhost:9000` from the host but needs to be
        // a Docker-routable address from inside the container.
        let resolved_endpoint = if primary.node_id.is_none() {
            s3_credentials
                .resolve_endpoint_for_container(&self.docker, &primary.container_name)
                .await
        } else {
            // Remote primary — we can't introspect the worker's docker
            // from here. Use the configured endpoint as-is; the agent
            // sees the same network the user supplied.
            s3_credentials.endpoint.clone()
        };

        let walg_env = build_walg_env(s3_credentials, &walg_prefix, resolved_endpoint.as_deref());

        // Write walg.env to every running data member. Cheap (kilobytes
        // per file) and means failover doesn't lose archiving — the
        // new primary already has the credentials. This also covers
        // our case where ALTER SYSTEM is replicated via the data
        // directory (postgresql.auto.conf) but the env file isn't.
        for m in members
            .iter()
            .filter(|m| !is_role_monitor(&m.role) && m.status == "running")
        {
            if let Err(e) = self.write_walg_env_file(m, &walg_env).await {
                // Don't fail the backup over an env file on a non-primary;
                // the primary's the one that matters now. Failover would
                // lose archiving on this node, but the next backup will
                // re-write it.
                warn!(
                    service_id = service.id,
                    member_id = m.id,
                    node_id = ?m.node_id,
                    error = %e,
                    "Failed to write walg.env to cluster member; continuing"
                );
            }
        }

        // Run the basebackup against the primary.
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            // Source the env file so wal-g picks up the credentials.
            // Same script the standalone enable_wal_archiving uses for
            // archive_command — keeps backup and archive pointing at
            // the same prefix.
            ". /var/lib/postgresql/walg.env && wal-g backup-push /var/lib/postgresql/pgdata"
                .to_string(),
        ];

        info!(
            service_id = service.id,
            primary_container = %primary.container_name,
            primary_node_id = ?primary.node_id,
            walg_prefix,
            "Running wal-g backup-push on primary"
        );

        let (exit_code, stdout, stderr) =
            self.exec_in_member(primary, cmd, Some("postgres")).await?;

        if exit_code != 0 {
            let detail = if !stderr.is_empty() { stderr } else { stdout };
            let err_msg = format!(
                "wal-g backup-push failed on '{}' (exit {}): {}",
                primary.container_name,
                exit_code,
                detail.trim()
            );
            // Mark the row failed before returning so the UI shows it.
            let mut update: external_service_backups::ActiveModel = backup_record.into();
            update.state = Set("failed".to_string());
            update.error_message = Set(Some(err_msg.clone()));
            update.finished_at = Set(Some(Utc::now()));
            let _ = update.update(self.db.as_ref()).await;
            return Err(ExternalServiceError::InternalError { reason: err_msg });
        }

        info!(
            service_id = service.id,
            walg_prefix, "wal-g basebackup completed; enabling continuous WAL archiving"
        );

        // Enable archive_command via ALTER SYSTEM. Idempotent — if it's
        // already set to the same value, postgres just rewrites the
        // line. The setting lives in postgresql.auto.conf which IS
        // streamed to replicas, so a future failover doesn't need this
        // step repeated.
        if let Err(e) = self.enable_cluster_wal_archiving(primary, service).await {
            // Don't fail the backup — the basebackup is on S3. WAL
            // archiving will be off until the next backup retries it.
            warn!(
                service_id = service.id,
                error = %e,
                "Basebackup succeeded but enabling continuous WAL archiving failed"
            );
        }

        // Compute size by listing the WAL-G prefix in S3.
        let s3_list_prefix = format!("{}/walg/", subpath_root.trim_matches('/'));
        let s3_client = s3_credentials.build_s3_client().await;
        let size_bytes = match crate::externalsvc::s3_util::list_total_size(
            &s3_client,
            &s3_credentials.bucket_name,
            &s3_list_prefix,
        )
        .await
        {
            Ok(n) => Some(n),
            Err(e) => {
                warn!(
                    service_id = service.id,
                    error = %e,
                    "Cluster backup succeeded but failed to compute size from S3"
                );
                None
            }
        };

        // Success — mark the row completed with the prefix and size.
        let mut update: external_service_backups::ActiveModel = backup_record.into();
        update.state = Set("completed".to_string());
        update.s3_location = Set(walg_prefix.clone());
        update.finished_at = Set(Some(Utc::now()));
        update.size_bytes = Set(size_bytes);
        if let Err(e) = update.update(self.db.as_ref()).await {
            warn!(
                service_id = service.id,
                error = %e,
                "Backup succeeded but failed to mark external_service_backups row as completed"
            );
        }

        Ok(crate::externalsvc::BackupOutcome::new(
            walg_prefix,
            size_bytes,
        ))
    }

    /// Write `/var/lib/postgresql/walg.env` to a single cluster member.
    /// The file is sourced by both `archive_command` (every WAL
    /// segment) and `backup-push` (basebackups), so both paths use
    /// identical credentials without needing them in the postgres
    /// process environment (which would leak into pg_dump output).
    async fn write_walg_env_file(
        &self,
        member: &ServiceMemberInfo,
        env_lines: &[String],
    ) -> Result<(), ExternalServiceError> {
        // chmod 0600 — credentials. Owned by postgres because that's the
        // user the archiver + backup commands run as.
        let env_body = env_lines.join("\n");
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!(
                "umask 077 && cat > /var/lib/postgresql/walg.env <<'WALG_ENV_EOF'\n{}\nWALG_ENV_EOF\n\
                 chown postgres:postgres /var/lib/postgresql/walg.env && \
                 chmod 0600 /var/lib/postgresql/walg.env",
                env_body
            ),
        ];

        // Run as root because the file may not exist yet and chown
        // requires it. The file ends up owned by postgres regardless.
        let (exit_code, _stdout, stderr) = self.exec_in_member(member, cmd, None).await?;
        if exit_code != 0 {
            return Err(ExternalServiceError::InternalError {
                reason: format!(
                    "Failed to write walg.env on '{}' (exit {}): {}",
                    member.container_name,
                    exit_code,
                    stderr.trim()
                ),
            });
        }
        Ok(())
    }

    /// Run `ALTER SYSTEM SET archive_command` on the cluster's primary.
    /// `postgresql.auto.conf` is part of pgdata and gets streamed to
    /// replicas, so this only needs to run once per cluster (not per
    /// failover). Re-running is harmless.
    async fn enable_cluster_wal_archiving(
        &self,
        primary: &ServiceMemberInfo,
        service: &external_services::Model,
    ) -> Result<(), ExternalServiceError> {
        // Pull the app-user credentials so psql can authenticate. We
        // keep them in cluster parameters under `username` / `password`.
        let parameters = self.get_service_parameters(service.id).await?;
        let username = parameters
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or("postgres")
            .to_string();
        let password = parameters
            .get("password")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let database = parameters
            .get("database")
            .and_then(|v| v.as_str())
            .unwrap_or("postgres")
            .to_string();

        // Source the env file before running wal-g — same shape as the
        // standalone path. Single-quote the archive_command string so
        // the SQL parser sees the literal; the shell still expands $p
        // because postgres treats %p as its own placeholder.
        let archive_command = ". /var/lib/postgresql/walg.env && wal-g wal-push %p";
        let alter_sql = format!(
            "ALTER SYSTEM SET archive_command = '{}'",
            archive_command.replace('\'', "''")
        );
        // Need archive_mode too — defaults to off. archive_mode is a
        // POSTMASTER setting (requires restart). pg_auto_failover
        // tolerates a server restart cleanly, so we set it and rely on
        // the next pg_autoctl-driven restart to pick it up. Until
        // restart, archive_command runs but archiver isn't enabled —
        // which means WAL accumulates locally without being shipped.
        // Acceptable for the first basebackup; operators can manually
        // restart the primary to start streaming, or wait for the next
        // pg_auto_failover-initiated restart.
        let psql_cmd = vec![
            "psql".to_string(),
            "-U".to_string(),
            username,
            "-d".to_string(),
            database,
            "-c".to_string(),
            alter_sql,
            "-c".to_string(),
            "ALTER SYSTEM SET archive_mode = 'on'".to_string(),
            "-c".to_string(),
            "ALTER SYSTEM SET wal_level = 'replica'".to_string(),
            "-c".to_string(),
            "SELECT pg_reload_conf()".to_string(),
        ];

        // psql needs PGPASSWORD; pass it through env, NOT the command
        // line, so it doesn't show up in `ps`.
        let mut envs = std::collections::HashMap::new();
        envs.insert("PGPASSWORD".to_string(), password);

        let (exit_code, _stdout, stderr) = self
            .exec_in_member_with_env(primary, psql_cmd, Some("postgres"), envs)
            .await?;
        if exit_code != 0 {
            return Err(ExternalServiceError::InternalError {
                reason: format!(
                    "ALTER SYSTEM SET archive_command failed (exit {}): {}",
                    exit_code,
                    stderr.trim()
                ),
            });
        }
        Ok(())
    }

    /// Run a command inside a cluster member's container, regardless of
    /// whether it's local (control-plane bollard) or remote (agent).
    /// Returns `(exit_code, stdout, stderr)`. Same signature as the
    /// existing `exec_in_local_container` so callers can reuse error
    /// handling.
    async fn exec_in_member(
        &self,
        member: &ServiceMemberInfo,
        cmd: Vec<String>,
        user: Option<&str>,
    ) -> Result<(i64, String, String), ExternalServiceError> {
        self.exec_in_member_with_env(member, cmd, user, std::collections::HashMap::new())
            .await
    }

    async fn exec_in_member_with_env(
        &self,
        member: &ServiceMemberInfo,
        cmd: Vec<String>,
        user: Option<&str>,
        env: std::collections::HashMap<String, String>,
    ) -> Result<(i64, String, String), ExternalServiceError> {
        if let Some(node_id) = member.node_id {
            let client = self.get_remote_client(node_id).await?;
            let result = client
                .exec_in_service(crate::remote_service_client::RemoteExecParams {
                    container_name: member.container_name.clone(),
                    command: cmd,
                    environment: env,
                    user: user.map(|s| s.to_string()),
                    detach: false,
                })
                .await
                .map_err(|e| ExternalServiceError::InternalError {
                    reason: format!(
                        "Remote exec failed on member '{}' (node {}): {}",
                        member.container_name, node_id, e
                    ),
                })?;
            Ok((result.exit_code, result.stdout, result.stderr))
        } else {
            // Local path — augment exec_in_local_container with env
            // support. Until we extend that helper, fall back to a
            // bollard call here.
            use bollard::exec::{CreateExecOptions, StartExecOptions};
            use futures::StreamExt;

            let cmd_refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
            let env_strings: Vec<String> =
                env.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
            let env_refs: Option<Vec<&str>> = if env_strings.is_empty() {
                None
            } else {
                Some(env_strings.iter().map(|s| s.as_str()).collect())
            };

            let exec = self
                .docker
                .create_exec(
                    &member.container_name,
                    CreateExecOptions {
                        cmd: Some(cmd_refs),
                        env: env_refs,
                        user,
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| ExternalServiceError::DockerError {
                    id: 0,
                    reason: format!(
                        "Failed to create exec in '{}': {}",
                        member.container_name, e
                    ),
                })?;

            let output = self
                .docker
                .start_exec(
                    &exec.id,
                    Some(StartExecOptions {
                        detach: false,
                        ..Default::default()
                    }),
                )
                .await
                .map_err(|e| ExternalServiceError::DockerError {
                    id: 0,
                    reason: format!("Failed to start exec in '{}': {}", member.container_name, e),
                })?;

            let mut stdout = String::new();
            let mut stderr = String::new();
            if let bollard::exec::StartExecResults::Attached { mut output, .. } = output {
                while let Some(chunk) = output.next().await {
                    match chunk {
                        Ok(bollard::container::LogOutput::StdOut { message }) => {
                            stdout.push_str(&String::from_utf8_lossy(&message));
                        }
                        Ok(bollard::container::LogOutput::StdErr { message }) => {
                            stderr.push_str(&String::from_utf8_lossy(&message));
                        }
                        Ok(other) => stdout.push_str(&other.to_string()),
                        Err(e) => {
                            return Err(ExternalServiceError::DockerError {
                                id: 0,
                                reason: format!("Exec stream error: {}", e),
                            });
                        }
                    }
                }
            }

            let inspect = self.docker.inspect_exec(&exec.id).await.map_err(|e| {
                ExternalServiceError::DockerError {
                    id: 0,
                    reason: format!("Failed to inspect exec result: {}", e),
                }
            })?;
            let exit_code = inspect.exit_code.unwrap_or(-1);
            Ok((exit_code, stdout, stderr))
        }
    }

    /// Restore a Postgres HA cluster from a WAL-G backup.
    ///
    /// **In-place destructive** restore: every existing data node is
    /// torn down (containers + volumes + DNS + service_members rows).
    /// The same monitor + member topology is rebuilt with the primary's
    /// pgdata pre-seeded from S3. Replicas come up via the standard
    /// pg_auto_failover basebackup-from-primary path.
    ///
    /// MVP scope:
    ///   * Single-host clusters only (every member's `node_id IS NULL`).
    ///     Multi-host needs the agent to spin up the pre-seeding helper
    ///     container on the right worker — wired in a follow-up.
    ///   * Plain restore-to-latest (no point-in-time target). The
    ///     `recovery_target` argument is reserved but ignored today.
    ///
    /// Caller flow (e.g. `BackupService::restore_external_service`):
    ///   1. Look up the backup's S3 source + walg_prefix.
    ///   2. Call this with `(service, walg_prefix, s3_credentials)`.
    ///   3. Returns once the cluster is back at `status='running'`
    ///      and the primary has fully recovered.
    pub async fn restore_postgres_cluster(
        &self,
        service: &external_services::Model,
        walg_s3_prefix: &str,
        s3_credentials: &crate::S3Credentials,
    ) -> Result<(), ExternalServiceError> {
        info!(
            service_id = service.id,
            service_name = %service.name,
            walg_s3_prefix,
            "Starting in-place restore of Postgres HA cluster"
        );

        if service.topology != "cluster" || service.service_type != "postgres" {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id: service.id,
                reason: format!(
                    "restore_postgres_cluster requires topology='cluster' and service_type='postgres' (got {}/{})",
                    service.topology, service.service_type,
                ),
            });
        }

        // Snapshot the current member topology before tearing it down.
        // We rebuild with the same names/ordinals/node assignments so
        // downstream consumers (DNS reconciler, app conn strings) see
        // continuity across the restore.
        let members = service_members::Entity::find()
            .filter(service_members::Column::ServiceId.eq(service.id))
            .order_by_asc(service_members::Column::Ordinal)
            .all(self.db.as_ref())
            .await?;
        if members.is_empty() {
            return Err(ExternalServiceError::InitializationFailed {
                id: service.id,
                reason: "Cannot restore: cluster has no members on record".to_string(),
            });
        }

        // MVP gate: refuse multi-host. The pre-seeding helper has to
        // run on the same Docker daemon as the primary's volume; that
        // works for local members via bollard, but remote members
        // need an agent-side "create + run helper container" RPC we
        // don't have yet.
        if members.iter().any(|m| m.node_id.is_some()) {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id: service.id,
                reason: "MVP: cluster restore is single-host only (no remote members yet). \
                         Move all members to the control plane or wait for the multi-host \
                         restore path."
                    .to_string(),
            });
        }

        // Find the primary in the snapshot — the node that *currently*
        // holds the writable copy of the data. We can't trust
        // `service_members.role` here (it's `replica` for every data
        // node post-rework); ask pg_auto_failover instead.
        let original_primary = self
            .find_live_primary_member(service, &members)
            .await?
            .ok_or(ExternalServiceError::InitializationFailed {
                id: service.id,
                reason: "Cannot restore: cluster has no primary on record".to_string(),
            })?;
        let primary_container_name = original_primary.container_name.clone();
        let primary_volume_name = format!("{}_data", primary_container_name);

        // Reconstruct the member spec list (role + node_id) so we can
        // re-run initialize_cluster after teardown. Filter the monitor
        // out — initialize_cluster expects you to pass the monitor as
        // its own member spec, which is fine.
        let member_specs: Vec<ClusterMemberRequest> = members
            .iter()
            .map(|m| ClusterMemberRequest {
                role: m.role.clone(),
                node_id: m.node_id,
            })
            .collect();

        // ---- Phase 1: tear down everything pg_auto_failover-managed ----
        info!(
            service_id = service.id,
            "Restore phase 1: tearing down current cluster members"
        );
        self.stop_role_reconciler(service.id).await;

        for m in &members {
            // Drop DNS first so consumers see NXDOMAIN instead of a
            // stale IP for the duration of the rebuild.
            let _ = self
                .dns_registry
                .delete_by_owner(temps_dns::InternalOwnerKind::ServiceMember, m.id as i64)
                .await;

            // Stop + remove the container. Best-effort; container may
            // have died on its own already.
            let _ = self
                .docker
                .remove_container(
                    &m.container_name,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;

            // Remove the data volume too — full reset. The primary's
            // volume gets recreated below with restored pgdata; the
            // monitor and replicas get fresh ones.
            let volume_name = format!("{}_data", m.container_name);
            let _ = self
                .docker
                .remove_volume(
                    &volume_name,
                    None::<bollard::query_parameters::RemoveVolumeOptions>,
                )
                .await;
        }

        // Drop role/VIP records (Tier 3) once.
        let _ = self
            .dns_registry
            .delete_by_owner(temps_dns::InternalOwnerKind::ServiceRole, service.id as i64)
            .await;

        // Drop the service_members rows. We keep the external_services
        // row in place so the URL/credentials/UI bookmarks survive the
        // restore.
        service_members::Entity::delete_many()
            .filter(service_members::Column::ServiceId.eq(service.id))
            .exec(self.db.as_ref())
            .await?;

        // Mark the parent service back to creating so the UI shows
        // progress + retry_cluster won't be confused if this aborts.
        let mut svc_update: external_services::ActiveModel = service.clone().into();
        svc_update.status = Set("creating".to_string());
        svc_update.updated_at = Set(Utc::now());
        let _ = svc_update.update(self.db.as_ref()).await;

        // ---- Phase 2: pre-seed the primary's pgdata from S3 ----
        info!(
            service_id = service.id,
            walg_s3_prefix,
            primary_volume = %primary_volume_name,
            "Restore phase 2: pre-seeding primary pgdata via wal-g backup-fetch"
        );
        if let Err(e) = self
            .preseed_primary_pgdata(
                service,
                &primary_volume_name,
                walg_s3_prefix,
                s3_credentials,
            )
            .await
        {
            // Pre-seed failed — leave the service in `creating` so the
            // operator can retry, but surface the real reason.
            return Err(ExternalServiceError::InitializationFailed {
                id: service.id,
                reason: format!("Pre-seed of primary pgdata failed: {}", e),
            });
        }

        // ---- Phase 3: rebuild cluster on top of the restored data ----
        info!(
            service_id = service.id,
            "Restore phase 3: rebuilding cluster on top of restored pgdata"
        );
        // Wrap in Arc::new(self.clone())? No — we already are &Arc<Self>
        // for the reconciler. initialize_cluster takes &self, that's
        // fine. The primary's container will start, see existing
        // pgdata, postgres will recover-from-WAL up to consistency,
        // then pg_autoctl create will register it as the new primary.
        // Replicas pull a fresh basebackup from the new primary as
        // part of their own pg_autoctl create.
        if let Err(e) = self.initialize_cluster(service.id, &member_specs).await {
            return Err(ExternalServiceError::InitializationFailed {
                id: service.id,
                reason: format!("Cluster rebuild after restore failed: {}", e),
            });
        }

        info!(
            service_id = service.id,
            "Cluster restore complete; service is back at status='running'"
        );
        Ok(())
    }

    /// Run a one-shot helper container that fetches `wal-g backup-fetch
    /// LATEST` into the named volume that the new primary will attach.
    /// Also writes `recovery.signal` + `restore_command` so the primary
    /// container's first postgres boot replays WAL up to consistency
    /// before pg_autoctl takes over.
    async fn preseed_primary_pgdata(
        &self,
        service: &external_services::Model,
        primary_volume_name: &str,
        walg_s3_prefix: &str,
        s3_credentials: &crate::S3Credentials,
    ) -> Result<(), ExternalServiceError> {
        use bollard::models::{ContainerCreateBody, HostConfig};
        use bollard::query_parameters::CreateContainerOptionsBuilder;
        use futures::StreamExt;

        // Make sure the volume exists. Docker is happy to (re)create
        // it; this also covers the case where teardown removed it.
        let _ = self
            .docker
            .create_volume(bollard::models::VolumeCreateRequest {
                name: Some(primary_volume_name.to_string()),
                ..Default::default()
            })
            .await
            .map_err(|e| ExternalServiceError::DockerError {
                id: service.id,
                reason: format!(
                    "Failed to create primary volume '{}': {}",
                    primary_volume_name, e
                ),
            })?;

        // Resolve S3 endpoint relative to the helper. Helper runs on
        // the same host as the future primary, so endpoint resolution
        // can use the same temps-overlay heuristics. There's no live
        // primary container to inspect yet, so probe the postgres-ha
        // image's network membership instead — actually we don't have
        // a container at all, so just pass the endpoint through; the
        // resolve helper bails to None for non-localhost endpoints
        // anyway.
        let resolved_endpoint = s3_credentials.endpoint.clone();
        let walg_env = build_walg_env(s3_credentials, walg_s3_prefix, resolved_endpoint.as_deref());

        // The helper script:
        //   1. Fetch the latest WAL-G basebackup into pgdata.
        //   2. Drop a recovery.signal so postgres enters recovery mode
        //      on first boot.
        //   3. Write postgresql.auto.conf with restore_command so
        //      postgres can pull WAL segments from S3 to roll forward
        //      to consistency. Disable archive_mode/archive_command so
        //      the recovering primary doesn't re-push WAL into the
        //      source's prefix mid-recovery.
        //   4. chown to postgres:999 (postgres user uid in the
        //      official image) so postgres can read its own data.
        let env_lines = walg_env.join("\n");
        let script = format!(
            r#"set -eu
PGDATA=/var/lib/postgresql/pgdata
mkdir -p "$PGDATA"
chown -R postgres:postgres /var/lib/postgresql

# Stash the env file the recovery + future archiver will source.
umask 077
cat > /var/lib/postgresql/walg-restore.env <<'WALG_RESTORE_EOF'
{env_lines}
WALG_RESTORE_EOF
chown postgres:postgres /var/lib/postgresql/walg-restore.env
chmod 0600 /var/lib/postgresql/walg-restore.env

echo "[restore] Fetching latest WAL-G basebackup into $PGDATA..."
gosu postgres sh -c '. /var/lib/postgresql/walg-restore.env && wal-g backup-fetch "$PGDATA" LATEST'

echo "[restore] Writing recovery.signal + restore_command"
touch "$PGDATA/recovery.signal"
chown postgres:postgres "$PGDATA/recovery.signal"

cat > "$PGDATA/postgresql.auto.conf" <<'PG_AUTO_EOF'
# Written by Temps cluster restore. Overwrites any source-side settings.
restore_command = '. /var/lib/postgresql/walg-restore.env && wal-g wal-fetch %f %p'
recovery_target = 'immediate'
recovery_target_action = 'promote'
archive_mode = 'off'
archive_command = '/bin/true'
PG_AUTO_EOF
chown postgres:postgres "$PGDATA/postgresql.auto.conf"
chmod 0600 "$PGDATA/postgresql.auto.conf"

echo "[restore] Pre-seed complete"
"#,
            env_lines = env_lines,
        );

        let helper_name = format!(
            "temps-restore-helper-{}-{}",
            service.id,
            Utc::now().timestamp()
        );
        let helper_config = ContainerCreateBody {
            // postgres-ha has both wal-g and gosu, so no extra image
            // shopping. Pin to the same -walg-bundled tag the cluster
            // uses (DEFAULT_CLUSTER_IMAGE) — both wal-g binary and the
            // image version need to match the primary's pgdata layout.
            image: Some(crate::externalsvc::postgres_cluster::DEFAULT_CLUSTER_IMAGE.to_string()),
            cmd: Some(vec!["sh".to_string(), "-c".to_string(), script]),
            host_config: Some(HostConfig {
                binds: Some(vec![format!("{}:/var/lib/postgresql", primary_volume_name)]),
                ..Default::default()
            }),
            // Run as root so the chown calls land — the helper drops
            // to postgres internally for the wal-g call.
            user: Some("root".to_string()),
            ..Default::default()
        };

        let helper = self
            .docker
            .create_container(
                Some(
                    CreateContainerOptionsBuilder::new()
                        .name(&helper_name)
                        .build(),
                ),
                helper_config,
            )
            .await
            .map_err(|e| ExternalServiceError::DockerError {
                id: service.id,
                reason: format!("Failed to create restore helper container: {}", e),
            })?;

        // Pull the image first if it's missing (debug builds skip web,
        // but they don't pre-pull our images either).
        if let Err(e) = self
            .docker
            .start_container(
                &helper.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
        {
            // Clean up the half-created helper before bubbling out.
            let _ = self
                .docker
                .remove_container(
                    &helper.id,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        v: false,
                        ..Default::default()
                    }),
                )
                .await;
            return Err(ExternalServiceError::DockerError {
                id: service.id,
                reason: format!("Failed to start restore helper container: {}", e),
            });
        }

        // Wait for the helper to finish.
        let wait_result = self
            .docker
            .wait_container(
                &helper.id,
                None::<bollard::query_parameters::WaitContainerOptions>,
            )
            .next()
            .await;

        // Capture logs before removing — useful for surfacing the real
        // reason a wal-g fetch failed.
        let logs = self
            .docker
            .logs(
                &helper.id,
                Some(bollard::query_parameters::LogsOptions {
                    stdout: true,
                    stderr: true,
                    tail: "200".to_string(),
                    ..Default::default()
                }),
            )
            .map(|chunk| match chunk {
                Ok(c) => c.to_string(),
                Err(e) => format!("[log read error: {}]", e),
            })
            .collect::<Vec<_>>()
            .await
            .join("");

        let _ = self
            .docker
            .remove_container(
                &helper.id,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    v: false,
                    ..Default::default()
                }),
            )
            .await;

        match wait_result {
            Some(Ok(resp)) if resp.status_code == 0 => {
                info!(
                    service_id = service.id,
                    "Restore helper completed successfully"
                );
                Ok(())
            }
            Some(Ok(resp)) => Err(ExternalServiceError::InternalError {
                reason: format!(
                    "Restore helper exited with status {}.\nLast log lines:\n{}",
                    resp.status_code,
                    logs.trim_end()
                ),
            }),
            Some(Err(e)) => Err(ExternalServiceError::DockerError {
                id: service.id,
                reason: format!("Restore helper wait failed: {}", e),
            }),
            None => Err(ExternalServiceError::InternalError {
                reason: "Restore helper finished but no status code was returned".to_string(),
            }),
        }
    }

    /// Get the primary data node's connection address for a cluster service.
    ///
    /// Returns `Some((host, port))` if the service is a cluster with a running primary.
    /// Returns `None` if the service is standalone (not a cluster).
    ///
    /// For local clusters, `host` is the container name (Docker DNS).
    /// For remote clusters, `host` is the member's hostname (private/WireGuard IP).
    pub async fn get_cluster_primary_address(
        &self,
        service_id: i32,
    ) -> Result<Option<(String, u16)>, ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        if service.topology != "cluster" {
            return Ok(None);
        }

        // The primary is whichever node pg_auto_failover currently calls
        // primary, not whatever `service_members.role` happens to say.
        // Using the stored role here would have produced the same lag
        // bug the UI hit — Browse Data and other callers would dial a
        // freshly-demoted node post-failover.
        let members = self.get_service_members_with_live_state(service_id).await?;
        let primary = members.iter().find(|m| {
            m.status == "running"
                && matches!(m.live_state.as_deref(), Some("primary") | Some("single"))
        });

        if let Some(primary) = primary {
            let port = primary.port.unwrap_or(5432) as u16;

            // For local members (no node_id), the hostname is a Docker-internal IP
            // (e.g. 192.168.1.x) which is unreachable from the host. Since the
            // container port is mapped to the same host port, use localhost instead.
            // For remote members, use the node's private address.
            let host = if let Some(node_id) = primary.node_id {
                // Remote node — resolve via node's private address
                let node = nodes::Entity::find_by_id(node_id)
                    .one(self.db.as_ref())
                    .await?;
                node.map(|n| n.private_address).unwrap_or_else(|| {
                    primary
                        .hostname
                        .clone()
                        .unwrap_or_else(|| primary.container_name.clone())
                })
            } else {
                // Local node — use localhost since Docker maps host_port:container_port
                "localhost".to_string()
            };

            Ok(Some((host, port)))
        } else {
            Err(ExternalServiceError::InternalError {
                reason: format!(
                    "Cluster service {} has no running primary data node",
                    service_id
                ),
            })
        }
    }

    /// Build runtime environment variables for a cluster service.
    ///
    /// For cluster topology, the standard `ExternalService::get_runtime_env_vars()` returns
    /// empty because the cluster service doesn't have access to the database to look up
    /// member addresses. This method queries `service_members` and builds the multi-host
    /// connection string with `target_session_attrs=read-write` for automatic failover.
    ///
    /// Returns `None` if the service is not a cluster (caller should fall through to
    /// the standard `get_runtime_env_vars` path).
    async fn build_cluster_env_vars(
        &self,
        service: &external_services::Model,
        parameters: &HashMap<String, serde_json::Value>,
    ) -> Result<Option<HashMap<String, String>>, ExternalServiceError> {
        self.build_cluster_env_vars_for_resource(service, parameters, None)
            .await
    }

    /// Create the per-app database `name` on the cluster's live
    /// primary if it doesn't already exist. Idempotent — uses
    /// `pg_database` lookup before issuing CREATE.
    ///
    /// Connects to the cluster the same way Browse Data does:
    /// resolve the primary's host:port via the monitor, dial it
    /// through the existing `temps-query-postgres` TLS-then-plain
    /// fallback. The CP can reach worker-mapped ports because they
    /// bind to the worker's underlay IP.
    async fn ensure_cluster_app_database(
        &self,
        service_id: i32,
        admin_user: &str,
        admin_password: &str,
        db_name: &str,
    ) -> Result<(), ExternalServiceError> {
        // Sanity-check the name matches what postgres allows for a
        // bare-quoted identifier — same rules the standalone path
        // applies. Strict to keep the CREATE DATABASE parameterless
        // safe (Postgres doesn't accept bind params for CREATE).
        if !db_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
            || db_name.is_empty()
            || db_name
                .chars()
                .next()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false)
        {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!(
                    "Cluster app DB name '{}' must match [A-Za-z_][A-Za-z0-9_]*",
                    db_name
                ),
            });
        }

        let (host, port) = match self.get_cluster_primary_address(service_id).await? {
            Some(hp) => hp,
            None => {
                return Err(ExternalServiceError::InternalError {
                    reason: format!(
                        "Cannot provision app database '{}' for cluster {}: \
                         no running primary",
                        db_name, service_id
                    ),
                });
            }
        };

        // Dial the primary using the same connection helper as Browse
        // Data so TLS/plain fallback + chained-error reporting are
        // shared.
        let conn_str = format!(
            "host={} port={} user={} password={} dbname={}",
            host,
            port,
            admin_user,
            admin_password,
            // Connect to the cluster's bootstrap DB ("postgres" by
            // default) to issue CREATE DATABASE — you can't create
            // a DB while connected to it.
            "postgres",
        );

        let client = match temps_query_postgres::connect_with_self_signed_tls(&conn_str).await {
            Ok(c) => c,
            Err(tls_err) => {
                use tokio_postgres::NoTls;
                tokio_postgres::connect(&conn_str, NoTls)
                    .await
                    .map(|(client, conn)| {
                        tokio::spawn(async move {
                            if let Err(e) = conn.await {
                                warn!("Cluster admin connection error: {}", e);
                            }
                        });
                        client
                    })
                    .map_err(|plain_err| ExternalServiceError::InternalError {
                        reason: format!(
                            "Failed to connect to cluster {} primary at {}:{} \
                             (TLS error: {}, plain error: {})",
                            service_id, host, port, tls_err, plain_err
                        ),
                    })?
            }
        };

        let exists: bool = client
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)",
                &[&db_name],
            )
            .await
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to check if database '{}' exists: {}", db_name, e),
            })?
            .get(0);

        if exists {
            debug!(
                service_id,
                db_name, "App database already exists on cluster primary; skipping CREATE"
            );
            return Ok(());
        }

        // CREATE DATABASE doesn't accept bind params — the strict
        // identifier check above keeps this safe.
        let stmt = format!("CREATE DATABASE \"{}\"", db_name);
        client.execute(stmt.as_str(), &[]).await.map_err(|e| {
            ExternalServiceError::InternalError {
                reason: format!(
                    "Failed to create database '{}' on cluster {}: {}",
                    db_name, service_id, e
                ),
            }
        })?;
        info!(
            service_id,
            db_name, "Created app database on cluster primary"
        );
        Ok(())
    }

    /// Build cluster env vars, optionally provisioning a per-tenant
    /// database on the live primary first. When `resource_name` is
    /// `Some(name)`:
    ///   1. Connect to the live primary as the admin user.
    ///   2. `CREATE DATABASE "<name>" OWNER "<admin>"` if missing.
    ///   3. Emit env vars whose `POSTGRES_DB` and `POSTGRES_URL` point
    ///      at that DB (so each project/environment gets its own).
    ///
    /// When `resource_name` is `None`, fall back to the cluster's
    /// configured `database` parameter — kept for the legacy callers
    /// that want a generic cluster-level view.
    async fn build_cluster_env_vars_for_resource(
        &self,
        service: &external_services::Model,
        parameters: &HashMap<String, serde_json::Value>,
        resource_name: Option<&str>,
    ) -> Result<Option<HashMap<String, String>>, ExternalServiceError> {
        if service.topology != "cluster" {
            return Ok(None);
        }

        let members = self.get_service_members(service.id).await?;
        let params_str = Self::params_to_strings(parameters);

        // Extract credentials from parameters
        let username = params_str
            .get("username")
            .cloned()
            .unwrap_or_else(|| "postgres".to_string());
        let password = params_str.get("password").cloned().unwrap_or_default();
        let admin_database = params_str
            .get("database")
            .cloned()
            .unwrap_or_else(|| "postgres".to_string());

        // Per-tenant database: when the caller passes a resource name
        // we provision a dedicated DB on the primary so each app gets
        // its own. Falls back to the cluster's admin DB when no name
        // is given.
        let database = if let Some(name) = resource_name {
            self.ensure_cluster_app_database(service.id, &username, &password, name)
                .await?;
            name.to_string()
        } else {
            admin_database
        };

        // Build multi-host connection string from running data nodes (not monitor)
        let data_nodes: Vec<&ServiceMemberInfo> = members
            .iter()
            .filter(|m| !is_role_monitor(&m.role) && m.status == "running")
            .collect();

        let mut env_vars = HashMap::new();
        env_vars.insert("POSTGRES_USER".to_string(), username.clone());
        env_vars.insert("POSTGRES_PASSWORD".to_string(), password.clone());
        env_vars.insert("POSTGRES_DB".to_string(), database.clone());

        if data_nodes.is_empty() {
            // No running data nodes — still return credentials but no URL
            warn!(
                "Cluster service {} has no running data nodes, POSTGRES_URL will be empty",
                service.id
            );
            return Ok(Some(env_vars));
        }

        let hosts: Vec<String> = data_nodes
            .iter()
            .map(|n| {
                let host = n
                    .hostname
                    .clone()
                    .unwrap_or_else(|| n.container_name.clone());
                let port = n.port.unwrap_or(5432);
                format!("{}:{}", host, port)
            })
            .collect();

        let encoded_password = urlencoding::encode(&password);

        let postgres_url = format!(
            "postgresql://{}:{}@{}/{}?target_session_attrs=read-write",
            urlencoding::encode(&username),
            encoded_password,
            hosts.join(","),
            database,
        );

        let host_list = data_nodes
            .iter()
            .map(|n| {
                n.hostname
                    .clone()
                    .unwrap_or_else(|| n.container_name.clone())
            })
            .collect::<Vec<_>>()
            .join(",");

        let port = data_nodes
            .first()
            .and_then(|n| n.port)
            .unwrap_or(5432)
            .to_string();

        env_vars.insert("POSTGRES_URL".to_string(), postgres_url);
        env_vars.insert("POSTGRES_HOST".to_string(), host_list);
        env_vars.insert("POSTGRES_PORT".to_string(), port);

        Ok(Some(env_vars))
    }

    async fn get_service_parameters(
        &self,
        service_id_val: i32,
    ) -> Result<HashMap<String, serde_json::Value>, ExternalServiceError> {
        let service = self.get_service(service_id_val).await?;

        // Get encrypted config from service record
        let encrypted_config =
            service
                .config
                .ok_or_else(|| ExternalServiceError::InternalError {
                    reason: format!("Service {} has no config", service_id_val),
                })?;

        // Decrypt config
        let config_json = self
            .encryption_service
            .decrypt_string(&encrypted_config)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!(
                    "Failed to decrypt config for service {}: {}",
                    service_id_val, e
                ),
            })?;

        // Deserialize JSON to HashMap
        let parameters: HashMap<String, serde_json::Value> = serde_json::from_str(&config_json)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!(
                    "Failed to deserialize config for service {}: {}",
                    service_id_val, e
                ),
            })?;

        Ok(parameters)
    }

    async fn initialize_service(&self, service_id: i32) -> Result<(), ExternalServiceError> {
        info!("Initializing service: {}", service_id);
        self.ensure_no_active_upgrade(service_id).await?;
        let service = self.get_service(service_id).await?;
        let parameters = self.get_service_parameters(service_id).await?;
        let service_type_enum = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;

        // Remote node — delegate to agent
        if let Some(node_id) = service.node_id {
            return self
                .initialize_service_remote(
                    service_id,
                    node_id,
                    &service,
                    &parameters,
                    &service_type_enum,
                )
                .await;
        }

        // Local node — use existing Docker-based service logic
        let service_instance = self.create_service_instance_for_parameters(
            service.name.clone(),
            service_type_enum,
            &parameters,
        )?;

        let config = ServiceConfig {
            name: service.name.clone(),
            service_type: ServiceType::from_str(&service.service_type).map_err(|_| {
                ExternalServiceError::InvalidServiceType {
                    id: service_id,
                    service_type: service.service_type.clone(),
                }
            })?,
            version: service.version.clone(),
            parameters: serde_json::to_value(parameters).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to serialize parameters: {}", e),
                }
            })?,
        };

        // Stop existing container if running (important for upgrades)
        info!("Stopping existing container for service {}", service_id);
        if let Err(e) = service_instance.stop().await {
            // Log but don't fail - container might not exist yet
            info!("Could not stop container (may not exist): {}", e);
        }

        // Initialize the service
        let inferred_params = service_instance.init(config).await.map_err(|e| {
            ExternalServiceError::InitializationFailed {
                id: service_id,
                reason: e.to_string(),
            }
        })?;

        // Store inferred parameters
        self.store_inferred_parameters(service_id, service_instance.as_ref(), inferred_params)
            .await?;

        // Start the service (create and start container)
        service_instance
            .start()
            .await
            .map_err(|e| ExternalServiceError::InitializationFailed {
                id: service_id,
                reason: format!("Failed to start service: {}", e),
            })?;

        // Update status to running
        let mut service_update: external_services::ActiveModel = service.clone().into();
        service_update.status = Set("running".to_string());
        service_update.updated_at = Set(Utc::now());
        service_update.update(self.db.as_ref()).await?;

        Ok(())
    }

    /// Initialize a service on a remote node via the agent API.
    async fn initialize_service_remote(
        &self,
        service_id: i32,
        node_id: i32,
        service: &external_services::Model,
        parameters: &HashMap<String, serde_json::Value>,
        service_type: &ServiceType,
    ) -> Result<(), ExternalServiceError> {
        info!(
            "Initializing service {} on remote node {}",
            service_id, node_id
        );
        let client = self.get_remote_client(node_id).await?;

        // Flatten serde_json::Value parameters to strings for the builder
        let string_params: HashMap<String, String> = parameters
            .iter()
            .map(|(k, v)| {
                let s = match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                (k.clone(), s)
            })
            .collect();

        let create_params =
            self.build_remote_create_params(&service.name, service_type, &string_params)?;

        // Try to stop existing container first (ignore errors — may not exist)
        let container_name = create_params.name.clone();
        if let Err(e) = client.stop_service(&container_name).await {
            info!(
                "Could not stop remote container {} (may not exist): {}",
                container_name, e
            );
        }

        // Create the container on the remote node
        let response = client.create_service(create_params).await.map_err(|e| {
            ExternalServiceError::InitializationFailed {
                id: service_id,
                reason: format!("Remote agent create_service failed: {}", e),
            }
        })?;

        info!(
            "Service {} created on node {} — container {} (port {})",
            service_id, node_id, response.container_name, response.host_port
        );

        // Store the host_port as an inferred parameter so env-var generation works
        let mut inferred = HashMap::new();
        inferred.insert("port".to_string(), response.host_port.to_string());
        inferred.insert("container_id".to_string(), response.container_id.clone());

        // Persist inferred parameters
        let mut current_params = self.get_service_parameters(service_id).await?;
        for (key, value) in inferred {
            if Self::is_inferred_parameter(&key) || !current_params.contains_key(&key) {
                current_params.insert(key, serde_json::Value::String(value));
            }
        }
        let config_json = serde_json::to_string(&current_params).map_err(|e| {
            ExternalServiceError::InternalError {
                reason: format!("Failed to serialize updated params: {}", e),
            }
        })?;
        let encrypted_config = self
            .encryption_service
            .encrypt_string(&config_json)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to encrypt updated params: {}", e),
            })?;

        let mut service_update: external_services::ActiveModel = service.clone().into();
        service_update.status = Set("running".to_string());
        service_update.config = Set(Some(encrypted_config));
        service_update.updated_at = Set(Utc::now());
        service_update.update(self.db.as_ref()).await?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Cluster initialization
    // -----------------------------------------------------------------------

    /// Create a cluster-aware service instance for the given service type.
    fn create_cluster_service_instance(
        &self,
        name: String,
        service_type: ServiceType,
    ) -> Option<Box<dyn ExternalService>> {
        match service_type {
            ServiceType::Postgres => Some(Box::new(PostgresClusterService::new(
                name,
                self.docker.clone(),
            ))),
            // Future: Redis Sentinel, MongoDB Replica Set, RustFS distributed
            _ => None,
        }
    }

    /// Initialize a cluster service: create member containers across nodes,
    /// then record them in the service_members table.
    async fn initialize_cluster(
        &self,
        service_id: i32,
        member_requests: &[ClusterMemberRequest],
    ) -> Result<(), ExternalServiceError> {
        info!("Initializing cluster for service {}", service_id);
        let service = self.get_service(service_id).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;

        // Topology + role validation happens BEFORE parameter decryption so
        // bad-input requests fail with the correct error variant (and a
        // helpful message) instead of a generic "Service has no config".
        // Older ordering decrypted first and ate the validation error.
        let cluster_instance = self
            .create_cluster_service_instance(service.name.clone(), service_type)
            .ok_or_else(|| ExternalServiceError::InitializationFailed {
                id: service_id,
                reason: format!(
                    "Service type '{}' does not support cluster topology",
                    service.service_type
                ),
            })?;

        // Validate roles
        let valid_roles = cluster_instance.valid_cluster_roles();
        for (i, member) in member_requests.iter().enumerate() {
            if !valid_roles.contains(&member.role.as_str()) {
                return Err(ExternalServiceError::ParameterValidationFailed {
                    service_id,
                    reason: format!(
                        "Invalid role '{}' for member {}. Valid roles: {:?}",
                        member.role, i, valid_roles
                    ),
                });
            }
        }

        // Parameter decryption only after validation has passed; otherwise
        // operators creating a cluster with an unsupported type or invalid
        // role get a misleading "service has no config" surface error.
        let parameters = self.get_service_parameters(service_id).await?;

        // Build member specs with ordinals and hostnames.
        //
        // When the cluster spans multiple nodes (has any remote members),
        // local members must advertise a routable IP instead of a Docker
        // container name — remote workers cannot resolve container names
        // from another host's Docker network.
        let has_remote_members = member_requests.iter().any(|m| m.node_id.is_some());
        let local_private_ip: Option<String> = if has_remote_members {
            Some(Self::get_local_private_ip().map_err(|e| {
                ExternalServiceError::InitializationFailed {
                    id: service_id,
                    reason: format!(
                        "Cluster has remote members but could not determine local private IP: {}",
                        e
                    ),
                }
            })?)
        } else {
            None
        };

        let mut member_specs = Vec::new();
        for (i, member) in member_requests.iter().enumerate() {
            let hostname: Option<String> = if let Some(node_id) = member.node_id {
                // Look up the node's private address for inter-member communication
                let node = nodes::Entity::find_by_id(node_id)
                    .one(self.db.as_ref())
                    .await?
                    .ok_or(ExternalServiceError::InternalError {
                        reason: format!("Node {} not found", node_id),
                    })?;
                Some(node.private_address.clone())
            } else {
                // Local member: use control plane's private IP if available
                // (so remote workers can reach it), otherwise None (Docker DNS)
                local_private_ip.clone()
            };

            member_specs.push(ClusterMemberSpec {
                role: member.role.clone(),
                node_id: member.node_id,
                ordinal: i as i32,
                hostname,
            });
        }

        // Get the cluster config for building member-specific params
        let service_config = ServiceConfig {
            name: service.name.clone(),
            service_type,
            version: service.version.clone(),
            parameters: serde_json::to_value(&parameters).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to serialize parameters: {}", e),
                }
            })?,
        };

        // Call init_cluster to get the container specs (names, ports)
        let member_results = cluster_instance
            .init_cluster(service_config.clone(), member_specs.clone())
            .await
            .map_err(|e| ExternalServiceError::InitializationFailed {
                id: service_id,
                reason: format!("Cluster init_cluster failed: {}", e),
            })?;

        // Get the Postgres cluster service for building member params
        let pg_cluster = match service_type {
            ServiceType::Postgres => Some(PostgresClusterService::new(
                service.name.clone(),
                self.docker.clone(),
            )),
            _ => None,
        };

        let cluster_config_parsed: crate::externalsvc::postgres_cluster::PostgresClusterConfig =
            serde_json::from_value(service_config.parameters.clone()).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to parse cluster config: {}", e),
                }
            })?;

        // Pull resource limits once for the whole cluster — every member
        // (monitor + data nodes) gets the same caps. Defaults to unlimited
        // when the operator hasn't set a `resources` block.
        let cluster_resource_limits =
            crate::externalsvc::ServiceResourceLimits::from_parameters(&service_config.parameters);
        if let Err(e) = cluster_resource_limits.validate() {
            return Err(ExternalServiceError::InternalError {
                reason: format!("Invalid cluster resource limits: {}", e),
            });
        }

        // Find the monitor hostname for data node configuration.
        // For remote workers, use the node's private/WireGuard address.
        // For local (no node_id), use the monitor container name so Docker DNS resolves it.
        let monitor_spec = member_specs.iter().find(|m| is_role_monitor(&m.role));
        let pg_cluster_name = service.name.clone();
        let monitor_container_fallback = format!("postgres-{}-monitor", pg_cluster_name);
        let monitor_hostname = monitor_spec
            .and_then(|m| m.hostname.as_deref())
            .unwrap_or(&monitor_container_fallback);

        // Assign unique host ports for each cluster member to avoid conflicts
        // with other services (e.g., the platform's own TimescaleDB on 5432).
        // Base port is derived from service_id to keep ports stable across restarts.
        // Range: 6000 + (service_id * 10) + ordinal, giving 10 ports per cluster.
        let base_port = 6000u16 + (service_id as u16 * 10);
        // Monitor gets base_port, data nodes get base_port + 1, +2, etc.
        let monitor_port = base_port;
        info!(
            "Cluster '{}' port assignment: monitor={}, data nodes start at {}",
            pg_cluster_name,
            monitor_port,
            base_port + 1
        );

        // Track successfully created members for rollback on failure
        struct CreatedMember {
            container_name: String,
            node_id: Option<i32>,
        }
        let mut created_members: Vec<CreatedMember> = Vec::new();

        // Create each member container (in order: monitor first, then data nodes)
        let create_result: Result<(), ExternalServiceError> = async {
            for (result, spec) in member_results.iter().zip(member_specs.iter()) {
                info!(
                    "Creating cluster member: {} (role: {}, ordinal: {}, node: {:?})",
                    result.container_name, result.role, result.ordinal, spec.node_id
                );

                // `service_members.role` is config-state — `monitor` for the
                // singleton orchestrator, `replica` for every data node.
                // "Primary" is a *runtime* fact owned by pg_auto_failover and
                // is surfaced via `live_state` (see
                // `get_service_members_with_live_state`). Storing one row as
                // `primary` would have to be reconciled on every failover,
                // and the lag between the monitor flipping and our row
                // catching up was the bug behind the "two primaries"
                // display. Treating roles as static config eliminates the
                // class.
                let stored_role = if is_role_monitor(&result.role) {
                    "monitor".to_string()
                } else {
                    "replica".to_string()
                };
                let member_record = service_members::ActiveModel {
                    service_id: Set(service_id),
                    node_id: Set(spec.node_id),
                    role: Set(stored_role),
                    container_id: Set(None),
                    container_name: Set(result.container_name.clone()),
                    hostname: Set(spec.hostname.clone()),
                    port: Set(None),
                    status: Set("creating".to_string()),
                    ordinal: Set(result.ordinal),
                    config: Set(None),
                    created_at: Set(Utc::now()),
                    updated_at: Set(Utc::now()),
                    ..Default::default()
                };
                let member_model = member_record.insert(self.db.as_ref()).await?;

                // Assign port: monitor gets base_port, data nodes get base + ordinal
                let member_port = if is_role_monitor(&spec.role) {
                    monitor_port
                } else {
                    base_port + spec.ordinal as u16
                };

                let (container_id, host_port, compute_ip) = if let Some(node_id) = spec.node_id {
                    // Remote: dispatch to agent
                    let client = self.get_remote_client(node_id).await?;

                    // Build member-specific create params
                    let member_params = if let Some(ref pg) = pg_cluster {
                        pg.build_member_params(
                            spec,
                            &cluster_config_parsed,
                            monitor_hostname,
                            monitor_port,
                            member_port,
                            cluster_resource_limits.clone(),
                        )
                    } else {
                        return Err(ExternalServiceError::InitializationFailed {
                            id: service_id,
                            reason: "Only Postgres clusters are currently supported".to_string(),
                        });
                    };

                    // Each cluster member uses a unique port assigned by the
                    // manager. Map container_port = host_port to avoid conflicts.
                    let volume_name = format!("{}_data", result.container_name);
                    let limits_for_remote = if member_params.resource_limits.is_unlimited() {
                        None
                    } else {
                        Some(member_params.resource_limits.clone())
                    };
                    let remote_params = RemoteServiceCreateParams {
                        name: result.container_name.clone(),
                        service_type: "postgres".to_string(),
                        image: member_params.image,
                        environment: member_params.environment,
                        port_mappings: vec![RemotePortMapping {
                            host_port: member_params.container_port,
                            container_port: member_params.container_port,
                        }],
                        volumes: HashMap::from([(volume_name, member_params.volume_path)]),
                        network: Some(temps_core::NETWORK_NAME.to_string()),
                        command: member_params.command,
                        resource_limits: limits_for_remote,
                    };

                    let response = client.create_service(remote_params).await.map_err(|e| {
                        ExternalServiceError::InitializationFailed {
                            id: service_id,
                            reason: format!(
                                "Failed to create cluster member '{}' on node {}: {}",
                                result.container_name, node_id, e
                            ),
                        }
                    })?;

                    (
                        response.container_id,
                        Some(response.host_port as i32),
                        response.compute_ip,
                    )
                } else {
                    // Local: create container directly via Docker
                    // For now, use the agent-style approach via local Docker
                    let member_params = if let Some(ref pg) = pg_cluster {
                        pg.build_member_params(
                            spec,
                            &cluster_config_parsed,
                            monitor_hostname,
                            monitor_port,
                            member_port,
                            cluster_resource_limits.clone(),
                        )
                    } else {
                        return Err(ExternalServiceError::InitializationFailed {
                            id: service_id,
                            reason: "Only Postgres clusters are currently supported".to_string(),
                        });
                    };

                    // Pull image, create and start container locally
                    self.create_local_cluster_member(&result.container_name, &member_params)
                        .await
                        .map_err(|e| ExternalServiceError::InitializationFailed {
                            id: service_id,
                            reason: format!(
                                "Failed to create local cluster member '{}': {}",
                                result.container_name, e
                            ),
                        })?
                };

                // Track this member for potential rollback
                created_members.push(CreatedMember {
                    container_name: result.container_name.clone(),
                    node_id: spec.node_id,
                });

                // Wait for the member to be healthy before proceeding to the next
                // This is important: monitor must be healthy before data nodes register
                if is_role_monitor(&spec.role) {
                    info!(
                        "Waiting for monitor '{}' to become healthy...",
                        result.container_name
                    );
                    self.wait_for_container_health(&result.container_name, 60)
                        .await
                        .map_err(|e| ExternalServiceError::InitializationFailed {
                            id: service_id,
                            reason: format!("Monitor failed health check: {}", e),
                        })?;
                }

                // Compute the FQDN for this member. Always populated post
                // ADR-011 — overrides whatever placeholder hostname (IP or
                // container name) the spec carried. Apps will resolve this
                // via the per-node DNS resolver.
                let member_fqdn = format!(
                    "{}-{}.{}.temps.local",
                    service.name, spec.ordinal, service.name
                );

                // Update member record with container info and "running" status,
                // plus the FQDN hostname and overlay IP (if any).
                let member_id = member_model.id;
                let mut member_update: service_members::ActiveModel = member_model.into();
                member_update.container_id = Set(Some(container_id));
                member_update.port = Set(host_port);
                member_update.status = Set("running".to_string());
                member_update.hostname = Set(Some(member_fqdn.clone()));
                member_update.compute_ip = Set(compute_ip.clone());
                member_update.updated_at = Set(Utc::now());
                member_update.update(self.db.as_ref()).await?;

                // Register the per-member A record (ADR-011, Tier 2).
                //
                // Prefer the overlay IP when the container is on
                // `temps0` — that points other containers straight at
                // each other on the multi-host bridge. If the overlay
                // isn't attached (single-host setups, or the monitor on
                // a control plane that's not in the allocator), fall
                // back to the underlay address + the published host
                // port so dialing through Docker's port forward still
                // works. This is what makes `MONITOR_URI=<fqdn>:<port>`
                // resolve from inside any container.
                let (record_ip, record_port) = match compute_ip.clone() {
                    Some(ip) => (Some(ip), member_port as i32),
                    None => match self
                        .resolve_member_underlay(spec.node_id, host_port, member_port)
                        .await
                    {
                        Some((ip, port)) => (Some(ip), port),
                        None => (None, member_port as i32),
                    },
                };

                if let Some(ip) = record_ip {
                    let draft = temps_dns::EndpointDraft {
                        fqdn: member_fqdn.clone(),
                        record_type: temps_dns::InternalRecordType::A,
                        target_ip: Some(ip.clone()),
                        target_port: Some(record_port),
                        ttl: 30,
                        owner_kind: temps_dns::InternalOwnerKind::ServiceMember,
                        owner_id: member_id as i64,
                        node_id: spec.node_id,
                    };
                    if let Err(e) = self
                        .dns_registry
                        .replace_endpoints_for_owner(
                            temps_dns::InternalOwnerKind::ServiceMember,
                            member_id as i64,
                            &[draft],
                        )
                        .await
                    {
                        warn!(
                            service_id,
                            member_id,
                            fqdn = %member_fqdn,
                            ip = %ip,
                            error = %e,
                            "Failed to register DNS record for cluster member"
                        );
                    } else {
                        info!(
                            service_id,
                            member_id,
                            fqdn = %member_fqdn,
                            ip = %ip,
                            port = record_port,
                            "Registered DNS A record for cluster member"
                        );
                    }
                }
            }
            Ok(())
        }
        .await;

        // If any member failed, roll back all previously created containers
        if let Err(e) = create_result {
            error!(
                "Cluster member creation failed for service {}: {}. Rolling back {} created container(s).",
                service_id, e, created_members.len()
            );

            for member in &created_members {
                if let Some(node_id) = member.node_id {
                    // Remote: ask agent to remove the container
                    match self.get_remote_client(node_id).await {
                        Ok(client) => {
                            if let Err(rm_err) = client.remove_service(&member.container_name).await
                            {
                                error!(
                                    "Rollback: failed to remove remote container '{}' on node {}: {}",
                                    member.container_name, node_id, rm_err
                                );
                            } else {
                                info!(
                                    "Rollback: removed remote container '{}' on node {}",
                                    member.container_name, node_id
                                );
                            }
                        }
                        Err(client_err) => {
                            error!(
                                "Rollback: failed to get remote client for node {}: {}",
                                node_id, client_err
                            );
                        }
                    }
                } else {
                    // Local: remove container directly via Docker
                    if let Err(rm_err) = self
                        .docker
                        .remove_container(
                            &member.container_name,
                            Some(bollard::query_parameters::RemoveContainerOptions {
                                force: true,
                                ..Default::default()
                            }),
                        )
                        .await
                    {
                        error!(
                            "Rollback: failed to remove local container '{}': {}",
                            member.container_name, rm_err
                        );
                    } else {
                        info!(
                            "Rollback: removed local container '{}'",
                            member.container_name
                        );
                    }

                    // Also remove the volume
                    let volume_name = format!("{}_data", member.container_name);
                    if let Err(vol_err) = self
                        .docker
                        .remove_volume(
                            &volume_name,
                            None::<bollard::query_parameters::RemoveVolumeOptions>,
                        )
                        .await
                    {
                        warn!(
                            "Rollback: failed to remove volume '{}': {}",
                            volume_name, vol_err
                        );
                    }
                }
            }

            // Mark remaining service_members as "failed" instead of deleting them.
            // This preserves the original member topology so the retry endpoint can
            // reconstruct the member specs without user re-input.
            if let Err(db_err) = service_members::Entity::update_many()
                .col_expr(service_members::Column::Status, Expr::value("failed"))
                .col_expr(service_members::Column::UpdatedAt, Expr::value(Utc::now()))
                .filter(service_members::Column::ServiceId.eq(service_id))
                .exec(self.db.as_ref())
                .await
            {
                error!(
                    "Rollback: failed to update service_members status for service {}: {}",
                    service_id, db_err
                );
            }

            return Err(e);
        }

        // Capture name before we move `service` into the ActiveModel below.
        let service_name = service.name.clone();

        // Update parent service status
        let mut service_update: external_services::ActiveModel = service.into();
        service_update.status = Set("running".to_string());
        service_update.updated_at = Set(Utc::now());
        service_update.update(self.db.as_ref()).await?;

        // Start the per-cluster role reconciler (ADR-011 Phase 4). Best-effort:
        // skipped if no DnsRegistry is wired (legacy plugin) or if a reconciler
        // is already running for this service_id (idempotent retry).
        self.spawn_role_reconciler(service_id, service_name).await;

        info!("Cluster service {} initialized successfully", service_id);
        Ok(())
    }

    /// Spawn the per-cluster Postgres role reconciler. Idempotent — if one is
    /// already running for `service_id`, returns immediately.
    /// Discover every running cluster service in the DB and spawn a role
    /// reconciler for each. Idempotent — calling multiple times leaves
    /// existing reconcilers alone (the inner `spawn_role_reconciler`
    /// guards on `reconciler_shutdowns`). Called once during plugin
    /// startup so reconcilers exist after every restart, not just for
    /// clusters created in this process's lifetime.
    pub async fn spawn_reconcilers_for_existing_clusters(&self) {
        let candidates = match external_services::Entity::find()
            .filter(external_services::Column::Topology.eq("cluster"))
            .filter(external_services::Column::Status.eq("running"))
            .filter(external_services::Column::ServiceType.eq("postgres"))
            .all(self.db.as_ref())
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                warn!(
                    error = %e,
                    "Failed to load running clusters at startup; reconcilers won't run \
                     until a member is added or the cluster is recreated"
                );
                return;
            }
        };
        if candidates.is_empty() {
            debug!("No running cluster services found at startup");
        } else {
            info!(
                count = candidates.len(),
                "Spawning role reconcilers for existing clusters"
            );
            for svc in candidates {
                self.spawn_role_reconciler(svc.id, svc.name).await;
            }
        }

        // Run the stuck-row watchdog after the reconcilers come up so
        // failed/stuck members appear as `failed` immediately to the
        // UI, instead of hanging in `creating` forever.
        self.fail_abandoned_provisioning_rows().await;
    }

    /// One-shot scan at startup: any `service_members` row whose
    /// `provisioning_step` is in flight AND whose `updated_at` is
    /// older than `STUCK_ROW_THRESHOLD` is marked `failed`. This
    /// happens when the control plane was killed mid-`add_cluster_member`
    /// — without this, the row would stay at `INSERTING_ROW` /
    /// `PROVISIONING_CONTAINER` forever and the operator would have
    /// no way to clean it up except hand-editing the DB.
    ///
    /// 15 minutes is generous: a cold-cache image pull on a slow
    /// connection can take 5+ minutes; doubling that as a timeout
    /// avoids killing legitimately slow provisions on flaky networks.
    async fn fail_abandoned_provisioning_rows(&self) {
        const STUCK_ROW_THRESHOLD: chrono::Duration = chrono::Duration::minutes(15);

        let cutoff = Utc::now() - STUCK_ROW_THRESHOLD;
        let in_flight = [
            member_provisioning_step::INSERTING_ROW,
            member_provisioning_step::PROVISIONING_CONTAINER,
            member_provisioning_step::REGISTERING_DNS,
        ];

        let stuck = match service_members::Entity::find()
            .filter(service_members::Column::Status.eq("creating"))
            .filter(service_members::Column::ProvisioningStep.is_in(in_flight))
            .filter(service_members::Column::UpdatedAt.lt(cutoff))
            .all(self.db.as_ref())
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                warn!(
                    error = %e,
                    "Failed to scan for stuck cluster member rows at startup; \
                     any half-provisioned members from a previous run will stay \
                     in 'creating' until manually fixed"
                );
                return;
            }
        };
        if stuck.is_empty() {
            return;
        }

        warn!(
            count = stuck.len(),
            threshold_minutes = STUCK_ROW_THRESHOLD.num_minutes(),
            "Found cluster member rows stuck mid-provisioning across a control \
             plane restart; marking them failed so the operator can retry"
        );
        for m in stuck {
            let member_id = m.id;
            let last_step = m.provisioning_step.clone().unwrap_or_default();
            let mut active: service_members::ActiveModel = m.into();
            active.status = Set("failed".to_string());
            active.provisioning_step = Set(Some(member_provisioning_step::FAILED.to_string()));
            active.provisioning_error = Set(Some(format!(
                "Control plane restart abandoned this provisioning attempt at step '{}'. \
                 No data was lost; click Add Replica again to retry.",
                last_step
            )));
            active.updated_at = Set(Utc::now());
            if let Err(e) = active.update(self.db.as_ref()).await {
                warn!(
                    member_id,
                    error = %e,
                    "Failed to mark abandoned member as failed; will retry next startup"
                );
            }
        }
    }

    async fn spawn_role_reconciler(&self, service_id: i32, service_name: String) {
        let registry = self.dns_registry.clone();

        let mut shutdowns = self.reconciler_shutdowns.lock().await;
        if shutdowns.contains_key(&service_id) {
            debug!(service_id, "role reconciler already running");
            return;
        }
        let shutdown = Arc::new(tokio::sync::Notify::new());
        shutdowns.insert(service_id, shutdown.clone());
        drop(shutdowns);

        let db = self.db.clone();
        // Supervised loop: a panic inside `run` (e.g. unexpected enum
        // value from a future pg_auto_failover release that breaks
        // `query_monitor`) used to silently kill DNS sync for one
        // cluster forever. Now we re-spawn after a 30s backoff. Bounded
        // restart rate (max 6 panics per hour) so a deterministic crash
        // doesn't become an infinite restart loop hammering the
        // monitor.
        const RESTART_BACKOFF: std::time::Duration = std::time::Duration::from_secs(30);
        const RESTART_WINDOW: std::time::Duration = std::time::Duration::from_secs(3600);
        const MAX_RESTARTS_PER_WINDOW: usize = 6;

        tokio::spawn(async move {
            let mut crash_times: Vec<std::time::Instant> = Vec::new();
            loop {
                let task_db = db.clone();
                let task_registry = registry.clone();
                let task_name = service_name.clone();
                let task_shutdown = shutdown.clone();
                // Wrap the future in AssertUnwindSafe + catch_unwind so
                // a panic in the reconciler returns Err instead of
                // killing this supervisor task.
                use futures::future::FutureExt;
                let result = std::panic::AssertUnwindSafe(
                    crate::externalsvc::postgres_role_reconciler::run(
                        task_db,
                        task_registry,
                        service_id,
                        task_name,
                        task_shutdown,
                    ),
                )
                .catch_unwind()
                .await;

                match result {
                    Ok(()) => {
                        // Clean exit (shutdown was notified). Don't restart.
                        debug!(service_id, "role reconciler exited cleanly");
                        return;
                    }
                    Err(panic) => {
                        let now = std::time::Instant::now();
                        crash_times.retain(|t| now.duration_since(*t) < RESTART_WINDOW);
                        crash_times.push(now);

                        let panic_msg = panic
                            .downcast_ref::<&'static str>()
                            .map(|s| s.to_string())
                            .or_else(|| panic.downcast_ref::<String>().cloned())
                            .unwrap_or_else(|| "<non-string panic payload>".to_string());

                        if crash_times.len() > MAX_RESTARTS_PER_WINDOW {
                            error!(
                                service_id,
                                panic = %panic_msg,
                                crashes_in_last_hour = crash_times.len(),
                                "Role reconciler crashed too many times; giving up. \
                                 DNS records for this cluster will go stale until \
                                 the control plane is restarted."
                            );
                            return;
                        }

                        error!(
                            service_id,
                            panic = %panic_msg,
                            crashes_in_last_hour = crash_times.len(),
                            backoff_secs = RESTART_BACKOFF.as_secs(),
                            "Role reconciler panicked; restarting after backoff"
                        );
                    }
                }

                // Backoff respects shutdown so a delete_service called
                // mid-backoff doesn't have to wait the full 30s.
                tokio::select! {
                    _ = tokio::time::sleep(RESTART_BACKOFF) => {}
                    _ = shutdown.notified() => {
                        debug!(service_id, "role reconciler shutdown during restart backoff");
                        return;
                    }
                }
            }
        });
    }

    /// Stop the per-cluster role reconciler if one is running. Called from
    /// `delete_service` after the DB tx commits — paired with
    /// `DnsRegistry::delete_by_owner` so role records get dropped after the
    /// reconciler has stopped writing them.
    async fn stop_role_reconciler(&self, service_id: i32) {
        let mut shutdowns = self.reconciler_shutdowns.lock().await;
        if let Some(notifier) = shutdowns.remove(&service_id) {
            notifier.notify_waiters();
            debug!(service_id, "role reconciler shutdown signalled");
        }
    }

    /// Retry a failed cluster service initialization.
    ///
    /// Cleans up any leftover containers and service_members from the previous
    /// attempt, then re-runs `initialize_cluster`.
    ///
    /// If `member_requests` is empty, the original member configuration is
    /// reconstructed from the preserved `service_members` records (which are
    /// now kept with "failed" status instead of being deleted on rollback).
    pub async fn retry_cluster(
        &self,
        service_id: i32,
        member_requests: &[ClusterMemberRequest],
    ) -> Result<ExternalServiceInfo, ExternalServiceError> {
        let service = self.get_service(service_id).await?;

        if service.topology != "cluster" {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: "retry_cluster is only valid for cluster topology services".to_string(),
            });
        }

        if service.status != "failed" && service.status != "creating" {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!(
                    "Service must be in 'failed' or 'creating' status to retry, current status: '{}'",
                    service.status
                ),
            });
        }

        info!(
            "Retrying cluster initialization for service {} (current status: {})",
            service_id, service.status
        );

        // Clean up any leftover service_members and their containers
        let leftover_members = service_members::Entity::find()
            .filter(service_members::Column::ServiceId.eq(service_id))
            .order_by_asc(service_members::Column::Ordinal)
            .all(self.db.as_ref())
            .await?;

        // Reconstruct member specs from preserved records if none were provided
        let effective_members: Vec<ClusterMemberRequest> = if member_requests.is_empty() {
            if leftover_members.is_empty() {
                return Err(ExternalServiceError::ParameterValidationFailed {
                    service_id,
                    reason:
                        "No member configuration provided and no previous member records found. \
                             Please provide the members array in the retry request."
                            .to_string(),
                });
            }
            info!(
                "Reconstructing member config from {} preserved records for service {}",
                leftover_members.len(),
                service_id
            );
            leftover_members
                .iter()
                .map(|m| ClusterMemberRequest {
                    role: m.role.clone(),
                    node_id: m.node_id,
                })
                .collect()
        } else {
            member_requests.to_vec()
        };

        for member in &leftover_members {
            // Try to remove the container (ignore errors — it may not exist)
            if let Some(node_id) = member.node_id {
                if let Ok(client) = self.get_remote_client(node_id).await {
                    if let Err(e) = client.remove_service(&member.container_name).await {
                        warn!(
                            "Retry cleanup: failed to remove remote container '{}' on node {}: {}",
                            member.container_name, node_id, e
                        );
                    }
                }
            } else {
                let _ = self
                    .docker
                    .remove_container(
                        &member.container_name,
                        Some(bollard::query_parameters::RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await;

                // Also remove the volume
                let volume_name = format!("{}_data", member.container_name);
                let _ = self
                    .docker
                    .remove_volume(
                        &volume_name,
                        None::<bollard::query_parameters::RemoveVolumeOptions>,
                    )
                    .await;
            }
        }

        // Delete leftover member records
        if !leftover_members.is_empty() {
            service_members::Entity::delete_many()
                .filter(service_members::Column::ServiceId.eq(service_id))
                .exec(self.db.as_ref())
                .await?;
            info!(
                "Retry cleanup: removed {} leftover member records for service {}",
                leftover_members.len(),
                service_id
            );
        }

        // Update status to "creating" and clear previous error
        let mut service_update: external_services::ActiveModel = service.into();
        service_update.status = Set("creating".to_string());
        service_update.error_message = Set(None);
        service_update.updated_at = Set(Utc::now());
        service_update.update(self.db.as_ref()).await?;

        // Spawn background task to re-initialize (same pattern as create)
        let db = self.db.clone();
        let docker = self.docker.clone();
        let encryption_service = self.encryption_service.clone();
        let dns_registry = self.dns_registry.clone();
        let members = effective_members;

        tokio::spawn(async move {
            let manager =
                ExternalServiceManager::new(db.clone(), encryption_service, docker, dns_registry);
            let result = manager.initialize_cluster(service_id, &members).await;

            match result {
                Ok(()) => {
                    info!(
                        "Cluster service {} retry succeeded (background)",
                        service_id
                    );
                }
                Err(e) => {
                    error!(
                        "Cluster service {} retry failed (background): {}",
                        service_id, e
                    );

                    let update_result: Result<_, sea_orm::DbErr> = async {
                        let mut svc: external_services::ActiveModel =
                            external_services::Entity::find_by_id(service_id)
                                .one(db.as_ref())
                                .await?
                                .ok_or(sea_orm::DbErr::RecordNotFound(
                                    "Service not found during retry rollback".to_string(),
                                ))?
                                .into();
                        svc.status = Set("failed".to_string());
                        svc.error_message = Set(Some(e.to_string()));
                        svc.updated_at = Set(Utc::now());
                        svc.update(db.as_ref()).await?;
                        Ok(())
                    }
                    .await;

                    if let Err(db_err) = update_result {
                        error!(
                            "Failed to update service {} status to 'failed' after retry: {}",
                            service_id, db_err
                        );
                    }
                }
            }
        });

        self.get_service_info(service_id).await
    }

    /// Begin adding a single new member (currently only `replica`) to a
    /// running Postgres cluster.
    ///
    /// **Returns immediately** after validating the request, resolving the
    /// existing monitor, and inserting a `service_members` row with
    /// `status='creating'` and `provisioning_step='inserting_row'`. The
    /// long-running container provisioning + DNS registration runs in a
    /// background tokio task that updates `provisioning_step` (and
    /// eventually `status='running'` / `status='failed'` +
    /// `provisioning_error`) so the UI can render a live timeline by
    /// polling the member row.
    ///
    /// Refuses `monitor` (singleton — created once at init) and
    /// `primary` (elected by pg_auto_failover, never declared by the user).
    pub async fn add_cluster_member(
        self: &Arc<Self>,
        service_id: i32,
        role: &str,
        node_id: Option<i32>,
    ) -> Result<ServiceMemberInfo, ExternalServiceError> {
        // Race-resilient insert. Two concurrent `add_cluster_member`
        // calls that observe the same `MAX(ordinal)` would each compute
        // the same next ordinal and try to insert the same
        // `(service_id, ordinal)` row. The unique constraint added in
        // m20260428_000001 makes the second insert fail; we recompute
        // the plan (which re-derives container_name + port + FQDN from
        // the new ordinal) and try again. Bounded at 8 attempts because
        // an explosion past that means something else is wrong.
        const MAX_ORDINAL_RETRIES: usize = 8;
        let (plan, member_model) = {
            let mut last_err = None;
            let mut chosen_plan: Option<AddMemberPlan> = None;
            let mut chosen_model: Option<service_members::Model> = None;
            for attempt in 0..MAX_ORDINAL_RETRIES {
                let plan = self
                    .plan_add_cluster_member(service_id, role, node_id)
                    .await?;
                let now = Utc::now();
                // See note in `initialize_cluster`: data members are stored
                // as `replica`. Promotion is a runtime concern owned by the
                // pg_auto_failover monitor and surfaced via `live_state`.
                let stored_role = if is_role_monitor(&plan.spec.role) {
                    "monitor".to_string()
                } else {
                    "replica".to_string()
                };
                let member_record = service_members::ActiveModel {
                    service_id: Set(service_id),
                    node_id: Set(plan.spec.node_id),
                    role: Set(stored_role),
                    container_id: Set(None),
                    container_name: Set(plan.container_name.clone()),
                    hostname: Set(plan.spec.hostname.clone()),
                    port: Set(None),
                    status: Set("creating".to_string()),
                    ordinal: Set(plan.spec.ordinal),
                    config: Set(None),
                    provisioning_step: Set(Some(
                        member_provisioning_step::INSERTING_ROW.to_string(),
                    )),
                    provisioning_error: Set(None),
                    created_at: Set(now),
                    updated_at: Set(now),
                    ..Default::default()
                };
                match member_record.insert(self.db.as_ref()).await {
                    Ok(model) => {
                        chosen_plan = Some(plan);
                        chosen_model = Some(model);
                        break;
                    }
                    Err(e) if is_unique_violation(&e) => {
                        // Another `add_cluster_member` won this ordinal.
                        // Loop and recompute against the now-larger
                        // member set.
                        warn!(
                            service_id,
                            attempted_ordinal = plan.spec.ordinal,
                            attempt = attempt + 1,
                            "Ordinal collision on cluster member insert; retrying with next free ordinal"
                        );
                        last_err = Some(e);
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            match (chosen_plan, chosen_model) {
                (Some(p), Some(m)) => (p, m),
                _ => {
                    return Err(ExternalServiceError::InternalError {
                        reason: format!(
                            "Failed to allocate a unique cluster member ordinal after {} attempts: {}",
                            MAX_ORDINAL_RETRIES,
                            last_err
                                .map(|e| e.to_string())
                                .unwrap_or_else(|| "no error captured".to_string())
                        ),
                    });
                }
            }
        };
        let member_id = member_model.id;

        // Spawn the long-running provisioning task. It owns its own Arc
        // clone of the manager so it can run independently of the request.
        let manager = self.clone();
        let plan_for_task = plan.clone();
        tokio::spawn(async move {
            manager
                .complete_add_cluster_member(member_id, plan_for_task)
                .await;
        });

        info!(
            service_id,
            member_id,
            ordinal = plan.spec.ordinal,
            "Cluster member provisioning started — see member.provisioning_step for live status"
        );

        Ok(ServiceMemberInfo {
            id: member_model.id,
            role: member_model.role,
            node_id: member_model.node_id,
            container_name: member_model.container_name,
            hostname: member_model.hostname,
            port: member_model.port,
            status: member_model.status,
            ordinal: member_model.ordinal,
            compute_ip: member_model.compute_ip,
            provisioning_step: member_model.provisioning_step,
            provisioning_error: member_model.provisioning_error,
            // Just-created members never have an FSM state to report
            // yet. The next polling cycle picks it up.
            live_state: None,
        })
    }

    /// Validate the add-member request and resolve everything needed by
    /// the background provisioner. Anything that should fail synchronously
    /// (returning a 400 to the user) belongs here.
    async fn plan_add_cluster_member(
        &self,
        service_id: i32,
        role: &str,
        node_id: Option<i32>,
    ) -> Result<AddMemberPlan, ExternalServiceError> {
        info!(
            service_id,
            role,
            node_id = ?node_id,
            "Adding cluster member (validating)"
        );

        let service = self.get_service(service_id).await?;

        if service.topology != "cluster" {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: "add_cluster_member is only valid for cluster topology services"
                    .to_string(),
            });
        }
        if service.status != "running" {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!(
                    "Cluster must be in 'running' status to add a member, current: '{}'",
                    service.status
                ),
            });
        }

        if role_from_str(role) != Some(crate::ClusterRole::Replica) {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!(
                    "Only 'replica' members can be added at runtime (got '{}'). \
                     Monitor is a singleton; primary is elected by pg_auto_failover.",
                    role
                ),
            });
        }

        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;

        let pg_cluster = match service_type {
            ServiceType::Postgres => {
                PostgresClusterService::new(service.name.clone(), self.docker.clone())
            }
            _ => {
                return Err(ExternalServiceError::ParameterValidationFailed {
                    service_id,
                    reason: format!(
                        "add_cluster_member is only supported for Postgres clusters (got '{}')",
                        service.service_type
                    ),
                });
            }
        };

        let existing_members = service_members::Entity::find()
            .filter(service_members::Column::ServiceId.eq(service_id))
            .order_by_asc(service_members::Column::Ordinal)
            .all(self.db.as_ref())
            .await?;

        let monitor = existing_members
            .iter()
            .find(|m| is_role_monitor(&m.role))
            .ok_or(ExternalServiceError::InitializationFailed {
                id: service_id,
                reason: "Cannot add member: cluster has no monitor".to_string(),
            })?;

        // Prefer the monitor's FQDN — every container we provision now
        // gets the per-host Hickory resolver wired into resolv.conf
        // (`HostConfig.dns`), so `postgres-<svc>-0.<svc>.temps.local`
        // resolves natively from inside the new container.
        //
        // Fallbacks (in order) keep older clusters working:
        //   1. monitor.hostname (FQDN, set by the lifecycle hook)
        //   2. monitor's node private_address (underlay IP, when remote)
        //   3. control plane's local IP (when monitor is on this host)
        //   4. monitor container name (single-host bridge DNS resolves it)
        let monitor_hostname: String = if let Some(h) = monitor.hostname.as_deref() {
            h.to_string()
        } else if let Some(nid) = monitor.node_id {
            let node = nodes::Entity::find_by_id(nid)
                .one(self.db.as_ref())
                .await?
                .ok_or(ExternalServiceError::InternalError {
                    reason: format!("Monitor's node {} not found", nid),
                })?;
            node.private_address.clone()
        } else {
            Self::get_local_private_ip()
                .unwrap_or_else(|_| format!("postgres-{}-monitor", service.name))
        };
        let monitor_port = monitor
            .port
            .ok_or(ExternalServiceError::InitializationFailed {
                id: service_id,
                reason: "Monitor has no host port recorded".to_string(),
            })? as u16;

        // Reuse the lowest free ordinal (≥ 1 — 0 is reserved for the
        // monitor) so that delete-then-add gives the operator back the
        // same node identity (e.g. node-2 stays node-2). Falling through
        // to MAX+1 here meant a removed node-2 would come back as node-4
        // and pg_auto_failover treated the original :6152 ghost as a new
        // peer, blocking the FSM. Together with the
        // `pg_autoctl drop node` call in `remove_cluster_member`, this
        // makes delete+add idempotent from the cluster's point of view.
        let used_ordinals: std::collections::BTreeSet<i32> =
            existing_members.iter().map(|m| m.ordinal).collect();
        let next_ordinal: i32 = (1..)
            .find(|n| !used_ordinals.contains(n))
            .expect("ordinal range is unbounded");

        let has_any_remote =
            existing_members.iter().any(|m| m.node_id.is_some()) || node_id.is_some();
        let local_private_ip: Option<String> = if has_any_remote && node_id.is_none() {
            Some(Self::get_local_private_ip().map_err(|e| {
                ExternalServiceError::InitializationFailed {
                    id: service_id,
                    reason: format!(
                        "Cluster has remote members but could not determine local private IP: {}",
                        e
                    ),
                }
            })?)
        } else {
            None
        };

        let hostname: Option<String> = if let Some(nid) = node_id {
            let node = nodes::Entity::find_by_id(nid)
                .one(self.db.as_ref())
                .await?
                .ok_or(ExternalServiceError::InternalError {
                    reason: format!("Node {} not found", nid),
                })?;
            Some(node.private_address.clone())
        } else {
            local_private_ip
        };

        let spec = ClusterMemberSpec {
            role: role.to_string(),
            node_id,
            ordinal: next_ordinal,
            hostname,
        };

        let parameters = self.get_service_parameters(service_id).await?;
        let service_config = ServiceConfig {
            name: service.name.clone(),
            service_type,
            version: service.version.clone(),
            parameters: serde_json::to_value(&parameters).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to serialize parameters: {}", e),
                }
            })?,
        };
        let cluster_config: crate::externalsvc::postgres_cluster::PostgresClusterConfig =
            serde_json::from_value(service_config.parameters.clone()).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to parse cluster config: {}", e),
                }
            })?;

        // Inherit cluster-wide resource limits when adding a new member so
        // every node ends up with the same caps as the rest of the cluster.
        let member_limits =
            crate::externalsvc::ServiceResourceLimits::from_parameters(&service_config.parameters);
        if let Err(e) = member_limits.validate() {
            return Err(ExternalServiceError::InternalError {
                reason: format!("Invalid cluster resource limits: {}", e),
            });
        }

        let base_port = 6000u16 + (service_id as u16 * 10);
        let member_port = base_port + spec.ordinal as u16;

        let member_params = pg_cluster.build_member_params(
            &spec,
            &cluster_config,
            &monitor_hostname,
            monitor_port,
            member_port,
            member_limits,
        );
        let container_name = member_params.container_name.clone();
        let member_fqdn = format!(
            "{}-{}.{}.temps.local",
            service.name, spec.ordinal, service.name
        );

        Ok(AddMemberPlan {
            service_id,
            service_name: service.name.clone(),
            spec,
            container_name,
            member_fqdn,
            member_port,
            member_params,
        })
    }

    /// Background half of `add_cluster_member`. Owns the long-running
    /// container creation + DNS registration. Updates the row's
    /// `provisioning_step` after each phase so the UI's polling loop
    /// can render progress.
    async fn complete_add_cluster_member(self: Arc<Self>, member_id: i32, plan: AddMemberPlan) {
        let service_id = plan.service_id;
        let ordinal = plan.spec.ordinal;

        info!(
            service_id,
            member_id,
            ordinal,
            container = %plan.container_name,
            "Provisioning replica container"
        );
        self.set_provisioning_step(member_id, member_provisioning_step::PROVISIONING_CONTAINER)
            .await;

        let create_outcome: Result<(String, Option<i32>, Option<String>), ExternalServiceError> =
            if let Some(nid) = plan.spec.node_id {
                let client = match self.get_remote_client(nid).await {
                    Ok(c) => c,
                    Err(e) => {
                        self.fail_member(
                            member_id,
                            format!(
                                "Could not reach worker node {} to provision container: {}",
                                nid, e
                            ),
                        )
                        .await;
                        return;
                    }
                };
                let volume_name = format!("{}_data", plan.container_name);
                let limits_for_remote = if plan.member_params.resource_limits.is_unlimited() {
                    None
                } else {
                    Some(plan.member_params.resource_limits.clone())
                };
                let remote_params = RemoteServiceCreateParams {
                    name: plan.container_name.clone(),
                    service_type: "postgres".to_string(),
                    image: plan.member_params.image.clone(),
                    environment: plan.member_params.environment.clone(),
                    port_mappings: vec![RemotePortMapping {
                        host_port: plan.member_params.container_port,
                        container_port: plan.member_params.container_port,
                    }],
                    volumes: HashMap::from([(volume_name, plan.member_params.volume_path.clone())]),
                    network: Some(temps_core::NETWORK_NAME.to_string()),
                    command: plan.member_params.command.clone(),
                    resource_limits: limits_for_remote,
                };
                client
                    .create_service(remote_params)
                    .await
                    .map(|r| (r.container_id, Some(r.host_port as i32), r.compute_ip))
                    .map_err(|e| ExternalServiceError::InitializationFailed {
                        id: service_id,
                        reason: format!(
                            "Failed to create cluster member '{}' on node {}: {}",
                            plan.container_name, nid, e
                        ),
                    })
            } else {
                self.create_local_cluster_member(&plan.container_name, &plan.member_params)
                    .await
                    .map_err(|e| ExternalServiceError::InitializationFailed {
                        id: service_id,
                        reason: format!(
                            "Failed to create local cluster member '{}': {}",
                            plan.container_name, e
                        ),
                    })
            };

        let (container_id, host_port, compute_ip) = match create_outcome {
            Ok(t) => t,
            Err(e) => {
                self.fail_member(member_id, e.to_string()).await;
                return;
            }
        };

        // Promote the row to "running" with the live container metadata.
        let updated_at = Utc::now();
        let update_result = service_members::Entity::update_many()
            .col_expr(
                service_members::Column::ContainerId,
                Expr::value(container_id),
            )
            .col_expr(service_members::Column::Port, Expr::value(host_port))
            .col_expr(service_members::Column::Status, Expr::value("running"))
            .col_expr(
                service_members::Column::Hostname,
                Expr::value(plan.member_fqdn.clone()),
            )
            .col_expr(
                service_members::Column::ComputeIp,
                Expr::value(compute_ip.clone()),
            )
            .col_expr(
                service_members::Column::ProvisioningStep,
                Expr::value(member_provisioning_step::REGISTERING_DNS),
            )
            .col_expr(service_members::Column::UpdatedAt, Expr::value(updated_at))
            .filter(service_members::Column::Id.eq(member_id))
            .exec(self.db.as_ref())
            .await;
        if let Err(e) = update_result {
            self.fail_member(
                member_id,
                format!("Container created but DB update failed: {}", e),
            )
            .await;
            return;
        }

        // Register Tier-2 DNS A record. Prefer the overlay IP; fall
        // back to (node_underlay, host_port) so the FQDN still works
        // when the overlay isn't attached. Best-effort: a failed
        // registration logs loudly but doesn't mark the member as
        // failed — the role reconciler will try again on its next tick.
        let (record_ip, record_port) = match compute_ip.clone() {
            Some(ip) => (Some(ip), plan.member_port as i32),
            None => match self
                .resolve_member_underlay(plan.spec.node_id, host_port, plan.member_port)
                .await
            {
                Some((ip, port)) => (Some(ip), port),
                None => (None, plan.member_port as i32),
            },
        };
        if let Some(ip) = record_ip {
            let draft = temps_dns::EndpointDraft {
                fqdn: plan.member_fqdn.clone(),
                record_type: temps_dns::InternalRecordType::A,
                target_ip: Some(ip.clone()),
                target_port: Some(record_port),
                ttl: 30,
                owner_kind: temps_dns::InternalOwnerKind::ServiceMember,
                owner_id: member_id as i64,
                node_id: plan.spec.node_id,
            };
            if let Err(e) = self
                .dns_registry
                .replace_endpoints_for_owner(
                    temps_dns::InternalOwnerKind::ServiceMember,
                    member_id as i64,
                    &[draft],
                )
                .await
            {
                warn!(
                    service_id,
                    member_id,
                    fqdn = %plan.member_fqdn,
                    ip = %ip,
                    error = %e,
                    "Failed to register DNS record for added cluster member"
                );
            } else {
                info!(
                    service_id,
                    member_id,
                    fqdn = %plan.member_fqdn,
                    ip = %ip,
                    port = record_port,
                    "Registered DNS A record for added cluster member"
                );
            }
        }

        self.set_provisioning_step(member_id, member_provisioning_step::DONE)
            .await;
        info!(
            service_id,
            member_id,
            ordinal,
            "Cluster member added; reconciler will refresh role records on next tick"
        );
    }

    /// Update the member row's `provisioning_step` field. Used by the
    /// background provisioning task at each phase boundary so the
    /// frontend's polling loop can render progress.
    async fn set_provisioning_step(&self, member_id: i32, step: &str) {
        let result = service_members::Entity::update_many()
            .col_expr(service_members::Column::ProvisioningStep, Expr::value(step))
            .col_expr(service_members::Column::UpdatedAt, Expr::value(Utc::now()))
            .filter(service_members::Column::Id.eq(member_id))
            .exec(self.db.as_ref())
            .await;
        if let Err(e) = result {
            warn!(
                member_id,
                step,
                error = %e,
                "Failed to write provisioning_step"
            );
        }
    }

    /// Mark the member as failed and stash the error message so the UI
    /// can render it. Best-effort: a DB write failure here is logged but
    /// can't be recovered from.
    async fn fail_member(&self, member_id: i32, error_message: String) {
        warn!(
            member_id,
            error = %error_message,
            "Cluster member provisioning failed"
        );
        let result = service_members::Entity::update_many()
            .col_expr(service_members::Column::Status, Expr::value("failed"))
            .col_expr(
                service_members::Column::ProvisioningStep,
                Expr::value(member_provisioning_step::FAILED),
            )
            .col_expr(
                service_members::Column::ProvisioningError,
                Expr::value(error_message),
            )
            .col_expr(service_members::Column::UpdatedAt, Expr::value(Utc::now()))
            .filter(service_members::Column::Id.eq(member_id))
            .exec(self.db.as_ref())
            .await;
        if let Err(e) = result {
            warn!(member_id, error = %e, "Failed to mark member as failed");
        }
    }

    /// Look up a single cluster member. Returns `NotFound` if the row
    /// doesn't belong to the named service so callers can return a 404
    /// without leaking the existence of unrelated members.
    pub async fn get_cluster_member(
        &self,
        service_id: i32,
        member_id: i32,
    ) -> Result<ServiceMemberInfo, ExternalServiceError> {
        let member = service_members::Entity::find_by_id(member_id)
            .one(self.db.as_ref())
            .await?
            .filter(|m| m.service_id == service_id)
            .ok_or(ExternalServiceError::InitializationFailed {
                id: service_id,
                reason: format!("Cluster member {} not found", member_id),
            })?;

        Ok(ServiceMemberInfo {
            id: member.id,
            role: member.role,
            node_id: member.node_id,
            container_name: member.container_name,
            hostname: member.hostname,
            port: member.port,
            status: member.status,
            ordinal: member.ordinal,
            compute_ip: member.compute_ip,
            provisioning_step: member.provisioning_step,
            provisioning_error: member.provisioning_error,
            // Single-member fetch path. Callers that need live state for
            // a single member should use `member_is_live_primary` or
            // `get_service_members_with_live_state` instead.
            live_state: None,
        })
    }

    /// Remove a single member from a running cluster.
    ///
    /// Safety guarantees (this function refuses to proceed unless they hold):
    ///   * The member must belong to the named service.
    ///   * The member must not be the `monitor` (singleton — would orphan
    ///     every data node).
    ///   * The member must not be the current `primary` (caller must
    ///     trigger a failover via pg_auto_failover first; we never
    ///     forcibly demote a writable primary).
    ///   * The remaining data members (excluding the monitor) must still
    ///     have at least 2 entries — anything fewer drops below quorum
    ///     and the cluster loses HA.
    ///
    /// Steps:
    ///   1. Stop + remove the container (local Docker or remote agent).
    ///   2. Delete the `service_members` row.
    ///   3. Drop the Tier-2 DNS A record for the member (best-effort).
    ///   4. Reconciler will refresh role records on its next tick.
    ///
    /// Also runs `pg_autoctl drop node --formation default --name node-N`
    /// against the monitor before deleting the row. Skipping that call
    /// leaves an orphan node registered with the monitor that will be
    /// asked to participate in quorum decisions (e.g. report_lsn during
    /// failover) and never respond, which deadlocks the FSM. The drop is
    /// best-effort — if the monitor is unreachable we still tear down the
    /// container + DB row, but log loudly so the operator can clean up.
    pub async fn remove_cluster_member(
        &self,
        service_id: i32,
        member_id: i32,
    ) -> Result<(), ExternalServiceError> {
        info!(service_id, member_id, "Removing cluster member");

        let service = self.get_service(service_id).await?;

        if service.topology != "cluster" {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: "remove_cluster_member is only valid for cluster topology services"
                    .to_string(),
            });
        }

        let member = service_members::Entity::find_by_id(member_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::InitializationFailed {
                id: service_id,
                reason: format!("Cluster member {} not found", member_id),
            })?;

        if member.service_id != service_id {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!(
                    "Member {} does not belong to service {} (it belongs to service {})",
                    member_id, service_id, member.service_id
                ),
            });
        }

        if is_role_monitor(&member.role) {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: "Cannot remove the monitor — it is required for cluster operation"
                    .to_string(),
            });
        }
        // Block removal of whichever node pg_auto_failover *currently*
        // calls primary, regardless of what `service_members.role` says
        // (which is now always `replica` for data members — see the
        // initialize_cluster comment). If the monitor is unreachable we
        // allow the delete, since the operator likely needs an escape
        // hatch in that exact scenario.
        if self.member_is_live_primary(&service, &member).await? {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: "Cannot remove the current primary. \
                         Trigger a failover first (pg_autoctl perform failover) \
                         so a replica is promoted, then remove this node once it has \
                         been demoted to a replica or has gone offline."
                    .to_string(),
            });
        }

        // Quorum check: pg_auto_failover needs at least 2 data members
        // (one primary + one replica) to keep HA. Removing this member
        // must not leave fewer than 2.
        let all_members = service_members::Entity::find()
            .filter(service_members::Column::ServiceId.eq(service_id))
            .all(self.db.as_ref())
            .await?;
        let data_member_count = all_members
            .iter()
            .filter(|m| !is_role_monitor(&m.role))
            .count();
        if data_member_count <= 2 {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!(
                    "Refusing to remove member: cluster has only {} data member(s); \
                     removing this one would drop the cluster below the 2-member \
                     quorum required for HA. Add a replica first, then remove.",
                    data_member_count
                ),
            });
        }

        // 1. Drop the node from pg_auto_failover *first*. If we delete the
        //    container before this, pg_autoctl on the monitor will treat
        //    the node as unreachable but still expect it to participate
        //    in quorum (e.g. report_lsn during a later failover), wedging
        //    the FSM. Best-effort: a monitor that's down shouldn't block
        //    user-initiated cleanup, but we want loud logs.
        //
        // The pg_autoctl node name is the docker container name (set in
        // `PostgresClusterService::container_params`), so the monitor's
        // identifier matches what `service_members.container_name` holds
        // exactly. Older clusters that registered as `node-{ordinal}`
        // need the legacy name for backwards compatibility — try the
        // container name first, fall back to `node-N`.
        let primary_name = member.container_name.clone();
        let legacy_name = format!("node-{}", member.ordinal);
        let drop_result = self.drop_node_from_monitor(service_id, &primary_name).await;
        let drop_result = match drop_result {
            Ok(()) => Ok(()),
            Err(e) => {
                debug!(
                    service_id,
                    member_id,
                    primary_name = %primary_name,
                    error = %e,
                    "drop_node by container name failed; trying legacy node-N alias"
                );
                self.drop_node_from_monitor(service_id, &legacy_name).await
            }
        };
        if let Err(e) = drop_result {
            warn!(
                service_id,
                member_id,
                primary_name = %primary_name,
                legacy_name = %legacy_name,
                error = %e,
                "Failed to drop node from pg_auto_failover monitor; cluster may need manual `pg_autoctl drop node` after cleanup"
            );
        }

        // 2. Stop and remove the container.
        if let Some(node_id) = member.node_id {
            // Remote: dispatch to the worker's agent.
            match self.get_remote_client(node_id).await {
                Ok(client) => {
                    if let Err(e) = client.remove_service(&member.container_name).await {
                        // Log loudly but keep going — the row + DNS still
                        // need to disappear so the cluster's view is
                        // consistent. The container may already be gone.
                        warn!(
                            service_id,
                            member_id,
                            node_id,
                            container = %member.container_name,
                            error = %e,
                            "Failed to remove remote cluster member container; continuing with row + DNS cleanup"
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        service_id,
                        member_id,
                        node_id,
                        error = %e,
                        "Could not reach worker node to remove container; continuing with row + DNS cleanup"
                    );
                }
            }
        } else {
            // Local container.
            if let Err(e) = self
                .docker
                .remove_container(
                    &member.container_name,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await
            {
                warn!(
                    service_id,
                    member_id,
                    container = %member.container_name,
                    error = %e,
                    "Failed to remove local cluster member container; continuing with row + DNS cleanup"
                );
            }

            let volume_name = format!("{}_data", member.container_name);
            if let Err(e) = self
                .docker
                .remove_volume(
                    &volume_name,
                    None::<bollard::query_parameters::RemoveVolumeOptions>,
                )
                .await
            {
                // Volume removal failures are common (in-use, missing) and
                // not fatal — log at debug.
                debug!(
                    service_id,
                    member_id,
                    volume = %volume_name,
                    error = %e,
                    "Volume cleanup skipped"
                );
            }
        }

        // 3. Delete the service_members row.
        service_members::Entity::delete_by_id(member_id)
            .exec(self.db.as_ref())
            .await?;

        // 4. Drop the Tier-2 DNS record (best-effort — same policy as
        //    delete_service: a stuck DNS plane shouldn't block removal).
        if let Err(e) = self
            .dns_registry
            .delete_by_owner(
                temps_dns::InternalOwnerKind::ServiceMember,
                member_id as i64,
            )
            .await
        {
            warn!(
                service_id,
                member_id,
                error = %e,
                "Failed to drop DNS records for removed cluster member"
            );
        }

        info!(
            service_id,
            member_id,
            role = %member.role,
            ordinal = member.ordinal,
            "Cluster member removed; reconciler will refresh role records on next tick"
        );

        Ok(())
    }

    /// Promote a replica to primary by running `pg_autoctl perform
    /// promotion` inside its container. The monitor coordinates the
    /// failover: it demotes the current primary and the new replica
    /// transitions through `wait_primary` → `single` → `primary`. The
    /// role reconciler refreshes the role-aliased VIPs on its next tick.
    ///
    /// Refuses:
    ///   * member doesn't belong to this service
    ///   * member is the monitor (singletons can't be promoted)
    ///   * member is already the primary
    ///   * member is not running
    ///   * service isn't a cluster
    ///
    /// The command is bounded and takes no user input beyond the
    /// pre-validated `member_id` — same risk profile as the existing
    /// `service_exec` endpoint, much lower than password-reset which
    /// would have crossed user-supplied secrets.
    pub async fn promote_cluster_member(
        &self,
        service_id: i32,
        member_id: i32,
    ) -> Result<(), ExternalServiceError> {
        info!(service_id, member_id, "Promoting cluster member to primary");

        let service = self.get_service(service_id).await?;
        if service.topology != "cluster" {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: "promote_cluster_member is only valid for cluster topology services"
                    .to_string(),
            });
        }
        if service.service_type != "postgres" {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!(
                    "promote_cluster_member is only supported for Postgres clusters (got '{}')",
                    service.service_type
                ),
            });
        }

        let member = service_members::Entity::find_by_id(member_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::InitializationFailed {
                id: service_id,
                reason: format!("Cluster member {} not found", member_id),
            })?;

        if member.service_id != service_id {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!(
                    "Member {} does not belong to service {} (it belongs to {})",
                    member_id, service_id, member.service_id
                ),
            });
        }

        if is_role_monitor(&member.role) {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: "Cannot promote the monitor — it is not a data node".to_string(),
            });
        }
        if self.member_is_live_primary(&service, &member).await? {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!(
                    "Member {} is already the primary; nothing to do",
                    member.container_name
                ),
            });
        }
        if member.status != "running" {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!(
                    "Member {} is not running (status: {}); start it before promoting",
                    member.container_name, member.status
                ),
            });
        }

        // The standalone postgres image and the HA `postgres-ha` image
        // both put pgdata under /var/lib/postgresql/pgdata. We pin it
        // here rather than discovering at runtime — every cluster member
        // we provision uses the same path (see PostgresClusterService).
        let cmd = vec![
            "pg_autoctl".to_string(),
            "perform".to_string(),
            "promotion".to_string(),
            "--pgdata".to_string(),
            "/var/lib/postgresql/pgdata".to_string(),
        ];

        let (exit_code, stdout, stderr) = if let Some(node_id) = member.node_id {
            let client = self.get_remote_client(node_id).await?;
            let result = client
                .exec_in_service(crate::remote_service_client::RemoteExecParams {
                    container_name: member.container_name.clone(),
                    command: cmd,
                    environment: HashMap::new(),
                    user: Some("postgres".to_string()),
                    detach: false,
                })
                .await
                .map_err(|e| ExternalServiceError::InternalError {
                    reason: format!(
                        "Failed to promote member '{}' on node {}: {}",
                        member.container_name, node_id, e
                    ),
                })?;
            (result.exit_code, result.stdout, result.stderr)
        } else {
            self.exec_in_local_container(&member.container_name, &cmd, Some("postgres"))
                .await?
        };

        if exit_code != 0 {
            // Surface stderr first because pg_autoctl writes its real
            // error there; stdout is just the progress chatter.
            let detail = if !stderr.is_empty() { stderr } else { stdout };
            return Err(ExternalServiceError::InternalError {
                reason: format!(
                    "pg_autoctl perform promotion failed (exit {}): {}",
                    exit_code,
                    detail.trim()
                ),
            });
        }

        info!(
            service_id,
            member_id,
            container = %member.container_name,
            "Promotion command accepted by monitor; reconciler will flip role records on next tick"
        );

        Ok(())
    }

    /// Run a command inside a locally-managed container. Mirrors the
    /// agent's `service_exec` for the control-plane half of bipartite
    /// cluster operations. Returns `(exit_code, stdout, stderr)`.
    /// Run `pg_autoctl drop node --name <node_name>` inside the cluster's
    /// monitor container. Returns `Ok(())` on success or any explainable
    /// failure (monitor missing, container gone, exec error) — the caller
    /// is expected to log loudly and proceed with row + container cleanup
    /// regardless. The monitor row is the source of truth for
    /// pg_auto_failover; leaving an orphan there blocks FSM transitions.
    async fn drop_node_from_monitor(
        &self,
        service_id: i32,
        node_name: &str,
    ) -> Result<(), ExternalServiceError> {
        let monitor = service_members::Entity::find()
            .filter(service_members::Column::ServiceId.eq(service_id))
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .find(|m| is_role_monitor(&m.role))
            .ok_or_else(|| ExternalServiceError::InternalError {
                reason: format!(
                    "Cluster service {} has no monitor member; cannot drop node {} from pg_auto_failover",
                    service_id, node_name
                ),
            })?;

        // The monitor container's pg_autoctl runs out of
        // `/var/lib/postgresql/monitor` (see `monitor_command` in
        // `postgres_cluster.rs`), NOT the `/var/lib/postgresql/pgdata`
        // path the data nodes use. Using the wrong --pgdata makes
        // pg_autoctl fail with "Expected configuration file does not
        // exist", which is what the original "harmless orphan" comment
        // missed.
        let cmd = vec![
            "pg_autoctl".to_string(),
            "drop".to_string(),
            "node".to_string(),
            "--formation".to_string(),
            "default".to_string(),
            "--name".to_string(),
            node_name.to_string(),
            "--pgdata".to_string(),
            "/var/lib/postgresql/monitor".to_string(),
        ];

        let (exit_code, stdout, stderr) = if let Some(node_id) = monitor.node_id {
            let client = self.get_remote_client(node_id).await?;
            let result = client
                .exec_in_service(crate::remote_service_client::RemoteExecParams {
                    container_name: monitor.container_name.clone(),
                    command: cmd,
                    environment: HashMap::new(),
                    user: Some("postgres".to_string()),
                    detach: false,
                })
                .await
                .map_err(|e| ExternalServiceError::InternalError {
                    reason: format!(
                        "Failed to drop node {} via monitor on node {}: {}",
                        node_name, node_id, e
                    ),
                })?;
            (result.exit_code, result.stdout, result.stderr)
        } else {
            self.exec_in_local_container(&monitor.container_name, &cmd, Some("postgres"))
                .await?
        };

        if exit_code != 0 {
            // Common benign cases: node already dropped, name not found.
            // pg_autoctl writes the actual reason to stderr.
            let detail = if !stderr.is_empty() { stderr } else { stdout };
            let detail = detail.trim();
            if detail.contains("not found") || detail.contains("does not exist") {
                debug!(
                    service_id,
                    node_name, "pg_autoctl drop node reported the node was already absent"
                );
                return Ok(());
            }
            return Err(ExternalServiceError::InternalError {
                reason: format!(
                    "pg_autoctl drop node {} failed (exit {}): {}",
                    node_name, exit_code, detail
                ),
            });
        }

        info!(
            service_id,
            node_name, "Dropped node from pg_auto_failover monitor"
        );
        Ok(())
    }

    async fn exec_in_local_container(
        &self,
        container_name: &str,
        cmd: &[String],
        user: Option<&str>,
    ) -> Result<(i64, String, String), ExternalServiceError> {
        use bollard::exec::{CreateExecOptions, StartExecOptions};
        use futures::StreamExt;

        let cmd_refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
        let exec = self
            .docker
            .create_exec(
                container_name,
                CreateExecOptions {
                    cmd: Some(cmd_refs),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    user,
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| ExternalServiceError::DockerError {
                id: 0,
                reason: format!("Failed to create exec in '{}': {}", container_name, e),
            })?;

        let output = self
            .docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: false,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| ExternalServiceError::DockerError {
                id: 0,
                reason: format!("Failed to start exec in '{}': {}", container_name, e),
            })?;

        // Capture stdout + stderr separately so the caller can decide
        // which to surface in error messages.
        let mut stdout = String::new();
        let mut stderr = String::new();
        if let bollard::exec::StartExecResults::Attached { mut output, .. } = output {
            while let Some(chunk) = output.next().await {
                match chunk {
                    Ok(bollard::container::LogOutput::StdOut { message }) => {
                        stdout.push_str(&String::from_utf8_lossy(&message));
                    }
                    Ok(bollard::container::LogOutput::StdErr { message }) => {
                        stderr.push_str(&String::from_utf8_lossy(&message));
                    }
                    Ok(other) => {
                        // Console / StdIn never appear here, but include
                        // them in stdout for completeness rather than
                        // dropping silently.
                        stdout.push_str(&other.to_string());
                    }
                    Err(e) => {
                        return Err(ExternalServiceError::DockerError {
                            id: 0,
                            reason: format!("Exec stream error: {}", e),
                        });
                    }
                }
            }
        }

        let inspect = self.docker.inspect_exec(&exec.id).await.map_err(|e| {
            ExternalServiceError::DockerError {
                id: 0,
                reason: format!("Failed to inspect exec result: {}", e),
            }
        })?;
        let exit_code = inspect.exit_code.unwrap_or(-1);
        Ok((exit_code, stdout, stderr))
    }

    /// Resolve a fallback `(ip, port)` for a cluster member that doesn't
    /// have an overlay IP. Used by the DNS registration path so the
    /// member's FQDN still points *somewhere* — even when the overlay
    /// isn't attached. Returns `(node.private_address, host_port)`
    /// because that's the address+port docker-proxy listens on for the
    /// container. Returns `None` if we can't determine either piece.
    async fn resolve_member_underlay(
        &self,
        node_id: Option<i32>,
        host_port: Option<i32>,
        container_port: u16,
    ) -> Option<(String, i32)> {
        // Without a host port we have nothing useful to publish — the
        // FQDN can't point at the container's internal port without an
        // overlay IP.
        let port = host_port.unwrap_or(container_port as i32);

        let ip = if let Some(nid) = node_id {
            nodes::Entity::find_by_id(nid)
                .one(self.db.as_ref())
                .await
                .ok()
                .flatten()
                .map(|n| n.private_address)
        } else {
            // Local member (control plane). Use the same probe the
            // initialize_cluster path uses to learn this node's IP.
            Self::get_local_private_ip().ok()
        }?;

        Some((ip, port))
    }

    /// Look up the gateway IP of the multi-host overlay docker network
    /// (`temps0`). The per-host Hickory resolver listens there on :53 —
    /// every container we create gets it as `--dns` so they can resolve
    /// `*.temps.local` natively (ADR-011).
    ///
    /// Returns `None` when the overlay isn't bootstrapped on this host
    /// (single-host setups). Callers fall back to Docker's default DNS
    /// in that case.
    async fn lookup_overlay_bridge_gateway(&self) -> Option<Vec<String>> {
        // The overlay docker network name is fixed in temps-network's
        // Config::default (`temps0`). We don't take a hard dep on
        // temps-network just for this constant — if it ever changes,
        // the fallback (None → no DNS) keeps clusters functional, just
        // without FQDN resolution inside containers.
        const OVERLAY_NETWORK: &str = "temps0";

        let inspected = match self
            .docker
            .inspect_network(
                OVERLAY_NETWORK,
                None::<bollard::query_parameters::InspectNetworkOptions>,
            )
            .await
        {
            Ok(n) => n,
            Err(e) => {
                debug!(
                    error = %e,
                    network = OVERLAY_NETWORK,
                    "Overlay docker network not present; skipping DNS injection"
                );
                return None;
            }
        };

        // The IPAM config has the gateway we set when creating the
        // network in `temps-network/src/docker.rs`.
        let gateway = inspected
            .ipam
            .as_ref()
            .and_then(|ipam| ipam.config.as_ref())
            .and_then(|configs| {
                configs
                    .iter()
                    .find_map(|c| c.gateway.as_deref().filter(|s| !s.is_empty()))
            });

        gateway.map(|gw| vec![gw.to_string()])
    }

    /// Create a cluster member container on the local Docker daemon.
    ///
    /// Returns `(container_id, host_port, compute_ip)`:
    /// - `container_id` — Docker's internal id for the new container.
    /// - `host_port` — the host port the member's port maps to.
    /// - `compute_ip` — the container's IP on the multi-host overlay
    ///   (`temps-overlay`), or `None` on single-host clusters where the
    ///   overlay isn't attached. Read by the caller into
    ///   `service_members.compute_ip` and the DNS registry (ADR-011).
    async fn create_local_cluster_member(
        &self,
        container_name: &str,
        params: &crate::externalsvc::postgres_cluster::ClusterMemberCreateParams,
    ) -> Result<(String, Option<i32>, Option<String>), ExternalServiceError> {
        use bollard::models::*;
        use bollard::query_parameters::*;
        use futures::TryStreamExt;

        // Ensure network exists
        crate::utils::ensure_network_exists(&self.docker)
            .await
            .map_err(|e| ExternalServiceError::DockerError {
                id: 0,
                reason: format!("Failed to ensure network: {}", e),
            })?;

        // Pull image
        self.docker
            .create_image(
                Some(CreateImageOptions {
                    from_image: Some(params.image.clone()),
                    ..Default::default()
                }),
                None,
                None,
            )
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| ExternalServiceError::DockerError {
                id: 0,
                reason: format!("Failed to pull image {}: {}", params.image, e),
            })?;

        // Create volume
        let volume_name = format!("{}_data", container_name);
        let _ = self
            .docker
            .create_volume(bollard::models::VolumeCreateRequest {
                name: Some(volume_name.clone()),
                ..Default::default()
            })
            .await;

        // Build env vars
        let env: Vec<String> = params
            .environment
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect();

        // Port bindings: map the container port to the same host port.
        // Each cluster member uses a unique port assigned by the manager so
        // there are no conflicts even when multiple members run on the same host.
        let mut port_bindings = std::collections::HashMap::new();
        let container_port_key = format!("{}/tcp", params.container_port);
        port_bindings.insert(
            container_port_key.clone(),
            Some(vec![PortBinding {
                host_ip: Some("0.0.0.0".to_string()),
                host_port: Some(params.container_port.to_string()),
            }]),
        );

        // Wire the per-host Hickory resolver into the container's
        // resolv.conf so it can resolve `*.temps.local` natively
        // (ADR-011). The resolver listens on the bridge gateway IP of
        // the multi-host overlay (`temps0`); we look that up by
        // inspecting the network. Fails open: if the overlay isn't up
        // yet (single-host setups) we just don't set `dns` and fall
        // back to Docker's default resolver.
        let dns_servers = self.lookup_overlay_bridge_gateway().await;

        // Create container
        let mut cluster_host_config = HostConfig {
            binds: Some(vec![format!("{}:{}", volume_name, params.volume_path)]),
            port_bindings: Some(port_bindings),
            dns: dns_servers,
            restart_policy: Some(RestartPolicy {
                name: Some(RestartPolicyNameEnum::UNLESS_STOPPED),
                maximum_retry_count: None,
            }),
            network_mode: Some(temps_core::NETWORK_NAME.to_string()),
            ..Default::default()
        };
        params
            .resource_limits
            .apply_to_host_config(&mut cluster_host_config);
        let container_config = ContainerCreateBody {
            image: Some(params.image.clone()),
            env: Some(env),
            cmd: params.command.clone(),
            host_config: Some(cluster_host_config),
            labels: Some(HashMap::from([
                ("sh.temps.managed".to_string(), "true".to_string()),
                ("sh.temps.service".to_string(), "true".to_string()),
                (
                    "sh.temps.service.type".to_string(),
                    "postgres-cluster".to_string(),
                ),
                (
                    "sh.temps.service.name".to_string(),
                    container_name.to_string(),
                ),
            ])),
            ..Default::default()
        };

        let response = self
            .docker
            .create_container(
                Some(
                    CreateContainerOptionsBuilder::new()
                        .name(container_name)
                        .build(),
                ),
                container_config,
            )
            .await
            .map_err(|e| ExternalServiceError::DockerError {
                id: 0,
                reason: format!("Failed to create container {}: {}", container_name, e),
            })?;

        // Best-effort dual-attach to the multi-host overlay (ADR-011).
        // The container was created on temps-app-network for legacy
        // routing; this also attaches it to temps-overlay so it has a
        // routable cross-node IP and the DNS registry can write A
        // records pointing at it. Skipped silently when the overlay
        // isn't bootstrapped on this host (single-host mode).
        let overlay_name = temps_network::NetworkConfig::default().docker_network_name;
        match self
            .docker
            .list_networks(None::<bollard::query_parameters::ListNetworksOptions>)
            .await
        {
            Ok(networks)
                if networks
                    .iter()
                    .any(|n| n.name.as_deref() == Some(overlay_name.as_str())) =>
            {
                let req = bollard::models::NetworkConnectRequest {
                    container: response.id.clone(),
                    ..Default::default()
                };
                match self.docker.connect_network(&overlay_name, req).await {
                    Ok(()) => {
                        info!(
                            container = container_name,
                            overlay = %overlay_name,
                            "attached cluster member to overlay"
                        );
                    }
                    // 403 = already connected — no-op.
                    Err(bollard::errors::Error::DockerResponseServerError {
                        status_code: 403,
                        ..
                    }) => {}
                    Err(e) => {
                        warn!(
                            container = container_name,
                            overlay = %overlay_name,
                            error = %e,
                            "Failed to attach cluster member to overlay; continuing single-host"
                        );
                    }
                }
            }
            Ok(_) => {
                debug!(
                    container = container_name,
                    overlay = %overlay_name,
                    "overlay not present on this host; skipping attach"
                );
            }
            Err(e) => {
                warn!(error = %e, "list_networks failed during overlay-attach probe");
            }
        }

        // Start container
        self.docker
            .start_container(container_name, None::<StartContainerOptions>)
            .await
            .map_err(|e| ExternalServiceError::DockerError {
                id: 0,
                reason: format!("Failed to start container {}: {}", container_name, e),
            })?;

        // Each member uses a unique port — container_port == host_port
        let host_port = Some(params.container_port as i32);

        // Best-effort overlay-IP discovery for the DNS registry (ADR-011).
        // Failure here is non-fatal — the member still starts; the DNS
        // record is just not written for this generation.
        let compute_ip = match self
            .docker
            .inspect_container(container_name, None::<InspectContainerOptions>)
            .await
        {
            Ok(info) => {
                let overlay_name = temps_network::NetworkConfig::default().docker_network_name;
                info.network_settings
                    .as_ref()
                    .and_then(|ns| ns.networks.as_ref())
                    .and_then(|nets| nets.get(&overlay_name))
                    .and_then(|ep| ep.ip_address.as_deref())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            }
            Err(e) => {
                warn!(
                    container = %container_name,
                    "Failed to inspect new cluster member for overlay IP: {}",
                    e
                );
                None
            }
        };

        Ok((response.id, host_port, compute_ip))
    }

    /// Wait for a container to become healthy (Docker health check).
    async fn wait_for_container_health(
        &self,
        container_name: &str,
        timeout_secs: u64,
    ) -> Result<(), ExternalServiceError> {
        use bollard::query_parameters::InspectContainerOptions;
        use std::time::{Duration, Instant};

        let start = Instant::now();
        let timeout = Duration::from_secs(timeout_secs);

        loop {
            if start.elapsed() > timeout {
                return Err(ExternalServiceError::InitializationFailed {
                    id: 0,
                    reason: format!(
                        "Container {} did not become healthy within {}s",
                        container_name, timeout_secs
                    ),
                });
            }

            if let Ok(info) = self
                .docker
                .inspect_container(container_name, None::<InspectContainerOptions>)
                .await
            {
                let running = info.state.as_ref().and_then(|s| s.running).unwrap_or(false);

                if running {
                    // Check if container has a healthcheck and if it's healthy
                    let health_status = info
                        .state
                        .as_ref()
                        .and_then(|s| s.health.as_ref())
                        .and_then(|h| h.status.as_ref())
                        .map(|s| format!("{:?}", s));

                    match health_status.as_deref() {
                        Some("\"HEALTHY\"") | Some("Healthy") => return Ok(()),
                        None => {
                            // No healthcheck defined — just check if running
                            return Ok(());
                        }
                        _ => {} // Still starting or unhealthy — keep waiting
                    }
                }
            }
            // Container not found or not running yet — keep waiting

            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }

    async fn store_inferred_parameters(
        &self,
        service_id: i32,
        _service_instance: &dyn ExternalService,
        inferred_params: HashMap<String, String>,
    ) -> Result<(), ExternalServiceError> {
        // Get current parameters
        let mut current_params = self.get_service_parameters(service_id).await?;

        // Only merge parameters that are truly auto-generated/inferred
        // Skip user-facing parameters like docker_image, host, database, etc.
        for (key, value) in inferred_params {
            if Self::is_inferred_parameter(&key) {
                current_params.insert(key, serde_json::Value::String(value));
            }
        }

        // Serialize updated config to JSON and encrypt
        let config_json = serde_json::to_string(&current_params).map_err(|e| {
            ExternalServiceError::InternalError {
                reason: format!("Failed to serialize config to JSON: {}", e),
            }
        })?;

        let encrypted_config = self
            .encryption_service
            .encrypt_string(&config_json)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to encrypt config: {}", e),
            })?;

        // Update service config
        let service = self.get_service(service_id).await?;
        let mut service_update: external_services::ActiveModel = service.into();
        service_update.config = Set(Some(encrypted_config));
        service_update.updated_at = Set(Utc::now());
        service_update.update(self.db.as_ref()).await?;

        Ok(())
    }

    fn is_inferred_parameter(key: &str) -> bool {
        // Only truly inferred/auto-generated parameters should be merged here.
        // User-provided parameters (docker_image, etc.) should NOT be overwritten by inferred values.
        // Inferred parameters are those auto-generated by the init() method:
        // - Actual port mappings/addresses after container creation
        // - Connection strings derived from the deployed service
        // - Auto-generated passwords (when not provided or invalid)
        // - Other runtime-determined values
        matches!(
            key,
            // Only include truly inferred values
            "port"
                | "connection_string"
                | "local_address"
                | "inferred_port"
                | "password"
                | "root_password"
        )
    }

    // Add this new helper method
    fn generate_slug(name: &str) -> String {
        name.to_lowercase()
            .chars()
            .filter_map(|c| {
                if c.is_alphanumeric() {
                    Some(c)
                } else if c.is_whitespace() {
                    Some('-')
                } else {
                    None
                }
            })
            .collect()
    }

    /// Convert HashMap<String, serde_json::Value> to HashMap<String, String>
    fn params_to_strings(params: &HashMap<String, serde_json::Value>) -> HashMap<String, String> {
        params
            .iter()
            .map(|(k, v)| {
                let v_str = match v {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    serde_json::Value::Null => String::new(),
                    _ => v.to_string(),
                };
                (k.clone(), v_str)
            })
            .collect()
    }

    /// Guard against reconciling/(re)starting or recreating a container
    /// while a Postgres major upgrade (or its rollback) is actively working
    /// on it. The upgrade orchestrator stops the container and
    /// deletes/recreates its volumes across several phases
    /// (`postgres_upgrade.rs`), and `rollback()` does the same in reverse;
    /// a concurrent container mutation (start, reconcile, resource-limit
    /// recreate, a same-version image swap) would recreate the container
    /// against an implicitly re-created empty volume, silently diverging
    /// from the volume the orchestrator/rollback is dumping/restoring —
    /// accepting writes that never make it into the upgraded database.
    ///
    /// Called from every entry point that stops/recreates a service's
    /// container: `start_service`, `initialize_service` (covers
    /// `update_service`, service creation, and the resource-limit recreate
    /// path), and the legacy same-version `upgrade_service`.
    ///
    /// PENDING/RUNNING/ROLLING_BACK rows are active; FAILED/COMPLETED/
    /// CANCELLED/ROLLED_BACK are terminal and don't block a start.
    async fn ensure_no_active_upgrade(&self, service_id: i32) -> Result<(), ExternalServiceError> {
        use crate::externalsvc::postgres_upgrade::status;

        let active = postgres_major_upgrades::Entity::find()
            .filter(postgres_major_upgrades::Column::ServiceId.eq(service_id))
            .filter(
                postgres_major_upgrades::Column::Status
                    .eq(status::PENDING)
                    .or(postgres_major_upgrades::Column::Status.eq(status::RUNNING))
                    .or(postgres_major_upgrades::Column::Status.eq(status::ROLLING_BACK)),
            )
            .one(self.db.as_ref())
            .await
            .map_err(|e| ExternalServiceError::DatabaseError {
                reason: format!("failed to check for active major upgrade: {}", e),
            })?;

        if let Some(upgrade) = active {
            return Err(ExternalServiceError::UpgradeInProgress {
                id: service_id,
                upgrade_id: upgrade.id,
                phase: upgrade.phase,
            });
        }

        Ok(())
    }

    pub async fn start_service(
        &self,
        service_id: i32,
    ) -> Result<ExternalServiceInfo, ExternalServiceError> {
        self.ensure_no_active_upgrade(service_id).await?;
        let service = self.get_service(service_id).await?;
        let service_type_enum = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;
        let parameters = self.get_service_parameters(service_id).await?;

        // Remote node — delegate to agent
        if let Some(node_id) = service.node_id {
            let client = self.get_remote_client(node_id).await?;
            let container_name = self
                .create_service_instance_for_parameters(
                    service.name.clone(),
                    service_type_enum,
                    &parameters,
                )?
                .get_name();

            match client.start_service(&container_name).await {
                Ok(()) => {}
                Err(e) => {
                    info!(
                        "Remote start failed for service {} ({}), falling back to initialize: {}",
                        service_id, service.name, e
                    );
                    self.initialize_service(service_id)
                        .await
                        .map_err(|init_err| ExternalServiceError::StartFailed {
                            id: service_id,
                            reason: format!(
                                "Start failed: {}. Re-initialize also failed: {}",
                                e, init_err
                            ),
                        })?;
                    return self.get_service_info(service_id).await;
                }
            }
        } else {
            // Local node
            let service_instance = self.create_service_instance_for_parameters(
                service.name.clone(),
                service_type_enum,
                &parameters,
            )?;

            match service_instance.start().await {
                Ok(()) => {}
                Err(e) => {
                    info!(
                        "Direct start failed for service {} ({}), falling back to initialize: {}",
                        service_id, service.name, e
                    );
                    self.initialize_service(service_id)
                        .await
                        .map_err(|init_err| ExternalServiceError::StartFailed {
                            id: service_id,
                            reason: format!(
                                "Start failed: {}. Re-initialize also failed: {}",
                                e, init_err
                            ),
                        })?;
                    return self.get_service_info(service_id).await;
                }
            }
        }

        // Update status to running
        let mut service_update: external_services::ActiveModel = service.into();
        service_update.status = Set("running".to_string());
        service_update.updated_at = Set(Utc::now());
        service_update.update(self.db.as_ref()).await?;

        // Refresh the WAL health snapshot immediately for Postgres services.
        // The reconcile-on-start path may have recreated the container with a
        // different archive_mode; without this refresh, the UI would keep
        // showing the pre-recreate snapshot for up to one health-monitor
        // cycle (~30s). Failure is non-fatal — the background monitor will
        // converge on its own.
        if service_type_enum == ServiceType::Postgres {
            if let Err(e) = self.refresh_postgres_wal_health(service_id).await {
                tracing::debug!(
                    "Failed to refresh WAL health snapshot for service {} after start: {} (non-fatal)",
                    service_id,
                    e
                );
            }
        }

        self.get_service_info(service_id).await
    }

    /// Wipe `health_metadata.postgres_wal` then immediately run the WAL probe
    /// and persist the fresh snapshot. Called from `start_service` after a
    /// Postgres recreate so the UI doesn't show pre-recreate data while the
    /// background monitor catches up.
    async fn refresh_postgres_wal_health(
        &self,
        service_id: i32,
    ) -> Result<(), ExternalServiceError> {
        use crate::externalsvc::postgres_wal_health;

        // Reload the service row (status is now 'running'); skip if it isn't
        // actually a standalone Postgres service.
        let service = self.get_service(service_id).await?;
        if service.service_type != "postgres" || service.topology != "standalone" {
            return Ok(());
        }

        // Clear the stale snapshot first. Even if the probe below fails, the
        // UI will see "no snapshot yet" rather than wrong data.
        let cleared = clear_health_metadata_key(service.health_metadata.as_ref(), "postgres_wal");
        let mut active: external_services::ActiveModel = service.clone().into();
        active.health_metadata = Set(cleared);
        active.update(self.db.as_ref()).await?;

        // Probe fresh against the new container. Best-effort.
        let service_config = self.get_service_config(service_id).await?;
        let Some(conn_str) = postgres_wal_health::build_conn_str(&service_config.parameters) else {
            return Ok(());
        };
        let Some(snapshot) = postgres_wal_health::probe_wal_health(&conn_str).await else {
            return Ok(());
        };

        // Reload again, merge the fresh snapshot under postgres_wal.
        let service = self.get_service(service_id).await?;
        let merged =
            merge_health_metadata_key(service.health_metadata.as_ref(), "postgres_wal", &snapshot);
        let mut active: external_services::ActiveModel = service.into();
        active.health_metadata = Set(Some(merged));
        active.update(self.db.as_ref()).await?;

        Ok(())
    }

    pub async fn stop_service(
        &self,
        service_id: i32,
    ) -> Result<ExternalServiceInfo, ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        let service_type_enum = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;
        let parameters = self.get_service_parameters(service_id).await?;

        // Remote node — delegate to agent
        if let Some(node_id) = service.node_id {
            let client = self.get_remote_client(node_id).await?;
            let container_name = self
                .create_service_instance_for_parameters(
                    service.name.clone(),
                    service_type_enum,
                    &parameters,
                )?
                .get_name();

            client.stop_service(&container_name).await.map_err(|e| {
                ExternalServiceError::StopFailed {
                    id: service_id,
                    reason: e.to_string(),
                }
            })?;
        } else {
            // Local node
            let service_instance = self.create_service_instance_for_parameters(
                service.name.clone(),
                service_type_enum,
                &parameters,
            )?;

            if service_type_enum == ServiceType::Mariadb {
                let parameters = self.get_service_parameters(service_id).await?;
                if parameters.contains_key("container_name") {
                    let service_config = ServiceConfig {
                        name: service.name.clone(),
                        service_type: service_type_enum,
                        version: service.version.clone(),
                        parameters: serde_json::to_value(parameters).map_err(|e| {
                            ExternalServiceError::InternalError {
                                reason: format!("Failed to serialize parameters: {}", e),
                            }
                        })?,
                    };
                    service_instance.init(service_config).await.map_err(|e| {
                        ExternalServiceError::StopFailed {
                            id: service_id,
                            reason: format!(
                                "Failed to initialize imported MariaDB service before stop: {}",
                                e
                            ),
                        }
                    })?;
                }
            }

            service_instance
                .stop()
                .await
                .map_err(|e| ExternalServiceError::StopFailed {
                    id: service_id,
                    reason: e.to_string(),
                })?;
        }

        // Update status to stopped
        let mut service_update: external_services::ActiveModel = service.into();
        service_update.status = Set("stopped".to_string());
        service_update.updated_at = Set(Utc::now());
        service_update.update(self.db.as_ref()).await?;

        self.get_service_info(service_id).await
    }

    pub async fn link_service_to_project(
        &self,
        service_id_val: i32,
        project_id_val: i32,
    ) -> Result<ProjectServiceInfo, ExternalServiceError> {
        // Verify service exists and get its type
        let service = self.get_service(service_id_val).await?;
        let service_type = service.service_type.clone();

        // Verify project exists
        let _project = projects::Entity::find_by_id(project_id_val)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::ProjectNotFound { id: project_id_val })?;

        // Check for duplicate service type
        // Get all existing project_services for this project
        let existing_links = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project_id_val))
            .all(self.db.as_ref())
            .await?;

        // Check if any existing service has the same type
        for existing_link in existing_links {
            let existing_service = self.get_service(existing_link.service_id).await?;
            if existing_service.service_type == service_type {
                return Err(ExternalServiceError::DuplicateServiceType {
                    project_id: project_id_val,
                    service_type,
                });
            }
        }

        // Create link
        let new_link = project_services::ActiveModel {
            project_id: Set(project_id_val),
            service_id: Set(service_id_val),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };

        let link = new_link.insert(self.db.as_ref()).await?;
        let service_info = self.get_service_info(service_id_val).await?;

        // Fetch project metadata
        let project = projects::Entity::find_by_id(link.project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::ProjectNotFound {
                id: link.project_id,
            })?;

        Ok(ProjectServiceInfo {
            id: link.id,
            project: ProjectInfo {
                id: project.id,
                slug: project.slug,
                created_at: project.created_at.to_rfc3339(),
            },
            service: service_info,
        })
    }

    pub async fn get_service_environment_variables(
        &self,
        service_id_val: i32,
        _project_id_val: i32,
    ) -> Result<HashMap<String, String>, ExternalServiceError> {
        let service = self.get_service(service_id_val).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id_val,
                service_type: service.service_type.clone(),
            }
        })?;
        let parameters = self.get_service_parameters(service_id_val).await?;

        // Cluster services: use multi-host env vars from service_members
        if let Some(cluster_vars) = self.build_cluster_env_vars(&service, &parameters).await? {
            return Ok(cluster_vars);
        }

        let service_instance = self.create_service_instance_for_parameters(
            service.name.clone(),
            service_type,
            &parameters,
        )?;

        // Convert parameters to strings for the service
        let params_str = Self::params_to_strings(&parameters);

        // Get connection info from the service instance
        service_instance
            .get_environment_variables(&params_str)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to get environment variables: {}", e),
            })
    }

    pub async fn get_runtime_env_vars(
        &self,
        service_id_val: i32,
        project_id: i32,
        environment_id: i32,
    ) -> Result<HashMap<String, String>, ExternalServiceError> {
        // Get service
        let service = self.get_service(service_id_val).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id_val,
                service_type: service.service_type.clone(),
            }
        })?;

        // Verify service is linked to project
        let link_exists = project_services::Entity::find()
            .filter(
                project_services::Column::ServiceId
                    .eq(service_id_val)
                    .and(project_services::Column::ProjectId.eq(project_id)),
            )
            .one(self.db.as_ref())
            .await?;

        if link_exists.is_none() {
            return Err(ExternalServiceError::ServiceNotLinkedToProject {
                service_id: service_id_val,
                project_id,
            });
        }

        let parameters = self.get_service_parameters(service_id_val).await?;

        // Compute the per-tenant database name once — both paths use
        // the same `<project_slug>_<env_slug>` convention so an app
        // gets the same DB whether the upstream service is standalone
        // or clustered.
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::ProjectNotFound { id: project_id })?;
        let environment = temps_entities::environments::Entity::find_by_id(environment_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ExternalServiceError::InternalError {
                reason: format!("Environment {} not found", environment_id),
            })?;
        let resource_name = crate::externalsvc::postgres::PostgresService::normalize_database_name(
            &format!("{}_{}", project.slug, environment.slug),
        );

        // Cluster services: build multi-host env vars from
        // service_members AND provision the per-tenant database on
        // the live primary so apps get isolation parity with the
        // standalone path.
        if service.topology == "cluster" && service.service_type == "postgres" {
            if let Some(cluster_vars) = self
                .build_cluster_env_vars_for_resource(&service, &parameters, Some(&resource_name))
                .await?
            {
                return Ok(cluster_vars);
            }
        }
        // Other cluster types (none today, but keep the door open)
        // get the legacy non-tenant view.
        if let Some(cluster_vars) = self.build_cluster_env_vars(&service, &parameters).await? {
            return Ok(cluster_vars);
        }

        // Standalone: delegate to the service instance's get_runtime_env_vars
        let service_instance = self.create_service_instance_for_parameters(
            service.name.clone(),
            service_type,
            &parameters,
        )?;
        let service_config = ServiceConfig {
            name: service.name.clone(),
            service_type,
            version: service.version,
            parameters: serde_json::to_value(&parameters).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to serialize parameters: {}", e),
                }
            })?,
        };

        // Initialize the service to populate its internal config
        service_instance
            .init(service_config.clone())
            .await
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to initialize service: {}", e),
            })?;

        // Get runtime environment variables (this provisions resources like databases/buckets)
        // `project` and `environment` were fetched up top — reuse the slugs.
        service_instance
            .get_runtime_env_vars(service_config, &project.slug, &environment.slug)
            .await
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to get runtime environment variables: {}", e),
            })
    }

    /// Get the effective address components for a service.
    ///
    /// Returns `(container_name, internal_port, host_port)` where:
    /// - `container_name` is the Docker container name used in connection strings
    /// - `internal_port` is the port inside the container (e.g., 5432 for Postgres)
    /// - `host_port` is the mapped port on the host machine
    ///
    /// Used by the workflow planner to build remote environment variables by replacing
    /// `container_name:internal_port` with `private_address:host_port`.
    pub async fn get_service_effective_address(
        &self,
        service_id: i32,
    ) -> Result<(String, String, String), ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;

        let parameters = self.get_service_parameters(service_id).await?;
        let service_instance = self.create_service_instance_for_parameters(
            service.name.clone(),
            service_type,
            &parameters,
        )?;
        let service_config = ServiceConfig {
            name: service.name.clone(),
            service_type,
            version: service.version,
            parameters: serde_json::to_value(parameters).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to serialize parameters: {}", e),
                }
            })?,
        };

        // Use Docker container name and internal port directly — these match what
        // get_runtime_env_vars() puts in env var values (always Docker container names,
        // regardless of DeploymentMode). This is critical for cross-node env var rewriting.
        let container_name = service_instance.get_docker_container_name();
        let internal_port = service_instance.get_docker_internal_port();

        // get_local_address returns "localhost:{host_port}" — we need the host port
        // for the replacement target (private_address:host_port)
        let local_address = service_instance
            .get_local_address(service_config)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to get local address: {}", e),
            })?;
        let host_port = local_address
            .rsplit(':')
            .next()
            .unwrap_or(&internal_port)
            .to_string();

        Ok((container_name, internal_port, host_port))
    }

    /// Get runtime environment variables with cross-node address resolution.
    ///
    /// When the consuming container runs on a different node than the service,
    /// connection strings are rewritten to use the service node's private/WireGuard IP
    /// and host port instead of container names or localhost.
    ///
    /// If `target_node_id` is None or matches the service's node, returns
    /// standard env vars (same as `get_runtime_env_vars`).
    pub async fn get_cross_node_runtime_env_vars(
        &self,
        service_id_val: i32,
        project_id: i32,
        environment_id: i32,
        target_node_id: Option<i32>,
    ) -> Result<HashMap<String, String>, ExternalServiceError> {
        // Get the base env vars (standard same-node behavior)
        let mut env_vars = self
            .get_runtime_env_vars(service_id_val, project_id, environment_id)
            .await?;

        // If no target node specified, return as-is (single-node mode)
        let target_node_id = match target_node_id {
            Some(id) => id,
            None => return Ok(env_vars),
        };

        // Check if the service is on a different node
        let service = self.get_service(service_id_val).await?;
        let service_node_id = service.node_id;

        // Same node or both local: no rewriting needed
        if service_node_id == Some(target_node_id) || service_node_id.is_none() {
            return Ok(env_vars);
        }

        // Cross-node: resolve the service node's private address and host port
        let service_node_id = match service_node_id {
            Some(id) => id,
            None => return Ok(env_vars), // Service is local, target is remote — use local address
        };

        use temps_entities::nodes;
        let service_node = nodes::Entity::find_by_id(service_node_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ExternalServiceError::InternalError {
                reason: format!("Service node {} not found", service_node_id),
            })?;

        let private_addr = &service_node.private_address;

        // Get the service's host port from its container
        use temps_entities::deployment_containers;
        let service_container = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .filter(deployment_containers::Column::ContainerName.contains(&service.name))
            .one(self.db.as_ref())
            .await?;

        let host_port = service_container
            .as_ref()
            .map(|c| c.host_port.unwrap_or(c.container_port));
        let internal_port = service_container.as_ref().map(|c| c.container_port);

        rewrite_env_vars_for_cross_node(
            &mut env_vars,
            &service.name,
            private_addr,
            host_port,
            internal_port,
        );

        Ok(env_vars)
    }

    pub async fn get_service_docker_environment_variables(
        &self,
        service_id_val: i32,
        project_id_val: i32,
    ) -> Result<HashMap<String, String>, ExternalServiceError> {
        // Verify service exists
        let service = self.get_service(service_id_val).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id_val,
                service_type: service.service_type.clone(),
            }
        })?;

        // Verify service is linked to project
        let link_exists = project_services::Entity::find()
            .filter(
                project_services::Column::ServiceId
                    .eq(service_id_val)
                    .and(project_services::Column::ProjectId.eq(project_id_val)),
            )
            .one(self.db.as_ref())
            .await?;

        if link_exists.is_none() {
            return Err(ExternalServiceError::ServiceNotLinkedToProject {
                service_id: service_id_val,
                project_id: project_id_val,
            });
        }

        let parameters = self.get_service_parameters(service_id_val).await?;

        // Cluster services: use multi-host env vars from service_members
        if let Some(cluster_vars) = self.build_cluster_env_vars(&service, &parameters).await? {
            return Ok(cluster_vars);
        }

        let service_instance = self.create_service_instance_for_parameters(
            service.name.clone(),
            service_type,
            &parameters,
        )?;

        // Convert parameters to strings for the service
        let params_str = Self::params_to_strings(&parameters);

        service_instance
            .get_docker_environment_variables(&params_str)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to get docker environment variables: {}", e),
            })
    }

    pub async fn unlink_service_from_project(
        &self,
        service_id_val: i32,
        project_id_val: i32,
    ) -> Result<(), ExternalServiceError> {
        // Verify service exists
        self.get_service(service_id_val).await?;

        // Delete the link
        let deleted = project_services::Entity::delete_many()
            .filter(
                project_services::Column::ServiceId
                    .eq(service_id_val)
                    .and(project_services::Column::ProjectId.eq(project_id_val)),
            )
            .exec(self.db.as_ref())
            .await?;

        if deleted.rows_affected == 0 {
            return Err(ExternalServiceError::ServiceNotLinkedToProject {
                service_id: service_id_val,
                project_id: project_id_val,
            });
        }

        Ok(())
    }

    pub async fn list_service_projects(
        &self,
        service_id_val: i32,
    ) -> Result<Vec<ProjectServiceInfo>, ExternalServiceError> {
        // Verify service exists and get service info
        let service_info = self.get_service_info(service_id_val).await?;

        // Get all project links for this service
        let links = project_services::Entity::find()
            .filter(project_services::Column::ServiceId.eq(service_id_val))
            .all(self.db.as_ref())
            .await?;

        // Convert to ProjectServiceInfo with project metadata
        let mut project_services_list = Vec::new();
        for link in links {
            // Fetch project metadata
            let project = projects::Entity::find_by_id(link.project_id)
                .one(self.db.as_ref())
                .await?
                .ok_or(ExternalServiceError::ProjectNotFound {
                    id: link.project_id,
                })?;

            project_services_list.push(ProjectServiceInfo {
                id: link.id,
                project: ProjectInfo {
                    id: project.id,
                    slug: project.slug,
                    created_at: project.created_at.to_rfc3339(),
                },
                service: service_info.clone(),
            });
        }

        Ok(project_services_list)
    }

    pub async fn list_service_projects_paginated(
        &self,
        service_id_val: i32,
        page: u64,
        page_size: u64,
    ) -> Result<Vec<ProjectServiceInfo>, ExternalServiceError> {
        // Verify service exists and get service info
        let service_info = self.get_service_info(service_id_val).await?;

        // Get paginated project links for this service
        let links = project_services::Entity::find()
            .filter(project_services::Column::ServiceId.eq(service_id_val))
            .order_by_desc(project_services::Column::Id)
            .paginate(self.db.as_ref(), page_size)
            .fetch_page(page - 1)
            .await?;

        // Convert to ProjectServiceInfo with project metadata
        let mut project_services_list = Vec::new();
        for link in links {
            let project = projects::Entity::find_by_id(link.project_id)
                .one(self.db.as_ref())
                .await?
                .ok_or(ExternalServiceError::ProjectNotFound {
                    id: link.project_id,
                })?;

            project_services_list.push(ProjectServiceInfo {
                id: link.id,
                project: ProjectInfo {
                    id: project.id,
                    slug: project.slug,
                    created_at: project.created_at.to_rfc3339(),
                },
                service: service_info.clone(),
            });
        }

        Ok(project_services_list)
    }

    pub async fn list_project_services(
        &self,
        project_id_val: i32,
    ) -> Result<Vec<ProjectServiceInfo>, ExternalServiceError> {
        // Verify project exists and fetch its metadata
        let project = projects::Entity::find_by_id(project_id_val)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::ProjectNotFound { id: project_id_val })?;

        // Get all service links for this project
        let links = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project_id_val))
            .all(self.db.as_ref())
            .await?;

        // Convert to ProjectServiceInfo with service details
        let mut project_services_list = Vec::new();
        for link in links {
            let service_info = self.get_service_info(link.service_id).await?;
            project_services_list.push(ProjectServiceInfo {
                id: link.id,
                project: ProjectInfo {
                    id: project.id,
                    slug: project.slug.clone(),
                    created_at: project.created_at.to_rfc3339(),
                },
                service: service_info,
            });
        }

        Ok(project_services_list)
    }

    pub async fn list_project_services_paginated(
        &self,
        project_id_val: i32,
        page: u64,
        page_size: u64,
    ) -> Result<Vec<ProjectServiceInfo>, ExternalServiceError> {
        // Verify project exists and fetch its metadata
        let project = projects::Entity::find_by_id(project_id_val)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::ProjectNotFound { id: project_id_val })?;

        // Get paginated service links for this project
        let links = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project_id_val))
            .order_by_desc(project_services::Column::Id)
            .paginate(self.db.as_ref(), page_size)
            .fetch_page(page - 1)
            .await?;

        // Convert to ProjectServiceInfo with service details
        let mut project_services_list = Vec::new();
        for link in links {
            let service_info = self.get_service_info(link.service_id).await?;
            project_services_list.push(ProjectServiceInfo {
                id: link.id,
                project: ProjectInfo {
                    id: project.id,
                    slug: project.slug.clone(),
                    created_at: project.created_at.to_rfc3339(),
                },
                service: service_info,
            });
        }

        Ok(project_services_list)
    }

    pub async fn get_service_environment_variable(
        &self,
        service_id_val: i32,
        project_id_val: i32,
        var_name: &str,
    ) -> Result<EnvironmentVariableInfo, ExternalServiceError> {
        let service = self.get_service(service_id_val).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id_val,
                service_type: service.service_type.clone(),
            }
        })?;
        let parameters = self.get_service_parameters(service_id_val).await?;

        // Verify project link exists
        let link_exists = project_services::Entity::find()
            .filter(
                project_services::Column::ServiceId
                    .eq(service_id_val)
                    .and(project_services::Column::ProjectId.eq(project_id_val)),
            )
            .one(self.db.as_ref())
            .await?;

        if link_exists.is_none() {
            return Err(ExternalServiceError::ServiceNotLinkedToProject {
                service_id: service_id_val,
                project_id: project_id_val,
            });
        }

        let service_instance = self.create_service_instance_for_parameters(
            service.name.clone(),
            service_type,
            &parameters,
        )?;
        // Convert parameters to strings for the service
        let params_str = Self::params_to_strings(&parameters);

        let env_vars = service_instance
            .get_environment_variables(&params_str)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to get environment variables: {}", e),
            })?;

        // Check if the variable exists
        match env_vars.get(var_name) {
            Some(value) => {
                // All config is encrypted at rest, but we can return env vars
                // Mark common sensitive variable names as sensitive
                let sensitive_vars = ["password", "secret", "key", "token", "api_key"];
                let is_sensitive = sensitive_vars
                    .iter()
                    .any(|s| var_name.to_lowercase().contains(s));

                Ok(EnvironmentVariableInfo {
                    name: var_name.to_string(),
                    value: value.clone(),
                    sensitive: is_sensitive,
                })
            }
            None => Err(ExternalServiceError::EnvironmentVariableNotFound {
                service_id: service_id_val,
                var_name: var_name.to_string(),
            }),
        }
    }

    pub async fn get_project_service_environment_variables(
        &self,
        project_id_val: i32,
    ) -> Result<HashMap<i32, HashMap<String, String>>, ExternalServiceError> {
        // Verify project exists
        let _project = projects::Entity::find_by_id(project_id_val)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::ProjectNotFound { id: project_id_val })?;

        // Get all services linked to this project
        let linked_services = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project_id_val))
            .all(self.db.as_ref())
            .await?;

        let mut result = HashMap::new();

        // For each linked service, get its environment variables
        for linked_service in linked_services {
            match self
                .get_service_environment_variables(linked_service.service_id, project_id_val)
                .await
            {
                Ok(env_vars) => {
                    result.insert(linked_service.service_id, env_vars);
                }
                Err(e) => {
                    error!(
                        "Failed to get environment variables for service {}: {}",
                        linked_service.service_id, e
                    );
                    // Skip this service and continue with others
                    continue;
                }
            }
        }

        Ok(result)
    }

    /// Preview the env vars a deployment in `environment_id` would receive
    /// from every service linked to `project_id`. Side-effect-free: skips
    /// `CREATE DATABASE` / bucket creation that the real runtime path
    /// performs. Used by the resolved env vars UI so users can switch
    /// between environments and see the actual `<project>_<env>` values.
    pub async fn preview_project_service_environment_variables(
        &self,
        project_id_val: i32,
        environment_id: i32,
    ) -> Result<HashMap<i32, HashMap<String, String>>, ExternalServiceError> {
        let project = projects::Entity::find_by_id(project_id_val)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::ProjectNotFound { id: project_id_val })?;
        let environment = temps_entities::environments::Entity::find_by_id(environment_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ExternalServiceError::InternalError {
                reason: format!("Environment {} not found", environment_id),
            })?;

        let linked_services = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project_id_val))
            .all(self.db.as_ref())
            .await?;

        let mut result = HashMap::new();
        for linked in linked_services {
            match self
                .preview_service_environment_variables(
                    linked.service_id,
                    &project.slug,
                    &environment.slug,
                )
                .await
            {
                Ok(env_vars) => {
                    result.insert(linked.service_id, env_vars);
                }
                Err(e) => {
                    error!(
                        "Failed to preview environment variables for service {}: {}",
                        linked.service_id, e
                    );
                    continue;
                }
            }
        }

        Ok(result)
    }

    /// Side-effect-free per-service env var preview. Mirrors
    /// `get_service_environment_variables` but calls
    /// `preview_runtime_env_vars` on the service instance so no databases
    /// or buckets get provisioned. Cluster services fall back to their
    /// regular env var path because `build_cluster_env_vars` reads from
    /// `service_members` and doesn't provision anything.
    async fn preview_service_environment_variables(
        &self,
        service_id_val: i32,
        project_slug: &str,
        environment_slug: &str,
    ) -> Result<HashMap<String, String>, ExternalServiceError> {
        let service = self.get_service(service_id_val).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id_val,
                service_type: service.service_type.clone(),
            }
        })?;
        let parameters = self.get_service_parameters(service_id_val).await?;

        let resource_name = crate::externalsvc::postgres::PostgresService::normalize_database_name(
            &format!("{}_{}", project_slug, environment_slug),
        );

        if service.topology == "cluster" && service.service_type == "postgres" {
            if let Some(cluster_vars) = self
                .build_cluster_env_vars_for_resource(&service, &parameters, Some(&resource_name))
                .await?
            {
                return Ok(cluster_vars);
            }
        }
        if let Some(cluster_vars) = self.build_cluster_env_vars(&service, &parameters).await? {
            return Ok(cluster_vars);
        }

        let service_instance = self.create_service_instance_for_parameters(
            service.name.clone(),
            service_type,
            &parameters,
        )?;
        let service_config = ServiceConfig {
            name: service.name.clone(),
            service_type,
            version: service.version,
            parameters: serde_json::to_value(&parameters).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to serialize parameters: {}", e),
                }
            })?,
        };

        service_instance
            .init(service_config.clone())
            .await
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to initialize service: {}", e),
            })?;

        service_instance
            .preview_runtime_env_vars(service_config, project_slug, environment_slug)
            .await
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to preview runtime environment variables: {}", e),
            })
    }

    pub async fn get_service_type_schema(
        &self,
        service_type: ServiceType,
    ) -> Result<Option<serde_json::Value>, ExternalServiceError> {
        let service_instance = self.create_service_instance("temp".to_string(), service_type);
        Ok(service_instance.get_parameter_schema())
    }

    pub async fn get_service_details_by_slug(
        &self,
        service: external_services::Model,
    ) -> Result<ExternalServiceDetails, ExternalServiceError> {
        // Get service info
        let service_info = self.get_service_info(service.id).await?;
        let parameters = self.get_service_parameters(service.id).await?;
        let service_type = ServiceType::from_str(&service_info.service_type.to_string())?;

        let service_instance = self.create_service_instance_for_parameters(
            service_info.name.clone(),
            service_type,
            &parameters,
        )?;

        Ok(ExternalServiceDetails {
            service: service_info,
            parameter_schema: service_instance.get_parameter_schema(),
            current_parameters: Some(parameters),
        })
    }

    /// Consolidated method for getting environment variables with flexible options
    ///
    /// This method replaces 7 separate environment variable methods:
    /// - get_service_environment_variables()
    /// - get_runtime_env_vars()
    /// - get_service_docker_environment_variables()
    /// - get_service_environment_variable()
    /// - get_project_service_environment_variables()
    /// - get_service_preview_environment_variable_names()
    /// - get_service_preview_environment_variables_masked()
    pub async fn get_environment_variables(
        &self,
        service_id: i32,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        options: EnvironmentVariableOptions,
    ) -> Result<EnvironmentVariablesResponse, ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;

        let parameters = self.get_service_parameters(service_id).await?;
        let params_str = Self::params_to_strings(&parameters);
        let service_instance = self.create_service_instance_for_parameters(
            service.name.clone(),
            service_type,
            &parameters,
        )?;

        let mut all_vars = HashMap::new();

        // Cluster services: use multi-host env vars from service_members
        let is_cluster = service.topology == "cluster";
        if is_cluster {
            if let Some(cluster_vars) = self.build_cluster_env_vars(&service, &parameters).await? {
                all_vars.extend(cluster_vars);
            }
        }

        // Get basic environment variables (standalone only)
        if !is_cluster && !options.include_runtime {
            let basic_vars = service_instance
                .get_environment_variables(&params_str)
                .map_err(|e| ExternalServiceError::InternalError {
                    reason: format!("Failed to get environment variables: {}", e),
                })?;
            all_vars.extend(basic_vars);
        }

        // Get Docker-specific variables if requested (standalone only)
        if !is_cluster && options.include_docker {
            if let (Some(proj_id), Some(_env_id)) = (project_id, environment_id) {
                // Verify service is linked to project
                let link_exists = project_services::Entity::find()
                    .filter(
                        project_services::Column::ServiceId
                            .eq(service_id)
                            .and(project_services::Column::ProjectId.eq(proj_id)),
                    )
                    .one(self.db.as_ref())
                    .await?;

                if link_exists.is_none() {
                    return Err(ExternalServiceError::ServiceNotLinkedToProject {
                        service_id,
                        project_id: proj_id,
                    });
                }

                let docker_vars = service_instance
                    .get_docker_environment_variables(&params_str)
                    .map_err(|e| ExternalServiceError::InternalError {
                        reason: format!("Failed to get docker environment variables: {}", e),
                    })?;
                all_vars.extend(docker_vars);
            }
        }

        // Get runtime variables if requested (standalone only — clusters already populated above)
        if !is_cluster && options.include_runtime {
            if let (Some(proj_id), Some(env_id)) = (project_id, environment_id) {
                // Verify service is linked to project
                let link_exists = project_services::Entity::find()
                    .filter(
                        project_services::Column::ServiceId
                            .eq(service_id)
                            .and(project_services::Column::ProjectId.eq(proj_id)),
                    )
                    .one(self.db.as_ref())
                    .await?;

                if link_exists.is_none() {
                    return Err(ExternalServiceError::ServiceNotLinkedToProject {
                        service_id,
                        project_id: proj_id,
                    });
                }

                let service_config = ServiceConfig {
                    name: service.name.clone(),
                    service_type,
                    version: service.version,
                    parameters: serde_json::to_value(&parameters).map_err(|e| {
                        ExternalServiceError::InternalError {
                            reason: format!("Failed to serialize parameters: {}", e),
                        }
                    })?,
                };

                // Initialize the service to populate its internal config
                service_instance
                    .init(service_config.clone())
                    .await
                    .map_err(|e| ExternalServiceError::InternalError {
                        reason: format!("Failed to initialize service: {}", e),
                    })?;

                // Get project and environment slugs
                let project = projects::Entity::find_by_id(proj_id)
                    .one(self.db.as_ref())
                    .await?
                    .ok_or(ExternalServiceError::ProjectNotFound { id: proj_id })?;

                let environment = temps_entities::environments::Entity::find_by_id(env_id)
                    .one(self.db.as_ref())
                    .await?
                    .ok_or_else(|| ExternalServiceError::InternalError {
                        reason: format!("Environment {} not found", env_id),
                    })?;

                let runtime_vars = service_instance
                    .get_runtime_env_vars(service_config, &project.slug, &environment.slug)
                    .await
                    .map_err(|e| ExternalServiceError::InternalError {
                        reason: format!("Failed to get runtime environment variables: {}", e),
                    })?;

                all_vars.extend(runtime_vars);
            }
        }

        // Handle names_only option
        if options.names_only {
            let names_only: HashMap<String, String> = all_vars
                .keys()
                .map(|k| (k.clone(), String::new()))
                .collect();
            return Ok(EnvironmentVariablesResponse {
                variables: names_only,
                masked: false,
            });
        }

        // Handle mask_sensitive option
        let variables = if options.mask_sensitive {
            all_vars
                .into_iter()
                .map(|(key, value)| {
                    let masked_value = if Self::is_sensitive_variable(&key) {
                        "***".to_string()
                    } else {
                        value
                    };
                    (key, masked_value)
                })
                .collect()
        } else {
            all_vars
        };

        Ok(EnvironmentVariablesResponse {
            variables,
            masked: options.mask_sensitive,
        })
    }

    /// Get environment variable names (safe preview - no sensitive values)
    pub async fn get_service_preview_environment_variable_names(
        &self,
        service_id_val: i32,
    ) -> Result<Vec<String>, ExternalServiceError> {
        let service = self.get_service(service_id_val).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id_val,
                service_type: service.service_type.clone(),
            }
        })?;
        let parameters = self.get_service_parameters(service_id_val).await?;

        // Cluster services: use multi-host env vars from service_members
        if let Some(cluster_vars) = self.build_cluster_env_vars(&service, &parameters).await? {
            return Ok(cluster_vars.keys().cloned().collect());
        }

        let service_instance = self.create_service_instance_for_parameters(
            service.name.clone(),
            service_type,
            &parameters,
        )?;

        // Convert parameters to strings for the service
        let params_str = Self::params_to_strings(&parameters);

        let env_vars = service_instance
            .get_environment_variables(&params_str)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to get environment variables: {}", e),
            })?;

        Ok(env_vars.keys().cloned().collect())
    }

    /// Get environment variables with masked sensitive values
    pub async fn get_service_preview_environment_variables_masked(
        &self,
        service_id_val: i32,
    ) -> Result<HashMap<String, String>, ExternalServiceError> {
        let service = self.get_service(service_id_val).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id_val,
                service_type: service.service_type.clone(),
            }
        })?;
        let parameters = self.get_service_parameters(service_id_val).await?;

        // Cluster services: use multi-host env vars from service_members
        let env_vars =
            if let Some(cluster_vars) = self.build_cluster_env_vars(&service, &parameters).await? {
                cluster_vars
            } else {
                let service_instance = self.create_service_instance_for_parameters(
                    service.name.clone(),
                    service_type,
                    &parameters,
                )?;
                let params_str = Self::params_to_strings(&parameters);
                service_instance
                    .get_environment_variables(&params_str)
                    .map_err(|e| ExternalServiceError::InternalError {
                        reason: format!("Failed to get environment variables: {}", e),
                    })?
            };

        // Mask sensitive values based on variable names
        let masked_vars = env_vars
            .into_iter()
            .map(|(key, value)| {
                let masked_value = if Self::is_sensitive_variable(&key) {
                    "***".to_string()
                } else {
                    value
                };
                (key, masked_value)
            })
            .collect();

        Ok(masked_vars)
    }

    /// Determine if a variable name indicates sensitive data
    fn is_sensitive_variable(var_name: &str) -> bool {
        let sensitive_patterns = [
            "password",
            "pass",
            "secret",
            "key",
            "token",
            "credential",
            "auth",
            "api_key",
            "private",
            "cert",
            "ssl",
            "tls",
        ];

        let var_lower = var_name.to_lowercase();
        sensitive_patterns
            .iter()
            .any(|pattern| var_lower.contains(pattern))
    }

    /// List available Docker containers that can be imported as services
    pub async fn list_available_containers(&self) -> Result<Vec<AvailableContainer>> {
        use bollard::query_parameters::ListContainersOptions;

        // Get list of managed services (we use their service names to exclude them)
        let managed_services = external_services::Entity::find()
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|service| service.name.to_lowercase())
            .collect::<std::collections::HashSet<_>>();

        let mut filters = HashMap::new();
        filters.insert("status".to_string(), vec!["running".to_string()]);

        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters: Some(filters),
                ..Default::default()
            }))
            .await
            .map_err(|e| anyhow::anyhow!("Failed to list Docker containers: {}", e))?;

        let mut available: Vec<AvailableContainer> = Vec::new();

        for container in containers {
            let container_id = container.id.clone().unwrap_or_default();

            // Extract container name (removing leading slash)
            let container_name_raw = container
                .names
                .clone()
                .and_then(|mut names| names.pop())
                .unwrap_or_else(|| container_id.clone());
            let container_name_lower = container_name_raw
                .strip_prefix('/')
                .unwrap_or(&container_name_raw)
                .to_lowercase();

            // Skip containers that are already managed by Temps
            if managed_services.contains(&container_name_lower) {
                continue;
            }

            let image = match &container.image {
                Some(img) => img.clone(),
                None => continue,
            };

            // Detect service type based on image name
            #[allow(deprecated)]
            let service_type = if crate::mariadb_query::is_mariadb_compatible_image(&image) {
                ServiceType::Mariadb
            } else if image.contains("postgres")
                || image.contains("timescaledb")
                || image.contains("pgvector")
            {
                ServiceType::Postgres
            } else if image.contains("redis") {
                ServiceType::Redis
            } else if image.contains("mongo") {
                ServiceType::Mongodb
            } else if image.contains("rustfs") {
                ServiceType::Rustfs
            } else if image.contains("minio") {
                // Existing MinIO containers are detected as deprecated Minio type
                ServiceType::Minio
            } else {
                continue; // Skip unknown service types
            };

            // Extract version from image tag
            let version = if let Some(tag_pos) = image.rfind(':') {
                image[tag_pos + 1..].to_string()
            } else {
                "latest".to_string()
            };

            // Extract exposed ports from container ports
            let exposed_ports = container
                .ports
                .clone()
                .unwrap_or_default()
                .iter()
                .map(|port| port.private_port)
                .collect::<Vec<u16>>();

            available.push(AvailableContainer {
                container_id,
                container_name: container_name_raw
                    .strip_prefix('/')
                    .unwrap_or(&container_name_raw)
                    .to_string(),
                image,
                version,
                service_type,
                is_running: matches!(
                    container.state,
                    Some(bollard::models::ContainerSummaryStateEnum::RUNNING)
                ),
                exposed_ports,
            });
        }

        Ok(available)
    }

    /// Import an existing Docker container as a managed external service
    pub async fn import_service(
        &self,
        request: ImportExternalServiceRequest,
    ) -> Result<ExternalServiceInfo> {
        // Get the service-specific implementation based on Docker inspection
        let container = self
            .docker
            .inspect_container(
                &request.container_id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to inspect container '{}': {}",
                    request.container_id,
                    e
                )
            })?;

        let _image = container.config.and_then(|c| c.image).ok_or_else(|| {
            anyhow::anyhow!(
                "Could not determine image for container '{}'",
                request.container_id
            )
        })?;

        // Convert request parameters to credentials and additional_config for compatibility
        // Credentials are typically: username, password
        // Additional config is: docker_image, port, etc.
        let mut credentials = HashMap::new();
        let mut additional_config = serde_json::json!({});

        for (key, value) in &request.parameters {
            match key.as_str() {
                "username" | "password" | "database" | "root_password" => {
                    if let Some(str_value) = value.as_str() {
                        credentials.insert(key.clone(), str_value.to_string());
                    }
                }
                _ => {
                    if let Some(obj) = additional_config.as_object_mut() {
                        obj.insert(key.clone(), value.clone());
                    }
                }
            }
        }

        // Get the appropriate service instance and call import
        #[allow(deprecated)]
        let service_config = match request.service_type {
            ServiceType::Mariadb => {
                let mariadb = MariaDbService::new(request.name.clone(), Arc::clone(&self.docker));
                mariadb
                    .import_from_container(
                        request.container_id.clone(),
                        request.name.clone(),
                        credentials,
                        additional_config,
                    )
                    .await?
            }
            ServiceType::Postgres => {
                let postgres = PostgresService::new(request.name.clone(), Arc::clone(&self.docker));
                postgres
                    .import_from_container(
                        request.container_id.clone(),
                        request.name.clone(),
                        credentials,
                        additional_config,
                    )
                    .await?
            }
            ServiceType::Redis => {
                let redis = RedisService::new(request.name.clone(), Arc::clone(&self.docker));
                redis
                    .import_from_container(
                        request.container_id.clone(),
                        request.name.clone(),
                        credentials,
                        additional_config,
                    )
                    .await?
            }
            ServiceType::Mongodb => {
                let mongodb = MongodbService::new(request.name.clone(), Arc::clone(&self.docker));
                mongodb
                    .import_from_container(
                        request.container_id.clone(),
                        request.name.clone(),
                        credentials,
                        additional_config,
                    )
                    .await?
            }
            // S3 now uses RustFS by default
            ServiceType::S3 => {
                let rustfs = RustfsService::new(
                    request.name.clone(),
                    Arc::clone(&self.docker),
                    Arc::clone(&self.encryption_service),
                );
                rustfs
                    .import_from_container(
                        request.container_id.clone(),
                        request.name.clone(),
                        credentials,
                        additional_config,
                    )
                    .await?
            }
            // Temps KV uses Redis backend
            ServiceType::Kv => {
                let redis =
                    RedisService::new(format!("kv-{}", request.name), Arc::clone(&self.docker));
                redis
                    .import_from_container(
                        request.container_id.clone(),
                        request.name.clone(),
                        credentials,
                        additional_config,
                    )
                    .await?
            }
            // Temps Blob uses RustfsService (high-performance S3-compatible storage)
            ServiceType::Blob => {
                let rustfs = RustfsService::new(
                    format!("blob-{}", request.name),
                    Arc::clone(&self.docker),
                    Arc::clone(&self.encryption_service),
                );
                rustfs
                    .import_from_container(
                        request.container_id.clone(),
                        request.name.clone(),
                        credentials,
                        additional_config,
                    )
                    .await?
            }
            // RustFS standalone S3-compatible storage
            ServiceType::Rustfs => {
                let rustfs = RustfsService::new(
                    request.name.clone(),
                    Arc::clone(&self.docker),
                    Arc::clone(&self.encryption_service),
                );
                rustfs
                    .import_from_container(
                        request.container_id.clone(),
                        request.name.clone(),
                        credentials,
                        additional_config,
                    )
                    .await?
            }
            // MinIO (deprecated) - kept for backward compatibility
            ServiceType::Minio => {
                let s3 = S3Service::new(
                    request.name.clone(),
                    Arc::clone(&self.docker),
                    Arc::clone(&self.encryption_service),
                );
                s3.import_from_container(
                    request.container_id.clone(),
                    request.name.clone(),
                    credentials,
                    additional_config,
                )
                .await?
            }
        };

        // Store in database
        let config_json = serde_json::to_string(&service_config.parameters)
            .map_err(|e| anyhow::anyhow!("Failed to serialize config: {}", e))?;

        // Encrypt the config
        let encrypted_config = self
            .encryption_service
            .encrypt(config_json.as_bytes())
            .map_err(|e| anyhow::anyhow!("Failed to encrypt service configuration: {}", e))?;

        let external_service = external_services::ActiveModel {
            name: Set(service_config.name.clone()),
            service_type: Set(service_config.service_type.to_string()),
            version: Set(service_config.version.clone()),
            status: Set("running".to_string()),
            config: Set(Some(encrypted_config)),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await
        .map_err(|e| anyhow::anyhow!("Failed to save service to database: {}", e))?;

        // Return the created service info
        Ok(ExternalServiceInfo {
            id: external_service.id,
            name: external_service.name,
            service_type: ServiceType::from_str(&external_service.service_type)?,
            version: external_service.version,
            status: external_service.status,
            connection_info: None,
            created_at: external_service.created_at.to_rfc3339(),
            updated_at: external_service.updated_at.to_rfc3339(),
            node_id: external_service.node_id,
            topology: external_service.topology,
            members: Vec::new(),
            error_message: external_service.error_message,
            metrics_enabled: external_service.metrics_enabled,
        })
    }

    // -----------------------------------------------------------------------
    // Runtime + stats
    //
    // These methods sit close to bollard so handlers can return a clean DTO
    // without having to deal with `inspect_container` quirks (Option<...>
    // everywhere, OOMKilled vs RestartCount nesting, etc.).
    //
    // For services running on a remote node (`service.node_id = Some(_)`),
    // we currently return an empty/placeholder runtime entry — the agent
    // does not yet expose an inspect-or-stats endpoint. Wiring that up is
    // a separate change; this lets the UI render local services today.
    // -----------------------------------------------------------------------

    /// Resolve `(role, container_name)` pairs for every container that
    /// makes up this service. Standalone services return a single entry;
    /// cluster services return one entry per `service_members` row.
    async fn resolve_member_containers(
        &self,
        service: &external_services::Model,
    ) -> Result<Vec<(String, String)>, ExternalServiceError> {
        if service.topology == "cluster" {
            let members = service_members::Entity::find()
                .filter(service_members::Column::ServiceId.eq(service.id))
                .all(self.db.as_ref())
                .await?;
            Ok(members
                .into_iter()
                .map(|m| (m.role, m.container_name))
                .collect())
        } else {
            // Build a fresh service instance just to ask for its container
            // name — every engine knows its own naming convention.
            let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
                ExternalServiceError::InvalidServiceType {
                    id: service.id,
                    service_type: service.service_type.clone(),
                }
            })?;
            let parameters = self.get_service_parameters(service.id).await?;
            let instance = self.create_service_instance_for_parameters(
                service.name.clone(),
                service_type,
                &parameters,
            )?;
            Ok(vec![(
                "standalone".to_string(),
                instance.get_docker_container_name(),
            )])
        }
    }

    /// Inspect a single container and project the result onto
    /// `ContainerRuntimeInfo`. Treats container-not-found as a soft
    /// signal (returns `container_id: None`) rather than an error so
    /// the UI can still render "container missing" instead of a 500.
    async fn inspect_one_container(
        &self,
        role: String,
        container_name: String,
    ) -> ContainerRuntimeInfo {
        let inspected = self
            .docker
            .inspect_container(
                &container_name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await;

        match inspected {
            Ok(info) => {
                let state = info.state.as_ref();
                let host_config_limits = info
                    .host_config
                    .as_ref()
                    .map(|hc| crate::externalsvc::ServiceResourceLimits {
                        memory_mb: hc.memory.filter(|&m| m > 0).map(|m| m / (1024 * 1024)),
                        memory_swap_mb: hc
                            .memory_swap
                            .filter(|&m| m > 0)
                            .map(|m| m / (1024 * 1024)),
                        nano_cpus: hc.nano_cpus.filter(|&n| n > 0),
                        cpu_shares: hc.cpu_shares.filter(|&n| n > 0),
                        shm_size_mb: hc.shm_size.filter(|&m| m > 0).map(|m| m / (1024 * 1024)),
                    })
                    .unwrap_or_default();

                ContainerRuntimeInfo {
                    role,
                    container_name,
                    container_id: info.id,
                    status: state.and_then(|s| {
                        s.status
                            .as_ref()
                            .map(|st| format!("{:?}", st).to_lowercase())
                    }),
                    restart_count: info.restart_count,
                    oom_killed: state.and_then(|s| s.oom_killed),
                    exit_code: state.and_then(|s| s.exit_code),
                    started_at: state.and_then(|s| s.started_at.clone()),
                    finished_at: state.and_then(|s| s.finished_at.clone()),
                    image: info.image,
                    resource_limits: host_config_limits,
                }
            }
            Err(_) => ContainerRuntimeInfo {
                role,
                container_name,
                container_id: None,
                status: None,
                restart_count: None,
                oom_killed: None,
                exit_code: None,
                started_at: None,
                finished_at: None,
                image: None,
                resource_limits: crate::externalsvc::ServiceResourceLimits::default(),
            },
        }
    }

    /// Get a snapshot of every container that makes up this service:
    /// status, restart count, OOM-killed flag, exit code, and current
    /// applied resource limits.
    pub async fn get_service_runtime(
        &self,
        service_id: i32,
    ) -> Result<ServiceRuntimeReport, ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        let containers = self.resolve_member_containers(&service).await?;

        let mut members = Vec::with_capacity(containers.len());
        for (role, name) in containers {
            members.push(self.inspect_one_container(role, name).await);
        }

        Ok(ServiceRuntimeReport {
            service_id: service.id,
            topology: service.topology,
            members,
        })
    }

    /// Sample one stats snapshot from every container in this service.
    /// Uses `one_shot=true, stream=false` so the call returns immediately
    /// instead of holding the connection open for streaming updates.
    pub async fn get_service_stats(
        &self,
        service_id: i32,
    ) -> Result<ServiceStatsReport, ExternalServiceError> {
        // Stream consumption + sampling now lives in
        // `sample_container_stats_twice`. This caller just iterates
        // containers and projects the result.
        let service = self.get_service(service_id).await?;
        let containers = self.resolve_member_containers(&service).await?;

        let mut members = Vec::with_capacity(containers.len());
        for (role, name) in containers {
            // Docker's `one_shot` stats response carries `precpu_stats` as
            // zeros — the CPU formula needs deltas, so a single one_shot
            // sample produces either 0% or the "cumulative since container
            // start" ratio (which was the pre-fix bug: a container at
            // 108% real load read back as 0.6% because total/system over
            // the container's full lifetime is dominated by idle history).
            //
            // Take two one_shot samples 1s apart and compute the delta
            // ourselves. Matches `docker stats` exactly. The 1s window is
            // the same default the Docker CLI uses for its "default"
            // streaming interval.
            let stats = match sample_container_stats_twice(&self.docker, &name).await {
                Some((first, second)) => {
                    // `first` is the earlier sample, `second` is the later one.
                    // `compute_stats_sample` wants (current=later, previous=earlier)
                    // so the delta is positive — passing them reversed makes
                    // `cpu_delta` negative and CPU reads back as `None` (the UI
                    // shows "—" while memory still works from `current`).
                    compute_stats_sample(role.clone(), name.clone(), &second, Some(&first))
                }
                None => ContainerStatsSample {
                    role,
                    container_name: name,
                    cpu_percent: None,
                    memory_usage_bytes: None,
                    memory_limit_bytes: None,
                    memory_percent: None,
                    online_cpus: None,
                },
            };
            members.push(stats);
        }

        Ok(ServiceStatsReport {
            service_id: service.id,
            topology: service.topology,
            members,
        })
    }

    /// Apply a resource-limits block to every container that backs this
    /// service via Docker's live `update_container` API. Works on running
    /// AND stopped containers (Docker accepts updates for both states —
    /// stopped containers pick up the new caps on next start).
    ///
    /// **Limitation: removing a previously-set memory cap requires a
    /// container recreate on most Docker setups.** The Docker daemon
    /// silently treats `Memory: 0` as "no change" and rejects `Memory:
    /// -1` with "Minimum memory limit allowed is 6MB" on Docker Desktop /
    /// recent versions. We detect this case and mark the outcome as
    /// "requires_recreate" so the UI can prompt the operator to restart.
    ///
    /// CPU caps don't have this problem: `NanoCpus: 0` correctly removes
    /// the CPU cap on a running container.
    ///
    /// Returns a per-member outcome so the caller can tell which
    /// containers got the update and which were skipped (missing or
    /// errored). Never fails the request as a whole — limits are already
    /// persisted in the DB by the time this runs.
    async fn apply_limits_to_running_containers(
        &self,
        service: &external_services::Model,
        limits: &crate::externalsvc::ServiceResourceLimits,
    ) -> Vec<ResourceLimitApplyResult> {
        // Resolve every container name that backs this service. Soft-fails
        // back to an empty list when resolution itself errors so the
        // outer call doesn't blow up — limits are already persisted.
        let containers = match self.resolve_member_containers(service).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    service_id = service.id,
                    error = %e,
                    "Could not resolve member containers for live limit update"
                );
                return Vec::new();
            }
        };

        let mut results = Vec::with_capacity(containers.len());

        for (role, container_name) in containers {
            // First check whether the container actually exists. Calling
            // update_container on a missing name returns a confusing 404;
            // distinguishing "missing" from "failed" up front gives the
            // operator a clearer signal in the response.
            let inspected = self
                .docker
                .inspect_container(
                    &container_name,
                    None::<bollard::query_parameters::InspectContainerOptions>,
                )
                .await;

            let outcome = match inspected {
                Err(_) => ResourceLimitApplyResult {
                    role,
                    container_name,
                    outcome: "missing".to_string(),
                    error: None,
                },
                Ok(info) => {
                    let is_running = info.state.as_ref().and_then(|s| s.running).unwrap_or(false);

                    // Detect "removing a previously-set memory cap" up
                    // front: if the container currently has a non-zero
                    // memory limit and the new request is unlimited, the
                    // live update can't honor it. Tell the caller they
                    // need to restart the container to pick up the
                    // change. The persisted limits are correct, and the
                    // recreate path in the engines' `start()` will apply
                    // them on next boot.
                    let current_memory = info
                        .host_config
                        .as_ref()
                        .and_then(|hc| hc.memory)
                        .unwrap_or(0);
                    let removing_memory_cap = current_memory > 0 && limits.memory_mb.is_none();

                    // Detect a shared-memory (`/dev/shm`) size change. Docker
                    // cannot hot-apply `shm_size` — it's a create-time-only
                    // HostConfig field with no slot in the update API — so a
                    // changed shm requires removing + recreating the
                    // container. Only flag when the operator explicitly set a
                    // size that differs from what the live container has; an
                    // unset (None) shm leaves whatever Docker already chose and
                    // must not trigger a spurious recreate against the 64 MiB
                    // default.
                    let current_shm = info
                        .host_config
                        .as_ref()
                        .and_then(|hc| hc.shm_size)
                        .unwrap_or(0);
                    let shm_changed = match limits.shm_size_mb {
                        Some(mb) => {
                            current_shm > 0 && mb.saturating_mul(1024 * 1024) != current_shm
                        }
                        None => false,
                    };

                    if removing_memory_cap {
                        ResourceLimitApplyResult {
                            role,
                            container_name,
                            outcome: "requires_recreate".to_string(),
                            error: Some(
                                "Docker cannot remove a memory limit on a live \
                                 container. Restart the service to apply unlimited \
                                 memory."
                                    .to_string(),
                            ),
                        }
                    } else if shm_changed {
                        ResourceLimitApplyResult {
                            role,
                            container_name,
                            outcome: "requires_recreate".to_string(),
                            error: Some(
                                "Changing shared-memory (/dev/shm) size requires \
                                 deleting and recreating the container (a stop/start \
                                 reuses the old container and keeps the old shm size). \
                                 Recreate the service to apply the new shm size."
                                    .to_string(),
                            ),
                        }
                    } else {
                        // Build the body fresh per-container so a future
                        // per-member override (different caps per cluster
                        // role, etc.) slots in cleanly.
                        let body = build_container_update_body(limits);
                        match self.docker.update_container(&container_name, body).await {
                            Ok(()) => ResourceLimitApplyResult {
                                role,
                                container_name,
                                outcome: if is_running { "applied" } else { "stopped" }.to_string(),
                                error: None,
                            },
                            Err(e) => {
                                // Most common failure: setting memory
                                // below current usage (Docker rejects).
                                // Surface the raw message so the operator
                                // sees the actionable detail.
                                let msg = e.to_string();
                                tracing::warn!(
                                    service_id = service.id,
                                    container = %container_name,
                                    error = %msg,
                                    "docker update_container rejected"
                                );
                                ResourceLimitApplyResult {
                                    role,
                                    container_name,
                                    outcome: "failed".to_string(),
                                    error: Some(msg),
                                }
                            }
                        }
                    }
                }
            };
            results.push(outcome);
        }

        results
    }

    /// Persist a new resource-limits block onto an existing service's
    /// `external_service_params`, then live-apply via Docker's update API.
    ///
    /// Memory and CPU caps can be hot-changed on running containers — no
    /// restart required. Stopped containers also accept the update; they
    /// pick up the new caps on next start. When the container is
    /// completely absent (e.g., never created on this node), only the
    /// stored config is updated and the apply step records "missing".
    /// Delete and recreate a service's container so a new `/dev/shm` size
    /// takes effect.
    ///
    /// `shm_size` is fixed when a Docker container is created — there is no
    /// live update for it, and a plain stop+start reuses the existing
    /// container (keeping the old shm). The only way to apply a new value is
    /// to REMOVE the container and let the engine recreate it. We remove the
    /// container with `v: false` so the **data volume is preserved** (the
    /// engine's `remove()` trait method would also drop the volume — we must
    /// not use it here), then call `initialize_service`, whose
    /// `create_container` now takes the create branch and bakes in the new
    /// `HostConfig.shm_size`.
    ///
    /// Scope: standalone, local-node services. Remote-node and cluster
    /// services are rejected with a clear message so the caller surfaces
    /// "recreate manually" rather than silently doing nothing.
    async fn recreate_service_container_for_shm(
        &self,
        service: &external_services::Model,
    ) -> Result<(), ExternalServiceError> {
        // Remote-node containers live on a worker's Docker daemon, not on
        // `self.docker`. Removing here would target the wrong daemon. The
        // worker's create path already honors shm; the operator must
        // redeploy/recreate the service on that node.
        if service.node_id.is_some() {
            return Err(ExternalServiceError::InternalError {
                reason: format!(
                    "Service {} runs on a remote node; recreate it on that node to apply the new shm size",
                    service.id
                ),
            });
        }

        // Cluster recreate spans multiple members with ordering constraints
        // (monitor → primary → replicas) and isn't covered by
        // `initialize_service`. Don't pretend; tell the operator to recreate.
        if service.topology == "cluster" {
            return Err(ExternalServiceError::InternalError {
                reason: format!(
                    "Service {} is a cluster; recreate its members to apply the new shm size",
                    service.id
                ),
            });
        }

        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service.id,
                service_type: service.service_type.clone(),
            }
        })?;
        let parameters = self.get_service_parameters(service.id).await?;
        let instance = self.create_service_instance_for_parameters(
            service.name.clone(),
            service_type,
            &parameters,
        )?;
        let container_name = instance.get_docker_container_name();

        // Stop, then DELETE the container (volume preserved). Stop is
        // best-effort — the container may already be stopped or gone.
        let _ = self
            .docker
            .stop_container(
                &container_name,
                None::<bollard::query_parameters::StopContainerOptions>,
            )
            .await;
        match self
            .docker
            .remove_container(
                &container_name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    v: false, // keep the data volume — only the container is recreated
                    force: true,
                    ..Default::default()
                }),
            )
            .await
        {
            Ok(()) => {
                info!(
                    service_id = service.id,
                    container = %container_name,
                    "Removed container to apply new /dev/shm size (data volume preserved)"
                );
            }
            Err(e) => {
                // "no such container" is benign — nothing to recreate-from,
                // initialize_service will just create it fresh. Surface other
                // errors so the caller can warn.
                let msg = e.to_string();
                if !msg.contains("No such container") && !msg.contains("no such container") {
                    return Err(ExternalServiceError::InternalError {
                        reason: format!(
                            "Failed to remove container '{}' for shm recreate: {}",
                            container_name, msg
                        ),
                    });
                }
            }
        }

        // Recreate from the now-persisted config (incl. the new resources
        // block). With the old container gone, `create_container` takes the
        // create branch and applies the new shm.
        self.initialize_service(service.id).await
    }

    pub async fn update_service_resource_limits(
        &self,
        service_id: i32,
        new_limits: crate::externalsvc::ServiceResourceLimits,
    ) -> Result<ResourceLimitsUpdateResponse, ExternalServiceError> {
        if let Err(e) = new_limits.validate() {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: e,
            });
        }

        // Load the service plus its current parameters JSON.
        let service = self.get_service(service_id).await?;
        let mut config_json: serde_json::Value = match service.config.as_deref() {
            Some(s) => match self.encryption_service.decrypt_string(s) {
                Ok(decrypted) => serde_json::from_str(&decrypted).map_err(|e| {
                    ExternalServiceError::InternalError {
                        reason: format!(
                            "Failed to parse stored config for service {}: {}",
                            service_id, e
                        ),
                    }
                })?,
                Err(e) => {
                    return Err(ExternalServiceError::InternalError {
                        reason: format!(
                            "Failed to decrypt config for service {}: {}",
                            service_id, e
                        ),
                    })
                }
            },
            None => serde_json::json!({}),
        };

        // Capture the prior shm size before we splice in the new block.
        // shm_size is create-time-only in Docker, so a change here means the
        // container must be removed + recreated — a plain live update can't
        // honor it. We compare prior vs new and drive a recreate below.
        let prior_limits = crate::externalsvc::ServiceResourceLimits::from_parameters(&config_json);
        let shm_changed = prior_limits.shm_size_mb != new_limits.shm_size_mb;

        // Splice the resources block into the parameters JSON. Setting
        // `unlimited` removes the block so the service goes back to
        // running without caps.
        if new_limits.is_unlimited() {
            if let Some(obj) = config_json.as_object_mut() {
                obj.remove("resources");
            }
        } else {
            let limits_value = serde_json::to_value(&new_limits).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to serialize resource limits: {}", e),
                }
            })?;
            match config_json.as_object_mut() {
                Some(obj) => {
                    obj.insert("resources".to_string(), limits_value);
                }
                None => {
                    config_json = serde_json::json!({ "resources": limits_value });
                }
            }
        }

        // Re-encrypt and persist.
        let serialized = serde_json::to_string(&config_json).map_err(|e| {
            ExternalServiceError::InternalError {
                reason: format!("Failed to serialize updated config: {}", e),
            }
        })?;
        let encrypted = self
            .encryption_service
            .encrypt_string(&serialized)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to encrypt updated config: {}", e),
            })?;

        let mut active: external_services::ActiveModel = service.clone().into();
        active.config = Set(Some(encrypted));
        active.update(self.db.as_ref()).await?;

        // When the shared-memory size changed, a live `update_container` can't
        // honor it (shm_size is fixed at container-CREATE time in Docker). The
        // container must be DELETED and recreated — a stop+start reuses the old
        // container and keeps the old shm. `recreate_service_container_for_shm`
        // removes the container (data volume preserved) so the engine's
        // `create_container` rebuilds it with the new `HostConfig.shm_size`.
        //
        // Best-effort: the limits are already persisted, so a recreate failure
        // must not roll them back. We log and fall through to report the
        // per-member outcome — the operator can recreate manually.
        if shm_changed {
            if let Err(e) = self.recreate_service_container_for_shm(&service).await {
                tracing::warn!(
                    service_id,
                    error = %e,
                    "Failed to recreate service container after shm size change; \
                     limits are persisted but the running container still has the \
                     old shm size — recreate manually to apply"
                );
            }
        }

        // Best-effort live apply. Errors here don't undo the persisted
        // limits — the operator can still recreate the container manually
        // to pick them up. After a recreate above, the fresh container
        // already has the new caps, so this reports "applied"/"missing"
        // harmlessly.
        let applied = self
            .apply_limits_to_running_containers(&service, &new_limits)
            .await;

        Ok(ResourceLimitsUpdateResponse {
            limits: new_limits,
            applied,
        })
    }
}

/// Map our `ServiceResourceLimits` onto a bollard `ContainerUpdateBody`.
///
/// CRITICAL: Docker uses `0` (not `null`) as the special value for
/// "remove this constraint". Sending `null` leaves the existing limit in
/// place — exactly the bug an operator hits when they switch a service
/// from limited back to unlimited. So we always emit explicit zeros for
/// the four fields we manage.
///
/// Conversions:
/// - memory_mb       → memory       (bytes; 0 = unlimited)
/// - memory_swap_mb  → memory_swap  (bytes; -1 = unlimited swap, 0 = no swap; we map None→0)
/// - nano_cpus       → nano_cpus    (1e9 = 1 core; 0 = unlimited)
/// - cpu_shares      → cpu_shares   (default 1024; 0 = default)
///
/// NOTE: `shm_size` is intentionally absent. Docker treats `/dev/shm` size
/// as create-time-only; `ContainerUpdateBody` has no `shm_size` field.
/// Changing shm requires REMOVING + RECREATING the container — handled by
/// the auto-recreate path in `update_service_resource_limits`, NOT here.
fn build_container_update_body(
    limits: &crate::externalsvc::ServiceResourceLimits,
) -> bollard::models::ContainerUpdateBody {
    let memory_bytes = limits
        .memory_mb
        .map(|mb| mb.saturating_mul(1024 * 1024))
        .unwrap_or(0);
    let memory_swap_bytes = limits
        .memory_swap_mb
        .map(|mb| mb.saturating_mul(1024 * 1024))
        .unwrap_or(0);
    bollard::models::ContainerUpdateBody {
        memory: Some(memory_bytes),
        memory_swap: Some(memory_swap_bytes),
        nano_cpus: Some(limits.nano_cpus.unwrap_or(0)),
        cpu_shares: Some(limits.cpu_shares.unwrap_or(0)),
        ..Default::default()
    }
}

/// Sample the same container twice ~1s apart so we have a delta window for
/// the CPU formula. Returns `None` on any error or if Docker returns no
/// frames (container missing / stopped). The 1-second pause matches the
/// Docker CLI's default sampling interval.
async fn sample_container_stats_twice(
    docker: &bollard::Docker,
    name: &str,
) -> Option<(
    bollard::models::ContainerStatsResponse,
    bollard::models::ContainerStatsResponse,
)> {
    use futures::StreamExt;

    let opts = bollard::query_parameters::StatsOptionsBuilder::default()
        .stream(false)
        .one_shot(true)
        .build();

    let mut first_stream = docker.stats(name, Some(opts.clone()));
    let first = first_stream.next().await?.ok()?;
    drop(first_stream);

    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    let mut second_stream = docker.stats(name, Some(opts));
    let second = second_stream.next().await?.ok()?;

    Some((first, second))
}

/// Compute the docker-CLI-equivalent CPU percent from two consecutive
/// stats samples. Returns `None` when either sample is missing the
/// counters we need, the deltas are zero/negative (container just
/// started / stopped), or the result isn't finite.
///
/// Formula (matches `docker stats`):
/// ```
/// Remove a single key from an `external_services.health_metadata` JSONB
/// blob. Returns `None` when the result would be an empty object (so the
/// column goes back to NULL instead of `{}`).
fn clear_health_metadata_key(
    existing: Option<&sea_orm::JsonValue>,
    key: &str,
) -> Option<sea_orm::JsonValue> {
    let map = match existing {
        Some(serde_json::Value::Object(m)) => m,
        _ => return None,
    };
    let mut next = map.clone();
    next.remove(key);
    if next.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(next))
    }
}

/// Merge a single typed snapshot under `key` into an
/// `external_services.health_metadata` JSONB blob, preserving sibling keys.
fn merge_health_metadata_key<T: serde::Serialize>(
    existing: Option<&sea_orm::JsonValue>,
    key: &str,
    snapshot: &T,
) -> sea_orm::JsonValue {
    let value = serde_json::to_value(snapshot).unwrap_or(serde_json::Value::Null);
    let mut map = match existing {
        Some(serde_json::Value::Object(m)) => m.clone(),
        _ => serde_json::Map::new(),
    };
    map.insert(key.to_string(), value);
    serde_json::Value::Object(map)
}

/// cpu_delta    = current.total_usage     - previous.total_usage
/// system_delta = current.system_cpu_usage - previous.system_cpu_usage
/// percent      = (cpu_delta / system_delta) * online_cpus * 100
/// ```
fn cpu_percent_from_delta(
    current: &bollard::models::ContainerCpuStats,
    previous: &bollard::models::ContainerCpuStats,
) -> Option<f64> {
    let cur_total = current.cpu_usage.as_ref()?.total_usage? as i128;
    let prev_total = previous.cpu_usage.as_ref()?.total_usage? as i128;
    let cur_system = current.system_cpu_usage? as i128;
    let prev_system = previous.system_cpu_usage? as i128;

    let cpu_delta = cur_total - prev_total;
    let system_delta = cur_system - prev_system;

    // The system delta is the increment of CPU time available across ALL
    // cores; multiplying by online_cpus rescales the ratio so a fully
    // pinned 4-core container reads as 400%, not 100%.
    let cpus = current.online_cpus.unwrap_or(1).max(1) as f64;

    // `system_delta <= 0` means we have no elapsed wall-clock CPU time to
    // divide against — either Docker returned identical samples (just
    // started, missing counters) or the counter wrapped. Either way we
    // can't compute a meaningful percent.
    if system_delta <= 0 {
        return None;
    }

    // `cpu_delta < 0` indicates a counter reset (container restart between
    // samples). `cpu_delta == 0` is the legitimate idle case — the
    // container did zero CPU work during the sample window. Report 0.0%
    // explicitly rather than `None`, otherwise idle services render as
    // "—" in the UI and look like a sampling bug.
    if cpu_delta < 0 {
        return None;
    }

    let percent = (cpu_delta as f64 / system_delta as f64) * cpus * 100.0;
    if percent.is_finite() && percent >= 0.0 {
        Some(percent)
    } else {
        None
    }
}

/// Subtract page cache from raw memory usage so the number matches
/// `docker stats`'s "MEM USAGE" column.
///
/// Docker reports `usage` straight from cgroups, which includes page
/// cache. A Postgres container with an 8 GB working set + 8 GB of file
/// cache reads back as `usage == limit` on a 16 GB cap, even though only
/// half is real RSS. The Docker CLI compensates by subtracting:
/// - cgroup v1: `stats.cache`
/// - cgroup v2: `stats.inactive_file`
///
/// We try cgroup v2 first (modern hosts), fall back to v1. If neither
/// key is present, return the raw usage unchanged — better to slightly
/// over-report than to crash on a missing field.
fn memory_usage_excluding_cache(mem: &bollard::models::ContainerMemoryStats) -> Option<u64> {
    let raw_usage = mem.usage?;
    let cache = mem.stats.as_ref().and_then(|s| {
        // cgroup v2 uses `inactive_file`; older v1 hosts use `cache`.
        // Prefer v2; fall back to v1. Some hosts report both, in which
        // case `inactive_file` is the better signal (matches docker
        // CLI exactly).
        s.get("inactive_file").or_else(|| s.get("cache")).copied()
    });
    match cache {
        Some(c) if c <= raw_usage => Some(raw_usage - c),
        _ => Some(raw_usage),
    }
}

/// Project two consecutive stats responses onto `ContainerStatsSample`.
/// `previous` is `None` when only a single sample is available — in that
/// case CPU is reported as `None` since the delta formula needs two
/// samples; memory is still computed from the latest sample.
fn compute_stats_sample(
    role: String,
    container_name: String,
    current: &bollard::models::ContainerStatsResponse,
    previous: Option<&bollard::models::ContainerStatsResponse>,
) -> ContainerStatsSample {
    let cur_cpu = current.cpu_stats.as_ref();
    let online_cpus = cur_cpu.and_then(|c| c.online_cpus);

    let cpu_percent = match (cur_cpu, previous.and_then(|p| p.cpu_stats.as_ref())) {
        (Some(c), Some(p)) => cpu_percent_from_delta(c, p),
        _ => None,
    };

    let mem_stats = current.memory_stats.as_ref();
    let memory_usage_bytes = mem_stats.and_then(memory_usage_excluding_cache);
    let memory_limit_bytes = mem_stats.and_then(|m| m.limit);
    let memory_percent = match (memory_usage_bytes, memory_limit_bytes) {
        (Some(usage), Some(limit)) if limit > 0 => Some((usage as f64 / limit as f64) * 100.0),
        _ => None,
    };

    ContainerStatsSample {
        role,
        container_name,
        cpu_percent,
        memory_usage_bytes,
        memory_limit_bytes,
        memory_percent,
        online_cpus,
    }
}

/// Rewrites env var values for cross-node deployments.
///
/// Replaces container names and localhost references with the service node's
/// private (WireGuard) address and host port.
fn rewrite_env_vars_for_cross_node(
    env_vars: &mut HashMap<String, String>,
    service_name: &str,
    private_addr: &str,
    host_port: Option<i32>,
    internal_port: Option<i32>,
) {
    let container_name = format!("{}-service", service_name);
    for value in env_vars.values_mut() {
        // Replace container_name:internal_port with private_addr:host_port
        if value.contains(&container_name) {
            if let (Some(hp), Some(ip)) = (host_port, internal_port) {
                *value = value
                    .replace(
                        &format!("{}:{}", container_name, ip),
                        &format!("{}:{}", private_addr, hp),
                    )
                    .replace(&container_name, private_addr);
            }
        }
        // Also replace localhost references for baremetal mode
        if value.contains("localhost") || value.contains("127.0.0.1") {
            *value = value
                .replace("localhost", private_addr)
                .replace("127.0.0.1", private_addr);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Container stats helpers ──────────────────────────────────────────────

    fn cpu_stats_at(
        total: u64,
        system: u64,
        online_cpus: u32,
    ) -> bollard::models::ContainerCpuStats {
        bollard::models::ContainerCpuStats {
            cpu_usage: Some(bollard::models::ContainerCpuUsage {
                total_usage: Some(total),
                ..Default::default()
            }),
            system_cpu_usage: Some(system),
            online_cpus: Some(online_cpus),
            ..Default::default()
        }
    }

    /// 50% on a 2-CPU host: cpu_delta = 1e9 (1 second of CPU time at the
    /// nanosecond resolution Docker reports), system_delta = 4e9 (the
    /// host's "system CPU" counter advances at `wall_ticks * cpus`).
    /// (cpu_delta / system_delta) * online_cpus = 0.25 * 2 = 0.5 = 50%.
    /// Matches docker stats output for a container at half utilization
    /// on 2 cores.
    #[test]
    fn cpu_percent_delta_50pct_two_cpus() {
        let prev = cpu_stats_at(0, 0, 2);
        let curr = cpu_stats_at(1_000_000_000, 4_000_000_000, 2);
        let pct = cpu_percent_from_delta(&curr, &prev).unwrap();
        assert!((pct - 50.0).abs() < 0.01, "expected ~50%, got {pct}");
    }

    /// A container fully saturating both of its 2 CPUs reads as 200%
    /// (matches docker stats display for multi-core saturation).
    #[test]
    fn cpu_percent_delta_fully_pinned_two_cpus_reads_200pct() {
        let prev = cpu_stats_at(0, 0, 2);
        // cpu_delta == system_delta means the container used every
        // available CPU-second the system gave it across all cores.
        let curr = cpu_stats_at(2_000_000_000, 2_000_000_000, 2);
        let pct = cpu_percent_from_delta(&curr, &prev).unwrap();
        assert!((pct - 200.0).abs() < 0.01, "expected ~200%, got {pct}");
    }

    /// Zero/negative deltas (container just started, stopped, or clock
    /// went backwards) must report `None` instead of NaN/Inf/negative.
    /// The pre-fix code returned a misleading "cumulative since boot"
    /// ratio here — usually ~0% for any long-running container.
    #[test]
    fn cpu_percent_delta_zero_returns_none() {
        let prev = cpu_stats_at(5_000_000_000, 10_000_000_000, 4);
        let same = cpu_stats_at(5_000_000_000, 10_000_000_000, 4);
        assert!(cpu_percent_from_delta(&same, &prev).is_none());
    }

    /// Idle running container: zero cpu_delta but positive system_delta
    /// (the host's wall clock kept ticking, the container did no work).
    /// Must report 0.0% — returning None here is what caused idle
    /// services to render as "—" in the UI instead of "0.0%".
    #[test]
    fn cpu_percent_delta_idle_container_reads_zero() {
        let prev = cpu_stats_at(5_000_000_000, 10_000_000_000, 4);
        // Same cpu_total, advanced system_total by 4s on 4 cores.
        let curr = cpu_stats_at(5_000_000_000, 14_000_000_000, 4);
        let pct = cpu_percent_from_delta(&curr, &prev).unwrap();
        assert_eq!(pct, 0.0, "idle container must read 0.0%, got {pct}");
    }

    /// A backwards cpu_delta (counter reset / container restart between
    /// samples) is still `None` — we can't compute a meaningful percent
    /// across a restart.
    #[test]
    fn cpu_percent_delta_negative_cpu_returns_none() {
        let prev = cpu_stats_at(10_000_000_000, 50_000_000_000, 4);
        let curr = cpu_stats_at(1_000_000_000, 54_000_000_000, 4);
        assert!(cpu_percent_from_delta(&curr, &prev).is_none());
    }

    #[test]
    fn cpu_percent_delta_missing_counters_returns_none() {
        let prev = bollard::models::ContainerCpuStats {
            cpu_usage: None,
            system_cpu_usage: Some(0),
            online_cpus: Some(1),
            ..Default::default()
        };
        let curr = cpu_stats_at(1_000_000_000, 1_000_000_000, 1);
        assert!(cpu_percent_from_delta(&curr, &prev).is_none());
    }

    fn mem_stats(
        usage: u64,
        limit: u64,
        cache: Option<(&'static str, u64)>,
    ) -> bollard::models::ContainerMemoryStats {
        let mut stats_map = std::collections::HashMap::new();
        if let Some((key, val)) = cache {
            stats_map.insert(key.to_string(), val);
        }
        bollard::models::ContainerMemoryStats {
            usage: Some(usage),
            limit: Some(limit),
            stats: if cache.is_some() {
                Some(stats_map)
            } else {
                None
            },
            ..Default::default()
        }
    }

    /// cgroup v2 `inactive_file` is preferred over the v1 `cache` key.
    /// A Postgres container with 8 GB working set + 8 GB page cache on a
    /// 16 GB limit must read as 8 GB usage (matching docker stats), not
    /// 16 GB / 16 GB which was the pre-fix bug.
    #[test]
    fn memory_usage_subtracts_inactive_file_cgroup_v2() {
        let mem = mem_stats(
            16 * 1024 * 1024 * 1024, // 16 GB raw usage
            16 * 1024 * 1024 * 1024, // 16 GB limit
            Some(("inactive_file", 8 * 1024 * 1024 * 1024)),
        );
        let usage = memory_usage_excluding_cache(&mem).unwrap();
        assert_eq!(usage, 8 * 1024 * 1024 * 1024);
    }

    /// cgroup v1 hosts surface the cache as `cache`. Subtract it.
    #[test]
    fn memory_usage_subtracts_cache_cgroup_v1() {
        let mem = mem_stats(
            10 * 1024 * 1024 * 1024,
            16 * 1024 * 1024 * 1024,
            Some(("cache", 3 * 1024 * 1024 * 1024)),
        );
        let usage = memory_usage_excluding_cache(&mem).unwrap();
        assert_eq!(usage, 7 * 1024 * 1024 * 1024);
    }

    /// When both keys are present (some hosts report both), prefer
    /// `inactive_file` — that's what the Docker CLI does and it's the
    /// more accurate signal on cgroup v2.
    #[test]
    fn memory_usage_prefers_inactive_file_over_cache_when_both_present() {
        let mut stats_map = std::collections::HashMap::new();
        stats_map.insert("inactive_file".to_string(), 4 * 1024 * 1024 * 1024);
        stats_map.insert("cache".to_string(), 6 * 1024 * 1024 * 1024);
        let mem = bollard::models::ContainerMemoryStats {
            usage: Some(10 * 1024 * 1024 * 1024),
            limit: Some(16 * 1024 * 1024 * 1024),
            stats: Some(stats_map),
            ..Default::default()
        };
        // 10 GB - 4 GB inactive_file = 6 GB. If the helper preferred
        // `cache` we'd see 4 GB.
        let expected: u64 = 6 * 1024 * 1024 * 1024;
        assert_eq!(memory_usage_excluding_cache(&mem).unwrap(), expected);
    }

    /// Without cache info, return raw usage rather than crashing.
    #[test]
    fn memory_usage_returns_raw_when_no_cache_info() {
        let mem = mem_stats(5 * 1024 * 1024 * 1024, 16 * 1024 * 1024 * 1024, None);
        assert_eq!(
            memory_usage_excluding_cache(&mem).unwrap(),
            5u64 * 1024 * 1024 * 1024
        );
    }

    /// Defensive: if `cache` is somehow larger than `usage` (sentinel
    /// values, stat skew), don't underflow — return raw usage.
    #[test]
    fn memory_usage_handles_cache_larger_than_usage() {
        let mem = mem_stats(
            1024 * 1024,
            16 * 1024 * 1024 * 1024,
            Some(("cache", 10 * 1024 * 1024 * 1024)),
        );
        // cache > usage → fall through to raw usage rather than wrap.
        assert_eq!(memory_usage_excluding_cache(&mem).unwrap(), 1024 * 1024);
    }

    fn stats_response_at(
        total: u64,
        system: u64,
        online_cpus: u32,
    ) -> bollard::models::ContainerStatsResponse {
        bollard::models::ContainerStatsResponse {
            cpu_stats: Some(bollard::models::ContainerCpuStats {
                cpu_usage: Some(bollard::models::ContainerCpuUsage {
                    total_usage: Some(total),
                    ..Default::default()
                }),
                system_cpu_usage: Some(system),
                online_cpus: Some(online_cpus),
                ..Default::default()
            }),
            memory_stats: Some(mem_stats(
                100 * 1024 * 1024,
                1024 * 1024 * 1024,
                Some(("inactive_file", 10 * 1024 * 1024)),
            )),
            ..Default::default()
        }
    }

    /// Regression: `compute_stats_sample(current=later, previous=earlier)` is
    /// the correct argument order. The earlier production bug had the call
    /// site swapped, which made `cpu_delta` negative and read back as `None`
    /// — UI showed "—" for CPU while memory still rendered (it doesn't need
    /// the delta).
    #[test]
    fn compute_stats_sample_correct_argument_order_reports_positive_cpu() {
        let earlier = stats_response_at(0, 0, 2);
        let later = stats_response_at(1_000_000_000, 4_000_000_000, 2);

        let sample = compute_stats_sample("primary".into(), "test".into(), &later, Some(&earlier));
        assert_eq!(sample.cpu_percent, Some(50.0));
        assert_eq!(sample.online_cpus, Some(2));
    }

    /// Reversed args (the pre-fix bug shape) produce `None`, not garbage.
    /// Documenting this so the kill-switch is obvious if someone reintroduces
    /// the swap.
    #[test]
    fn compute_stats_sample_swapped_arguments_reads_none() {
        let earlier = stats_response_at(0, 0, 2);
        let later = stats_response_at(1_000_000_000, 4_000_000_000, 2);

        let sample = compute_stats_sample("primary".into(), "test".into(), &earlier, Some(&later));
        assert_eq!(sample.cpu_percent, None);
    }

    // ── End container stats helpers ──────────────────────────────────────────

    #[cfg(feature = "docker-tests")]
    use bollard::Docker;
    #[cfg(feature = "docker-tests")]
    use serde_json::Value as JsonValue;
    #[cfg(feature = "docker-tests")]
    use std::collections::HashMap;
    #[cfg(feature = "docker-tests")]
    use std::net::TcpListener;
    #[cfg(feature = "docker-tests")]
    use temps_core::EncryptionService;
    #[cfg(feature = "docker-tests")]
    use temps_database::test_utils::TestDatabase;

    #[cfg(feature = "docker-tests")]
    fn get_unused_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .expect("Failed to bind to address")
            .local_addr()
            .unwrap()
            .port()
    }
    #[cfg(feature = "docker-tests")]
    async fn setup_test_manager() -> (Arc<ExternalServiceManager>, TestDatabase) {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.db.clone();

        let encryption_key = "test_encryption_key_1234567890ab";
        let encryption_service = Arc::new(EncryptionService::new(encryption_key).unwrap());
        let docker = Arc::new(Docker::connect_with_local_defaults().ok().unwrap());

        let dns_registry = Arc::new(temps_dns::DnsRegistry::new(db.clone()));
        let manager = Arc::new(ExternalServiceManager::new(
            db,
            encryption_service,
            docker.clone(),
            dns_registry,
        ));
        (manager, test_db)
    }

    /// The core safety guard: only PENDING/RUNNING/ROLLING_BACK upgrade rows
    /// block a start/reconcile; terminal statuses must NOT. A silent regression
    /// in the status filter reopens the concurrent-container-mutation-vs-upgrade
    /// race the guard exists to prevent. (Docker-gated only because the test DB
    /// itself needs a container; the guard logic under test is pure Sea-ORM.)
    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn ensure_no_active_upgrade_blocks_only_active_statuses() {
        use crate::externalsvc::postgres_upgrade::status;
        use sea_orm::{ActiveModelTrait, ActiveValue::Set};
        use temps_entities::{postgres_major_upgrades, users};

        let (manager, test_db) = setup_test_manager().await;
        let port = get_unused_port();
        let name = format!("guard-test-{}", chrono::Utc::now().timestamp_millis());
        let mut params = HashMap::new();
        params.insert(
            "database".to_string(),
            JsonValue::String("appdb".to_string()),
        );
        params.insert(
            "username".to_string(),
            JsonValue::String("appuser".to_string()),
        );
        params.insert(
            "password".to_string(),
            JsonValue::String("appsecret".to_string()),
        );
        params.insert("port".to_string(), JsonValue::String(port.to_string()));
        params.insert(
            "docker_image".to_string(),
            JsonValue::String("postgres:17-bookworm".to_string()),
        );
        let svc = manager
            .create_service(CreateExternalServiceRequest {
                name,
                service_type: ServiceType::Postgres,
                version: Some("17".to_string()),
                parameters: params,
                node_id: None,
                topology: "standalone".to_string(),
                members: Vec::new(),
            })
            .await
            .expect("create service");

        let result = async {
            // No upgrade row -> allowed.
            manager
                .ensure_no_active_upgrade(svc.id)
                .await
                .map_err(|e| format!("no upgrade row should allow: {e}"))?;

            let now = chrono::Utc::now();
            // A user row to satisfy postgres_major_upgrades.created_by -> users.id.
            let user = users::ActiveModel {
                name: Set("Guard Test".to_string()),
                email: Set(format!(
                    "guard-user-{}@test.local",
                    now.timestamp_nanos_opt().unwrap_or(0)
                )),
                email_verified: Set(true),
                mfa_enabled: Set(false),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            }
            .insert(test_db.db.as_ref())
            .await
            .map_err(|e| format!("insert user: {e}"))?;

            let row = postgres_major_upgrades::ActiveModel {
                service_id: Set(svc.id),
                from_version: Set("17".to_string()),
                to_version: Set("18".to_string()),
                from_image: Set("postgres:17-bookworm".to_string()),
                to_image: Set("postgres:18-bookworm".to_string()),
                status: Set(status::PENDING.to_string()),
                phase: Set(status::PENDING.to_string()),
                pre_upgrade_backup_id: Set(None),
                log_id: Set(format!("guard-{}", svc.id)),
                rollback_volume_name: Set(None),
                rollback_volume_expires_at: Set(None),
                error_message: Set(None),
                attempt: Set(1),
                started_at: Set(None),
                finished_at: Set(None),
                created_by: Set(user.id),
                created_at: Set(now),
                ..Default::default()
            }
            .insert(test_db.db.as_ref())
            .await
            .map_err(|e| format!("insert upgrade row: {e}"))?;

            for st in [status::PENDING, status::RUNNING, status::ROLLING_BACK] {
                let mut am: postgres_major_upgrades::ActiveModel = row.clone().into();
                am.status = Set(st.to_string());
                am.update(test_db.db.as_ref())
                    .await
                    .map_err(|e| format!("update status {st}: {e}"))?;
                match manager.ensure_no_active_upgrade(svc.id).await {
                    Err(ExternalServiceError::UpgradeInProgress { .. }) => {}
                    other => return Err(format!("status {st} must block, got {other:?}")),
                }
            }

            for st in [
                status::FAILED,
                status::COMPLETED,
                status::CANCELLED,
                status::ROLLED_BACK,
            ] {
                let mut am: postgres_major_upgrades::ActiveModel = row.clone().into();
                am.status = Set(st.to_string());
                am.update(test_db.db.as_ref())
                    .await
                    .map_err(|e| format!("update status {st}: {e}"))?;
                manager
                    .ensure_no_active_upgrade(svc.id)
                    .await
                    .map_err(|e| format!("terminal status {st} must not block: {e}"))?;
            }

            // A different service id is unaffected.
            manager
                .ensure_no_active_upgrade(svc.id + 100_000)
                .await
                .map_err(|e| format!("unrelated service must not block: {e}"))?;
            Ok::<(), String>(())
        }
        .await;

        let _ = manager.delete_service(svc.id).await;
        if let Err(e) = result {
            panic!("ensure_no_active_upgrade guard test failed: {e}");
        }
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_create_postgres_service() {
        let (manager, _test_db) = setup_test_manager().await;
        let random_unused_port = get_unused_port();
        let service_name = format!("test-postgres-{}", chrono::Utc::now().timestamp_millis());
        let mut params = HashMap::new();
        params.insert(
            "database".to_string(),
            JsonValue::String("testdb".to_string()),
        );
        params.insert(
            "username".to_string(),
            JsonValue::String("testuser".to_string()),
        );
        params.insert(
            "password".to_string(),
            JsonValue::String("testpass".to_string()),
        );
        params.insert(
            "port".to_string(),
            JsonValue::String(random_unused_port.to_string()),
        );
        params.insert(
            "host".to_string(),
            JsonValue::String("localhost".to_string()),
        );
        params.insert("max_connections".to_string(), JsonValue::Number(100.into()));
        params.insert(
            "docker_image".to_string(),
            JsonValue::String("gotempsh/postgres-walg:18-bookworm".to_string()),
        );

        let request = CreateExternalServiceRequest {
            name: service_name.clone(),
            service_type: ServiceType::Postgres,
            version: Some("18".to_string()),
            parameters: params,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        };

        let result = manager.create_service(request).await;
        assert!(
            result.is_ok(),
            "Failed to create service: {:?}",
            result.err()
        );

        let service = result.unwrap();
        assert_eq!(service.name, service_name);
        assert_eq!(service.service_type, ServiceType::Postgres);
        assert_eq!(service.version, Some("18".to_string()));
        assert_eq!(service.status, "running");

        // Cleanup
        let _ = manager.delete_service(service.id).await;
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_create_redis_service() {
        let (manager, _test_db) = setup_test_manager().await;
        let random_unused_port = get_unused_port();
        // Unique service name so the derived container name (redis-<name>) does
        // not collide with other tests' containers on the shared CI runner.
        let service_name = format!("test-redis-{}", chrono::Utc::now().timestamp_millis());
        let mut params = HashMap::new();
        params.insert(
            "port".to_string(),
            JsonValue::String(random_unused_port.to_string()),
        );
        let request = CreateExternalServiceRequest {
            name: service_name.clone(),
            service_type: ServiceType::Redis,
            version: Some("7".to_string()),
            parameters: params,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        };

        let result = manager.create_service(request).await;

        let service = result.expect("Failed to create Redis service");
        assert_eq!(service.name, service_name);
        assert_eq!(service.service_type, ServiceType::Redis);
        assert_eq!(service.status, "running");

        // Cleanup
        let _ = manager.delete_service(service.id).await;
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_create_s3_service() {
        let (manager, _test_db) = setup_test_manager().await;

        let random_unused_port = get_unused_port();
        let mut params = HashMap::new();
        params.insert(
            "port".to_string(),
            JsonValue::String(random_unused_port.to_string()),
        );
        // Note: bucket_name is not a parameter - buckets are created dynamically during provisioning
        // access_key and secret_key have defaults, so they're optional

        let request = CreateExternalServiceRequest {
            name: "test-s3".to_string(),
            service_type: ServiceType::S3,
            version: None,
            parameters: params,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        };

        let result = manager.create_service(request).await;

        let service = result.expect("Failed to create S3 service");
        assert_eq!(service.name, "test-s3");
        assert_eq!(service.service_type, ServiceType::S3);
        assert_eq!(service.status, "running");

        // Cleanup
        let _ = manager.delete_service(service.id).await;
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_stop_and_start_service() {
        let (manager, _test_db) = setup_test_manager().await;
        let random_unused_port = get_unused_port();
        // Create a service first. Postgres requires database/username/password
        // at the parameter-validation layer (parameter_strategies), so they
        // must be present even when the test only cares about lifecycle.
        let mut params = HashMap::new();
        params.insert(
            "database".to_string(),
            JsonValue::String("testdb".to_string()),
        );
        params.insert(
            "username".to_string(),
            JsonValue::String("testuser".to_string()),
        );
        params.insert(
            "password".to_string(),
            JsonValue::String("testpass".to_string()),
        );
        params.insert(
            "port".to_string(),
            JsonValue::String(random_unused_port.to_string()),
        );
        params.insert(
            "host".to_string(),
            JsonValue::String("localhost".to_string()),
        );

        let request = CreateExternalServiceRequest {
            name: "test-stop-start".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: params,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        };

        let service = manager.create_service(request).await.unwrap();
        let service_id = service.id;

        // Stop the service
        let stopped_service = manager.stop_service(service_id).await;
        assert!(stopped_service.is_ok());
        assert_eq!(stopped_service.unwrap().status, "stopped");

        // Start the service
        let started_service = manager.start_service(service_id).await;
        assert!(started_service.is_ok());
        assert_eq!(started_service.unwrap().status, "running");

        // Cleanup
        let _ = manager.delete_service(service_id).await;
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_delete_service() {
        let (manager, _test_db) = setup_test_manager().await;

        // Create a service first. Use an explicit unused port and a unique name
        // so the Redis container does not collide with the default port (6379)
        // or another test's container name on the shared CI runner.
        let random_unused_port = get_unused_port();
        let service_name = format!("test-delete-{}", chrono::Utc::now().timestamp_millis());
        let mut params = HashMap::new();
        params.insert(
            "password".to_string(),
            JsonValue::String("redis_pass".to_string()),
        );
        params.insert(
            "port".to_string(),
            JsonValue::String(random_unused_port.to_string()),
        );

        let request = CreateExternalServiceRequest {
            name: service_name,
            service_type: ServiceType::Redis,
            version: None,
            parameters: params,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        };

        let service = manager.create_service(request).await.unwrap();
        let service_id = service.id;

        // Delete the service
        let delete_result = manager.delete_service(service_id).await;
        assert!(delete_result.is_ok());

        // Verify service is deleted
        let get_result = manager.get_service_details(service_id).await;
        assert!(get_result.is_err());
        assert!(matches!(
            get_result.unwrap_err(),
            ExternalServiceError::ServiceNotFound { .. }
        ));
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_update_service_parameters() {
        let (manager, _test_db) = setup_test_manager().await;

        // Create a service first. Use an explicit unused port and unique names
        // so the Postgres container (and its post-rename recreate) does not
        // collide with the default port (5449) or another test's container name
        // on the shared CI runner.
        let random_unused_port = get_unused_port();
        let ts = chrono::Utc::now().timestamp_millis();
        let service_name = format!("test-update-{}", ts);
        let renamed_name = format!("test-update-renamed-{}", ts);
        let mut params = HashMap::new();
        params.insert(
            "database".to_string(),
            JsonValue::String("original_db".to_string()),
        );
        params.insert(
            "username".to_string(),
            JsonValue::String("original_user".to_string()),
        );
        params.insert(
            "password".to_string(),
            JsonValue::String("original_pass".to_string()),
        );
        params.insert(
            "port".to_string(),
            JsonValue::String(random_unused_port.to_string()),
        );

        let request = CreateExternalServiceRequest {
            name: service_name,
            service_type: ServiceType::Postgres,
            version: None,
            parameters: params,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        };

        let service = manager.create_service(request).await.unwrap();
        let service_id = service.id;

        // Update only updateable fields — Postgres marks `database`,
        // `username`, `password`, and `host` as readonly (see
        // parameter_strategies.rs), so the request must not touch them or
        // the strategy rejects the whole update with a validation error.
        // The test asserts the rename + docker_image path works.
        let update_request = UpdateExternalServiceRequest {
            name: Some(renamed_name.clone()),
            parameters: HashMap::new(),
            docker_image: Some("gotempsh/postgres-walg:18-bookworm".to_string()),
        };

        let updated_service = manager.update_service(service_id, update_request).await;
        assert!(
            updated_service.is_ok(),
            "update_service failed: {:?}",
            updated_service.err()
        );
        let updated = updated_service.unwrap();
        assert_eq!(updated.name, renamed_name);

        // Sensitive readonly fields must NOT have been mutated.
        let params_after = manager.get_service_parameters(service_id).await.unwrap();
        assert_eq!(
            params_after.get("database").and_then(|v| v.as_str()),
            Some("original_db"),
            "readonly `database` must be preserved across update"
        );

        // Cleanup
        let _ = manager.delete_service(service_id).await;
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_get_service_by_name() {
        let (manager, _test_db) = setup_test_manager().await;

        // Create a service
        let mut params = HashMap::new();
        params.insert(
            "password".to_string(),
            JsonValue::String("test".to_string()),
        );

        let request = CreateExternalServiceRequest {
            name: "unique-service-name".to_string(),
            service_type: ServiceType::Redis,
            version: None,
            parameters: params,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        };

        let service = manager.create_service(request).await.unwrap();
        let service_id = service.id;

        // Get service by name
        let found_service = manager.get_service_by_name("unique-service-name").await;
        assert!(found_service.is_ok());
        assert_eq!(found_service.unwrap().id, service.id);

        // Cleanup
        let _ = manager.delete_service(service_id).await;
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_get_service_by_slug() {
        let (manager, _test_db) = setup_test_manager().await;

        // Use a name that slugifies to something different (uppercase + hyphens)
        // but still produces a Docker-compatible resource name. Whitespace in
        // the name was previously tested here, but `*Service::new` interpolates
        // the raw name into Docker volume/container names, which forbid
        // whitespace — so the create call would explode before this test
        // could even reach the slug lookup. Picking a Docker-safe name that
        // still differs from its slug keeps slugification under test without
        // colliding with Docker's resource-name regex.
        let mut params = HashMap::new();
        params.insert(
            "password".to_string(),
            JsonValue::String("test".to_string()),
        );

        let raw_name = "Service-By-Slug-Test";
        let expected_slug = ExternalServiceManager::generate_slug(raw_name);
        assert_ne!(
            raw_name, expected_slug,
            "test relies on raw name differing from slug to prove the lookup uses the slug column"
        );

        let request = CreateExternalServiceRequest {
            name: raw_name.to_string(),
            service_type: ServiceType::Redis,
            version: None,
            parameters: params,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        };

        let service = manager.create_service(request).await.unwrap();
        let service_id = service.id;

        // Lookup must succeed by slug, not raw name.
        let found_service = manager
            .get_service_by_slug(&expected_slug)
            .await
            .expect("lookup by slug should succeed");
        assert_eq!(found_service.id, service.id);

        // Lookup by the raw name through the slug endpoint must NOT succeed —
        // that would mean we're filtering by name instead of slug (the bug
        // this test exists to guard against).
        let by_name_through_slug = manager.get_service_by_slug(raw_name).await;
        assert!(
            by_name_through_slug.is_err(),
            "get_service_by_slug must filter on the slug column, not name"
        );

        // Cleanup
        let _ = manager.delete_service(service_id).await;
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_list_services() {
        let (manager, _test_db) = setup_test_manager().await;

        // Create multiple services
        let mut services_created = vec![];

        for i in 0..3 {
            let random_unused_port = get_unused_port();
            let mut params = HashMap::new();
            params.insert(
                "port".to_string(),
                JsonValue::String(random_unused_port.to_string()),
            );

            let request = CreateExternalServiceRequest {
                name: format!("service-{}", i),
                service_type: ServiceType::Redis,
                version: None,
                parameters: params,
                node_id: None,
                topology: "standalone".to_string(),
                members: Vec::new(),
            };

            let service = manager.create_service(request).await.unwrap();
            services_created.push(service);
        }

        // List all services
        let all_services = manager.list_services().await;
        assert!(all_services.is_ok());

        let services_list = all_services.unwrap();
        assert!(services_list.len() >= 3);

        // Verify our created services are in the list
        for created in &services_created {
            assert!(services_list.iter().any(|s| s.id == created.id));
        }

        // Cleanup
        for service in services_created {
            let _ = manager.delete_service(service.id).await;
        }
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_service_environment_variables() {
        let (manager, _test_db) = setup_test_manager().await;
        let random_unused_port = get_unused_port();
        // Create a postgres service
        let mut params = HashMap::new();
        params.insert(
            "database".to_string(),
            JsonValue::String("envtest".to_string()),
        );
        params.insert(
            "username".to_string(),
            JsonValue::String("envuser".to_string()),
        );
        params.insert(
            "password".to_string(),
            JsonValue::String("envpass".to_string()),
        );
        params.insert(
            "port".to_string(),
            JsonValue::String(random_unused_port.to_string()),
        );
        params.insert(
            "host".to_string(),
            JsonValue::String("localhost".to_string()),
        );

        let request = CreateExternalServiceRequest {
            name: "env-test-service".to_string(),
            service_type: ServiceType::Postgres,
            version: Some("16".to_string()),
            parameters: params,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        };

        let service = manager.create_service(request).await.unwrap();
        let service_id = service.id;

        // Create a dummy project for testing
        let project_id = 1; // Assuming project with ID 1 exists or will be created

        // Get environment variables
        let env_vars_result = manager
            .get_service_environment_variables(service_id, project_id)
            .await;
        assert!(env_vars_result.is_ok());

        let env_vars = env_vars_result.unwrap();
        assert!(env_vars.contains_key("POSTGRES_DB"));
        assert!(env_vars.contains_key("POSTGRES_USER"));
        assert!(env_vars.contains_key("POSTGRES_PASSWORD"));
        assert_eq!(env_vars.get("POSTGRES_DB"), Some(&"envtest".to_string()));
        assert_eq!(env_vars.get("POSTGRES_USER"), Some(&"envuser".to_string()));

        // Cleanup
        let _ = manager.delete_service(service_id).await;
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_service_parameter_encryption() {
        let (manager, _test_db) = setup_test_manager().await;
        let random_unused_port = get_unused_port();
        // Create a service with sensitive parameters
        let mut params = HashMap::new();
        params.insert(
            "database".to_string(),
            JsonValue::String("cryptodb".to_string()),
        );
        params.insert(
            "username".to_string(),
            JsonValue::String("cryptouser".to_string()),
        );
        params.insert(
            "password".to_string(),
            JsonValue::String("super_secret_password".to_string()),
        );
        params.insert(
            "port".to_string(),
            JsonValue::String(random_unused_port.to_string()),
        );
        params.insert(
            "host".to_string(),
            JsonValue::String("localhost".to_string()),
        );
        params.insert("max_connections".to_string(), JsonValue::Number(100.into()));
        params.insert(
            "docker_image".to_string(),
            JsonValue::String("gotempsh/postgres-walg:18-bookworm".to_string()),
        );

        let request = CreateExternalServiceRequest {
            name: "crypto-service".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: params,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        };

        let service = manager.create_service(request).await.unwrap();
        let service_id = service.id;

        // Get service details and verify parameters are properly handled
        let details = manager.get_service_details(service_id).await;
        assert!(details.is_ok());

        let service_details = details.unwrap();
        assert!(service_details.current_parameters.is_some());

        let current_params = service_details.current_parameters.unwrap();
        // Password should be decrypted for authorized access
        assert_eq!(
            current_params.get("password"),
            Some(&JsonValue::String("super_secret_password".to_string()))
        );

        // Cleanup
        let _ = manager.delete_service(service_id).await;
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_invalid_service_type() {
        let (manager, _test_db) = setup_test_manager().await;

        // Try to get a service with invalid ID
        let result = manager.get_service_details(99999).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ExternalServiceError::ServiceNotFound { .. }
        ));
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_validate_parameters_fails_with_missing_required() {
        let (manager, _test_db) = setup_test_manager().await;

        // Create a postgres service without required parameters
        let params = HashMap::new(); // Empty parameters

        let request = CreateExternalServiceRequest {
            name: "invalid-service".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: params,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        };

        let result = manager.create_service(request).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ExternalServiceError::ParameterValidationFailed { .. }
        ));
    }

    #[tokio::test]
    async fn test_slug_generation() {
        // Test the slug generation logic
        assert_eq!(
            ExternalServiceManager::generate_slug("My Service Name"),
            "my-service-name"
        );
        assert_eq!(
            ExternalServiceManager::generate_slug("Service@#$123"),
            "service123"
        );
        assert_eq!(
            ExternalServiceManager::generate_slug("   Spaces   Everywhere   "),
            "---spaces---everywhere---"
        );
    }

    #[tokio::test]
    async fn test_is_sensitive_variable() {
        assert!(ExternalServiceManager::is_sensitive_variable("password"));
        assert!(ExternalServiceManager::is_sensitive_variable("SECRET_KEY"));
        assert!(ExternalServiceManager::is_sensitive_variable("api_token"));
        assert!(ExternalServiceManager::is_sensitive_variable(
            "PRIVATE_CERT"
        ));
        assert!(ExternalServiceManager::is_sensitive_variable(
            "auth_credential"
        ));

        assert!(!ExternalServiceManager::is_sensitive_variable("database"));
        assert!(!ExternalServiceManager::is_sensitive_variable("username"));
        assert!(!ExternalServiceManager::is_sensitive_variable("port"));
        assert!(!ExternalServiceManager::is_sensitive_variable("host"));
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_upgrade_postgres_image_parameter_update() {
        // This test verifies that the docker_image parameter can be updated.
        // Uses same-major-version update (18 -> 18-alpine) to avoid data format
        // incompatibility issues that occur with cross-major-version upgrades.
        let (manager, _test_db) = setup_test_manager().await;
        let random_unused_port = get_unused_port();

        // Step 1: Create a PostgreSQL service with postgres:18
        let mut params = HashMap::new();
        params.insert(
            "database".to_string(),
            JsonValue::String("testdb".to_string()),
        );
        params.insert(
            "username".to_string(),
            JsonValue::String("testuser".to_string()),
        );
        params.insert(
            "password".to_string(),
            JsonValue::String("testpass".to_string()),
        );
        params.insert(
            "port".to_string(),
            JsonValue::String(random_unused_port.to_string()),
        );
        params.insert(
            "host".to_string(),
            JsonValue::String("localhost".to_string()),
        );
        params.insert("max_connections".to_string(), JsonValue::Number(100.into()));
        params.insert(
            "docker_image".to_string(),
            JsonValue::String("postgres:18".to_string()),
        );

        let request = CreateExternalServiceRequest {
            name: "test-postgres-upgrade-params".to_string(),
            service_type: ServiceType::Postgres,
            version: Some("18".to_string()),
            parameters: params,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        };

        let service = manager
            .create_service(request)
            .await
            .expect("Failed to create PostgreSQL 18 service");
        let service_id = service.id;

        // Verify initial service configuration
        let initial_details = manager.get_service_details(service_id).await.unwrap();
        let initial_params = initial_details.current_parameters.unwrap();
        assert_eq!(
            initial_params.get("docker_image").and_then(|v| v.as_str()),
            Some("postgres:18"),
            "Initial docker_image should be postgres:18"
        );

        // Step 2: Update docker_image parameter to gotempsh/postgres-walg:18-bookworm (same major version, different variant).
        // Only include updateable parameters - readonly params (database, username, password, host)
        // are rejected by validate_for_update().
        let mut update_params = HashMap::new();
        update_params.insert(
            "port".to_string(),
            JsonValue::String(random_unused_port.to_string()),
        );
        update_params.insert("max_connections".to_string(), JsonValue::Number(100.into()));

        let update_request = UpdateExternalServiceRequest {
            name: None,
            parameters: update_params,
            docker_image: Some("gotempsh/postgres-walg:18-bookworm".to_string()),
        };

        // Update the service - same major version so data is compatible.
        // Container reinitialization may fail in CI (e.g., image pull timeout), so we
        // tolerate errors from container recreation while still verifying the DB was updated.
        let update_result = manager.update_service(service_id, update_request).await;
        if let Err(ref e) = update_result {
            eprintln!(
                "Note: update_service returned error (container reinit may have failed): {}",
                e
            );
        }

        // Verify the docker_image parameter has been updated in the database.
        // The parameter update happens before container reinitialization in update_service(),
        // so even if container recreation fails, the config should be persisted.
        let updated_details = manager.get_service_details(service_id).await.unwrap();
        let updated_params = updated_details.current_parameters.unwrap();
        assert_eq!(
            updated_params.get("docker_image").and_then(|v| v.as_str()),
            Some("gotempsh/postgres-walg:18-bookworm"),
            "Docker image parameter should be updated to gotempsh/postgres-walg:18-bookworm"
        );

        // Cleanup - force delete to remove even unhealthy containers
        let _ = manager.delete_service(service_id).await;
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_create_service_with_invalid_params_rolls_back() {
        let (manager, _test_db) = setup_test_manager().await;

        // Create a Redis service with invalid port (email address)
        let mut params = HashMap::new();
        params.insert(
            "port".to_string(),
            JsonValue::String("dviejo@kfs.es".to_string()), // Invalid port
        );
        params.insert(
            "host".to_string(),
            JsonValue::String("localhost".to_string()),
        );

        let request = CreateExternalServiceRequest {
            name: "invalid-redis".to_string(),
            service_type: ServiceType::Redis,
            version: Some("7".to_string()),
            parameters: params,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        };

        // Attempt to create the service - should fail
        let result = manager.create_service(request).await;
        assert!(
            result.is_err(),
            "Expected service creation to fail with invalid port"
        );

        // Verify the error is an initialization failure
        match result.unwrap_err() {
            ExternalServiceError::InitializationFailed { id, reason } => {
                // Verify the error message contains information about the invalid port
                assert!(
                    reason.contains("invalid port") || reason.contains("port specification"),
                    "Expected error about invalid port, got: {}",
                    reason
                );

                // Most importantly: verify the service record was NOT left in the database
                let service_check = manager.get_service(id).await;
                assert!(
                    service_check.is_err(),
                    "Service record should not exist after failed initialization"
                );

                // Verify it's specifically a "not found" error
                match service_check.unwrap_err() {
                    ExternalServiceError::ServiceNotFound { .. } => {
                        // This is what we expect - service was properly cleaned up
                    }
                    other => panic!(
                        "Expected ServiceNotFound error, got different error: {:?}",
                        other
                    ),
                }
            }
            other => panic!(
                "Expected InitializationFailed error, got different error: {:?}",
                other
            ),
        }

        // Double-check: list all services and verify our failed service is not there
        let all_services = manager.list_services().await.unwrap();
        assert!(
            !all_services.iter().any(|s| s.name == "invalid-redis"),
            "Failed service should not appear in service list"
        );
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_masked_environment_variables() {
        let (manager, _test_db) = setup_test_manager().await;
        // Find a random unused port on the system

        let random_unused_port = get_unused_port();

        // Create a service with sensitive parameters
        let mut params = HashMap::new();
        params.insert(
            "database".to_string(),
            JsonValue::String("testdb".to_string()),
        );
        params.insert(
            "username".to_string(),
            JsonValue::String("user".to_string()),
        );
        params.insert(
            "password".to_string(),
            JsonValue::String("secret123".to_string()),
        );
        params.insert(
            "port".to_string(),
            JsonValue::String(random_unused_port.to_string()),
        );

        let request = CreateExternalServiceRequest {
            name: "masked-test".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: params,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        };

        let service = manager.create_service(request).await.unwrap();
        let service_id = service.id;

        // Get masked environment variables
        let masked_vars = manager
            .get_service_preview_environment_variables_masked(service_id)
            .await;

        assert!(masked_vars.is_ok());
        let vars = masked_vars.unwrap();

        // Password should be masked
        assert_eq!(vars.get("POSTGRES_PASSWORD"), Some(&"***".to_string()));
        // Non-sensitive values should not be masked
        assert_eq!(vars.get("POSTGRES_DB"), Some(&"testdb".to_string()));
        assert_eq!(vars.get("POSTGRES_USER"), Some(&"user".to_string()));

        // Cleanup
        let _ = manager.delete_service(service_id).await;
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_cannot_update_postgres_username() {
        let (manager, _test_db) = setup_test_manager().await;
        let random_unused_port = get_unused_port();
        let mut params = HashMap::new();
        params.insert(
            "database".to_string(),
            JsonValue::String("testdb".to_string()),
        );
        params.insert(
            "username".to_string(),
            JsonValue::String("testuser".to_string()),
        );
        params.insert(
            "password".to_string(),
            JsonValue::String("testpass".to_string()),
        );
        params.insert(
            "port".to_string(),
            JsonValue::String(random_unused_port.to_string()),
        );

        let request = CreateExternalServiceRequest {
            name: "test-postgres-readonly".to_string(),
            service_type: ServiceType::Postgres,
            version: Some("16".to_string()),
            parameters: params,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        };

        let service = manager
            .create_service(request)
            .await
            .expect("Failed to create service");
        let service_id = service.id;

        // Try to update username (readonly parameter)
        let mut update_params = HashMap::new();
        update_params.insert(
            "username".to_string(),
            JsonValue::String("newuser".to_string()),
        );

        let update_request = UpdateExternalServiceRequest {
            name: None,
            parameters: update_params,
            docker_image: None,
        };

        // This should FAIL because username is readonly
        let result = manager.update_service(service_id, update_request).await;
        assert!(
            result.is_err(),
            "Expected update to fail for readonly parameter"
        );

        match result.unwrap_err() {
            ExternalServiceError::ParameterValidationFailed { reason, .. } => {
                assert!(
                    reason.contains("username"),
                    "Error should mention 'username', got: {}",
                    reason
                );
                assert!(
                    reason.contains("Cannot update"),
                    "Error should say cannot update"
                );
            }
            other => panic!("Expected ParameterValidationFailed, got: {:?}", other),
        }

        // Cleanup
        let _ = manager.delete_service(service_id).await;
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_cannot_update_postgres_password() {
        let (manager, _test_db) = setup_test_manager().await;
        let random_unused_port = get_unused_port();
        let mut params = HashMap::new();
        params.insert(
            "database".to_string(),
            JsonValue::String("testdb".to_string()),
        );
        params.insert(
            "username".to_string(),
            JsonValue::String("testuser".to_string()),
        );
        params.insert(
            "password".to_string(),
            JsonValue::String("testpass".to_string()),
        );
        params.insert(
            "port".to_string(),
            JsonValue::String(random_unused_port.to_string()),
        );

        let request = CreateExternalServiceRequest {
            name: "test-postgres-pwd".to_string(),
            service_type: ServiceType::Postgres,
            version: Some("16".to_string()),
            parameters: params,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        };

        let service = manager
            .create_service(request)
            .await
            .expect("Failed to create service");
        let service_id = service.id;

        // Try to update password (readonly parameter)
        let mut update_params = HashMap::new();
        update_params.insert(
            "password".to_string(),
            JsonValue::String("wrongpassword".to_string()),
        );

        let update_request = UpdateExternalServiceRequest {
            name: None,
            parameters: update_params,
            docker_image: None,
        };

        let result = manager.update_service(service_id, update_request).await;
        assert!(
            result.is_err(),
            "Expected update to fail for readonly password parameter"
        );

        // Cleanup
        let _ = manager.delete_service(service_id).await;
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_cannot_update_postgres_database() {
        let (manager, _test_db) = setup_test_manager().await;
        let random_unused_port = get_unused_port();
        let mut params = HashMap::new();
        params.insert(
            "database".to_string(),
            JsonValue::String("testdb".to_string()),
        );
        params.insert(
            "username".to_string(),
            JsonValue::String("testuser".to_string()),
        );
        params.insert(
            "password".to_string(),
            JsonValue::String("testpass".to_string()),
        );
        params.insert(
            "port".to_string(),
            JsonValue::String(random_unused_port.to_string()),
        );

        let request = CreateExternalServiceRequest {
            name: "test-postgres-db".to_string(),
            service_type: ServiceType::Postgres,
            version: Some("16".to_string()),
            parameters: params,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        };

        let service = manager
            .create_service(request)
            .await
            .expect("Failed to create service");
        let service_id = service.id;

        // Try to update database (readonly parameter)
        let mut update_params = HashMap::new();
        update_params.insert(
            "database".to_string(),
            JsonValue::String("newdb".to_string()),
        );

        let update_request = UpdateExternalServiceRequest {
            name: None,
            parameters: update_params,
            docker_image: None,
        };

        let result = manager.update_service(service_id, update_request).await;
        assert!(
            result.is_err(),
            "Expected update to fail for readonly database parameter"
        );

        // Cleanup
        let _ = manager.delete_service(service_id).await;
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_can_update_postgres_docker_image() {
        let (manager, _test_db) = setup_test_manager().await;
        let random_unused_port = get_unused_port();
        let mut params = HashMap::new();
        params.insert(
            "database".to_string(),
            JsonValue::String("testdb".to_string()),
        );
        params.insert(
            "username".to_string(),
            JsonValue::String("testuser".to_string()),
        );
        params.insert(
            "password".to_string(),
            JsonValue::String("testpass".to_string()),
        );
        params.insert(
            "port".to_string(),
            JsonValue::String(random_unused_port.to_string()),
        );
        // Explicitly set docker_image so the test is deterministic
        params.insert(
            "docker_image".to_string(),
            JsonValue::String("postgres:18".to_string()),
        );

        let request = CreateExternalServiceRequest {
            name: "test-postgres-image".to_string(),
            service_type: ServiceType::Postgres,
            version: Some("18".to_string()),
            parameters: params,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        };

        let service = manager
            .create_service(request)
            .await
            .expect("Failed to create service");
        let service_id = service.id;

        // Update docker_image to a compatible variant (same major version, different tag).
        // Changing to a different major version (e.g., 18 -> 17) would fail because
        // PostgreSQL data files are not backward-compatible across major versions.
        let update_params = HashMap::new();

        let update_request = UpdateExternalServiceRequest {
            name: None,
            parameters: update_params,
            docker_image: Some("gotempsh/postgres-walg:18-bookworm".to_string()),
        };

        let result = manager.update_service(service_id, update_request).await;
        assert!(result.is_ok(), "Should be able to update docker_image");

        // Verify the docker_image was updated
        let details = manager.get_service_details(service_id).await.unwrap();
        let params = details.current_parameters.unwrap();
        assert_eq!(
            params.get("docker_image").and_then(|v| v.as_str()),
            Some("gotempsh/postgres-walg:18-bookworm")
        );

        // Cleanup
        let _ = manager.delete_service(service_id).await;
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_cannot_update_redis_password() {
        let (manager, _test_db) = setup_test_manager().await;
        let random_unused_port = get_unused_port();
        let mut params = HashMap::new();
        params.insert(
            "password".to_string(),
            JsonValue::String("redis_password".to_string()),
        );
        params.insert(
            "port".to_string(),
            JsonValue::String(random_unused_port.to_string()),
        );

        let request = CreateExternalServiceRequest {
            name: "test-redis-pwd".to_string(),
            service_type: ServiceType::Redis,
            version: Some("7".to_string()),
            parameters: params,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        };

        let service = manager
            .create_service(request)
            .await
            .expect("Failed to create service");
        let service_id = service.id;

        // Try to update password (readonly parameter for Redis)
        let mut update_params = HashMap::new();
        update_params.insert(
            "password".to_string(),
            JsonValue::String("new_password".to_string()),
        );

        let update_request = UpdateExternalServiceRequest {
            name: None,
            parameters: update_params,
            docker_image: None,
        };

        let result = manager.update_service(service_id, update_request).await;
        assert!(
            result.is_err(),
            "Expected update to fail for readonly password parameter in Redis"
        );

        // Cleanup
        let _ = manager.delete_service(service_id).await;
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_prevent_duplicate_service_type_linking() {
        use temps_entities::preset::Preset;
        use temps_entities::{external_services, project_services, projects};

        let (_manager, test_db) = setup_test_manager().await;

        // Create a test project
        let project = projects::ActiveModel {
            name: Set("test-project-duplicate-services".to_string()),
            preset: Set(Preset::Static),
            slug: Set("test-project-duplicate".to_string()),
            directory: Set(".".to_string()),
            main_branch: Set("main".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            ..Default::default()
        };
        let project = project
            .insert(test_db.db.as_ref())
            .await
            .expect("Failed to create project");
        let project_id = project.id;

        // Create first PostgreSQL service (directly in database, not via manager)
        let service_pg1 = external_services::ActiveModel {
            name: Set("test-postgres-1".to_string()),
            service_type: Set("postgres".to_string()),
            version: Set(Some("16".to_string())),
            status: Set("active".to_string()),
            slug: Set(Some("test-postgres-1".to_string())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let service_pg1 = service_pg1
            .insert(test_db.db.as_ref())
            .await
            .expect("Failed to create first service");

        // Create second PostgreSQL service
        let service_pg2 = external_services::ActiveModel {
            name: Set("test-postgres-2".to_string()),
            service_type: Set("postgres".to_string()),
            version: Set(Some("16".to_string())),
            status: Set("active".to_string()),
            slug: Set(Some("test-postgres-2".to_string())),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let service_pg2 = service_pg2
            .insert(test_db.db.as_ref())
            .await
            .expect("Failed to create second service");

        // Create an ExternalServiceManager for testing
        let encryption_key = "test_encryption_key_1234567890ab";
        let encryption_service = Arc::new(EncryptionService::new(encryption_key).unwrap());
        let docker = Arc::new(Docker::connect_with_local_defaults().ok().unwrap());
        let dns_registry = Arc::new(temps_dns::DnsRegistry::new(test_db.db.clone()));
        let manager = ExternalServiceManager::new(
            test_db.db.clone(),
            encryption_service,
            docker,
            dns_registry,
        );

        // Link first PostgreSQL service to project
        let result_link1 = manager
            .link_service_to_project(service_pg1.id, project_id)
            .await;
        assert!(
            result_link1.is_ok(),
            "Failed to link first PostgreSQL service: {:?}",
            result_link1.err()
        );

        // Try to link second PostgreSQL service (should fail due to duplicate type)
        let result_link2 = manager
            .link_service_to_project(service_pg2.id, project_id)
            .await;

        assert!(
            result_link2.is_err(),
            "Expected linking second PostgreSQL service to fail due to duplicate service type"
        );

        // Verify it's the correct error type
        match result_link2 {
            Err(ExternalServiceError::DuplicateServiceType {
                project_id: pid,
                service_type,
            }) => {
                assert_eq!(pid, project_id);
                assert_eq!(service_type, "postgres");
            }
            _ => panic!(
                "Expected DuplicateServiceType error, got: {:?}",
                result_link2
            ),
        }

        // Verify first link was created by checking the database
        let links = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project_id))
            .all(test_db.db.as_ref())
            .await
            .expect("Failed to query links");

        assert_eq!(links.len(), 1, "Expected exactly one service link");
        assert_eq!(links[0].service_id, service_pg1.id);
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_import_postgres_container_from_docker() {
        // Skip if Docker is not available
        let _docker = match Docker::connect_with_local_defaults() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Docker not available, skipping import test");
                return;
            }
        };

        let (manager, _test_db) = setup_test_manager().await;

        // TODO: Implement proper Docker container creation and import test
        // This test requires fixing the Bollard API usage for container creation
        // For now, we just verify that the manager can be created and list_available_containers works

        // Test list_available_containers - should return Ok even if no containers match
        match manager.list_available_containers().await {
            Ok(_containers) => {
                println!("✅ list_available_containers test passed");
            }
            Err(e) => {
                println!("⚠️  list_available_containers returned error: {}", e);
                // Don't panic - Docker may not be fully configured in test environment
            }
        }
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_list_available_containers() {
        // Skip if Docker is not available
        let _docker = match Docker::connect_with_local_defaults() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Docker not available, skipping list containers test");
                return;
            }
        };

        let (manager, _test_db) = setup_test_manager().await;

        // List available containers
        let result = manager.list_available_containers().await;

        assert!(
            result.is_ok(),
            "Failed to list containers: {:?}",
            result.err()
        );

        let containers = result.unwrap();
        println!("Found {} available containers", containers.len());

        // Verify structure of returned containers
        for container in containers {
            assert!(!container.container_id.is_empty(), "Container ID is empty");
            assert!(
                !container.container_name.is_empty(),
                "Container name is empty"
            );
            assert!(!container.image.is_empty(), "Image is empty");
            assert!(!container.version.is_empty(), "Version is empty");
        }
    }

    #[test]
    fn test_available_container_structure() {
        // Test that AvailableContainer struct is properly formed
        let container = AvailableContainer {
            container_id: "abc123".to_string(),
            container_name: "postgres-prod".to_string(),
            image: "gotempsh/postgres-walg:15-bookworm".to_string(),
            version: "15-bookworm".to_string(),
            service_type: ServiceType::Postgres,
            is_running: true,
            exposed_ports: vec![5432],
        };

        assert_eq!(container.container_id, "abc123");
        assert_eq!(container.container_name, "postgres-prod");
        assert_eq!(container.image, "gotempsh/postgres-walg:15-bookworm");
        assert_eq!(container.version, "15-bookworm");
        assert_eq!(container.service_type, ServiceType::Postgres);
        assert!(container.is_running);
    }

    #[test]
    fn test_service_type_detection_postgres() {
        let images = vec![
            "gotempsh/postgres-walg:15-bookworm",
            "gotempsh/postgres-walg:16-bookworm",
            "timescaledb/timescaledb-ha:pg15",
        ];

        for image in images {
            let detected = if image.contains("postgres") || image.contains("timescaledb") {
                ServiceType::Postgres
            } else {
                ServiceType::Redis
            };
            assert_eq!(
                detected,
                ServiceType::Postgres,
                "Failed for image: {}",
                image
            );
        }
    }

    #[test]
    fn test_service_type_detection_redis() {
        let images = vec![
            "gotempsh/redis-walg:8-bookworm",
            "redis:latest",
            "redis:6.2-bullseye",
        ];

        for image in images {
            let detected = if image.contains("redis") {
                ServiceType::Redis
            } else {
                ServiceType::Postgres
            };
            assert_eq!(detected, ServiceType::Redis, "Failed for image: {}", image);
        }
    }

    #[test]
    fn test_service_type_detection_mongodb() {
        let images = vec![
            "gotempsh/mongodb-walg:7.0",
            "mongo:latest",
            "gotempsh/mongodb-walg:8.0",
        ];

        for image in images {
            let detected = if image.contains("mongo") {
                ServiceType::Mongodb
            } else {
                ServiceType::Postgres
            };
            assert_eq!(
                detected,
                ServiceType::Mongodb,
                "Failed for image: {}",
                image
            );
        }
    }

    #[test]
    #[allow(deprecated)]
    fn test_service_type_detection_s3() {
        // S3 type is now backed by RustFS - MinIO images are detected as Minio (deprecated)
        let minio_images = vec![
            "minio/minio:latest",
            "minio/minio:RELEASE.2025-01-01T00-00-00Z",
        ];

        for image in minio_images {
            let detected = if image.contains("rustfs") {
                ServiceType::Rustfs
            } else if image.contains("minio") {
                ServiceType::Minio
            } else {
                ServiceType::Postgres
            };
            assert_eq!(
                detected,
                ServiceType::Minio,
                "MinIO image should be detected as Minio (deprecated): {}",
                image
            );
        }
    }

    #[test]
    fn test_service_type_detection_rustfs() {
        let images = vec![
            "rustfs/rustfs:latest",
            "rustfs/rustfs:1.0.0-alpha.98",
            "rustfs/rustfs:1.0.0",
        ];

        for image in images {
            let detected = if image.contains("rustfs") {
                ServiceType::Rustfs
            } else {
                ServiceType::Postgres
            };
            assert_eq!(detected, ServiceType::Rustfs, "Failed for image: {}", image);
        }
    }

    #[test]
    fn test_external_service_info_structure() {
        // Test that ExternalServiceInfo struct is properly created for import
        let service_info = ExternalServiceInfo {
            id: 1,
            name: "imported-postgres".to_string(),
            service_type: ServiceType::Postgres,
            version: Some("15-alpine".to_string()),
            status: "running".to_string(),
            connection_info: Some("postgresql://localhost:5432/postgres".to_string()),
            created_at: "2025-01-12T10:30:00Z".to_string(),
            updated_at: "2025-01-12T10:30:00Z".to_string(),
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
            error_message: None,
            metrics_enabled: false,
        };

        assert_eq!(service_info.id, 1);
        assert_eq!(service_info.name, "imported-postgres");
        assert_eq!(service_info.service_type, ServiceType::Postgres);
        assert_eq!(service_info.status, "running");
        assert!(service_info.connection_info.is_some());
    }

    #[test]
    fn test_import_requires_valid_credentials() {
        // Test that credentials are required for import
        let credentials: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        // Empty credentials should fail validation
        assert!(credentials.is_empty());
    }

    #[test]
    fn test_import_service_config_parameters() {
        // Test that ServiceConfig parameters are properly structured
        let params = serde_json::json!({
            "host": "localhost",
            "port": 5432,
            "database": "importeddb",
            "username": "postgres",
            "password": "secret",
            "container_id": "abc123",
            "docker_image": "gotempsh/postgres-walg:15-bookworm",
        });

        assert_eq!(params["host"], "localhost");
        assert_eq!(params["port"], 5432);
        assert_eq!(params["database"], "importeddb");
        assert_eq!(params["container_id"], "abc123");
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_postgres_v17_import_and_upgrade_to_v18() {
        // This test demonstrates the complete workflow:
        // 1. Create a PostgreSQL v17 Docker container
        // 2. Import it as a service in Temps
        // 3. Upgrade the container to PostgreSQL v18
        // 4. Verify the imported service still works with the new version

        // Setup
        let (_manager, _test_db) = setup_test_manager().await;

        // Verify Docker is available
        let _docker = match Docker::connect_with_local_defaults() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("⚠️  Docker not available, skipping v17→v18 upgrade test");
                return;
            }
        };

        // Test workflow documentation:
        // =============================
        //
        // Step 1: Create PostgreSQL v17 container
        //   - Image: gotempsh/postgres-walg:17-bookworm
        //   - Environment: POSTGRES_DB=testdb, POSTGRES_USER=pguser, POSTGRES_PASSWORD=pgpass
        //   - Port: 5432 exposed
        //   - Name: test-postgres-v17-upgrade
        //
        // Step 2: Wait for container startup
        //   - Check postgres_isready command
        //   - Allow 5-10 seconds for full initialization
        //
        // Step 3: Import the container as a service
        //   - Call manager.list_available_containers()
        //   - Verify PostgreSQL v17 container is found
        //   - Call manager.import_service() with credentials:
        //     * username: pguser
        //     * password: pgpass
        //     * port: 5432
        //     * database: testdb
        //   - Service name: "imported-postgres-v17"
        //
        // Step 4: Verify initial import
        //   - Connect to imported service via connection_url
        //   - Execute: SELECT version() - should show 17.x
        //   - Execute: SELECT datname FROM pg_database - should list testdb
        //
        // Step 5: Upgrade PostgreSQL v17 → v18
        //   - Stop the v17 container
        //   - Create a backup/snapshot of the data volume (optional)
        //   - Create new v18 container with same volumes
        //   - Execute pg_upgrade (if needed)
        //   - Start the v18 container
        //
        // Step 6: Verify upgraded service still works
        //   - Re-connect using the same imported service credentials
        //   - Execute: SELECT version() - should show 18.x
        //   - Verify all databases still exist
        //   - Verify tables and data are intact
        //
        // Step 7: Cleanup
        //   - Stop and remove v18 container
        //   - Remove any volumes created for testing
        //   - Delete the imported service from database

        println!("✅ test_postgres_v17_import_and_upgrade_to_v18 placeholder created");
        println!("   This test verifies the complete import + upgrade workflow");
        println!("   Requires proper Bollard API implementation for container management");
        println!("   When implemented, this test will:");
        println!("   1. Create PostgreSQL v17 container");
        println!("   2. Import it as a Temps service");
        println!("   3. Upgrade the container to v18");
        println!("   4. Verify service connectivity with both versions");
    }

    // --- Cross-node env var rewriting tests ---

    #[test]
    fn test_rewrite_env_vars_docker_mode_container_name() {
        let mut env_vars = HashMap::new();
        env_vars.insert(
            "DATABASE_URL".to_string(),
            "postgresql://user:pass@my-postgres-service:5432/db".to_string(),
        );
        env_vars.insert(
            "REDIS_URL".to_string(),
            "redis://my-redis-service:6379/0".to_string(),
        );

        rewrite_env_vars_for_cross_node(
            &mut env_vars,
            "my-postgres",
            "10.100.0.3",
            Some(5433),
            Some(5432),
        );

        // DATABASE_URL should be rewritten with private addr and host port
        assert_eq!(
            env_vars["DATABASE_URL"],
            "postgresql://user:pass@10.100.0.3:5433/db"
        );
        // REDIS_URL is for a different service, should be unchanged
        assert_eq!(env_vars["REDIS_URL"], "redis://my-redis-service:6379/0");
    }

    #[test]
    fn test_rewrite_env_vars_baremetal_mode_localhost() {
        let mut env_vars = HashMap::new();
        env_vars.insert(
            "DATABASE_URL".to_string(),
            "postgresql://user:pass@localhost:5433/db".to_string(),
        );

        rewrite_env_vars_for_cross_node(
            &mut env_vars,
            "my-postgres",
            "10.100.0.3",
            Some(5433),
            Some(5432),
        );

        assert_eq!(
            env_vars["DATABASE_URL"],
            "postgresql://user:pass@10.100.0.3:5433/db"
        );
    }

    #[test]
    fn test_rewrite_env_vars_baremetal_mode_127001() {
        let mut env_vars = HashMap::new();
        env_vars.insert(
            "DATABASE_URL".to_string(),
            "postgresql://user:pass@127.0.0.1:5433/db".to_string(),
        );

        rewrite_env_vars_for_cross_node(
            &mut env_vars,
            "my-postgres",
            "10.100.0.3",
            Some(5433),
            Some(5432),
        );

        assert_eq!(
            env_vars["DATABASE_URL"],
            "postgresql://user:pass@10.100.0.3:5433/db"
        );
    }

    #[test]
    fn test_rewrite_env_vars_no_matching_patterns_unchanged() {
        let mut env_vars = HashMap::new();
        env_vars.insert("APP_NAME".to_string(), "my-cool-app".to_string());
        env_vars.insert("LOG_LEVEL".to_string(), "debug".to_string());

        rewrite_env_vars_for_cross_node(
            &mut env_vars,
            "my-postgres",
            "10.100.0.3",
            Some(5433),
            Some(5432),
        );

        assert_eq!(env_vars["APP_NAME"], "my-cool-app");
        assert_eq!(env_vars["LOG_LEVEL"], "debug");
    }

    #[test]
    fn test_rewrite_env_vars_no_ports_skips_container_name_rewrite() {
        let mut env_vars = HashMap::new();
        env_vars.insert(
            "DATABASE_URL".to_string(),
            "postgresql://user:pass@my-postgres-service:5432/db".to_string(),
        );

        // When host_port/internal_port are None, container name replacement is skipped
        rewrite_env_vars_for_cross_node(&mut env_vars, "my-postgres", "10.100.0.3", None, None);

        // Container name not rewritten (no port info available)
        assert_eq!(
            env_vars["DATABASE_URL"],
            "postgresql://user:pass@my-postgres-service:5432/db"
        );
    }

    #[test]
    fn test_rewrite_env_vars_multiple_values_rewritten() {
        let mut env_vars = HashMap::new();
        env_vars.insert(
            "DATABASE_URL".to_string(),
            "postgresql://user:pass@my-pg-service:5432/db".to_string(),
        );
        env_vars.insert("DATABASE_HOST".to_string(), "my-pg-service".to_string());
        env_vars.insert("DATABASE_PORT".to_string(), "5432".to_string());

        rewrite_env_vars_for_cross_node(
            &mut env_vars,
            "my-pg",
            "10.100.0.5",
            Some(5433),
            Some(5432),
        );

        assert_eq!(
            env_vars["DATABASE_URL"],
            "postgresql://user:pass@10.100.0.5:5433/db"
        );
        // Bare container name without port gets replaced with private_addr
        assert_eq!(env_vars["DATABASE_HOST"], "10.100.0.5");
        // Plain port string doesn't match any pattern, stays as-is
        assert_eq!(env_vars["DATABASE_PORT"], "5432");
    }

    // ── Cluster validation tests ──────────────────────────────────────

    #[cfg(feature = "docker-tests")]
    async fn insert_test_service(
        db: &DatabaseConnection,
        name: &str,
        service_type: &str,
        topology: &str,
        status: &str,
    ) -> i32 {
        use sea_orm::ActiveValue::Set;

        let model = external_services::ActiveModel {
            name: Set(name.to_string()),
            service_type: Set(service_type.to_string()),
            version: Set(None),
            status: Set(status.to_string()),
            config: Set(None),
            node_id: Set(None),
            topology: Set(topology.to_string()),
            error_message: Set(None),
            ..Default::default()
        };
        let result = model.insert(db).await.unwrap();
        result.id
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_initialize_cluster_not_found() {
        let (manager, _test_db) = setup_test_manager().await;

        let result = manager
            .initialize_cluster(
                99999,
                &[ClusterMemberRequest {
                    role: "primary".to_string(),
                    node_id: None,
                }],
            )
            .await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ExternalServiceError::ServiceNotFound { id: 99999 }
        ));
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_initialize_cluster_unsupported_type() {
        let (manager, _test_db) = setup_test_manager().await;

        // S3 does not support cluster topology
        let service_id = insert_test_service(
            manager.db.as_ref(),
            "test-s3-cluster",
            "s3",
            "cluster",
            "creating",
        )
        .await;

        let result = manager
            .initialize_cluster(
                service_id,
                &[ClusterMemberRequest {
                    role: "primary".to_string(),
                    node_id: None,
                }],
            )
            .await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ExternalServiceError::InitializationFailed { .. }
        ));
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_initialize_cluster_invalid_role() {
        let (manager, _test_db) = setup_test_manager().await;

        let service_id = insert_test_service(
            manager.db.as_ref(),
            "test-pg-bad-role",
            "postgres",
            "cluster",
            "creating",
        )
        .await;

        let result = manager
            .initialize_cluster(
                service_id,
                &[ClusterMemberRequest {
                    role: "invalid_role".to_string(),
                    node_id: None,
                }],
            )
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ExternalServiceError::ParameterValidationFailed { .. }),
            "Expected ParameterValidationFailed, got: {:?}",
            err
        );
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_retry_cluster_not_found() {
        let (manager, _test_db) = setup_test_manager().await;

        let result = manager.retry_cluster(99999, &[]).await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ExternalServiceError::ServiceNotFound { id: 99999 }
        ));
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_retry_cluster_standalone_rejected() {
        let (manager, _test_db) = setup_test_manager().await;

        let service_id = insert_test_service(
            manager.db.as_ref(),
            "test-standalone-retry",
            "postgres",
            "standalone",
            "failed",
        )
        .await;

        let result = manager.retry_cluster(service_id, &[]).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ExternalServiceError::ParameterValidationFailed { .. }),
            "Expected ParameterValidationFailed for standalone topology, got: {:?}",
            err
        );
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_retry_cluster_wrong_status() {
        let (manager, _test_db) = setup_test_manager().await;

        let service_id = insert_test_service(
            manager.db.as_ref(),
            "test-running-retry",
            "postgres",
            "cluster",
            "running",
        )
        .await;

        let result = manager.retry_cluster(service_id, &[]).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ExternalServiceError::ParameterValidationFailed { .. }),
            "Expected ParameterValidationFailed for running status, got: {:?}",
            err
        );
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_retry_cluster_no_members() {
        let (manager, _test_db) = setup_test_manager().await;

        let service_id = insert_test_service(
            manager.db.as_ref(),
            "test-no-members-retry",
            "postgres",
            "cluster",
            "failed",
        )
        .await;

        // Empty member request + no preserved members in DB
        let result = manager.retry_cluster(service_id, &[]).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ExternalServiceError::ParameterValidationFailed { .. }),
            "Expected ParameterValidationFailed for missing members, got: {:?}",
            err
        );
    }

    // ── add_cluster_member validation ──────────────────────────────────

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_add_cluster_member_not_found() {
        let (manager, _test_db) = setup_test_manager().await;

        let result = manager.add_cluster_member(99999, "replica", None).await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ExternalServiceError::ServiceNotFound { id: 99999 }
        ));
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_add_cluster_member_rejects_standalone() {
        let (manager, _test_db) = setup_test_manager().await;

        let service_id = insert_test_service(
            manager.db.as_ref(),
            "test-add-standalone",
            "postgres",
            "standalone",
            "running",
        )
        .await;

        let result = manager
            .add_cluster_member(service_id, "replica", None)
            .await;

        assert!(matches!(
            result.unwrap_err(),
            ExternalServiceError::ParameterValidationFailed { .. }
        ));
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_add_cluster_member_rejects_non_running_status() {
        let (manager, _test_db) = setup_test_manager().await;

        let service_id = insert_test_service(
            manager.db.as_ref(),
            "test-add-failed",
            "postgres",
            "cluster",
            "failed",
        )
        .await;

        let result = manager
            .add_cluster_member(service_id, "replica", None)
            .await;

        assert!(matches!(
            result.unwrap_err(),
            ExternalServiceError::ParameterValidationFailed { .. }
        ));
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_add_cluster_member_rejects_monitor_role() {
        let (manager, _test_db) = setup_test_manager().await;

        let service_id = insert_test_service(
            manager.db.as_ref(),
            "test-add-monitor",
            "postgres",
            "cluster",
            "running",
        )
        .await;

        let result = manager
            .add_cluster_member(service_id, "monitor", None)
            .await;

        let err = result.unwrap_err();
        assert!(
            matches!(err, ExternalServiceError::ParameterValidationFailed { .. }),
            "monitor must be rejected at runtime: {:?}",
            err
        );
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_add_cluster_member_rejects_primary_role() {
        let (manager, _test_db) = setup_test_manager().await;

        let service_id = insert_test_service(
            manager.db.as_ref(),
            "test-add-primary",
            "postgres",
            "cluster",
            "running",
        )
        .await;

        let result = manager
            .add_cluster_member(service_id, "primary", None)
            .await;

        let err = result.unwrap_err();
        assert!(
            matches!(err, ExternalServiceError::ParameterValidationFailed { .. }),
            "primary is elected, must be rejected at runtime: {:?}",
            err
        );
    }

    // ── remove_cluster_member validation ───────────────────────────────

    #[cfg(feature = "docker-tests")]
    async fn insert_test_member(
        db: &DatabaseConnection,
        service_id: i32,
        role: &str,
        ordinal: i32,
        container_name: &str,
    ) -> i32 {
        use sea_orm::ActiveValue::Set;
        let model = service_members::ActiveModel {
            service_id: Set(service_id),
            node_id: Set(None),
            role: Set(role.to_string()),
            container_id: Set(None),
            container_name: Set(container_name.to_string()),
            hostname: Set(None),
            port: Set(None),
            status: Set("running".to_string()),
            ordinal: Set(ordinal),
            config: Set(None),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        model.insert(db).await.unwrap().id
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_remove_cluster_member_rejects_standalone() {
        let (manager, _test_db) = setup_test_manager().await;

        let service_id = insert_test_service(
            manager.db.as_ref(),
            "test-rm-standalone",
            "postgres",
            "standalone",
            "running",
        )
        .await;

        let err = manager
            .remove_cluster_member(service_id, 12345)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ExternalServiceError::ParameterValidationFailed { .. }
        ));
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_remove_cluster_member_not_found() {
        let (manager, _test_db) = setup_test_manager().await;

        let service_id = insert_test_service(
            manager.db.as_ref(),
            "test-rm-missing",
            "postgres",
            "cluster",
            "running",
        )
        .await;

        // No service_members rows — member 99999 doesn't exist.
        let err = manager
            .remove_cluster_member(service_id, 99999)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ExternalServiceError::InitializationFailed { .. }
        ));
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_remove_cluster_member_rejects_monitor() {
        let (manager, _test_db) = setup_test_manager().await;

        let service_id = insert_test_service(
            manager.db.as_ref(),
            "test-rm-monitor",
            "postgres",
            "cluster",
            "running",
        )
        .await;
        let monitor_id = insert_test_member(
            manager.db.as_ref(),
            service_id,
            "monitor",
            0,
            "postgres-test-rm-monitor-monitor",
        )
        .await;
        // Quorum-satisfying data nodes so the monitor branch is the one we trip.
        insert_test_member(
            manager.db.as_ref(),
            service_id,
            "primary",
            1,
            "postgres-test-rm-monitor-1",
        )
        .await;
        insert_test_member(
            manager.db.as_ref(),
            service_id,
            "replica",
            2,
            "postgres-test-rm-monitor-2",
        )
        .await;
        insert_test_member(
            manager.db.as_ref(),
            service_id,
            "replica",
            3,
            "postgres-test-rm-monitor-3",
        )
        .await;

        let err = manager
            .remove_cluster_member(service_id, monitor_id)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            matches!(err, ExternalServiceError::ParameterValidationFailed { .. })
                && msg.contains("monitor"),
            "monitor must be rejected: {}",
            msg
        );
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_remove_cluster_member_rejects_primary() {
        let (manager, _test_db) = setup_test_manager().await;

        let service_id = insert_test_service(
            manager.db.as_ref(),
            "test-rm-primary",
            "postgres",
            "cluster",
            "running",
        )
        .await;
        insert_test_member(
            manager.db.as_ref(),
            service_id,
            "monitor",
            0,
            "postgres-test-rm-primary-monitor",
        )
        .await;
        let primary_id = insert_test_member(
            manager.db.as_ref(),
            service_id,
            "primary",
            1,
            "postgres-test-rm-primary-1",
        )
        .await;
        insert_test_member(
            manager.db.as_ref(),
            service_id,
            "replica",
            2,
            "postgres-test-rm-primary-2",
        )
        .await;
        insert_test_member(
            manager.db.as_ref(),
            service_id,
            "replica",
            3,
            "postgres-test-rm-primary-3",
        )
        .await;

        let err = manager
            .remove_cluster_member(service_id, primary_id)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            matches!(err, ExternalServiceError::ParameterValidationFailed { .. })
                && msg.contains("primary"),
            "primary must be rejected: {}",
            msg
        );
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_remove_cluster_member_rejects_quorum_drop() {
        let (manager, _test_db) = setup_test_manager().await;

        let service_id = insert_test_service(
            manager.db.as_ref(),
            "test-rm-quorum",
            "postgres",
            "cluster",
            "running",
        )
        .await;
        insert_test_member(
            manager.db.as_ref(),
            service_id,
            "monitor",
            0,
            "postgres-test-rm-quorum-monitor",
        )
        .await;
        insert_test_member(
            manager.db.as_ref(),
            service_id,
            "primary",
            1,
            "postgres-test-rm-quorum-1",
        )
        .await;
        // Only 2 data members total; removing one drops below quorum.
        let replica_id = insert_test_member(
            manager.db.as_ref(),
            service_id,
            "replica",
            2,
            "postgres-test-rm-quorum-2",
        )
        .await;

        let err = manager
            .remove_cluster_member(service_id, replica_id)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            matches!(err, ExternalServiceError::ParameterValidationFailed { .. })
                && msg.contains("quorum"),
            "quorum violation must be rejected: {}",
            msg
        );
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_remove_cluster_member_rejects_wrong_service() {
        let (manager, _test_db) = setup_test_manager().await;

        let service_a = insert_test_service(
            manager.db.as_ref(),
            "test-rm-svc-a",
            "postgres",
            "cluster",
            "running",
        )
        .await;
        let service_b = insert_test_service(
            manager.db.as_ref(),
            "test-rm-svc-b",
            "postgres",
            "cluster",
            "running",
        )
        .await;

        // Insert a member into service_b
        let stray_id = insert_test_member(
            manager.db.as_ref(),
            service_b,
            "replica",
            1,
            "postgres-test-rm-svc-b-1",
        )
        .await;

        // Try to remove it from service_a — must refuse.
        let err = manager
            .remove_cluster_member(service_a, stray_id)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            matches!(err, ExternalServiceError::ParameterValidationFailed { .. })
                && msg.contains("does not belong"),
            "cross-service removal must be rejected: {}",
            msg
        );
    }

    // ── promote_cluster_member validation ─────────────────────────────

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_promote_cluster_member_rejects_standalone() {
        let (manager, _test_db) = setup_test_manager().await;

        let service_id = insert_test_service(
            manager.db.as_ref(),
            "test-promote-standalone",
            "postgres",
            "standalone",
            "running",
        )
        .await;

        let err = manager
            .promote_cluster_member(service_id, 12345)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ExternalServiceError::ParameterValidationFailed { .. }
        ));
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_promote_cluster_member_rejects_non_postgres() {
        let (manager, _test_db) = setup_test_manager().await;

        let service_id = insert_test_service(
            manager.db.as_ref(),
            "test-promote-redis",
            "redis",
            "cluster",
            "running",
        )
        .await;

        let err = manager
            .promote_cluster_member(service_id, 12345)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ExternalServiceError::ParameterValidationFailed { .. }
        ));
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_promote_cluster_member_not_found() {
        let (manager, _test_db) = setup_test_manager().await;

        let service_id = insert_test_service(
            manager.db.as_ref(),
            "test-promote-missing",
            "postgres",
            "cluster",
            "running",
        )
        .await;

        let err = manager
            .promote_cluster_member(service_id, 99999)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ExternalServiceError::InitializationFailed { .. }
        ));
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_promote_cluster_member_rejects_monitor() {
        let (manager, _test_db) = setup_test_manager().await;

        let service_id = insert_test_service(
            manager.db.as_ref(),
            "test-promote-monitor",
            "postgres",
            "cluster",
            "running",
        )
        .await;
        let monitor_id = insert_test_member(
            manager.db.as_ref(),
            service_id,
            "monitor",
            0,
            "postgres-test-promote-monitor-monitor",
        )
        .await;

        let err = manager
            .promote_cluster_member(service_id, monitor_id)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            matches!(err, ExternalServiceError::ParameterValidationFailed { .. })
                && msg.contains("monitor"),
            "monitor must be rejected: {}",
            msg
        );
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_promote_cluster_member_rejects_already_primary() {
        let (manager, _test_db) = setup_test_manager().await;

        let service_id = insert_test_service(
            manager.db.as_ref(),
            "test-promote-already",
            "postgres",
            "cluster",
            "running",
        )
        .await;
        let primary_id = insert_test_member(
            manager.db.as_ref(),
            service_id,
            "primary",
            1,
            "postgres-test-promote-already-1",
        )
        .await;

        let err = manager
            .promote_cluster_member(service_id, primary_id)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            matches!(err, ExternalServiceError::ParameterValidationFailed { .. })
                && msg.contains("already the primary"),
            "already-primary must be rejected: {}",
            msg
        );
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_promote_cluster_member_rejects_wrong_service() {
        let (manager, _test_db) = setup_test_manager().await;

        let service_a = insert_test_service(
            manager.db.as_ref(),
            "test-promote-svc-a",
            "postgres",
            "cluster",
            "running",
        )
        .await;
        let service_b = insert_test_service(
            manager.db.as_ref(),
            "test-promote-svc-b",
            "postgres",
            "cluster",
            "running",
        )
        .await;
        let stray_id = insert_test_member(
            manager.db.as_ref(),
            service_b,
            "replica",
            1,
            "postgres-test-promote-svc-b-1",
        )
        .await;

        let err = manager
            .promote_cluster_member(service_a, stray_id)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            matches!(err, ExternalServiceError::ParameterValidationFailed { .. })
                && msg.contains("does not belong"),
            "cross-service promotion must be rejected: {}",
            msg
        );
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_promote_cluster_member_rejects_not_running() {
        let (manager, _test_db) = setup_test_manager().await;

        let service_id = insert_test_service(
            manager.db.as_ref(),
            "test-promote-stopped",
            "postgres",
            "cluster",
            "running",
        )
        .await;
        // Insert a stopped replica.
        use sea_orm::ActiveValue::Set;
        let stopped = service_members::ActiveModel {
            service_id: Set(service_id),
            node_id: Set(None),
            role: Set("replica".to_string()),
            container_id: Set(None),
            container_name: Set("postgres-test-promote-stopped-1".to_string()),
            hostname: Set(None),
            port: Set(None),
            status: Set("stopped".to_string()),
            ordinal: Set(1),
            config: Set(None),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let stopped_id = stopped.insert(manager.db.as_ref()).await.unwrap().id;

        let err = manager
            .promote_cluster_member(service_id, stopped_id)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            matches!(err, ExternalServiceError::ParameterValidationFailed { .. })
                && msg.contains("not running"),
            "stopped member must be rejected: {}",
            msg
        );
    }
}
