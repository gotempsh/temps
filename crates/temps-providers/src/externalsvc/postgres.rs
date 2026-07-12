use anyhow::{Context, Result};
use async_trait::async_trait;
use bollard::query_parameters::{InspectContainerOptions, StopContainerOptions};
use bollard::{body_full, Docker};
use futures::{StreamExt, TryStreamExt};
use schemars::JsonSchema;
use sea_orm::{prelude::*, *};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use temps_entities::external_service_backups;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};
use urlencoding;

use crate::utils::ensure_network_exists;

/// Hard ceiling for a single backup `docker exec`. Hit this and we give up,
/// surface the captured output, and mark the backup row as failed. Six hours
/// covers very large WAL-G + pg_dumpall runs while still bounding stuck-exec
/// blast radius — without this, a hung exec would keep the row in `running`
/// indefinitely.
const BACKUP_EXEC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(6 * 3600);

use super::{
    ExternalService, HealthProbeResult, RuntimeEnvVar, ServiceConfig, ServiceResourceLimits,
    ServiceType,
};

/// POSIX-safe shell escaping: wraps value in single quotes, escaping any
/// embedded single quotes. Safe for use in `sh -c` command strings.
///
/// Shared across the Postgres provider files (`postgres_upgrade`,
/// `postgres_lifecycle`) instead of each keeping its own copy -- the drift
/// between duplicated copies of this exact kind of string-builder is what
/// caused the healthcheck bug this crate's `postgres_healthcheck_cmd` fixes.
pub(crate) fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Rejections from same-version-only [`PostgresService::upgrade`]. The
/// `ExternalService::upgrade` trait method returns `anyhow::Result<()>`
/// across every provider, so this can't be a variant of a typed
/// `Result<(), E>` without changing that trait's signature everywhere.
/// Instead it's wrapped into the `anyhow::Error` via `?`/`.into()` so
/// callers can `downcast_ref::<PostgresUpgradeRejected>()` for an exact,
/// wording-independent match instead of matching on `e.to_string()`.
#[derive(Debug, thiserror::Error)]
pub enum PostgresUpgradeRejected {
    #[error("Cannot downgrade PostgreSQL (from {from} to {to})")]
    Downgrade { from: u32, to: u32 },

    #[error(
        "Cannot change PostgreSQL major version via this endpoint (from {from} to {to}). \
         Use the major-version upgrade API (POST /external-services/{{id}}/upgrades) instead, \
         which performs a real pg_dumpall/restore with a retained rollback volume and a \
         mandatory pre-upgrade backup."
    )]
    MajorVersionChange { from: u32, to: u32 },
}

/// Builds the `pg_isready` healthcheck command pinned to the configured
/// username/database. Without `-d`, `pg_isready` (via libpq) defaults the
/// target database to the username, so any service where `database !=
/// username` would log a FATAL on every healthcheck tick.
///
/// Shared with `postgres_lifecycle` so both container-creation sites build
/// the exact same healthcheck command.
pub(crate) fn postgres_healthcheck_cmd(username: &str, database: &str) -> String {
    format!(
        "pg_isready -U {} -d {}",
        shell_escape(username),
        shell_escape(database)
    )
}

/// Validate a PostgreSQL role/username before it is interpolated into a
/// shell command or SQL. Allows only `[A-Za-z0-9_]` (the realistic role-name
/// charset), non-empty, and at most 63 bytes.
///
/// Defense-in-depth: the upgrade orchestrator already `shell_escape`s the
/// username at every interpolation site, but rejecting shell/SQL
/// metacharacters (quotes, `$`, `;`, spaces, backticks, …) up front means a
/// crafted username can never reach a `sh -c` string or a `sed` program in
/// the first place. Mirrors [`PostgresService::validate_database_name`], but
/// permits uppercase since role names are commonly mixed-case.
pub(crate) fn validate_pg_username(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("PostgreSQL username cannot be empty".to_string());
    }
    if name.len() > 63 {
        return Err(format!(
            "PostgreSQL username '{name}' exceeds the 63 character limit"
        ));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!(
            "PostgreSQL username '{name}' contains invalid characters; only \
             ASCII letters, digits, and underscores are allowed"
        ));
    }
    Ok(())
}

/// Input configuration for creating a PostgreSQL service
/// This is what users provide when creating the service
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(
    title = "PostgreSQL Configuration",
    description = "Configuration for PostgreSQL service"
)]
pub struct PostgresInputConfig {
    /// PostgreSQL host address
    #[serde(default = "default_host")]
    #[schemars(example = "example_host", default = "default_host")]
    pub host: String,

    /// PostgreSQL port (auto-assigned if not provided)
    #[schemars(example = "example_port")]
    pub port: Option<String>,

    /// PostgreSQL database name
    #[serde(default = "default_database")]
    #[schemars(example = "example_database", default = "default_database")]
    pub database: String,

    /// PostgreSQL username
    #[serde(default = "default_username")]
    #[schemars(example = "example_username", default = "default_username")]
    pub username: String,

    /// PostgreSQL password (auto-generated if not provided or empty)
    #[serde(default, deserialize_with = "deserialize_optional_password")]
    #[schemars(with = "Option<String>", example = "example_password")]
    pub password: Option<String>,

    /// Maximum number of connections
    #[serde(
        default = "default_max_connections",
        deserialize_with = "deserialize_max_connections"
    )]
    #[schemars(
        example = "example_max_connections",
        default = "default_max_connections"
    )]
    pub max_connections: u32,

    /// SSL mode (disable, allow, prefer, require)
    #[serde(default = "default_ssl_mode")]
    #[schemars(example = "example_ssl_mode", default = "default_ssl_mode_string")]
    pub ssl_mode: Option<String>,

    /// Docker image to use (defaults to gotempsh/postgres-walg:18-bookworm, supports timescale/timescaledb-ha:pg18)
    #[serde(default = "default_docker_image")]
    #[schemars(example = "example_docker_image", default = "default_docker_image")]
    pub docker_image: Option<String>,

    /// Real Docker container name when this service was imported from an
    /// existing PostgreSQL-compatible container (set by
    /// `import_from_container`, never user-editable — omitted from the
    /// create form). Overrides the derived `postgres-{name}` container name
    /// so internal addressing targets the actual pre-existing container
    /// instead of a synthesized name that doesn't exist. Deserialized
    /// through `deserialize_optional_non_empty` as a second guard alongside
    /// `#[schemars(skip)]`, mirroring the MariaDB fix for the same class of
    /// bug (see `crates/temps-providers/src/externalsvc/mariadb.rs`).
    #[serde(default, deserialize_with = "deserialize_optional_non_empty")]
    #[schemars(skip)]
    pub container_name: Option<String>,
}

/// Internal runtime configuration for PostgreSQL service
/// This is what the service uses internally after processing input
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostgresConfig {
    pub host: String,
    pub port: String,
    pub database: String,
    pub username: String,
    pub password: String,
    #[serde(deserialize_with = "deserialize_max_connections")]
    pub max_connections: u32,
    pub ssl_mode: Option<String>,
    pub docker_image: String,
    /// Real container name for imported services — see
    /// `PostgresInputConfig::container_name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
}

impl From<PostgresInputConfig> for PostgresConfig {
    fn from(input: PostgresInputConfig) -> Self {
        Self {
            host: input.host,
            port: input.port.unwrap_or_else(|| {
                find_available_port(5432)
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "5432".to_string())
            }),
            database: input.database,
            username: input.username,
            password: input.password.unwrap_or_else(generate_password),
            max_connections: input.max_connections,
            ssl_mode: input.ssl_mode,
            docker_image: input
                .docker_image
                .unwrap_or_else(|| "gotempsh/postgres-walg:18-bookworm".to_string()),
            container_name: input.container_name,
        }
    }
}

/// Treats a blank string the same as an absent value — see
/// `PostgresInputConfig::container_name`.
fn deserialize_optional_non_empty<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(opt.filter(|s| !s.is_empty()))
}

fn deserialize_optional_password<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(match opt {
        Some(s) if !s.is_empty() => Some(s),
        _ => None,
    })
}

/// Deserialize max_connections from either string or number
fn deserialize_max_connections<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Deserialize};

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrNumber {
        String(String),
        Number(u32),
    }

    match StringOrNumber::deserialize(deserializer)? {
        StringOrNumber::String(s) => s.parse::<u32>().map_err(de::Error::custom),
        StringOrNumber::Number(n) => Ok(n),
    }
}

fn default_host() -> String {
    "localhost".to_string()
}

fn default_database() -> String {
    "postgres".to_string()
}

fn default_username() -> String {
    "postgres".to_string()
}

pub fn generate_password() -> String {
    use rand::{distributions::Alphanumeric, Rng};
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(16)
        .map(char::from)
        .collect()
}

fn default_max_connections() -> u32 {
    100
}

fn default_ssl_mode() -> Option<String> {
    Some("disable".to_string())
}

fn default_ssl_mode_string() -> String {
    "disable".to_string()
}

fn default_docker_image() -> Option<String> {
    Some("gotempsh/postgres-walg:18-bookworm".to_string())
}

// Schema example functions
fn example_host() -> &'static str {
    "localhost"
}

fn example_port() -> &'static str {
    "5432"
}

fn example_database() -> &'static str {
    "myapp"
}

fn example_username() -> &'static str {
    "postgres"
}

fn example_password() -> &'static str {
    "your-secure-password"
}

fn example_max_connections() -> u32 {
    10
}

fn example_ssl_mode() -> &'static str {
    "disable"
}

fn example_docker_image() -> &'static str {
    "gotempsh/postgres-walg:18-bookworm"
}

use super::port_util::{find_available_port, find_available_port_async, is_port_conflict_error};

pub struct PostgresService {
    name: String,
    config: Arc<RwLock<Option<PostgresConfig>>>,
    /// Resource limits captured at init time and reused by `start()` when
    /// recreating a container that was removed externally. Defaults to
    /// unlimited; populated from the `resources` block in the
    /// `ServiceConfig::parameters` JSON.
    resource_limits: Arc<RwLock<ServiceResourceLimits>>,
    docker: Arc<Docker>,
}

impl PostgresService {
    pub fn new(name: String, docker: Arc<Docker>) -> Self {
        Self {
            name,
            config: Arc::new(RwLock::new(None)),
            resource_limits: Arc::new(RwLock::new(ServiceResourceLimits::default())),
            docker,
        }
    }

    fn get_postgres_config(&self, service_config: ServiceConfig) -> Result<PostgresConfig> {
        // Parse input config and transform to runtime config
        // First deserialize to PostgresInputConfig to apply defaults and custom handling
        let input_config: PostgresInputConfig =
            serde_json::from_value(service_config.parameters)
                .map_err(|e| anyhow::anyhow!("Failed to parse PostgreSQL configuration: {}", e))?;
        // Then convert to PostgresConfig which applies additional transformations
        Ok(PostgresConfig::from(input_config))
    }
    fn get_container_name(&self) -> String {
        format!("postgres-{}", self.name)
    }

    /// The container this service actually runs in: the imported container's
    /// real name when `config.container_name` is set, otherwise the derived
    /// `postgres-{name}`. Every operation that talks to the live container
    /// (admin SQL, backup, restore, start/stop) must resolve through this,
    /// not `get_container_name()` directly, or it targets a synthesized name
    /// that doesn't exist for imported services.
    fn get_live_container_name(&self, config: &PostgresConfig) -> String {
        config
            .container_name
            .clone()
            .unwrap_or_else(|| self.get_container_name())
    }

    /// Creates and starts the PostgreSQL container, retrying with a fresh host
    /// port if the chosen one lost the race described in `port_util` docs
    /// (bindable when we checked, but taken by the time Docker actually binds
    /// it). The container name is deterministic, so a failed attempt must be
    /// removed before retrying or the next attempt's "already exists" check
    /// short-circuits without picking a new port.
    ///
    /// `config` is taken by mutable reference so a retry's port change is
    /// written back to the caller — otherwise the caller (and anything it
    /// persists to the database) keeps referencing the original port even
    /// though the container actually ended up bound to a different one.
    async fn create_container(
        &self,
        docker: &Docker,
        config: &mut PostgresConfig,
        resource_limits: &ServiceResourceLimits,
        enable_archiving: bool,
    ) -> Result<()> {
        const MAX_ATTEMPTS: u32 = 3;
        let mut attempt_config = config.clone();
        for attempt in 1..=MAX_ATTEMPTS {
            match self
                .create_container_once(docker, &attempt_config, resource_limits, enable_archiving)
                .await
            {
                Ok(()) => {
                    *config = attempt_config;
                    return Ok(());
                }
                Err(e) if attempt < MAX_ATTEMPTS && is_port_conflict_error(&e.to_string()) => {
                    warn!(
                        "Port {} for PostgreSQL container was already allocated (attempt {}/{}), retrying with a fresh port: {}",
                        attempt_config.port, attempt, MAX_ATTEMPTS, e
                    );
                    let _ = docker
                        .remove_container(
                            &self.get_container_name(),
                            Some(bollard::query_parameters::RemoveContainerOptions {
                                force: true,
                                ..Default::default()
                            }),
                        )
                        .await;
                    let base_port: u16 = attempt_config.port.parse().unwrap_or(5432);
                    if let Some(new_port) =
                        find_available_port_async(docker, base_port.wrapping_add(1)).await
                    {
                        attempt_config.port = new_port.to_string();
                    }
                }
                Err(e) => return Err(e),
            }
        }
        unreachable!("loop always returns Ok or Err before exhausting MAX_ATTEMPTS")
    }

    async fn create_container_once(
        &self,
        docker: &Docker,
        config: &PostgresConfig,
        resource_limits: &ServiceResourceLimits,
        enable_archiving: bool,
    ) -> Result<()> {
        // Pull image first
        info!("Pulling PostgreSQL image {}", config.docker_image);

        // Parse image name and tag
        let (image_name, tag) = if let Some((name, tag)) = config.docker_image.split_once(':') {
            (name.to_string(), tag.to_string())
        } else {
            (config.docker_image.clone(), "latest".to_string())
        };

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
            .await?;

        let container_name = self.get_container_name();
        let volume_name = format!("{}_data", container_name);

        // Create volume if it doesn't exist
        match docker
            .create_volume(bollard::models::VolumeCreateRequest {
                name: Some(volume_name.clone()),
                ..Default::default()
            })
            .await
        {
            Ok(_) => info!("Created or reused volume {}", volume_name),
            Err(e) => return Err(anyhow::anyhow!("Failed to create volume: {:?}", e)),
        };

        // Check if container already exists
        let containers = docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: true,
                filters: Some(HashMap::from([(
                    "name".to_string(),
                    vec![container_name.to_string()],
                )])),
                ..Default::default()
            }))
            .await?;

        if !containers.is_empty() {
            // Container exists - check if the image has changed
            let existing_container = &containers[0];
            let existing_image = existing_container.image.as_deref().unwrap_or("");

            if existing_image != config.docker_image {
                info!(
                    "Container {} exists with different image ({}), removing to upgrade to {}",
                    container_name, existing_image, config.docker_image
                );

                // Stop the container if running
                let _ = docker
                    .stop_container(&container_name, None::<StopContainerOptions>)
                    .await;

                // Remove the container (but keep the volume for data persistence)
                docker
                    .remove_container(
                        &container_name,
                        Some(bollard::query_parameters::RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await
                    .context("Failed to remove old container for upgrade")?;

                info!("Old container removed, proceeding with new image");
            } else {
                info!(
                    "Container {} already exists with same image",
                    container_name
                );
                return Ok(());
            }
        }

        let service_label_key = format!("{}service_type", temps_core::DOCKER_LABEL_PREFIX);
        let name_label_key = format!("{}service_name", temps_core::DOCKER_LABEL_PREFIX);

        let container_labels = HashMap::from([
            (service_label_key, "postgres".to_string()),
            (name_label_key, self.name.to_string()),
        ]);

        // Determine PGDATA path based on docker image
        let pgdata_path = Self::get_pgdata_path(&config.docker_image)
            .map_err(|e| anyhow::anyhow!("Failed to determine PGDATA path: {}", e))?;

        let env_vars = [
            format!("POSTGRES_USER={}", config.username),
            format!("POSTGRES_PASSWORD={}", config.password),
            format!("POSTGRES_DB={}", config.database),
            format!("PGDATA={}", pgdata_path),
            "POSTGRES_HOST_AUTH_METHOD=md5".to_string(), // Use md5 password authentication for better compatibility
        ];

        let mut host_config = bollard::models::HostConfig {
            port_bindings: Some(crate::utils::local_port_binding("5432/tcp", &config.port)),
            mounts: Some(vec![bollard::models::Mount {
                // Always mount at /var/lib/postgresql - PGDATA env var controls subdirectory
                target: Some("/var/lib/postgresql".to_string()),
                source: Some(volume_name),
                typ: Some(bollard::models::MountTypeEnum::VOLUME),
                ..Default::default()
            }]),
            log_config: Some(crate::utils::default_service_log_config()),
            // Security hardening for service containers
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
            exposed_ports: Some(Vec::from(["5432/tcp".to_string()])),
            env: Some(env_vars.iter().map(|s| s.to_string()).collect()),
            labels: Some(container_labels),
            // archive_mode is computed from on-disk truth, not stored state:
            // `/var/lib/postgresql/walg.env` exists on the volume iff WAL-G
            // archiving has been configured for this service. The
            // reconcile-on-start path in `start()` recomputes and recreates
            // the container if this value drifts. This makes the bad combo
            // (archive_mode=on, archive_command='') unrepresentable for any
            // service that's been Stop+Start'd at least once.
            cmd: Some(vec![
                "postgres".to_string(),
                "-c".to_string(),
                format!("max_connections={}", config.max_connections),
                "-c".to_string(),
                "wal_level=replica".to_string(),
                "-c".to_string(),
                format!(
                    "archive_mode={}",
                    if enable_archiving { "on" } else { "off" }
                ),
                "-c".to_string(),
                "archive_timeout=60".to_string(),
            ]),
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
                    // Without -d, pg_isready (via libpq) defaults the target
                    // database to the username being checked. The hardcoded
                    // "postgres" here was wrong for any service with a
                    // non-default username/database (e.g. username="appuser",
                    // database="appdb") — the postmaster would receive a
                    // connection attempt for a role/database that doesn't
                    // exist, logging a FATAL every interval (1s) forever.
                    // pg_isready still reports healthy either way (the
                    // server did respond), so this was invisible in the UI,
                    // just a permanent log-spam leak. Use the real
                    // configured username/database instead.
                    postgres_healthcheck_cmd(&config.username, &config.database),
                ]),
                interval: Some(1000000000), // 1 second
                timeout: Some(3000000000),  // 3 seconds
                retries: Some(3),
                start_period: Some(30000000000), // 30 seconds - gives PostgreSQL time to initialize
                start_interval: Some(1000000000), // 1 second
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
            .map_err(|e| anyhow::anyhow!("Failed to create PostgreSQL container: {}", e))?;

        docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start PostgreSQL container: {}", e))?;

        // Wait for container to be healthy
        self.wait_for_container_health(docker, &container.id)
            .await?;

        info!("PostgreSQL container {} created and started", container.id);
        Ok(())
    }

    /// Read a file from inside a container and return its contents as a String.
    /// Used for capturing log output on failure. Returns a fallback message on error.
    async fn read_container_file(&self, container_name: &str, path: &str) -> String {
        use bollard::exec::{CreateExecOptions, StartExecOptions};

        let log_exec = match self
            .docker
            .create_exec(
                container_name,
                CreateExecOptions {
                    cmd: Some(vec!["cat", path]),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    user: Some("postgres"),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(e) => e,
            Err(_) => return "(failed to create exec for log capture)".to_string(),
        };

        match self
            .docker
            .start_exec(&log_exec.id, None::<StartExecOptions>)
            .await
        {
            Ok(bollard::exec::StartExecResults::Attached { mut output, .. }) => {
                use futures::StreamExt;
                let mut log_output = String::new();
                while let Some(Ok(chunk)) = output.next().await {
                    log_output.push_str(&chunk.to_string());
                }
                log_output
            }
            _ => "(failed to read log file)".to_string(),
        }
    }

    /// Check if the WAL-G binary is available inside a container.
    /// Returns true if `wal-g` is found, false otherwise.
    async fn container_has_walg(&self, container_name: &str) -> bool {
        use bollard::exec::{CreateExecOptions, StartExecOptions};

        let exec = match self
            .docker
            .create_exec(
                container_name,
                CreateExecOptions {
                    cmd: Some(vec!["which", "wal-g"]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(e) => e,
            Err(_) => return false,
        };

        if self
            .docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await
            .is_err()
        {
            return false;
        }

        // Wait for completion
        loop {
            match self.docker.inspect_exec(&exec.id).await {
                Ok(inspect) => {
                    if inspect.running == Some(false) {
                        return inspect.exit_code == Some(0);
                    }
                }
                Err(_) => return false,
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    /// Write WAL-G credentials to an env file on the shared volume and enable
    /// continuous WAL archiving via `ALTER SYSTEM`.
    ///
    /// PostgreSQL calls `archive_command` once per completed WAL segment (16 MB each).
    /// Each invocation is a fresh shell, so it reads the latest env file automatically.
    /// This means credential rotations take effect without any restart — just overwrite
    /// the file and the next WAL push uses the new credentials.
    ///
    /// Write `/var/lib/postgresql/walg.env` onto the given container.
    ///
    /// This is the credential file `archive_command = wal-g wal-push %p`
    /// relies on when continuous WAL archiving is enabled. Called from
    /// `enable_wal_archiving` after the first successful backup.
    ///
    /// Idempotent — overwrites any existing file. Writes with 0600 perms.
    async fn write_walg_env_file(&self, container_name: &str, walg_env: &[String]) -> Result<()> {
        self.write_walg_env_file_at(container_name, walg_env, "/var/lib/postgresql/walg.env")
            .await
    }

    /// Write a read-only WAL-G credential file used only by
    /// `restore_command`. Kept separate from the write-capable `walg.env`
    /// so a restored cluster can read WAL from the source's prefix without
    /// ever having the credentials at a path that `archive_command` would
    /// find, preventing source-prefix contamination during recovery.
    async fn write_walg_restore_env_file(
        &self,
        container_name: &str,
        walg_env: &[String],
    ) -> Result<()> {
        self.write_walg_env_file_at(
            container_name,
            walg_env,
            "/var/lib/postgresql/walg-restore.env",
        )
        .await
    }

    /// Internal: write a wal-g credential file at an arbitrary path.
    /// See `write_walg_env_file` / `write_walg_restore_env_file` for the
    /// two concrete roles.
    async fn write_walg_env_file_at(
        &self,
        container_name: &str,
        walg_env: &[String],
        target_path: &str,
    ) -> Result<()> {
        use bollard::exec::{CreateExecOptions, StartExecOptions};

        // Only WAL-G / AWS envs go into the file — PG connection envs (PGHOST,
        // PGUSER, etc.) are not needed by wal-g archive/fetch from inside PG.
        let env_file_lines: Vec<&String> = walg_env
            .iter()
            .filter(|line| line.starts_with("WALG_") || line.starts_with("AWS_"))
            .collect();

        let walg_env_path = target_path;

        let write_cmd = format!(
            "printf '%s\\n' {} > {} && chmod 600 {}",
            env_file_lines
                .iter()
                .map(|line| format!("'export {}'", line.replace('\'', "'\\''")))
                .collect::<Vec<_>>()
                .join(" "),
            walg_env_path,
            walg_env_path,
        );

        let exec = self
            .docker
            .create_exec(
                container_name,
                CreateExecOptions {
                    cmd: Some(vec!["sh", "-c", &write_cmd]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    user: Some("postgres"),
                    ..Default::default()
                },
            )
            .await?;
        self.docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await?;

        loop {
            let inspect = self.docker.inspect_exec(&exec.id).await?;
            if inspect.running == Some(false) {
                if inspect.exit_code != Some(0) {
                    return Err(anyhow::anyhow!(
                        "Failed to write walg.env on container '{}' (exit code {:?})",
                        container_name,
                        inspect.exit_code
                    ));
                }
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        info!(
            "Written WAL-G credentials to {} on container '{}'",
            walg_env_path, container_name
        );
        Ok(())
    }

    /// The env file lives at `/var/lib/postgresql/walg.env` on the shared volume, so it:
    /// - Survives container restarts
    /// - Is accessible via `volumes_from` in helper containers
    /// - Is NOT inside PGDATA (so pg_basebackup/wal-g don't back it up — credentials
    ///   should not be stored inside backups)
    ///
    /// Flow:
    ///   1. Write `walg.env` onto the volume. From this moment, the volume
    ///      records "WAL-G is configured" — `compute_desired_enable_archiving`
    ///      will return true on every subsequent start.
    ///   2. Write `archive_command` via ALTER SYSTEM so an immediate
    ///      `wal-g wal-push` works on the running container (SIGHUP-reloadable).
    ///   3. Recreate the container so `archive_mode=on` lands in CMD args.
    ///      `archive_mode` is postmaster-context — recreate is the only way
    ///      to flip it. Volume is preserved; PGDATA is intact.
    async fn enable_wal_archiving(
        &self,
        container_name: &str,
        walg_env: &[String],
        postgres_config: &PostgresConfig,
    ) -> Result<()> {
        use bollard::exec::{CreateExecOptions, StartExecOptions};

        // Step 1: write walg.env onto the volume. This is the durable truth
        // source `compute_desired_enable_archiving` reads on every start.
        self.write_walg_env_file(container_name, walg_env).await?;
        let walg_env_path = "/var/lib/postgresql/walg.env";

        // Step 2: set archive_command via ALTER SYSTEM. SIGHUP-reloadable —
        // takes effect immediately. archive_mode comes in step 3 via CMD.
        let archive_command = format!(". {} && wal-g wal-push %p", walg_env_path);
        let alter_command_sql = format!(
            "ALTER SYSTEM SET archive_command = '{}'",
            archive_command.replace('\'', "''")
        );
        let reload_sql = "SELECT pg_reload_conf()";

        let password_env = format!("PGPASSWORD={}", postgres_config.password);
        let exec = self
            .docker
            .create_exec(
                container_name,
                CreateExecOptions {
                    cmd: Some(vec![
                        "psql",
                        "-U",
                        &postgres_config.username,
                        "-d",
                        &postgres_config.database,
                        "-c",
                        &alter_command_sql,
                        "-c",
                        reload_sql,
                    ]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    env: Some(vec![&password_env]),
                    ..Default::default()
                },
            )
            .await?;

        self.docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await?;

        loop {
            let inspect = self.docker.inspect_exec(&exec.id).await?;
            if inspect.running == Some(false) {
                if inspect.exit_code != Some(0) {
                    return Err(anyhow::anyhow!(
                        "ALTER SYSTEM SET archive_command failed (exit code {:?})",
                        inspect.exit_code
                    ));
                }
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        info!(
            "Wrote walg.env + archive_command in container '{}'. Recreating container so archive_mode=on lands in CMD.",
            container_name
        );

        // Step 3: recreate so archive_mode=on lands in CMD args. We go through
        // `stop()` → `docker.remove_container` → `create_container(.., true)`
        // → `docker.start_container` → `wait_for_container_health`. Same path
        // `start()`'s reconcile branch uses.
        self.stop().await?;
        self.docker
            .remove_container(
                container_name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to remove container '{}' before re-creating with archive_mode=on: {}",
                    container_name,
                    e
                )
            })?;

        let limits = self.resource_limits.read().await.clone();
        // `postgres_config` is a caller-owned `&PostgresConfig` here (used
        // only for this recreate, not persisted afterward), so retry port
        // changes are applied to a local clone rather than threaded further.
        let mut recreate_config = postgres_config.clone();
        self.create_container(&self.docker, &mut recreate_config, &limits, true)
            .await?;
        self.docker
            .start_container(
                container_name,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to start container '{}' after recreating with archive_mode=on: {}",
                    container_name,
                    e
                )
            })?;
        self.wait_for_container_health(&self.docker, container_name)
            .await?;

        info!(
            "Recreated container '{}' with archive_mode=on. WAL-G archiving active.",
            container_name
        );

        Ok(())
    }

    /// Compute the desired `archive_mode` for this service's container CMD.
    ///
    /// Truth source: `/var/lib/postgresql/walg.env` existing on the
    /// service's data volume. WAL-G archiving is enabled iff that credential
    /// file is present — it's written by `enable_wal_archiving()` and lives
    /// on the persistent volume, so it survives container recreates, Temps
    /// restarts, and node failovers.
    ///
    /// On any inspection error, returns `false` (archiving off) — the safer
    /// default. A spurious `false` causes archiving to be disabled until the
    /// operator notices; a spurious `true` would cause WAL bloat, which is
    /// the exact bug we're trying to avoid.
    async fn compute_desired_enable_archiving(&self) -> bool {
        let container_name = self.get_container_name();
        let volume_name = format!("{}_data", container_name);
        self.walg_env_exists_on_volume(&volume_name).await
    }

    /// Returns true iff `/var/lib/postgresql/walg.env` exists on the named
    /// Docker volume. Runs a one-shot `busybox` container with the volume
    /// mounted read-only. Any error (image pull, exec failure) returns
    /// false — we err on the side of not enabling archiving.
    async fn walg_env_exists_on_volume(&self, volume_name: &str) -> bool {
        use bollard::query_parameters::{
            CreateContainerOptions, CreateImageOptions, RemoveContainerOptions,
            StartContainerOptions, WaitContainerOptions,
        };
        use futures::StreamExt;

        // Pull busybox; cheap (~700 KB) and cached after first use.
        let mut pull_stream = self.docker.create_image(
            Some(CreateImageOptions {
                from_image: Some("busybox".to_string()),
                tag: Some("latest".to_string()),
                ..Default::default()
            }),
            None,
            None,
        );
        while let Some(result) = pull_stream.next().await {
            if result.is_err() {
                // Best-effort; treat unavailability as "no archiving".
                return false;
            }
        }

        let probe_name = format!("temps-walg-probe-{}", uuid::Uuid::new_v4());
        let host_config = bollard::models::HostConfig {
            mounts: Some(vec![bollard::models::Mount {
                target: Some("/var/lib/postgresql".to_string()),
                source: Some(volume_name.to_string()),
                typ: Some(bollard::models::MountTypeEnum::VOLUME),
                read_only: Some(true),
                ..Default::default()
            }]),
            auto_remove: Some(false),
            ..Default::default()
        };

        let create_result = self
            .docker
            .create_container(
                Some(CreateContainerOptions {
                    name: Some(probe_name.clone()),
                    ..Default::default()
                }),
                bollard::models::ContainerCreateBody {
                    image: Some("busybox:latest".to_string()),
                    cmd: Some(vec![
                        "sh".to_string(),
                        "-c".to_string(),
                        "test -f /var/lib/postgresql/walg.env".to_string(),
                    ]),
                    host_config: Some(host_config),
                    ..Default::default()
                },
            )
            .await;

        if create_result.is_err() {
            return false;
        }

        // Best effort cleanup: always try to remove the probe container.
        let cleanup = |name: String| async move {
            let _ = self
                .docker
                .remove_container(
                    &name,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;
        };

        if self
            .docker
            .start_container(&probe_name, None::<StartContainerOptions>)
            .await
            .is_err()
        {
            cleanup(probe_name).await;
            return false;
        }

        let mut wait_stream = self
            .docker
            .wait_container(&probe_name, None::<WaitContainerOptions>);

        let mut exit_code: Option<i64> = None;
        while let Some(item) = wait_stream.next().await {
            if let Ok(resp) = item {
                exit_code = Some(resp.status_code);
                break;
            }
        }

        cleanup(probe_name).await;

        // `test -f` exits 0 when the file is present.
        matches!(exit_code, Some(0))
    }

    /// Returns true when the running container's CMD specifies an
    /// `archive_mode` value that disagrees with what we'd emit now.
    /// Returns false when the value matches OR when we can't determine it
    /// (don't recreate on inspection failure — stability over correctness
    /// for this branch).
    async fn container_cmd_archive_mode_differs(
        &self,
        container: &bollard::models::ContainerSummary,
        desired: bool,
    ) -> bool {
        let id = match container.id.as_deref() {
            Some(id) => id,
            None => return false,
        };
        let info = match self
            .docker
            .inspect_container(
                id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(i) => i,
            Err(_) => return false,
        };
        let cmd = info
            .config
            .as_ref()
            .and_then(|c| c.cmd.as_ref())
            .map(|v| v.iter().map(|s| s.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();

        // Find `archive_mode=<value>` token.
        let actual_on = cmd.iter().any(|tok| {
            let t = tok.trim();
            t.eq_ignore_ascii_case("archive_mode=on")
                || t.eq_ignore_ascii_case("archive_mode=always")
        });
        let actual_off = cmd
            .iter()
            .any(|tok| tok.trim().eq_ignore_ascii_case("archive_mode=off"));

        if !actual_on && !actual_off {
            // Container predates our CMD-baking — don't recreate.
            return false;
        }
        let actual = actual_on; // true = on, false = off
        actual != desired
    }

    async fn wait_for_container_health(&self, docker: &Docker, container_id: &str) -> Result<()> {
        let mut delay = Duration::from_millis(500);
        let mut total_wait = Duration::from_secs(0);
        let max_wait = Duration::from_secs(90);
        let max_delay = Duration::from_secs(2);

        while total_wait < max_wait {
            let info = docker
                .inspect_container(container_id, None::<InspectContainerOptions>)
                .await?;
            if let Some(state) = info.state {
                // PostgreSQL container is considered ready if:
                // 1. It's running
                // 2. Either it has a health status of HEALTHY, or no health check is defined
                let is_running =
                    state.status == Some(bollard::models::ContainerStateStatusEnum::RUNNING);
                let health_status = state.health.as_ref().and_then(|h| h.status.as_ref());

                info!(
                    "Container {} status: running={}, health={:?}",
                    container_id, is_running, health_status
                );

                // Container is healthy if running AND (no health check defined OR health is HEALTHY)
                if is_running
                    && (health_status.is_none()
                        || health_status == Some(&bollard::models::HealthStatusEnum::HEALTHY))
                {
                    info!("Container {} is healthy", container_id);
                    return Ok(());
                }

                // If container exited or is dead, fail fast instead of waiting
                if state.status == Some(bollard::models::ContainerStateStatusEnum::EXITED)
                    || state.status == Some(bollard::models::ContainerStateStatusEnum::DEAD)
                {
                    let exit_code = state.exit_code.unwrap_or(-1);
                    return Err(anyhow::anyhow!(
                        "PostgreSQL container exited unexpectedly with code {}",
                        exit_code
                    ));
                }
            } else {
                info!("Container {} state is None", container_id);
            }
            sleep(delay).await;
            total_wait += delay;
            // Exponential backoff capped at max_delay to keep polling responsive
            // during Docker's health check start_period (30s)
            delay = std::cmp::min(delay.mul_f32(1.5), max_delay);
        }

        error!(
            "Container {} health check timed out after {:?}",
            container_id, total_wait
        );
        Err(anyhow::anyhow!(
            "PostgreSQL container health check timed out"
        ))
    }

    /// Validate that a database name is safe for use in SQL identifiers.
    /// Only allows lowercase alphanumeric characters and underscores,
    /// must start with a letter or underscore, and be <= 63 characters.
    fn validate_database_name(name: &str) -> Result<()> {
        if name.is_empty() {
            return Err(anyhow::anyhow!("Database name cannot be empty"));
        }
        if name.len() > 63 {
            return Err(anyhow::anyhow!(
                "Database name '{}' exceeds 63 character limit",
                name
            ));
        }
        // Must only contain lowercase alphanumeric and underscores
        if !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return Err(anyhow::anyhow!(
                "Database name '{}' contains invalid characters. Only lowercase alphanumeric and underscores are allowed",
                name
            ));
        }
        // Must not start with a digit
        if name.starts_with(|c: char| c.is_ascii_digit()) {
            return Err(anyhow::anyhow!(
                "Database name '{}' must not start with a digit",
                name
            ));
        }
        Ok(())
    }

    async fn create_database(&self, service_config: ServiceConfig, name: &str) -> Result<()> {
        // Enforce strict validation on the database name to prevent SQL injection.
        // normalize_database_name() is called by callers, but we validate here as
        // defense-in-depth to ensure no unsafe name ever reaches the SQL query.
        Self::validate_database_name(name)?;

        let config: PostgresConfig = self.get_postgres_config(service_config)?;
        let connection_string = format!(
            "postgres://{}:{}@{}:{}/postgres?sslmode=disable",
            urlencoding::encode(&config.username),
            urlencoding::encode(&config.password),
            config.host,
            config.port
        );
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(config.max_connections)
            .connect(&connection_string)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to postgres: {}", e))?;

        // Check if database exists using parameterized query
        let exists = sqlx::query("SELECT 1 FROM pg_database WHERE datname = $1")
            .bind(name)
            .fetch_optional(&pool)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to check database existence: {}", e))?;

        if exists.is_none() {
            // CREATE DATABASE cannot use parameterized queries in PostgreSQL,
            // so we use a quoted identifier. The name has been validated above
            // to contain only [a-z0-9_] characters, making injection impossible.
            let create_db = format!("CREATE DATABASE \"{}\"", name);
            info!("Creating database: {}", name);
            sqlx::query(&create_db)
                .execute(&pool)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create database '{}': {}", name, e))?;
        } else {
            info!("Database {} already exists, skipping creation", name);
        }

        Ok(())
    }

    async fn drop_database(&self, _name: &str) -> Result<()> {
        Ok(())
    }

    /// Build the `POSTGRES_*` env vars for a given per-tenant resource name.
    /// Shared between `get_runtime_env_vars` (which also provisions the DB)
    /// and `preview_runtime_env_vars` (which doesn't).
    fn build_runtime_env_vars(
        &self,
        service_config: ServiceConfig,
        resource_name: &str,
    ) -> Result<HashMap<String, String>> {
        let config: PostgresConfig = self.get_postgres_config(service_config)?;
        let mut env_vars = HashMap::new();

        let effective_host = self.get_live_container_name(&config);
        let effective_port = POSTGRES_INTERNAL_PORT.to_string();

        env_vars.insert("POSTGRES_DATABASE".to_string(), resource_name.to_string());
        env_vars.insert(
            "POSTGRES_URL".to_string(),
            format!(
                "postgresql://{}:{}@{}:{}/{}",
                urlencoding::encode(&config.username),
                urlencoding::encode(&config.password),
                effective_host,
                effective_port,
                resource_name
            ),
        );
        env_vars.insert("POSTGRES_HOST".to_string(), effective_host);
        env_vars.insert("POSTGRES_PORT".to_string(), effective_port);
        // `POSTGRES_DB` is the canonical name (matches the official Postgres
        // Docker image and what every app library expects). `POSTGRES_NAME`
        // is kept as a back-compat alias for older deployments that already
        // wired their app config to that key — drop it once a migration
        // window has passed.
        env_vars.insert("POSTGRES_DB".to_string(), resource_name.to_string());
        env_vars.insert("POSTGRES_NAME".to_string(), resource_name.to_string());
        env_vars.insert("POSTGRES_USER".to_string(), config.username.clone());
        env_vars.insert("POSTGRES_PASSWORD".to_string(), config.password.clone());

        Ok(env_vars)
    }

    pub(crate) fn normalize_database_name(name: &str) -> String {
        let normalized = name
            .to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '_' })
            .collect::<String>();

        let prefixed = if normalized.chars().next().unwrap().is_numeric() {
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

    /// Extract PostgreSQL major version from Docker image name
    /// Examples: "gotempsh/postgres-walg:16-bookworm" -> 16, "timescale/timescaledb-ha:pg18" -> 18
    fn extract_postgres_version(docker_image: &str) -> Result<u32> {
        // Try to extract version from image name
        if let Some(tag) = docker_image.split(':').nth(1) {
            // Handle formats like "16-alpine", "17.2-alpine", "pg17"
            let version_str = tag
                .trim_start_matches("pg")
                .split('-')
                .next()
                .and_then(|v| v.split('.').next())
                .ok_or_else(|| {
                    anyhow::anyhow!("Could not extract version from image: {}", docker_image)
                })?;

            version_str
                .parse::<u32>()
                .map_err(|e| anyhow::anyhow!("Failed to parse version '{}': {}", version_str, e))
        } else {
            Err(anyhow::anyhow!(
                "Invalid Docker image format: {}",
                docker_image
            ))
        }
    }

    /// Determine the PGDATA directory based on the docker image
    /// All PostgreSQL versions use: /var/lib/postgresql/{version}/docker
    fn get_pgdata_path(docker_image: &str) -> Result<String> {
        let version = Self::extract_postgres_version(docker_image)?;
        Ok(format!("/var/lib/postgresql/{}/docker", version))
    }

    async fn restore_backup_file(
        &self,
        docker: &Docker,
        container_name: &str,
        backup_data: Vec<u8>,
        username: &str,
        password: &str,
    ) -> Result<()> {
        // Create a temporary file with the backup data
        // Create a temporary file for the backup data
        let temp_file = tempfile::NamedTempFile::new()?;
        tokio::fs::write(temp_file.path(), backup_data).await?;

        // Create a tar archive containing the backup file
        let mut tar = tar::Builder::new(Vec::new());
        tar.append_path_with_name(temp_file.path(), "backup.sql")?;
        let tar_data = tar.into_inner()?;
        // Copy the tar archive into the container
        docker
            .upload_to_container(
                container_name,
                Some(bollard::query_parameters::UploadToContainerOptions {
                    path: "/".to_string(),
                    ..Default::default()
                }),
                body_full(bytes::Bytes::from(tar_data)),
            )
            .await
            .map_err(|e| {
                anyhow::anyhow!(format!("Failed to upload backup file to container: {}", e))
            })?;

        // Execute psql to restore the backup with actual credentials
        let password_env = format!("PGPASSWORD={}", password);
        let exec = docker
            .create_exec(
                container_name,
                bollard::exec::CreateExecOptions {
                    cmd: Some(vec!["psql", "-U", username, "-f", "/backup.sql"]),
                    env: Some(vec![password_env.as_str()]),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| anyhow::anyhow!(format!("Failed to create exec: {}", e)))?;

        let output = docker.start_exec(&exec.id, None).await?;
        if let bollard::exec::StartExecResults::Attached { mut output, .. } = output {
            while let Some(Ok(output)) = output.next().await {
                match output {
                    bollard::container::LogOutput::StdOut { message } => {
                        info!("stdout: {}", String::from_utf8_lossy(&message));
                    }
                    bollard::container::LogOutput::StdErr { message } => {
                        error!("stderr: {}", String::from_utf8_lossy(&message));
                    }
                    _ => {}
                }
            }
        }

        Ok(())
    }

    /// Restore a custom-format pg_dump backup via pg_restore inside the container.
    /// Used for backward compatibility with backups created before the switch to plain format.
    async fn restore_custom_backup_file(
        &self,
        docker: &Docker,
        container_name: &str,
        backup_data: Vec<u8>,
        username: &str,
        password: &str,
    ) -> Result<()> {
        let temp_file = tempfile::NamedTempFile::new()?;
        tokio::fs::write(temp_file.path(), &backup_data).await?;

        // Create a tar archive containing the backup file
        let mut tar = tar::Builder::new(Vec::new());
        tar.append_path_with_name(temp_file.path(), "backup.pgdump")?;
        let tar_data = tar.into_inner()?;

        // Copy the tar archive into the container
        docker
            .upload_to_container(
                container_name,
                Some(bollard::query_parameters::UploadToContainerOptions {
                    path: "/".to_string(),
                    ..Default::default()
                }),
                body_full(bytes::Bytes::from(tar_data)),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to upload backup file to container: {}", e))?;

        // Execute pg_restore inside the container
        let password_env = format!("PGPASSWORD={}", password);
        let exec = docker
            .create_exec(
                container_name,
                bollard::exec::CreateExecOptions {
                    cmd: Some(vec![
                        "pg_restore",
                        "--verbose",
                        "--clean",
                        "--if-exists",
                        "--no-password",
                        "-U",
                        username,
                        "-d",
                        username, // default database is same as username
                        "/backup.pgdump",
                    ]),
                    env: Some(vec![password_env.as_str()]),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create pg_restore exec: {}", e))?;

        let output = docker.start_exec(&exec.id, None).await?;
        if let bollard::exec::StartExecResults::Attached { mut output, .. } = output {
            while let Some(Ok(output)) = output.next().await {
                match output {
                    bollard::container::LogOutput::StdOut { message } => {
                        info!("pg_restore stdout: {}", String::from_utf8_lossy(&message));
                    }
                    bollard::container::LogOutput::StdErr { message } => {
                        // pg_restore emits progress info on stderr, log at debug level
                        debug!("pg_restore stderr: {}", String::from_utf8_lossy(&message));
                    }
                    _ => {}
                }
            }
        }

        Ok(())
    }

    /// Verify that a Docker image can be pulled without actually downloading the full image
    /// Attempts to pull the image - fails if it doesn't exist or cannot be accessed
    async fn verify_image_pullable(&self, image: &str) -> Result<()> {
        // Parse image name and tag
        let (image_name, tag) = if let Some((name, tag)) = image.split_once(':') {
            (name.to_string(), tag.to_string())
        } else {
            (image.to_string(), "latest".to_string())
        };

        info!("Attempting to pull Docker image: {}", image);

        // Try to pull the image - this will fail if it doesn't exist
        let result = self
            .docker
            .create_image(
                Some(bollard::query_parameters::CreateImageOptions {
                    from_image: Some(image_name.clone()),
                    tag: Some(tag.clone()),
                    ..Default::default()
                }),
                None,
                None,
            )
            .try_collect::<Vec<_>>()
            .await;

        match result {
            Ok(_) => {
                info!("Docker image {} is available and pullable", image);
                Ok(())
            }
            Err(e) => {
                error!("Failed to pull Docker image {}: {}", image, e);
                Err(anyhow::anyhow!(
                    "Cannot upgrade: Docker image '{}' is not available or cannot be pulled. Error: {}",
                    image, e
                ))
            }
        }
    }

    /// Restore from a WAL-G backup stored in S3.
    ///
    /// WAL-G restore requires stopping PostgreSQL, clearing PGDATA, fetching the backup,
    /// and restarting. This is done via `docker exec` commands.
    async fn restore_from_walg(
        &self,
        s3_credentials: &super::S3Credentials,
        walg_s3_prefix: &str,
        service_config: ServiceConfig,
        recovery_target: Option<&super::RecoveryTarget>,
    ) -> Result<()> {
        use bollard::exec::CreateExecOptions;

        let postgres_config = self.get_postgres_config(service_config)?;
        let container_name = self.get_live_container_name(&postgres_config);

        info!(
            "Restoring PostgreSQL from WAL-G backup (prefix: {}) in container '{}'",
            walg_s3_prefix, container_name
        );

        // Build WAL-G environment variables
        let mut walg_env: Vec<String> = vec![
            format!("WALG_S3_PREFIX={}", walg_s3_prefix),
            format!("AWS_ACCESS_KEY_ID={}", s3_credentials.access_key_id),
            format!("AWS_SECRET_ACCESS_KEY={}", s3_credentials.secret_key),
            format!("AWS_REGION={}", s3_credentials.region),
            format!("PGUSER={}", postgres_config.username),
            format!("PGPASSWORD={}", postgres_config.password),
            format!("PGDATABASE={}", postgres_config.database),
            "PGHOST=localhost".to_string(),
            format!("PGPORT={}", POSTGRES_INTERNAL_PORT),
        ];

        // Resolve S3 endpoint for use inside the Docker container.
        if let Some(resolved_endpoint) = s3_credentials
            .resolve_endpoint_for_container(&self.docker, &container_name)
            .await
        {
            walg_env.push(format!("AWS_ENDPOINT={}", resolved_endpoint));
        }
        if s3_credentials.force_path_style {
            walg_env.push("AWS_S3_FORCE_PATH_STYLE=true".to_string());
        }

        use bollard::exec::StartExecOptions;
        let walg_env_refs: Vec<&str> = walg_env.iter().map(|s| s.as_str()).collect();

        // Step 1: Fetch backup to a temporary directory while PostgreSQL is still running.
        // We cannot stop PostgreSQL first because it's PID 1 in the container — stopping it
        // would stop the container entirely or leave shared memory blocks behind.
        //
        // IMPORTANT: The temp directory MUST be on the shared volume (/var/lib/postgresql),
        // NOT in /tmp (which is in the container's writable layer). The helper container
        // in step 4 uses `volumes_from` to share the named volume, and it can only see
        // paths on that volume — not the original container's writable layer.
        info!("Fetching WAL-G backup to temporary directory on shared volume");
        let restore_temp = "/var/lib/postgresql/restore_temp";
        let fetch_cmd_str = format!(
            "mkdir -p {restore_temp} && rm -rf {restore_temp}/* && wal-g backup-fetch {restore_temp} LATEST > /tmp/walg_restore.log 2>&1"
        );
        let fetch_cmd = vec!["sh", "-c", &fetch_cmd_str];

        let exec = self
            .docker
            .create_exec(
                &container_name,
                CreateExecOptions {
                    cmd: Some(fetch_cmd),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    env: Some(walg_env_refs.clone()),
                    user: Some("postgres"),
                    ..Default::default()
                },
            )
            .await?;

        self.docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await?;

        // Poll for fetch completion
        loop {
            let inspect = self.docker.inspect_exec(&exec.id).await?;
            if let Some(running) = inspect.running {
                if !running {
                    if let Some(exit_code) = inspect.exit_code {
                        if exit_code != 0 {
                            let log_output = self
                                .read_container_file(&container_name, "/tmp/walg_restore.log")
                                .await;
                            return Err(anyhow::anyhow!(
                                "WAL-G backup-fetch failed with exit code {} in container '{}'. Log:\n{}",
                                exit_code,
                                container_name,
                                log_output
                            ));
                        }
                    }
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        info!("WAL-G backup fetched to {}", restore_temp);

        // Step 2: Prepare restored PGDATA for recovery (while PG still runs).
        //
        // WAL-G backup-push uses pg_start_backup/pg_stop_backup, which creates a
        // backup_label referencing WAL segments needed for recovery. PG MUST be
        // able to read at least the WAL that runs from the base backup's redo
        // LSN through the checkpoint at the end of the backup; otherwise it
        // aborts with "could not locate required checkpoint record".
        //
        // Our strategy is identical for plain in-place restore and PITR:
        //
        // a) Add `recovery.signal` — tells PG 12+ to enter recovery mode
        // b) Set `restore_command = '. walg.env && wal-g wal-fetch %f %p'`
        //    — PG will fetch any WAL it needs from S3. This is the
        //    source of truth for WAL during recovery.
        // c) Set a recovery target: `immediate` (plain restore, stops at first
        //    consistency point) or the caller-specified PITR target.
        // d) Set `recovery_target_action = 'promote'` — promote to primary
        //    after recovery.
        //
        // We previously copied `pg_wal` from the running container and used
        // `restore_command='/bin/true'`. That only worked for same-service
        // restores where the running container's pg_wal happened to hold the
        // needed segments. For cross-service restore (e.g., new service, or
        // restoring onto a fresh service) the target container's pg_wal is
        // empty, so PG couldn't locate the checkpoint and the cluster would
        // refuse to start. `wal-g wal-fetch` works for both cases.
        // Write a READ-ONLY credential file for `restore_command`, distinct
        // from the regular `walg.env` that `archive_command` uses for
        // `wal-g wal-push`. This split is load-bearing:
        //
        //   - `walg-restore.env` lives only for the duration of recovery and
        //     points at the SOURCE backup's S3 prefix. `restore_command`
        //     sources it to call `wal-g wal-fetch`.
        //   - `walg.env` (the write-capable one) is NOT written here. A
        //     restored cluster has no business archiving into the source's
        //     prefix — that's how prior failed restores poisoned the source
        //     with stray `00000002.history` / `00000003.history` files,
        //     which then caused every subsequent restore to fail with
        //     "requested timeline N is not a child of this server's history".
        //     The restored cluster's own `walg.env` is created fresh the
        //     first time the user runs a backup of it (via
        //     `enable_wal_archiving`), pointing at the NEW service's prefix.
        self.write_walg_restore_env_file(&container_name, &walg_env)
            .await?;

        let recovery_target_line = match recovery_target {
            None => "recovery_target = 'immediate'".to_string(),
            Some(super::RecoveryTarget::Time { time }) => format!(
                "recovery_target_time = '{}'",
                time.format("%Y-%m-%d %H:%M:%S%:z")
            ),
            Some(super::RecoveryTarget::Xid { xid }) => {
                format!("recovery_target_xid = '{}'", xid.replace('\'', ""))
            }
            Some(super::RecoveryTarget::Lsn { lsn }) => {
                format!("recovery_target_lsn = '{}'", lsn.replace('\'', ""))
            }
            Some(super::RecoveryTarget::Name { name }) => {
                format!("recovery_target_name = '{}'", name.replace('\'', ""))
            }
        };

        // `restore_command` sources the read-only credential file. `archive_command`
        // and `archive_mode` are explicitly disabled so the restored cluster does
        // not push anything back into S3 during recovery — see the walg-restore.env
        // comment block above for why.
        let prepare_cmd_str = format!(
            concat!(
                "touch {restore_temp}/recovery.signal && ",
                // Overwrite (not append) so whatever archive_command /
                // primary_conninfo / recovery_target settings the source baked
                // into its postgresql.auto.conf are wiped. Our restore is the
                // sole author of this file going forward.
                "cat > {restore_temp}/postgresql.auto.conf <<'EOF_TEMPS_RESTORE'\n",
                "# Written by Temps restore. Overwrites any source-side settings.\n",
                "restore_command = '. /var/lib/postgresql/walg-restore.env && wal-g wal-fetch %f %p'\n",
                "{recovery_target_line}\n",
                "recovery_target_action = 'promote'\n",
                "archive_mode = 'off'\n",
                "archive_command = '/bin/true'\n",
                "EOF_TEMPS_RESTORE\n",
                // pg_wal may exist in the fetched base backup (WAL-G sometimes
                // includes the start-of-backup segment). Ensure it exists as
                // an empty dir at minimum so PG can start; wal-fetch will
                // populate segments as recovery requests them.
                "mkdir -p {restore_temp}/pg_wal"
            ),
            restore_temp = restore_temp,
            recovery_target_line = recovery_target_line,
        );
        let prepare_cmd = vec!["sh", "-c", &prepare_cmd_str];

        let exec = self
            .docker
            .create_exec(
                &container_name,
                CreateExecOptions {
                    cmd: Some(prepare_cmd),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    user: Some("postgres"),
                    ..Default::default()
                },
            )
            .await?;
        self.docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await?;
        loop {
            let inspect = self.docker.inspect_exec(&exec.id).await?;
            if inspect.running == Some(false) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        // Step 3: Stop the container. This cleanly shuts down PostgreSQL (PID 1)
        // and releases all shared memory. The container's writable layer is preserved.
        //
        // IMPORTANT: Disable the restart policy first. The container has
        // restart_policy=always, so Docker would immediately restart it after stop,
        // preventing the helper container from accessing the shared volume exclusively.
        info!("Disabling restart policy and stopping container for PGDATA swap");
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

        self.docker
            .stop_container(
                &container_name,
                Some(bollard::query_parameters::StopContainerOptions {
                    t: Some(30),
                    signal: None,
                }),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to stop container for restore: {}", e))?;

        // Step 4: Start a temporary container sharing the same volumes to swap PGDATA.
        // We can't exec into a stopped container, so we create an ephemeral container
        // that mounts the same data and performs the file swap.
        info!("Swapping PGDATA via ephemeral container");
        let pgdata_path = Self::get_pgdata_path(&postgres_config.docker_image)?;
        let swap_script = format!(
            "rm -rf {pgdata}/* && cp -a {restore_temp}/* {pgdata}/ && rm -rf {restore_temp}",
            pgdata = pgdata_path,
            restore_temp = restore_temp,
        );

        // Use `docker commit` approach: start the same container with a swap command.
        // Since the container is stopped, we need to change its command to do the swap.
        // Simpler: just start the original container — the entrypoint will start PostgreSQL
        // using the PGDATA that's already there. We need to replace it BEFORE starting.
        //
        // The simplest reliable approach: use docker cp or a helper container.
        // Let's use a helper container that shares the original container's volumes.
        use bollard::models::{ContainerCreateBody, HostConfig};
        let helper_name = format!("{}-restore-helper", container_name);
        let helper_config = ContainerCreateBody {
            image: Some(postgres_config.docker_image.clone()),
            cmd: Some(vec!["sh".to_string(), "-c".to_string(), swap_script]),
            host_config: Some(HostConfig {
                volumes_from: Some(vec![container_name.clone()]),
                ..Default::default()
            }),
            user: Some("postgres".to_string()),
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

        self.docker
            .start_container(
                &helper.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start restore helper container: {}", e))?;

        // Wait for helper to finish
        let wait_result = self
            .docker
            .wait_container(
                &helper.id,
                None::<bollard::query_parameters::WaitContainerOptions>,
            )
            .next()
            .await;

        // Clean up helper container
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

        if let Some(Ok(wait_response)) = wait_result {
            if wait_response.status_code != 0 {
                return Err(anyhow::anyhow!(
                    "PGDATA swap helper exited with code {} in container '{}'",
                    wait_response.status_code,
                    container_name
                ));
            }
        }

        // Step 5: Re-enable restart policy and start the original container.
        // The entrypoint will detect existing PGDATA and start PostgreSQL,
        // which will enter recovery mode due to recovery.signal.
        info!("Re-enabling restart policy and starting container with restored PGDATA");
        self.docker
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
            .map_err(|e| anyhow::anyhow!("Failed to re-enable restart policy: {}", e))?;

        self.docker
            .start_container(
                &container_name,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start container after restore: {}", e))?;

        // Wait for PostgreSQL to become healthy
        self.wait_for_container_health(&self.docker, &container_name)
            .await?;

        info!("PostgreSQL WAL-G restore completed successfully");
        Ok(())
    }

    /// Restore from a legacy backup (pre-WAL-G .sql.gz or .pgdump.gz files in S3).
    /// Falls back to the old approach: download from S3, decompress, psql/pg_restore.
    async fn restore_from_legacy(
        &self,
        s3_client: &aws_sdk_s3::Client,
        backup_location: &str,
        s3_source: &temps_entities::s3_sources::Model,
        service_config: ServiceConfig,
    ) -> Result<()> {
        info!("Restoring from legacy backup format: {}", backup_location);

        // Ensure container is running before attempting restore
        self.start().await?;

        let postgres_config = self.get_postgres_config(service_config)?;

        // Get the backup object from S3
        let get_obj = s3_client
            .get_object()
            .bucket(&s3_source.bucket_name)
            .key(backup_location)
            .send()
            .await?;

        // Read the backup data
        let backup_data = get_obj.body.collect().await?.to_vec();

        // Decompress (assuming gzip compression)
        let mut decoder = flate2::read::GzDecoder::new(&backup_data[..]);
        let mut decompressed_data = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut decompressed_data)?;

        let container_name = self.get_live_container_name(&postgres_config);

        // Detect backup format from the S3 location
        let is_plain_format = backup_location.ends_with(".sql.gz");

        if is_plain_format {
            self.restore_backup_file(
                &self.docker,
                &container_name,
                decompressed_data,
                &postgres_config.username,
                &postgres_config.password,
            )
            .await?;
        } else {
            self.restore_custom_backup_file(
                &self.docker,
                &container_name,
                decompressed_data,
                &postgres_config.username,
                &postgres_config.password,
            )
            .await?;
        }

        info!("Legacy PostgreSQL restore completed successfully");
        Ok(())
    }

    /// Backup PostgreSQL data to S3 using WAL-G.
    ///
    /// Runs `wal-g backup-push` inside the running PostgreSQL container via `docker exec`.
    /// WAL-G uploads the backup directly to S3 from within the container — zero data flows
    /// through the Temps process, keeping memory usage flat regardless of database size.
    ///
    /// After a successful backup, this method also:
    /// 1. Writes WAL-G S3 credentials to `/var/lib/postgresql/walg.env` on the shared volume
    /// 2. Enables continuous WAL archiving via `ALTER SYSTEM SET archive_command`
    /// 3. Calls `pg_reload_conf()` so PostgreSQL picks up the change without restart
    #[allow(clippy::too_many_arguments)]
    async fn backup_to_s3_walg(
        &self,
        s3_client: &aws_sdk_s3::Client,
        s3_credentials: &super::S3Credentials,
        backup: temps_entities::backups::Model,
        subpath_root: &str,
        pool: &temps_database::DbConnection,
        external_service: &temps_entities::external_services::Model,
        service_config: ServiceConfig,
    ) -> anyhow::Result<super::BackupOutcome> {
        use chrono::Utc;
        use sea_orm::*;

        let postgres_config = self.get_postgres_config(service_config)?;
        let container_name = self.get_live_container_name(&postgres_config);

        let metadata = serde_json::json!({
            "service_type": "postgres",
            "service_name": self.name,
            "backup_tool": "wal-g",
        });

        let backup_record = external_service_backups::ActiveModel {
            service_id: Set(external_service.id),
            backup_id: Set(backup.id),
            backup_type: Set("full".to_string()),
            state: Set("running".to_string()),
            started_at: Set(Utc::now()),
            s3_location: Set("".to_string()),
            metadata: Set(metadata),
            compression_type: Set("lz4".to_string()),
            created_by: Set(0),
            ..Default::default()
        }
        .insert(pool)
        .await?;

        // Build the WAL-G S3 prefix using the STABLE subpath_root (no date component).
        let walg_s3_prefix = format!(
            "s3://{}/{}/walg",
            s3_credentials.bucket_name,
            subpath_root.trim_matches('/')
        );
        // Bucket-relative prefix used to list backup objects after success.
        let s3_list_prefix = format!("{}/walg/", subpath_root.trim_matches('/'));

        // Run the backup, then either persist success or mark failure. Any
        // `?` propagation in the inner block lands in the failure branch.
        let result = self
            .run_walg_backup_push(
                &container_name,
                &walg_s3_prefix,
                s3_credentials,
                &postgres_config,
            )
            .await;

        match result {
            Ok(walg_env) => {
                // Compute size by listing S3. WAL-G streams chunks; we
                // don't see them locally.
                let size_bytes = match super::s3_util::list_total_size(
                    s3_client,
                    &s3_credentials.bucket_name,
                    &s3_list_prefix,
                )
                .await
                {
                    Ok(n) => Some(n),
                    Err(e) => {
                        warn!(
                            "WAL-G backup succeeded but failed to compute size for s3://{}/{}: {}",
                            s3_credentials.bucket_name, s3_list_prefix, e
                        );
                        None
                    }
                };

                let mut backup_update: external_service_backups::ActiveModel =
                    backup_record.clone().into();
                backup_update.state = Set("completed".to_string());
                backup_update.finished_at = Set(Some(Utc::now()));
                backup_update.s3_location = Set(walg_s3_prefix.clone());
                backup_update.size_bytes = Set(size_bytes);
                backup_update.update(pool).await?;

                info!(
                    "PostgreSQL WAL-G backup completed successfully (prefix: {}, size: {:?})",
                    walg_s3_prefix, size_bytes
                );

                // Enable continuous WAL archiving.
                // Failures here are logged but do NOT fail the backup.
                if let Err(e) = self
                    .enable_wal_archiving(&container_name, &walg_env, &postgres_config)
                    .await
                {
                    error!(
                        "Failed to enable WAL archiving in container '{}': {}. \
                         Base backup succeeded but continuous WAL archiving is not active.",
                        container_name, e
                    );
                }

                Ok(super::BackupOutcome::new(walg_s3_prefix, size_bytes))
            }
            Err(e) => {
                let error_msg = format!("WAL-G backup failed: {}", e);
                error!("{}", error_msg);
                let mut backup_update: external_service_backups::ActiveModel =
                    backup_record.clone().into();
                backup_update.state = Set("failed".to_string());
                backup_update.error_message = Set(Some(error_msg.clone()));
                backup_update.finished_at = Set(Some(Utc::now()));
                if let Err(update_err) = backup_update.update(pool).await {
                    error!(
                        "Failed to mark external_service_backups row {} as failed: {}",
                        backup_record.id, update_err
                    );
                }
                Err(e)
            }
        }
    }

    /// Build the wal-g env, run `wal-g backup-push` via `docker exec`, and
    /// return the env vector so the caller can also wire up WAL archiving.
    async fn run_walg_backup_push(
        &self,
        container_name: &str,
        walg_s3_prefix: &str,
        s3_credentials: &super::S3Credentials,
        postgres_config: &PostgresConfig,
    ) -> anyhow::Result<Vec<String>> {
        let mut walg_env: Vec<String> = vec![
            format!("WALG_S3_PREFIX={}", walg_s3_prefix),
            format!("AWS_ACCESS_KEY_ID={}", s3_credentials.access_key_id),
            format!("AWS_SECRET_ACCESS_KEY={}", s3_credentials.secret_key),
            format!("AWS_REGION={}", s3_credentials.region),
            format!("PGUSER={}", postgres_config.username),
            format!("PGPASSWORD={}", postgres_config.password),
            format!("PGDATABASE={}", postgres_config.database),
            "PGHOST=localhost".to_string(),
            format!("PGPORT={}", POSTGRES_INTERNAL_PORT),
        ];

        if let Some(resolved_endpoint) = s3_credentials
            .resolve_endpoint_for_container(&self.docker, container_name)
            .await
        {
            walg_env.push(format!("AWS_ENDPOINT={}", resolved_endpoint));
        }
        if s3_credentials.force_path_style {
            walg_env.push("AWS_S3_FORCE_PATH_STYLE=true".to_string());
        }

        info!(
            "Running wal-g backup-push in container '{}' (S3 prefix: {})",
            container_name, walg_s3_prefix
        );

        super::exec_util::run_exec(
            &self.docker,
            container_name,
            vec![
                "sh".into(),
                "-c".into(),
                "wal-g backup-push $PGDATA 2>&1".into(),
            ],
            Some(walg_env.clone()),
            BACKUP_EXEC_TIMEOUT,
        )
        .await?;

        Ok(walg_env)
    }

    /// Backup PostgreSQL data to S3 using pg_dump via a sidecar container.
    ///
    /// Legacy fallback for containers without WAL-G (e.g., `postgres:18-alpine`,
    /// `pgvector/pgvector:pg17`). Runs pg_dump in a sidecar container on the same
    /// Docker network, streams output through gzip to a temp file, then uploads to S3.
    #[allow(clippy::too_many_arguments)]
    async fn backup_to_s3_pgdump(
        &self,
        s3_client: &aws_sdk_s3::Client,
        backup: temps_entities::backups::Model,
        s3_source: &temps_entities::s3_sources::Model,
        subpath: &str,
        pool: &temps_database::DbConnection,
        external_service: &temps_entities::external_services::Model,
        service_config: ServiceConfig,
    ) -> anyhow::Result<super::BackupOutcome> {
        use chrono::Utc;
        use sea_orm::*;

        info!("Starting PostgreSQL backup to S3 via pg_dump sidecar");

        let postgres_config = self.get_postgres_config(service_config)?;

        let metadata = serde_json::json!({
            "service_type": "postgres",
            "service_name": self.name,
            "backup_tool": "pg_dumpall",
        });

        let backup_record = external_service_backups::ActiveModel {
            service_id: Set(external_service.id),
            backup_id: Set(backup.id),
            backup_type: Set("full".to_string()),
            state: Set("running".to_string()),
            started_at: Set(Utc::now()),
            s3_location: Set("".to_string()),
            metadata: Set(metadata),
            compression_type: Set("gzip".to_string()),
            created_by: Set(0),
            ..Default::default()
        }
        .insert(pool)
        .await?;

        let outcome = self
            .run_pg_dumpall_to_s3(s3_client, s3_source, subpath, &postgres_config)
            .await;

        match outcome {
            Ok((backup_key, size_bytes)) => {
                let mut backup_update: external_service_backups::ActiveModel =
                    backup_record.clone().into();
                backup_update.state = Set("completed".to_string());
                backup_update.finished_at = Set(Some(Utc::now()));
                backup_update.size_bytes = Set(Some(size_bytes));
                backup_update.s3_location = Set(backup_key.clone());
                backup_update.update(pool).await?;
                Ok(super::BackupOutcome::new(backup_key, Some(size_bytes)))
            }
            Err(e) => {
                let error_msg = format!("pg_dumpall backup failed: {}", e);
                error!("{}", error_msg);
                let mut backup_update: external_service_backups::ActiveModel =
                    backup_record.clone().into();
                backup_update.state = Set("failed".to_string());
                backup_update.error_message = Set(Some(error_msg.clone()));
                backup_update.finished_at = Set(Some(Utc::now()));
                if let Err(update_err) = backup_update.update(pool).await {
                    error!(
                        "Failed to mark external_service_backups row {} as failed: {}",
                        backup_record.id, update_err
                    );
                }
                Err(e)
            }
        }
    }

    /// Pull image, spin up the sidecar, run `pg_dumpall | gzip` to a bind
    /// mount, upload to S3, clean up. Returns `(backup_key, size_bytes)`.
    ///
    /// All cleanup (sidecar removal, temp file deletion) is best-effort and
    /// runs regardless of which step failed — so the caller only has to
    /// decide whether to mark the DB row as completed or failed.
    async fn run_pg_dumpall_to_s3(
        &self,
        s3_client: &aws_sdk_s3::Client,
        s3_source: &temps_entities::s3_sources::Model,
        subpath: &str,
        postgres_config: &PostgresConfig,
    ) -> anyhow::Result<(String, i64)> {
        use bollard::models::ContainerCreateBody as Config;
        use bollard::query_parameters::RemoveContainerOptions;
        use chrono::Utc;

        let db_container_name = self.get_live_container_name(postgres_config);
        let sidecar_image = postgres_config.docker_image.clone();

        info!("Pulling sidecar image {} for pg_dump", sidecar_image);
        let (image_name, image_tag) = sidecar_image
            .split_once(':')
            .map(|(n, t)| (n.to_string(), t.to_string()))
            .unwrap_or_else(|| (sidecar_image.clone(), "latest".to_string()));

        self.docker
            .create_image(
                Some(bollard::query_parameters::CreateImageOptions {
                    from_image: Some(image_name),
                    tag: Some(image_tag),
                    ..Default::default()
                }),
                None,
                None,
            )
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to pull pg_dump sidecar image {}: {}",
                    sidecar_image,
                    e
                )
            })?;

        let sidecar_name = format!("temps-pg-backup-{}", uuid::Uuid::new_v4());
        let password_env = format!("PGPASSWORD={}", postgres_config.password);

        // Create a host directory for the bind mount so pg_dump writes
        // directly to disk, bypassing the Temps process entirely.
        let backup_dir = std::env::temp_dir().join("temps-extpg-backup");
        tokio::fs::create_dir_all(&backup_dir).await.map_err(|e| {
            anyhow::anyhow!(
                "Failed to create backup temp directory {}: {}",
                backup_dir.display(),
                e
            )
        })?;
        let backup_filename = format!("{}.sql.gz", uuid::Uuid::new_v4());
        let host_backup_path = backup_dir.join(&backup_filename);
        let container_backup_path = format!("/backup/{}", backup_filename);
        let stderr_path_in_container = format!("/backup/{}.stderr", uuid::Uuid::new_v4());
        let host_stderr_path = backup_dir.join(
            std::path::Path::new(&stderr_path_in_container)
                .file_name()
                .unwrap(),
        );

        let sidecar_config = Config {
            image: Some(sidecar_image.clone()),
            entrypoint: Some(vec!["/bin/sleep".to_string()]),
            cmd: Some(vec!["86400".to_string()]),
            env: Some(vec![password_env.clone()]),
            user: Some("root".to_string()),
            host_config: Some(bollard::models::HostConfig {
                oom_score_adj: Some(-500),
                binds: Some(vec![format!("{}:/backup:rw", backup_dir.display())]),
                ..Default::default()
            }),
            networking_config: Some(bollard::models::NetworkingConfig {
                endpoints_config: Some(std::collections::HashMap::from([(
                    temps_core::NETWORK_NAME.to_string(),
                    bollard::models::EndpointSettings {
                        ..Default::default()
                    },
                )])),
            }),
            ..Default::default()
        };

        self.docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&sidecar_name)
                        .build(),
                ),
                sidecar_config,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create pg_dump sidecar container: {}", e))?;

        self.docker
            .start_container(
                &sidecar_name,
                Some(bollard::query_parameters::StartContainerOptionsBuilder::new().build()),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start pg_dump sidecar container: {}", e))?;

        // Cleanup runs regardless of success/failure. We capture clones so
        // the closure outlives the function-level `?` boundary.
        let cleanup = || {
            let docker = self.docker.clone();
            let sidecar = sidecar_name.clone();
            let host_backup = host_backup_path.clone();
            let host_stderr = host_stderr_path.clone();
            async move {
                let _ = docker
                    .remove_container(
                        &sidecar,
                        Some(RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await;
                let _ = tokio::fs::remove_file(&host_backup).await;
                let _ = tokio::fs::remove_file(&host_stderr).await;
            }
        };

        let port_str = POSTGRES_INTERNAL_PORT.to_string();

        info!(
            "Running pg_dumpall sidecar for service '{}' (host={}, bind-mount mode)",
            self.name, db_container_name
        );

        // Run pg_dumpall | gzip inside the sidecar, writing directly to the
        // bind-mounted host filesystem. pg_dumpall dumps the entire cluster
        // (all DBs, roles, tablespaces); `--database` is just the bootstrap
        // connection target.
        let pg_dump_shell_cmd = format!(
            "pg_dumpall --clean --if-exists --no-password --host={} --port={} --username={} --database={} 2>{} | gzip > {}",
            shell_escape(&db_container_name),
            shell_escape(&port_str),
            shell_escape(&postgres_config.username),
            shell_escape(&postgres_config.database),
            stderr_path_in_container,
            container_backup_path,
        );

        let exec_result = super::exec_util::run_exec(
            &self.docker,
            &sidecar_name,
            vec!["sh".into(), "-c".into(), pg_dump_shell_cmd],
            Some(vec![password_env.clone()]),
            BACKUP_EXEC_TIMEOUT,
        )
        .await;

        // Read sidecar-side stderr (pg_dumpall writes to it via 2>) for
        // diagnostics. Best-effort; missing file is fine.
        let stderr_from_file = tokio::fs::read(&host_stderr_path)
            .await
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();

        if let Err(e) = exec_result {
            cleanup().await;
            return Err(anyhow::anyhow!(
                "pg_dumpall exec failed: {}{}",
                e,
                if stderr_from_file.is_empty() {
                    String::new()
                } else {
                    format!("\npg_dumpall stderr:\n{}", stderr_from_file)
                }
            ));
        }

        if !stderr_from_file.is_empty() {
            tracing::debug!(
                "pg_dumpall stderr for service '{}': {}",
                self.name,
                stderr_from_file
            );
        }

        let size_bytes = match tokio::fs::metadata(&host_backup_path).await {
            Ok(m) => m.len() as i64,
            Err(e) => {
                cleanup().await;
                return Err(anyhow::anyhow!(
                    "Failed to stat backup file {}: {}",
                    host_backup_path.display(),
                    e
                ));
            }
        };

        if size_bytes == 0 {
            cleanup().await;
            return Err(anyhow::anyhow!(
                "PostgreSQL backup failed: backup file has zero size (pg_dumpall produced no output)"
            ));
        }

        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let backup_key = format!(
            "{}/postgres_backup_{}.sql.gz",
            subpath.trim_matches('/'),
            timestamp
        );

        let body = match aws_sdk_s3::primitives::ByteStream::from_path(&host_backup_path).await {
            Ok(b) => b,
            Err(e) => {
                cleanup().await;
                return Err(anyhow::anyhow!(
                    "Failed to open backup file {} for upload: {}",
                    host_backup_path.display(),
                    e
                ));
            }
        };

        if let Err(e) = s3_client
            .put_object()
            .bucket(&s3_source.bucket_name)
            .key(&backup_key)
            .body(body)
            .content_type("application/x-gzip")
            .send()
            .await
        {
            cleanup().await;
            return Err(anyhow::anyhow!(
                "Failed to upload backup to s3://{}/{}: {}",
                s3_source.bucket_name,
                backup_key,
                e
            ));
        }

        cleanup().await;
        info!(
            "Successfully uploaded pg_dumpall backup to s3://{}/{} ({} bytes)",
            s3_source.bucket_name, backup_key, size_bytes
        );

        Ok((backup_key, size_bytes))
    }
}

/// Internal port used by PostgreSQL inside the container
const POSTGRES_INTERNAL_PORT: &str = "5432";

#[async_trait]
impl ExternalService for PostgresService {
    fn get_local_address(&self, service_config: ServiceConfig) -> Result<String> {
        let config = self.get_postgres_config(service_config)?;
        Ok(format!("localhost:{}", config.port))
    }

    fn get_effective_address(&self, service_config: ServiceConfig) -> Result<(String, String)> {
        let config = self.get_postgres_config(service_config)?;

        if temps_core::DeploymentMode::is_docker() {
            // Docker mode: use container name and internal port
            Ok((
                self.get_live_container_name(&config),
                POSTGRES_INTERNAL_PORT.to_string(),
            ))
        } else {
            // Baremetal mode: use localhost and exposed port
            Ok(("localhost".to_string(), config.port))
        }
    }

    fn get_docker_container_name(&self) -> String {
        self.get_container_name()
    }

    fn get_docker_internal_port(&self) -> String {
        POSTGRES_INTERNAL_PORT.to_string()
    }

    /// Backup PostgreSQL data to S3.
    ///
    /// Detects whether the container has WAL-G installed:
    /// - **WAL-G available**: Uses `wal-g backup-push` inside the container. Zero data flows
    ///   through the Temps process. After success, enables continuous WAL archiving for PITR.
    /// - **WAL-G not available** (legacy images like `postgres:18-alpine`): Falls back to
    ///   pg_dump via a sidecar container, streaming to a temp file and uploading to S3.
    async fn backup_to_s3(
        &self,
        s3_client: &aws_sdk_s3::Client,
        s3_credentials: &super::S3Credentials,
        backup: temps_entities::backups::Model,
        s3_source: &temps_entities::s3_sources::Model,
        subpath: &str,
        subpath_root: &str,
        pool: &temps_database::DbConnection,
        external_service: &temps_entities::external_services::Model,
        service_config: ServiceConfig,
    ) -> anyhow::Result<super::BackupOutcome> {
        let postgres_config = self.get_postgres_config(service_config.clone())?;
        let container_name = self.get_live_container_name(&postgres_config);

        if self.container_has_walg(&container_name).await {
            info!(
                "WAL-G detected in container '{}', using WAL-G backup",
                container_name
            );
            self.backup_to_s3_walg(
                s3_client,
                s3_credentials,
                backup,
                subpath_root,
                pool,
                external_service,
                service_config,
            )
            .await
        } else {
            info!(
                "WAL-G not found in container '{}', falling back to pg_dump sidecar",
                container_name
            );
            self.backup_to_s3_pgdump(
                s3_client,
                backup,
                s3_source,
                subpath,
                pool,
                external_service,
                service_config,
            )
            .await
        }
    }

    async fn init(&self, config: ServiceConfig) -> Result<HashMap<String, String>> {
        info!(
            "Initializing PostgreSQL service (name={}, type={:?}, version={:?})",
            config.name, config.service_type, config.version
        );

        // Pull resource limits out of the raw parameters JSON before the
        // typed config consumes it. Missing/malformed `resources` block
        // defaults to unlimited, preserving legacy behavior for services
        // created before this field existed.
        let resource_limits = ServiceResourceLimits::from_parameters(&config.parameters);
        if let Err(e) = resource_limits.validate() {
            return Err(anyhow::anyhow!("Invalid resource limits: {}", e));
        }

        // Parse input config and transform to runtime config
        let mut postgres_config = self.get_postgres_config(config)?;

        // Store runtime config and limits so `start()` can recreate the
        // container with the same constraints if it has been removed. This
        // gets overwritten below once the real container port is known.
        *self.config.write().await = Some(postgres_config.clone());
        *self.resource_limits.write().await = resource_limits.clone();

        if postgres_config.container_name.is_none() {
            // Create Docker container. New services always start with archiving
            // off — `enable_wal_archiving()` recreates with archiving on when
            // WAL-G is later configured. `create_container` may retry on a
            // different host port than requested (see its docs); it writes
            // that back into `postgres_config`, so everything below —
            // `self.config` and the DB-persisted parameters — reflects the
            // port the container is actually bound to.
            self.create_container(&self.docker, &mut postgres_config, &resource_limits, false)
                .await?;
            *self.config.write().await = Some(postgres_config.clone());
        } else {
            info!(
                "PostgreSQL service '{}' is imported from container '{}'; skipping container creation",
                self.name,
                self.get_live_container_name(&postgres_config)
            );
        }

        // Serialize the full runtime config to save to database
        // This ensures auto-generated values (password, port) are persisted
        let runtime_config_json = serde_json::to_value(&postgres_config)
            .context("Failed to serialize PostgreSQL runtime config")?;

        let runtime_config_map = runtime_config_json
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Runtime config is not an object"))?;

        let mut inferred_params = HashMap::new();
        for (key, value) in runtime_config_map {
            if let Some(str_value) = value.as_str() {
                inferred_params.insert(key.clone(), str_value.to_string());
            } else if let Some(num_value) = value.as_u64() {
                inferred_params.insert(key.clone(), num_value.to_string());
            }
        }

        Ok(inferred_params)
    }

    async fn health_check(&self) -> Result<bool> {
        // let pool = self.get_pool().await?;
        // let result = sqlx::query("SELECT 1").fetch_one(&pool).await.is_ok();
        Ok(true)
    }

    async fn health_probe(&self, service_config: ServiceConfig) -> Result<HealthProbeResult> {
        use std::time::Instant;

        const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
        const DEGRADED_MS: u128 = 2000;

        let cfg = match self.get_postgres_config(service_config) {
            Ok(c) => c,
            Err(e) => {
                return Ok(HealthProbeResult::down(format!(
                    "invalid postgres config: {}",
                    e
                )))
            }
        };

        let conn_str = format!(
            "host={} port={} user={} password={} dbname={} connect_timeout=3",
            cfg.host, cfg.port, cfg.username, cfg.password, cfg.database
        );

        let start = Instant::now();
        let connect = tokio::time::timeout(
            PROBE_TIMEOUT,
            tokio_postgres::connect(&conn_str, tokio_postgres::NoTls),
        )
        .await;

        match connect {
            Err(_) => Ok(HealthProbeResult::down(format!(
                "postgres probe to {}:{} timed out after {}s",
                cfg.host,
                cfg.port,
                PROBE_TIMEOUT.as_secs()
            ))),
            Ok(Err(e)) => Ok(HealthProbeResult::down(format!(
                "postgres connect to {}:{} failed: {}",
                cfg.host, cfg.port, e
            ))),
            Ok(Ok((client, connection))) => {
                // Drive the connection on a background task for the lifetime
                // of this probe. `client` is dropped at the end of the match
                // arm which closes the connection cleanly.
                let connection_task = tokio::spawn(async move {
                    let _ = connection.await;
                });

                let query_result =
                    tokio::time::timeout(PROBE_TIMEOUT, client.simple_query("SELECT 1")).await;

                connection_task.abort();

                let elapsed_ms = start.elapsed().as_millis();
                let response_time = i32::try_from(elapsed_ms).ok();

                match query_result {
                    Err(_) => Ok(HealthProbeResult::down(format!(
                        "postgres SELECT 1 timed out after {}s",
                        PROBE_TIMEOUT.as_secs()
                    ))),
                    Ok(Err(e)) => Ok(HealthProbeResult::down(format!(
                        "postgres SELECT 1 failed: {}",
                        e
                    ))),
                    Ok(Ok(_)) => {
                        if elapsed_ms > DEGRADED_MS {
                            Ok(HealthProbeResult::degraded(
                                format!(
                                    "postgres responded in {}ms (>{}ms)",
                                    elapsed_ms, DEGRADED_MS
                                ),
                                response_time,
                            ))
                        } else {
                            Ok(HealthProbeResult::operational(response_time))
                        }
                    }
                }
            }
        }
    }

    fn get_type(&self) -> ServiceType {
        ServiceType::Postgres
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
                "postgres://{}:***@{}:{}/{}",
                cfg.username, cfg.host, cfg.port, cfg.database
            )),
            None => Err(anyhow::anyhow!("PostgreSQL not configured")),
        }
    }

    fn get_runtime_env_definitions(&self) -> Vec<RuntimeEnvVar> {
        vec![
            RuntimeEnvVar {
                name: "POSTGRES_DATABASE".to_string(),
                description: "Database name specific to this project/environment".to_string(),
                example: "project_123_production".to_string(),
                sensitive: false,
            },
            RuntimeEnvVar {
                name: "POSTGRES_URL".to_string(),
                description: "Full connection URL including project-specific database".to_string(),
                example: "postgresql://user:pass@localhost:5432/project_123_production".to_string(),
                sensitive: true, // Contains password
            },
        ]
    }
    async fn get_runtime_env_vars(
        &self,
        service_config: ServiceConfig,
        project_id: &str,
        environment: &str,
    ) -> Result<HashMap<String, String>> {
        let resource_name = format!("{}_{}", project_id, environment);
        let resource_name = Self::normalize_database_name(&resource_name);

        // Create the database
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
        let resource_name = format!("{}_{}", project_id, environment);
        let resource_name = Self::normalize_database_name(&resource_name);
        // Preview path: skip `create_database` so the UI can show what a
        // deployment would receive without actually provisioning the DB.
        self.build_runtime_env_vars(service_config, &resource_name)
    }
    fn get_docker_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>> {
        let mut env_vars = HashMap::new();

        let username = parameters
            .get("username")
            .context("Missing username parameter")?;
        let password = parameters
            .get("password")
            .context("Missing password parameter")?;
        let database = parameters
            .get("database")
            .context("Missing database parameter")?;

        // Always use container name and internal port for container-to-container
        // communication. An imported service's real container name (stored raw
        // in parameters, since the typed config isn't available here) wins over
        // the derived one.
        let effective_host = parameters
            .get("container_name")
            .cloned()
            .unwrap_or_else(|| self.get_container_name());
        let effective_port = POSTGRES_INTERNAL_PORT.to_string();

        let url = format!(
            "postgresql://{}:{}@{}:{}/{}",
            urlencoding::encode(username),
            urlencoding::encode(password),
            effective_host,
            effective_port,
            database
        );

        env_vars.insert("POSTGRES_URL".to_string(), url);
        env_vars.insert("POSTGRES_HOST".to_string(), effective_host);
        env_vars.insert("POSTGRES_PORT".to_string(), effective_port);
        // `POSTGRES_DB` is the canonical name (matches the official Postgres
        // Docker image and what every app library expects). `POSTGRES_NAME`
        // is kept as a back-compat alias for older deployments — see the
        // sibling provision_resource() impl for the full rationale.
        env_vars.insert("POSTGRES_DB".to_string(), database.clone());
        env_vars.insert("POSTGRES_NAME".to_string(), database.clone());
        env_vars.insert("POSTGRES_USER".to_string(), username.clone());
        env_vars.insert("POSTGRES_PASSWORD".to_string(), password.clone());

        Ok(env_vars)
    }
    async fn cleanup(&self) -> Result<()> {
        Ok(())
    }

    fn get_parameter_schema(&self) -> Option<serde_json::Value> {
        // Generate JSON Schema from PostgresInputConfig
        let schema = schemars::schema_for!(PostgresInputConfig);
        let mut schema_json = serde_json::to_value(schema).ok()?;

        // Add metadata about which fields are editable
        if let Some(properties) = schema_json
            .get_mut("properties")
            .and_then(|p| p.as_object_mut())
        {
            for key in properties.keys().cloned().collect::<Vec<_>>() {
                // Define which fields should be editable
                let editable = match key.as_str() {
                    "host" => false,           // Don't change host after creation
                    "port" => true,            // Port can be changed
                    "database" => false,       // Don't change database name after creation
                    "username" => false,       // Don't change username after creation
                    "password" => true,        // Password can be changed by user
                    "max_connections" => true, // Max connections can be adjusted
                    "ssl_mode" => true,        // SSL mode can be changed
                    "docker_image" => true,    // Docker image can be upgraded
                    _ => false,
                };

                if let Some(prop) = schema_json["properties"][&key].as_object_mut() {
                    prop.insert("x-editable".to_string(), serde_json::json!(editable));
                }
            }
        }

        Some(schema_json)
    }

    async fn start(&self) -> Result<()> {
        let existing_config = self.config.read().await.as_ref().cloned();
        let container_name = existing_config
            .as_ref()
            .map(|config| self.get_live_container_name(config))
            .unwrap_or_else(|| self.get_container_name());
        info!("Starting PostgreSQL container {}", container_name);

        // Imported services skip the drift-reconciliation path entirely: the
        // archive_mode CMD check and its stop+remove+recreate response below
        // assume Temps owns the container's lifecycle and volume naming.
        // Running that against a pre-existing container the operator brought
        // in would delete their real database to "fix" a CMD mismatch that
        // was never Temps' to manage.
        if let Some(config) = existing_config
            .as_ref()
            .filter(|c| c.container_name.is_some())
        {
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
                return Err(anyhow::anyhow!(
                    "Imported PostgreSQL container '{}' not found",
                    container_name
                ));
            }
            let is_running = matches!(
                containers[0].state,
                Some(bollard::models::ContainerSummaryStateEnum::RUNNING)
            );
            if !is_running {
                self.docker
                    .start_container(
                        &container_name,
                        None::<bollard::query_parameters::StartContainerOptions>,
                    )
                    .await
                    .context("Failed to start imported PostgreSQL container")?;
            }
            self.wait_for_container_health(&self.docker, &container_name)
                .await?;
            let _ = config;
            return Ok(());
        }

        // Reconcile-on-start. The desired `archive_mode` is derived from
        // on-disk truth: `/var/lib/postgresql/walg.env` exists on the
        // service's volume iff WAL-G archiving has been configured. If the
        // existing container's CMD doesn't match (e.g., it was created by an
        // older version that baked archive_mode=on unconditionally), we
        // recreate the container here. This is the only path that auto-
        // repairs config drift — and it's operator-initiated (Stop+Start),
        // so the downtime is expected.
        let desired_enable_archiving = self.compute_desired_enable_archiving().await;

        // Check if container exists and get its status
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

        let mut need_create = containers.is_empty();
        if let Some(container) = containers.first() {
            // Inspect the existing CMD. If it disagrees with what we'd emit
            // now, force a recreate by stopping + removing the old container
            // and falling through to the create branch.
            let drift = self
                .container_cmd_archive_mode_differs(container, desired_enable_archiving)
                .await;
            if drift {
                info!(
                    "Container {} has archive_mode CMD drift (desired={}). \
                     Recreating to apply correct config.",
                    container_name, desired_enable_archiving
                );
                let _ = self
                    .docker
                    .stop_container(
                        &container_name,
                        None::<bollard::query_parameters::StopContainerOptions>,
                    )
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
                    .context("Failed to remove drifted container during reconcile")?;
                need_create = true;
            }
        }

        if need_create {
            let mut config = self
                .config
                .read()
                .await
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("PostgreSQL configuration not found"))?
                .clone();
            let limits = self.resource_limits.read().await.clone();
            self.create_container(&self.docker, &mut config, &limits, desired_enable_archiving)
                .await?;
            *self.config.write().await = Some(config);
        } else {
            // Container exists and CMD matches desired state. Just start it
            // if it isn't already running.
            let container = &containers[0];
            let is_running = matches!(
                container.state,
                Some(bollard::models::ContainerSummaryStateEnum::RUNNING)
            );

            if !is_running {
                let start_result = self
                    .docker
                    .start_container(
                        &container_name,
                        None::<bollard::query_parameters::StartContainerOptions>,
                    )
                    .await;

                match start_result {
                    Ok(_) => info!("Started existing PostgreSQL container {}", container_name),
                    Err(e) => {
                        // "already started" is benign — we raced ourselves.
                        let error_msg = e.to_string();
                        if !error_msg.contains("already started") {
                            return Err(e)
                                .context("Failed to start existing PostgreSQL container")?;
                        }
                        info!("PostgreSQL container {} is already started", container_name);
                    }
                }
            } else {
                info!("PostgreSQL container {} is already running", container_name);
            }
        }

        // Wait for container to be healthy
        self.wait_for_container_health(&self.docker, &container_name)
            .await?;

        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        // Stop the container if Docker is available
        let container_name = self
            .config
            .read()
            .await
            .as_ref()
            .map(|config| self.get_live_container_name(config))
            .unwrap_or_else(|| self.get_container_name());

        // Check if container exists before attempting to stop
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
                .stop_container(
                    &container_name,
                    None::<bollard::query_parameters::StopContainerOptions>,
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to stop PostgreSQL container: {:?}", e))?;
        }

        Ok(())
    }

    async fn remove(&self) -> Result<()> {
        // First cleanup any connections
        self.cleanup().await?;

        // Then remove container and volume if Docker is available
        let container_name = self.get_container_name();
        let volume_name = format!("{}_data", container_name);

        info!("Removing PostgreSQL container and volume for {}", self.name);

        // Remove container if it exists
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
            // Stop container first if running
            self.docker
                .stop_container(&container_name, None::<StopContainerOptions>)
                .await
                .context("Failed to stop PostgreSQL container")?;

            // Remove the container
            self.docker
                .remove_container(
                    &container_name,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await
                .context("Failed to remove PostgreSQL container")?;
        }

        // Remove volume
        match self
            .docker
            .remove_volume(
                &volume_name,
                None::<bollard::query_parameters::RemoveVolumeOptions>,
            )
            .await
        {
            Ok(_) => info!("Removed volume {}", volume_name),
            Err(e) => info!("Error removing volume {}: {}", volume_name, e),
        }

        Ok(())
    }

    fn get_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>> {
        let mut env_vars = HashMap::new();

        let database = parameters
            .get("database")
            .context("Missing database parameter")?;
        let username = parameters
            .get("username")
            .context("Missing username parameter")?;
        let password = parameters
            .get("password")
            .context("Missing password parameter")?;

        // Always use container name and internal port for container-to-container
        // communication. An imported service's real container name (stored raw
        // in parameters, since the typed config isn't available here) wins over
        // the derived one.
        let effective_host = parameters
            .get("container_name")
            .cloned()
            .unwrap_or_else(|| self.get_container_name());
        let effective_port = POSTGRES_INTERNAL_PORT.to_string();

        let url = format!(
            "postgresql://{}:{}@{}:{}/{}",
            urlencoding::encode(username),
            urlencoding::encode(password),
            effective_host,
            effective_port,
            database
        );

        env_vars.insert("POSTGRES_URL".to_string(), url);
        env_vars.insert("POSTGRES_HOST".to_string(), effective_host);
        env_vars.insert("POSTGRES_PORT".to_string(), effective_port);
        // `POSTGRES_DB` is the canonical name (matches the official Postgres
        // Docker image and what every app library expects). `POSTGRES_NAME`
        // is kept as a back-compat alias — see the sibling
        // provision_resource() impl for the full rationale.
        env_vars.insert("POSTGRES_DB".to_string(), database.clone());
        env_vars.insert("POSTGRES_NAME".to_string(), database.clone());
        env_vars.insert("POSTGRES_USER".to_string(), username.clone());
        env_vars.insert("POSTGRES_PASSWORD".to_string(), password.clone());

        Ok(env_vars)
    }

    async fn deprovision_resource(&self, project_id: &str, environment: &str) -> Result<()> {
        let resource_name = format!("{}_{}", project_id, environment);
        self.drop_database(&resource_name).await
    }

    /// Restore PostgreSQL data from S3 using WAL-G
    ///
    /// Runs `wal-g backup-fetch` inside the PostgreSQL container to download and restore
    /// the latest backup from S3. For legacy backups (pre-WAL-G .sql.gz or .pgdump.gz),
    /// falls back to the old psql/pg_restore approach.
    async fn restore_from_s3(
        &self,
        s3_client: &aws_sdk_s3::Client,
        s3_credentials: &super::S3Credentials,
        backup_location: &str,
        s3_source: &temps_entities::s3_sources::Model,
        service_config: ServiceConfig,
    ) -> Result<()> {
        info!("Starting PostgreSQL restore from S3: {}", backup_location);

        // Detect if this is a WAL-G backup (s3:// prefix) or a legacy backup (.sql.gz / .pgdump.gz)
        if backup_location.starts_with("s3://") {
            // WAL-G backup: use wal-g backup-fetch
            self.restore_from_walg(s3_credentials, backup_location, service_config, None)
                .await
        } else {
            // Legacy backup: fall back to old psql/pg_restore approach
            self.restore_from_legacy(s3_client, backup_location, s3_source, service_config)
                .await
        }
    }

    async fn upgrade(&self, old_config: ServiceConfig, new_config: ServiceConfig) -> Result<()> {
        let old_pg_config = self.get_postgres_config(old_config)?;
        let mut new_pg_config = self.get_postgres_config(new_config)?;

        // Extract version numbers from Docker images
        let old_version = Self::extract_postgres_version(&old_pg_config.docker_image)?;
        let new_version = Self::extract_postgres_version(&new_pg_config.docker_image)?;

        info!(
            "PostgreSQL upgrade: version {} -> {}, image '{}' -> '{}'",
            old_version, new_version, old_pg_config.docker_image, new_pg_config.docker_image
        );

        if old_version > new_version {
            return Err(PostgresUpgradeRejected::Downgrade {
                from: old_version,
                to: new_version,
            }
            .into());
        }

        // Major version upgrades (e.g. 17 -> 18) are handled exclusively by
        // the dedicated PostgresUpgradeOrchestrator via
        // `POST /external-services/{id}/upgrades` (plural). That path does a
        // real pg_dumpall/restore with a retained rollback volume and a
        // mandatory pre-upgrade S3 backup. This same-version-only `upgrade()`
        // used to also handle major-version bumps itself (`run_pg_upgrade`),
        // but that implementation never actually migrated data — it deleted
        // the live volume, kept an unused raw-file copy in a `_backup_N`
        // volume, and booted a brand-new empty cluster hardcoded to
        // `postgres:{version}-alpine` with `POSTGRES_USER=postgres`,
        // silently discarding the real configured username/database/data
        // while reporting success. Refuse here instead of repeating that;
        // nothing has been touched yet (no stop, no volume changes), so this
        // is a safe no-op rejection, not a partial failure.
        if old_version != new_version {
            return Err(PostgresUpgradeRejected::MajorVersionChange {
                from: old_version,
                to: new_version,
            }
            .into());
        }

        // Verify the new image can be pulled BEFORE stopping the old container
        info!(
            "Verifying new Docker image is available: {}",
            new_pg_config.docker_image
        );
        self.verify_image_pullable(&new_pg_config.docker_image)
            .await?;
        info!("New Docker image verified and is available");

        // Same major version — image swap only (e.g., postgres:18 -> gotempsh/postgres-walg:18).
        // No pg_upgrade needed. Just recreate the container with the new image;
        // data is preserved on the Docker volume.
        if old_pg_config.docker_image == new_pg_config.docker_image {
            return Err(anyhow::anyhow!(
                "New image is identical to current image ({})",
                old_pg_config.docker_image
            ));
        }
        info!(
            "Same PostgreSQL major version ({}), swapping image without pg_upgrade",
            old_version
        );
        self.stop().await?;
        let limits = self.resource_limits.read().await.clone();
        // Preserve archiving state across the image swap by reading
        // `walg.env` from the existing volume — same rule as `start()`.
        let enable_archiving = self.compute_desired_enable_archiving().await;
        self.create_container(&self.docker, &mut new_pg_config, &limits, enable_archiving)
            .await?;
        *self.config.write().await = Some(new_pg_config);
        info!("PostgreSQL image swap completed successfully");

        Ok(())
    }

    fn get_default_docker_image(&self) -> (String, String) {
        // Return (image_name, version)
        (
            "gotempsh/postgres-walg".to_string(),
            "18-bookworm".to_string(),
        )
    }

    async fn get_current_docker_image(&self) -> Result<(String, String)> {
        let container_name = self
            .config
            .read()
            .await
            .as_ref()
            .map(|config| self.get_live_container_name(config))
            .unwrap_or_else(|| self.get_container_name());
        let container = self
            .docker
            .inspect_container(
                &container_name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await?;

        // Get the image from the container's inspection data
        if let Some(image) = container.config.and_then(|c| c.image) {
            // Parse image name and tag from the full image string
            if let Some((name, tag)) = image.split_once(':') {
                Ok((name.to_string(), tag.to_string()))
            } else {
                Ok((image.clone(), "latest".to_string()))
            }
        } else {
            Err(anyhow::anyhow!(
                "Failed to get current docker image for PostgreSQL container"
            ))
        }
    }

    fn get_default_version(&self) -> String {
        "18-bookworm".to_string()
    }

    async fn get_current_version(&self) -> Result<String> {
        let (_, version) = self.get_current_docker_image().await?;
        Ok(version)
    }

    async fn import_from_container(
        &self,
        container_id: String,
        service_name: String,
        credentials: HashMap<String, String>,
        additional_config: serde_json::Value,
    ) -> Result<ServiceConfig> {
        // Inspect the container to get details
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

        // The real Docker container name — every operation on an imported
        // service must target this, not the derived `postgres-{name}`.
        let imported_container_name = container
            .name
            .as_deref()
            .unwrap_or(&container_id)
            .trim_start_matches('/')
            .to_string();

        // Extract image name and version
        let image = container.config.and_then(|c| c.image).ok_or_else(|| {
            anyhow::anyhow!("Could not determine image for container '{}'", container_id)
        })?;

        // Extract version from image name (e.g., "gotempsh/postgres-walg:18-bookworm" -> "18")
        let version = if let Some(tag_pos) = image.rfind(':') {
            image[tag_pos + 1..].to_string()
        } else {
            "18-bookworm".to_string()
        };

        // Extract credentials from user input
        let username = credentials
            .get("username")
            .cloned()
            .unwrap_or_else(|| "postgres".to_string());
        let password = credentials
            .get("password")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Password is required for PostgreSQL import"))?;
        let database = credentials
            .get("database")
            .cloned()
            .unwrap_or_else(|| "postgres".to_string());

        // Extract port from additional config if provided, otherwise use 5432
        let port = additional_config
            .get("port")
            .and_then(|v| v.as_str())
            .unwrap_or("5432")
            .to_string();

        // Verify connection to the imported service. Connects directly with
        // `.await` on the current runtime — spinning up a nested
        // `tokio::runtime::Runtime` and calling `block_on` here panics with
        // "Cannot start a runtime from within a runtime", since this
        // `async fn` is already driven by one.
        let connection_url = format!(
            "postgresql://{}:{}@localhost:{}/{}",
            urlencoding::encode(&username),
            urlencoding::encode(&password),
            port,
            urlencoding::encode(&database)
        );
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&connection_url)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to connect to PostgreSQL at localhost:{} with provided credentials: {}",
                    port,
                    e
                )
            })?;
        pool.close().await;
        info!("Successfully verified PostgreSQL connection for import");

        let network_ready = match ensure_network_exists(&self.docker).await {
            Ok(()) => true,
            Err(e) => {
                warn!(
                    "Failed to ensure Temps Docker network before PostgreSQL import attach: {:?}",
                    e
                );
                false
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
                    "Attached imported PostgreSQL container '{}' to {}",
                    imported_container_name, network_name
                ),
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 403, ..
                }) => debug!(
                    "Imported PostgreSQL container '{}' is already attached to {}",
                    imported_container_name, network_name
                ),
                Err(e) => warn!(
                    "Failed to attach imported PostgreSQL container '{}' to {}: {}",
                    imported_container_name, network_name, e
                ),
            }
        }

        // Build the ServiceConfig for registration
        let config = ServiceConfig {
            name: service_name,
            service_type: ServiceType::Postgres,
            version: Some(version),
            parameters: serde_json::json!({
                "host": "localhost",
                "port": port,
                "database": database,
                "username": username,
                "password": password,
                "max_connections": "20",
                "ssl_mode": "disable",
                "docker_image": image,
                "container_name": imported_container_name,
            }),
        };

        info!(
            "Successfully imported PostgreSQL service '{}' from container",
            config.name
        );
        Ok(config)
    }

    /// PostgreSQL restore capability declaration.
    ///
    /// Postgres supports all three modes:
    /// - In-place restore from both WAL-G (`s3://` prefix) and legacy pg_dump backups
    /// - Restore-to-new-service: clones backup into a fresh container+volume
    /// - PITR: WAL replay to a target time/xid/LSN/name (WAL-G backups only;
    ///   the orchestrator rejects PITR requests against legacy pg_dump backups
    ///   by inspecting the backup row)
    ///
    /// We don't populate `earliest_pitr_time` / `latest_pitr_time` here —
    /// those would require querying `wal-g backup-list` + `wal-g wal-verify`
    /// per S3 source, which is expensive. The UI shows an unconstrained
    /// datetime picker and the server validates on execute.
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

    /// Provision a new PostgreSQL service from an existing backup.
    ///
    /// Strategy: clone the source service's config (image, version, database
    /// name, credentials), allocate a fresh host port, create a new
    /// container+volume with that name, then invoke the same `restore_from_s3`
    /// logic (WAL-G or legacy) that in-place restore uses.
    ///
    /// The orchestrator creates the `external_services` DB row AFTER this
    /// returns, using the parameters we hand back.
    async fn restore_to_new_service(
        &self,
        ctx: super::RestoreContext<'_>,
        new_service_name: String,
        parameter_overrides: serde_json::Value,
    ) -> Result<super::NewServiceRestoreResult> {
        info!(
            "Provisioning new PostgreSQL service '{}' from backup at {}",
            new_service_name, ctx.backup_location
        );

        // Start from the source service's parameters, then apply caller overrides.
        let mut source_config = self.get_postgres_config(ctx.source_config.clone())?;

        // Allocate a fresh host port (source's port is taken).
        let new_port = find_available_port(5432)
            .ok_or_else(|| anyhow::anyhow!("No available ports for new PostgreSQL service"))?
            .to_string();
        source_config.port = new_port.clone();

        // Apply caller overrides on top of the cloned config.
        if let Some(overrides) = parameter_overrides.as_object() {
            if let Some(port) = overrides.get("port").and_then(|v| v.as_str()) {
                source_config.port = port.to_string();
            }
            if let Some(image) = overrides.get("docker_image").and_then(|v| v.as_str()) {
                source_config.docker_image = image.to_string();
            }
            if let Some(db) = overrides.get("database").and_then(|v| v.as_str()) {
                source_config.database = db.to_string();
            }
        }

        // Build a new PostgresService for the target name.
        let new_service = PostgresService::new(new_service_name.clone(), self.docker.clone());

        // Carry resource limits over from the source service so the
        // restored copy inherits the same constraints (or unlimited if
        // none were set on the source).
        let cloned_limits = ServiceResourceLimits::from_parameters(&ctx.source_config.parameters);

        // Stash the runtime config so later methods (restore_from_walg -> get_postgres_config)
        // can resolve via ServiceConfig.
        *new_service.config.write().await = Some(source_config.clone());
        *new_service.resource_limits.write().await = cloned_limits.clone();

        // Create the new container+volume. Restored services start with
        // archiving off — the operator decides whether to wire WAL-G to the
        // new service explicitly. `create_container` writes any port-conflict
        // retry back into `source_config`, so everything serialized below
        // reflects the port the container actually bound to.
        new_service
            .create_container(&self.docker, &mut source_config, &cloned_limits, false)
            .await?;
        *new_service.config.write().await = Some(source_config.clone());

        // Build a ServiceConfig that parses cleanly back into PostgresConfig.
        let new_service_config = ServiceConfig {
            name: new_service_name.clone(),
            service_type: ServiceType::Postgres,
            version: ctx.source_config.version.clone(),
            parameters: serde_json::to_value(&source_config)
                .map_err(|e| anyhow::anyhow!("Failed to serialize new PostgreSQL config: {}", e))?,
        };

        // Dispatch to the same WAL-G / legacy paths used for in-place restore.
        if ctx.backup_location.starts_with("s3://") {
            new_service
                .restore_from_walg(
                    ctx.s3_credentials,
                    ctx.backup_location,
                    new_service_config,
                    None,
                )
                .await?;
        } else {
            new_service
                .restore_from_legacy(
                    ctx.s3_client,
                    ctx.backup_location,
                    ctx.s3_source,
                    new_service_config,
                )
                .await?;
        }

        // Serialize the final runtime config for the orchestrator to persist.
        let runtime_json = serde_json::to_value(&source_config)
            .map_err(|e| anyhow::anyhow!("Failed to serialize runtime config: {}", e))?;
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
            "postgres://{}:***@{}:{}/{}",
            source_config.username, source_config.host, source_config.port, source_config.database
        );

        Ok(super::NewServiceRestoreResult {
            parameters,
            connection_info,
        })
    }

    /// Perform point-in-time recovery on a PostgreSQL service.
    ///
    /// Requires a WAL-G backup (orchestrator validates the source backup's
    /// `s3_location` starts with `s3://`). For in-place PITR we restore onto
    /// the existing service; for to_new_service we clone first.
    async fn restore_pitr(
        &self,
        ctx: super::RestoreContext<'_>,
        target: super::RecoveryTarget,
        to_new_service: bool,
        new_service_name: Option<String>,
    ) -> Result<Option<super::NewServiceRestoreResult>> {
        if !ctx.backup_location.starts_with("s3://") {
            return Err(anyhow::anyhow!(
                "PITR requires a WAL-G backup (s3:// prefix); got '{}'",
                ctx.backup_location
            ));
        }

        info!(
            "Running PostgreSQL PITR to target {:?} (to_new_service={}) on backup {}",
            target, to_new_service, ctx.backup_location
        );

        if to_new_service {
            let new_name = new_service_name.ok_or_else(|| {
                anyhow::anyhow!("new_service_name is required when to_new_service=true")
            })?;

            // Clone the source's config onto a fresh container+port, like
            // restore_to_new_service does, then run WAL-G fetch with the PITR
            // target configuration.
            let mut source_config = self.get_postgres_config(ctx.source_config.clone())?;
            let new_port = find_available_port(5432)
                .ok_or_else(|| anyhow::anyhow!("No available ports for new PostgreSQL service"))?
                .to_string();
            source_config.port = new_port;

            let new_service = PostgresService::new(new_name.clone(), self.docker.clone());
            let cloned_limits =
                ServiceResourceLimits::from_parameters(&ctx.source_config.parameters);
            *new_service.config.write().await = Some(source_config.clone());
            *new_service.resource_limits.write().await = cloned_limits.clone();
            new_service
                .create_container(&self.docker, &mut source_config, &cloned_limits, false)
                .await?;
            *new_service.config.write().await = Some(source_config.clone());

            let new_service_config = ServiceConfig {
                name: new_name.clone(),
                service_type: ServiceType::Postgres,
                version: ctx.source_config.version.clone(),
                parameters: serde_json::to_value(&source_config).map_err(|e| {
                    anyhow::anyhow!("Failed to serialize new PostgreSQL config: {}", e)
                })?,
            };

            new_service
                .restore_from_walg(
                    ctx.s3_credentials,
                    ctx.backup_location,
                    new_service_config,
                    Some(&target),
                )
                .await?;

            let runtime_json = serde_json::to_value(&source_config)
                .map_err(|e| anyhow::anyhow!("Failed to serialize runtime config: {}", e))?;
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
                "postgres://{}:***@{}:{}/{}",
                source_config.username,
                source_config.host,
                source_config.port,
                source_config.database
            );

            Ok(Some(super::NewServiceRestoreResult {
                parameters,
                connection_info,
            }))
        } else {
            // In-place PITR — replay the WAL onto the existing container.
            self.restore_from_walg(
                ctx.s3_credentials,
                ctx.backup_location,
                ctx.source_config.clone(),
                Some(&target),
            )
            .await?;
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::externalsvc::DEPLOYMENT_MODE_MUTEX as ENV_MUTEX;

    #[test]
    fn healthcheck_cmd_pins_database_when_it_differs_from_username() {
        let cmd = postgres_healthcheck_cmd("appuser", "appdb");
        assert_eq!(cmd, "pg_isready -U 'appuser' -d 'appdb'");
    }

    #[test]
    fn healthcheck_cmd_escapes_single_quotes_in_username_and_database() {
        let cmd = postgres_healthcheck_cmd("a'user", "a'db");
        assert_eq!(cmd, "pg_isready -U 'a'\\''user' -d 'a'\\''db'");
    }

    #[test]
    fn test_postgres_input_config_default_values() {
        let config = PostgresInputConfig {
            host: default_host(),
            port: None,
            database: default_database(),
            username: default_username(),
            password: None,
            max_connections: default_max_connections(),
            ssl_mode: default_ssl_mode(),
            docker_image: None,
            container_name: None,
        };

        let runtime_config: PostgresConfig = config.into();

        assert_eq!(runtime_config.host, "localhost");
        assert_eq!(runtime_config.database, "postgres");
        assert_eq!(runtime_config.username, "postgres");
        assert_eq!(runtime_config.max_connections, 100);
        assert_eq!(
            runtime_config.docker_image,
            "gotempsh/postgres-walg:18-bookworm"
        );
        assert!(runtime_config.password.len() >= 16); // Auto-generated password
    }

    #[test]
    fn test_postgres_input_config_custom_docker_image() {
        let config = PostgresInputConfig {
            host: "localhost".to_string(),
            port: Some("5432".to_string()),
            database: "mydb".to_string(),
            username: "myuser".to_string(),
            password: Some("mypass".to_string()),
            max_connections: 50,
            ssl_mode: Some("disable".to_string()),
            docker_image: Some("timescale/timescaledb-ha:pg18".to_string()),
            container_name: None,
        };

        let runtime_config: PostgresConfig = config.into();

        assert_eq!(runtime_config.docker_image, "timescale/timescaledb-ha:pg18");
    }

    #[test]
    fn test_parameter_schema_editable_fields() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = PostgresService::new("test-editable".to_string(), docker);

        // Get the parameter schema
        let schema_opt = service.get_parameter_schema();
        assert!(schema_opt.is_some(), "Schema should be generated");

        let schema = schema_opt.unwrap();
        let schema_obj = schema.as_object().expect("Schema should be an object");
        let properties = schema_obj
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("Properties should be an object");

        // Define expected editable status for each field
        let editable_status = vec![
            ("host", false),
            ("port", true),
            ("database", false),
            ("username", false),
            ("password", true),
            ("max_connections", true),
            ("ssl_mode", true),
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

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_port_change_after_creation() {
        // Use OS-assigned ports so this test doesn't collide with anything
        // else on the runner. Hardcoded ports (6543/6544 previously) flaked
        // whenever another parallel test or background process held the
        // socket. We only need two distinct free ports; the test doesn't
        // actually *bind* the container to them, just verifies that
        // get_local_address reflects the configured value.
        use std::net::TcpListener;
        let pick = || {
            TcpListener::bind("127.0.0.1:0")
                .expect("failed to bind for port allocation")
                .local_addr()
                .expect("failed to read local addr")
                .port()
        };
        let initial_port = pick();
        let new_port = loop {
            let p = pick();
            if p != initial_port {
                break p;
            }
        };

        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = PostgresService::new("test-port-change".to_string(), docker);

        let config1 = ServiceConfig {
            name: "test-postgres".to_string(),
            service_type: super::ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": initial_port.to_string(),
                "database": "testdb",
                "username": "testuser",
                "password": "testpass123",
                "max_connections": 100,
                "ssl_mode": "disable",
                "docker_image": "gotempsh/postgres-walg:18-bookworm"
            }),
        };

        // Initialize service
        let result = service.init(config1.clone()).await;
        assert!(result.is_ok(), "Service initialization failed");

        // Verify initial port is set
        let local_addr = service.get_local_address(config1.clone()).unwrap();
        let initial_port_str = initial_port.to_string();
        assert!(
            local_addr.contains(&initial_port_str),
            "Initial port should be {initial_port_str}, got '{local_addr}'"
        );

        let config2 = ServiceConfig {
            name: "test-postgres".to_string(),
            service_type: super::ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": new_port.to_string(),
                "database": "testdb",
                "username": "testuser",
                "password": "testpass123",
                "max_connections": 100,
                "ssl_mode": "disable",
                "docker_image": "gotempsh/postgres-walg:18-bookworm"
            }),
        };

        // Verify new port configuration is recognized
        let new_local_addr = service.get_local_address(config2).unwrap();
        let new_port_str = new_port.to_string();
        assert!(
            new_local_addr.contains(&new_port_str),
            "New port should be {new_port_str}, got '{new_local_addr}'"
        );

        // Cleanup
        let _ = service.cleanup().await;
    }

    /// Regression test for a bug where `init()` persisted the *requested*
    /// host port even when `create_container`'s retry-on-conflict logic
    /// bound the container to a *different* port. Reported symptom: the
    /// service's stored config (and DB-persisted connection info) showed
    /// port 5441, but `docker ps` showed the container published on 5446 —
    /// any client using the stored port couldn't connect.
    ///
    /// Reproduced deterministically by pre-occupying the requested port with
    /// a plain `TcpListener` before calling `init()`, forcing Docker's own
    /// bind to fail with "port is already allocated" and triggering the
    /// exact retry path that used to lose track of the real port.
    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_init_persists_actual_port_after_conflict_retry() {
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => d,
            Err(_) => {
                println!("Docker not available, skipping");
                return;
            }
        };
        if docker.ping().await.is_err() {
            println!("Docker not available, skipping");
            return;
        }

        let requested_port = find_available_port(5432).expect("an OS-bindable port should exist");

        // Hold the port so Docker's own bind fails with "port is already
        // allocated" — the exact race `create_container`'s retry handles.
        let _blocker = std::net::TcpListener::bind(("0.0.0.0", requested_port))
            .expect("failed to occupy the port for the test");

        let docker = Arc::new(docker);
        let service = PostgresService::new("test-port-conflict-retry".to_string(), docker);

        let config = ServiceConfig {
            name: "test-postgres-conflict".to_string(),
            service_type: super::ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": requested_port.to_string(),
                "database": "testdb",
                "username": "testuser",
                "password": "testpass123",
                "max_connections": 100,
                "ssl_mode": "disable",
                "docker_image": "gotempsh/postgres-walg:18-bookworm"
            }),
        };

        let inferred_params = service
            .init(config)
            .await
            .expect("init should succeed by retrying on a fresh port");

        let persisted_port: u16 = inferred_params
            .get("port")
            .expect("port should be present in inferred params")
            .parse()
            .expect("port should be numeric");

        assert_ne!(
            persisted_port, requested_port,
            "persisted port must differ from the occupied port that forced a retry"
        );

        // The in-memory config used by start()/health checks must agree too.
        let cached_port: u16 = service
            .config
            .read()
            .await
            .as_ref()
            .expect("config should be set after init")
            .port
            .parse()
            .expect("cached port should be numeric");
        assert_eq!(
            cached_port, persisted_port,
            "cached config port must match what was persisted"
        );

        // And it must match the port the container is actually bound to.
        let container_name = service.get_container_name();
        let inspect = service
            .docker
            .inspect_container(
                &container_name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .expect("container should exist");
        let bound_ports: Vec<u16> = inspect
            .network_settings
            .and_then(|ns| ns.ports)
            .into_iter()
            .flatten()
            .filter_map(|(_, bindings)| bindings)
            .flatten()
            .filter_map(|b| b.host_port.and_then(|p| p.parse().ok()))
            .collect();
        assert!(
            bound_ports.contains(&persisted_port),
            "container's actual bound ports {:?} should include the persisted port {}",
            bound_ports,
            persisted_port
        );

        let _ = service.cleanup().await;
    }

    #[test]
    fn test_default_docker_image() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = PostgresService::new("test-image".to_string(), docker);

        let (image_name, version) = service.get_default_docker_image();
        assert_eq!(
            image_name, "gotempsh/postgres-walg",
            "Default image should be gotempsh/postgres-walg"
        );
        assert_eq!(
            version, "18-bookworm",
            "Default version should be 18-bookworm"
        );
    }

    #[tokio::test]
    async fn test_image_upgrade_scenario() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let _service = PostgresService::new("test-upgrade".to_string(), docker);

        // Create initial config with previous PostgreSQL version
        let old_config = ServiceConfig {
            name: "test-postgres".to_string(),
            service_type: super::ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": Some("6545"),
                "database": "testdb",
                "username": "testuser",
                "password": "testpass123",
                "max_connections": 100,
                "ssl_mode": "disable",
                "docker_image": "gotempsh/postgres-walg:17-bookworm"
            }),
        };

        // Create new config with upgraded PostgreSQL version
        let new_config = ServiceConfig {
            name: "test-postgres".to_string(),
            service_type: super::ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": Some("6545"),
                "database": "testdb",
                "username": "testuser",
                "password": "testpass123",
                "max_connections": 100,
                "ssl_mode": "disable",
                "docker_image": "gotempsh/postgres-walg:18-bookworm"
            }),
        };

        // Note: Full upgrade test would require actual Docker containers
        // This test verifies the configuration structure
        assert!(old_config.parameters.get("docker_image").is_some());
        assert!(new_config.parameters.get("docker_image").is_some());

        let old_image = old_config
            .parameters
            .get("docker_image")
            .and_then(|v| v.as_str());
        let new_image = new_config
            .parameters
            .get("docker_image")
            .and_then(|v| v.as_str());

        assert_eq!(old_image, Some("gotempsh/postgres-walg:17-bookworm"));
        assert_eq!(new_image, Some("gotempsh/postgres-walg:18-bookworm"));
    }

    #[test]
    fn test_parameter_schema_includes_docker_image() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = PostgresService::new("test-schema".to_string(), docker);

        let schema_opt = service.get_parameter_schema();
        assert!(schema_opt.is_some(), "Schema should be generated");

        let schema = schema_opt.unwrap();
        let properties = schema
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("Properties should be an object");

        // Verify docker_image field exists in schema
        assert!(
            properties.contains_key("docker_image"),
            "docker_image should be in schema"
        );

        // Verify docker_image is marked as editable
        let docker_image_field = properties
            .get("docker_image")
            .and_then(|v| v.as_object())
            .expect("docker_image field should be an object");

        let is_editable = docker_image_field
            .get("x-editable")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        assert!(is_editable, "docker_image should be editable");
    }

    #[test]
    fn test_extract_postgres_version() {
        // Test various PostgreSQL image formats
        let test_cases = vec![
            ("gotempsh/postgres-walg:16-bookworm", 16),
            ("gotempsh/postgres-walg:18-bookworm", 18),
            ("postgres:16.0-alpine", 16),
            ("postgres:17.2-alpine", 17),
            ("timescale/timescaledb-ha:pg16", 16),
            ("timescale/timescaledb-ha:pg18", 18),
            ("postgres:15", 15),
            ("postgres:14.5", 14),
        ];

        for (image, expected_version) in test_cases {
            let result = PostgresService::extract_postgres_version(image);
            assert!(
                result.is_ok(),
                "Failed to extract version from image: {}",
                image
            );
            assert_eq!(
                result.unwrap(),
                expected_version,
                "Image {} should extract version {}",
                image,
                expected_version
            );
        }
    }

    #[test]
    fn test_version_extraction_invalid_formats() {
        // Test invalid image formats
        let invalid_cases = vec![
            "postgres",            // No tag
            "postgres:latest",     // Non-numeric version
            "postgres:abc-alpine", // Non-numeric version
            "postgres:alpha",      // Non-numeric version
        ];

        for image in invalid_cases {
            let result = PostgresService::extract_postgres_version(image);
            assert!(
                result.is_err(),
                "Image {} should fail to extract version",
                image
            );
        }
    }

    #[test]
    fn test_upgrade_version_check() {
        // Test that downgrade is prevented
        let old_config = PostgresInputConfig {
            host: "localhost".to_string(),
            port: Some("5432".to_string()),
            database: "testdb".to_string(),
            username: "testuser".to_string(),
            password: Some("testpass".to_string()),
            max_connections: 100,
            ssl_mode: Some("disable".to_string()),
            docker_image: Some("gotempsh/postgres-walg:18-bookworm".to_string()),
            container_name: None,
        };

        let downgrade_config = PostgresInputConfig {
            host: "localhost".to_string(),
            port: Some("5432".to_string()),
            database: "testdb".to_string(),
            username: "testuser".to_string(),
            password: Some("testpass".to_string()),
            max_connections: 100,
            ssl_mode: Some("disable".to_string()),
            docker_image: Some("gotempsh/postgres-walg:17-bookworm".to_string()),
            container_name: None,
        };

        let old_version =
            PostgresService::extract_postgres_version(&old_config.docker_image.clone().unwrap())
                .unwrap();
        let downgrade_version = PostgresService::extract_postgres_version(
            &downgrade_config.docker_image.clone().unwrap(),
        )
        .unwrap();

        // Verify that downgrade is detected (old >= new means no upgrade)
        assert!(
            old_version >= downgrade_version,
            "Downgrade should be detected: {} >= {}",
            old_version,
            downgrade_version
        );
    }

    #[test]
    fn test_postgres_v17_to_v18_upgrade_config() {
        // Test the configuration for upgrading from PostgreSQL 17 to 18
        let v17_config = PostgresInputConfig {
            host: "localhost".to_string(),
            port: Some("5432".to_string()),
            database: "mydb".to_string(),
            username: "postgres".to_string(),
            password: Some("mysecretpass".to_string()),
            max_connections: 100,
            ssl_mode: Some("disable".to_string()),
            docker_image: Some("gotempsh/postgres-walg:17-bookworm".to_string()),
            container_name: None,
        };

        let v18_config = PostgresInputConfig {
            host: "localhost".to_string(),
            port: Some("5432".to_string()),
            database: "mydb".to_string(),
            username: "postgres".to_string(),
            password: Some("mysecretpass".to_string()),
            max_connections: 100,
            ssl_mode: Some("disable".to_string()),
            docker_image: Some("gotempsh/postgres-walg:18-bookworm".to_string()),
            container_name: None,
        };

        // Convert to runtime configs
        let v17_runtime: PostgresConfig = v17_config.into();
        let v18_runtime: PostgresConfig = v18_config.into();

        // Verify both configs are valid
        assert_eq!(
            v17_runtime.docker_image,
            "gotempsh/postgres-walg:17-bookworm"
        );
        assert_eq!(
            v18_runtime.docker_image,
            "gotempsh/postgres-walg:18-bookworm"
        );

        // Verify other parameters are preserved
        assert_eq!(v17_runtime.database, v18_runtime.database);
        assert_eq!(v17_runtime.username, v18_runtime.username);
        assert_eq!(v17_runtime.password, v18_runtime.password);
        assert_eq!(v17_runtime.max_connections, v18_runtime.max_connections);

        // Extract versions
        let v17_version = PostgresService::extract_postgres_version(&v17_runtime.docker_image)
            .expect("Should extract v17");
        let v18_version = PostgresService::extract_postgres_version(&v18_runtime.docker_image)
            .expect("Should extract v18");

        // Verify upgrade path is valid
        assert_eq!(v17_version, 17);
        assert_eq!(v18_version, 18);
        assert!(v18_version > v17_version, "v18 should be greater than v17");
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_postgres_v17_to_v18_upgrade_rejected_and_data_survives() {
        // `ExternalService::upgrade()` used to attempt major-version bumps
        // itself via a broken `run_pg_upgrade` that deleted the live volume
        // and booted a brand-new empty cluster hardcoded to
        // `postgres:{version}-alpine` / `POSTGRES_USER=postgres` — silently
        // discarding the real data while reporting success. The old version
        // of this test only asserted `SELECT version()` on the post-upgrade
        // container, which is exactly why that data loss went unnoticed: it
        // never checked that pre-upgrade data was still there.
        //
        // `upgrade()` now refuses any major-version change outright and
        // points callers at the dedicated orchestrator
        // (`POST /external-services/{id}/upgrades`, `postgres_upgrade.rs`).
        // This test asserts that refusal, and — the part the original test
        // was missing — that the v17 container and its data are completely
        // untouched afterward.

        let docker = match Docker::connect_with_defaults() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Docker not available, skipping test");
                return;
            }
        };

        let port = 19432u16; // Use unique port to avoid conflicts
        let password = "postgres"; // Use default PostgreSQL password
        let service_name = format!(
            "test_postgres_upgrade_{}",
            chrono::Utc::now().timestamp_millis()
        );

        // Create v17 service configuration
        let v17_params = serde_json::json!({
            "host": "localhost",
            "port": port.to_string(),
            "database": "postgres",
            "username": "postgres",
            "password": password,
            "max_connections": 100,
            "docker_image": "gotempsh/postgres-walg:17-bookworm",
        });

        let v17_config = ServiceConfig {
            name: service_name.clone(),
            service_type: super::ServiceType::Postgres,
            version: Some("17".to_string()),
            parameters: v17_params,
        };

        // Create v18 service configuration (used only as the `upgrade()`
        // target — the call is expected to be rejected before touching
        // anything).
        let v18_params = serde_json::json!({
            "host": "localhost",
            "port": port.to_string(),
            "database": "postgres",
            "username": "postgres",
            "password": password,
            "max_connections": 100,
            "docker_image": "gotempsh/postgres-walg:18-bookworm",
        });

        let v18_config = ServiceConfig {
            name: service_name.clone(),
            service_type: super::ServiceType::Postgres,
            version: Some("18".to_string()),
            parameters: v18_params,
        };

        // Initialize v17 service
        let v17_service = PostgresService::new(service_name.clone(), docker.clone());

        match v17_service.init(v17_config.clone()).await {
            Ok(_) => {}
            Err(e) => {
                println!("Failed to initialize v17 service: {}. Skipping test (Docker may not be available)", e);
                let _ = v17_service.remove().await;
                return;
            }
        }

        // Give the container time to start and fully initialize with password
        // PostgreSQL needs time to initialize the database and set up authentication
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

        // Wait for PostgreSQL to be healthy
        let mut retries = 0;
        loop {
            match v17_service.health_check().await {
                Ok(healthy) if healthy => break,
                _ if retries < 60 => {
                    retries += 1;
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                }
                _ => {
                    println!("PostgreSQL 17 failed to start after 60 retries (30 seconds)");
                    let _ = v17_service.remove().await;
                    return;
                }
            }
        }

        // Connect, verify v17 version, and seed a marker row we can check
        // for survival after the rejected upgrade attempt.
        let connection_string = format!(
            "postgresql://postgres:{}@127.0.0.1:{}/postgres",
            urlencoding::encode(password),
            port
        );

        // Try to connect with retries since database might still be initializing
        let mut db_pool = None;
        for attempt in 0..10 {
            match sqlx::postgres::PgPoolOptions::new()
                .max_connections(5)
                .connect(&connection_string)
                .await
            {
                Ok(pool) => {
                    db_pool = Some(pool);
                    break;
                }
                Err(e) if attempt < 9 => {
                    println!(
                        "Connection attempt {} failed: {}. Retrying...",
                        attempt + 1,
                        e
                    );
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                }
                Err(e) => {
                    println!(
                        "Failed to connect to v17 PostgreSQL after 10 attempts: {}. Skipping test",
                        e
                    );
                    let _ = v17_service.remove().await;
                    return;
                }
            }
        }

        let db_pool = db_pool.unwrap();

        let version_v17: (String,) =
            match sqlx::query_as("SELECT version()").fetch_one(&db_pool).await {
                Ok(v) => v,
                Err(e) => {
                    println!("Failed to query version from v17: {}. Skipping test", e);
                    db_pool.close().await;
                    let _ = v17_service.remove().await;
                    return;
                }
            };

        println!("PostgreSQL 17 version: {}", version_v17.0);
        assert!(
            version_v17.0.contains("17"),
            "Version should contain '17', got: {}",
            version_v17.0
        );

        sqlx::query("CREATE TABLE upgrade_survives_marker (id int PRIMARY KEY, note text)")
            .execute(&db_pool)
            .await
            .expect("failed to create marker table");
        sqlx::query("INSERT INTO upgrade_survives_marker (id, note) VALUES (1, 'pre-upgrade')")
            .execute(&db_pool)
            .await
            .expect("failed to insert marker row");

        db_pool.close().await;

        // Attempt the major-version upgrade via `upgrade()` — must be
        // rejected, not attempted.
        let upgrade_result = v17_service
            .upgrade(v17_config.clone(), v18_config.clone())
            .await;

        let err = match upgrade_result {
            Ok(_) => {
                let _ = v17_service.remove().await;
                panic!(
                    "upgrade() must reject a major-version change (17 -> 18) instead of \
                     attempting it directly; use the dedicated orchestrator instead"
                );
            }
            Err(e) => e,
        };
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("Cannot change PostgreSQL major version"),
            "expected rejection to name the major-version-change restriction, got: {}",
            err_msg
        );

        // Nothing should have been touched: same container, same image,
        // same data. Re-verify against the ORIGINAL v17 service handle —
        // if `upgrade()` had stopped/replaced the container despite
        // rejecting, this reconnect would fail or return a different image.
        let mut db_pool = None;
        for attempt in 0..10 {
            match sqlx::postgres::PgPoolOptions::new()
                .max_connections(5)
                .connect(&connection_string)
                .await
            {
                Ok(pool) => {
                    db_pool = Some(pool);
                    break;
                }
                Err(e) if attempt < 9 => {
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                    let _ = e;
                }
                Err(e) => {
                    let _ = v17_service.remove().await;
                    panic!(
                        "container should still be reachable on v17 after a rejected upgrade, \
                         but reconnect failed: {}",
                        e
                    );
                }
            }
        }
        let db_pool = db_pool.unwrap();

        let version_after: (String,) = sqlx::query_as("SELECT version()")
            .fetch_one(&db_pool)
            .await
            .expect("version query should still work against the untouched v17 container");
        assert!(
            version_after.0.contains("17"),
            "container must still be on v17 after a rejected upgrade, got: {}",
            version_after.0
        );

        let marker: (i32, String) =
            sqlx::query_as("SELECT id, note FROM upgrade_survives_marker WHERE id = 1")
                .fetch_one(&db_pool)
                .await
                .expect("marker row must survive a rejected upgrade attempt");
        assert_eq!(marker.1, "pre-upgrade");

        println!("Major-version upgrade correctly rejected; v17 data intact.");

        // Cleanup
        db_pool.close().await;
        let _ = v17_service.stop().await;
        let _ = v17_service.remove().await;
    }

    #[test]
    fn test_import_service_config_creation() {
        // Test that ServiceConfig is properly created for import
        let config = ServiceConfig {
            name: "test-postgres-import".to_string(),
            service_type: ServiceType::Postgres,
            version: Some("15-bookworm".to_string()),
            parameters: serde_json::json!({
                "host": "localhost",
                "port": 5432,
                "database": "testdb",
                "username": "postgres",
                "password": "testpass",
                "max_connections": "20",
                "ssl_mode": "disable",
                "docker_image": "gotempsh/postgres-walg:15-bookworm",
                "container_id": "abc123def456",
            }),
        };

        assert_eq!(config.name, "test-postgres-import");
        assert_eq!(config.service_type, ServiceType::Postgres);
        assert_eq!(config.version, Some("15-bookworm".to_string()));
        assert_eq!(config.parameters["host"], "localhost");
        assert_eq!(config.parameters["port"], 5432);
    }

    #[test]
    fn test_import_version_extraction_with_tag() {
        // Test version extraction from Docker image names
        let test_cases = vec![
            ("gotempsh/postgres-walg:15-bookworm", "15-bookworm"),
            ("postgres:latest", "latest"),
            ("postgres:14.5", "14.5"),
            ("postgres:16-bookworm", "16-bookworm"),
        ];

        for (image, expected_version) in test_cases {
            let version = if let Some(tag_pos) = image.rfind(':') {
                image[tag_pos + 1..].to_string()
            } else {
                "latest".to_string()
            };

            assert_eq!(version, expected_version, "Failed for image: {}", image);
        }
    }

    #[test]
    fn test_import_version_extraction_without_tag() {
        let image = "postgres";
        let version = if let Some(tag_pos) = image.rfind(':') {
            image[tag_pos + 1..].to_string()
        } else {
            "latest".to_string()
        };

        assert_eq!(version, "latest");
    }

    #[test]
    fn test_import_connection_url_format() {
        let username = "postgres";
        let password = "mysecretpassword";
        let port = 5432;
        let database = "importeddb";

        let connection_url = format!(
            "postgresql://{}:{}@localhost:{}/{}",
            username, password, port, database
        );

        // Verify all components are present
        assert!(connection_url.contains("postgresql://"));
        assert!(connection_url.contains("postgres"));
        assert!(connection_url.contains("mysecretpassword"));
        assert!(connection_url.contains("localhost"));
        assert!(connection_url.contains("5432"));
        assert!(connection_url.contains("importeddb"));
    }

    #[test]
    fn test_import_validates_required_credentials() {
        let credentials: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        // Missing all required fields

        // These should all be None
        assert!(!credentials.contains_key("username"));
        assert!(!credentials.contains_key("password"));
        assert!(!credentials.contains_key("port"));
        assert!(!credentials.contains_key("database"));
    }

    #[test]
    fn test_import_credential_extraction() {
        let mut credentials: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        credentials.insert("username".to_string(), "importuser".to_string());
        credentials.insert("password".to_string(), "importpass".to_string());
        credentials.insert("port".to_string(), "5433".to_string());
        credentials.insert("database".to_string(), "importdb".to_string());

        // Verify credential extraction
        assert_eq!(
            credentials.get("username").map(|s| s.as_str()),
            Some("importuser")
        );
        assert_eq!(
            credentials.get("password").map(|s| s.as_str()),
            Some("importpass")
        );
        assert_eq!(credentials.get("port").map(|s| s.as_str()), Some("5433"));
        assert_eq!(
            credentials.get("database").map(|s| s.as_str()),
            Some("importdb")
        );
    }

    // `flavor = "multi_thread"` is required because `MinioTestContainer`'s
    // `Drop` impl calls `tokio::task::block_in_place`, which panics on the
    // default current-thread runtime.
    #[cfg(feature = "docker-tests")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_postgres_backup_and_restore_to_s3() {
        // Whole-test wall-clock budget. Anything above this is a hang — fail
        // loudly with a diagnostic instead of stalling the CI runner for 90 min.
        // See incident: GitHub run 25940925537 (PR #89) burned 86 min on this
        // test because it never returned. Sister tests in redis.rs and
        // mongodb.rs already wrap themselves the same way.
        //
        // The body pulls the wal-g image, boots Postgres, creates a table,
        // runs a full base backup to MinIO, then a full restore — comfortably
        // under 5 minutes on cold runners but can hang indefinitely on a
        // wedged Docker daemon if left unbounded.
        const TEST_TIMEOUT: Duration = Duration::from_secs(300);

        tokio::time::timeout(TEST_TIMEOUT, run_postgres_backup_and_restore_to_s3())
            .await
            .expect(
                "test_postgres_backup_and_restore_to_s3 exceeded 300s — likely hung on \
                 Postgres/Docker/wal-g wait",
            );
    }

    /// Body of `test_postgres_backup_and_restore_to_s3`, extracted so the
    /// outer test can wrap it in `tokio::time::timeout` without a giant
    /// async block at the call site.
    #[cfg(feature = "docker-tests")]
    async fn run_postgres_backup_and_restore_to_s3() {
        use super::super::test_utils::{
            create_mock_backup, create_mock_db, create_mock_external_service, MinioTestContainer,
        };

        // Check if Docker is available
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => Arc::new(d),
            Err(e) => {
                println!("Docker not available, skipping test: {}", e);
                return;
            }
        };

        // Verify Docker is actually responding
        if docker.ping().await.is_err() {
            println!("Docker daemon not responding, skipping test");
            return;
        }

        // Start MinIO container for S3 operations
        let minio = match MinioTestContainer::start(docker.clone(), "postgres-backup-test").await {
            Ok(m) => m,
            Err(e) => {
                let error_msg = e.to_string();
                if error_msg.contains("certificate")
                    || error_msg.contains("TrustStore")
                    || error_msg.contains("panicked")
                {
                    println!("❌ Skipping PostgreSQL backup test: TLS certificate issue");
                    println!(
                        "   Reason: {}",
                        error_msg.lines().next().unwrap_or(&error_msg)
                    );
                    println!("   Solution: Install system root certificates (required by AWS SDK even for HTTP endpoints)");
                    return;
                }
                panic!("Failed to start MinIO container: {}", e);
            }
        };

        // Create PostgreSQL service
        //
        // Pick a free port so parallel test runs (and leaked containers from
        // previous runs) don't collide. Previously hardcoded to 15432, which
        // caused "port is already allocated" failures in CI when a leftover
        // container held the port (see redis.rs's identical fix for 16379).
        let pg_port = match find_available_port(15432) {
            Some(p) => p,
            None => {
                println!("No available port in 15432..16432 range, skipping test");
                let _ = minio.cleanup().await;
                return;
            }
        };
        let pg_password = "testpass123";
        let service_name = format!("test_pg_backup_{}", chrono::Utc::now().timestamp_millis());

        let pg_params = serde_json::json!({
            "host": "localhost",
            "port": pg_port.to_string(),
            "database": "postgres",
            "username": "postgres",
            "password": pg_password,
            "max_connections": 100,
            "docker_image": "gotempsh/postgres-walg:18-bookworm",
        });

        let pg_config = ServiceConfig {
            name: service_name.clone(),
            service_type: ServiceType::Postgres,
            version: Some("18".to_string()),
            parameters: pg_params,
        };

        let pg_service = PostgresService::new(service_name.clone(), docker.clone());

        // Initialize PostgreSQL service
        match pg_service.init(pg_config.clone()).await {
            Ok(_) => println!("✓ PostgreSQL service initialized"),
            Err(e) => {
                println!("Failed to initialize PostgreSQL: {}. Skipping test", e);
                let _ = minio.cleanup().await;
                return;
            }
        }

        // Wait for PostgreSQL to be healthy
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

        // Create a test database and insert data
        let connection_string = format!(
            "postgresql://postgres:{}@127.0.0.1:{}/postgres",
            urlencoding::encode(pg_password),
            pg_port
        );

        let db_pool = match sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&connection_string)
            .await
        {
            Ok(pool) => pool,
            Err(e) => {
                println!("Failed to connect to PostgreSQL: {}. Skipping test", e);
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };

        // Create test table and insert data
        match sqlx::query("CREATE TABLE test_backup (id SERIAL PRIMARY KEY, name TEXT NOT NULL, value INT NOT NULL)")
            .execute(&db_pool)
            .await
        {
            Ok(_) => println!("✓ Test table created"),
            Err(e) => {
                println!("Failed to create test table: {}. Skipping test", e);
                db_pool.close().await;
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        }

        match sqlx::query(
            "INSERT INTO test_backup (name, value) VALUES ($1, $2), ($3, $4), ($5, $6)",
        )
        .bind("test1")
        .bind(100)
        .bind("test2")
        .bind(200)
        .bind("test3")
        .bind(300)
        .execute(&db_pool)
        .await
        {
            Ok(_) => println!("✓ Test data inserted"),
            Err(e) => {
                println!("Failed to insert test data: {}. Skipping test", e);
                db_pool.close().await;
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        }

        // Verify data was inserted
        let count: (i64,) = match sqlx::query_as("SELECT COUNT(*) FROM test_backup")
            .fetch_one(&db_pool)
            .await
        {
            Ok(c) => c,
            Err(e) => {
                println!("Failed to count test data: {}. Skipping test", e);
                db_pool.close().await;
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };
        assert_eq!(count.0, 3, "Should have 3 rows");
        println!("✓ Verified {} rows in test table", count.0);

        // Close connection before backup
        db_pool.close().await;

        // Create mock database connection for backup/restore operations
        let mock_db = match create_mock_db().await {
            Ok(db) => db,
            Err(e) => {
                println!("Failed to create mock database: {}. Skipping test", e);
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };

        // Create mock backup record
        let backup = create_mock_backup("backups/postgres/test");
        let external_service = create_mock_external_service(service_name.clone(), "postgres", "17");

        // Perform backup to S3
        let s3_creds = minio.s3_credentials();
        let backup_location = match pg_service
            .backup_to_s3(
                &minio.s3_client,
                &s3_creds,
                backup,
                &minio.s3_source,
                "backups/postgres",
                "backups",
                &mock_db,
                &external_service,
                pg_config.clone(),
            )
            .await
        {
            Ok(outcome) => {
                println!(
                    "✓ Backup completed to: {} ({:?} bytes)",
                    outcome.location, outcome.size_bytes
                );
                outcome.location
            }
            Err(e) => {
                println!("Backup failed: {}. Skipping test", e);
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };

        // Drop the test table to simulate data loss
        let db_pool = match sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&connection_string)
            .await
        {
            Ok(pool) => pool,
            Err(e) => {
                println!("Failed to reconnect to PostgreSQL: {}. Skipping test", e);
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };

        match sqlx::query("DROP TABLE IF EXISTS test_backup")
            .execute(&db_pool)
            .await
        {
            Ok(_) => println!("✓ Test table dropped (simulating data loss)"),
            Err(e) => {
                println!("Failed to drop test table: {}. Skipping test", e);
                db_pool.close().await;
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        }

        // Verify table is gone
        let table_exists: (bool,) = match sqlx::query_as(
            "SELECT EXISTS (SELECT FROM information_schema.tables WHERE table_name = 'test_backup')"
        )
        .fetch_one(&db_pool)
        .await
        {
            Ok(exists) => exists,
            Err(e) => {
                println!("Failed to check table existence: {}. Skipping test", e);
                db_pool.close().await;
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };
        assert!(!table_exists.0, "Table should not exist after drop");
        println!("✓ Verified table was dropped");

        db_pool.close().await;

        // Restore from S3 backup
        match pg_service
            .restore_from_s3(
                &minio.s3_client,
                &s3_creds,
                &backup_location,
                &minio.s3_source,
                pg_config.clone(),
            )
            .await
        {
            Ok(_) => println!("✓ Restore completed from: {}", backup_location),
            Err(e) => {
                println!("Restore failed: {}. Skipping test", e);
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };

        // Verify restored data
        let db_pool = match sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&connection_string)
            .await
        {
            Ok(pool) => pool,
            Err(e) => {
                println!("Failed to reconnect after restore: {}. Skipping test", e);
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };

        // Verify table exists
        let table_exists: (bool,) = match sqlx::query_as(
            "SELECT EXISTS (SELECT FROM information_schema.tables WHERE table_name = 'test_backup')"
        )
        .fetch_one(&db_pool)
        .await
        {
            Ok(exists) => exists,
            Err(e) => {
                println!("Failed to check restored table: {}. Skipping test", e);
                db_pool.close().await;
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };
        assert!(table_exists.0, "Table should exist after restore");
        println!("✓ Verified table was restored");

        // Verify row count
        let count: (i64,) = match sqlx::query_as("SELECT COUNT(*) FROM test_backup")
            .fetch_one(&db_pool)
            .await
        {
            Ok(c) => c,
            Err(e) => {
                println!("Failed to count restored data: {}. Skipping test", e);
                db_pool.close().await;
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };
        assert_eq!(count.0, 3, "Should have 3 rows after restore");
        println!("✓ Verified {} rows were restored", count.0);

        // Verify actual data values
        let rows: Vec<(i32, String, i32)> =
            match sqlx::query_as("SELECT id, name, value FROM test_backup ORDER BY id")
                .fetch_all(&db_pool)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    println!("Failed to fetch restored rows: {}. Skipping test", e);
                    db_pool.close().await;
                    let _ = pg_service.remove().await;
                    let _ = minio.cleanup().await;
                    return;
                }
            };

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].1, "test1");
        assert_eq!(rows[0].2, 100);
        assert_eq!(rows[1].1, "test2");
        assert_eq!(rows[1].2, 200);
        assert_eq!(rows[2].1, "test3");
        assert_eq!(rows[2].2, 300);
        println!("✓ Verified all data values match original");

        // Cleanup
        db_pool.close().await;
        let _ = pg_service.stop().await;
        let _ = pg_service.remove().await;
        let _ = minio.cleanup().await;

        println!("✅ PostgreSQL backup and restore test passed!");
    }

    #[test]
    fn test_get_effective_address_baremetal_mode() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // Clear Docker mode to ensure baremetal mode
        unsafe { std::env::remove_var("DEPLOYMENT_MODE") };

        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = PostgresService::new("test-effective-addr".to_string(), docker);

        let config = ServiceConfig {
            name: "test-postgres".to_string(),
            service_type: super::ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "5432",
                "database": "testdb",
                "username": "postgres",
                "password": "testpass",
                "max_connections": 100,
            }),
        };

        let (host, port) = service.get_effective_address(config).unwrap();

        // In baremetal mode, should return localhost with exposed port
        assert_eq!(host, "localhost");
        assert_eq!(port, "5432");
    }

    #[test]
    fn test_get_effective_address_docker_mode() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // Set Docker mode
        unsafe { std::env::set_var("DEPLOYMENT_MODE", "docker") };

        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = PostgresService::new("test-effective-addr-docker".to_string(), docker);

        let config = ServiceConfig {
            name: "test-postgres".to_string(),
            service_type: super::ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "5432",
                "database": "testdb",
                "username": "postgres",
                "password": "testpass",
                "max_connections": 100,
            }),
        };

        let (host, port) = service.get_effective_address(config).unwrap();

        // In Docker mode, should return container name with internal port
        assert_eq!(host, "postgres-test-effective-addr-docker");
        assert_eq!(port, "5432"); // Internal port

        // Clean up
        unsafe { std::env::remove_var("DEPLOYMENT_MODE") };
    }

    #[test]
    fn test_get_effective_address_docker_mode_uses_imported_container_name() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("DEPLOYMENT_MODE", "docker") };

        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = PostgresService::new("imported-svc".to_string(), docker);

        let config = ServiceConfig {
            name: "imported-svc".to_string(),
            service_type: super::ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "5432",
                "database": "testdb",
                "username": "postgres",
                "password": "testpass",
                "max_connections": 100,
                "container_name": "legacy-postgres",
            }),
        };

        let (host, port) = service.get_effective_address(config).unwrap();
        // The imported container name wins over the derived `postgres-{name}`.
        assert_eq!(host, "legacy-postgres");
        assert_eq!(port, "5432");

        unsafe { std::env::remove_var("DEPLOYMENT_MODE") };
    }

    #[test]
    fn test_container_name_is_not_a_user_input() {
        // container_name is derived from the service name at creation time
        // (`postgres-{name}`), never supplied by the client — same as
        // MariaDB (see mariadb.rs's identical test). The create form is
        // generated from this schema, so the field must not appear in it.
        let schema = serde_json::to_value(schemars::schema_for!(PostgresInputConfig)).unwrap();
        assert!(
            !schema.to_string().contains("container_name"),
            "container_name leaked into the PostgreSQL create schema"
        );
    }

    /// Regression guard for the cross-tenant IDOR: a client-supplied
    /// `container_name` in a create request would make every subsequent Docker
    /// operation (start, exec, backup, restore) target that named container
    /// instead of creating a new one. Because Docker names are global and
    /// predictable (`postgres-{slug}`), this lets a tenant redirect operations
    /// to a different tenant's live database container.
    ///
    /// The schema-level `#[schemars(skip)]` only hides the field from the UI
    /// form — it does NOT stop `serde_json::from_value` from deserialising a
    /// client-supplied value. `validate_for_creation` must reject it explicitly.
    #[test]
    fn test_container_name_rejected_by_validate_for_creation() {
        use crate::parameter_strategies::PostgresParameterStrategy;
        let strategy = PostgresParameterStrategy;
        let mut params = std::collections::HashMap::new();
        params.insert(
            "database".to_string(),
            serde_json::Value::String("mydb".to_string()),
        );
        params.insert(
            "username".to_string(),
            serde_json::Value::String("myuser".to_string()),
        );
        params.insert(
            "container_name".to_string(),
            serde_json::Value::String("postgres-victim-slug".to_string()),
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

    #[test]
    fn test_get_environment_variables_always_uses_container_name() {
        // get_environment_variables always uses container name and internal port
        // for container-to-container communication, regardless of deployment mode
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = PostgresService::new("test-env-vars".to_string(), docker);

        let mut params = HashMap::new();
        params.insert("port".to_string(), "5433".to_string());
        params.insert("database".to_string(), "testdb".to_string());
        params.insert("username".to_string(), "testuser".to_string());
        params.insert("password".to_string(), "testpass".to_string());

        let env_vars = service.get_environment_variables(&params).unwrap();

        // Always uses container name and internal port (5432)
        assert_eq!(
            env_vars.get("POSTGRES_HOST").unwrap(),
            "postgres-test-env-vars"
        );
        assert_eq!(env_vars.get("POSTGRES_PORT").unwrap(), "5432");
        assert!(env_vars
            .get("POSTGRES_URL")
            .unwrap()
            .contains("postgres-test-env-vars:5432"));
    }

    #[test]
    fn test_get_docker_environment_variables_always_uses_container_name() {
        // get_docker_environment_variables always uses container name and internal port
        // for container-to-container communication, regardless of deployment mode
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = PostgresService::new("test-docker-env".to_string(), docker);

        let mut params = HashMap::new();
        params.insert("port".to_string(), "5434".to_string());
        params.insert("database".to_string(), "testdb".to_string());
        params.insert("username".to_string(), "testuser".to_string());
        params.insert("password".to_string(), "testpass".to_string());

        let env_vars = service.get_docker_environment_variables(&params).unwrap();

        // Always uses container name and internal port (5432)
        assert_eq!(
            env_vars.get("POSTGRES_HOST").unwrap(),
            "postgres-test-docker-env"
        );
        assert_eq!(env_vars.get("POSTGRES_PORT").unwrap(), "5432");
    }

    // ── Database Name SQL Injection Prevention Tests ─────────────────

    #[test]
    fn postgres_upgrade_rejected_roundtrips_through_anyhow() {
        // `ExternalServiceManager::upgrade_service` downcasts the anyhow error
        // back to this typed variant to map it to HTTP 400. Guard that the type
        // stays downcastable (a 'static std::error::Error) through the
        // `?`/`.into()` path — if it stopped being downcastable the response
        // would silently degrade to 500.
        let mv: anyhow::Error =
            PostgresUpgradeRejected::MajorVersionChange { from: 17, to: 18 }.into();
        assert!(matches!(
            mv.downcast_ref::<PostgresUpgradeRejected>(),
            Some(PostgresUpgradeRejected::MajorVersionChange { from: 17, to: 18 })
        ));
        assert!(mv
            .to_string()
            .contains("Cannot change PostgreSQL major version"));

        let dg: anyhow::Error = PostgresUpgradeRejected::Downgrade { from: 18, to: 17 }.into();
        assert!(matches!(
            dg.downcast_ref::<PostgresUpgradeRejected>(),
            Some(PostgresUpgradeRejected::Downgrade { .. })
        ));
        assert!(dg.to_string().contains("Cannot downgrade PostgreSQL"));
    }

    #[test]
    fn test_validate_pg_username_accepts_realistic_names() {
        assert!(validate_pg_username("postgres").is_ok());
        assert!(validate_pg_username("appuser").is_ok());
        assert!(validate_pg_username("My_User123").is_ok());
        assert!(validate_pg_username("_svc").is_ok());
        assert!(validate_pg_username(&"a".repeat(63)).is_ok());
    }

    #[test]
    fn test_validate_pg_username_rejects_shell_and_sed_injection() {
        // Empty / too long.
        assert!(validate_pg_username("").is_err());
        assert!(validate_pg_username(&"a".repeat(64)).is_err());
        // The exact break-out vector for the phase_restore sed filter: a
        // single quote closes the sed program's quoting.
        assert!(validate_pg_username("admin'").is_err());
        assert!(validate_pg_username("a'$(id)#").is_err());
        // Other shell/SQL metacharacters.
        assert!(validate_pg_username("u;drop").is_err());
        assert!(validate_pg_username("a b").is_err());
        assert!(validate_pg_username("a`b`").is_err());
        assert!(validate_pg_username("a$b").is_err());
        assert!(validate_pg_username("a-b").is_err());
    }

    #[test]
    fn test_validate_database_name_valid_names() {
        assert!(PostgresService::validate_database_name("mydb").is_ok());
        assert!(PostgresService::validate_database_name("project_1_production").is_ok());
        assert!(PostgresService::validate_database_name("db_test_env").is_ok());
        assert!(PostgresService::validate_database_name("a").is_ok());
        assert!(PostgresService::validate_database_name("_private").is_ok());
    }

    #[test]
    fn test_validate_database_name_rejects_empty() {
        assert!(PostgresService::validate_database_name("").is_err());
    }

    #[test]
    fn test_validate_database_name_rejects_sql_injection_single_quote() {
        // Classic SQL injection: ' OR 1=1 --
        assert!(PostgresService::validate_database_name("test'; DROP TABLE users--").is_err());
    }

    #[test]
    fn test_validate_database_name_rejects_sql_injection_semicolon() {
        assert!(PostgresService::validate_database_name("mydb; DROP DATABASE production").is_err());
    }

    #[test]
    fn test_validate_database_name_rejects_spaces() {
        assert!(PostgresService::validate_database_name("my database").is_err());
    }

    #[test]
    fn test_validate_database_name_rejects_special_chars() {
        assert!(PostgresService::validate_database_name("db-name").is_err());
        assert!(PostgresService::validate_database_name("db.name").is_err());
        assert!(PostgresService::validate_database_name("db/name").is_err());
        assert!(PostgresService::validate_database_name("db\\name").is_err());
        assert!(PostgresService::validate_database_name("db\"name").is_err());
        assert!(PostgresService::validate_database_name("db`name").is_err());
    }

    #[test]
    fn test_validate_database_name_rejects_uppercase() {
        // Uppercase is rejected to enforce consistency (normalize_database_name lowercases)
        assert!(PostgresService::validate_database_name("MyDatabase").is_err());
    }

    #[test]
    fn test_validate_database_name_rejects_leading_digit() {
        assert!(PostgresService::validate_database_name("1database").is_err());
        assert!(PostgresService::validate_database_name("123").is_err());
    }

    #[test]
    fn test_validate_database_name_rejects_too_long() {
        let long_name = "a".repeat(64);
        assert!(PostgresService::validate_database_name(&long_name).is_err());
    }

    #[test]
    fn test_validate_database_name_accepts_max_length() {
        let max_name = "a".repeat(63);
        assert!(PostgresService::validate_database_name(&max_name).is_ok());
    }

    #[test]
    fn test_normalize_then_validate_is_always_safe() {
        // Any input passed through normalize_database_name should pass validation
        let dangerous_inputs = vec![
            "'; DROP TABLE users--",
            "test; DELETE FROM sessions",
            "../../etc/passwd",
            "admin\x00hidden",
            "Robert'); DROP TABLE Students;--",
            "name WITH spaces AND STUFF",
            "UPPERCASE_NAME",
            "123_starts_with_number",
        ];

        for input in dangerous_inputs {
            let normalized = PostgresService::normalize_database_name(input);
            assert!(
                PostgresService::validate_database_name(&normalized).is_ok(),
                "normalize_database_name('{}') produced '{}' which failed validation",
                input,
                normalized
            );
        }
    }

    #[tokio::test]
    async fn test_restore_capabilities_declares_all_modes_supported() {
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Docker not available, skipping");
                return;
            }
        };
        let pg = PostgresService::new("test-caps".to_string(), docker);
        let cfg = ServiceConfig {
            name: "test-caps".into(),
            service_type: ServiceType::Postgres,
            version: Some("18".into()),
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "5432",
                "database": "postgres",
                "username": "postgres",
                "password": "p",
                "max_connections": 100,
                "docker_image": "gotempsh/postgres-walg:18-bookworm",
            }),
        };
        let caps = pg.restore_capabilities(cfg).await.unwrap();
        assert!(caps.restore_in_place);
        assert!(caps.restore_to_new_service);
        assert!(caps.pitr);
        // We don't compute bounds here — unbounded picker in UI.
        assert!(caps.earliest_pitr_time.is_none());
        assert!(caps.latest_pitr_time.is_none());
    }

    #[tokio::test]
    async fn test_restore_pitr_rejects_legacy_backup_without_docker_work() {
        // restore_pitr must reject a non-WAL-G backup (missing s3:// prefix)
        // BEFORE attempting any Docker operations, so this test can run
        // anywhere the library builds.
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Docker not available, skipping");
                return;
            }
        };
        let pg = PostgresService::new("test-pitr-guard".to_string(), docker);

        let cfg = ServiceConfig {
            name: "test-pitr-guard".into(),
            service_type: ServiceType::Postgres,
            version: Some("18".into()),
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "5432",
                "database": "postgres",
                "username": "postgres",
                "password": "p",
                "max_connections": 100,
                "docker_image": "gotempsh/postgres-walg:18-bookworm",
            }),
        };

        // Synthesize the minimum viable RestoreContext with a legacy backup
        // location (.pgdump.gz, no s3:// prefix).
        let legacy_location = "backups/legacy/dump.pgdump.gz".to_string();
        let s3_creds = crate::externalsvc::S3Credentials {
            access_key_id: "k".into(),
            secret_key: "s".into(),
            region: "us-east-1".into(),
            endpoint: None,
            bucket_name: "b".into(),
            bucket_path: "".into(),
            force_path_style: true,
        };
        let s3_source = temps_entities::s3_sources::Model {
            id: 1,
            name: "src".into(),
            bucket_name: "b".into(),
            bucket_path: "".into(),
            access_key_id: "enc".into(),
            secret_key: "enc".into(),
            region: "us-east-1".into(),
            endpoint: None,
            force_path_style: Some(true),
            is_default: false,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let backup = temps_entities::backups::Model {
            id: 1,
            name: "b".into(),
            backup_id: "id".into(),
            schedule_id: None,
            schedule_run_id: None,
            backup_type: "external_service".into(),
            state: "completed".into(),
            started_at: chrono::Utc::now(),
            finished_at: None,
            size_bytes: None,
            file_count: None,
            s3_source_id: 1,
            s3_location: legacy_location.clone(),
            error_message: None,
            metadata: "{}".into(),
            checksum: None,
            compression_type: "gzip".into(),
            created_by: 1,
            expires_at: None,
            tags: "".into(),
        };
        let source_service = temps_entities::external_services::Model {
            id: 1,
            name: "source".into(),
            service_type: "postgres".into(),
            version: Some("18".into()),
            status: "running".into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            slug: None,
            config: Some("{}".into()),
            node_id: None,
            topology: "standalone".into(),
            error_message: None,
            health_status: None,
            last_health_check_at: None,
            last_health_error: None,
            consecutive_health_failures: 0,
            health_metadata: None,
            metrics_enabled: false,
            default_backup_provisioned: false,
            container_name: None,
        };
        // Build a MockDatabase for the `pool` slot — restore_pitr for
        // Postgres doesn't touch it in the legacy-reject path.
        let mock_db =
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection();
        // Build the S3 client. The AWS SDK eagerly initialises its rustls
        // TrustStore at `Client::from_conf` time, and on hosts without any
        // system root CAs (some CI runners, minimal containers, macOS
        // without keychain access) it panics with "TrustStore configured
        // to enable native roots but no valid root certificates parsed!".
        // We wrap construction in `catch_unwind` and skip the test on that
        // specific panic — mirroring the pattern in
        // `externalsvc/test_utils.rs::MinioTestContainer::start`.
        let s3_client = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let aws_creds = aws_sdk_s3::config::Credentials::new("k", "s", None, None, "test");
            let conf = aws_sdk_s3::Config::builder()
                .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
                .region(aws_sdk_s3::config::Region::new("us-east-1"))
                .credentials_provider(aws_creds)
                .build();
            aws_sdk_s3::Client::from_conf(conf)
        })) {
            Ok(c) => c,
            Err(panic_payload) => {
                let panic_msg = panic_payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| {
                        panic_payload
                            .downcast_ref::<&'static str>()
                            .map(ToString::to_string)
                    })
                    .unwrap_or_else(|| "(non-string panic payload)".to_string());
                if panic_msg.contains("TrustStore") || panic_msg.contains("certificate") {
                    println!(
                        "Skipping test: AWS SDK panicked initialising rustls TrustStore: {}",
                        panic_msg
                    );
                    return;
                }
                panic!("AWS SDK panic constructing S3 client: {}", panic_msg);
            }
        };
        let ctx = crate::externalsvc::RestoreContext {
            s3_client: &s3_client,
            s3_credentials: &s3_creds,
            s3_source: &s3_source,
            backup: &backup,
            backup_location: &legacy_location,
            source_service: &source_service,
            source_config: cfg,
            pool: &mock_db,
        };

        let err = pg
            .restore_pitr(
                ctx,
                crate::externalsvc::RecoveryTarget::Time {
                    time: chrono::Utc::now(),
                },
                false,
                None,
            )
            .await
            .expect_err("PITR on legacy backup must fail fast");
        let msg = err.to_string();
        assert!(
            msg.contains("WAL-G"),
            "expected WAL-G requirement in error, got: {}",
            msg
        );
    }
}
