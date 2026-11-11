// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What the control plane and a node agent exchange about managed services,
//! and the interface through which an agent reaches engine code.
//!
//! The engines (`temps-providers`) need a lot of the workspace: query drivers,
//! DNS, alarms and more. A worker only calls a handful of operations on them,
//! so it depends on [`NodeServiceRuntime`] here, and the binary that starts the
//! agent passes the implementation in.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use bollard::models::ContainerInspectResponse;
use bollard::Docker;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

mod mariadb;
pub use mariadb::*;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ServiceConfig {
    pub name: String,
    pub service_type: ServiceType,
    pub version: Option<String>,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ServiceType {
    Mariadb,
    Mongodb,
    Postgres,
    Redis,
    /// S3-compatible object storage (RustFS-backed by default)
    S3,
    /// RustFS S3-compatible object storage (standalone)
    Rustfs,
    /// Temps KV service (Redis-backed key-value store)
    Kv,
    /// Temps Blob service (RustFS-backed object storage)
    Blob,
    /// MinIO S3-compatible object storage (deprecated, use S3/RustFS instead)
    #[deprecated(
        note = "Use S3 (RustFS-backed) instead. MinIO is kept for backward compatibility with existing services."
    )]
    Minio,
}

impl std::fmt::Display for ServiceType {
    #[allow(deprecated)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceType::Mariadb => write!(f, "mariadb"),
            ServiceType::Mongodb => write!(f, "mongodb"),
            ServiceType::Postgres => write!(f, "postgres"),
            ServiceType::Redis => write!(f, "redis"),
            ServiceType::S3 => write!(f, "s3"),
            ServiceType::Rustfs => write!(f, "rustfs"),
            ServiceType::Kv => write!(f, "kv"),
            ServiceType::Blob => write!(f, "blob"),
            ServiceType::Minio => write!(f, "minio"),
        }
    }
}

impl ServiceType {
    #[allow(clippy::should_implement_trait)]
    #[allow(deprecated)]
    pub fn from_str(s: &str) -> Result<Self, ServiceTypeParseError> {
        match s.to_lowercase().as_str() {
            "mariadb" => Ok(ServiceType::Mariadb),
            "mongodb" => Ok(ServiceType::Mongodb),
            "postgres" => Ok(ServiceType::Postgres),
            "redis" => Ok(ServiceType::Redis),
            "s3" => Ok(ServiceType::S3),
            "rustfs" => Ok(ServiceType::Rustfs),
            "kv" => Ok(ServiceType::Kv),
            "blob" => Ok(ServiceType::Blob),
            "minio" => Ok(ServiceType::Minio),
            _ => Err(ServiceTypeParseError {
                service_type: s.to_string(),
            }),
        }
    }

    /// Returns a Vec containing all available service types
    #[allow(deprecated)]
    pub fn get_all() -> Vec<ServiceType> {
        vec![
            ServiceType::Mariadb,
            ServiceType::Mongodb,
            ServiceType::Postgres,
            ServiceType::Redis,
            ServiceType::S3,
            ServiceType::Rustfs,
            ServiceType::Kv,
            ServiceType::Blob,
            ServiceType::Minio,
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

/// A service type name no engine has.
///
/// The message lists what is accepted and does not repeat the input, which is
/// whatever the caller put in that field, a pasted connection string included,
/// and this crate has no redaction to apply to it. A caller that can redact
/// reads `service_type`.
#[derive(Debug, thiserror::Error)]
#[error(
    "Invalid service type: expected one of mariadb, mongodb, postgres, redis, s3, rustfs, kv, \
     blob, minio"
)]
pub struct ServiceTypeParseError {
    pub service_type: String,
}

/// Result of a single probe against a managed external service.
/// Returned by `ExternalService::health_probe` so the monitor can record
/// structured health history without the trait having to know about DB rows.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HealthProbeResult {
    pub status: HealthProbeStatus,
    /// Round-trip probe latency, when measurable.
    pub response_time_ms: Option<i32>,
    /// Present when status is `Degraded` or `Down`. Never contains secrets.
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HealthProbeStatus {
    Operational,
    Degraded,
    Down,
}

impl HealthProbeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Operational => "operational",
            Self::Degraded => "degraded",
            Self::Down => "down",
        }
    }
}

impl HealthProbeResult {
    pub fn operational(response_time_ms: Option<i32>) -> Self {
        Self {
            status: HealthProbeStatus::Operational,
            response_time_ms,
            error_message: None,
        }
    }

    pub fn down(message: impl Into<String>) -> Self {
        Self {
            status: HealthProbeStatus::Down,
            response_time_ms: None,
            error_message: Some(message.into()),
        }
    }

    pub fn degraded(message: impl Into<String>, response_time_ms: Option<i32>) -> Self {
        Self {
            status: HealthProbeStatus::Degraded,
            response_time_ms,
            error_message: Some(message.into()),
        }
    }
}

/// Request to initialize a service and provision one project's logical
/// resource on the node that owns the service container.
#[derive(Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct RemoteRuntimeEnvRequest {
    pub service_config: ServiceConfig,
    pub project_slug: String,
    pub environment_slug: String,
}

/// Runtime environment returned after the logical resource is provisioned.
#[derive(Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct RemoteRuntimeEnvResponse {
    pub environment: HashMap<String, String>,
}

/// Request for a provider-authenticated probe on the service's owning node.
/// The agent accepts no caller-selected command or free-form target.
#[derive(Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoteHealthProbeRequest {
    pub service_config: ServiceConfig,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct RemoteHealthProbeResponse {
    pub result: HealthProbeResult,
}

/// A database container as Docker reports it, and its image's defaults
/// (which Docker merged into the container's environment and command).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContainerSnapshot {
    pub found: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspect: Option<ContainerInspectResponse>,
    #[serde(default)]
    pub image_env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_cmd: Option<Vec<String>>,
}

/// A managed service container's lifecycle state, read from `docker inspect`
/// on the node that runs it.
///
/// Only status fields, never the container's environment or command: those
/// hold the service's credentials, and this is read on every poll of the
/// service page.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ServiceContainerRuntime {
    /// `false` when no container has this name on the node.
    pub found: bool,
    #[serde(default)]
    pub container_id: Option<String>,
    /// Docker's state: "running", "exited", "dead" and so on.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub restart_count: Option<i64>,
    #[serde(default)]
    pub oom_killed: Option<bool>,
    #[serde(default)]
    pub exit_code: Option<i64>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    /// The limits applied to the container's `HostConfig`, in Docker's
    /// units (bytes, nano-CPUs, relative shares). Absent when unlimited.
    #[serde(default)]
    pub memory_bytes: Option<i64>,
    #[serde(default)]
    pub memory_swap_bytes: Option<i64>,
    #[serde(default)]
    pub nano_cpus: Option<i64>,
    #[serde(default)]
    pub cpu_shares: Option<i64>,
    #[serde(default)]
    pub shm_size_bytes: Option<i64>,
}

impl ServiceContainerRuntime {
    /// The status fields of an inspected container. The one projection both
    /// the control plane (for its own containers) and a worker's agent use.
    pub fn from_inspect(info: &ContainerInspectResponse) -> Self {
        let state = info.state.as_ref();
        let host_config = info.host_config.as_ref();
        let positive = |value: Option<i64>| value.filter(|value| *value > 0);
        Self {
            found: true,
            container_id: info.id.clone(),
            status: state
                .and_then(|state| state.status.as_ref())
                .map(|status| format!("{status:?}").to_lowercase()),
            restart_count: info.restart_count,
            oom_killed: state.and_then(|state| state.oom_killed),
            exit_code: state.and_then(|state| state.exit_code),
            started_at: state.and_then(|state| state.started_at.clone()),
            finished_at: state.and_then(|state| state.finished_at.clone()),
            image: info.image.clone(),
            memory_bytes: positive(host_config.and_then(|config| config.memory)),
            memory_swap_bytes: positive(host_config.and_then(|config| config.memory_swap)),
            nano_cpus: positive(host_config.and_then(|config| config.nano_cpus)),
            cpu_shares: positive(host_config.and_then(|config| config.cpu_shares)),
            shm_size_bytes: positive(host_config.and_then(|config| config.shm_size)),
        }
    }
}

/// The most samples one stats request takes, and the pause between two. Two
/// samples a second apart are what a CPU percentage needs: Docker zeroes
/// `precpu_stats` on a one-shot response, so the delta has to come from two.
pub const MAX_SERVICE_STATS_SAMPLES: usize = 2;
pub const SERVICE_STATS_SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Raw one-shot `docker stats` samples of a managed service container,
/// oldest first. The caller computes CPU and memory from them, so a worker's
/// container is measured with exactly the control plane's formula.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServiceContainerStats {
    /// `false` when no container has this name on the node.
    pub found: bool,
    /// Empty when the container is missing or Docker returned no frame
    /// (a stopped container).
    #[serde(default)]
    pub samples: Vec<bollard::models::ContainerStatsResponse>,
}

fn is_container_missing(error: &bollard::errors::Error) -> bool {
    matches!(
        error,
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}

/// Take one `one_shot` stats sample of `container_name`. `Ok(None)` when
/// Docker returns no frame (a stopped container).
async fn sample_container_stats_once(
    docker: &Docker,
    container_name: &str,
) -> Result<Option<bollard::models::ContainerStatsResponse>, bollard::errors::Error> {
    use futures::StreamExt;
    let options = bollard::query_parameters::StatsOptionsBuilder::default()
        .stream(false)
        .one_shot(true)
        .build();
    match docker.stats(container_name, Some(options)).next().await {
        Some(Ok(sample)) => Ok(Some(sample)),
        Some(Err(error)) => Err(error),
        None => Ok(None),
    }
}

/// Take up to `samples` (at most [`MAX_SERVICE_STATS_SAMPLES`]) one-shot
/// stats samples of `container_name`, [`SERVICE_STATS_SAMPLE_INTERVAL`]
/// apart. A missing container is `found: false`, not an error; a sample that
/// returns no frame ends the series early.
pub async fn sample_container_stats(
    docker: &Docker,
    container_name: &str,
    samples: usize,
) -> Result<ServiceContainerStats, bollard::errors::Error> {
    let wanted = samples.clamp(1, MAX_SERVICE_STATS_SAMPLES);
    let mut taken = Vec::with_capacity(wanted);
    for index in 0..wanted {
        if index > 0 {
            tokio::time::sleep(SERVICE_STATS_SAMPLE_INTERVAL).await;
        }
        match sample_container_stats_once(docker, container_name).await {
            Ok(Some(sample)) => taken.push(sample),
            Ok(None) => break,
            Err(error) if is_container_missing(&error) => {
                return Ok(ServiceContainerStats {
                    found: false,
                    samples: Vec::new(),
                })
            }
            Err(error) => return Err(error),
        }
    }
    Ok(ServiceContainerStats {
        found: true,
        samples: taken,
    })
}

/// Request to run the Postgres WAL and archive probe on the node that owns
/// the service. Like the health probe, it names no host, port or query: the
/// node derives the target from the stored configuration and its own
/// published address, and the queries are fixed in the engine.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoteWalHealthRequest {
    pub service_config: ServiceConfig,
}

/// The probe's result. `snapshot` is the engine's `PostgresWalHealth`,
/// carried opaque because this crate does not build the engines; the control
/// plane parses it into that type. `None` when the probe could not connect
/// or a query failed, with the reason in `error`.
#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
pub struct RemoteWalHealthResponse {
    #[serde(default)]
    pub snapshot: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<String>,
}

/// The S3 bucket a worker writes a backup to or reads one from, with
/// decrypted credentials. Only ever sent inside a body sealed to the node;
/// `Debug` prints neither the secret key nor the session token.
#[derive(Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkerS3Destination {
    pub bucket: String,
    pub region: String,
    /// `None`: AWS's own endpoint for the region.
    #[serde(default)]
    pub endpoint: Option<String>,
    pub force_path_style: bool,
    pub access_key_id: String,
    pub secret_access_key: String,
    /// Present for a temporary credential (Temps Cloud managed backups).
    #[serde(default)]
    pub session_token: Option<String>,
}

impl std::fmt::Debug for WorkerS3Destination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerS3Destination")
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("endpoint", &self.endpoint)
            .field("force_path_style", &self.force_path_style)
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"[REDACTED]")
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

/// `POST /agent/services/data-backup`: back up a service this node runs into
/// `destination`, with the engine's own tool, on this node.
///
/// The agent takes no command, image or host from the caller: the engine and
/// its fixed dump command follow from `service_config.service_type`, the
/// container from the service's name, and the image from that container.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoteServiceBackupRequest {
    pub service_config: ServiceConfig,
    pub destination: WorkerS3Destination,
    /// The object key of a dump (`.../dump.archive`), or the key prefix the
    /// buckets of a storage service are mirrored under.
    pub key: String,
}

/// What a worker backup wrote.
#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
pub struct RemoteServiceBackupResponse {
    /// Bytes uploaded for a dump; `0` for a bucket mirror (the control plane
    /// measures the prefix).
    pub size_bytes: u64,
    /// The buckets a storage service's mirror copied.
    #[serde(default)]
    pub buckets: Vec<String>,
}

/// `POST /agent/services/data-restore`: restore a service this node runs, in
/// place, from a backup the worker path wrote (`location` is that backup's
/// object key or mirror prefix in `source`).
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoteServiceRestoreRequest {
    pub service_config: ServiceConfig,
    pub source: WorkerS3Destination,
    pub location: String,
    /// The caller's name for this restore (the control plane's restore run).
    /// With one, the node keeps the restore's outcome after the request
    /// ends, under [`WorkerRestoreEngine::Data`], so a caller whose request
    /// timed out or lost its connection (or that restarted) can ask for it
    /// ([`WorkerRestoreOutcomeRequest`]) instead of guessing.
    #[serde(default)]
    pub operation_id: Option<String>,
}

/// What a worker restore applied.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct RemoteServiceRestoreResponse {
    /// The buckets a storage service's restore copied back.
    #[serde(default)]
    pub buckets: Vec<String>,
}

/// A generic restore that answered applied its backup: it fails with an
/// error, never with a result.
impl From<RemoteServiceRestoreResponse> for WorkerRestoreResult {
    fn from(_: RemoteServiceRestoreResponse) -> Self {
        WorkerRestoreResult::Completed
    }
}

/// `POST /agent/services/postgres/walg-backup`: a WAL-G base backup of a
/// PostgreSQL service on the node that runs it, by the control plane's own
/// engine (ADR-099). Continuous archiving is turned on (or its settings file
/// refreshed) first, exactly as on the control plane.
///
/// Names no command, image, host or path: the container follows from the
/// service's name, the WAL-G prefix from the bucket, `bucket_path` and the
/// service's name, the commands are fixed in the engine.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PostgresWalgBackupRequest {
    pub service_id: i32,
    /// The node the service runs on, recorded on a recreated container's
    /// placement.
    pub node_id: i32,
    pub service_config: ServiceConfig,
    pub destination: WorkerS3Destination,
    /// The S3 source's base path inside its bucket; empty for none.
    #[serde(default)]
    pub bucket_path: String,
    /// The backup's id, recorded in WAL-G's sentinel to find it again.
    pub backup_uuid: String,
    /// Whether the caller confirmed restarting PostgreSQL, which turning
    /// archiving on needs while `archive_mode` is off.
    pub allow_restart: bool,
}

/// What a [`PostgresWalgBackupRequest`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PostgresWalgBackupResult {
    /// The base backup was pushed.
    Completed {
        /// PostgreSQL was restarted in a recreated container to turn
        /// archiving on; its data was verified to be the same cluster first.
        restarted: bool,
        /// The end of `wal-g backup-push`'s output, where it names the backup.
        stdout: String,
        stderr: String,
        /// `pg_current_wal_lsn()` after the backup, when it could be read.
        lsn: Option<String>,
    },
    /// Nothing was changed and retrying cannot change that: a restart was
    /// needed and not confirmed, or the recreate guard refused.
    Refused { reason: String },
    /// The backup failed; a retry may succeed.
    Failed { reason: String },
}

/// `POST /agent/services/postgres/walg-archive`: turn continuous WAL-G
/// archiving on, or point it at another S3 source, without a base backup
/// (ADR-099). The control plane's own `enable_continuous_archiving`, run on
/// the node: `walg.env` rewritten for the destination, `archive_command`
/// reading it, and, when `archive_mode` is off, the guarded recreate of the
/// running container with archiving on. Sent only for an operator's
/// explicit, audited re-point, which accepts that restart, as on the
/// control plane. No command comes from the caller.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PostgresWalgArchiveRequest {
    pub service_id: i32,
    /// The node the service runs on, recorded on a recreated container's
    /// placement.
    pub node_id: i32,
    pub service_config: ServiceConfig,
    pub destination: WorkerS3Destination,
    /// The S3 source's base path inside its bucket; empty for none.
    #[serde(default)]
    pub bucket_path: String,
}

/// What a [`PostgresWalgArchiveRequest`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PostgresWalgArchiveResult {
    /// Archiving writes to the destination.
    Completed {
        /// PostgreSQL was restarted in a recreated container to turn
        /// archiving on; its data was verified to be the same cluster first.
        restarted: bool,
    },
    /// Nothing was changed and retrying cannot change that.
    Refused { reason: String },
    /// Archiving could not be pointed at the destination; a retry may
    /// succeed.
    Failed { reason: String },
}

/// `POST /agent/services/postgres/walg-credentials`: replace the credential
/// continuous archiving reads from `walg.env` with a fresh one for the same
/// WAL-G repository (ADR-093). A worker's archiving credential is scoped to
/// the service's repository and short-lived, so the control plane refreshes
/// it on an interval. Nothing else changes: no `ALTER SYSTEM`, no restart;
/// refused when archiving is not set up on the node. No command comes from
/// the caller.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PostgresWalgCredentialsRequest {
    pub service_id: i32,
    pub service_config: ServiceConfig,
    pub destination: WorkerS3Destination,
    /// The S3 source's base path inside its bucket; empty for none.
    #[serde(default)]
    pub bucket_path: String,
}

/// What a [`PostgresWalgCredentialsRequest`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PostgresWalgCredentialsResult {
    /// `walg.env` holds the new credential.
    Refreshed,
    /// Nothing was changed: archiving is not set up on this node.
    Refused { reason: String },
    /// The file could not be written; a retry may succeed.
    Failed { reason: String },
}

/// Engine-specific recovery target for PITR.
///
/// Postgres honors all variants; Redis/Mongo/S3 will likely reject non-Time
/// variants or define their own semantics when they grow PITR support.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecoveryTarget {
    /// Recover to a specific timestamp.
    Time {
        #[schema(value_type = String, format = DateTime)]
        time: chrono::DateTime<chrono::Utc>,
    },
    /// Recover to a specific transaction id (Postgres).
    Xid { xid: String },
    /// Recover to a specific log sequence number (Postgres).
    Lsn { lsn: String },
    /// Recover to a named restore point created via `pg_create_restore_point` (Postgres).
    Name { name: String },
}

/// `POST /agent/services/postgres/walg-restore`: the control plane's WAL-G
/// restore of a worker's PostgreSQL, run on its node against its container
/// (ADR-099). The node fetches the base backup into a staging directory
/// while the server runs, swaps it in keeping the original aside, starts the
/// restored server, and keeps the result only once recovery has promoted;
/// otherwise it puts the original back. No command comes from the caller.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PostgresWalgRestoreRequest {
    pub service_id: i32,
    /// The node the service runs on, recorded on the recreated container's
    /// placement.
    pub node_id: i32,
    /// The login the restored data answers to: the target's configuration
    /// with the backup's origin login when the backup is another service's.
    pub service_config: ServiceConfig,
    /// The target's own configuration: the login of the server running now,
    /// and of the original when it is put back.
    pub live_service_config: ServiceConfig,
    /// The S3 source holding the backup.
    pub source: WorkerS3Destination,
    /// The backup's WAL-G repository, `s3://<source's bucket>/…/walg`.
    pub walg_prefix: String,
    /// The point to recover to; `None` stops at the base backup's
    /// consistency point.
    #[serde(default)]
    pub recovery_target: Option<RecoveryTarget>,
    /// The sentinel user data that selects the base backup; `None` selects
    /// the latest.
    #[serde(default)]
    pub target_user_data: Option<String>,
    /// The caller's name for this restore (the control plane's restore run).
    /// With one, the node keeps the restore's outcome after the request
    /// ends, so a caller whose request timed out or lost its connection can
    /// ask for it ([`WorkerRestoreOutcomeRequest`]) instead of guessing: a
    /// restore that began writing outlives its deadline until it has
    /// started the restored data or put the original back.
    #[serde(default)]
    pub operation_id: Option<String>,
}

/// What a [`PostgresWalgRestoreRequest`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PostgresWalgRestoreResult {
    /// The restored server finished recovery and promoted; the original data
    /// was deleted.
    Completed,
    /// Nothing was changed: the request cannot run on this service.
    Refused { reason: String },
    /// The restore failed. The original is running again unless the reason
    /// says otherwise.
    Failed { reason: String },
}

/// The engine a worker restore ran, which names its outcome record on the
/// node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkerRestoreEngine {
    PostgresWalg,
    Mongodb,
    Redis,
    Mariadb,
    /// The agent's generic restore (`/agent/services/data-restore`): a
    /// storage service's bucket mirror, or another engine's dump.
    Data,
}

impl WorkerRestoreEngine {
    /// The engine's name in messages.
    pub fn label(self) -> &'static str {
        match self {
            Self::PostgresWalg => "PostgreSQL WAL-G restore",
            Self::Mongodb => "MongoDB restore",
            Self::Redis => "Redis restore",
            Self::Mariadb => "MariaDB restore",
            Self::Data => "restore on the node",
        }
    }
}

/// `POST /agent/services/restore/outcome`: what became of the `engine`
/// restore of service `service` started with `operation_id`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkerRestoreOutcomeRequest {
    pub engine: WorkerRestoreEngine,
    /// The service's name on the node (its configuration's `name`).
    pub service: String,
    /// The `operation_id` the restore request was started with.
    pub operation_id: String,
}

/// What a worker restore did, whatever its engine: each engine's result
/// ([`PostgresWalgRestoreResult`], [`MongodbRestoreResult`],
/// [`RedisRestoreResult`], [`MariadbRestoreResult`]) has these three
/// outcomes and converts into it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WorkerRestoreResult {
    /// The restored data is live.
    Completed,
    /// Nothing was changed: the request cannot run on this service.
    Refused { reason: String },
    /// The restore failed. The original is running again unless the reason
    /// says otherwise.
    Failed { reason: String },
}

macro_rules! into_worker_restore_result {
    ($($result:ident),* $(,)?) => {$(
        impl From<$result> for WorkerRestoreResult {
            fn from(result: $result) -> Self {
                match result {
                    $result::Completed => Self::Completed,
                    $result::Refused { reason } => Self::Refused { reason },
                    $result::Failed { reason } => Self::Failed { reason },
                }
            }
        }
    )*};
}

into_worker_restore_result!(
    PostgresWalgRestoreResult,
    MongodbRestoreResult,
    RedisRestoreResult,
    MariadbRestoreResult,
);

/// Where a worker restore started with an `operation_id` is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorkerRestoreProgress {
    /// Still running. Once `writes_began`, the service was stopped or is
    /// being written: the restore runs until it has started the restored
    /// data or put the original back, whatever the deadline.
    Running { writes_began: bool },
    /// The engine ended with this outcome.
    Finished { outcome: WorkerRestoreResult },
    /// The engine ended without an outcome (it could not run, passed its
    /// deadline, or stopped unexpectedly). With `writes_began` false the
    /// service was never changed.
    Ended { writes_began: bool, reason: String },
    /// The node has no restore of this service under this id: it never
    /// received it, or its agent restarted since.
    Unknown,
}

/// `POST /agent/services/mongodb/backup`: a backup of a MongoDB service on
/// the node that runs it, by the control plane's own engine (ADR-099). The
/// control plane checks the service's image for WAL-G, as it does for a
/// local one, and names the kind of backup in `mode`: `wal-g backup-push`
/// streaming `mongodump --archive` into the service's WAL-G repository, or a
/// pinned `mongodump --archive --gzip` helper writing one archive object at
/// the key the control plane chose. `destination` is a credential scoped to
/// exactly that repository or that archive's directory (ADR-093), so the node
/// cannot pick another.
///
/// Names no command, image, host or path: the container follows from the
/// service's name, the WAL-G prefix from the bucket, `bucket_path` and the
/// service's name, and the commands are fixed in the engine. The archive key
/// is checked against the same layout before anything is written.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MongodbBackupRequest {
    pub service_id: i32,
    /// The node the service runs on, for its messages.
    pub node_id: i32,
    pub service_config: ServiceConfig,
    pub destination: WorkerS3Destination,
    /// The S3 source's base path inside its bucket; empty for none.
    #[serde(default)]
    pub bucket_path: String,
    /// The backup's id, in WAL-G's sentinel and in the archive's key.
    pub backup_uuid: String,
    /// The backup the control plane granted `destination` for.
    pub mode: MongodbBackupMode,
}

/// The kind of MongoDB backup a [`MongodbBackupRequest`] takes, chosen by
/// the control plane so that its credential can be scoped to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MongodbBackupMode {
    /// `wal-g backup-push` into the service's WAL-G repository. A container
    /// without WAL-G fails the backup (retryable: the next attempt checks the
    /// image again) instead of taking an archive it has no credential for.
    WalgStream,
    /// A mongodump archive uploaded to exactly `key`, which must be this
    /// service's and backup's archive key.
    Archive { key: String },
}

/// What a [`MongodbBackupRequest`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MongodbBackupResult {
    /// WAL-G streamed the dump into the service's WAL-G repository.
    WalgStream {
        /// The end of `wal-g backup-push`'s output.
        stdout: String,
        stderr: String,
    },
    /// The image has no WAL-G: a gzipped mongodump archive was uploaded to
    /// `key`.
    Archive { key: String, size_bytes: u64 },
    /// Nothing was changed and retrying cannot change that.
    Refused { reason: String },
    /// The backup failed; a retry may succeed.
    Failed { reason: String },
}

/// `POST /agent/services/mongodb/restore`: the control plane's in-place
/// restore of a worker's MongoDB, run on its node against its container
/// (ADR-099): a WAL-G repository (`s3://…`) is fetched by `wal-g
/// backup-fetch` inside the container, a mongodump archive (an object key) is
/// downloaded and replayed by a `mongorestore --drop` sidecar. No command
/// comes from the caller.
/// `Debug` prints neither password: only the service's name stands for its
/// configuration, and the origin's password is redacted.
#[derive(Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MongodbRestoreRequest {
    pub service_id: i32,
    /// The node the service runs on, for its messages.
    pub node_id: i32,
    /// The target's own configuration: the login of the server running now.
    pub service_config: ServiceConfig,
    /// The backup origin's password, for another service's backup: tried
    /// when the target's own is refused (an earlier restore already replaced
    /// `admin.system.users`), as on the control plane.
    #[serde(default)]
    pub origin_password: Option<String>,
    /// The S3 source holding the backup.
    pub source: WorkerS3Destination,
    /// The backup: a WAL-G repository `s3://<source's bucket>/…`, or a
    /// mongodump archive's key in the source's bucket.
    pub location: String,
    /// The caller's name for this restore (the control plane's restore run).
    /// With one, the node keeps the restore's outcome after the request
    /// ends, so a caller whose request timed out or lost its connection can
    /// ask for it ([`WorkerRestoreOutcomeRequest`]) instead of guessing: a
    /// restore that began writing outlives its deadline until it has
    /// started the restored data or put the original back.
    #[serde(default)]
    pub operation_id: Option<String>,
}

impl std::fmt::Debug for MongodbRestoreRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MongodbRestoreRequest")
            .field("service_id", &self.service_id)
            .field("node_id", &self.node_id)
            .field("service", &self.service_config.name)
            .field(
                "origin_password",
                &self.origin_password.as_ref().map(|_| "[REDACTED]"),
            )
            .field("source", &self.source)
            .field("location", &self.location)
            .field("operation_id", &self.operation_id)
            .finish()
    }
}

impl MongodbRestoreRequest {
    /// The configuration the engine restores with: the target's, with the
    /// origin's password as its `_alt_password` fallback, exactly as the
    /// control plane hands it to the provider.
    pub fn engine_config(&self) -> ServiceConfig {
        let mut config = self.service_config.clone();
        if let (Some(origin), Some(parameters)) = (
            self.origin_password.as_ref(),
            config.parameters.as_object_mut(),
        ) {
            parameters.insert(
                "_alt_password".to_string(),
                serde_json::Value::String(origin.clone()),
            );
        }
        config
    }
}

/// What a [`MongodbRestoreRequest`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MongodbRestoreResult {
    /// The backup was restored into the running service.
    Completed,
    /// Nothing was changed: the request cannot run on this service.
    Refused { reason: String },
    /// The restore failed.
    Failed { reason: String },
}

/// `POST /agent/services/redis/walg-backup`: the control plane's WAL-G stream
/// backup of a Redis service, run on the node that hosts it (ADR-099):
/// `wal-g backup-push` in the container, streaming `redis-cli --rdb` straight
/// to the S3 source. Never stops or restarts Redis.
///
/// Names no command, image, host or path: the container follows from the
/// service's name, the WAL-G prefix from the bucket, `bucket_path` and the
/// service's name, the stream commands are fixed in the engine.
/// `destination` is a credential scoped to that one repository (ADR-093).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RedisWalgBackupRequest {
    pub service_id: i32,
    pub service_config: ServiceConfig,
    pub destination: WorkerS3Destination,
    /// The S3 source's base path inside its bucket; empty for none.
    #[serde(default)]
    pub bucket_path: String,
    /// The backup's id, recorded in WAL-G's sentinel to find it again.
    pub backup_uuid: String,
}

/// What a [`RedisWalgBackupRequest`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RedisWalgBackupResult {
    /// The stream was pushed; the control plane measures the repository.
    Completed,
    /// Nothing ran and retrying cannot change that.
    Refused { reason: String },
    /// The backup failed; a retry may succeed.
    Failed { reason: String },
}

/// `POST /agent/services/redis/rdb-backup`: the control plane's logical
/// backup of a Redis service whose image has no WAL-G, run on the node that
/// hosts it: the pinned `redis-cli` helper takes an RDB snapshot over the
/// service network and its gzipped output is streamed to `key`. Never stops
/// or restarts Redis. `key` must be a `dump.rdb.gz` object under the
/// service's own `external_services/redis/<service>/` prefix, and
/// `destination` is a credential scoped to `key`'s directory alone
/// (ADR-093): the control plane chose the key, date included.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RedisRdbBackupRequest {
    pub service_id: i32,
    pub service_config: ServiceConfig,
    pub destination: WorkerS3Destination,
    pub key: String,
}

/// What a [`RedisRdbBackupRequest`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RedisRdbBackupResult {
    /// The gzipped snapshot was uploaded.
    Completed { size_bytes: u64 },
    /// Nothing ran and retrying cannot change that.
    Refused { reason: String },
    /// The backup failed; a retry may succeed.
    Failed { reason: String },
}

/// `POST /agent/services/redis/restore`: the control plane's in-place
/// restore of a Redis service, run on the node that hosts it (ADR-099):
/// a WAL-G stream backup (`location` is its `s3://<source's bucket>/…/walg`
/// repository) or a gzipped RDB snapshot (`location` is its
/// `….rdb.gz` key). The snapshot is staged on the data volume while the
/// container is stopped and replaces the data only once it was fetched
/// whole; on any failure the original data is started again as it was.
/// `source` is a read-only credential for that one repository or the
/// snapshot's directory (ADR-093).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RedisRestoreRequest {
    pub service_id: i32,
    pub service_config: ServiceConfig,
    pub source: WorkerS3Destination,
    pub location: String,
    /// The backup is another service's (or its producer is unknown): Temps'
    /// own bookkeeping keys in database 0 (`_temps:*`, which record which
    /// project owns which logical database) are put back to the target's
    /// after the restore instead of taking the backup's.
    pub keep_target_bookkeeping: bool,
    /// The caller's name for this restore (the control plane's restore run).
    /// With one, the node keeps the restore's outcome after the request
    /// ends, so a caller whose request timed out or lost its connection can
    /// ask for it ([`WorkerRestoreOutcomeRequest`]) instead of guessing: a
    /// restore that began writing outlives its deadline until it has
    /// started the restored data or put the original back.
    #[serde(default)]
    pub operation_id: Option<String>,
}

/// What a [`RedisRestoreRequest`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RedisRestoreResult {
    /// The restored data is live.
    Completed,
    /// Nothing was changed: the request cannot run on this service.
    Refused { reason: String },
    /// The restore failed. The original data is running again unless the
    /// reason says otherwise.
    Failed { reason: String },
}

/// The point after which a restore on a node owns its target until it ends.
///
/// The node bounds every data operation by a deadline, but stopping a
/// restore after it began writing to its target (a PostgreSQL restore that
/// stopped the server and moved its data aside, a `mongorestore --drop`)
/// would leave the service down or half restored. The engine calls
/// [`begin_target_writes`](Self::begin_target_writes) once, at its last
/// safe point; the runner calls [`expire`](Self::expire) when the deadline
/// fires. Exactly one of them wins: an expired restore is refused the write
/// phase and unwinds what it staged, and a restore that is writing is left
/// to start its restored data or put the original back.
#[derive(Debug)]
pub struct TargetWriteBoundary {
    target: String,
    state: std::sync::atomic::AtomicU8,
}

impl TargetWriteBoundary {
    const READING: u8 = 0;
    const WRITING: u8 = 1;
    const EXPIRED: u8 = 2;

    /// A boundary for a restore of `target` (a service or container name)
    /// that has not written to it yet.
    pub fn new(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            state: std::sync::atomic::AtomicU8::new(Self::READING),
        }
    }

    /// The service or container this restore writes to.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Engine side: enter the write phase. `false` when the deadline expired
    /// first, in which case nothing may be written to the target.
    pub fn begin_target_writes(&self) -> bool {
        use std::sync::atomic::Ordering;
        match self.state.compare_exchange(
            Self::READING,
            Self::WRITING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => true,
            Err(current) => current == Self::WRITING,
        }
    }

    /// Whether the deadline expired before the write phase began.
    pub fn is_expired(&self) -> bool {
        self.state.load(std::sync::atomic::Ordering::Acquire) == Self::EXPIRED
    }

    /// Whether the engine entered the write phase.
    pub fn writes_began(&self) -> bool {
        self.state.load(std::sync::atomic::Ordering::Acquire) == Self::WRITING
    }

    /// Runner side: the deadline fired. `true` when the restore was still
    /// only reading, and is now refused the write phase; `false` when it is
    /// already writing and must be let finish.
    pub fn expire(&self) -> bool {
        use std::sync::atomic::Ordering;
        self.state
            .compare_exchange(
                Self::READING,
                Self::EXPIRED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

/// Runs once between creating a replacement container and starting it, with
/// its id (the agent joins it to the overlay there). Shared, unlike
/// [`AfterCreate`], because an engine may recreate more than once.
pub type RecreateHook =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Where on its node an instance is attached. The only part of a database
/// request that differs by node, on purpose.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NodeAttachment {
    /// The interface the port is published on: loopback on the control
    /// plane, the private address on a worker.
    pub host_ip: String,
    pub network: Option<String>,
    /// Resolvers: the overlay's on a worker, Docker's default otherwise.
    pub dns: Option<Vec<String>>,
}

/// The body of `POST /agent/services/workload`, as the agent reads it: the
/// plan stays opaque until [`NodeServiceRuntime::admit_workload`] parses and
/// admits it. The control plane's side is
/// `temps_providers::remote_service_client::ServiceWorkloadCreateRequest`.
///
/// `Debug` prints the names of the secrets, never their values.
#[derive(Clone, Serialize, Deserialize)]
pub struct WorkloadCreateBody {
    pub service_name: String,
    pub plan: serde_json::Value,
    #[serde(default)]
    pub secrets: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(default)]
    pub replace_existing: bool,
}

impl std::fmt::Debug for WorkloadCreateBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkloadCreateBody")
            .field("service_name", &self.service_name)
            .field("plan", &self.plan)
            .field("secrets", &self.secrets.keys().collect::<Vec<_>>())
            .field("network", &self.network)
            .field("replace_existing", &self.replace_existing)
            .finish()
    }
}

/// A failed engine operation. `message` is the engine's own error text, with
/// the service, container and operation it names.
#[derive(Debug, thiserror::Error)]
pub enum NodeServiceError {
    /// The workload plan is not a plan at all.
    #[error("{message}")]
    MalformedPlan { message: String },
    /// Admission refused the plan.
    #[error("{message}")]
    Refused { container: String, message: String },
    /// The engine ran and failed.
    #[error("{message}")]
    Engine { message: String },
}

/// Runs once between creating a container and starting it, with its id.
pub type AfterCreate<'a> =
    Box<dyn FnOnce(String) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> + Send + 'a>;

/// A database workload plan that passed admission on this node.
#[async_trait]
pub trait AdmittedWorkload: Send + Sync {
    fn container_name(&self) -> &str;
    fn engine(&self) -> String;
    /// The Docker volumes the plan mounts.
    fn volumes(&self) -> Vec<String>;
    /// The image to pull. Admission refuses anything but an OCI image.
    fn image(&self) -> Option<&str>;
    fn host_port(&self) -> Option<u16>;

    /// Create the container and start it.
    async fn create(
        &self,
        docker: &Docker,
        secrets: &BTreeMap<String, String>,
        node: &NodeAttachment,
        after_create: AfterCreate<'_>,
    ) -> Result<(), NodeServiceError>;

    /// Replace the running container through the recreate guard, which keeps
    /// the original until the replacement is healthy on the same data.
    async fn replace(
        &self,
        docker: &Docker,
        service_name: &str,
        secrets: &BTreeMap<String, String>,
        node: &NodeAttachment,
        after_create: AfterCreate<'_>,
    ) -> Result<(), NodeServiceError>;
}

/// The engine operations a worker runs on the services it hosts.
#[async_trait]
pub trait NodeServiceRuntime: Send + Sync {
    /// Parse `plan` and admit it. A request is never trusted to have been
    /// admitted by its sender, so every implementation must run admission.
    fn admit_workload(
        &self,
        plan: serde_json::Value,
    ) -> Result<Box<dyn AdmittedWorkload>, NodeServiceError>;

    /// Inspect a container and its image. A missing container is
    /// `found: false`, not an error.
    async fn container_snapshot(
        &self,
        docker: &Docker,
        container_name: &str,
    ) -> Result<ContainerSnapshot, NodeServiceError>;

    /// Initialize a service and provision one project's logical resource,
    /// returning its runtime environment.
    ///
    /// `published_address` is the address this node publishes managed-service
    /// ports on. The stored host of a service is loopback, but the node binds
    /// its ports to that address only, so provisioning connects there, the
    /// same way the health and WAL probes do.
    #[allow(
        clippy::too_many_arguments,
        reason = "one argument per input of the provisioning call; a struct would only rename them"
    )]
    async fn provision_runtime_environment(
        &self,
        name: String,
        service_type: ServiceType,
        config: ServiceConfig,
        project_slug: &str,
        environment_slug: &str,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
    ) -> Result<HashMap<String, String>, NodeServiceError>;

    /// Preview one project's runtime environment of a service this node
    /// runs: what [`Self::provision_runtime_environment`] would return, with
    /// nothing created, renamed or provisioned. Refuses a service whose
    /// container this node does not run.
    #[allow(
        clippy::too_many_arguments,
        reason = "one argument per input of the provisioning call; a struct would only rename them"
    )]
    async fn preview_runtime_environment(
        &self,
        name: String,
        service_type: ServiceType,
        config: ServiceConfig,
        project_slug: &str,
        environment_slug: &str,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
    ) -> Result<HashMap<String, String>, NodeServiceError>;

    /// Revoke a project environment's managed S3 credentials.
    async fn revoke_runtime_environment(
        &self,
        name: String,
        service_type: ServiceType,
        config: ServiceConfig,
        project_slug: &str,
        environment_slug: &str,
        docker: Arc<Docker>,
    ) -> Result<(), NodeServiceError>;

    /// Run the provider's own health probe for `config`. `published_address` is
    /// where this node publishes service ports, from its own configuration and
    /// never from the request.
    async fn probe_service_health(
        &self,
        config: ServiceConfig,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
    ) -> Result<HealthProbeResult, NodeServiceError>;

    /// Run the Postgres WAL and archive probe for `config`, against the
    /// container this node runs for it. `published_address` is as for
    /// [`Self::probe_service_health`]. Refuses a service that is not
    /// Postgres, a host that is not this node and a port that is not the
    /// service container's.
    async fn probe_postgres_wal(
        &self,
        config: ServiceConfig,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
    ) -> Result<RemoteWalHealthResponse, NodeServiceError>;

    /// Back up the service `request` names, which this node runs, with the
    /// engine's own tool: a dump streamed to the destination for MongoDB,
    /// Redis and MariaDB, a bucket mirror for managed S3 storage. Never stops
    /// or restarts the service. `published_address` is as for
    /// [`Self::probe_service_health`].
    async fn backup_service(
        &self,
        request: RemoteServiceBackupRequest,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
    ) -> Result<RemoteServiceBackupResponse, NodeServiceError>;

    /// Restore the service `request` names, in place, from a backup that
    /// [`Self::backup_service`] wrote, with the engine's own restore.
    async fn restore_service(
        &self,
        request: RemoteServiceRestoreRequest,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
    ) -> Result<RemoteServiceRestoreResponse, NodeServiceError>;

    /// Run the control plane's WAL-G base backup on this node (see
    /// [`PostgresWalgBackupRequest`]). A recreate that turns archiving on
    /// rebuilds the running container's own specification, attached as
    /// `node` says, with `after_create` between create and start.
    async fn postgres_walg_backup(
        &self,
        request: PostgresWalgBackupRequest,
        docker: Arc<Docker>,
        node: NodeAttachment,
        after_create: RecreateHook,
        published_address: Option<IpAddr>,
    ) -> Result<PostgresWalgBackupResult, NodeServiceError>;

    /// Point continuous archiving at a destination on this node (see
    /// [`PostgresWalgArchiveRequest`]), recreating as for a backup.
    async fn postgres_walg_archive(
        &self,
        request: PostgresWalgArchiveRequest,
        docker: Arc<Docker>,
        node: NodeAttachment,
        after_create: RecreateHook,
        published_address: Option<IpAddr>,
    ) -> Result<PostgresWalgArchiveResult, NodeServiceError>;

    /// Refresh the credential continuous archiving reads on this node (see
    /// [`PostgresWalgCredentialsRequest`]). Never recreates or restarts.
    async fn postgres_walg_credentials(
        &self,
        request: PostgresWalgCredentialsRequest,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
    ) -> Result<PostgresWalgCredentialsResult, NodeServiceError>;

    /// Run the control plane's WAL-G restore on this node (see
    /// [`PostgresWalgRestoreRequest`]). The restored and, on failure, the
    /// original container are rebuilt from the running container's own
    /// specification, attached as `node` says. The engine passes `writes`
    /// before it stops the server, so a deadline cannot stop it mid-swap.
    async fn postgres_walg_restore(
        &self,
        request: PostgresWalgRestoreRequest,
        docker: Arc<Docker>,
        node: NodeAttachment,
        after_create: RecreateHook,
        published_address: Option<IpAddr>,
        writes: Arc<TargetWriteBoundary>,
    ) -> Result<PostgresWalgRestoreResult, NodeServiceError>;

    /// Run the control plane's MongoDB backup on this node (see
    /// [`MongodbBackupRequest`]).
    async fn mongodb_backup(
        &self,
        request: MongodbBackupRequest,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
    ) -> Result<MongodbBackupResult, NodeServiceError>;

    /// Run the control plane's in-place MongoDB restore on this node (see
    /// [`MongodbRestoreRequest`]).
    /// The engine passes `writes` before `mongorestore` runs.
    async fn mongodb_restore(
        &self,
        request: MongodbRestoreRequest,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
        writes: Arc<TargetWriteBoundary>,
    ) -> Result<MongodbRestoreResult, NodeServiceError>;

    /// Run the control plane's Redis WAL-G stream backup on this node (see
    /// [`RedisWalgBackupRequest`]).
    async fn redis_walg_backup(
        &self,
        request: RedisWalgBackupRequest,
        docker: Arc<Docker>,
    ) -> Result<RedisWalgBackupResult, NodeServiceError>;

    /// Run the control plane's Redis RDB backup on this node (see
    /// [`RedisRdbBackupRequest`]).
    async fn redis_rdb_backup(
        &self,
        request: RedisRdbBackupRequest,
        docker: Arc<Docker>,
    ) -> Result<RedisRdbBackupResult, NodeServiceError>;

    /// Run the control plane's Redis in-place restore on this node (see
    /// [`RedisRestoreRequest`]).
    /// The engine passes `writes` before it touches the container.
    async fn redis_restore(
        &self,
        request: RedisRestoreRequest,
        docker: Arc<Docker>,
        writes: Arc<TargetWriteBoundary>,
    ) -> Result<RedisRestoreResult, NodeServiceError>;

    /// Run the control plane's MariaDB physical (WAL-G) base backup on this
    /// node (see [`MariadbPhysicalBackupRequest`]).
    async fn mariadb_physical_backup(
        &self,
        request: MariadbPhysicalBackupRequest,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
    ) -> Result<MariadbPhysicalBackupResult, NodeServiceError>;

    /// Run the control plane's MariaDB logical dump on this node (see
    /// [`MariadbDumpBackupRequest`]).
    async fn mariadb_dump_backup(
        &self,
        request: MariadbDumpBackupRequest,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
    ) -> Result<MariadbDumpBackupResult, NodeServiceError>;

    /// Run the control plane's in-place MariaDB restore on this node (see
    /// [`MariadbRestoreRequest`]).
    /// The engine passes `writes` before it stops the server or applies a
    /// downloaded backup.
    async fn mariadb_restore(
        &self,
        request: MariadbRestoreRequest,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
        writes: Arc<TargetWriteBoundary>,
    ) -> Result<MariadbRestoreResult, NodeServiceError>;

    /// Run the control plane's binary-log archiver on this node (see
    /// [`MariadbBinlogArchiveRequest`]).
    async fn mariadb_binlog_archive(
        &self,
        request: MariadbBinlogArchiveRequest,
        docker: Arc<Docker>,
        published_address: Option<IpAddr>,
    ) -> Result<MariadbBinlogArchiveResult, NodeServiceError>;
}

#[cfg(test)]
mod tests {
    /// The engine entering writes and the deadline expiring race; exactly
    /// one wins, and the loser sees it.
    #[test]
    fn a_write_boundary_lets_either_the_writes_or_the_deadline_win() {
        let writing = super::TargetWriteBoundary::new("orders");
        assert!(writing.begin_target_writes());
        assert!(!writing.expire(), "a restore that is writing is let finish");
        assert!(writing.writes_began() && !writing.is_expired());
        assert!(writing.begin_target_writes(), "entering twice is harmless");

        let expired = super::TargetWriteBoundary::new("orders");
        assert!(expired.expire());
        assert!(
            !expired.begin_target_writes(),
            "an expired restore must not start writing"
        );
        assert!(expired.is_expired() && !expired.writes_began());
        assert_eq!(expired.target(), "orders");
    }

    use super::*;

    /// A MongoDB restore request printed for a log or an error shows which
    /// service and backup, never a password or the S3 secret.
    #[test]
    fn a_mongodb_restore_request_debug_prints_no_secret() {
        let request = MongodbRestoreRequest {
            service_id: 8,
            node_id: 2,
            service_config: ServiceConfig {
                name: "orders".to_string(),
                service_type: ServiceType::Mongodb,
                version: None,
                parameters: serde_json::json!({ "password": "target-secret" }),
            },
            origin_password: Some("origin-secret".to_string()),
            source: WorkerS3Destination {
                bucket: "b".to_string(),
                region: "us-east-1".to_string(),
                endpoint: None,
                force_path_style: true,
                access_key_id: "AKID".to_string(),
                secret_access_key: "s3-secret".to_string(),
                session_token: Some("token-secret".to_string()),
            },
            location: "fleet/external_services/mongodb/orders/walg".to_string(),
            operation_id: Some("restore-run-3".to_string()),
        };
        let printed = format!("{request:?}");
        for secret in [
            "target-secret",
            "origin-secret",
            "s3-secret",
            "token-secret",
        ] {
            assert!(!printed.contains(secret), "{printed}");
        }
        assert!(printed.contains("orders"), "{printed}");
    }

    #[test]
    fn service_type_names_round_trip_and_an_unknown_one_is_not_repeated() {
        for service_type in ServiceType::get_all() {
            let parsed = ServiceType::from_str(&service_type.to_string()).expect("known name");
            assert_eq!(parsed, service_type);
        }
        let input = "postgres://admin:hunter2@db.internal/app";
        let error = ServiceType::from_str(input).expect_err("unknown name");
        let message = error.to_string();
        assert!(!message.contains("hunter2"), "{message}");
        assert_eq!(error.service_type, input);
        for service_type in ServiceType::get_all() {
            assert!(
                message.contains(&service_type.to_string()),
                "the message must list {service_type}: {message}"
            );
        }
    }

    /// The agent reads what the control plane sends, field for field, and a
    /// log line of the body never carries a secret value.
    #[test]
    fn workload_body_parses_with_defaults_and_hides_secret_values() {
        let body: WorkloadCreateBody = serde_json::from_value(serde_json::json!({
            "service_name": "db",
            "plan": { "container_name": "temps-db" },
            "secrets": { "POSTGRES_PASSWORD": "hunter2" },
        }))
        .expect("parse body");
        assert!(!body.replace_existing);
        assert_eq!(body.network, None);
        let logged = format!("{body:?}");
        assert!(logged.contains("POSTGRES_PASSWORD"));
        assert!(!logged.contains("hunter2"));
    }

    /// A worker backup request's credentials never reach a log line, and the
    /// agent refuses a field it does not know (no caller-chosen command).
    #[test]
    fn worker_backup_requests_hide_credentials_and_take_no_command() {
        let request: RemoteServiceBackupRequest = serde_json::from_value(serde_json::json!({
            "service_config": {
                "name": "cache",
                "service_type": "redis",
                "version": null,
                "parameters": {}
            },
            "destination": {
                "bucket": "backups",
                "region": "us-east-1",
                "force_path_style": true,
                "access_key_id": "AKIAEXAMPLE",
                "secret_access_key": "very-secret",
                "session_token": "session-secret"
            },
            "key": "redis/cache/1/dump.rdb.gz"
        }))
        .expect("parse request");
        let logged = format!("{request:?}");
        assert!(!logged.contains("very-secret"), "{logged}");
        assert!(!logged.contains("session-secret"), "{logged}");
        assert!(logged.contains("AKIAEXAMPLE"));

        let refused = serde_json::from_value::<RemoteServiceBackupRequest>(serde_json::json!({
            "service_config": {
                "name": "cache",
                "service_type": "redis",
                "version": null,
                "parameters": {}
            },
            "destination": {
                "bucket": "backups",
                "region": "us-east-1",
                "force_path_style": true,
                "access_key_id": "a",
                "secret_access_key": "b"
            },
            "key": "k",
            "command": ["sh", "-c", "id"]
        }));
        assert!(refused.is_err());
    }

    /// The Redis operations take no command, image, path or host of the
    /// caller's, and never log a credential.
    #[test]
    fn redis_requests_take_no_command_and_hide_credentials() {
        let config = serde_json::json!({
            "name": "cache",
            "service_type": "redis",
            "version": null,
            "parameters": {"password": "redis-secret"}
        });
        let destination = serde_json::json!({
            "bucket": "backups",
            "region": "us-east-1",
            "force_path_style": true,
            "access_key_id": "AKIAEXAMPLE",
            "secret_access_key": "very-secret"
        });
        let walg = serde_json::json!({
            "service_id": 4,
            "service_config": config.clone(),
            "destination": destination.clone(),
            "backup_uuid": "u-1"
        });
        let rdb = serde_json::json!({
            "service_id": 4,
            "service_config": config.clone(),
            "destination": destination.clone(),
            "key": "external_services/redis/cache/2026/11/08/u-1/dump.rdb.gz"
        });
        let restore = serde_json::json!({
            "service_id": 4,
            "service_config": config,
            "source": destination,
            "location": "s3://backups/external_services/redis/cache/walg",
            "keep_target_bookkeeping": true
        });
        let parsed: RedisWalgBackupRequest =
            serde_json::from_value(walg.clone()).expect("a WAL-G backup request");
        assert_eq!(parsed.bucket_path, "");
        assert!(!format!("{parsed:?}").contains("very-secret"));
        serde_json::from_value::<RedisRdbBackupRequest>(rdb.clone()).expect("an RDB request");
        let parsed: RedisRestoreRequest =
            serde_json::from_value(restore.clone()).expect("a restore request");
        assert!(parsed.keep_target_bookkeeping);
        assert!(!format!("{parsed:?}").contains("very-secret"));
        for field in ["command", "image", "container", "host_path"] {
            let mut walg = walg.clone();
            walg[field] = serde_json::json!("x");
            assert!(serde_json::from_value::<RedisWalgBackupRequest>(walg).is_err());
            let mut rdb = rdb.clone();
            rdb[field] = serde_json::json!("x");
            assert!(serde_json::from_value::<RedisRdbBackupRequest>(rdb).is_err());
            let mut restore = restore.clone();
            restore[field] = serde_json::json!("x");
            assert!(serde_json::from_value::<RedisRestoreRequest>(restore).is_err());
        }
        assert_eq!(
            serde_json::to_value(RedisRestoreResult::Completed).expect("serialize"),
            serde_json::json!({"status": "completed"})
        );
    }

    #[test]
    fn health_probe_results_carry_their_status() {
        assert_eq!(
            HealthProbeResult::operational(Some(3)).status,
            HealthProbeStatus::Operational
        );
        let down = HealthProbeResult::down("refused");
        assert_eq!(down.status.as_str(), "down");
        assert_eq!(down.error_message.as_deref(), Some("refused"));
        assert_eq!(
            HealthProbeResult::degraded("slow", Some(900)).status,
            HealthProbeStatus::Degraded
        );
    }
}
