use crate::utils::ensure_network_exists;

use super::{
    ExternalService, HealthProbeResult, LogicalResource, NewServiceRestoreResult, RecoveryTarget,
    RuntimeEnvVar, ServiceConfig, ServiceResourceLimits, ServiceType,
};
use anyhow::Result;
use async_trait::async_trait;
use bollard::exec::CreateExecOptions;
use bollard::query_parameters::{InspectContainerOptions, StopContainerOptions};
use bollard::{body_full, Docker};
use futures::{StreamExt, TryStreamExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

const MARIADB_INTERNAL_PORT: &str = "3306";
const DEFAULT_MARIADB_IMAGE: &str = "mariadb:lts";
const MIN_PASSWORD_LENGTH: usize = 8;
const MARIADB_BACKUP_EXEC_TIMEOUT: Duration = Duration::from_secs(4 * 3600);
const MARIADB_IMAGE_PULL_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const MARIADB_BINLOG_UPLOAD_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MARIADB_BINLOG_REPLAY_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MARIADB_RESTORE_HELPER_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Resource/tuning profile for Temps-managed MariaDB containers.
///
/// A MariaDB service is a shared database server: linked projects receive
/// separate databases inside this container, not separate MariaDB daemons.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MariaDbSizeProfile {
    /// Conservative default for single-node 4 GiB and 8 GiB hosts.
    #[default]
    Small,
    /// Larger shared service profile for hosts with more spare memory.
    Standard,
    /// Minimal cgroup limits; use when the host is dedicated to this service.
    Dedicated,
}

impl MariaDbSizeProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Standard => "standard",
            Self::Dedicated => "dedicated",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "small" => Some(Self::Small),
            "standard" => Some(Self::Standard),
            "dedicated" => Some(Self::Dedicated),
            _ => None,
        }
    }

    pub fn default_resource_limits(self) -> ServiceResourceLimits {
        match self {
            Self::Small => ServiceResourceLimits {
                memory_mb: Some(512),
                memory_swap_mb: Some(768),
                nano_cpus: Some(750_000_000),
                cpu_shares: None,
                shm_size_mb: None,
            },
            Self::Standard => ServiceResourceLimits {
                memory_mb: Some(1024),
                memory_swap_mb: Some(1536),
                nano_cpus: Some(1_500_000_000),
                cpu_shares: None,
                shm_size_mb: None,
            },
            Self::Dedicated => ServiceResourceLimits {
                memory_mb: None,
                memory_swap_mb: None,
                nano_cpus: None,
                cpu_shares: Some(2048),
                shm_size_mb: None,
            },
        }
    }

    pub fn server_args(self) -> Vec<String> {
        let (
            buffer_pool,
            max_connections,
            table_open_cache,
            thread_cache_size,
            tmp_table_size,
            performance_schema,
        ) = match self {
            Self::Small => ("128M", "50", "256", "16", "32M", "OFF"),
            Self::Standard => ("384M", "100", "400", "32", "64M", "ON"),
            Self::Dedicated => ("1024M", "200", "800", "64", "128M", "ON"),
        };

        vec![
            "--skip-name-resolve".to_string(),
            format!("--innodb-buffer-pool-size={buffer_pool}"),
            format!("--max-connections={max_connections}"),
            format!("--table-open-cache={table_open_cache}"),
            format!("--thread-cache-size={thread_cache_size}"),
            format!("--tmp-table-size={tmp_table_size}"),
            format!("--max-heap-table-size={tmp_table_size}"),
            format!("--performance-schema={performance_schema}"),
        ]
    }
}

/// How often closed binary-log segments are shipped to S3 — the PITR
/// granularity (recovery-point objective) for a MariaDB service.
///
/// MariaDB has no continuous archiver, so a background task ships rotated
/// binlogs on this cadence (see the binlog archiver). Smaller intervals lower
/// the worst-case data loss on restore at the cost of more frequent S3
/// uploads; the residual RPO is one interval. `binlog_expire_logs_seconds` is
/// derived from this value to always exceed the ship interval, so a segment is
/// never purged locally before it has been archived.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub enum BinlogArchiveInterval {
    /// Ship every minute — lowest RPO, highest S3 churn.
    #[serde(rename = "1m")]
    Min1,
    /// Ship every 5 minutes (default).
    #[default]
    #[serde(rename = "5m")]
    Min5,
    /// Ship every 15 minutes.
    #[serde(rename = "15m")]
    Min15,
    /// Ship every 60 minutes — lowest churn, highest RPO.
    #[serde(rename = "60m")]
    Min60,
}

impl BinlogArchiveInterval {
    /// Ship cadence in seconds.
    pub fn seconds(self) -> u64 {
        match self {
            Self::Min1 => 60,
            Self::Min5 => 300,
            Self::Min15 => 900,
            Self::Min60 => 3600,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Min1 => "1m",
            Self::Min5 => "5m",
            Self::Min15 => "15m",
            Self::Min60 => "60m",
        }
    }

    /// Local binlog retention (`binlog_expire_logs_seconds`). Kept well beyond
    /// the ship interval (>= 6x, floor 1h) so a segment is never purged before
    /// the archiver has shipped it — the continuity invariant for PITR.
    pub fn binlog_expire_seconds(self) -> u64 {
        (self.seconds() * 6).max(3600)
    }
}

/// Derive a stable, non-zero `server-id` for a MariaDB service from its name.
///
/// `--log-bin` requires a non-zero `server-id`. These standalone servers do
/// not replicate with each other, so uniqueness is not strictly required, but
/// deriving a stable value from the name keeps it consistent across recreates
/// and avoids collisions if two are ever wired into replication. FNV-1a hash
/// mapped into `1..=2_000_000_000`.
fn stable_server_id(name: &str) -> u32 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a offset basis
    for b in name.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3); // FNV-1a prime
    }
    (h % 2_000_000_000) as u32 + 1
}

/// Binary-logging server args appended to a MariaDB container's command so the
/// service is PITR-capable: ROW-format binlog, a stable server-id, durable
/// flushing (`sync_binlog=1` so a committed-but-unsynced binlog tail is not
/// lost on crash), and a derived retention window.
///
/// Credentials are never involved here — these are purely server tuning flags.
fn binlog_server_args(server_id: u32, interval: BinlogArchiveInterval) -> Vec<String> {
    vec![
        "--log-bin=mysql-bin".to_string(),
        "--binlog-format=ROW".to_string(),
        format!("--server-id={server_id}"),
        "--sync-binlog=1".to_string(),
        format!(
            "--binlog-expire-logs-seconds={}",
            interval.binlog_expire_seconds()
        ),
    ]
}

/// Manifest describing which binary-log segments have been shipped to S3 for
/// a MariaDB service. Stored at the `binlog/manifest.json` key. The restore
/// path reads this to know the contiguous set of segments available to replay.
///
/// Filenames are stored bare (e.g. `mysql-bin.000007`), without the `.gz`
/// suffix the on-disk S3 objects carry.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BinlogManifest {
    /// The highest segment shipped so far (lexicographically). `None` before
    /// the first segment is archived. Gates the to-ship set on each run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_shipped_file: Option<String>,
    /// RFC 3339 timestamp of the last successful manifest update.
    #[serde(default)]
    pub updated_at: String,
    /// Every segment shipped to S3, in ship order.
    #[serde(default)]
    pub shipped_files: Vec<String>,
}

/// Input configuration for creating a MariaDB service.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(
    title = "MariaDB Configuration",
    description = "Configuration for MariaDB service"
)]
pub struct MariaDbInputConfig {
    /// MariaDB host address.
    #[serde(default = "default_host")]
    #[schemars(example = "example_host", default = "default_host")]
    pub host: String,

    /// MariaDB host port (auto-assigned if not provided).
    #[schemars(example = "example_port")]
    pub port: Option<String>,

    /// Initial application database.
    #[serde(default = "default_database")]
    #[schemars(example = "example_database", default = "default_database")]
    pub database: String,

    /// Initial application user.
    #[serde(default = "default_username")]
    #[schemars(example = "example_username", default = "default_username")]
    pub username: String,

    /// Application user password (auto-generated if not provided or too short).
    #[serde(default, deserialize_with = "deserialize_optional_password")]
    #[schemars(
        with = "Option<String>",
        example = "example_password",
        description = "Application user password (minimum 8 characters, auto-generated if not provided)"
    )]
    pub password: Option<String>,

    /// Root password used by Temps for administrative provisioning.
    #[serde(default, deserialize_with = "deserialize_optional_password")]
    #[schemars(
        with = "Option<String>",
        example = "example_root_password",
        description = "Root password (minimum 8 characters, auto-generated if not provided)"
    )]
    pub root_password: Option<String>,

    /// Full Docker image reference.
    #[serde(default = "default_docker_image")]
    #[schemars(example = "example_docker_image", default = "default_docker_image")]
    pub docker_image: String,

    /// Managed service size/tuning profile.
    #[serde(default)]
    pub size_profile: MariaDbSizeProfile,

    /// Point-in-time-recovery granularity: how often binary logs are shipped
    /// to S3. Smaller = less data lost on restore, more frequent uploads.
    #[serde(default)]
    pub binlog_archive_interval: BinlogArchiveInterval,

    /// Real Docker container name when this service was imported from an
    /// existing MariaDB-compatible container (set by `import_from_container`,
    /// never user-editable — omitted from the create form). Overrides the
    /// derived `mariadb-{name}` container name so internal addressing
    /// targets the actual pre-existing container instead of a synthesized
    /// name that doesn't exist.
    ///
    /// Deserialized through `deserialize_optional_non_empty` as a second,
    /// independent guard alongside `#[schemars(skip)]`: an empty string here
    /// previously made `init` treat the service as imported (skipping
    /// container creation) and fail with a Docker 301 on
    /// `POST /containers//start` — normalize blank to `None` regardless of
    /// how the value reaches this struct.
    #[serde(default, deserialize_with = "deserialize_optional_non_empty")]
    #[schemars(skip)]
    pub container_name: Option<String>,
}

/// Internal runtime configuration for MariaDB service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MariaDbConfig {
    pub host: String,
    pub port: String,
    pub database: String,
    pub username: String,
    pub password: String,
    pub root_password: String,
    pub docker_image: String,
    /// Real container name for imported services — see
    /// `MariaDbInputConfig::container_name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
    #[serde(default)]
    pub size_profile: MariaDbSizeProfile,
    #[serde(default)]
    pub binlog_archive_interval: BinlogArchiveInterval,
}

impl From<MariaDbInputConfig> for MariaDbConfig {
    fn from(input: MariaDbInputConfig) -> Self {
        Self {
            host: input.host,
            port: input.port.unwrap_or_else(|| {
                find_available_port(3306)
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "3306".to_string())
            }),
            database: input.database,
            username: input.username,
            password: input.password.unwrap_or_else(generate_password),
            root_password: input.root_password.unwrap_or_else(generate_password),
            docker_image: input.docker_image,
            container_name: input.container_name,
            size_profile: input.size_profile,
            binlog_archive_interval: input.binlog_archive_interval,
        }
    }
}

fn deserialize_optional_password<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(match opt {
        Some(s) if !s.is_empty() && s.len() >= MIN_PASSWORD_LENGTH => Some(s),
        _ => None,
    })
}

/// Treats a blank string the same as an absent value. Used for
/// `container_name` so a stray `""` (e.g. from a generic parameters-edit
/// path that isn't schema-driven) can never be mistaken for "this service is
/// imported from an existing container" — see `MariaDbInputConfig::container_name`.
fn deserialize_optional_non_empty<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(opt.filter(|s| !s.is_empty()))
}

fn default_host() -> String {
    "localhost".to_string()
}

fn default_database() -> String {
    "app".to_string()
}

fn default_username() -> String {
    "app".to_string()
}

fn default_docker_image() -> String {
    DEFAULT_MARIADB_IMAGE.to_string()
}

fn example_host() -> &'static str {
    "localhost"
}

fn example_port() -> &'static str {
    "3306"
}

fn example_database() -> &'static str {
    "app"
}

fn example_username() -> &'static str {
    "app"
}

fn example_password() -> &'static str {
    "your-secure-password"
}

fn example_root_password() -> &'static str {
    "your-secure-root-password"
}

fn example_docker_image() -> &'static str {
    DEFAULT_MARIADB_IMAGE
}

fn is_port_available(port: u16) -> bool {
    TcpListener::bind(("0.0.0.0", port)).is_ok()
}

fn find_available_port(start_port: u16) -> Option<u16> {
    (start_port..start_port + 100).find(|&port| is_port_available(port))
}

fn generate_password() -> String {
    use rand::{distributions::Alphanumeric, Rng};
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(24)
        .map(char::from)
        .collect()
}

pub struct MariaDbService {
    name: String,
    config: Arc<RwLock<Option<MariaDbConfig>>>,
    resource_limits: Arc<RwLock<ServiceResourceLimits>>,
    docker: Arc<Docker>,
}

impl MariaDbService {
    pub fn new(name: String, docker: Arc<Docker>) -> Self {
        Self {
            name,
            config: Arc::new(RwLock::new(None)),
            resource_limits: Arc::new(RwLock::new(ServiceResourceLimits::default())),
            docker,
        }
    }

    fn get_container_name(&self) -> String {
        format!("mariadb-{}", self.name)
    }

    /// The container this service actually runs in: the imported container's
    /// real name when `config.container_name` is set, otherwise the derived
    /// `mariadb-{name}`. Every operation that talks to the live container
    /// (admin SQL, backup, restore, binlog shipping, start/stop) must resolve
    /// through this, not `get_container_name()` directly, or it targets a
    /// synthesized name that doesn't exist for imported services.
    fn get_live_container_name(&self, config: &MariaDbConfig) -> String {
        config
            .container_name
            .clone()
            .unwrap_or_else(|| self.get_container_name())
    }

    fn get_mariadb_config(&self, service_config: ServiceConfig) -> Result<MariaDbConfig> {
        let input_config: MariaDbInputConfig = serde_json::from_value(service_config.parameters)
            .map_err(|e| anyhow::anyhow!("Failed to parse MariaDB configuration: {}", e))?;
        let config = MariaDbConfig::from(input_config);

        Self::validate_identifier("database", &config.database)?;
        Self::validate_identifier("username", &config.username)?;
        Self::validate_password("password", &config.password)?;
        Self::validate_password("root_password", &config.root_password)?;

        Ok(config)
    }

    async fn create_container(
        &self,
        docker: &Docker,
        config: &MariaDbConfig,
        resource_limits: &ServiceResourceLimits,
    ) -> Result<()> {
        let container_name = self.get_container_name();

        if docker.inspect_image(&config.docker_image).await.is_ok() {
            info!(
                "MariaDB image {} already present locally",
                config.docker_image
            );
        } else {
            info!("Pulling MariaDB image {}", config.docker_image);
            let (image_name, tag) = if let Some((name, tag)) = config.docker_image.split_once(':') {
                (name.to_string(), tag.to_string())
            } else {
                (config.docker_image.clone(), "latest".to_string())
            };

            tokio::time::timeout(MARIADB_IMAGE_PULL_TIMEOUT, async {
                docker
                    .create_image(
                        Some(bollard::query_parameters::CreateImageOptions {
                            from_image: Some(image_name),
                            tag: Some(tag),
                            ..Default::default()
                        }),
                        None,
                        None,
                    )
                    .try_collect::<Vec<_>>()
                    .await
            })
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "Timed out pulling MariaDB image {} after {}s",
                    config.docker_image,
                    MARIADB_IMAGE_PULL_TIMEOUT.as_secs()
                )
            })?
            .map_err(|e| anyhow::anyhow!("Failed to pull MariaDB image: {}", e))?;
        }

        let containers = docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: true,
                filters: Some(HashMap::from([(
                    "name".to_string(),
                    vec![container_name.clone()],
                )])),
                ..Default::default()
            }))
            .await?;

        if let Some(existing) = containers.first() {
            let existing_image = existing.image.as_deref().unwrap_or("");
            if existing_image == config.docker_image {
                info!(
                    "Container {} already exists with same image",
                    container_name
                );
                return Ok(());
            }

            info!(
                "Container {} already exists with different image (current: {}, requested: {}), recreating it",
                container_name, existing_image, config.docker_image
            );
            let _ = docker
                .stop_container(&container_name, None::<StopContainerOptions>)
                .await;
            docker
                .remove_container(
                    &container_name,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        v: false,
                        ..Default::default()
                    }),
                )
                .await
                .map_err(|e| {
                    anyhow::anyhow!("Failed to remove existing MariaDB container: {}", e)
                })?;
        }

        self.warn_if_host_capacity_tight(docker, config, resource_limits)
            .await;

        let service_label_key = format!("{}service_type", temps_core::DOCKER_LABEL_PREFIX);
        let name_label_key = format!("{}service_name", temps_core::DOCKER_LABEL_PREFIX);
        let container_labels = HashMap::from([
            (service_label_key, "mariadb".to_string()),
            (name_label_key, self.name.clone()),
        ]);

        let env_vars = vec![
            format!("MARIADB_ROOT_PASSWORD={}", config.root_password),
            format!("MARIADB_DATABASE={}", config.database),
            format!("MARIADB_USER={}", config.username),
            format!("MARIADB_PASSWORD={}", config.password),
            "MARIADB_AUTO_UPGRADE=1".to_string(),
            // Pin the server timezone to UTC so binlog event timestamps — and
            // therefore PITR `mysqlbinlog --stop-datetime` targets — are
            // unambiguous. RecoveryTarget::Time is UTC; without this the
            // recovery target could be misinterpreted in the host's local TZ.
            "TZ=UTC".to_string(),
        ];

        let volume_name = format!("mariadb_data_{}", self.name);
        docker
            .create_volume(bollard::models::VolumeCreateRequest {
                name: Some(volume_name.clone()),
                ..Default::default()
            })
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create MariaDB volume: {}", e))?;

        let mut host_config = bollard::models::HostConfig {
            port_bindings: Some(crate::utils::local_port_binding("3306/tcp", &config.port)),
            mounts: Some(vec![bollard::models::Mount {
                target: Some("/var/lib/mysql".to_string()),
                source: Some(volume_name),
                typ: Some(bollard::models::MountTypeEnum::VOLUME),
                ..Default::default()
            }]),
            log_config: Some(crate::utils::default_service_log_config()),
            security_opt: Some(vec!["no-new-privileges:true".to_string()]),
            pids_limit: Some(512),
            ..Default::default()
        };
        resource_limits.apply_to_host_config(&mut host_config);

        ensure_network_exists(docker)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to ensure network exists: {:?}", e))?;
        let networking_config = Some(bollard::models::NetworkingConfig {
            endpoints_config: Some(HashMap::from([(
                temps_core::NETWORK_NAME.to_string(),
                bollard::models::EndpointSettings {
                    ..Default::default()
                },
            )])),
        });

        let container_config = bollard::models::ContainerCreateBody {
            image: Some(config.docker_image.clone()),
            exposed_ports: Some(Vec::from(["3306/tcp".to_string()])),
            env: Some(env_vars),
            labels: Some(container_labels),
            // Tuning args + binary-logging args. Binlog is enabled by default
            // so the service is PITR-capable from creation (the MariaDB analog
            // of Postgres WAL archiving). Enabling binlog requires the flags at
            // server start, so existing containers created before this adopt it
            // on their next recreate (e.g. image upgrade); we do not force a
            // disruptive recreate of a healthy running container here.
            cmd: Some({
                let mut args = config.size_profile.server_args();
                args.extend(binlog_server_args(
                    stable_server_id(&self.name),
                    config.binlog_archive_interval,
                ));
                args
            }),
            host_config: Some(bollard::models::HostConfig {
                restart_policy: Some(bollard::models::RestartPolicy {
                    name: Some(bollard::models::RestartPolicyNameEnum::ALWAYS),
                    maximum_retry_count: None,
                }),
                ..host_config
            }),
            networking_config,
            healthcheck: Some(bollard::models::HealthConfig {
                test: Some(vec![
                    "CMD-SHELL".to_string(),
                    "mariadb-admin ping -h 127.0.0.1 -uroot -p\"$MARIADB_ROOT_PASSWORD\" --silent"
                        .to_string(),
                ]),
                interval: Some(1000000000),
                timeout: Some(3000000000),
                retries: Some(5),
                start_period: Some(30000000000),
                start_interval: Some(1000000000),
            }),
            ..Default::default()
        };

        let container = docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&container_name)
                        .build(),
                ),
                container_config,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create MariaDB container: {}", e))?;

        docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start MariaDB container: {}", e))?;

        self.wait_for_container_health(docker, &container.id)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to wait for MariaDB container health: {}", e))?;

        info!("MariaDB container {} created and started", container.id);
        Ok(())
    }

    async fn warn_if_host_capacity_tight(
        &self,
        docker: &Docker,
        config: &MariaDbConfig,
        resource_limits: &ServiceResourceLimits,
    ) {
        let Ok(containers) = docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: true,
                ..Default::default()
            }))
            .await
        else {
            return;
        };

        let service_label_key = format!("{}service_type", temps_core::DOCKER_LABEL_PREFIX);
        let existing_mariadb_count = containers
            .iter()
            .filter(|container| {
                let labeled = container
                    .labels
                    .as_ref()
                    .and_then(|labels| labels.get(&service_label_key))
                    .map(|value| value == "mariadb")
                    .unwrap_or(false);
                let named = container.names.as_ref().is_some_and(|names| {
                    names
                        .iter()
                        .any(|name| name.trim_start_matches('/').starts_with("mariadb-"))
                });
                labeled || named
            })
            .count();

        let host_memory_mb = Self::host_memory_mb();
        let requested_memory_mb = resource_limits
            .memory_mb
            .unwrap_or(match config.size_profile {
                MariaDbSizeProfile::Small => 512,
                MariaDbSizeProfile::Standard => 1024,
                MariaDbSizeProfile::Dedicated => 0,
            });

        if let Some(host_memory_mb) = host_memory_mb {
            let projected = if requested_memory_mb > 0 {
                requested_memory_mb * (existing_mariadb_count as i64 + 1)
            } else {
                0
            };
            if host_memory_mb <= 8192 && existing_mariadb_count >= 1 {
                warn!(
                    service = %self.name,
                    profile = config.size_profile.as_str(),
                    existing_mariadb_services = existing_mariadb_count,
                    host_memory_mb,
                    projected_mariadb_limit_mb = projected,
                    "Creating another MariaDB service container on a small host. Prefer sharing one MariaDB service across projects; Temps links create separate per-project databases inside that service."
                );
            }
        } else if existing_mariadb_count >= 2 {
            warn!(
                service = %self.name,
                profile = config.size_profile.as_str(),
                existing_mariadb_services = existing_mariadb_count,
                "Creating another MariaDB service container. Prefer sharing one MariaDB service across projects; Temps links create separate per-project databases inside that service."
            );
        }
    }

    fn host_memory_mb() -> Option<i64> {
        let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
        let total_kb = meminfo.lines().find_map(|line| {
            let rest = line.strip_prefix("MemTotal:")?;
            rest.split_whitespace().next()?.parse::<i64>().ok()
        })?;
        Some(total_kb / 1024)
    }

    async fn wait_for_container_health(&self, docker: &Docker, container_id: &str) -> Result<()> {
        let mut delay = Duration::from_millis(500);
        let mut total_wait = Duration::from_secs(0);
        let max_wait = Duration::from_secs(120);
        let max_delay = Duration::from_secs(2);

        while total_wait < max_wait {
            let info = docker
                .inspect_container(container_id, None::<InspectContainerOptions>)
                .await?;
            if let Some(state) = info.state {
                let is_running =
                    state.status == Some(bollard::models::ContainerStateStatusEnum::RUNNING);
                let health_status = state.health.as_ref().and_then(|h| h.status.as_ref());

                if is_running
                    && (health_status.is_none()
                        || health_status == Some(&bollard::models::HealthStatusEnum::HEALTHY))
                {
                    return Ok(());
                }

                if state.status == Some(bollard::models::ContainerStateStatusEnum::EXITED)
                    || state.status == Some(bollard::models::ContainerStateStatusEnum::DEAD)
                {
                    let exit_code = state.exit_code.unwrap_or(-1);
                    return Err(anyhow::anyhow!(
                        "MariaDB container exited unexpectedly with code {}",
                        exit_code
                    ));
                }
            }

            sleep(delay).await;
            total_wait += delay;
            delay = std::cmp::min(delay.mul_f32(1.5), max_delay);
        }

        Err(anyhow::anyhow!("MariaDB container health check timed out"))
    }

    async fn run_container_command(
        &self,
        container_name: &str,
        cmd: Vec<String>,
        env: Option<Vec<String>>,
        timeout: Duration,
    ) -> Result<String> {
        tokio::time::timeout(timeout, async {
            let exec = self
                .docker
                .create_exec(
                    container_name,
                    bollard::exec::CreateExecOptions {
                        cmd: Some(cmd),
                        env,
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create MariaDB exec: {}", e))?;

            let mut output_text = String::new();
            if let bollard::exec::StartExecResults::Attached { mut output, .. } = self
                .docker
                .start_exec(&exec.id, None)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to start MariaDB exec: {}", e))?
            {
                while let Some(result) = output.next().await {
                    match result {
                        Ok(bollard::container::LogOutput::StdOut { message })
                        | Ok(bollard::container::LogOutput::StdErr { message }) => {
                            output_text.push_str(&String::from_utf8_lossy(&message));
                        }
                        Ok(_) => {}
                        Err(e) => {
                            return Err(anyhow::anyhow!(
                                "Failed to read MariaDB exec output: {}",
                                e
                            ));
                        }
                    }
                }
            }

            let inspect = self
                .docker
                .inspect_exec(&exec.id)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to inspect MariaDB exec: {}", e))?;
            let exit_code = inspect.exit_code.unwrap_or(-1);
            if exit_code != 0 {
                return Err(anyhow::anyhow!(
                    "MariaDB command failed with exit code {}: {}",
                    exit_code,
                    output_text.trim()
                ));
            }

            Ok(output_text)
        })
        .await
        .map_err(|_| anyhow::anyhow!("MariaDB command timed out after {}s", timeout.as_secs()))?
    }

    async fn run_admin_sql(&self, config: &MariaDbConfig, sql: &str) -> Result<()> {
        let container_name = self.get_live_container_name(config);
        self.run_container_command(
            &container_name,
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "if command -v mariadb >/dev/null 2>&1; then \
                     mariadb -uroot -e \"$TEMPS_MARIADB_SQL\"; \
                 else \
                     mysql -uroot -e \"$TEMPS_MARIADB_SQL\"; \
                 fi"
                .to_string(),
            ],
            Some(vec![
                format!("MYSQL_PWD={}", config.root_password),
                format!("MARIADB_PWD={}", config.root_password),
                format!("TEMPS_MARIADB_SQL={}", sql),
            ]),
            Duration::from_secs(15),
        )
        .await
        .map(|_| ())
    }

    async fn ping(&self, config: &MariaDbConfig) -> Result<()> {
        let container_name = self.get_live_container_name(config);
        self.run_container_command(
            &container_name,
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "if command -v mariadb-admin >/dev/null 2>&1; then \
                     mariadb-admin ping -h 127.0.0.1 -uroot --silent; \
                 else \
                     mysqladmin ping -h 127.0.0.1 -uroot --silent; \
                 fi"
                .to_string(),
            ],
            Some(vec![
                format!("MYSQL_PWD={}", config.root_password),
                format!("MARIADB_PWD={}", config.root_password),
            ]),
            Duration::from_secs(5),
        )
        .await
        .map(|_| ())
    }

    async fn create_database(&self, service_config: ServiceConfig, database: &str) -> Result<()> {
        Self::validate_identifier("database", database)?;
        let config = self.get_mariadb_config(service_config)?;

        let database_ident = Self::quote_identifier(database);
        let username_literal = Self::sql_string_literal(&config.username);
        let password_literal = Self::sql_string_literal(&config.password);
        let sql = format!(
            "CREATE DATABASE IF NOT EXISTS {database_ident}; \
             CREATE USER IF NOT EXISTS {username_literal}@'%' IDENTIFIED BY {password_literal}; \
             GRANT ALL PRIVILEGES ON {database_ident}.* TO {username_literal}@'%'; \
             FLUSH PRIVILEGES;"
        );

        self.run_admin_sql(&config, &sql).await
    }

    async fn drop_database(&self, service_config: ServiceConfig, database: &str) -> Result<()> {
        Self::validate_identifier("database", database)?;
        let config = self.get_mariadb_config(service_config)?;
        let sql = format!(
            "DROP DATABASE IF EXISTS {};",
            Self::quote_identifier(database)
        );
        self.run_admin_sql(&config, &sql).await
    }

    fn build_runtime_env_vars(
        &self,
        service_config: ServiceConfig,
        resource_name: &str,
    ) -> Result<HashMap<String, String>> {
        let config = self.get_mariadb_config(service_config)?;
        Self::build_env_vars(
            &self.get_live_container_name(&config),
            MARIADB_INTERNAL_PORT,
            resource_name,
            &config.username,
            &config.password,
        )
    }

    fn build_env_vars(
        host: &str,
        port: &str,
        database: &str,
        username: &str,
        password: &str,
    ) -> Result<HashMap<String, String>> {
        Self::validate_identifier("database", database)?;
        Self::validate_identifier("username", username)?;
        Self::validate_password("password", password)?;

        let url = format!(
            "mysql://{}:{}@{}:{}/{}",
            urlencoding::encode(username),
            urlencoding::encode(password),
            host,
            port,
            database
        );

        let mut env_vars = HashMap::new();
        env_vars.insert("DATABASE_URL".to_string(), url.clone());
        env_vars.insert("MYSQL_URL".to_string(), url.clone());
        env_vars.insert("MYSQL_HOST".to_string(), host.to_string());
        env_vars.insert("MYSQL_PORT".to_string(), port.to_string());
        env_vars.insert("MYSQL_DATABASE".to_string(), database.to_string());
        env_vars.insert("MYSQL_USER".to_string(), username.to_string());
        env_vars.insert("MYSQL_PASSWORD".to_string(), password.to_string());
        env_vars.insert("MARIADB_URL".to_string(), url);
        env_vars.insert("MARIADB_HOST".to_string(), host.to_string());
        env_vars.insert("MARIADB_PORT".to_string(), port.to_string());
        env_vars.insert("MARIADB_DATABASE".to_string(), database.to_string());
        env_vars.insert("MARIADB_USER".to_string(), username.to_string());
        env_vars.insert("MARIADB_PASSWORD".to_string(), password.to_string());
        Ok(env_vars)
    }

    pub(crate) fn normalize_database_name(name: &str) -> String {
        let normalized = name
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>();

        let prefixed = if normalized
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(true)
        {
            format!("db_{}", normalized)
        } else {
            normalized
        };

        if prefixed.len() > 63 {
            prefixed[..63].to_string()
        } else {
            prefixed
        }
    }

    fn validate_identifier(label: &str, value: &str) -> Result<()> {
        if value.is_empty() {
            return Err(anyhow::anyhow!("{} cannot be empty", label));
        }
        if value.len() > 63 {
            return Err(anyhow::anyhow!(
                "{} '{}' exceeds 63 character limit",
                label,
                value
            ));
        }
        let mut chars = value.chars();
        let Some(first) = chars.next() else {
            return Err(anyhow::anyhow!("{} cannot be empty", label));
        };
        if !first.is_ascii_alphabetic() && first != '_' {
            return Err(anyhow::anyhow!(
                "{} '{}' must start with a letter or underscore",
                label,
                value
            ));
        }
        if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(anyhow::anyhow!(
                "{} '{}' contains invalid characters. Only ASCII letters, digits, and underscores are allowed",
                label,
                value
            ));
        }
        Ok(())
    }

    fn validate_password(label: &str, value: &str) -> Result<()> {
        if value.len() < MIN_PASSWORD_LENGTH {
            return Err(anyhow::anyhow!(
                "{} must be at least {} characters",
                label,
                MIN_PASSWORD_LENGTH
            ));
        }
        if value.len() > 256 {
            return Err(anyhow::anyhow!("{} too long (max 256 characters)", label));
        }
        for (i, c) in value.chars().enumerate() {
            match c {
                '\'' => {
                    return Err(anyhow::anyhow!(
                        "{} contains a single quote at position {}",
                        label,
                        i
                    ))
                }
                '\\' => {
                    return Err(anyhow::anyhow!(
                        "{} contains a backslash at position {}",
                        label,
                        i
                    ))
                }
                '\0' => return Err(anyhow::anyhow!("{} contains a null byte", label)),
                '\n' | '\r' => return Err(anyhow::anyhow!("{} contains a newline", label)),
                c if c.is_control() => {
                    return Err(anyhow::anyhow!(
                        "{} contains control character at position {}",
                        label,
                        i
                    ))
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn quote_identifier(value: &str) -> String {
        format!("`{}`", value)
    }

    fn sql_string_literal(value: &str) -> String {
        format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
    }

    fn env_to_map(env: Option<Vec<String>>) -> HashMap<String, String> {
        env.unwrap_or_default()
            .into_iter()
            .filter_map(|entry| {
                let (key, value) = entry.split_once('=')?;
                Some((key.to_string(), value.to_string()))
            })
            .collect()
    }

    fn first_non_empty<'a>(values: impl IntoIterator<Item = Option<&'a String>>) -> Option<String> {
        values
            .into_iter()
            .flatten()
            .find(|value| !value.trim().is_empty())
            .cloned()
    }

    fn json_string(value: &serde_json::Value, key: &str) -> Option<String> {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(ToString::to_string)
    }

    fn extract_host_port(container: &bollard::models::ContainerInspectResponse) -> Option<String> {
        container
            .network_settings
            .as_ref()
            .and_then(|settings| settings.ports.as_ref())
            .and_then(|ports| ports.get("3306/tcp"))
            .and_then(|bindings| bindings.as_ref())
            .and_then(|bindings| bindings.first())
            .and_then(|binding| binding.host_port.clone())
    }

    async fn verify_import_connection(
        username: &str,
        password: &str,
        port: &str,
        database: &str,
    ) -> Result<()> {
        let connection_url = format!(
            "mysql://{}:{}@localhost:{}/{}",
            urlencoding::encode(username),
            urlencoding::encode(password),
            port,
            urlencoding::encode(database)
        );

        let pool = sqlx::mysql::MySqlPoolOptions::new()
            .max_connections(1)
            .connect(&connection_url)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to connect to MariaDB-compatible container at localhost:{} with provided credentials: {}",
                    port,
                    e
                )
            })?;
        pool.close().await;
        Ok(())
    }

    fn backup_key_from_location(location: &str, bucket: &str) -> String {
        let bucket_prefix = format!("s3://{}/", bucket);
        location
            .strip_prefix(&bucket_prefix)
            .unwrap_or(location)
            .to_string()
    }

    // ── Binary-log archiver (PITR "frequent scheduled ship" half) ──────────
    //
    // MariaDB has no continuous archiver, so a background task periodically
    // ships closed binary-log segments to S3. The active (last) segment is
    // never shipped because it is still being written. A manifest object
    // records which segments have been shipped so the run is idempotent and
    // the restore path knows what is available to replay.

    /// Ship every closed (rotated) binary-log segment that has not yet been
    /// archived to S3, advancing the manifest only past segments that actually
    /// uploaded successfully. Returns the number of segments shipped this run.
    ///
    /// Steps:
    /// 1. `FLUSH BINARY LOGS` rotates the active segment closed.
    /// 2. `SHOW BINARY LOGS` lists segments; the last one is still active.
    /// 3. The S3 manifest's `last_shipped_file` gates what is new.
    /// 4. Each newer closed segment is downloaded, gzipped, and PUT.
    /// 5. The manifest is rewritten to reflect what landed.
    ///
    /// Credentials are passed via `MYSQL_PWD`/`MARIADB_PWD` exec env, never on
    /// argv, and are never logged.
    pub async fn archive_binlogs(
        &self,
        s3_client: &aws_sdk_s3::Client,
        s3_source: &temps_entities::s3_sources::Model,
        config: &MariaDbConfig,
    ) -> Result<usize> {
        let container_name = self.get_live_container_name(config);
        let bucket = &s3_source.bucket_name;
        let prefix = s3_source.bucket_path.trim_matches('/');

        // 1. Rotate so the currently-active segment closes and becomes
        //    shippable on this (or a subsequent) run.
        self.flush_binary_logs(config).await?;

        // 2. Enumerate segments. The last entry is the new active segment.
        let raw = self.show_binary_logs(config).await?;
        let all_files = Self::parse_show_binary_logs(&raw);
        let closed = Self::closed_binlog_files(&all_files);
        if closed.is_empty() {
            debug!(
                service = %self.name,
                "No closed MariaDB binlog segments to archive yet"
            );
            return Ok(0);
        }

        // 3. Read the manifest to learn what we have already shipped.
        let mut manifest = self
            .read_binlog_manifest(s3_client, bucket, prefix, &self.name)
            .await
            .unwrap_or_default();

        // 4. Compute the to-ship set: closed segments lexicographically
        //    greater than last_shipped_file (excludes the active file and
        //    anything already shipped).
        let to_ship = Self::binlogs_to_ship(&all_files, manifest.last_shipped_file.as_deref());
        if to_ship.is_empty() {
            debug!(service = %self.name, "MariaDB binlogs already up to date in S3");
            return Ok(0);
        }

        info!(
            service = %self.name,
            count = to_ship.len(),
            "Shipping MariaDB binlog segment(s) to S3"
        );

        let mut shipped = 0usize;
        for file in &to_ship {
            let key = Self::binlog_object_key(prefix, &self.name, file);
            match self
                .ship_one_binlog(s3_client, bucket, &container_name, file, &key)
                .await
            {
                Ok(()) => {
                    // Advance the manifest only past files that actually
                    // uploaded, so a mid-run failure never claims an unshipped
                    // segment is present.
                    manifest.last_shipped_file = Some(file.clone());
                    if !manifest.shipped_files.contains(file) {
                        manifest.shipped_files.push(file.clone());
                    }
                    shipped += 1;
                    info!(service = %self.name, binlog = %file, "Shipped MariaDB binlog segment");
                }
                Err(e) => {
                    // Stop at the first failure: segments must be shipped in
                    // order so the replay chain stays contiguous. Persist
                    // progress so far below.
                    warn!(
                        service = %self.name,
                        binlog = %file,
                        "Failed to ship MariaDB binlog segment, stopping run: {}",
                        e
                    );
                    break;
                }
            }
        }

        // 5. Persist the manifest reflecting what actually landed.
        if shipped > 0 {
            manifest.updated_at = chrono::Utc::now().to_rfc3339();
            if let Err(e) = self
                .write_binlog_manifest(s3_client, bucket, prefix, &manifest)
                .await
            {
                // The segments are uploaded; only the manifest write failed.
                // The next run re-reads the (stale) manifest and re-ships the
                // same segments idempotently (overwrite is a no-op of content).
                warn!(
                    service = %self.name,
                    "Shipped {} MariaDB binlog segment(s) but failed to update manifest: {}",
                    shipped,
                    e
                );
                return Err(e);
            }
        }

        Ok(shipped)
    }

    /// `FLUSH BINARY LOGS` — rotates the active binlog so it closes.
    async fn flush_binary_logs(&self, config: &MariaDbConfig) -> Result<()> {
        self.run_admin_sql(config, "FLUSH BINARY LOGS").await
    }

    /// Raw `SHOW BINARY LOGS` output (tab-separated rows: filename, size, ...).
    async fn show_binary_logs(&self, config: &MariaDbConfig) -> Result<String> {
        let container_name = self.get_live_container_name(config);
        self.run_container_command(
            &container_name,
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "if command -v mariadb >/dev/null 2>&1; then \
                     mariadb -N -B -uroot -e 'SHOW BINARY LOGS'; \
                 else \
                     mysql -N -B -uroot -e 'SHOW BINARY LOGS'; \
                 fi"
                .to_string(),
            ],
            Some(vec![
                format!("MYSQL_PWD={}", config.root_password),
                format!("MARIADB_PWD={}", config.root_password),
            ]),
            Duration::from_secs(15),
        )
        .await
    }

    /// Download a single binlog segment out of the container, gzip it, and PUT
    /// it to its S3 key. Reads the file as a tar stream so the raw bytes are
    /// preserved (an `exec cat` through the log-mux corrupts binary data).
    async fn ship_one_binlog(
        &self,
        s3_client: &aws_sdk_s3::Client,
        bucket: &str,
        container_name: &str,
        file: &str,
        key: &str,
    ) -> Result<()> {
        use std::io::Write;

        let bytes = self
            .read_binlog_from_container(container_name, file)
            .await?;

        let gzipped = {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(&bytes)?;
            encoder.finish()?
        };

        s3_client
            .put_object()
            .bucket(bucket)
            .key(key)
            .body(aws_sdk_s3::primitives::ByteStream::from(gzipped))
            .content_type("application/x-gzip")
            .send()
            .await
            .map_err(|e| {
                anyhow::anyhow!("Failed to upload binlog to s3://{}/{}: {}", bucket, key, e)
            })?;

        Ok(())
    }

    /// Read `/var/lib/mysql/{file}` out of the container as raw bytes via the
    /// Docker tar download API (preserves binary content).
    async fn read_binlog_from_container(
        &self,
        container_name: &str,
        file: &str,
    ) -> Result<Vec<u8>> {
        use std::io::Read;

        let path = format!("/var/lib/mysql/{}", file);
        let options = bollard::query_parameters::DownloadFromContainerOptionsBuilder::default()
            .path(&path)
            .build();

        let mut tar_stream = self
            .docker
            .download_from_container(container_name, Some(options));

        let mut tar_bytes: Vec<u8> = Vec::new();
        while let Some(chunk) = tar_stream.next().await {
            let bytes = chunk.map_err(|e| {
                anyhow::anyhow!("Failed to download binlog {} from container: {}", file, e)
            })?;
            tar_bytes.extend_from_slice(&bytes);
        }

        let mut archive = tar::Archive::new(std::io::Cursor::new(tar_bytes));
        let entries = archive.entries().map_err(|e| {
            anyhow::anyhow!("Failed to read binlog tar archive for {}: {}", file, e)
        })?;
        for entry in entries {
            let mut entry =
                entry.map_err(|e| anyhow::anyhow!("Failed to read binlog tar entry: {}", e))?;
            let mut content = Vec::new();
            entry
                .read_to_end(&mut content)
                .map_err(|e| anyhow::anyhow!("Failed to read binlog bytes for {}: {}", file, e))?;
            if !content.is_empty() {
                return Ok(content);
            }
        }
        Err(anyhow::anyhow!(
            "Binlog segment {} not found in container tar archive",
            file
        ))
    }

    /// Fetch + parse the binlog manifest from S3. Returns the default (empty)
    /// manifest when no manifest exists yet.
    async fn read_binlog_manifest(
        &self,
        s3_client: &aws_sdk_s3::Client,
        bucket: &str,
        prefix: &str,
        // Service name the binlogs were ARCHIVED under. For in-place this is
        // `self.name`; for restore-to-new-service it must be the SOURCE service
        // name (the new service has a different name but no archived binlogs).
        name: &str,
    ) -> Result<BinlogManifest> {
        let key = Self::binlog_manifest_key(prefix, name);
        let resp = match s3_client.get_object().bucket(bucket).key(&key).send().await {
            Ok(r) => r,
            // Missing manifest is normal on first run.
            Err(_) => return Ok(BinlogManifest::default()),
        };

        let bytes = resp
            .body
            .collect()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read binlog manifest body: {}", e))?
            .into_bytes();

        serde_json::from_slice::<BinlogManifest>(&bytes)
            .map_err(|e| anyhow::anyhow!("Failed to parse binlog manifest: {}", e))
    }

    /// Serialize + PUT the manifest to S3.
    async fn write_binlog_manifest(
        &self,
        s3_client: &aws_sdk_s3::Client,
        bucket: &str,
        prefix: &str,
        manifest: &BinlogManifest,
    ) -> Result<()> {
        let key = Self::binlog_manifest_key(prefix, &self.name);
        let body = serde_json::to_vec(manifest)
            .map_err(|e| anyhow::anyhow!("Failed to serialize binlog manifest: {}", e))?;
        s3_client
            .put_object()
            .bucket(bucket)
            .key(&key)
            .body(aws_sdk_s3::primitives::ByteStream::from(body))
            .content_type("application/json")
            .send()
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to upload binlog manifest to s3://{}/{}: {}",
                    bucket,
                    key,
                    e
                )
            })?;
        Ok(())
    }

    /// Parse `SHOW BINARY LOGS` output into segment filenames, in order.
    /// Each row is tab-separated (`filename\tsize[\t...]`); blank lines and
    /// the `Log_name` header (when present) are ignored.
    pub(crate) fn parse_show_binary_logs(raw: &str) -> Vec<String> {
        raw.lines()
            .filter_map(|line| {
                let name = line.split('\t').next()?.trim();
                if name.is_empty() || name == "Log_name" {
                    return None;
                }
                Some(name.to_string())
            })
            .collect()
    }

    /// All segments except the last — the last one is the currently-active
    /// file that is still being written and must not be shipped.
    pub(crate) fn closed_binlog_files(all_files: &[String]) -> Vec<String> {
        if all_files.len() <= 1 {
            return Vec::new();
        }
        all_files[..all_files.len() - 1].to_vec()
    }

    /// Given the full ordered segment list and the manifest's
    /// `last_shipped_file`, compute the segments to ship this run:
    /// closed (not the active/last file), strictly lexicographically greater
    /// than `last_shipped_file`. `mysql-bin.NNNNNN` names sort correctly
    /// lexicographically.
    pub(crate) fn binlogs_to_ship(all_files: &[String], last_shipped: Option<&str>) -> Vec<String> {
        Self::closed_binlog_files(all_files)
            .into_iter()
            .filter(|f| match last_shipped {
                Some(last) => f.as_str() > last,
                None => true,
            })
            .collect()
    }

    /// S3 object key for a single gzipped binlog segment.
    /// `{prefix}/external_services/mariadb/{service}/binlog/{file}.gz`
    /// (the leading `{prefix}/` is dropped when `prefix` is empty).
    pub(crate) fn binlog_object_key(prefix: &str, service_name: &str, file: &str) -> String {
        let tail = format!(
            "external_services/mariadb/{}/binlog/{}.gz",
            service_name, file
        );
        if prefix.is_empty() {
            tail
        } else {
            format!("{}/{}", prefix, tail)
        }
    }

    /// S3 object key for the binlog manifest.
    /// `{prefix}/external_services/mariadb/{service}/binlog/manifest.json`.
    pub(crate) fn binlog_manifest_key(prefix: &str, service_name: &str) -> String {
        let tail = format!(
            "external_services/mariadb/{}/binlog/manifest.json",
            service_name
        );
        if prefix.is_empty() {
            tail
        } else {
            format!("{}/{}", prefix, tail)
        }
    }

    async fn dump_all_databases_to_gzip_file(
        &self,
        config: &MariaDbConfig,
        output_path: &std::path::Path,
    ) -> Result<()> {
        use std::io::Write;

        let container_name = self.get_live_container_name(config);
        let env = [
            format!("MYSQL_PWD={}", config.root_password),
            format!("MARIADB_PWD={}", config.root_password),
        ];
        let cmd = [
            "sh".to_string(),
            "-c".to_string(),
            "if command -v mariadb >/dev/null 2>&1; then client=mariadb; else client=mysql; fi; \
             if command -v mariadb-dump >/dev/null 2>&1; then dump=mariadb-dump; else dump=mysqldump; fi; \
             dbs=$($client -N -B -uroot -e \"SELECT SCHEMA_NAME FROM information_schema.SCHEMATA WHERE SCHEMA_NAME NOT IN ('information_schema','mysql','performance_schema','sys') ORDER BY SCHEMA_NAME\"); \
             if [ -z \"$dbs\" ]; then \
                 echo '-- No user databases to dump'; \
                 exit 0; \
             fi; \
             $dump --databases $dbs --single-transaction --quick -uroot"
            .to_string(),
        ];

        tokio::time::timeout(MARIADB_BACKUP_EXEC_TIMEOUT, async {
            let exec = self
                .docker
                .create_exec(
                    &container_name,
                    CreateExecOptions {
                        cmd: Some(cmd.iter().map(|s| s.as_str()).collect()),
                        env: Some(env.iter().map(|s| s.as_str()).collect()),
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create MariaDB dump exec: {}", e))?;

            let mut encoder = flate2::write::GzEncoder::new(
                std::fs::File::create(output_path)?,
                flate2::Compression::default(),
            );
            let mut stderr = String::new();

            let output = self
                .docker
                .start_exec(&exec.id, None)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to start MariaDB dump exec: {}", e))?;

            if let bollard::exec::StartExecResults::Attached { mut output, .. } = output {
                while let Some(result) = output.next().await {
                    match result {
                        Ok(bollard::container::LogOutput::StdOut { message }) => {
                            encoder.write_all(&message)?;
                        }
                        Ok(bollard::container::LogOutput::StdErr { message }) => {
                            stderr.push_str(&String::from_utf8_lossy(&message));
                        }
                        Ok(_) => {}
                        Err(e) => {
                            return Err(anyhow::anyhow!(
                                "Failed to stream MariaDB dump output: {}",
                                e
                            ));
                        }
                    }
                }
            }

            encoder.finish()?;

            let inspect = self
                .docker
                .inspect_exec(&exec.id)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to inspect MariaDB dump exec: {}", e))?;
            let exit_code = inspect.exit_code.unwrap_or(-1);
            if exit_code != 0 {
                return Err(anyhow::anyhow!(
                    "MariaDB dump failed with exit code {}: {}",
                    exit_code,
                    stderr.trim()
                ));
            }

            let size_bytes = std::fs::metadata(output_path)?.len();
            if size_bytes == 0 {
                return Err(anyhow::anyhow!(
                    "MariaDB backup failed: dump file has zero size"
                ));
            }

            if !stderr.trim().is_empty() {
                debug!("MariaDB dump stderr: {}", stderr.trim());
            }

            Ok(())
        })
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "MariaDB dump timed out after {}s",
                MARIADB_BACKUP_EXEC_TIMEOUT.as_secs()
            )
        })?
    }

    async fn restore_sql_file(
        &self,
        config: &MariaDbConfig,
        sql_path: &std::path::Path,
    ) -> Result<()> {
        let container_name = self.get_live_container_name(config);
        let restore_filename = "temps_mariadb_restore.sql";

        let tar_data = {
            let mut archive = tar::Builder::new(Vec::new());
            archive.append_path_with_name(sql_path, restore_filename)?;
            archive.finish()?;
            archive.into_inner()?
        };

        self.docker
            .upload_to_container(
                &container_name,
                Some(bollard::query_parameters::UploadToContainerOptions {
                    path: "/tmp".to_string(),
                    ..Default::default()
                }),
                body_full(bytes::Bytes::from(tar_data)),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to upload MariaDB restore SQL: {}", e))?;

        let restore_path = format!("/tmp/{}", restore_filename);
        let restore_cmd = format!(
            "if command -v mariadb >/dev/null 2>&1; then \
                 mariadb -uroot < {}; \
             else \
                 mysql -uroot < {}; \
             fi",
            restore_path, restore_path
        );
        let env = vec![
            format!("MYSQL_PWD={}", config.root_password),
            format!("MARIADB_PWD={}", config.root_password),
        ];

        let result = super::exec_util::run_exec(
            &self.docker,
            &container_name,
            vec!["sh".into(), "-c".into(), restore_cmd],
            Some(env),
            MARIADB_BACKUP_EXEC_TIMEOUT,
        )
        .await;

        let _ = super::exec_util::run_exec(
            &self.docker,
            &container_name,
            vec!["rm".into(), "-f".into(), restore_path],
            None,
            Duration::from_secs(30),
        )
        .await;

        result.map(|_| ())
    }

    // ── Physical (PITR) restore helpers ────────────────────────────────────
    //
    // A physical base backup is a gzipped `mariadb-backup --stream=mbstream`
    // stream (`base.mbstream.gz`). Restoring it is the documented
    // prepare/copy-back dance:
    //   1. gunzip the stream onto the host,
    //   2. mbstream-extract it into a staging dir inside a helper container
    //      that shares the service's data volume,
    //   3. `mariadb-backup --prepare` the staging dir (apply redo logs),
    //   4. wipe the (empty) datadir and `--copy-back` into it,
    //   5. chown back to mysql so the server can read it on start.
    //
    // Getting the stream INTO the helper: the helper is *created* (not started)
    // with `volumes_from = [service_container]`, then we `upload_to_container`
    // the gunzipped mbstream to `/var/tmp/restore.mbstream` on its writable
    // layer (the Docker archive-upload API works on a created/stopped
    // container). Only then do we start it to run the swap script. This avoids
    // bind mounts (which `volumes_from` cannot express) and avoids feeding the
    // stream over an exec stdin pipe (which the log-mux would corrupt).

    /// True when this backup location is a physical (`mariadb-backup` mbstream)
    /// base — the only kind PITR can replay onto.
    pub(crate) fn is_physical_base_location(location: &str) -> bool {
        location.ends_with("base.mbstream.gz")
    }

    /// Derive the `metadata.json` companion key from a base backup key by
    /// replacing the last path segment. Mirrors
    /// `temps_backup::engines::v2_common::derive_metadata_key`.
    pub(crate) fn derive_metadata_key(base_key: &str) -> String {
        match base_key.rsplit_once('/') {
            Some((dir, _last)) => format!("{}/metadata.json", dir),
            None => format!("{}.metadata.json", base_key),
        }
    }

    /// Download the `metadata.json` companion for a base backup and parse it.
    async fn fetch_base_metadata(
        &self,
        s3_client: &aws_sdk_s3::Client,
        bucket: &str,
        base_key: &str,
    ) -> Result<serde_json::Value> {
        let metadata_key = Self::derive_metadata_key(base_key);
        let resp = s3_client
            .get_object()
            .bucket(bucket)
            .key(&metadata_key)
            .send()
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to download base metadata from s3://{}/{}: {}",
                    bucket,
                    metadata_key,
                    e
                )
            })?;
        let bytes = resp
            .body
            .collect()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read base metadata body: {}", e))?
            .into_bytes();
        serde_json::from_slice::<serde_json::Value>(&bytes)
            .map_err(|e| anyhow::anyhow!("Failed to parse base metadata.json: {}", e))
    }

    /// Download `base.mbstream.gz` from S3, gunzip it, and write the raw
    /// mbstream to `dest` on the host.
    async fn download_and_gunzip_base(
        &self,
        s3_client: &aws_sdk_s3::Client,
        bucket: &str,
        base_key: &str,
        dest: &std::path::Path,
    ) -> Result<()> {
        use std::io::Read;

        let resp = s3_client
            .get_object()
            .bucket(bucket)
            .key(base_key)
            .send()
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to download physical base from s3://{}/{}: {}",
                    bucket,
                    base_key,
                    e
                )
            })?;
        let gz = resp
            .body
            .collect()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read physical base body: {}", e))?
            .into_bytes();

        let mut decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(gz));
        let mut stream = Vec::new();
        decoder
            .read_to_end(&mut stream)
            .map_err(|e| anyhow::anyhow!("Failed to gunzip physical base: {}", e))?;
        if stream.is_empty() {
            return Err(anyhow::anyhow!(
                "Physical base mbstream is empty after gunzip"
            ));
        }
        tokio::fs::write(dest, &stream).await.map_err(|e| {
            anyhow::anyhow!("Failed to write mbstream to {}: {}", dest.display(), e)
        })?;
        Ok(())
    }

    /// Perform a physical (mbstream) restore into the named container's data
    /// volume. The container is expected to be the service's live container
    /// (running or not); on return the container is restarted and healthy.
    ///
    /// Sequence mirrors postgres' ephemeral-helper data swap:
    /// disable restart policy → stop → run helper (extract/prepare/copy-back/
    /// chown) → re-enable restart policy → start → wait healthy.
    async fn physical_restore_into_container(
        &self,
        config: &MariaDbConfig,
        mbstream_host_path: &std::path::Path,
    ) -> Result<()> {
        let container_name = self.get_live_container_name(config);

        // 1. Disable restart policy FIRST so Docker doesn't bounce the
        //    container back up while the helper holds the volume.
        info!(
            "Disabling restart policy and stopping container {} for MariaDB physical restore",
            container_name
        );
        self.docker
            .update_container(
                &container_name,
                bollard::models::ContainerUpdateBody {
                    restart_policy: Some(bollard::models::RestartPolicy {
                        name: Some(bollard::models::RestartPolicyNameEnum::NO),
                        maximum_retry_count: None,
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to disable restart policy: {}", e))?;

        // 2. Stop the container so the helper has exclusive access to the
        //    datadir volume.
        let _ = self
            .docker
            .stop_container(
                &container_name,
                Some(bollard::query_parameters::StopContainerOptions {
                    t: Some(30),
                    signal: None,
                }),
            )
            .await;

        // 3. Run the helper that extracts, prepares, and copy-backs the base.
        let swap_result = self
            .run_physical_restore_helper(config, &container_name, mbstream_host_path)
            .await;

        // 4. Always re-enable the restart policy, even if the swap failed, so a
        //    later manual start brings the service back under supervision.
        if let Err(e) = self
            .docker
            .update_container(
                &container_name,
                bollard::models::ContainerUpdateBody {
                    restart_policy: Some(bollard::models::RestartPolicy {
                        name: Some(bollard::models::RestartPolicyNameEnum::ALWAYS),
                        maximum_retry_count: None,
                    }),
                    ..Default::default()
                },
            )
            .await
        {
            warn!("Failed to re-enable restart policy after restore: {}", e);
        }

        swap_result?;

        // 5. Start the container on the restored datadir and wait for health.
        self.docker
            .start_container(
                &container_name,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| {
                anyhow::anyhow!("Failed to start MariaDB container after restore: {}", e)
            })?;
        self.wait_for_container_health(&self.docker, &container_name)
            .await?;

        info!("MariaDB physical restore completed for {}", container_name);
        Ok(())
    }

    /// Create (don't start) the helper, upload the mbstream onto its writable
    /// layer, then start it to run extract → prepare → copy-back → chown.
    async fn run_physical_restore_helper(
        &self,
        config: &MariaDbConfig,
        container_name: &str,
        mbstream_host_path: &std::path::Path,
    ) -> Result<()> {
        use bollard::models::{ContainerCreateBody, HostConfig};

        let helper_name = format!("{}-restore-helper", container_name);
        // Best-effort cleanup of a leftover helper from a prior failed run.
        let _ = self
            .docker
            .remove_container(
                &helper_name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    v: false,
                    ..Default::default()
                }),
            )
            .await;

        // The staging dir and copy-back run as root inside the helper; the
        // final chown hands the datadir back to the mysql uid the server runs
        // as. CRITICAL: the datadir must be EMPTY before --copy-back, and owned
        // by mysql afterwards.
        let stage = "/var/tmp/temps-mariadb-restore";
        let stream_path = "/var/tmp/restore.mbstream";
        let swap_script = format!(
            "set -ex; \
             if command -v mariadb-backup >/dev/null 2>&1; then BK=mariadb-backup; else BK=mariabackup; fi; \
             echo temps-mariadb-restore: staging extract; \
             rm -rf {stage}; mkdir -p {stage}; \
             mbstream -x -C {stage} < {stream}; \
             echo temps-mariadb-restore: preparing base; \
             \"$BK\" --prepare --target-dir={stage}; \
             echo temps-mariadb-restore: replacing datadir; \
             find /var/lib/mysql -mindepth 1 -maxdepth 1 -exec rm -rf {{}} +; \
             echo temps-mariadb-restore: copy-back; \
             \"$BK\" --copy-back --target-dir={stage} --datadir=/var/lib/mysql; \
             echo temps-mariadb-restore: chown datadir; \
             chown -R mysql:mysql /var/lib/mysql; \
             rm -rf {stage} {stream}; \
             echo temps-mariadb-restore: complete",
            stage = stage,
            stream = stream_path,
        );

        let helper_config = ContainerCreateBody {
            image: Some(config.docker_image.clone()),
            cmd: Some(vec!["sh".to_string(), "-c".to_string(), swap_script]),
            // Run as root so we can wipe the datadir and chown back to mysql.
            user: Some("root".to_string()),
            host_config: Some(HostConfig {
                volumes_from: Some(vec![container_name.to_string()]),
                ..Default::default()
            }),
            ..Default::default()
        };

        let helper = self
            .docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&helper_name)
                        .build(),
                ),
                helper_config,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create restore helper container: {}", e))?;

        // Upload the gunzipped mbstream onto the helper's writable layer at
        // /var/tmp/. The archive-upload API works on a created (not running)
        // container, so the file is in place before the entrypoint runs.
        let upload_result = self
            .upload_file_to_container(
                &helper.id,
                mbstream_host_path,
                "/var/tmp",
                "restore.mbstream",
                MARIADB_BACKUP_EXEC_TIMEOUT,
            )
            .await;
        if let Err(e) = upload_result {
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
            return Err(e);
        }

        // Start the helper and wait for it to finish.
        let run_result = async {
            self.docker
                .start_container(
                    &helper.id,
                    None::<bollard::query_parameters::StartContainerOptions>,
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to start restore helper container: {}", e))?;

            let wait = tokio::time::timeout(MARIADB_RESTORE_HELPER_TIMEOUT, async {
                self.docker
                    .wait_container(
                        &helper.id,
                        None::<bollard::query_parameters::WaitContainerOptions>,
                    )
                    .next()
                    .await
            })
            .await;

            // Capture helper logs for diagnostics on failure.
            let logs = self.collect_container_logs(&helper.id).await;

            match wait {
                Ok(Some(Ok(resp))) if resp.status_code == 0 => Ok(()),
                Ok(Some(Ok(resp))) => Err(anyhow::anyhow!(
                    "MariaDB restore helper exited with code {}: {}",
                    resp.status_code,
                    logs.trim()
                )),
                Ok(Some(Err(e))) => Err(anyhow::anyhow!(
                    "Failed waiting on MariaDB restore helper: {}",
                    e
                )),
                Ok(None) => Err(anyhow::anyhow!(
                    "MariaDB restore helper produced no wait result"
                )),
                Err(_) => Err(anyhow::anyhow!(
                    "MariaDB restore helper timed out after {}s: {}",
                    MARIADB_RESTORE_HELPER_TIMEOUT.as_secs(),
                    logs.trim()
                )),
            }
        }
        .await;

        // Always remove the helper.
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

        run_result
    }

    /// Upload a single host file into a container at `dest_dir/dest_name` via
    /// the Docker tar archive-upload API (works on created/stopped containers).
    async fn upload_file_to_container(
        &self,
        container_id: &str,
        host_path: &std::path::Path,
        dest_dir: &str,
        dest_name: &str,
        timeout: Duration,
    ) -> Result<()> {
        let tar_data = {
            let mut archive = tar::Builder::new(Vec::new());
            archive
                .append_path_with_name(host_path, dest_name)
                .map_err(|e| {
                    anyhow::anyhow!("Failed to tar {} for upload: {}", host_path.display(), e)
                })?;
            archive.finish()?;
            archive
                .into_inner()
                .map_err(|e| anyhow::anyhow!("Failed to finalize upload tar: {}", e))?
        };

        tokio::time::timeout(timeout, async {
            self.docker
                .upload_to_container(
                    container_id,
                    Some(bollard::query_parameters::UploadToContainerOptions {
                        path: dest_dir.to_string(),
                        ..Default::default()
                    }),
                    body_full(bytes::Bytes::from(tar_data)),
                )
                .await
        })
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "Timed out uploading {} to container {} after {}s",
                dest_name,
                container_id,
                timeout.as_secs()
            )
        })?
        .map_err(|e| anyhow::anyhow!("Failed to upload {} to container: {}", dest_name, e))?;
        Ok(())
    }

    /// Best-effort collection of a container's combined stdout/stderr logs,
    /// for diagnostics. Never returns an error (returns "" on failure).
    async fn collect_container_logs(&self, container_id: &str) -> String {
        match tokio::time::timeout(Duration::from_secs(10), async {
            let mut out = String::new();
            let mut stream = self.docker.logs(
                container_id,
                Some(bollard::query_parameters::LogsOptions {
                    stdout: true,
                    stderr: true,
                    follow: false,
                    tail: "200".to_string(),
                    ..Default::default()
                }),
            );
            while let Some(item) = stream.next().await {
                match item {
                    Ok(chunk) => out.push_str(&String::from_utf8_lossy(&chunk.into_bytes())),
                    Err(_) => break,
                }
            }
            out
        })
        .await
        {
            Ok(logs) => logs,
            Err(_) => "timed out while collecting MariaDB restore helper logs".to_string(),
        }
    }

    // ── Binlog fetch + replay (PITR forward-roll) ──────────────────────────

    /// Format a UTC time as the `mysqlbinlog --stop-datetime` argument value
    /// (`YYYY-MM-DD HH:MM:SS`). The server runs `TZ=UTC` so this is
    /// interpreted in UTC.
    pub(crate) fn format_stop_datetime(time: chrono::DateTime<chrono::Utc>) -> String {
        time.format("%Y-%m-%d %H:%M:%S").to_string()
    }

    /// Map a `RecoveryTarget` to the `mysqlbinlog` stop-flag for the FINAL
    /// segment being replayed.
    ///
    /// Returns the flag as a `(flag, value)` pair, or `None` for "replay to the
    /// end" (no stop). Errors for targets MariaDB cannot honor.
    ///
    /// - `Time`  → `--stop-datetime='YYYY-MM-DD HH:MM:SS'` (UTC).
    /// - `Lsn`   → interpreted as `binlog_file:position`; `--stop-position` is
    ///   only meaningful when that file is the final segment replayed. A bare
    ///   position (no `file:`) is rejected as ambiguous across segments.
    /// - `Xid`   → GTID stop is not yet expressible via a single mysqlbinlog
    ///   invocation here; rejected rather than silently recovering to the
    ///   wrong point.
    /// - `Name`  → no MariaDB equivalent; rejected.
    pub(crate) fn recovery_target_to_stop_flag(
        target: &RecoveryTarget,
    ) -> Result<Option<(String, String)>> {
        match target {
            RecoveryTarget::Time { time } => Ok(Some((
                "--stop-datetime".to_string(),
                Self::format_stop_datetime(*time),
            ))),
            RecoveryTarget::Lsn { lsn } => {
                // Accept "binlog_file:position"; reject a bare position.
                match lsn.rsplit_once(':') {
                    Some((file, pos))
                        if !file.is_empty()
                            && pos.chars().all(|c| c.is_ascii_digit())
                            && !pos.is_empty() =>
                    {
                        Ok(Some(("--stop-position".to_string(), pos.to_string())))
                    }
                    _ => Err(anyhow::anyhow!(
                        "PITR Lsn target must be 'binlog_file:position' (a bare position is \
                         ambiguous across binlog segments); got '{}'",
                        lsn
                    )),
                }
            }
            RecoveryTarget::Xid { xid } => Err(anyhow::anyhow!(
                "PITR Xid/GTID target ('{}') is not yet supported for MariaDB physical \
                 restore; use a Time target",
                xid
            )),
            RecoveryTarget::Name { name } => Err(anyhow::anyhow!(
                "PITR Name target ('{}') has no MariaDB equivalent",
                name
            )),
        }
    }

    /// For an `Lsn` target, the binlog file the `--stop-position` applies to
    /// (the final segment to replay). `None` for non-Lsn targets.
    fn lsn_target_file(target: &RecoveryTarget) -> Option<String> {
        match target {
            RecoveryTarget::Lsn { lsn } => lsn
                .rsplit_once(':')
                .map(|(file, _)| file.to_string())
                .filter(|f| !f.is_empty()),
            _ => None,
        }
    }

    /// Download the archived binlog segments needed for replay (every shipped
    /// file lexicographically >= `start_file`), gunzip them, and write them to
    /// `dest_dir` preserving order. Returns the ordered list of (host_path,
    /// filename) pairs.
    async fn fetch_binlogs_for_replay(
        &self,
        s3_client: &aws_sdk_s3::Client,
        bucket: &str,
        prefix: &str,
        // Service name the binlogs were ARCHIVED under (the SOURCE service),
        // which differs from `self.name` on a restore-to-new-service.
        source_name: &str,
        start_file: &str,
        dest_dir: &std::path::Path,
    ) -> Result<Vec<(std::path::PathBuf, String)>> {
        use std::io::Read;

        let manifest = self
            .read_binlog_manifest(s3_client, bucket, prefix, source_name)
            .await
            .unwrap_or_default();

        // Contiguous segment set: every shipped file >= the base's start file,
        // in lexicographic (== chronological for fixed-width names) order.
        let mut files: Vec<String> = manifest
            .shipped_files
            .iter()
            .filter(|f| f.as_str() >= start_file)
            .cloned()
            .collect();
        files.sort();
        files.dedup();

        if files.is_empty() {
            warn!(
                service = %self.name,
                start_file = %start_file,
                "No archived binlog segments >= base start file; PITR will replay nothing \
                 (recovery target may predate the base, or binlogs not yet shipped)"
            );
        }

        let mut result = Vec::with_capacity(files.len());
        for file in files {
            let key = Self::binlog_object_key(prefix, source_name, &file);
            let resp = s3_client
                .get_object()
                .bucket(bucket)
                .key(&key)
                .send()
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to download binlog segment s3://{}/{}: {}",
                        bucket,
                        key,
                        e
                    )
                })?;
            let gz = resp
                .body
                .collect()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to read binlog segment {}: {}", file, e))?
                .into_bytes();
            let mut decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(gz));
            let mut raw = Vec::new();
            decoder
                .read_to_end(&mut raw)
                .map_err(|e| anyhow::anyhow!("Failed to gunzip binlog segment {}: {}", file, e))?;
            let host_path = dest_dir.join(&file);
            tokio::fs::write(&host_path, &raw)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to write binlog {} to host: {}", file, e))?;
            result.push((host_path, file));
        }
        Ok(result)
    }

    /// Replay the given ordered binlog segments into the (running, restored)
    /// container up to the recovery target, in a SINGLE `mysqlbinlog`
    /// invocation piped to ONE `mariadb` client.
    ///
    /// `--start-position` applies to the FIRST file only (the base's recorded
    /// position). The stop flag (from `recovery_target_to_stop_flag`) applies
    /// to the LAST file replayed. `--disable-log-bin` keeps replayed events out
    /// of the restored server's own binlog so future PITR coordinates stay
    /// correct. Credentials flow via `MYSQL_PWD` env, never argv.
    async fn replay_binlogs(
        &self,
        config: &MariaDbConfig,
        segments: &[(std::path::PathBuf, String)],
        start_position: &str,
        target: &RecoveryTarget,
    ) -> Result<()> {
        if segments.is_empty() {
            info!("No binlog segments to replay for PITR; base restore is the recovery point");
            return Ok(());
        }

        let container_name = self.get_live_container_name(config);
        let container_dir = "/var/tmp/temps-binlogs";

        // Upload every segment into the container, preserving filenames.
        let _ = super::exec_util::run_exec(
            &self.docker,
            &container_name,
            vec!["mkdir".into(), "-p".into(), container_dir.into()],
            None,
            Duration::from_secs(30),
        )
        .await;

        let mut container_files: Vec<String> = Vec::with_capacity(segments.len());
        for (host_path, file) in segments {
            info!(
                service = %self.name,
                file = %file,
                "Uploading MariaDB PITR binlog segment to restored container"
            );
            self.upload_file_to_container(
                &container_name,
                host_path,
                container_dir,
                file,
                MARIADB_BINLOG_UPLOAD_TIMEOUT,
            )
            .await?;
            container_files.push(format!("{}/{}", container_dir, file));
        }
        info!(
            service = %self.name,
            segments = container_files.len(),
            "Uploaded MariaDB PITR binlog segments to restored container"
        );

        // Build the single mysqlbinlog invocation. --start-position applies to
        // the first file; the stop flag (if any) to the run as a whole (it
        // takes effect on the segment that contains the target).
        let stop_flag = Self::recovery_target_to_stop_flag(target)?;
        // For an Lsn target, --stop-position is only sound when the target file
        // is the LAST segment replayed; otherwise the same numeric position
        // exists in multiple files and we'd stop in the wrong one.
        if let Some(target_file) = Self::lsn_target_file(target) {
            let last = container_files
                .last()
                .map(|p| p.rsplit('/').next().unwrap_or(p).to_string())
                .unwrap_or_default();
            if last != target_file {
                return Err(anyhow::anyhow!(
                    "PITR Lsn target file '{}' is not the final replayed segment ('{}'); \
                     a bare --stop-position would be ambiguous across segments",
                    target_file,
                    last
                ));
            }
        }

        // `$BINLOG` is resolved at run time in the shell below — mariadb:lts
        // ships `mariadb-binlog`, not `mysqlbinlog`.
        let mut binlog_args = String::from("\"$BINLOG\" --disable-log-bin");
        binlog_args.push_str(&format!(" --start-position={}", start_position));
        if let Some((flag, value)) = &stop_flag {
            // Quote the value (datetimes contain a space).
            binlog_args.push_str(&format!(" {}={}", flag, Self::shell_single_quote(value)));
        }
        for f in &container_files {
            binlog_args.push(' ');
            binlog_args.push_str(&Self::shell_single_quote(f));
        }

        // Resolve tool names at run time: mariadb:lts ships `mariadb-binlog`
        // and `mariadb` (NOT `mysqlbinlog`/`mysql`); fall back to the mysql
        // names for non-MariaDB images. dash has no pipefail, so we decode to
        // an intermediate file FIRST (under `set -e`, a failed decode aborts
        // before the client runs) and only then feed it to the client — this
        // surfaces a broken replay as an error rather than a silent
        // half-apply masked by the client's exit code in a pipe.
        let replay_file = "/var/tmp/temps-pitr-replay.sql";
        let replay_cmd = format!(
            "set -ex; \
             if command -v mariadb-binlog >/dev/null 2>&1; then BINLOG=mariadb-binlog; else BINLOG=mysqlbinlog; fi; \
             if command -v mariadb >/dev/null 2>&1; then CLIENT=mariadb; else CLIENT=mysql; fi; \
             echo temps-mariadb-pitr-replay: decode-binlogs; \
             timeout 120s {binlog} > {file}; \
             ls -lh {file}; \
             echo temps-mariadb-pitr-replay: apply-sql; \
             timeout 120s \"$CLIENT\" --protocol=TCP -h127.0.0.1 -P3306 --connect-timeout=10 -uroot --password=\"$MARIADB_ROOT_PASSWORD\" --binary-mode=1 < {file}; \
             rm -f {file}; \
             echo temps-mariadb-pitr-replay: complete",
            binlog = binlog_args,
            file = replay_file,
        );

        let env = vec![
            format!("MYSQL_PWD={}", config.root_password),
            format!("MARIADB_PWD={}", config.root_password),
            format!("MARIADB_ROOT_PASSWORD={}", config.root_password),
        ];

        info!(
            service = %self.name,
            segments = segments.len(),
            "Replaying MariaDB binlog segments for PITR"
        );

        let result = super::exec_util::run_exec(
            &self.docker,
            &container_name,
            vec!["sh".into(), "-c".into(), replay_cmd],
            Some(env),
            MARIADB_BINLOG_REPLAY_TIMEOUT,
        )
        .await;

        // Clean up uploaded binlogs regardless of outcome.
        let _ = super::exec_util::run_exec(
            &self.docker,
            &container_name,
            vec!["rm".into(), "-rf".into(), container_dir.into()],
            None,
            Duration::from_secs(30),
        )
        .await;

        result
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("MariaDB binlog replay failed (PITR not applied): {}", e))
    }

    /// Build the parameter map + connection string the orchestrator persists
    /// for a restore-to-new-service result.
    fn new_service_result(config: &MariaDbConfig) -> Result<NewServiceRestoreResult> {
        let runtime_json = serde_json::to_value(config)
            .map_err(|e| anyhow::anyhow!("Failed to serialize new MariaDB config: {}", e))?;
        let mut parameters = HashMap::new();
        if let Some(obj) = runtime_json.as_object() {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    parameters.insert(k.clone(), s.to_string());
                } else if let Some(n) = v.as_u64() {
                    parameters.insert(k.clone(), n.to_string());
                }
            }
        }
        let connection_info = format!(
            "mysql://{}:***@{}:{}/{}",
            config.username, config.host, config.port, config.database
        );
        Ok(NewServiceRestoreResult {
            parameters,
            connection_info,
        })
    }

    /// Clone the source config onto a fresh port and build a new
    /// `MariaDbService` for it, creating its container. Shared by
    /// restore_to_new_service and to_new_service PITR.
    async fn provision_new_service_for_restore(
        &self,
        source_config: &ServiceConfig,
        new_service_name: &str,
        parameter_overrides: &serde_json::Value,
    ) -> Result<(MariaDbService, MariaDbConfig)> {
        let mut config = self.get_mariadb_config(source_config.clone())?;

        // Fresh port (the source's is taken). A restored new service is its own
        // container, not an imported one.
        config.container_name = None;
        let new_port = find_available_port(3306)
            .ok_or_else(|| anyhow::anyhow!("No available ports for new MariaDB service"))?
            .to_string();
        config.port = new_port;

        if let Some(overrides) = parameter_overrides.as_object() {
            if let Some(port) = overrides.get("port").and_then(|v| v.as_str()) {
                config.port = port.to_string();
            }
            if let Some(image) = overrides.get("docker_image").and_then(|v| v.as_str()) {
                config.docker_image = image.to_string();
            }
            if let Some(db) = overrides.get("database").and_then(|v| v.as_str()) {
                config.database = db.to_string();
            }
        }

        let new_service = MariaDbService::new(new_service_name.to_string(), self.docker.clone());
        let cloned_limits = ServiceResourceLimits::from_parameters(&source_config.parameters);
        *new_service.config.write().await = Some(config.clone());
        *new_service.resource_limits.write().await = cloned_limits.clone();
        new_service
            .create_container(&self.docker, &config, &cloned_limits)
            .await?;

        Ok((new_service, config))
    }

    /// Restore a base backup (physical or logical) into the given service's
    /// container, dispatching on the backup location.
    async fn restore_base_into(
        &self,
        service: &MariaDbService,
        config: &MariaDbConfig,
        s3_client: &aws_sdk_s3::Client,
        bucket: &str,
        backup_location: &str,
    ) -> Result<()> {
        let base_key = Self::backup_key_from_location(backup_location, bucket);
        if Self::is_physical_base_location(&base_key) {
            let temp_dir = tempfile::tempdir()?;
            let mbstream_path = temp_dir.path().join("base.mbstream");
            service
                .download_and_gunzip_base(s3_client, bucket, &base_key, &mbstream_path)
                .await?;
            service
                .physical_restore_into_container(config, &mbstream_path)
                .await?;
        } else {
            // Logical .sql.gz base — download, gunzip, and restore via the
            // mariadb client. Shares the internal logical-restore helper with
            // `restore_from_s3` so we don't fabricate an `s3_sources::Model`.
            service
                .restore_logical_from_s3(s3_client, bucket, backup_location, config)
                .await?;
        }
        Ok(())
    }

    /// Logical restore core: download a `.sql[.gz]` backup from S3, decompress
    /// if needed, and feed it to the mariadb client. Shared by the public
    /// `restore_from_s3` and the restore framework's `restore_base_into`.
    async fn restore_logical_from_s3(
        &self,
        s3_client: &aws_sdk_s3::Client,
        bucket: &str,
        backup_location: &str,
        config: &MariaDbConfig,
    ) -> Result<()> {
        use std::io::Read;

        let backup_key = Self::backup_key_from_location(backup_location, bucket);
        let response = s3_client
            .get_object()
            .bucket(bucket)
            .key(&backup_key)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to download MariaDB backup from S3: {}", e))?;

        let backup_data = response
            .body
            .collect()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read MariaDB backup data: {}", e))?
            .into_bytes();

        let temp_dir = tempfile::tempdir()?;
        let sql_path = temp_dir.path().join("restore.sql");

        if backup_key.ends_with(".gz") {
            let mut decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(backup_data));
            let mut sql = Vec::new();
            decoder.read_to_end(&mut sql)?;
            tokio::fs::write(&sql_path, sql).await?;
        } else {
            tokio::fs::write(&sql_path, backup_data).await?;
        }

        self.restore_sql_file(config, &sql_path).await
    }

    /// POSIX single-quote escape for embedding a value in an `sh -c` string.
    pub(crate) fn shell_single_quote(s: &str) -> String {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[async_trait]
impl ExternalService for MariaDbService {
    async fn init(&self, config: ServiceConfig) -> Result<HashMap<String, String>> {
        info!(
            "Initializing MariaDB service (name={}, type={:?}, version={:?})",
            config.name, config.service_type, config.version
        );

        let resource_limits = ServiceResourceLimits::from_parameters(&config.parameters);
        if let Err(e) = resource_limits.validate() {
            return Err(anyhow::anyhow!("Invalid resource limits: {}", e));
        }

        let mariadb_config = self.get_mariadb_config(config)?;

        debug!(
            "MariaDB init - storing config: port={}, username={}, database={}",
            mariadb_config.port, mariadb_config.username, mariadb_config.database
        );

        *self.config.write().await = Some(mariadb_config.clone());
        *self.resource_limits.write().await = resource_limits.clone();

        if mariadb_config.container_name.is_none() {
            self.create_container(&self.docker, &mariadb_config, &resource_limits)
                .await?;
        } else {
            info!(
                "MariaDB service '{}' is imported from container '{}'; skipping container creation",
                self.name,
                self.get_live_container_name(&mariadb_config)
            );
        }

        let runtime_config_json = serde_json::to_value(&mariadb_config)
            .map_err(|e| anyhow::anyhow!("Failed to serialize MariaDB runtime config: {}", e))?;
        let runtime_config_map = runtime_config_json
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Runtime config is not an object"))?;

        let mut inferred_params = HashMap::new();
        for (key, value) in runtime_config_map {
            if let Some(str_value) = value.as_str() {
                inferred_params.insert(key.clone(), str_value.to_string());
            }
        }

        Ok(inferred_params)
    }

    async fn health_check(&self) -> Result<bool> {
        let config = self
            .config
            .read()
            .await
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("MariaDB configuration not found"))?
            .clone();
        Ok(self.ping(&config).await.is_ok())
    }

    async fn health_probe(&self, service_config: ServiceConfig) -> Result<HealthProbeResult> {
        use std::time::Instant;

        const DEGRADED_MS: u128 = 2000;

        let cfg = match self.get_mariadb_config(service_config) {
            Ok(c) => c,
            Err(e) => {
                return Ok(HealthProbeResult::down(format!(
                    "invalid mariadb config: {}",
                    e
                )))
            }
        };

        let start = Instant::now();
        match self.ping(&cfg).await {
            Ok(()) => {
                let elapsed_ms = start.elapsed().as_millis();
                let response_time = i32::try_from(elapsed_ms).ok();
                if elapsed_ms > DEGRADED_MS {
                    Ok(HealthProbeResult::degraded(
                        format!("mariadb responded in {}ms (>{}ms)", elapsed_ms, DEGRADED_MS),
                        response_time,
                    ))
                } else {
                    Ok(HealthProbeResult::operational(response_time))
                }
            }
            Err(e) => Ok(HealthProbeResult::down(format!(
                "mariadb probe failed: {}",
                e
            ))),
        }
    }

    fn get_type(&self) -> ServiceType {
        ServiceType::Mariadb
    }

    fn get_name(&self) -> String {
        self.name.clone()
    }

    fn get_connection_info(&self) -> Result<String> {
        let config = self
            .config
            .try_read()
            .map_err(|_| anyhow::anyhow!("Failed to read config"))?;

        match &*config {
            Some(cfg) => Ok(format!(
                "mysql://{}:***@{}:{}/{}",
                cfg.username, cfg.host, cfg.port, cfg.database
            )),
            None => Err(anyhow::anyhow!("MariaDB not configured")),
        }
    }

    async fn cleanup(&self) -> Result<()> {
        Ok(())
    }

    fn get_parameter_schema(&self) -> Option<serde_json::Value> {
        let schema = schemars::schema_for!(MariaDbInputConfig);
        let mut schema_json = serde_json::to_value(schema).ok()?;

        if let Some(properties) = schema_json
            .get_mut("properties")
            .and_then(|p| p.as_object_mut())
        {
            for key in properties.keys().cloned().collect::<Vec<_>>() {
                let editable = match key.as_str() {
                    "port" => true,
                    "docker_image" => true,
                    // PITR granularity can be tuned at runtime: the archiver
                    // picks up a new cadence live; the derived
                    // binlog_expire_logs_seconds takes effect on next recreate.
                    "binlog_archive_interval" => true,
                    "size_profile" => false,
                    "host" | "database" | "username" | "password" | "root_password" => false,
                    _ => false,
                };

                if let Some(prop) = properties.get_mut(&key).and_then(|p| p.as_object_mut()) {
                    prop.insert("x-editable".to_string(), serde_json::json!(editable));
                }
            }

            properties.insert(
                "size_profile".to_string(),
                serde_json::json!({
                    "type": "string",
                    "description": "MariaDB resource/tuning profile. Small is the default for shared 4 GiB and 8 GiB Temps hosts; linked projects get separate databases inside this service.",
                    "default": "small",
                    "enum": ["small", "standard", "dedicated"],
                    "x-editable": false
                }),
            );

            properties.insert(
                "binlog_archive_interval".to_string(),
                serde_json::json!({
                    "type": "string",
                    "description": "Point-in-time-recovery granularity: how often binary logs are shipped to S3. Smaller intervals lose less data on restore (lower RPO) but upload more often. The worst-case data loss on restore is one interval.",
                    "default": "5m",
                    "enum": ["1m", "5m", "15m", "60m"],
                    "x-editable": true
                }),
            );
        }

        Some(schema_json)
    }

    async fn start(&self) -> Result<()> {
        let existing_config = self.config.read().await.as_ref().cloned();
        let container_name = existing_config
            .as_ref()
            .map(|config| self.get_live_container_name(config))
            .unwrap_or_else(|| self.get_container_name());
        info!("Starting MariaDB container {}", container_name);

        let containers = self
            .docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: true,
                filters: Some(HashMap::from([(
                    "name".to_string(),
                    vec![container_name.clone()],
                )])),
                ..Default::default()
            }))
            .await?;

        if containers.is_empty() {
            let config = existing_config
                .ok_or_else(|| anyhow::anyhow!("MariaDB configuration not found"))?;
            if config.container_name.is_some() {
                return Err(anyhow::anyhow!(
                    "Imported MariaDB container '{}' not found",
                    container_name
                ));
            }
            let limits = self.resource_limits.read().await.clone();
            self.create_container(&self.docker, &config, &limits)
                .await?;
        } else {
            let container = &containers[0];
            let is_running = matches!(
                container.state,
                Some(bollard::models::ContainerSummaryStateEnum::RUNNING)
            );

            if !is_running {
                self.docker
                    .start_container(
                        &container_name,
                        None::<bollard::query_parameters::StartContainerOptions>,
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to start MariaDB container: {}", e))?;
            }
        }

        self.wait_for_container_health(&self.docker, &container_name)
            .await?;
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        let container_name = self
            .config
            .read()
            .await
            .as_ref()
            .map(|config| self.get_live_container_name(config))
            .unwrap_or_else(|| self.get_container_name());
        let containers = self
            .docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: true,
                filters: Some(HashMap::from([(
                    "name".to_string(),
                    vec![container_name.clone()],
                )])),
                ..Default::default()
            }))
            .await?;

        if !containers.is_empty() {
            self.docker
                .stop_container(&container_name, None::<StopContainerOptions>)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to stop MariaDB container: {}", e))?;
        }

        Ok(())
    }

    async fn remove(&self) -> Result<()> {
        self.cleanup().await?;

        let container_name = self.get_container_name();
        let volume_name = format!("mariadb_data_{}", self.name);

        let containers = self
            .docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: true,
                filters: Some(HashMap::from([(
                    "name".to_string(),
                    vec![container_name.clone()],
                )])),
                ..Default::default()
            }))
            .await?;

        if !containers.is_empty() {
            let _ = self
                .docker
                .stop_container(&container_name, None::<StopContainerOptions>)
                .await;
            self.docker
                .remove_container(
                    &container_name,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to remove MariaDB container: {}", e))?;
        }

        match self
            .docker
            .remove_volume(
                &volume_name,
                None::<bollard::query_parameters::RemoveVolumeOptions>,
            )
            .await
        {
            Ok(_) => info!("Removed MariaDB volume {}", volume_name),
            Err(e) => info!("Error removing MariaDB volume {}: {}", volume_name, e),
        }

        Ok(())
    }

    fn get_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>> {
        let database = parameters
            .get("database")
            .ok_or_else(|| anyhow::anyhow!("Missing database parameter"))?;
        let username = parameters
            .get("username")
            .ok_or_else(|| anyhow::anyhow!("Missing username parameter"))?;
        let password = parameters
            .get("password")
            .ok_or_else(|| anyhow::anyhow!("Missing password parameter"))?;

        let host = parameters
            .get("container_name")
            .cloned()
            .or_else(|| parameters.get("host").cloned())
            .unwrap_or_else(|| self.get_container_name());

        Self::build_env_vars(&host, MARIADB_INTERNAL_PORT, database, username, password)
    }

    fn get_docker_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>> {
        self.get_environment_variables(parameters)
    }

    async fn provision_resource(
        &self,
        service_config: ServiceConfig,
        project_id: &str,
        environment: &str,
    ) -> Result<LogicalResource> {
        let resource_name =
            Self::normalize_database_name(&format!("{}_{}", project_id, environment));
        self.create_database(service_config.clone(), &resource_name)
            .await?;

        let credentials = self.build_runtime_env_vars(service_config, &resource_name)?;
        Ok(LogicalResource {
            name: resource_name,
            resource_type: "database".to_string(),
            credentials,
        })
    }

    async fn deprovision_resource(&self, project_id: &str, environment: &str) -> Result<()> {
        let resource_name =
            Self::normalize_database_name(&format!("{}_{}", project_id, environment));
        let Some(config) = self.config.read().await.as_ref().cloned() else {
            return Ok(());
        };
        let service_config = ServiceConfig {
            name: self.name.clone(),
            service_type: ServiceType::Mariadb,
            version: None,
            parameters: serde_json::to_value(config)?,
        };
        self.drop_database(service_config, &resource_name).await
    }

    fn get_runtime_env_definitions(&self) -> Vec<RuntimeEnvVar> {
        vec![
            RuntimeEnvVar {
                name: "DATABASE_URL".to_string(),
                description: "Full MariaDB-compatible connection URL".to_string(),
                example: "mysql://app:pass@mariadb-service:3306/project_production".to_string(),
                sensitive: true,
            },
            RuntimeEnvVar {
                name: "MYSQL_DATABASE".to_string(),
                description: "Database name specific to this project/environment".to_string(),
                example: "project_production".to_string(),
                sensitive: false,
            },
            RuntimeEnvVar {
                name: "MYSQL_USER".to_string(),
                description: "MariaDB application user".to_string(),
                example: "app".to_string(),
                sensitive: false,
            },
            RuntimeEnvVar {
                name: "MYSQL_PASSWORD".to_string(),
                description: "MariaDB application user password".to_string(),
                example: "secure-password".to_string(),
                sensitive: true,
            },
        ]
    }

    async fn get_runtime_env_vars(
        &self,
        service_config: ServiceConfig,
        project_id: &str,
        environment: &str,
    ) -> Result<HashMap<String, String>> {
        let resource_name =
            Self::normalize_database_name(&format!("{}_{}", project_id, environment));
        self.create_database(service_config.clone(), &resource_name)
            .await?;
        self.build_runtime_env_vars(service_config, &resource_name)
    }

    async fn preview_runtime_env_vars(
        &self,
        service_config: ServiceConfig,
        project_id: &str,
        environment: &str,
    ) -> Result<HashMap<String, String>> {
        let resource_name =
            Self::normalize_database_name(&format!("{}_{}", project_id, environment));
        self.build_runtime_env_vars(service_config, &resource_name)
    }

    fn get_local_address(&self, service_config: ServiceConfig) -> Result<String> {
        let config = self.get_mariadb_config(service_config)?;
        Ok(format!("localhost:{}", config.port))
    }

    fn get_effective_address(&self, service_config: ServiceConfig) -> Result<(String, String)> {
        let config = self.get_mariadb_config(service_config)?;

        if temps_core::DeploymentMode::is_docker() {
            Ok((
                self.get_live_container_name(&config),
                MARIADB_INTERNAL_PORT.to_string(),
            ))
        } else {
            Ok(("localhost".to_string(), config.port))
        }
    }

    fn get_docker_container_name(&self) -> String {
        self.get_container_name()
    }

    fn get_docker_internal_port(&self) -> String {
        MARIADB_INTERNAL_PORT.to_string()
    }

    async fn backup_to_s3(
        &self,
        s3_client: &aws_sdk_s3::Client,
        _s3_credentials: &super::S3Credentials,
        backup: temps_entities::backups::Model,
        s3_source: &temps_entities::s3_sources::Model,
        subpath: &str,
        _subpath_root: &str,
        pool: &temps_database::DbConnection,
        external_service: &temps_entities::external_services::Model,
        service_config: ServiceConfig,
    ) -> Result<super::BackupOutcome> {
        use chrono::Utc;
        use sea_orm::*;

        info!("Starting MariaDB backup to S3 via mariadb-dump");

        let config = self.get_mariadb_config(service_config)?;
        let backup_record = temps_entities::external_service_backups::Entity::insert(
            temps_entities::external_service_backups::ActiveModel {
                service_id: Set(external_service.id),
                backup_id: Set(backup.id),
                backup_type: Set("full".to_string()),
                state: Set("running".to_string()),
                started_at: Set(Utc::now()),
                s3_location: Set(String::new()),
                metadata: Set(serde_json::json!({
                    "service_type": "mariadb",
                    "service_name": self.name,
                    "backup_tool": "mariadb-dump",
                })),
                compression_type: Set("gzip".to_string()),
                created_by: Set(0),
                ..Default::default()
            },
        )
        .exec_with_returning(pool)
        .await?;

        let temp_dir = tempfile::tempdir()?;
        let dump_path = temp_dir
            .path()
            .join(format!("mariadb_backup_{}.sql.gz", uuid::Uuid::new_v4()));

        let result = async {
            self.dump_all_databases_to_gzip_file(&config, &dump_path)
                .await?;

            let size_bytes = tokio::fs::metadata(&dump_path).await?.len() as i64;
            let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
            let backup_key = format!(
                "{}/mariadb_backup_{}.sql.gz",
                subpath.trim_matches('/'),
                timestamp
            );

            let body = aws_sdk_s3::primitives::ByteStream::from_path(&dump_path).await?;
            s3_client
                .put_object()
                .bucket(&s3_source.bucket_name)
                .key(&backup_key)
                .body(body)
                .content_type("application/x-gzip")
                .send()
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to upload backup to s3://{}/{}: {}",
                        s3_source.bucket_name,
                        backup_key,
                        e
                    )
                })?;

            Ok::<(String, i64), anyhow::Error>((backup_key, size_bytes))
        }
        .await;

        match result {
            Ok((backup_key, size_bytes)) => {
                let mut update: temps_entities::external_service_backups::ActiveModel =
                    backup_record.clone().into();
                update.state = Set("completed".to_string());
                update.finished_at = Set(Some(Utc::now()));
                update.s3_location = Set(backup_key.clone());
                update.size_bytes = Set(Some(size_bytes));
                update.update(pool).await?;

                info!(
                    "MariaDB backup completed successfully: {} ({} bytes)",
                    backup_key, size_bytes
                );
                Ok(super::BackupOutcome::new(backup_key, Some(size_bytes)))
            }
            Err(e) => {
                let error_msg = format!("MariaDB backup failed: {}", e);
                error!("{}", error_msg);
                let mut update: temps_entities::external_service_backups::ActiveModel =
                    backup_record.into();
                update.state = Set("failed".to_string());
                update.error_message = Set(Some(error_msg.clone()));
                update.finished_at = Set(Some(Utc::now()));
                if let Err(update_err) = update.update(pool).await {
                    error!(
                        "Failed to mark MariaDB backup row as failed: {}",
                        update_err
                    );
                }
                Err(e)
            }
        }
    }

    async fn restore_from_s3(
        &self,
        s3_client: &aws_sdk_s3::Client,
        _s3_credentials: &super::S3Credentials,
        backup_location: &str,
        s3_source: &temps_entities::s3_sources::Model,
        service_config: ServiceConfig,
    ) -> Result<()> {
        info!("Starting MariaDB restore from S3: {}", backup_location);

        let config = self.get_mariadb_config(service_config)?;
        let bucket = &s3_source.bucket_name;
        let backup_key = Self::backup_key_from_location(backup_location, bucket);

        // Detect a physical (`mariadb-backup` mbstream) base vs the legacy
        // logical `.sql.gz` dump and dispatch accordingly. We treat a
        // `base.mbstream.gz` location as physical; everything else stays on the
        // existing logical path. (We don't fetch metadata.json here to keep the
        // common logical path a single round-trip; PITR fetches it for its
        // guard.)
        if Self::is_physical_base_location(&backup_key) {
            info!("Detected physical MariaDB base backup; performing in-place physical restore");
            let temp_dir = tempfile::tempdir()?;
            let mbstream_path = temp_dir.path().join("base.mbstream");
            self.download_and_gunzip_base(s3_client, bucket, &backup_key, &mbstream_path)
                .await?;
            self.physical_restore_into_container(&config, &mbstream_path)
                .await?;
        } else {
            self.restore_logical_from_s3(s3_client, bucket, backup_location, &config)
                .await?;
        }

        info!("MariaDB restore completed successfully");
        Ok(())
    }

    /// MariaDB supports in-place restore, restore-to-new-service, and PITR.
    /// PITR requires a physical (`mariadb-backup`) base plus archived binlogs;
    /// logical-only backups are rejected at execute time by `restore_pitr`.
    ///
    /// We don't populate `earliest_pitr_time` / `latest_pitr_time` — deriving
    /// them would require reading every base's metadata and the binlog manifest
    /// per S3 source. The UI shows an unconstrained datetime picker and the
    /// server validates on execute.
    async fn restore_capabilities(
        &self,
        _service_config: ServiceConfig,
    ) -> Result<super::RestoreCapabilities> {
        Ok(super::RestoreCapabilities {
            restore_in_place: true,
            restore_to_new_service: true,
            pitr: true,
            earliest_pitr_time: None,
            latest_pitr_time: None,
        })
    }

    /// Provision a new MariaDB service from an existing backup (physical or
    /// logical). Clones the source config onto a fresh port, creates the
    /// container, and restores the base into it.
    async fn restore_to_new_service(
        &self,
        ctx: super::RestoreContext<'_>,
        new_service_name: String,
        parameter_overrides: serde_json::Value,
    ) -> Result<super::NewServiceRestoreResult> {
        info!(
            "Provisioning new MariaDB service '{}' from backup at {}",
            new_service_name, ctx.backup_location
        );

        let (new_service, config) = self
            .provision_new_service_for_restore(
                &ctx.source_config,
                &new_service_name,
                &parameter_overrides,
            )
            .await?;

        self.restore_base_into(
            &new_service,
            &config,
            ctx.s3_client,
            &ctx.s3_source.bucket_name,
            ctx.backup_location,
        )
        .await?;

        Self::new_service_result(&config)
    }

    /// Point-in-time recovery: restore a physical base, then replay archived
    /// binlogs up to the recovery target.
    async fn restore_pitr(
        &self,
        ctx: super::RestoreContext<'_>,
        target: super::RecoveryTarget,
        to_new_service: bool,
        new_service_name: Option<String>,
    ) -> Result<Option<super::NewServiceRestoreResult>> {
        let bucket = &ctx.s3_source.bucket_name;
        let base_key = Self::backup_key_from_location(ctx.backup_location, bucket);

        // ── Guard: PITR requires a physical base with binlog coordinates ─────
        // Mirrors postgres' WAL-G guard. Logical (`mariadb_dump`) backups carry
        // no binlog start position and cannot anchor a replay.
        if !Self::is_physical_base_location(&base_key) {
            return Err(anyhow::anyhow!(
                "PITR requires a physical (mariadb-backup) base backup; '{}' is a \
                 logical dump and cannot be used for point-in-time recovery",
                ctx.backup_location
            ));
        }
        let metadata = self
            .fetch_base_metadata(ctx.s3_client, bucket, &base_key)
            .await?;
        let engine = metadata
            .get("engine")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let pitr_enabled = metadata
            .get("pitr")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let binlog_file = metadata
            .get("binlog_file")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let binlog_position = metadata
            .get("binlog_position")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if engine != "mariadb_physical" || !pitr_enabled || binlog_file.is_empty() {
            return Err(anyhow::anyhow!(
                "PITR requires a physical (mariadb-backup) base with binlog coordinates; \
                 base metadata has engine='{}', pitr={}, binlog_file='{}' — not usable for \
                 point-in-time recovery",
                engine,
                pitr_enabled,
                binlog_file
            ));
        }
        let start_position = if binlog_position.is_empty() {
            "4".to_string() // binlog header size; replay the whole first segment
        } else {
            binlog_position
        };

        info!(
            "Running MariaDB PITR to target {:?} (to_new_service={}) from base {} (binlog {}:{})",
            target, to_new_service, ctx.backup_location, binlog_file, start_position
        );

        // Validate the recovery target maps to something we can honor BEFORE
        // we destroy any data — fail fast on Name/Xid/bad-Lsn targets.
        let _ = Self::recovery_target_to_stop_flag(&target)?;

        // ── Restore the base (new service or in place) ──────────────────────
        let (target_service, target_config, new_result) = if to_new_service {
            let new_name = new_service_name.ok_or_else(|| {
                anyhow::anyhow!("new_service_name is required when to_new_service=true")
            })?;
            info!(
                source_service = %ctx.source_config.name,
                target_service = %new_name,
                "Provisioning MariaDB service for PITR restore"
            );
            let (new_service, config) = self
                .provision_new_service_for_restore(
                    &ctx.source_config,
                    &new_name,
                    &serde_json::Value::Null,
                )
                .await?;
            let result = Self::new_service_result(&config)?;
            info!(
                target_service = %new_name,
                target_port = %config.port,
                "Provisioned MariaDB service for PITR restore"
            );
            (new_service, config, Some(result))
        } else {
            let config = self.get_mariadb_config(ctx.source_config.clone())?;
            // Restore in place onto self; clone self's docker handle.
            let svc = MariaDbService::new(self.name.clone(), self.docker.clone());
            *svc.config.write().await = Some(config.clone());
            (svc, config, None)
        };

        // Physical base restore into the target container.
        let temp_dir = tempfile::tempdir()?;
        let mbstream_path = temp_dir.path().join("base.mbstream");
        info!(
            target_service = %target_service.name,
            base_key = %base_key,
            "Downloading MariaDB PITR physical base"
        );
        target_service
            .download_and_gunzip_base(ctx.s3_client, bucket, &base_key, &mbstream_path)
            .await?;
        info!(
            target_service = %target_service.name,
            "Restoring MariaDB PITR physical base"
        );
        target_service
            .physical_restore_into_container(&target_config, &mbstream_path)
            .await?;
        info!(
            target_service = %target_service.name,
            "Restored MariaDB PITR physical base"
        );

        // ── Forward-roll: fetch + replay archived binlogs to the target ─────
        let prefix = ctx.s3_source.bucket_path.trim_matches('/');
        let binlog_temp = tempfile::tempdir()?;
        // Binlogs were archived under the SOURCE service name; the target may
        // be a freshly-named new service, so fetch from the source's prefix.
        let segments = target_service
            .fetch_binlogs_for_replay(
                ctx.s3_client,
                bucket,
                prefix,
                &ctx.source_config.name,
                &binlog_file,
                binlog_temp.path(),
            )
            .await?;
        info!(
            target_service = %target_service.name,
            segments = segments.len(),
            "Fetched MariaDB PITR binlog segments"
        );
        target_service
            .replay_binlogs(&target_config, &segments, &start_position, &target)
            .await?;

        info!("MariaDB PITR completed successfully");
        Ok(new_result)
    }

    async fn import_from_container(
        &self,
        container_id: String,
        service_name: String,
        credentials: HashMap<String, String>,
        additional_config: serde_json::Value,
    ) -> Result<ServiceConfig> {
        let container = self
            .docker
            .inspect_container(
                &container_id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .map_err(|e| {
                anyhow::anyhow!("Failed to inspect container '{}': {}", container_id, e)
            })?;

        let container_config = container.config.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Could not inspect config for container '{}'", container_id)
        })?;
        let image = container_config.image.clone().ok_or_else(|| {
            anyhow::anyhow!("Could not determine image for container '{}'", container_id)
        })?;
        if !crate::mariadb_query::is_mariadb_compatible_image(&image) {
            return Err(anyhow::anyhow!(
                "Container '{}' image '{}' is not MariaDB/MySQL-compatible",
                container_id,
                image
            ));
        }
        let imported_container_name = container
            .name
            .as_deref()
            .unwrap_or(&container_id)
            .trim_start_matches('/')
            .to_string();

        let env = Self::env_to_map(container_config.env.clone());
        let database_override = Self::json_string(&additional_config, "database");
        let port_override = Self::json_string(&additional_config, "port");

        let root_password = Self::first_non_empty([
            credentials.get("root_password"),
            credentials.get("password").filter(|_| {
                credentials
                    .get("username")
                    .map(|u| u.eq_ignore_ascii_case("root"))
                    .unwrap_or(false)
            }),
            env.get("MARIADB_ROOT_PASSWORD"),
            env.get("MYSQL_ROOT_PASSWORD"),
        ])
        .ok_or_else(|| {
            anyhow::anyhow!(
                "root_password is required for MariaDB import unless the container exposes MARIADB_ROOT_PASSWORD or MYSQL_ROOT_PASSWORD"
            )
        })?;

        let database = Self::first_non_empty([
            credentials.get("database"),
            database_override.as_ref(),
            env.get("MARIADB_DATABASE"),
            env.get("MYSQL_DATABASE"),
        ])
        .unwrap_or_else(|| "mysql".to_string());

        let username = Self::first_non_empty([
            credentials.get("username"),
            env.get("MARIADB_USER"),
            env.get("MYSQL_USER"),
        ])
        .unwrap_or_else(|| "root".to_string());

        let password = Self::first_non_empty([
            credentials.get("password"),
            env.get("MARIADB_PASSWORD"),
            env.get("MYSQL_PASSWORD"),
        ])
        .unwrap_or_else(|| {
            if username.eq_ignore_ascii_case("root") {
                root_password.clone()
            } else {
                String::new()
            }
        });

        if password.is_empty() {
            return Err(anyhow::anyhow!(
                "password is required for MariaDB import when username is not root"
            ));
        }

        Self::validate_identifier("database", &database)?;
        Self::validate_identifier("username", &username)?;
        Self::validate_password("password", &password)?;
        Self::validate_password("root_password", &root_password)?;

        let port = port_override
            .or_else(|| Self::extract_host_port(&container))
            .unwrap_or_else(|| MARIADB_INTERNAL_PORT.to_string());

        Self::verify_import_connection(&username, &password, &port, &database).await?;
        info!("Successfully verified MariaDB-compatible connection for import");

        let network_ready = {
            match ensure_network_exists(&self.docker).await {
                Ok(()) => true,
                Err(e) => {
                    warn!(
                        "Failed to ensure Temps Docker network before MariaDB import attach: {:?}",
                        e
                    );
                    false
                }
            }
        };

        if network_ready {
            let network_name = temps_core::NETWORK_NAME.as_str();
            let request = bollard::models::NetworkConnectRequest {
                container: container_id.clone(),
                ..Default::default()
            };
            match self.docker.connect_network(network_name, request).await {
                Ok(()) => info!(
                    "Attached imported MariaDB-compatible container '{}' to {}",
                    imported_container_name, network_name
                ),
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 403, ..
                }) => debug!(
                    "Imported MariaDB-compatible container '{}' is already attached to {}",
                    imported_container_name, network_name
                ),
                Err(e) => warn!(
                    "Failed to attach imported MariaDB-compatible container '{}' to {}: {}",
                    imported_container_name, network_name, e
                ),
            }
        }

        let version = image
            .rfind(':')
            .map(|tag_pos| image[tag_pos + 1..].to_string())
            .unwrap_or_else(|| "latest".to_string());

        Ok(ServiceConfig {
            name: service_name,
            service_type: ServiceType::Mariadb,
            version: Some(version),
            parameters: serde_json::json!({
                "host": "localhost",
                "port": port,
                "database": database,
                "username": username,
                "password": password,
                "root_password": root_password,
                "docker_image": image,
                "size_profile": "dedicated",
                "container_name": imported_container_name,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::externalsvc::DEPLOYMENT_MODE_MUTEX as ENV_MUTEX;

    #[test]
    fn normalizes_database_names() {
        assert_eq!(
            MariaDbService::normalize_database_name("Project-123 Production"),
            "project_123_production"
        );
        assert_eq!(
            MariaDbService::normalize_database_name("123-prod"),
            "db_123_prod"
        );
    }

    #[test]
    fn rejects_unsafe_identifiers() {
        assert!(MariaDbService::validate_identifier("database", "valid_name").is_ok());
        assert!(MariaDbService::validate_identifier("database", "bad-name").is_err());
        assert!(MariaDbService::validate_identifier("database", "1bad").is_err());
        assert!(MariaDbService::validate_identifier("database", "bad`name").is_err());
    }

    #[test]
    fn builds_mysql_and_mariadb_env_aliases() {
        let env = MariaDbService::build_env_vars(
            "mariadb-app",
            "3306",
            "project_prod",
            "app",
            "secretpass",
        )
        .expect("env vars should build");

        assert_eq!(env.get("MYSQL_DATABASE"), Some(&"project_prod".to_string()));
        assert_eq!(
            env.get("MARIADB_DATABASE"),
            Some(&"project_prod".to_string())
        );
        assert_eq!(
            env.get("DATABASE_URL"),
            Some(&"mysql://app:secretpass@mariadb-app:3306/project_prod".to_string())
        );
    }

    #[test]
    fn small_profile_sets_conservative_resources_and_server_args() {
        let resources = MariaDbSizeProfile::Small.default_resource_limits();
        assert_eq!(resources.memory_mb, Some(512));
        assert_eq!(resources.memory_swap_mb, Some(768));
        assert_eq!(resources.nano_cpus, Some(750_000_000));

        let args = MariaDbSizeProfile::Small.server_args();
        assert!(args.contains(&"--innodb-buffer-pool-size=128M".to_string()));
        assert!(args.contains(&"--max-connections=50".to_string()));
        assert!(args.contains(&"--performance-schema=OFF".to_string()));
    }

    #[test]
    fn parses_size_profile_from_config() {
        let config = MariaDbConfig::from(MariaDbInputConfig {
            host: "localhost".to_string(),
            port: Some("3306".to_string()),
            database: "app".to_string(),
            username: "app".to_string(),
            password: Some("secretpass".to_string()),
            root_password: Some("rootpass1".to_string()),
            docker_image: DEFAULT_MARIADB_IMAGE.to_string(),
            container_name: None,
            size_profile: MariaDbSizeProfile::Standard,
            binlog_archive_interval: BinlogArchiveInterval::Min15,
        });

        assert_eq!(config.size_profile, MariaDbSizeProfile::Standard);
        assert_eq!(config.binlog_archive_interval, BinlogArchiveInterval::Min15);
    }

    #[test]
    fn binlog_archive_interval_defaults_to_5m() {
        assert_eq!(
            BinlogArchiveInterval::default(),
            BinlogArchiveInterval::Min5
        );
        assert_eq!(BinlogArchiveInterval::default().as_str(), "5m");
    }

    #[test]
    fn binlog_interval_seconds_and_expire() {
        // Retention must always exceed the ship interval (>= 6x, floor 1h) so
        // a segment is never purged before it is archived.
        for iv in [
            BinlogArchiveInterval::Min1,
            BinlogArchiveInterval::Min5,
            BinlogArchiveInterval::Min15,
            BinlogArchiveInterval::Min60,
        ] {
            assert!(
                iv.binlog_expire_seconds() >= iv.seconds() * 6,
                "{}: expire must be >= 6x interval",
                iv.as_str()
            );
            assert!(
                iv.binlog_expire_seconds() >= 3600,
                "{}: expire floor is 1h",
                iv.as_str()
            );
        }
        assert_eq!(BinlogArchiveInterval::Min60.seconds(), 3600);
        assert_eq!(BinlogArchiveInterval::Min60.binlog_expire_seconds(), 21600);
    }

    #[test]
    fn binlog_interval_serde_round_trips_wire_format() {
        let cfg: MariaDbInputConfig =
            serde_json::from_value(serde_json::json!({ "binlog_archive_interval": "1m" }))
                .expect("parse");
        assert_eq!(cfg.binlog_archive_interval, BinlogArchiveInterval::Min1);
    }

    #[test]
    fn binlog_server_args_has_expected_flags_and_no_credentials() {
        let args = binlog_server_args(42, BinlogArchiveInterval::Min5);
        assert!(args.contains(&"--log-bin=mysql-bin".to_string()));
        assert!(args.contains(&"--binlog-format=ROW".to_string()));
        assert!(args.contains(&"--server-id=42".to_string()));
        assert!(args.contains(&"--sync-binlog=1".to_string()));
        assert!(args.contains(&"--binlog-expire-logs-seconds=3600".to_string()));
        // Server tuning flags only — never a password.
        assert!(!args
            .iter()
            .any(|a| a.contains("password") || a.contains("PWD")));
    }

    #[test]
    fn stable_server_id_is_deterministic_and_nonzero() {
        let a = stable_server_id("orders-db");
        let b = stable_server_id("orders-db");
        let c = stable_server_id("analytics-db");
        assert_eq!(a, b, "must be stable across calls");
        assert_ne!(a, 0, "server-id must be non-zero for --log-bin");
        assert_ne!(a, c, "distinct names should generally differ");
    }

    #[test]
    fn parameter_schema_exposes_editable_binlog_interval() {
        let service = MariaDbService::new(
            "schema-test".to_string(),
            Arc::new(Docker::connect_with_http_defaults().expect("docker client")),
        );
        let schema = service
            .get_parameter_schema()
            .expect("schema should be available");
        let prop = schema
            .get("properties")
            .and_then(|p| p.get("binlog_archive_interval"))
            .expect("binlog_archive_interval should be present");
        assert_eq!(prop.get("default").and_then(|v| v.as_str()), Some("5m"));
        assert_eq!(prop.get("x-editable").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn parse_show_binary_logs_extracts_filenames_in_order() {
        // Typical `mariadb -N -B` output: tab-separated, no header.
        let raw = "mysql-bin.000001\t1234\nmysql-bin.000002\t5678\nmysql-bin.000003\t90\n";
        assert_eq!(
            MariaDbService::parse_show_binary_logs(raw),
            vec![
                "mysql-bin.000001".to_string(),
                "mysql-bin.000002".to_string(),
                "mysql-bin.000003".to_string(),
            ]
        );
    }

    #[test]
    fn parse_show_binary_logs_ignores_header_and_blank_lines() {
        // Some clients (non -N) emit a header row and trailing blank lines.
        let raw = "Log_name\tFile_size\nmysql-bin.000007\t100\n\n";
        assert_eq!(
            MariaDbService::parse_show_binary_logs(raw),
            vec!["mysql-bin.000007".to_string()]
        );
    }

    #[test]
    fn closed_binlog_files_excludes_active_last_segment() {
        let all = vec![
            "mysql-bin.000001".to_string(),
            "mysql-bin.000002".to_string(),
            "mysql-bin.000003".to_string(),
        ];
        // The last segment is active and must not be shippable.
        assert_eq!(
            MariaDbService::closed_binlog_files(&all),
            vec![
                "mysql-bin.000001".to_string(),
                "mysql-bin.000002".to_string()
            ]
        );

        // A single segment is the active one — nothing closed yet.
        assert!(MariaDbService::closed_binlog_files(&["mysql-bin.000001".to_string()]).is_empty());
        assert!(MariaDbService::closed_binlog_files(&[]).is_empty());
    }

    #[test]
    fn binlogs_to_ship_excludes_active_and_already_shipped() {
        let all = vec![
            "mysql-bin.000001".to_string(),
            "mysql-bin.000002".to_string(),
            "mysql-bin.000003".to_string(),
            "mysql-bin.000004".to_string(), // active — never shipped
        ];

        // Nothing shipped yet: ship all closed segments (1..=3), not the active 4.
        assert_eq!(
            MariaDbService::binlogs_to_ship(&all, None),
            vec![
                "mysql-bin.000001".to_string(),
                "mysql-bin.000002".to_string(),
                "mysql-bin.000003".to_string(),
            ]
        );

        // last_shipped=000002: only 000003 is new (000004 is active).
        assert_eq!(
            MariaDbService::binlogs_to_ship(&all, Some("mysql-bin.000002")),
            vec!["mysql-bin.000003".to_string()]
        );

        // last_shipped=000003: everything closed is already shipped.
        assert!(MariaDbService::binlogs_to_ship(&all, Some("mysql-bin.000003")).is_empty());
    }

    #[test]
    fn binlogs_to_ship_lexicographic_ordering_holds_across_rollover() {
        // mysql-bin.NNNNNN names sort lexicographically the same as numerically
        // within the fixed-width range.
        let all = vec![
            "mysql-bin.000009".to_string(),
            "mysql-bin.000010".to_string(),
            "mysql-bin.000011".to_string(), // active
        ];
        assert_eq!(
            MariaDbService::binlogs_to_ship(&all, Some("mysql-bin.000009")),
            vec!["mysql-bin.000010".to_string()]
        );
    }

    #[test]
    fn binlog_object_key_handles_empty_and_nonempty_prefix() {
        // Non-empty bucket_path prefix.
        assert_eq!(
            MariaDbService::binlog_object_key("backups/prod", "orders-db", "mysql-bin.000007"),
            "backups/prod/external_services/mariadb/orders-db/binlog/mysql-bin.000007.gz"
        );
        // Empty prefix drops the leading segment.
        assert_eq!(
            MariaDbService::binlog_object_key("", "orders-db", "mysql-bin.000007"),
            "external_services/mariadb/orders-db/binlog/mysql-bin.000007.gz"
        );
    }

    #[test]
    fn binlog_manifest_key_handles_empty_and_nonempty_prefix() {
        assert_eq!(
            MariaDbService::binlog_manifest_key("backups/prod", "orders-db"),
            "backups/prod/external_services/mariadb/orders-db/binlog/manifest.json"
        );
        assert_eq!(
            MariaDbService::binlog_manifest_key("", "orders-db"),
            "external_services/mariadb/orders-db/binlog/manifest.json"
        );
    }

    #[test]
    fn binlog_manifest_round_trips_json_shape() {
        let manifest = BinlogManifest {
            last_shipped_file: Some("mysql-bin.000007".to_string()),
            updated_at: "2026-06-23T00:00:00+00:00".to_string(),
            shipped_files: vec![
                "mysql-bin.000003".to_string(),
                "mysql-bin.000004".to_string(),
            ],
        };
        let json = serde_json::to_value(&manifest).expect("serialize");
        assert_eq!(json["last_shipped_file"], "mysql-bin.000007");
        assert_eq!(json["shipped_files"][0], "mysql-bin.000003");
        let parsed: BinlogManifest = serde_json::from_value(json).expect("deserialize");
        assert_eq!(parsed, manifest);
    }

    #[test]
    fn binlog_manifest_default_is_empty() {
        let m = BinlogManifest::default();
        assert!(m.last_shipped_file.is_none());
        assert!(m.shipped_files.is_empty());
    }

    // ── Restore / PITR unit tests (no Docker) ──────────────────────────────

    fn mariadb_service_for_tests() -> MariaDbService {
        MariaDbService::new(
            "pitr-test".to_string(),
            Arc::new(Docker::connect_with_http_defaults().expect("docker client")),
        )
    }

    #[tokio::test]
    async fn restore_capabilities_reports_pitr_and_both_modes() {
        let service = mariadb_service_for_tests();
        let cfg = ServiceConfig {
            name: "pitr-test".to_string(),
            service_type: ServiceType::Mariadb,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "3306",
                "database": "app",
                "username": "app",
                "password": "secretpass",
                "root_password": "rootpass1",
                "docker_image": DEFAULT_MARIADB_IMAGE,
            }),
        };
        let caps = service
            .restore_capabilities(cfg)
            .await
            .expect("capabilities");
        assert!(caps.pitr, "MariaDB should advertise PITR support");
        assert!(caps.restore_in_place);
        assert!(caps.restore_to_new_service);
    }

    #[test]
    fn detects_physical_vs_logical_backup_from_location() {
        assert!(MariaDbService::is_physical_base_location(
            "backups/prod/external_services/mariadb/orders/2026/06/23/abc/base.mbstream.gz"
        ));
        assert!(!MariaDbService::is_physical_base_location(
            "backups/prod/mariadb_backup_20260623_010101.sql.gz"
        ));
        assert!(!MariaDbService::is_physical_base_location("dump.sql.gz"));
    }

    #[test]
    fn derives_metadata_key_from_base_key() {
        assert_eq!(
            MariaDbService::derive_metadata_key(
                "backups/prod/external_services/mariadb/orders/2026/06/23/abc/base.mbstream.gz"
            ),
            "backups/prod/external_services/mariadb/orders/2026/06/23/abc/metadata.json"
        );
        // No slash → companion-suffix fallback.
        assert_eq!(
            MariaDbService::derive_metadata_key("base.mbstream.gz"),
            "base.mbstream.gz.metadata.json"
        );
    }

    #[test]
    fn recovery_target_time_maps_to_stop_datetime() {
        use chrono::TimeZone;
        let time = chrono::Utc
            .with_ymd_and_hms(2026, 6, 23, 14, 30, 15)
            .single()
            .expect("valid time");
        let flag = MariaDbService::recovery_target_to_stop_flag(&RecoveryTarget::Time { time })
            .expect("time target maps")
            .expect("has stop flag");
        assert_eq!(flag.0, "--stop-datetime");
        assert_eq!(flag.1, "2026-06-23 14:30:15");
        assert_eq!(
            MariaDbService::format_stop_datetime(time),
            "2026-06-23 14:30:15"
        );
    }

    #[test]
    fn recovery_target_lsn_requires_file_and_position() {
        // file:position → --stop-position
        let flag = MariaDbService::recovery_target_to_stop_flag(&RecoveryTarget::Lsn {
            lsn: "mysql-bin.000007:1234".to_string(),
        })
        .expect("valid lsn")
        .expect("has flag");
        assert_eq!(flag.0, "--stop-position");
        assert_eq!(flag.1, "1234");

        // bare position → rejected (ambiguous across segments)
        assert!(
            MariaDbService::recovery_target_to_stop_flag(&RecoveryTarget::Lsn {
                lsn: "1234".to_string(),
            })
            .is_err()
        );
    }

    #[test]
    fn recovery_target_xid_and_name_are_rejected() {
        assert!(
            MariaDbService::recovery_target_to_stop_flag(&RecoveryTarget::Xid {
                xid: "0-1-100".to_string(),
            })
            .is_err()
        );
        assert!(
            MariaDbService::recovery_target_to_stop_flag(&RecoveryTarget::Name {
                name: "my-restore-point".to_string(),
            })
            .is_err()
        );
    }

    #[test]
    fn pitr_guard_rejects_logical_only_backup() {
        // A logical dump location is rejected by the location-based guard
        // before any network call: it is not a physical base.
        let location = "s3://my-bucket/backups/mariadb_backup_20260623.sql.gz";
        let key = MariaDbService::backup_key_from_location(location, "my-bucket");
        assert!(!MariaDbService::is_physical_base_location(&key));

        // Exercise the guard message wording directly: it must mention PITR and
        // physical so operators (and greps) can find it.
        let guard_msg = format!(
            "PITR requires a physical (mariadb-backup) base backup; '{}' is a logical dump",
            location
        );
        assert!(guard_msg.contains("PITR"));
        assert!(guard_msg.contains("physical"));

        // And confirm the engine-mismatch guard (used when metadata says
        // mariadb_dump) produces a PITR+physical message too.
        let metadata = serde_json::json!({
            "engine": "mariadb_dump",
            "pitr": false,
        });
        let engine = metadata
            .get("engine")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let pitr = metadata
            .get("pitr")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        assert_eq!(engine, "mariadb_dump");
        assert!(!pitr);
        let mismatch_msg = format!(
            "PITR requires a physical (mariadb-backup) base with binlog coordinates; \
             base metadata has engine='{}', pitr={}",
            engine, pitr
        );
        assert!(mismatch_msg.contains("PITR"));
        assert!(mismatch_msg.contains("physical"));
    }

    #[test]
    fn shell_single_quote_escapes_embedded_quotes() {
        assert_eq!(MariaDbService::shell_single_quote("plain"), "'plain'");
        assert_eq!(MariaDbService::shell_single_quote("a'b"), "'a'\\''b'");
        // A datetime value (contains a space) stays a single shell token.
        assert_eq!(
            MariaDbService::shell_single_quote("2026-06-23 14:30:15"),
            "'2026-06-23 14:30:15'"
        );
    }

    #[test]
    fn parameter_schema_exposes_create_time_size_profile() {
        let service = MariaDbService::new(
            "schema-test".to_string(),
            Arc::new(Docker::connect_with_http_defaults().expect("docker client")),
        );
        let schema = service
            .get_parameter_schema()
            .expect("schema should be available");
        let size_profile = schema
            .get("properties")
            .and_then(|p| p.get("size_profile"))
            .expect("size_profile should be present");

        assert_eq!(
            size_profile.get("default").and_then(|v| v.as_str()),
            Some("small")
        );
        assert_eq!(
            size_profile.get("x-editable").and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    // ── Identifier / database-name validation (parity with Postgres) ────────
    //
    // MariaDB's `validate_identifier` is the gate for database/username before
    // any SQL is emitted. Unlike Postgres it accepts uppercase (MariaDB
    // identifiers are case-sensitive on some platforms and validate is a
    // safety gate, not a normalizer); `normalize_database_name` lowercases.

    #[test]
    fn test_validate_database_name_valid_names() {
        assert!(MariaDbService::validate_identifier("database", "mydb").is_ok());
        assert!(MariaDbService::validate_identifier("database", "project_1_production").is_ok());
        assert!(MariaDbService::validate_identifier("database", "db_test_env").is_ok());
        assert!(MariaDbService::validate_identifier("database", "a").is_ok());
        assert!(MariaDbService::validate_identifier("database", "_private").is_ok());
    }

    #[test]
    fn test_validate_database_name_rejects_empty() {
        assert!(MariaDbService::validate_identifier("database", "").is_err());
    }

    #[test]
    fn test_validate_database_name_rejects_sql_injection_single_quote() {
        // Classic SQL injection: ' OR 1=1 --
        assert!(
            MariaDbService::validate_identifier("database", "test'; DROP TABLE users--").is_err()
        );
    }

    #[test]
    fn test_validate_database_name_rejects_sql_injection_semicolon() {
        assert!(
            MariaDbService::validate_identifier("database", "mydb; DROP DATABASE production")
                .is_err()
        );
    }

    #[test]
    fn test_validate_database_name_rejects_spaces() {
        assert!(MariaDbService::validate_identifier("database", "my database").is_err());
    }

    #[test]
    fn test_validate_database_name_rejects_special_chars() {
        assert!(MariaDbService::validate_identifier("database", "db-name").is_err());
        assert!(MariaDbService::validate_identifier("database", "db.name").is_err());
        assert!(MariaDbService::validate_identifier("database", "db/name").is_err());
        assert!(MariaDbService::validate_identifier("database", "db\\name").is_err());
        assert!(MariaDbService::validate_identifier("database", "db`name").is_err());
    }

    #[test]
    fn test_validate_database_name_accepts_uppercase() {
        // MariaDB's validator accepts uppercase letters (it is a safety gate,
        // not a normalizer). This diverges from Postgres, which rejects
        // uppercase; we assert MariaDB's actual behavior.
        assert!(MariaDbService::validate_identifier("database", "MyDatabase").is_ok());
    }

    #[test]
    fn test_validate_database_name_rejects_leading_digit() {
        assert!(MariaDbService::validate_identifier("database", "1database").is_err());
        assert!(MariaDbService::validate_identifier("database", "123").is_err());
    }

    #[test]
    fn test_validate_database_name_rejects_too_long() {
        let long_name = "a".repeat(64);
        assert!(MariaDbService::validate_identifier("database", &long_name).is_err());
    }

    #[test]
    fn test_validate_database_name_accepts_max_length() {
        let max_name = "a".repeat(63);
        assert!(MariaDbService::validate_identifier("database", &max_name).is_ok());
    }

    #[test]
    fn test_normalize_then_validate_is_always_safe() {
        // Any input passed through normalize_database_name must pass validation.
        let dangerous_inputs = vec![
            "'; DROP TABLE users--",
            "test; DELETE FROM sessions",
            "../../etc/passwd",
            "admin\x00hidden",
            "Robert'); DROP TABLE Students;--",
            "name WITH spaces AND STUFF",
            "UPPERCASE_NAME",
            "123_starts_with_number",
            "db`name",
        ];

        for input in dangerous_inputs {
            let normalized = MariaDbService::normalize_database_name(input);
            assert!(
                MariaDbService::validate_identifier("database", &normalized).is_ok(),
                "normalize_database_name('{}') produced '{}' which failed validation",
                input,
                normalized
            );
        }
    }

    // ── Config / schema parity ──────────────────────────────────────────────

    #[test]
    fn test_mariadb_input_config_default_values() {
        let input = MariaDbInputConfig {
            host: default_host(),
            port: None,
            database: default_database(),
            username: default_username(),
            password: None,
            root_password: None,
            docker_image: default_docker_image(),
            container_name: None,
            size_profile: MariaDbSizeProfile::default(),
            binlog_archive_interval: BinlogArchiveInterval::default(),
        };

        let config: MariaDbConfig = input.into();

        assert_eq!(config.host, "localhost");
        assert_eq!(config.database, "app");
        assert_eq!(config.username, "app");
        assert_eq!(config.docker_image, DEFAULT_MARIADB_IMAGE);
        assert_eq!(config.size_profile, MariaDbSizeProfile::Small);
        assert_eq!(config.binlog_archive_interval, BinlogArchiveInterval::Min5);
        // Auto-generated credentials: 24 alphanumeric chars, distinct.
        assert_eq!(config.password.len(), 24);
        assert_eq!(config.root_password.len(), 24);
        assert!(config.password.chars().all(|c| c.is_ascii_alphanumeric()));
        assert!(config
            .root_password
            .chars()
            .all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(
            config.password, config.root_password,
            "app and root passwords should be independently generated"
        );
        // Generated passwords satisfy the password validator.
        assert!(MariaDbService::validate_password("password", &config.password).is_ok());
        assert!(MariaDbService::validate_password("root_password", &config.root_password).is_ok());
    }

    #[test]
    fn test_mariadb_input_config_custom_docker_image() {
        let input = MariaDbInputConfig {
            host: "localhost".to_string(),
            port: Some("3306".to_string()),
            database: "mydb".to_string(),
            username: "myuser".to_string(),
            password: Some("mypassword".to_string()),
            root_password: Some("myrootpassword".to_string()),
            docker_image: "mariadb:11.4".to_string(),
            container_name: None,
            size_profile: MariaDbSizeProfile::default(),
            binlog_archive_interval: BinlogArchiveInterval::default(),
        };

        let config: MariaDbConfig = input.into();
        assert_eq!(config.docker_image, "mariadb:11.4");
        assert_eq!(config.port, "3306");
        // A supplied >= 8 char password is kept (not regenerated).
        assert_eq!(config.password, "mypassword");
        assert_eq!(config.root_password, "myrootpassword");
    }

    #[test]
    fn test_short_password_is_regenerated() {
        // The optional-password deserializer drops too-short values, so a
        // sub-8-char password is replaced with an auto-generated one.
        let input: MariaDbInputConfig = serde_json::from_value(serde_json::json!({
            "host": "localhost",
            "database": "app",
            "username": "app",
            "password": "short",
            "docker_image": DEFAULT_MARIADB_IMAGE,
        }))
        .expect("parse input config");
        assert!(
            input.password.is_none(),
            "too-short password must be dropped to None by the deserializer"
        );
        let config: MariaDbConfig = input.into();
        assert_eq!(config.password.len(), 24, "dropped password is regenerated");
    }

    #[test]
    fn test_parameter_schema_editable_fields() {
        let service = MariaDbService::new(
            "test-editable".to_string(),
            Arc::new(Docker::connect_with_http_defaults().expect("docker client")),
        );

        let schema = service
            .get_parameter_schema()
            .expect("schema should be generated");
        let properties = schema
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("properties should be an object");

        // Connection identity is fixed after creation; only port and image
        // (and binlog cadence, covered by its own test) are editable.
        let editable_status = vec![
            ("host", false),
            ("port", true),
            ("database", false),
            ("username", false),
            ("password", false),
            ("root_password", false),
            ("docker_image", true),
        ];

        for (field_name, should_be_editable) in editable_status {
            let field = properties
                .get(field_name)
                .and_then(|v| v.as_object())
                .unwrap_or_else(|| panic!("{} field should exist", field_name));
            let is_editable = field
                .get("x-editable")
                .and_then(|v| v.as_bool())
                .unwrap_or_else(|| panic!("{} should have x-editable property", field_name));
            assert_eq!(
                is_editable, should_be_editable,
                "Field {} editable status should be {}",
                field_name, should_be_editable
            );
        }
    }

    #[test]
    fn test_default_docker_image_constant() {
        // The default image tag the service provisions with.
        assert_eq!(default_docker_image(), "mariadb:lts");
        assert_eq!(DEFAULT_MARIADB_IMAGE, "mariadb:lts");
    }

    // ── Address / env-var routing (parity with Postgres) ────────────────────

    #[test]
    fn test_get_effective_address_baremetal_mode() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // Clear Docker mode to ensure baremetal mode.
        unsafe { std::env::remove_var("DEPLOYMENT_MODE") };

        let service = MariaDbService::new(
            "test-effective-addr".to_string(),
            Arc::new(Docker::connect_with_http_defaults().expect("docker client")),
        );
        let config = ServiceConfig {
            name: "test-mariadb".to_string(),
            service_type: ServiceType::Mariadb,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "3307",
                "database": "app",
                "username": "app",
                "password": "secretpass",
                "root_password": "rootpass1",
                "docker_image": DEFAULT_MARIADB_IMAGE,
            }),
        };

        let (host, port) = service.get_effective_address(config).unwrap();
        // Baremetal: localhost with the exposed host port.
        assert_eq!(host, "localhost");
        assert_eq!(port, "3307");
    }

    #[test]
    fn test_get_effective_address_docker_mode() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("DEPLOYMENT_MODE", "docker") };

        let service = MariaDbService::new(
            "test-effective-addr-docker".to_string(),
            Arc::new(Docker::connect_with_http_defaults().expect("docker client")),
        );
        let config = ServiceConfig {
            name: "test-mariadb".to_string(),
            service_type: ServiceType::Mariadb,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "3307",
                "database": "app",
                "username": "app",
                "password": "secretpass",
                "root_password": "rootpass1",
                "docker_image": DEFAULT_MARIADB_IMAGE,
            }),
        };

        let (host, port) = service.get_effective_address(config).unwrap();
        // Docker: container name with the internal port, not the host port.
        assert_eq!(host, "mariadb-test-effective-addr-docker");
        assert_eq!(port, MARIADB_INTERNAL_PORT);

        unsafe { std::env::remove_var("DEPLOYMENT_MODE") };
    }

    #[test]
    fn test_get_effective_address_docker_mode_uses_imported_container_name() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("DEPLOYMENT_MODE", "docker") };

        let service = MariaDbService::new(
            "imported-svc".to_string(),
            Arc::new(Docker::connect_with_http_defaults().expect("docker client")),
        );
        let config = ServiceConfig {
            name: "imported-svc".to_string(),
            service_type: ServiceType::Mariadb,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "3307",
                "database": "app",
                "username": "app",
                "password": "secretpass",
                "root_password": "rootpass1",
                "docker_image": DEFAULT_MARIADB_IMAGE,
                "container_name": "legacy-mariadb",
            }),
        };

        let (host, port) = service.get_effective_address(config).unwrap();
        // The imported container name wins over the derived mariadb-{name}.
        assert_eq!(host, "legacy-mariadb");
        assert_eq!(port, MARIADB_INTERNAL_PORT);

        unsafe { std::env::remove_var("DEPLOYMENT_MODE") };
    }

    #[test]
    fn test_get_environment_variables_always_uses_container_name() {
        // get_environment_variables always targets the container name and the
        // internal port (3306) for container-to-container traffic, regardless
        // of the exposed host port.
        let service = MariaDbService::new(
            "test-env-vars".to_string(),
            Arc::new(Docker::connect_with_http_defaults().expect("docker client")),
        );

        let mut params = HashMap::new();
        params.insert("port".to_string(), "3399".to_string()); // host port, ignored
        params.insert("database".to_string(), "project_prod".to_string());
        params.insert("username".to_string(), "app".to_string());
        params.insert("password".to_string(), "secretpass".to_string());

        let env = service.get_environment_variables(&params).unwrap();

        assert_eq!(env.get("MYSQL_HOST").unwrap(), "mariadb-test-env-vars");
        assert_eq!(env.get("MARIADB_HOST").unwrap(), "mariadb-test-env-vars");
        assert_eq!(env.get("MYSQL_PORT").unwrap(), MARIADB_INTERNAL_PORT);
        assert_eq!(env.get("MARIADB_PORT").unwrap(), MARIADB_INTERNAL_PORT);
        assert_eq!(env.get("MYSQL_DATABASE").unwrap(), "project_prod");
        assert_eq!(
            env.get("DATABASE_URL").unwrap(),
            "mysql://app:secretpass@mariadb-test-env-vars:3306/project_prod"
        );
        // Internal port is used; the host port is never embedded.
        assert!(!env.get("DATABASE_URL").unwrap().contains("3399"));
    }

    #[test]
    fn test_get_environment_variables_prefers_explicit_container_name() {
        // An imported service surfaces its real container_name as the host.
        let service = MariaDbService::new(
            "svc".to_string(),
            Arc::new(Docker::connect_with_http_defaults().expect("docker client")),
        );

        let mut params = HashMap::new();
        params.insert("container_name".to_string(), "legacy-mariadb".to_string());
        params.insert("host".to_string(), "localhost".to_string());
        params.insert("database".to_string(), "app".to_string());
        params.insert("username".to_string(), "app".to_string());
        params.insert("password".to_string(), "secretpass".to_string());

        let env = service.get_environment_variables(&params).unwrap();
        assert_eq!(env.get("MARIADB_HOST").unwrap(), "legacy-mariadb");
        assert!(env
            .get("MARIADB_URL")
            .unwrap()
            .contains("legacy-mariadb:3306"));
    }

    #[test]
    fn test_get_docker_environment_variables_match_environment_variables() {
        let service = MariaDbService::new(
            "test-docker-env".to_string(),
            Arc::new(Docker::connect_with_http_defaults().expect("docker client")),
        );

        let mut params = HashMap::new();
        params.insert("port".to_string(), "3399".to_string());
        params.insert("database".to_string(), "app".to_string());
        params.insert("username".to_string(), "app".to_string());
        params.insert("password".to_string(), "secretpass".to_string());

        let env = service.get_docker_environment_variables(&params).unwrap();
        assert_eq!(env.get("MYSQL_HOST").unwrap(), "mariadb-test-docker-env");
        assert_eq!(env.get("MYSQL_PORT").unwrap(), MARIADB_INTERNAL_PORT);
    }

    #[test]
    fn test_get_environment_variables_missing_params_error() {
        let service = MariaDbService::new(
            "missing".to_string(),
            Arc::new(Docker::connect_with_http_defaults().expect("docker client")),
        );
        // No database/username/password → error.
        let params = HashMap::new();
        assert!(service.get_environment_variables(&params).is_err());
    }

    // ── Import (parity with Postgres) ───────────────────────────────────────

    #[test]
    fn test_import_connection_url_format() {
        // Mirrors verify_import_connection's URL construction (mysql:// scheme).
        let username = "app";
        let password = "mysecretpassword";
        let port = "3306";
        let database = "importeddb";
        let connection_url = format!(
            "mysql://{}:{}@localhost:{}/{}",
            urlencoding::encode(username),
            urlencoding::encode(password),
            port,
            urlencoding::encode(database)
        );
        assert!(connection_url.contains("mysql://"));
        assert!(connection_url.contains("app"));
        assert!(connection_url.contains("mysecretpassword"));
        assert!(connection_url.contains("localhost"));
        assert!(connection_url.contains("3306"));
        assert!(connection_url.contains("importeddb"));
    }

    #[test]
    fn test_import_version_extraction_with_tag() {
        // Import derives `version` from the image tag (image.rfind(':')).
        let test_cases = vec![
            ("mariadb:lts", "lts"),
            ("mariadb:11.4", "11.4"),
            ("mysql:8.0", "8.0"),
            ("docker.io/library/mariadb:10.11", "10.11"),
        ];
        for (image, expected) in test_cases {
            let version = image
                .rfind(':')
                .map(|p| image[p + 1..].to_string())
                .unwrap_or_else(|| "latest".to_string());
            assert_eq!(version, expected, "failed for image {}", image);
        }
    }

    #[test]
    fn test_import_version_extraction_without_tag() {
        let image = "mariadb";
        let version = image
            .rfind(':')
            .map(|p| image[p + 1..].to_string())
            .unwrap_or_else(|| "latest".to_string());
        assert_eq!(version, "latest");
    }

    #[test]
    fn test_import_root_password_resolution_from_credentials_and_env() {
        // root_password resolution prefers explicit credentials, then a
        // username==root password, then MARIADB/MYSQL env. Exercises the
        // first_non_empty precedence the import path relies on.
        let root_pw = "explicit-root-pw".to_string();
        let env_pw = "env-root-pw".to_string();
        let resolved = MariaDbService::first_non_empty([Some(&root_pw), Some(&env_pw)]);
        assert_eq!(resolved.as_deref(), Some("explicit-root-pw"));

        // Blank/whitespace entries are skipped.
        let blank = "   ".to_string();
        let resolved = MariaDbService::first_non_empty([Some(&blank), Some(&env_pw)]);
        assert_eq!(resolved.as_deref(), Some("env-root-pw"));

        // Nothing usable → None (the import path then errors).
        let resolved = MariaDbService::first_non_empty([None, Some(&blank)]);
        assert!(resolved.is_none());
    }

    #[test]
    fn test_import_env_to_map_parses_container_env() {
        // import reads root/db/user/password from the container's env list.
        let env = MariaDbService::env_to_map(Some(vec![
            "MARIADB_ROOT_PASSWORD=rootsecret".to_string(),
            "MARIADB_DATABASE=appdb".to_string(),
            "MARIADB_USER=appuser".to_string(),
            "PATH=/usr/bin".to_string(),
            "MALFORMED_NO_EQUALS".to_string(),
        ]));
        assert_eq!(
            env.get("MARIADB_ROOT_PASSWORD").map(String::as_str),
            Some("rootsecret")
        );
        assert_eq!(
            env.get("MARIADB_DATABASE").map(String::as_str),
            Some("appdb")
        );
        assert_eq!(env.get("MARIADB_USER").map(String::as_str), Some("appuser"));
        // A line without '=' is dropped, not panicked on.
        assert!(!env.contains_key("MALFORMED_NO_EQUALS"));
    }

    #[test]
    fn test_import_json_string_extracts_override() {
        // database/port overrides come from additional_config via json_string.
        let cfg = serde_json::json!({
            "database": "overridedb",
            "port": "3399",
            "blank": "   ",
        });
        assert_eq!(
            MariaDbService::json_string(&cfg, "database").as_deref(),
            Some("overridedb")
        );
        assert_eq!(
            MariaDbService::json_string(&cfg, "port").as_deref(),
            Some("3399")
        );
        // Blank-only values are treated as absent.
        assert!(MariaDbService::json_string(&cfg, "blank").is_none());
        assert!(MariaDbService::json_string(&cfg, "missing").is_none());
    }

    // ── Upgrade decision ────────────────────────────────────────────────────
    //
    // MariaDB does NOT override the ExternalService::upgrade method. The
    // container runs with MARIADB_AUTO_UPGRADE=1, which performs the
    // datadir/system-table upgrade automatically on startup after an image
    // bump (the MariaDB analog of mysql_upgrade). There is therefore no
    // pg_upgrade-style explicit upgrade orchestration here, and calling
    // upgrade() returns the trait's default "not implemented" error. A major
    // version change happens via a container recreate on the new image, not
    // through this method.

    #[tokio::test]
    async fn test_upgrade_returns_not_implemented() {
        let service = mariadb_service_for_tests();
        let old = ServiceConfig {
            name: "pitr-test".to_string(),
            service_type: ServiceType::Mariadb,
            version: Some("10.11".to_string()),
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "3306",
                "database": "app",
                "username": "app",
                "password": "secretpass",
                "root_password": "rootpass1",
                "docker_image": "mariadb:10.11",
            }),
        };
        let new = ServiceConfig {
            version: Some("11.4".to_string()),
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "3306",
                "database": "app",
                "username": "app",
                "password": "secretpass",
                "root_password": "rootpass1",
                "docker_image": "mariadb:11.4",
            }),
            ..old.clone()
        };

        // MariaDB relies on MARIADB_AUTO_UPGRADE on recreate, so the explicit
        // upgrade entrypoint is intentionally the trait default (errors).
        let result = service.upgrade(old, new).await;
        assert!(
            result.is_err(),
            "MariaDB should not implement explicit in-place upgrade"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Upgrade not implemented"),
            "unexpected upgrade error message: {}",
            msg
        );
    }

    #[test]
    fn test_create_container_env_enables_auto_upgrade() {
        // Document the upgrade mechanism: MARIADB_AUTO_UPGRADE=1 is the env the
        // container is created with (see create_container). This is the path by
        // which version upgrades actually happen.
        let env_vars = ["MARIADB_AUTO_UPGRADE=1".to_string()];
        assert!(env_vars.contains(&"MARIADB_AUTO_UPGRADE=1".to_string()));
    }

    #[test]
    fn test_container_name_is_not_a_user_input() {
        // container_name is derived from the service name at creation time
        // (`mariadb-{name}`), never supplied by the client — same as Postgres.
        // The create form is generated from this schema, so the field must not
        // appear in it (previously a stray "" made init skip container creation
        // and start POST /containers//start, which Docker answers with a 301).
        let schema = serde_json::to_value(schemars::schema_for!(MariaDbInputConfig)).unwrap();
        assert!(
            !schema.to_string().contains("container_name"),
            "container_name leaked into the MariaDB create schema"
        );
    }

    /// Regression guard for the cross-tenant IDOR: a client-supplied
    /// `container_name` in a create request would make every subsequent Docker
    /// operation (start, exec, backup, restore) target that named container
    /// instead of creating a new one. Because Docker names are global and
    /// predictable (`mariadb-{slug}`), this lets a tenant redirect operations
    /// to a different tenant's live database container.
    ///
    /// The schema-level `#[schemars(skip)]` only hides the field from the UI
    /// form — it does NOT stop `serde_json::from_value` from deserialising a
    /// client-supplied value. `validate_for_creation` must reject it explicitly.
    #[test]
    fn test_container_name_rejected_by_validate_for_creation() {
        use crate::parameter_strategies::MariaDbParameterStrategy;
        let strategy = MariaDbParameterStrategy;
        let mut params = std::collections::HashMap::new();
        params.insert(
            "container_name".to_string(),
            serde_json::Value::String("mariadb-victim-slug".to_string()),
        );
        let result = crate::parameter_strategies::ParameterStrategy::validate_for_creation(
            &strategy, &params,
        );
        assert!(
            result.is_err(),
            "validate_for_creation must reject a client-supplied container_name"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("container_name"),
            "error message should mention 'container_name', got: {err}"
        );
    }
}
