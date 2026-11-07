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
}

/// What a worker restore applied.
#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
pub struct RemoteServiceRestoreResponse {
    /// The buckets a storage service's restore copied back.
    #[serde(default)]
    pub buckets: Vec<String>,
}

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
}

#[cfg(test)]
mod tests {
    use super::*;

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
