// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Request, response and report types of the external-service manager.

use super::*;
use crate::externalsvc::{HealthProbeStatus, ServiceType};
use crate::readiness::{self, ReadinessPhase, ServiceReadiness};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use temps_entities::{external_services, project_services};
use utoipa::ToSchema;

/// Outcome of copying one managed S3 service's buckets into another.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct S3MigrationReport {
    pub source_service_id: i32,
    pub target_service_id: i32,
    pub source_backend: String,
    pub target_backend: String,
    /// Buckets that were copied, by their name on the source.
    pub buckets: Vec<String>,
}

/// Controls which logical database a deployment receives through a
/// project-to-service link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseProvisioningMode {
    /// All environments in the project share one database.
    Project,
    /// Each project/environment pair receives its own database.
    #[default]
    ProjectEnvironment,
    /// Every deployment reuses the explicitly configured database name.
    Custom,
}

impl DatabaseProvisioningMode {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::ProjectEnvironment => "project_environment",
            Self::Custom => "custom",
        }
    }

    pub(super) fn from_persisted(
        value: &str,
        service_id: i32,
        project_id: i32,
    ) -> Result<Self, ExternalServiceError> {
        match value {
            "project" => Ok(Self::Project),
            "project_environment" => Ok(Self::ProjectEnvironment),
            "custom" => Ok(Self::Custom),
            _ => Err(ExternalServiceError::InvalidDatabaseProvisioning {
                service_id,
                project_id,
                reason: format!("unknown persisted mode '{value}'"),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DatabaseProvisioningConfig {
    pub mode: DatabaseProvisioningMode,
    pub custom_database_name: Option<String>,
}

impl DatabaseProvisioningConfig {
    pub(super) fn validate(
        &self,
        service_id: i32,
        project_id: i32,
        service_type: &str,
    ) -> Result<(), ExternalServiceError> {
        let is_named_database = matches!(service_type, "postgres" | "mariadb" | "mongodb");
        if !is_named_database && self != &Self::default() {
            return Err(ExternalServiceError::InvalidDatabaseProvisioning {
                service_id,
                project_id,
                reason: format!("service type '{service_type}' does not provision named databases"),
            });
        }

        match (self.mode, self.custom_database_name.as_deref()) {
            (DatabaseProvisioningMode::Custom, Some(name))
                if crate::sandbox_engines::is_reserved_sandbox_database_name(name) =>
            {
                Err(ExternalServiceError::InvalidDatabaseProvisioning {
                    service_id,
                    project_id,
                    reason: format!(
                        "custom database name '{name}' starts with '{}', which is reserved for \
                         sandbox databases",
                        crate::sandbox_engines::SANDBOX_DATABASE_PREFIX
                    ),
                })
            }
            (DatabaseProvisioningMode::Custom, Some(name))
                if is_valid_custom_database_name(name) =>
            {
                Ok(())
            }
            (DatabaseProvisioningMode::Custom, Some(name)) => {
                Err(ExternalServiceError::InvalidDatabaseProvisioning {
                    service_id,
                    project_id,
                    reason: format!(
                        "custom database name '{name}' must match [a-z_][a-z0-9_]{{0,62}}"
                    ),
                })
            }
            (DatabaseProvisioningMode::Custom, None) => {
                Err(ExternalServiceError::InvalidDatabaseProvisioning {
                    service_id,
                    project_id,
                    reason: "custom mode requires custom_database_name".to_string(),
                })
            }
            (_, Some(_)) => Err(ExternalServiceError::InvalidDatabaseProvisioning {
                service_id,
                project_id,
                reason: "custom_database_name is only valid in custom mode".to_string(),
            }),
            (_, None) => Ok(()),
        }
    }

    pub(super) fn runtime_scope(
        &self,
        project_slug: &str,
        environment_slug: &str,
        service_id: i32,
        project_id: i32,
    ) -> Result<(String, String), ExternalServiceError> {
        match self.mode {
            DatabaseProvisioningMode::Project => Ok((project_slug.to_string(), String::new())),
            DatabaseProvisioningMode::ProjectEnvironment => {
                Ok((project_slug.to_string(), environment_slug.to_string()))
            }
            DatabaseProvisioningMode::Custom => self
                .custom_database_name
                .clone()
                .map(|name| (name, String::new()))
                .ok_or_else(|| ExternalServiceError::InvalidDatabaseProvisioning {
                    service_id,
                    project_id,
                    reason: "custom mode has no persisted custom database name".to_string(),
                }),
        }
    }

    pub(super) fn from_link(link: &project_services::Model) -> Result<Self, ExternalServiceError> {
        Ok(Self {
            mode: DatabaseProvisioningMode::from_persisted(
                &link.database_provisioning_mode,
                link.service_id,
                link.project_id,
            )?,
            custom_database_name: link.custom_database_name.clone(),
        })
    }
}

pub(super) fn is_valid_custom_database_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=63).contains(&bytes.len())
        && matches!(bytes[0], b'a'..=b'z' | b'_')
        && bytes[1..]
            .iter()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalServiceProjectScope {
    pub service_id: i32,
    pub project_ids: Vec<i32>,
    pub created_by_user_id: Option<i32>,
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
    /// Cluster member specifications for topology "cluster". Optional: when
    /// omitted the cluster gets a monitor, a primary and a replica, all on the
    /// control plane. When given it must hold exactly one monitor and at least
    /// one primary or replica. Each member specifies a role and a target node.
    #[serde(default)]
    pub members: Vec<ClusterMemberRequest>,
}

pub(super) fn default_topology() -> String {
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
    pub sensitive_parameters: Vec<String>,
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
    /// S3 source ID that this service's continuous archiving (Postgres/
    /// Timescale WAL-G `archive_command`, or MariaDB's binlog shipper)
    /// currently writes to. `None` for service types with no continuous
    /// archiving concept, or a Postgres/MariaDB service that has never had
    /// one provisioned. See `repoint_continuous_archive_source`.
    pub continuous_archive_s3_source_id: Option<i32>,
    /// When `continuous_archive_s3_source_id` was last set. `None` alongside
    /// a `Some` source id means it was set by the original provisioning
    /// flow rather than an explicit repoint.
    pub continuous_archive_pinned_at: Option<String>,
    /// Startup readiness while the service is `starting`, or why it failed
    /// to initialize when it is `failed`. `None` otherwise, and for engines
    /// without a readiness gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<ServiceReadiness>,
}

/// The readiness snapshot worth reporting for a service in `status`: the
/// live one while it is starting, the failure once it has failed. A
/// snapshot left over from an earlier start is never reported.
pub(crate) fn reported_readiness(
    status: &str,
    health_metadata: Option<&serde_json::Value>,
) -> Option<ServiceReadiness> {
    let readiness = ServiceReadiness::from_health_metadata(health_metadata)?;
    let current = match status {
        readiness::STARTING_STATUS => readiness.phase == ReadinessPhase::Starting,
        "failed" => readiness.phase == ReadinessPhase::Failed,
        _ => false,
    };
    current.then_some(readiness)
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
    pub(super) fn down(reason: String) -> Self {
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
    /// Data members Temps runs for this cluster beyond the primary, whether
    /// or not they have registered with the monitor yet. Tells "a standby is
    /// still being set up" apart from "the cluster has no standby".
    pub configured_standbys: usize,
}

/// Whether a cluster could fail over if its primary failed right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClusterProtectionState {
    /// At least one healthy, promotable standby is `secondary`.
    Protected,
    /// No standby could take over: none is configured, the first one is
    /// still being cloned (`wait_standby`/`catchingup`), or all are unhealthy.
    /// The cluster accepts writes, but a primary failure would be an outage.
    NotProtected,
    /// The monitor could not be read, so it is not known.
    Unknown,
}

/// [`ClusterProtectionState`] plus the sentence that explains it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClusterProtection {
    pub state: ClusterProtectionState,
    /// Names the standby that protects the cluster, or what is missing and
    /// whether it is expected to resolve on its own.
    pub detail: String,
}

impl ClusterHealthReport {
    /// Whether a standby could take over now, from the monitor's view.
    ///
    /// A standby protects the cluster only once pg_auto_failover reports it
    /// `secondary` (and wants it there), healthy, and promotable
    /// (`candidate_priority > 0`). Until then a new cluster accepts SQL and
    /// lists every member as running, yet a primary failure cannot fail over,
    /// which is what this tells the operator.
    pub fn protection(&self) -> ClusterProtection {
        use crate::externalsvc::cluster_role::PgAutoFailoverState;

        if let Some(error) = &self.monitor_error {
            return ClusterProtection {
                state: ClusterProtectionState::Unknown,
                detail: format!(
                    "Failover protection is unknown: the monitor could not be read ({error})"
                ),
            };
        }
        let state_of = |m: &ClusterMemberHealth| {
            m.reported_state
                .parse::<PgAutoFailoverState>()
                .unwrap_or(PgAutoFailoverState::Other("(other)"))
        };
        // A member the monitor is turning into the primary (`init` → `single`
        // while the cluster starts) is not a standby, whatever it reports.
        let becoming_primary = |m: &ClusterMemberHealth| {
            state_of(m).is_primary()
                || m.goal_state
                    .parse::<PgAutoFailoverState>()
                    .is_ok_and(|goal| goal.is_primary())
        };
        let standbys: Vec<&ClusterMemberHealth> = self
            .members
            .iter()
            .filter(|m| !becoming_primary(m))
            .collect();
        let ready: Vec<&str> = standbys
            .iter()
            .filter(|m| {
                state_of(m) == PgAutoFailoverState::Secondary
                    && m.goal_state == "secondary"
                    && m.health == 1
                    && m.candidate_priority > 0
            })
            .map(|m| m.nodename.as_str())
            .collect();
        if !ready.is_empty() {
            return ClusterProtection {
                state: ClusterProtectionState::Protected,
                detail: if ready.len() == 1 {
                    format!(
                        "Protected: standby {} is in sync and can take over if the primary fails",
                        ready[0]
                    )
                } else {
                    format!(
                        "Protected: standbys {} are in sync and can take over if the primary fails",
                        ready.join(", ")
                    )
                },
            };
        }
        if standbys.is_empty() {
            return ClusterProtection {
                state: ClusterProtectionState::NotProtected,
                detail: if self.configured_standbys > 0 {
                    format!(
                        "Not yet protected: {} standby member(s) are still being set up and \
                         have not registered with the monitor. The cluster cannot fail over \
                         until a standby reaches secondary",
                        self.configured_standbys
                    )
                } else {
                    "Not protected: the cluster has no standby, so a primary failure \
                     cannot fail over. Add a replica member"
                        .to_string()
                },
            };
        }
        let cloning: Vec<String> = standbys
            .iter()
            .filter(|m| {
                matches!(m.reported_state.as_str(), "wait_standby" | "catchingup")
                    || (m.reported_state == "init" && m.goal_state != "init")
            })
            .map(|m| format!("{} ({})", m.nodename, m.reported_state))
            .collect();
        if !cloning.is_empty() {
            return ClusterProtection {
                state: ClusterProtectionState::NotProtected,
                detail: format!(
                    "Not yet protected: standby {} is still copying or catching up with \
                     the primary. The cluster accepts writes, but cannot fail over until a \
                     standby reaches secondary",
                    cloning.join(", ")
                ),
            };
        }
        ClusterProtection {
            state: ClusterProtectionState::NotProtected,
            detail: format!(
                "Not protected: no standby is a healthy, promotable secondary ({})",
                standbys
                    .iter()
                    .map(|m| format!(
                        "{}: {} → {}, health {}, candidate priority {}",
                        m.nodename, m.reported_state, m.goal_state, m.health, m.candidate_priority
                    ))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        }
    }
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
    /// Most recent provisioning phase, written by initial cluster creation
    /// and by the add-member task. See `member_provisioning_step` for the
    /// canonical names. NULL for members created before steps were recorded.
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
    pub database_provisioning_mode: DatabaseProvisioningMode,
    pub custom_database_name: Option<String>,
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
    /// The still-open (firing or acknowledged) down alarm for this service.
    /// Only looked up once the failure streak reaches the alert threshold.
    pub down_alarm: Option<ServiceDownAlarm>,
}

/// The open `external_service_down` alarm raised by the health monitor.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ServiceDownAlarm {
    pub alarm_id: i32,
    pub status: String,
    pub fired_at: chrono::DateTime<chrono::Utc>,
    pub silenced_until: Option<chrono::DateTime<chrono::Utc>>,
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
    pub resource_limits: crate::externalsvc::ServiceResourceLimits,
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
    /// Set when a new `/dev/shm` size was saved but could not be applied,
    /// because that needs a new container and replacing it was refused or
    /// failed (the reason says which, and that no data was changed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shm_not_applied: Option<String>,
}

/// A managed service's own container log ingestion budget. `None` inherits
/// the platform budget (Settings → Monitoring → Container log ingestion);
/// `Some(0)` lifts that limit for this service's containers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ServiceLogIngestion {
    /// Lines per second per container (0–1,000,000). null = platform default.
    #[schema(minimum = 0, maximum = 1000000, example = 2000)]
    #[serde(default)]
    pub log_ingest_lines_per_sec: Option<i32>,
    /// KiB of log text per second per container (0–1,048,576). null =
    /// platform default.
    #[schema(minimum = 0, maximum = 1048576, example = 4096)]
    #[serde(default)]
    pub log_ingest_kib_per_sec: Option<i32>,
}

impl ServiceLogIngestion {
    pub fn of(service: &external_services::Model) -> Self {
        Self {
            log_ingest_lines_per_sec: service.log_ingest_lines_per_sec,
            log_ingest_kib_per_sec: service.log_ingest_kib_per_sec,
        }
    }
}

/// What [`ExternalServiceManager::set_log_ingestion`] changed, read under the
/// row lock that carried the write so the audit trail pairs a real before
/// with a real after.
#[derive(Debug, Clone)]
pub struct ServiceLogIngestionUpdate {
    pub service_id: i32,
    pub service_name: String,
    pub previous: ServiceLogIngestion,
    pub current: ServiceLogIngestion,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::externalsvc::ServiceType;
    use crate::readiness::{ReadinessPhase, ServiceReadiness};
    use crate::services::test_support::*;
    use chrono::Utc;

    #[test]
    fn readiness_is_reported_only_for_the_phase_the_status_is_in() {
        use crate::readiness::{
            with_readiness_metadata, InitializationFailure, InitializationFailureKind,
            ReadinessPolicy,
        };
        let policy = ReadinessPolicy {
            deadline: std::time::Duration::from_secs(180),
            poll_interval: std::time::Duration::from_secs(3),
            max_restarts: 3,
        };
        let starting = ServiceReadiness::starting(Utc::now(), &policy);
        let attempt = crate::readiness::ReadinessAttempt::new();
        let starting_meta = with_readiness_metadata(None, Some((&starting, &attempt)));
        let mut failed = starting.clone();
        failed.phase = ReadinessPhase::Failed;
        failed.failure = Some(InitializationFailure {
            kind: InitializationFailureKind::Timeout,
            reason: "Initialization did not finish within 180s".to_string(),
            log_excerpt: vec![],
            next_actions: vec![],
        });
        let failed_meta = with_readiness_metadata(None, Some((&failed, &attempt)));

        assert_eq!(
            reported_readiness("starting", starting_meta.as_ref()),
            Some(starting.clone())
        );
        assert_eq!(
            reported_readiness("failed", failed_meta.as_ref()),
            Some(failed)
        );
        // A running service, or a snapshot from an earlier phase, reports nothing.
        assert_eq!(reported_readiness("running", starting_meta.as_ref()), None);
        assert_eq!(reported_readiness("stopped", failed_meta.as_ref()), None);
        assert_eq!(reported_readiness("failed", starting_meta.as_ref()), None);
        assert_eq!(reported_readiness("starting", None), None);
    }

    #[test]
    fn a_custom_database_name_in_the_sandbox_namespace_is_refused() {
        let config = |name: &str| DatabaseProvisioningConfig {
            mode: DatabaseProvisioningMode::Custom,
            custom_database_name: Some(name.to_string()),
        };
        let error = config("_sandbox_shop")
            .validate(71, 10, "postgres")
            .expect_err("the sandbox namespace is reserved");
        match error {
            ExternalServiceError::InvalidDatabaseProvisioning {
                service_id: 71,
                project_id: 10,
                reason,
            } => assert!(reason.contains("reserved for sandbox"), "{reason}"),
            other => panic!("expected InvalidDatabaseProvisioning, got {other:?}"),
        }
        // Names merely mentioning a sandbox are still allowed.
        for name in ["shop_sandbox", "sandbox_shop", "_shop"] {
            config(name)
                .validate(71, 10, "postgres")
                .unwrap_or_else(|error| panic!("{name}: {error}"));
        }
    }

    #[test]
    fn database_provisioning_modes_resolve_expected_runtime_scopes() {
        let per_environment = DatabaseProvisioningConfig::default()
            .runtime_scope("storefront", "preview", 7, 11)
            .expect("default scope");
        assert_eq!(per_environment, ("storefront".into(), "preview".into()));

        let per_project = DatabaseProvisioningConfig {
            mode: DatabaseProvisioningMode::Project,
            custom_database_name: None,
        }
        .runtime_scope("storefront", "preview", 7, 11)
        .expect("project scope");
        assert_eq!(per_project, ("storefront".into(), String::new()));

        let custom = DatabaseProvisioningConfig {
            mode: DatabaseProvisioningMode::Custom,
            custom_database_name: Some("shared_catalog".to_string()),
        }
        .runtime_scope("storefront", "preview", 7, 11)
        .expect("custom scope");
        assert_eq!(custom, ("shared_catalog".into(), String::new()));
    }

    #[test]
    fn custom_database_provisioning_requires_a_safe_exact_name() {
        let valid = DatabaseProvisioningConfig {
            mode: DatabaseProvisioningMode::Custom,
            custom_database_name: Some("shared_catalog_2".to_string()),
        };
        assert!(valid.validate(7, 11, "postgres").is_ok());

        for name in ["", "SharedCatalog", "2catalog", "shared-catalog"] {
            let invalid = DatabaseProvisioningConfig {
                mode: DatabaseProvisioningMode::Custom,
                custom_database_name: Some(name.to_string()),
            };
            assert!(matches!(
                invalid.validate(7, 11, "postgres"),
                Err(ExternalServiceError::InvalidDatabaseProvisioning { .. })
            ));
        }
    }

    #[test]
    fn non_database_services_reject_non_default_database_provisioning() {
        let custom = DatabaseProvisioningConfig {
            mode: DatabaseProvisioningMode::Custom,
            custom_database_name: Some("shared_cache".to_string()),
        };
        assert!(matches!(
            custom.validate(7, 11, "redis"),
            Err(ExternalServiceError::InvalidDatabaseProvisioning { .. })
        ));
    }

    // ── Cluster write availability ──────────────────────────────────────

    /// The window from the issue: SQL is accepted and every member runs, but
    /// the only standby is still taking its base backup.
    #[test]
    fn a_new_cluster_is_not_yet_protected_while_its_standby_is_cloning() {
        for standby_state in ["wait_standby", "catchingup"] {
            let mut standby = cluster_member_health("app-ha-2", standby_state);
            standby.goal_state = "secondary".to_string();
            standby.health = -1;
            let report = health_report(vec![
                cluster_member_health("app-ha-1", "wait_primary"),
                standby,
            ]);
            let protection = report.protection();
            assert_eq!(protection.state, ClusterProtectionState::NotProtected);
            assert!(
                protection.detail.starts_with("Not yet protected")
                    && protection.detail.contains("app-ha-2")
                    && protection.detail.contains(standby_state),
                "{}",
                protection.detail
            );
        }
    }

    #[test]
    fn a_healthy_promotable_secondary_protects_the_cluster() {
        let report = health_report(vec![
            cluster_member_health("app-ha-1", "primary"),
            cluster_member_health("app-ha-2", "secondary"),
        ]);
        let protection = report.protection();
        assert_eq!(protection.state, ClusterProtectionState::Protected);
        assert!(
            protection.detail.contains("app-ha-2"),
            "{}",
            protection.detail
        );
    }

    #[test]
    fn a_secondary_that_cannot_take_over_does_not_protect_the_cluster() {
        let mut never_promoted = cluster_member_health("app-ha-2", "secondary");
        never_promoted.candidate_priority = 0;
        let mut unhealthy = cluster_member_health("app-ha-3", "secondary");
        unhealthy.health = 0;
        let mut leaving = cluster_member_health("app-ha-4", "secondary");
        leaving.goal_state = "draining".to_string();
        let report = health_report(vec![
            cluster_member_health("app-ha-1", "primary"),
            never_promoted,
            unhealthy,
            leaving,
        ]);
        let protection = report.protection();
        assert_eq!(protection.state, ClusterProtectionState::NotProtected);
        for name in ["app-ha-2", "app-ha-3", "app-ha-4"] {
            assert!(protection.detail.contains(name), "{}", protection.detail);
        }
    }

    /// Before the standby registers with the monitor, only the primary is
    /// listed; the member being created must not read as "no standby".
    #[test]
    fn a_standby_still_being_created_is_not_yet_protected_not_missing() {
        let mut report = health_report(vec![cluster_member_health("app-ha-1", "single")]);
        report.configured_standbys = 1;
        let protection = report.protection();
        assert_eq!(protection.state, ClusterProtectionState::NotProtected);
        assert!(
            protection
                .detail
                .starts_with("Not yet protected: 1 standby member(s)"),
            "{}",
            protection.detail
        );
    }

    /// Seen live: the first member registers as `init` with goal `single`
    /// while it becomes the primary. It must not be described as a standby.
    #[test]
    fn the_member_becoming_primary_is_not_reported_as_a_cloning_standby() {
        let mut starting = cluster_member_health("app-ha-1", "init");
        starting.goal_state = "single".to_string();
        let mut report = health_report(vec![starting]);
        report.configured_standbys = 1;
        let protection = report.protection();
        assert_eq!(protection.state, ClusterProtectionState::NotProtected);
        assert!(
            protection
                .detail
                .starts_with("Not yet protected: 1 standby member(s)")
                && !protection.detail.contains("app-ha-1"),
            "{}",
            protection.detail
        );
    }

    #[test]
    fn a_cluster_without_a_standby_or_monitor_says_so() {
        let alone = health_report(vec![cluster_member_health("app-ha-1", "single")]);
        let protection = alone.protection();
        assert_eq!(protection.state, ClusterProtectionState::NotProtected);
        assert!(
            protection.detail.contains("no standby"),
            "{}",
            protection.detail
        );

        let mut unreadable = health_report(vec![]);
        unreadable.monitor_error = Some("connection refused".to_string());
        let protection = unreadable.protection();
        assert_eq!(protection.state, ClusterProtectionState::Unknown);
        assert!(protection.detail.contains("connection refused"));
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
            continuous_archive_s3_source_id: None,
            continuous_archive_pinned_at: None,
            readiness: None,
        };

        assert_eq!(service_info.id, 1);
        assert_eq!(service_info.name, "imported-postgres");
        assert_eq!(service_info.service_type, ServiceType::Postgres);
        assert_eq!(service_info.status, "running");
        assert!(service_info.connection_info.is_some());
    }
}
