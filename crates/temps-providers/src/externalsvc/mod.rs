use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::ToSchema;

pub mod cluster_role;
pub mod exec_util;
pub mod managed_s3;
pub mod mariadb;
pub mod mariadb_binlog_health;
pub mod mongodb;
pub mod port_util;
pub mod postgres;
pub mod postgres_cluster;
pub mod postgres_role_reconciler;
pub mod postgres_upgrade;
pub mod postgres_wal_health;
pub mod redis;
pub mod rustfs;
pub mod s3;
pub mod s3_util;

// Test utilities for backup and restore testing
#[cfg(test)]
pub mod test_utils;

// Integration tests for service clusters
#[cfg(test)]
mod cluster_integration_tests;

/// Shared mutex for tests that mutate the DEPLOYMENT_MODE environment variable.
/// This must be shared across all test modules (postgres, redis, etc.) because
/// env vars are process-global — a module-local mutex doesn't prevent cross-module races.
#[cfg(test)]
pub(crate) static DEPLOYMENT_MODE_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

// Re-export services for easier access
pub use cluster_role::{ClusterRole, PgAutoFailoverState};
pub use managed_s3::{ManagedS3Backend, ManagedS3BackendKind, ManagedS3BackendSelection};
pub use mariadb::{BinlogManifest, MariaDbService};
pub use mongodb::MongodbService;
pub use postgres::PostgresService;
pub use postgres_cluster::PostgresClusterService;
pub use redis::RedisService;
pub use rustfs::RustfsService;
pub use s3::S3Service;

/// Result of a successful `backup_to_s3` call.
///
/// Engines must always return the final S3 location. They should also return
/// `size_bytes` whenever it can be determined cheaply (e.g., a known temp
/// file's length). When the engine can't compute size locally — for example
/// WAL-G, which streams chunks straight to S3 — it returns `None` and the
/// service-layer orchestrator falls back to listing the S3 prefix.
#[derive(Debug, Clone)]
pub struct BackupOutcome {
    /// Where the backup landed (S3 URL or relative key, engine-specific).
    pub location: String,
    /// Size of the backup in bytes if the engine can determine it without
    /// a separate S3 list. `None` means "ask S3".
    pub size_bytes: Option<i64>,
}

impl BackupOutcome {
    pub fn new(location: impl Into<String>, size_bytes: Option<i64>) -> Self {
        Self {
            location: location.into(),
            size_bytes,
        }
    }
}

/// Decrypted S3 credentials for services that need to pass them to external tools
/// (e.g., WAL-G running inside a Docker container via `docker exec`).
/// The `backup_to_s3` orchestrator decrypts the encrypted credentials from the
/// `s3_sources` model and passes them through this struct.
#[derive(Debug, Clone)]
pub struct S3Credentials {
    pub access_key_id: String,
    pub secret_key: String,
    pub region: String,
    pub endpoint: Option<String>,
    pub bucket_name: String,
    pub bucket_path: String,
    pub force_path_style: bool,
}

impl S3Credentials {
    /// Build an `aws_sdk_s3::Client` from already-decrypted credentials.
    /// Used by post-backup steps (e.g. listing the WAL-G prefix to compute
    /// size) when we already hold a decrypted credential set and don't
    /// want to round-trip back through the encryption service.
    pub async fn build_s3_client(&self) -> aws_sdk_s3::Client {
        let creds = aws_sdk_s3::config::Credentials::new(
            self.access_key_id.clone(),
            self.secret_key.clone(),
            None,
            None,
            "temps-backup",
        );

        let mut config_builder = aws_sdk_s3::config::Config::builder()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new(self.region.clone()))
            .force_path_style(self.force_path_style)
            .credentials_provider(creds);

        if let Some(endpoint) = &self.endpoint {
            let endpoint_url = if endpoint.starts_with("http") {
                endpoint.clone()
            } else {
                format!("http://{}", endpoint)
            };
            config_builder = config_builder.endpoint_url(endpoint_url);
        }

        aws_sdk_s3::Client::from_conf(config_builder.build())
    }

    /// Resolve the S3 endpoint for use inside a Docker container.
    ///
    /// When WAL-G runs inside a Docker container via `docker exec`, `localhost` in the
    /// S3 endpoint refers to the container itself, not the host machine. This method
    /// detects `localhost`/`127.0.0.1` endpoints and resolves them to a Docker-accessible
    /// address by:
    ///
    /// 1. Finding a running container on the target network that exposes the S3 port
    /// 2. Falling back to `host.docker.internal` if no container is found on the network
    ///
    /// For non-localhost endpoints (e.g., `https://s3.amazonaws.com`), the endpoint is
    /// returned unchanged since the container can reach external addresses directly.
    pub async fn resolve_endpoint_for_container(
        &self,
        docker: &bollard::Docker,
        container_name: &str,
    ) -> Option<String> {
        let endpoint = self.endpoint.as_ref()?;

        // Parse the endpoint URL to extract host and port
        let url = if endpoint.starts_with("http") {
            endpoint.clone()
        } else {
            format!("http://{}", endpoint)
        };

        // Check if the host is localhost/127.0.0.1
        let is_localhost = url.contains("://localhost") || url.contains("://127.0.0.1");
        if !is_localhost {
            // External endpoint (e.g., s3.amazonaws.com) — usable as-is from the container
            return Some(endpoint.clone());
        }

        // Extract port from the endpoint URL
        let port: Option<u16> = url.split("://").nth(1).and_then(|host_port| {
            host_port
                .split('/')
                .next()
                .and_then(|hp| hp.rsplit(':').next())
                .and_then(|p| p.parse().ok())
        });

        // Determine which Docker network the target container is on
        let target_network = match docker
            .inspect_container(
                container_name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(info) => info
                .network_settings
                .and_then(|ns| ns.networks)
                .and_then(|nets| nets.into_keys().next()),
            Err(_) => None,
        };

        // Search for an S3/MinIO container on the same network that exposes the matching port
        if let (Some(port), Some(network)) = (port, &target_network) {
            if let Ok(containers) = docker
                .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                    all: false, // only running containers
                    ..Default::default()
                }))
                .await
            {
                for container in &containers {
                    // Skip the target container itself
                    let names = container.names.as_deref().unwrap_or(&[]);
                    let container_name_clean = names
                        .first()
                        .map(|n| n.trim_start_matches('/'))
                        .unwrap_or("");
                    if container_name_clean == container_name {
                        continue;
                    }

                    // Check if this container is on the same network
                    let on_same_network = container
                        .network_settings
                        .as_ref()
                        .and_then(|ns| ns.networks.as_ref())
                        .is_some_and(|nets| nets.contains_key(network.as_str()));

                    if !on_same_network {
                        continue;
                    }

                    // Check if this container exposes the matching host port
                    let has_matching_port = container
                        .ports
                        .as_ref()
                        .is_some_and(|ports| ports.iter().any(|p| p.public_port == Some(port)));

                    if has_matching_port {
                        // Found the S3 container — use its internal port
                        let internal_port = container
                            .ports
                            .as_ref()
                            .and_then(|ports| {
                                ports
                                    .iter()
                                    .find(|p| p.public_port == Some(port))
                                    .map(|p| p.private_port)
                            })
                            .unwrap_or(port);

                        let scheme = if url.starts_with("https") {
                            "https"
                        } else {
                            "http"
                        };
                        let resolved =
                            format!("{}://{}:{}", scheme, container_name_clean, internal_port);
                        tracing::info!(
                            "Resolved S3 endpoint '{}' -> '{}' (container '{}' on network '{}')",
                            endpoint,
                            resolved,
                            container_name_clean,
                            network
                        );
                        return Some(resolved);
                    }
                }
            }
        }

        // Fallback: use host.docker.internal (works on Docker Desktop for macOS/Windows,
        // and on Linux with --add-host=host.docker.internal:host-gateway)
        let resolved = url
            .replace("://localhost", "://host.docker.internal")
            .replace("://127.0.0.1", "://host.docker.internal");
        tracing::info!(
            "Resolved S3 endpoint '{}' -> '{}' (fallback to host.docker.internal)",
            endpoint,
            resolved
        );
        Some(resolved)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub name: String,
    pub service_type: ServiceType,
    pub version: Option<String>,
    pub parameters: serde_json::Value,
}

/// Optional cgroup resource limits applied to a service container.
///
/// All fields are `Option<i64>`: `None` means "no limit" (the kernel default),
/// matching Docker's behavior when the corresponding `HostConfig` field is
/// left at zero. Operators opt in to limits explicitly through the
/// `PATCH /external-services/{id}/resources` endpoint or by writing the
/// `resources` block into `ServiceConfig::parameters` at create time.
///
/// These map directly onto bollard fields:
/// - `memory_mb`     → `HostConfig.memory`        (bytes)
/// - `memory_swap_mb`→ `HostConfig.memory_swap`   (bytes; ≥ memory)
/// - `nano_cpus`     → `HostConfig.nano_cpus`     (1e9 = 1 full CPU)
/// - `cpu_shares`    → `HostConfig.cpu_shares`    (relative weight, default 1024)
/// - `shm_size_mb`   → `HostConfig.shm_size`      (bytes; default 64 MiB)
///
/// IMPORTANT: enabling hard memory limits causes the kernel OOM killer to
/// terminate the container when the working set exceeds the limit. The
/// container will restart (RestartPolicy::ALWAYS) but in-flight queries
/// fail. Surface this clearly in any UI that lets users set limits.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ServiceResourceLimits {
    /// Hard memory limit in MiB. None = unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<i64>,
    /// Memory + swap limit in MiB. None = unlimited.
    /// MUST be >= memory_mb when both are set; Docker rejects the request otherwise.
    /// Set equal to `memory_mb` to disable swap entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_swap_mb: Option<i64>,
    /// CPU quota in nano-cpus. 1_000_000_000 = 1 full CPU core. None = unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nano_cpus: Option<i64>,
    /// Relative CPU weight (default 1024). Only used when `nano_cpus` is None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_shares: Option<i64>,
    /// Shared memory (/dev/shm) size in MiB. None = Docker default (64 MiB).
    /// Maps to HostConfig.shm_size (bytes). PostgreSQL uses /dev/shm for parallel
    /// query workers and large work_mem; the 64 MiB default causes "could not
    /// resize shared memory segment ... No space left on device" under load.
    /// NOTE: shm_size is fixed at container-create time — Docker's live update
    /// API cannot change it, so changing this value recreates the container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shm_size_mb: Option<i64>,
}

impl ServiceResourceLimits {
    /// True when no limits are set — the container runs unconstrained.
    pub fn is_unlimited(&self) -> bool {
        self.memory_mb.is_none()
            && self.memory_swap_mb.is_none()
            && self.nano_cpus.is_none()
            && self.cpu_shares.is_none()
            && self.shm_size_mb.is_none()
    }

    /// Validate that memory_swap >= memory when both are set, and that no
    /// negative values slipped through.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(mem) = self.memory_mb {
            if mem <= 0 {
                return Err(format!("memory_mb must be > 0, got {}", mem));
            }
        }
        if let Some(swap) = self.memory_swap_mb {
            if swap <= 0 {
                return Err(format!("memory_swap_mb must be > 0, got {}", swap));
            }
            if let Some(mem) = self.memory_mb {
                if swap < mem {
                    return Err(format!(
                        "memory_swap_mb ({}) must be >= memory_mb ({})",
                        swap, mem
                    ));
                }
            }
        }
        if let Some(nc) = self.nano_cpus {
            if nc <= 0 {
                return Err(format!("nano_cpus must be > 0, got {}", nc));
            }
        }
        if let Some(cs) = self.cpu_shares {
            if cs <= 0 {
                return Err(format!("cpu_shares must be > 0, got {}", cs));
            }
        }
        if let Some(shm) = self.shm_size_mb {
            if shm <= 0 {
                return Err(format!("shm_size_mb must be > 0, got {}", shm));
            }
        }
        Ok(())
    }

    /// Extract a `ServiceResourceLimits` block from a service-config parameters JSON.
    ///
    /// Looks for a `resources` object at the top level. Missing or malformed
    /// blocks resolve to `ServiceResourceLimits::default()` (unlimited) so existing
    /// services continue to run unconstrained until an operator opts in.
    pub fn from_parameters(parameters: &serde_json::Value) -> Self {
        parameters
            .get("resources")
            .and_then(|v| serde_json::from_value::<ServiceResourceLimits>(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Apply these limits to a bollard `HostConfig`. Fields with `None`
    /// values are left untouched, preserving Docker defaults.
    pub fn apply_to_host_config(&self, host_config: &mut bollard::models::HostConfig) {
        if let Some(mb) = self.memory_mb {
            host_config.memory = Some(mb.saturating_mul(1024 * 1024));
        }
        if let Some(mb) = self.memory_swap_mb {
            host_config.memory_swap = Some(mb.saturating_mul(1024 * 1024));
        }
        if let Some(nc) = self.nano_cpus {
            host_config.nano_cpus = Some(nc);
        }
        if let Some(cs) = self.cpu_shares {
            host_config.cpu_shares = Some(cs);
        }
        if let Some(mb) = self.shm_size_mb {
            host_config.shm_size = Some(mb.saturating_mul(1024 * 1024));
        }
    }
}

/// Capabilities a service exposes for the generic restore framework.
///
/// Each engine overrides `ExternalService::restore_capabilities` to declare
/// what it supports. The handler layer uses this to validate requests and
/// the UI uses it to conditionally show options (e.g., PITR picker).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RestoreCapabilities {
    /// Restore a backup onto the same running service (destructive).
    pub restore_in_place: bool,
    /// Restore a backup into a freshly provisioned service.
    pub restore_to_new_service: bool,
    /// Point-in-time recovery using engine-specific continuous archives
    /// (WAL for Postgres, AOF for Redis, oplog for MongoDB, object versions for S3).
    pub pitr: bool,
    /// Earliest recoverable timestamp, if `pitr` is true. Derived from
    /// engine-specific archive metadata (e.g., `pg_stat_archiver`).
    #[schema(value_type = Option<String>, format = DateTime)]
    pub earliest_pitr_time: Option<chrono::DateTime<chrono::Utc>>,
    /// Latest recoverable timestamp, if `pitr` is true.
    #[schema(value_type = Option<String>, format = DateTime)]
    pub latest_pitr_time: Option<chrono::DateTime<chrono::Utc>>,
}

impl Default for RestoreCapabilities {
    fn default() -> Self {
        Self {
            restore_in_place: true,
            restore_to_new_service: false,
            pitr: false,
            earliest_pitr_time: None,
            latest_pitr_time: None,
        }
    }
}

/// What the caller wants to do with a backup.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RestoreMode {
    /// Restore onto the existing service, replacing current data.
    InPlace,
    /// Provision a new service and restore the backup into it.
    NewService {
        /// Name for the new service.
        name: String,
        /// Optional parameter overrides (e.g., different port, volume).
        /// Parameters not specified are copied from the source service.
        #[serde(default)]
        parameter_overrides: serde_json::Value,
    },
    /// Point-in-time recovery. Only valid when `RestoreCapabilities::pitr` is true.
    /// May apply in-place or create a new service depending on `target`.
    Pitr {
        /// Whether PITR creates a new service or restores in place.
        to_new_service: bool,
        /// Optional new service name (required when `to_new_service` is true).
        new_service_name: Option<String>,
        /// The recovery target — engine decides which variant it honors.
        target: RecoveryTarget,
    },
}

/// Engine-specific recovery target for PITR.
///
/// Postgres honors all variants; Redis/Mongo/S3 will likely reject non-Time
/// variants or define their own semantics when they grow PITR support.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
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

/// Context passed to restore methods: database handle, S3 clients, and
/// supporting services the orchestrator needs.
///
/// Kept as a struct (not positional args) because the list will grow as
/// more engines plug in (e.g., cluster coordinator, volume driver).
pub struct RestoreContext<'a> {
    pub s3_client: &'a aws_sdk_s3::Client,
    pub s3_credentials: &'a S3Credentials,
    /// S3 source row with DECRYPTED `access_key_id` / `secret_key` fields.
    /// The orchestrator clones the DB row and swaps the ciphertext out before
    /// handing it here, so trait implementations can use these values
    /// directly (passing to mc, env vars, etc.) without calling
    /// `EncryptionService::decrypt_string` again — doing so would fail
    /// because the bytes are no longer ciphertext.
    pub s3_source: &'a temps_entities::s3_sources::Model,
    pub backup: &'a temps_entities::backups::Model,
    pub backup_location: &'a str,
    /// The TARGET service — where the restored data will land.
    pub source_service: &'a temps_entities::external_services::Model,
    /// Config to use for the restore. For `restore_to_new_service` this is
    /// the template the new service clones. For `in_place` / PITR this is
    /// the config applied to the running container.
    ///
    /// The orchestrator pre-merges the ORIGIN service's password into this
    /// config (when the origin is known) because restored PGDATA / Redis
    /// AOF / mongo auth files carry source-side credential hashes. Using
    /// the target's credentials against restored data would fail
    /// authentication. When the origin is unknown (orphan), this is
    /// unchanged from the target's config and the caller is warned that
    /// the password is whatever the backup's original credentials were.
    pub source_config: ServiceConfig,
    pub pool: &'a temps_database::DbConnection,
}

/// Outcome of a restore-to-new-service operation.
///
/// The orchestrator uses these to create the new `external_services` row
/// and wire it up to projects/environments as the caller requested.
#[derive(Debug, Clone)]
pub struct NewServiceRestoreResult {
    /// The parameters (docker container id, port, credentials, etc.)
    /// that the new service ended up with. These get persisted into
    /// `external_service_params` by the handler.
    pub parameters: HashMap<String, String>,
    /// The new service's effective connection string.
    pub connection_info: String,
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
    pub fn from_str(s: &str) -> Result<Self> {
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
            _ => Err(anyhow::anyhow!("Invalid service type: {}", s)),
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

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ServiceParameter {
    pub name: String,
    pub required: bool,
    pub encrypted: bool,
    pub description: String,
    pub default_value: Option<String>,
    pub validation_pattern: Option<String>,
    /// Optional list of valid choices for this parameter
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choices: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogicalResource {
    pub name: String,
    pub resource_type: String,
    pub credentials: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeEnvVar {
    pub name: String,
    pub description: String,
    pub example: String,
    /// Whether this variable contains sensitive data (passwords, keys, tokens)
    pub sensitive: bool,
}

/// Information about an available Docker container that can be imported
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AvailableContainer {
    /// Container ID or name
    pub container_id: String,
    /// Container name
    pub container_name: String,
    /// Docker image name (e.g., "gotempsh/postgres-walg:18-bookworm")
    pub image: String,
    /// Extracted version from image (e.g., "17")
    pub version: String,
    /// Service type this container represents
    pub service_type: ServiceType,
    /// Whether the container is currently running
    pub is_running: bool,
    /// Exposed ports (e.g., [5432] for PostgreSQL, [6379] for Redis)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub exposed_ports: Vec<u16>,
}

/// Specification for a cluster member to be created.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterMemberSpec {
    /// Service-type-specific role (e.g., "monitor", "primary", "replica", "arbiter", "sentinel", "node")
    pub role: String,
    /// Target worker node ID. None = local (control plane).
    pub node_id: Option<i32>,
    /// Stable ordinal for this member (0, 1, 2, ...)
    pub ordinal: i32,
    /// WireGuard IP or hostname for inter-member communication
    pub hostname: Option<String>,
}

/// Result from initializing a single cluster member.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterMemberResult {
    pub ordinal: i32,
    pub role: String,
    pub container_id: String,
    pub container_name: String,
    pub port: Option<i32>,
    pub status: String,
}

/// Info about an existing cluster member, used for connection string generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterMemberInfo {
    pub role: String,
    pub hostname: String,
    pub port: i32,
    pub status: String,
}

/// Result of a single probe against a managed external service.
/// Returned by `ExternalService::health_probe` so the monitor can record
/// structured health history without the trait having to know about DB rows.
#[derive(Debug, Clone)]
pub struct HealthProbeResult {
    pub status: HealthProbeStatus,
    /// Round-trip probe latency, when measurable.
    pub response_time_ms: Option<i32>,
    /// Present when status is `Degraded` or `Down`. Never contains secrets.
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[async_trait]
#[allow(clippy::too_many_arguments)]
pub trait ExternalService: Send + Sync {
    /// Initialize the service with given configuration
    /// Returns a HashMap of inferred parameters that should be stored
    async fn init(&self, config: ServiceConfig) -> Result<HashMap<String, String>>;

    /// Check if the service is healthy
    async fn health_check(&self) -> Result<bool>;

    /// Structured health probe used by the background `ExternalServiceHealthMonitor`.
    ///
    /// Engines should override this to run a **real** check (Postgres `SELECT 1`,
    /// Redis `PING`, MongoDB `ping`, S3 `HeadBucket`, …) against the
    /// credentials in `service_config`. The default implementation returns
    /// `Down` with a clear message so any engine that forgets to implement
    /// this is visibly broken rather than silently green.
    ///
    /// Implementations MUST:
    /// - Apply their own timeout (≤ 5s total is recommended).
    /// - Never return secret material in `error_message`.
    async fn health_probe(&self, _service_config: ServiceConfig) -> Result<HealthProbeResult> {
        Ok(HealthProbeResult::down(
            "health_probe not implemented for this service type",
        ))
    }

    /// Restart the service container so that a freshly stored `metrics_ingest_key`
    /// in the config takes effect as OTLP env vars.
    ///
    /// The key is read from `service_config` — callers must persist it first.
    /// Default is a no-op; only OTLP-push engines (RustFS) override this.
    async fn apply_ingest_key(&self, _service_config: ServiceConfig) -> Result<()> {
        Ok(())
    }

    /// Get service type
    fn get_type(&self) -> ServiceType;

    /// Get service name
    fn get_name(&self) -> String;

    /// Get connection string or endpoint
    fn get_connection_info(&self) -> Result<String>;

    /// Cleanup/shutdown the service
    async fn cleanup(&self) -> Result<()>;

    /// Get parameter schema as JSON Schema
    /// Services must implement this to provide their configuration schema
    fn get_parameter_schema(&self) -> Option<serde_json::Value>;

    /// Start the service
    async fn start(&self) -> Result<()>;

    /// Stop the service
    async fn stop(&self) -> Result<()>;

    /// Remove the service and its data completely
    async fn remove(&self) -> Result<()>;

    fn get_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>>;

    fn get_docker_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>>;

    /// Provision a logical resource (like a database or schema) for a specific project and environment
    async fn provision_resource(
        &self,
        _service_config: ServiceConfig,
        project_id: &str,
        environment: &str,
    ) -> Result<LogicalResource> {
        Ok(LogicalResource {
            name: format!("{}_{}", project_id, environment),
            resource_type: "default".to_string(),
            credentials: HashMap::new(),
        })
    }

    /// Deprovision a logical resource
    async fn deprovision_resource(&self, _project_id: &str, _environment: &str) -> Result<()> {
        Ok(())
    }

    /// Get definitions of environment variables that will be available at runtime
    fn get_runtime_env_definitions(&self) -> Vec<RuntimeEnvVar> {
        Vec::new()
    }

    /// Get actual runtime environment variables for a specific project/environment
    async fn get_runtime_env_vars(
        &self,
        _config: ServiceConfig,
        _project_id: &str,
        _environment: &str,
    ) -> Result<HashMap<String, String>> {
        Ok(HashMap::new())
    }

    /// Side-effect-free variant of [`Self::get_runtime_env_vars`] for the UI
    /// preview path. Same `<project>_<env>` convention, but must not
    /// provision databases, buckets, or other external resources — the user
    /// is just looking at what their deployment *would* receive.
    ///
    /// Default delegates to `get_runtime_env_vars`. Services with
    /// provisioning side effects (Postgres `CREATE DATABASE`, S3 bucket
    /// create, etc.) override this to skip the side effect while still
    /// returning per-tenant values.
    async fn preview_runtime_env_vars(
        &self,
        config: ServiceConfig,
        project_id: &str,
        environment: &str,
    ) -> Result<HashMap<String, String>> {
        self.get_runtime_env_vars(config, project_id, environment)
            .await
    }
    fn get_local_address(&self, service_config: ServiceConfig) -> Result<String>;

    /// Get the effective host and port for connecting to this service
    /// In Docker mode, returns (container_name, internal_port)
    /// In Baremetal mode, returns (localhost, exposed_port)
    fn get_effective_address(&self, service_config: ServiceConfig) -> Result<(String, String)>;

    /// Get the Docker container name for this service.
    /// Used by cross-node env var rewriting to match container names in connection strings.
    fn get_docker_container_name(&self) -> String;

    /// Get the internal port used inside the Docker container (e.g., "5432" for Postgres).
    /// Used by cross-node env var rewriting alongside `get_docker_container_name`.
    fn get_docker_internal_port(&self) -> String;

    /// Backup the service data to an S3 location
    /// s3_client: Pre-built S3 client with decrypted credentials (for services that upload via AWS SDK)
    /// s3_credentials: Decrypted S3 credentials (for services that use WAL-G / external tools)
    /// s3_source: The S3 source configuration to use for backup
    /// subpath: The subpath within the S3 bucket where the backup should be stored
    async fn backup_to_s3(
        &self,
        _s3_client: &aws_sdk_s3::Client,
        _s3_credentials: &S3Credentials,
        _backup: temps_entities::backups::Model,
        _s3_source: &temps_entities::s3_sources::Model,
        _subpath: &str,
        _subpath_root: &str,
        _pool: &temps_database::DbConnection,
        _external_service: &temps_entities::external_services::Model,
        _service_config: ServiceConfig,
    ) -> Result<BackupOutcome> {
        Err(anyhow::anyhow!("Backup not implemented for this service"))
    }

    /// Restore the service data from an S3 backup
    async fn restore_from_s3(
        &self,
        _s3_client: &aws_sdk_s3::Client,
        _s3_credentials: &S3Credentials,
        _backup_location: &str,
        _s3_source: &temps_entities::s3_sources::Model,
        _service_config: ServiceConfig,
    ) -> Result<()> {
        Err(anyhow::anyhow!("Restore not implemented for this service"))
    }

    // -----------------------------------------------------------------------
    // Generic restore framework (Phase 1 of restore/PITR project).
    // Engines override `restore_capabilities` to declare what they support
    // and implement the matching method(s). Callers MUST consult
    // `restore_capabilities()` before invoking these methods — the default
    // impls return "not supported" so unimplemented paths fail fast.
    // -----------------------------------------------------------------------

    /// Declare what restore modes this service supports.
    ///
    /// The default preserves current behavior: in-place restore works for any
    /// service that implements `restore_from_s3`, and nothing else is claimed.
    /// Engines that can provision fresh services from a backup should override
    /// to set `restore_to_new_service = true`. Only Postgres should set
    /// `pitr = true` initially (after WAL archiving is hardened).
    async fn restore_capabilities(
        &self,
        _service_config: ServiceConfig,
    ) -> Result<RestoreCapabilities> {
        Ok(RestoreCapabilities::default())
    }

    /// Provision a new service and restore the given backup into it.
    ///
    /// Implementations should:
    /// 1. Create a fresh container/bucket/volume sized for the backup.
    /// 2. Stream or download the backup into the new storage.
    /// 3. Start the service and verify health.
    /// 4. Return the parameters the orchestrator should persist.
    ///
    /// The new service's `external_services` row is created by the handler
    /// layer after this returns — implementations should not insert rows.
    async fn restore_to_new_service(
        &self,
        _ctx: RestoreContext<'_>,
        _new_service_name: String,
        _parameter_overrides: serde_json::Value,
    ) -> Result<NewServiceRestoreResult> {
        Err(anyhow::anyhow!(
            "restore_to_new_service not supported for service type {}",
            self.get_type()
        ))
    }

    /// Perform a point-in-time recovery.
    ///
    /// Only called when `restore_capabilities().pitr == true`. If
    /// `to_new_service` is true the implementation should behave like
    /// `restore_to_new_service` but apply the recovery target before
    /// promoting; otherwise it restores in-place.
    async fn restore_pitr(
        &self,
        _ctx: RestoreContext<'_>,
        _target: RecoveryTarget,
        _to_new_service: bool,
        _new_service_name: Option<String>,
    ) -> Result<Option<NewServiceRestoreResult>> {
        Err(anyhow::anyhow!(
            "restore_pitr not supported for service type {}",
            self.get_type()
        ))
    }

    /// Upgrade the service to a new version/image with data migration
    /// This method handles version-specific upgrade logic (e.g., pg_upgrade for PostgreSQL)
    ///
    /// # Arguments
    /// * `old_config` - Configuration of the current running service
    /// * `new_config` - Configuration with the new version/image
    ///
    /// # Returns
    /// * `Ok(())` if upgrade successful
    /// * `Err(...)` if upgrade failed or not supported
    async fn upgrade(&self, _old_config: ServiceConfig, _new_config: ServiceConfig) -> Result<()> {
        Err(anyhow::anyhow!("Upgrade not implemented for this service"))
    }

    /// Get the default/recommended Docker image and version for this service
    /// Returns (image_name, version) tuple
    fn get_default_docker_image(&self) -> (String, String) {
        ("".to_string(), "latest".to_string())
    }

    /// Get the currently running Docker image and version for this service
    /// Returns (image_name, version) tuple
    async fn get_current_docker_image(&self) -> Result<(String, String)> {
        Err(anyhow::anyhow!(
            "Getting current docker image not implemented for this service"
        ))
    }

    /// Get the default/recommended version for this service
    fn get_default_version(&self) -> String {
        "latest".to_string()
    }

    /// Get the currently running version for this service
    async fn get_current_version(&self) -> Result<String> {
        Err(anyhow::anyhow!(
            "Getting current version not implemented for this service"
        ))
    }

    // -----------------------------------------------------------------------
    // Cluster lifecycle methods (opt-in for service types that support clustering)
    // -----------------------------------------------------------------------

    /// Whether this service type supports cluster topology.
    fn supports_cluster(&self) -> bool {
        false
    }

    /// Valid roles for this service type in cluster mode.
    /// Used for validation when creating or modifying cluster members.
    fn valid_cluster_roles(&self) -> Vec<&'static str> {
        Vec::new()
    }

    /// Initialize a cluster with the given member specifications.
    /// Members must be created in the returned order (monitor first, then primary, then replicas).
    ///
    /// Returns a Vec of `ClusterMemberResult` with container details for each member.
    async fn init_cluster(
        &self,
        _config: ServiceConfig,
        _members: Vec<ClusterMemberSpec>,
    ) -> Result<Vec<ClusterMemberResult>> {
        Err(anyhow::anyhow!(
            "Cluster mode not supported for service type {}",
            self.get_type()
        ))
    }

    /// Build the connection string for a cluster, given all member addresses.
    /// E.g., multi-host libpq for Postgres, replica set URI for MongoDB.
    fn cluster_connection_string(
        &self,
        _members: &[ClusterMemberInfo],
        _config: &ServiceConfig,
    ) -> Result<String> {
        Err(anyhow::anyhow!(
            "Cluster connection string not supported for service type {}",
            self.get_type()
        ))
    }

    /// Get the Docker image to use for cluster members (may differ from standalone).
    fn get_cluster_docker_image(&self) -> (String, String) {
        self.get_default_docker_image()
    }

    /// Import an existing running Docker container as a managed service
    /// User provides container ID and necessary credentials/configuration
    ///
    /// # Arguments
    /// * `container_id` - Docker container ID or name of the running service
    /// * `service_name` - Name to register the service as in Temps
    /// * `credentials` - User-provided credentials (username, password, etc)
    /// * `additional_config` - Any additional configuration needed (ports, paths, etc)
    ///
    /// # Returns
    /// * Returns registered ServiceConfig with managed parameters
    async fn import_from_container(
        &self,
        _container_id: String,
        _service_name: String,
        _credentials: HashMap<String, String>,
        _additional_config: serde_json::Value,
    ) -> Result<ServiceConfig> {
        Err(anyhow::anyhow!("Import not implemented for this service"))
    }
}

#[cfg(test)]
mod resource_limits_tests {
    use super::*;

    #[test]
    fn default_is_unlimited() {
        let limits = ServiceResourceLimits::default();
        assert!(limits.is_unlimited());
    }

    #[test]
    fn validate_rejects_zero_and_negative_values() {
        // memory must be > 0
        assert!(ServiceResourceLimits {
            memory_mb: Some(0),
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(ServiceResourceLimits {
            memory_mb: Some(-1),
            ..Default::default()
        }
        .validate()
        .is_err());

        // swap < memory is rejected (Docker would refuse the request anyway)
        assert!(ServiceResourceLimits {
            memory_mb: Some(512),
            memory_swap_mb: Some(256),
            ..Default::default()
        }
        .validate()
        .is_err());

        // swap == memory is fine — that disables swap
        assert!(ServiceResourceLimits {
            memory_mb: Some(512),
            memory_swap_mb: Some(512),
            ..Default::default()
        }
        .validate()
        .is_ok());

        // shm_size_mb must be > 0
        assert!(ServiceResourceLimits {
            shm_size_mb: Some(0),
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(ServiceResourceLimits {
            shm_size_mb: Some(256),
            ..Default::default()
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn from_parameters_reads_resources_block() {
        let params = serde_json::json!({
            "port": "5432",
            "resources": {
                "memory_mb": 1024,
                "nano_cpus": 1_000_000_000_i64
            }
        });
        let limits = ServiceResourceLimits::from_parameters(&params);
        assert_eq!(limits.memory_mb, Some(1024));
        assert_eq!(limits.nano_cpus, Some(1_000_000_000));
        assert_eq!(limits.cpu_shares, None);
        assert_eq!(limits.memory_swap_mb, None);
        assert_eq!(limits.shm_size_mb, None);
    }

    #[test]
    fn from_parameters_missing_block_is_unlimited() {
        let params = serde_json::json!({ "port": "5432" });
        let limits = ServiceResourceLimits::from_parameters(&params);
        assert!(limits.is_unlimited());
    }

    #[test]
    fn apply_to_host_config_only_sets_some_fields() {
        let limits = ServiceResourceLimits {
            memory_mb: Some(512),
            nano_cpus: Some(500_000_000),
            ..Default::default()
        };
        let mut hc = bollard::models::HostConfig::default();
        limits.apply_to_host_config(&mut hc);
        // 512 MiB = 536870912 bytes
        assert_eq!(hc.memory, Some(536_870_912));
        assert_eq!(hc.nano_cpus, Some(500_000_000));
        // Untouched fields stay None — Docker default = unlimited.
        assert_eq!(hc.memory_swap, None);
        assert_eq!(hc.cpu_shares, None);
        assert_eq!(hc.shm_size, None);
    }

    #[test]
    fn apply_to_host_config_sets_shm() {
        let limits = ServiceResourceLimits {
            shm_size_mb: Some(256),
            ..Default::default()
        };
        let mut hc = bollard::models::HostConfig::default();
        limits.apply_to_host_config(&mut hc);
        // 256 MiB = 268435456 bytes
        assert_eq!(hc.shm_size, Some(256 * 1024 * 1024));
    }

    #[test]
    fn apply_to_host_config_unlimited_leaves_host_config_untouched() {
        let limits = ServiceResourceLimits::default();
        let mut hc = bollard::models::HostConfig {
            cpu_shares: Some(1024), // pretend something else set this
            ..Default::default()
        };
        limits.apply_to_host_config(&mut hc);
        // None values must not overwrite — preserves whatever the engine set.
        assert_eq!(hc.cpu_shares, Some(1024));
        assert_eq!(hc.memory, None);
    }
}
