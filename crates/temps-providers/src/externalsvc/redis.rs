// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::utils::ensure_network_exists;

use super::{
    ExternalService, HealthProbeResult, ServiceConfig, ServiceResourceLimits, ServiceType,
};
use anyhow::Result;
use async_trait::async_trait;
use bollard::query_parameters::{InspectContainerOptions, StopContainerOptions};
use bollard::Docker;
use flate2::read::GzDecoder;
use redis::{aio::ConnectionManager, AsyncCommands, Client};
use schemars::JsonSchema;
use sea_orm::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};
use urlencoding;

/// Bound on a single Redis backup `docker exec` call. Redis backups are
/// typically small (RDB dumps), so 1 hour is plenty; larger setups can
/// extend this in the future.
const REDIS_BACKUP_EXEC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedisBackupLocationKind {
    Walg,
    RdbGzip,
    Unsupported,
}

pub fn classify_redis_backup_location(location: &str) -> RedisBackupLocationKind {
    if location.starts_with("s3://") {
        RedisBackupLocationKind::Walg
    } else if location.ends_with(".rdb.gz") {
        RedisBackupLocationKind::RdbGzip
    } else {
        RedisBackupLocationKind::Unsupported
    }
}

/// Input configuration for creating a Redis service
/// This is what users provide when creating the service
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(
    title = "Redis Configuration",
    description = "Configuration for Redis service"
)]
pub struct RedisInputConfig {
    /// Redis host address
    #[serde(default = "default_host")]
    #[schemars(example = example_host(), default = "default_host")]
    pub host: String,

    /// Redis port (auto-assigned if not provided)
    #[schemars(example = example_port())]
    pub port: Option<String>,

    /// Redis password (auto-generated if not provided, empty, or less than 8 characters)
    #[serde(default, deserialize_with = "deserialize_optional_password")]
    #[schemars(
        with = "Option<String>",
        example = example_password(),
        description = "Redis password (minimum 8 characters, auto-generated if not provided)"
    )]
    pub password: Option<String>,

    /// Full Docker image reference (e.g., "gotempsh/redis-walg:8-bookworm")
    #[serde(default = "default_docker_image")]
    #[schemars(example = example_docker_image(), default = "default_docker_image")]
    pub docker_image: String,

    /// Real Docker container name when this service was imported from an
    /// existing Redis-compatible container (set by `import_from_container`,
    /// never user-editable — omitted from the create form). Overrides the
    /// derived `redis-{name}` container name so internal addressing targets
    /// the actual pre-existing container instead of a synthesized name that
    /// doesn't exist. Mirrors the MariaDB/Postgres fix for the same class of
    /// bug.
    #[serde(default, deserialize_with = "deserialize_optional_non_empty")]
    #[schemars(skip)]
    pub container_name: Option<String>,
}

/// Internal runtime configuration for Redis service
/// This is what the service uses internally after processing input
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisConfig {
    pub host: String,
    pub port: String,
    pub password: String,
    pub docker_image: String,
    /// Real container name for imported services — see
    /// `RedisInputConfig::container_name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
}

impl From<RedisInputConfig> for RedisConfig {
    fn from(input: RedisInputConfig) -> Self {
        let password = if let Some(ref pwd) = input.password {
            tracing::info!(
                "RedisInputConfig->RedisConfig: using provided password (len={})",
                pwd.len()
            );
            pwd.clone()
        } else {
            let generated = generate_password();
            tracing::warn!(
                "RedisInputConfig->RedisConfig: password was None, generated new password (len={})",
                generated.len()
            );
            generated
        };

        Self {
            host: input.host,
            port: input.port.unwrap_or_else(|| {
                find_available_port(6379)
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "6379".to_string())
            }),
            password,
            docker_image: input.docker_image,
            container_name: input.container_name,
        }
    }
}

const MIN_PASSWORD_LENGTH: usize = 8;

fn deserialize_optional_password<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(match opt {
        Some(s) if !s.is_empty() && s.len() >= MIN_PASSWORD_LENGTH => Some(s),
        Some(s) if !s.is_empty() && s.len() < MIN_PASSWORD_LENGTH => {
            // Password provided but too short - treat as None to trigger auto-generation
            None
        }
        _ => None,
    })
}

/// Treats a blank string the same as an absent value — see
/// `RedisInputConfig::container_name`.
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

fn generate_password() -> String {
    use rand::{distr::Alphanumeric, RngExt};
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(16)
        .map(char::from)
        .collect()
}

// Schema example functions
fn example_host() -> &'static str {
    "localhost"
}

fn example_port() -> &'static str {
    "6379"
}

fn example_password() -> &'static str {
    "your-secure-password"
}

fn default_docker_image() -> String {
    "gotempsh/redis-walg:8-bookworm".to_string()
}

fn example_docker_image() -> &'static str {
    "gotempsh/redis-walg:8-bookworm"
}

use super::port_util::{find_available_port, find_available_port_async, is_port_conflict_error};

pub struct RedisService {
    name: String,
    config: Arc<RwLock<Option<RedisConfig>>>,
    /// Resource limits captured at init time, applied to recreate paths
    /// (start, upgrade) so the container keeps the same constraints.
    resource_limits: Arc<RwLock<ServiceResourceLimits>>,
    docker: Arc<Docker>,
}

impl RedisService {
    pub fn new(name: String, docker: Arc<Docker>) -> Self {
        Self {
            name,
            config: Arc::new(RwLock::new(None)),
            resource_limits: Arc::new(RwLock::new(ServiceResourceLimits::default())),
            docker,
        }
    }

    /// Create a fresh Redis connection
    /// Connection will be automatically closed when ConnectionManager is dropped
    /// This method is public to allow other services (like temps-kv) to get connections
    pub async fn get_connection(&self) -> Result<ConnectionManager> {
        info!("RedisService::get_connection - acquiring config read lock...");
        let config = self
            .config
            .read()
            .await
            .as_ref()
            .ok_or_else(|| {
                error!("RedisService::get_connection - config is None!");
                anyhow::anyhow!("Redis configuration not found")
            })?
            .clone();
        info!(
            "RedisService::get_connection - got config, port={}",
            config.port
        );

        let connection_url = if config.password.is_empty() {
            format!("redis://localhost:{}", config.port)
        } else {
            format!(
                "redis://:{}@localhost:{}",
                urlencoding::encode(&config.password),
                config.port
            )
        };

        info!(
            "RedisService::get_connection - creating client for URL (password masked): redis://...@localhost:{}",
            config.port
        );

        let client = Client::open(connection_url.as_str())
            .map_err(|e| anyhow::anyhow!("Failed to create Redis client: {}", e))?;

        info!("RedisService::get_connection - client created, establishing connection...");

        // Add a timeout to prevent hanging indefinitely
        let conn = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            ConnectionManager::new(client),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Redis connection timed out after 5 seconds"))?
        .map_err(|e| anyhow::anyhow!("Failed to create Redis connection manager: {}", e))?;

        info!("RedisService::get_connection - connection established successfully");

        Ok(conn)
    }

    /// The Docker container this instance owns.
    ///
    /// Public for the same reason as `RustfsService::get_container_name`:
    /// callers reasoning about the container should ask rather than re-derive
    /// `redis-{name}` themselves. See `externalsvc::naming`.
    pub fn get_container_name(&self) -> String {
        format!("redis-{}", self.name)
    }

    /// The container this service actually runs in: the imported container's
    /// real name when `config.container_name` is set, otherwise the derived
    /// `redis-{name}`. Every operation that talks to the live container must
    /// resolve through this, not `get_container_name()` directly, or it
    /// targets a synthesized name that doesn't exist for imported services.
    fn get_live_container_name(&self, config: &RedisConfig) -> String {
        config
            .container_name
            .clone()
            .unwrap_or_else(|| self.get_container_name())
    }

    fn get_effective_address_for_environment(
        &self,
        service_config: ServiceConfig,
        execution_environment: temps_core::ExecutionEnvironment,
    ) -> Result<(String, String)> {
        let config = self.get_redis_config(service_config)?;
        Ok(match execution_environment {
            temps_core::ExecutionEnvironment::Host => ("localhost".to_string(), config.port),
            temps_core::ExecutionEnvironment::Docker => (
                self.get_live_container_name(&config),
                REDIS_INTERNAL_PORT.to_string(),
            ),
        })
    }

    fn get_docker_environment_variables_for_environment(
        &self,
        parameters: &HashMap<String, String>,
        execution_environment: temps_core::ExecutionEnvironment,
    ) -> Result<HashMap<String, String>> {
        let mut env_vars = HashMap::new();
        let port = parameters
            .get("port")
            .ok_or_else(|| anyhow::anyhow!("Missing port parameter"))?;
        let password = parameters.get("password");

        let (effective_host, effective_port) = match execution_environment {
            temps_core::ExecutionEnvironment::Host => ("localhost".to_string(), port.clone()),
            temps_core::ExecutionEnvironment::Docker => (
                parameters
                    .get("container_name")
                    .cloned()
                    .unwrap_or_else(|| self.get_container_name()),
                REDIS_INTERNAL_PORT.to_string(),
            ),
        };

        let url = if let Some(pass) = password {
            format!(
                "redis://:{}@{}:{}",
                urlencoding::encode(pass),
                effective_host,
                effective_port
            )
        } else {
            format!("redis://{}:{}", effective_host, effective_port)
        };

        env_vars.insert("REDIS_URL".to_string(), url);
        env_vars.insert("REDIS_HOST".to_string(), effective_host);
        env_vars.insert("REDIS_PORT".to_string(), effective_port);
        if let Some(pass) = password {
            env_vars.insert("REDIS_PASSWORD".to_string(), pass.clone());
        }

        Ok(env_vars)
    }

    /// Creates and starts the Redis container, retrying with a fresh host
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
        config: &mut RedisConfig,
        password: &str,
        resource_limits: &ServiceResourceLimits,
    ) -> Result<()> {
        const MAX_ATTEMPTS: u32 = 3;
        let mut attempt_config = config.clone();
        for attempt in 1..=MAX_ATTEMPTS {
            match self
                .create_container_once(docker, &attempt_config, password, resource_limits)
                .await
            {
                Ok(()) => {
                    *config = attempt_config;
                    return Ok(());
                }
                Err(e) if attempt < MAX_ATTEMPTS && is_port_conflict_error(&e.to_string()) => {
                    warn!(
                        "Port {} for Redis container was already allocated (attempt {}/{}), retrying with a fresh port: {}",
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
                    let base_port: u16 = attempt_config.port.parse().unwrap_or(6379);
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
        config: &RedisConfig,
        password: &str,
        resource_limits: &ServiceResourceLimits,
    ) -> Result<()> {
        let container_name = self.get_container_name();

        // Use the docker_image from config
        info!("Pulling Redis image {}", config.docker_image);

        crate::utils::pull_image_with_retry(docker, &config.docker_image, None)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to pull Redis image: {}", e))?;

        // Check if container already exists and remove it
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

        if !containers.is_empty() {
            // Check if we need to recreate with a new image
            let existing_image = containers
                .first()
                .and_then(|c| c.image.as_deref())
                .unwrap_or("");

            if existing_image == config.docker_image {
                info!(
                    "Container {} already exists with same image",
                    container_name
                );
                return Ok(());
            }

            info!(
                "Container {} already exists with different image (current: {}, requested: {}), removing it to recreate",
                container_name, existing_image, config.docker_image
            );

            // Stop the container first
            let _ = docker
                .stop_container(
                    &container_name,
                    None::<bollard::query_parameters::StopContainerOptions>,
                )
                .await;

            // Remove the container
            docker
                .remove_container(
                    &container_name,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        v: true,
                        ..Default::default()
                    }),
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to remove existing container: {}", e))?;

            info!("Removed existing container {}", container_name);
        }

        let service_label_key = format!("{}service_type", temps_core::DOCKER_LABEL_PREFIX);
        let name_label_key = format!("{}service_name", temps_core::DOCKER_LABEL_PREFIX);

        let container_labels = HashMap::from([
            (service_label_key.as_str(), "redis"),
            (name_label_key.as_str(), self.name.as_str()),
        ]);

        let env_vars = [format!("REDIS_PASSWORD={}", password)];

        // Build Redis server command with password authentication if password is set
        let mut redis_cmd = vec![
            "redis-server".to_string(),
            "--appendonly".to_string(),
            "yes".to_string(),
        ];

        // Add password requirement if password is not empty
        if !password.is_empty() {
            redis_cmd.push("--requirepass".to_string());
            redis_cmd.push(password.to_string());
        }

        let volume_name = format!("redis_data_{}", self.name);
        let mut host_config = bollard::models::HostConfig {
            port_bindings: Some(crate::utils::local_port_binding("6379/tcp", &config.port)),
            mounts: Some(vec![bollard::models::Mount {
                target: Some("/data".to_string()),
                source: Some(volume_name.clone()),
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
            exposed_ports: Some(Vec::from(["6379/tcp".to_string()])),
            env: Some(env_vars.iter().map(|s| s.as_str().to_string()).collect()),
            labels: Some(
                container_labels
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ),
            cmd: Some(redis_cmd),
            host_config: Some(bollard::models::HostConfig {
                restart_policy: Some(bollard::models::RestartPolicy {
                    name: Some(bollard::models::RestartPolicyNameEnum::ALWAYS),
                    maximum_retry_count: None,
                }),
                ..host_config
            }),
            networking_config,
            healthcheck: Some(bollard::models::HealthConfig {
                test: Some(vec!["CMD-SHELL".to_string(), "redis-cli ping".to_string()]),
                interval: Some(1000000000), // 1 second
                timeout: Some(3000000000),  // 3 seconds
                retries: Some(3),
                start_period: Some(5000000000),   // 5 seconds
                start_interval: Some(1000000000), // 1 second
            }),
            ..Default::default()
        };

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
        }

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
            .map_err(|e| anyhow::anyhow!("Failed to create Redis container: {:?}", e))?;

        docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start Redis container: {:?}", e))?;

        // Wait for container to be healthy
        self.wait_for_container_health(docker, &container.id)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to wait for Redis container health: {:?}", e))?;

        info!("Redis container {} created and started", container.id);
        Ok(())
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
                // Considered ready if it's running and either has a HEALTHY
                // Docker healthcheck status or no healthcheck is defined at
                // all (e.g. an imported container built from a vanilla image
                // with no HEALTHCHECK directive — requiring an explicit
                // HEALTHY here would spin until `max_wait` every time).
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
                        "Redis container exited unexpectedly with code {}",
                        exit_code
                    ));
                }
            }
            sleep(delay).await;
            total_wait += delay;
            delay = std::cmp::min(delay.mul_f32(1.5), max_delay);
        }

        Err(anyhow::anyhow!("Redis container health check timed out"))
    }

    fn resource_mapping_key(resource_name: &str) -> String {
        format!("_temps:redis_db_mapping:{}", resource_name)
    }

    fn database_owner_key(db_number: u8) -> String {
        format!("_temps:redis_db_owner:{}", db_number)
    }

    /// Owner value recorded for a logical DB that already held data when we
    /// first looked at it. Such a DB predates this allocation scheme (it was
    /// picked by the old hash-of-resource-name mapping, or was written to by
    /// hand), so its contents belong to someone we cannot identify. Reserving
    /// it under a sentinel owner keeps it out of every future allocation
    /// without ever attributing it to a resource — which also means
    /// `drop_database` can never be led to `FLUSHDB` it.
    const UNMANAGED_DB_OWNER: &'static str = "_temps:unmanaged";

    /// `DBSIZE` for one logical database, leaving the connection back on the
    /// metadata DB 0 that the caller is working in.
    async fn database_key_count(conn: &mut ConnectionManager, db_number: u8) -> Result<u64> {
        redis::cmd("SELECT")
            .arg(db_number)
            .query_async::<()>(conn)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to select Redis DB {}: {}", db_number, e))?;

        let size = redis::cmd("DBSIZE").query_async::<u64>(conn).await;

        // Return to DB 0 whatever DBSIZE did — leaving the connection pointed
        // at a workload DB would make every later metadata write land in it.
        redis::cmd("SELECT")
            .arg(0)
            .query_async::<()>(conn)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to return to Redis metadata DB 0: {}", e))?;

        size.map_err(|e| anyhow::anyhow!("Failed to read DBSIZE for Redis DB {}: {}", db_number, e))
    }

    /// Allocate a Redis logical database for a project/environment resource.
    ///
    /// DB 0 is reserved for Temps allocation metadata. Workload databases are
    /// selected from DBs 1-15 and recorded in Redis before their connection
    /// details are returned, so two resources cannot silently receive the same
    /// logical DB. When all databases are in use, allocation fails closed.
    ///
    /// The "read mapping, then claim" sequence below isn't a single atomic
    /// transaction, so a concurrent `drop_database` for the same resource
    /// could in principle interleave between the mapping read and the
    /// `SETNX` claim. This is an accepted race: a single Redis instance and
    /// the short critical section make it low-risk in practice, and
    /// allocate/drop for the same resource aren't expected to run
    /// concurrently (provision and deprovision are serialized per resource
    /// at the caller).
    async fn allocate_database(&self, resource_name: &str) -> Result<u8> {
        let mut conn = self.get_connection().await?;
        redis::cmd("SELECT")
            .arg(0)
            .query_async::<()>(&mut conn)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to select Redis metadata DB 0: {}", e))?;

        let mapping_key = Self::resource_mapping_key(resource_name);
        let existing: Option<u8> = conn.get(&mapping_key).await.map_err(|e| {
            anyhow::anyhow!(
                "Failed to read Redis DB mapping for resource '{}': {}",
                resource_name,
                e
            )
        })?;
        if let Some(db_number) = existing {
            return Ok(db_number);
        }

        for db_number in 1..=15 {
            let owner_key = Self::database_owner_key(db_number);
            let claimed: bool = conn.set_nx(&owner_key, resource_name).await.map_err(|e| {
                anyhow::anyhow!(
                    "Failed to claim Redis DB {} for resource '{}': {}",
                    db_number,
                    resource_name,
                    e
                )
            })?;

            if claimed {
                // The owner key says the DB is free, but this Redis may have
                // been provisioned before ownership was tracked at all — under
                // the old hash-of-resource-name scheme a workload could be
                // sitting in any of DBs 1-15 with no metadata to show for it.
                // Handing such a DB out would expose that data to the new
                // resource and let `drop_database` FLUSHDB it later, so an
                // occupied-but-unowned DB is reserved and skipped instead.
                // The claim is already committed at this point, but the mapping
                // that makes it releasable is not written until below. A `?`
                // here would therefore leave the owner key set with no mapping,
                // and `drop_database` skips exactly that shape ("no mapping
                // found"), so a transient SELECT/DBSIZE failure would burn one
                // of the 15 logical DBs permanently — fifteen such blips and
                // every future allocation fails. Release the claim before
                // propagating so the error costs nothing but this attempt.
                let key_count = match Self::database_key_count(&mut conn, db_number).await {
                    Ok(count) => count,
                    Err(probe_error) => {
                        if let Err(release_error) =
                            conn.del::<_, ()>(&owner_key).await.map_err(|e| {
                                anyhow::anyhow!(
                                    "Failed to release Redis DB {} after a failed occupancy \
                                     probe: {}",
                                    db_number,
                                    e
                                )
                            })
                        {
                            // Both failed: say so, rather than reporting only
                            // the probe error and leaving the leak unexplained.
                            warn!(
                                db_number,
                                resource = resource_name,
                                %release_error,
                                "Could not release the Redis DB claim after a failed occupancy \
                                 probe; this logical DB may stay reserved until cleared manually"
                            );
                        }
                        return Err(probe_error);
                    }
                };
                if key_count > 0 {
                    warn!(
                        db_number,
                        key_count,
                        resource = resource_name,
                        "Redis DB holds data but has no recorded owner; reserving it as \
                         unmanaged rather than allocating it"
                    );
                    conn.set::<_, _, ()>(&owner_key, Self::UNMANAGED_DB_OWNER)
                        .await
                        .map_err(|e| {
                            anyhow::anyhow!(
                                "Failed to reserve unmanaged Redis DB {}: {}",
                                db_number,
                                e
                            )
                        })?;
                    continue;
                }

                conn.set::<_, _, ()>(&mapping_key, db_number)
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "Failed to store Redis DB {} mapping for resource '{}': {}",
                            db_number,
                            resource_name,
                            e
                        )
                    })?;
                return Ok(db_number);
            }

            let owner: Option<String> = conn.get(&owner_key).await.map_err(|e| {
                anyhow::anyhow!(
                    "Failed to read Redis DB {} owner while allocating resource '{}': {}",
                    db_number,
                    resource_name,
                    e
                )
            })?;
            if owner.as_deref() == Some(resource_name) {
                conn.set::<_, _, ()>(&mapping_key, db_number)
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "Failed to restore Redis DB {} mapping for resource '{}': {}",
                            db_number,
                            resource_name,
                            e
                        )
                    })?;
                return Ok(db_number);
            }
        }

        Err(anyhow::anyhow!(
            "No Redis logical databases are available for resource '{}'; DB 0 is reserved for metadata and DBs 1-15 are already allocated or hold unattributable data",
            resource_name
        ))
    }

    async fn drop_database(&self, resource_name: &str) -> Result<()> {
        let mut conn = self.get_connection().await?;
        redis::cmd("SELECT")
            .arg(0)
            .query_async::<()>(&mut conn)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to select Redis metadata DB 0: {}", e))?;

        let mapping_key = Self::resource_mapping_key(resource_name);
        let db_number: Option<u8> = conn.get(&mapping_key).await.map_err(|e| {
            anyhow::anyhow!(
                "Failed to read Redis DB mapping for resource '{}': {}",
                resource_name,
                e
            )
        })?;

        let Some(db_number) = db_number else {
            info!(
                "No Redis database mapping found for resource '{}'; skipping deprovision",
                resource_name
            );
            return Ok(());
        };

        redis::cmd("SELECT")
            .arg(db_number)
            .query_async::<()>(&mut conn)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to select Redis DB {} for resource '{}': {}",
                    db_number,
                    resource_name,
                    e
                )
            })?;
        redis::cmd("FLUSHDB")
            .query_async::<()>(&mut conn)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to flush Redis DB {} for resource '{}': {}",
                    db_number,
                    resource_name,
                    e
                )
            })?;

        redis::cmd("SELECT")
            .arg(0)
            .query_async::<()>(&mut conn)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to reselect Redis metadata DB 0: {}", e))?;
        conn.del::<_, ()>(&mapping_key).await.map_err(|e| {
            anyhow::anyhow!(
                "Failed to delete Redis DB mapping for resource '{}': {}",
                resource_name,
                e
            )
        })?;
        conn.del::<_, ()>(Self::database_owner_key(db_number))
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to delete Redis DB {} owner for resource '{}': {}",
                    db_number,
                    resource_name,
                    e
                )
            })?;

        info!(
            "Flushed Redis DB {} and removed allocation for resource '{}'",
            db_number, resource_name
        );
        Ok(())
    }

    fn get_redis_config(&self, service_config: ServiceConfig) -> Result<RedisConfig> {
        // Parse input config and transform to runtime config
        let input_config: RedisInputConfig = serde_json::from_value(service_config.parameters)
            .map_err(|e| anyhow::anyhow!("Failed to parse Redis configuration: {}", e))?;

        debug!(
            "get_redis_config - parsed input config: port={:?}, password_provided={}",
            input_config.port,
            input_config.password.is_some()
        );

        let redis_config = RedisConfig::from(input_config);

        debug!(
            "get_redis_config - resulting config: port={}, password_len={}",
            redis_config.port,
            redis_config.password.len()
        );

        Ok(redis_config)
    }

    /// Parse the configuration of an already-running Redis service without
    /// applying create-time defaults. In particular, a missing password means
    /// the live container was created without `--requirepass`; generating a
    /// fresh password during a health check can never authenticate and makes
    /// an operational service look down.
    fn get_redis_probe_config(&self, service_config: ServiceConfig) -> Result<RedisConfig> {
        let parameters = service_config.parameters;
        let string_parameter = |key: &str| {
            parameters
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };

        Ok(RedisConfig {
            host: string_parameter("host").unwrap_or_else(default_host),
            port: string_parameter("port").unwrap_or_else(|| "6379".to_string()),
            password: string_parameter("password").unwrap_or_default(),
            docker_image: string_parameter("docker_image").unwrap_or_else(default_docker_image),
            container_name: string_parameter("container_name").filter(|value| !value.is_empty()),
        })
    }

    /// Verify that a Docker image can be pulled without actually downloading the full image
    /// Attempts to pull the image - fails if it doesn't exist or cannot be accessed
    #[allow(dead_code)]
    async fn verify_image_pullable(&self, image: &str) -> Result<()> {
        info!("Attempting to pull Docker image: {}", image);

        // Try to pull the image - this will fail if it doesn't exist. Retries
        // transient stream errors so a dropped connection isn't mistaken for
        // the image genuinely being unavailable.
        match crate::utils::pull_image_with_retry(&self.docker, image, None).await {
            Ok(()) => {
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
}

impl RedisService {
    /// Build wal-g env and run `wal-g backup-push` via the resilient exec
    /// helper.
    async fn run_walg_backup_push(
        &self,
        container_name: &str,
        walg_s3_prefix: &str,
        s3_credentials: &super::S3Credentials,
        service_config: ServiceConfig,
    ) -> anyhow::Result<()> {
        let redis_password = self
            .get_redis_config(service_config)
            .map(|c| c.password.clone())
            .unwrap_or_default();

        // redis-cli --rdb writes the RDB snapshot to a file. We can't use
        // /dev/stdout directly because redis-cli tries to ftruncate() and
        // fsync() the output file, which fail on /dev/stdout (exit code 1).
        // Instead, write to a temp file and cat it to stdout for WAL-G to
        // capture the stream.
        let stream_create_cmd = if redis_password.is_empty() {
            "redis-cli --rdb /tmp/redis_backup.rdb && cat /tmp/redis_backup.rdb".to_string()
        } else {
            format!(
                "redis-cli -a '{}' --rdb /tmp/redis_backup.rdb && cat /tmp/redis_backup.rdb",
                redis_password
            )
        };

        let mut walg_env: Vec<String> = vec![
            format!("WALG_S3_PREFIX={}", walg_s3_prefix),
            format!("AWS_ACCESS_KEY_ID={}", s3_credentials.access_key_id),
            format!("AWS_SECRET_ACCESS_KEY={}", s3_credentials.secret_key),
            format!("AWS_REGION={}", s3_credentials.region),
            format!("WALG_STREAM_CREATE_COMMAND={}", stream_create_cmd),
            "WALG_STREAM_RESTORE_COMMAND=cat > /data/dump.rdb".to_string(),
        ];

        if !redis_password.is_empty() {
            walg_env.push(format!("WALG_REDIS_PASSWORD={}", redis_password));
        }

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
            vec!["sh".into(), "-c".into(), "wal-g backup-push 2>&1".into()],
            Some(walg_env),
            REDIS_BACKUP_EXEC_TIMEOUT,
        )
        .await
        .map(|_| ())
    }

    /// Run a one-shot WAL-G restore helper container that writes the LATEST
    /// backup from `walg_s3_prefix` into the data volume of
    /// `target_container_name` (via `volumes_from`).
    ///
    /// The caller must ensure the target container is STOPPED and its restart
    /// policy is disabled before calling this method, and is responsible for
    /// re-enabling the restart policy and starting the container afterwards.
    ///
    /// Returns Ok(()) when the helper exits with code 0, Err otherwise.
    async fn run_walg_restore_helper(
        &self,
        target_container_name: &str,
        redis_image: &str,
        walg_s3_prefix: &str,
        s3_credentials: &super::S3Credentials,
    ) -> Result<()> {
        // Build WAL-G environment variables for the helper container.
        // WALG_STREAM_RESTORE_COMMAND tells WAL-G how to write the restored data.
        let mut walg_env: Vec<String> = vec![
            format!("WALG_S3_PREFIX={}", walg_s3_prefix),
            format!("AWS_ACCESS_KEY_ID={}", s3_credentials.access_key_id),
            format!("AWS_SECRET_ACCESS_KEY={}", s3_credentials.secret_key),
            format!("AWS_REGION={}", s3_credentials.region),
            // WALG_STREAM_CREATE_COMMAND is required even for fetch (WAL-G validates it)
            "WALG_STREAM_CREATE_COMMAND=echo noop".to_string(),
            "WALG_STREAM_RESTORE_COMMAND=cat > /data/dump.rdb".to_string(),
        ];

        // Resolve S3 endpoint for use inside the Docker container.
        if let Some(resolved_endpoint) = s3_credentials
            .resolve_endpoint_for_container(&self.docker, target_container_name)
            .await
        {
            walg_env.push(format!("AWS_ENDPOINT={}", resolved_endpoint));
        }
        if s3_credentials.force_path_style {
            walg_env.push("AWS_S3_FORCE_PATH_STYLE=true".to_string());
        }

        // The helper runs WAL-G fetch (which writes dump.rdb) and then replaces
        // the AOF base file with the restored RDB. Redis 7+ with --appendonly yes
        // loads from the multi-part AOF in appendonlydir/ (base RDB + incremental
        // AOF files). If we just delete appendonlydir, Redis recreates an EMPTY
        // one on startup and ignores dump.rdb.
        //
        // Fix: After fetching the backup to dump.rdb, we:
        // 1. Remove the old appendonlydir contents
        // 2. Create a fresh appendonlydir with our dump.rdb as the base RDB
        // 3. Write a manifest that points to our base RDB only (no incremental files)
        //
        // This way Redis loads our restored data through its normal AOF loading path.
        let restore_script = concat!(
            "wal-g backup-fetch LATEST 2>&1 && ",
            "rm -rf /data/appendonlydir && ",
            "mkdir -p /data/appendonlydir && ",
            "cp /data/dump.rdb /data/appendonlydir/appendonly.aof.1.base.rdb && ",
            "printf 'file appendonly.aof.1.base.rdb seq 1 type b\\n' > /data/appendonlydir/appendonly.aof.manifest && ",
            "chown -R redis:redis /data/appendonlydir && ",
            "echo 'Restore helper completed successfully'"
        );

        // Join the same app network the original Redis container uses (see
        // `create_container_once`/`ensure_network_exists`). Without this the
        // helper only gets Docker's default bridge network, so the S3
        // endpoint we just resolved via `resolve_endpoint_for_container`
        // (relative to the original container's network) is unreachable
        // from inside it.
        ensure_network_exists(&self.docker)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to ensure network exists: {:?}", e))?;

        // Include a random suffix so two concurrent restores of the same service
        // don't collide on `create_container` with an opaque "name already in use"
        // error. `restore_from_legacy` uses the same pattern.
        let helper_suffix = uuid::Uuid::new_v4()
            .to_string()
            .split('-')
            .next()
            .unwrap_or("rr")
            .to_string();
        let helper_name = format!("{}-restore-helper-{}", target_container_name, helper_suffix);
        use bollard::models::{ContainerCreateBody, HostConfig};
        let helper_config = ContainerCreateBody {
            image: Some(redis_image.to_string()),
            cmd: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                restore_script.to_string(),
            ]),
            env: Some(walg_env),
            host_config: Some(HostConfig {
                volumes_from: Some(vec![target_container_name.to_string()]),
                ..Default::default()
            }),
            networking_config: Some(bollard::models::NetworkingConfig {
                endpoints_config: Some(HashMap::from([(
                    temps_core::NETWORK_NAME.to_string(),
                    bollard::models::EndpointSettings::default(),
                )])),
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

        // If `start_container` fails, the created container persists in Docker
        // with its S3 credentials visible via `docker inspect`. Always force-remove
        // the container on this path to avoid credential leaks.
        if let Err(e) = self
            .docker
            .start_container(
                &helper.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
        {
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
            return Err(anyhow::anyhow!(
                "Failed to start restore helper container: {}",
                e
            ));
        }

        // Wait for the helper to finish — bounded to avoid an indefinitely stuck
        // helper blocking the restore run forever.
        use futures::StreamExt;
        let wait_result = match tokio::time::timeout(
            REDIS_BACKUP_EXEC_TIMEOUT,
            self.docker
                .wait_container(
                    &helper.id,
                    None::<bollard::query_parameters::WaitContainerOptions>,
                )
                .next(),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => {
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
                return Err(anyhow::anyhow!(
                    "WAL-G backup-fetch helper for container '{}' did not exit within {:?}",
                    target_container_name,
                    REDIS_BACKUP_EXEC_TIMEOUT
                ));
            }
        };

        // Capture helper logs before cleanup for diagnostics.
        let log_output = {
            use bollard::query_parameters::LogsOptions;
            let mut log_stream = self.docker.logs(
                &helper.id,
                Some(LogsOptions {
                    stdout: true,
                    stderr: true,
                    follow: false,
                    ..Default::default()
                }),
            );
            let mut logs = String::new();
            while let Some(Ok(chunk)) = log_stream.next().await {
                logs.push_str(&chunk.to_string());
            }
            logs
        };

        if log_output.is_empty() {
            info!(
                "WAL-G restore helper produced no output for '{}'",
                target_container_name
            );
        } else {
            info!(
                "WAL-G restore helper logs for '{}': {}",
                target_container_name,
                log_output.trim()
            );
        }

        // Clean up the helper container.
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
                    "WAL-G backup-fetch helper exited with code {} for container '{}'. Logs: {}",
                    wait_response.status_code,
                    target_container_name,
                    log_output.trim()
                ));
            }
        }

        Ok(())
    }

    /// Restore from a WAL-G backup stored in S3.
    ///
    /// WAL-G restore requires stopping Redis, fetching the backup (which writes
    /// dump.rdb via WALG_STREAM_RESTORE_COMMAND), and restarting.
    async fn restore_from_walg(
        &self,
        s3_credentials: &super::S3Credentials,
        walg_s3_prefix: &str,
    ) -> Result<()> {
        let container_name = self
            .config
            .read()
            .await
            .as_ref()
            .map(|config| self.get_live_container_name(config))
            .unwrap_or_else(|| self.get_container_name());

        info!(
            "Restoring Redis from WAL-G backup (prefix: {}) in container '{}'",
            walg_s3_prefix, container_name
        );

        // Get the Redis image from the running container for the helper.
        let container_info = self
            .docker
            .inspect_container(
                &container_name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await?;
        let redis_image = container_info
            .config
            .as_ref()
            .and_then(|c| c.image.clone())
            .unwrap_or_else(|| "gotempsh/redis-walg:8-bookworm".to_string());

        // Step 1: Disable the restart policy and stop the container so it releases
        // the data volume exclusively to the restore helper.
        info!("Disabling restart policy and stopping Redis container for restore");
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
            .stop_container(&container_name, None::<StopContainerOptions>)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to stop Redis container for restore: {}", e))?;

        // Step 2: Run the WAL-G restore helper.
        let restore_result = self
            .run_walg_restore_helper(
                &container_name,
                &redis_image,
                walg_s3_prefix,
                s3_credentials,
            )
            .await;

        // Step 3: Re-enable restart policy and start the original Redis container.
        // Redis will load the restored dump.rdb on startup.
        //
        // Re-enable restart policy regardless of the restore outcome — a transient
        // Docker API error here must NOT propagate via `?` and strand the container
        // with restart_policy=NO (which prevents auto-recovery from crashes or
        // daemon restarts). Both sibling functions (`restore_from_legacy`,
        // `restore_to_new_service`) use `let _ =` here for the same reason.
        info!("Starting Redis with restored data");
        let _ = self
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
            .await;

        restore_result?;

        self.docker
            .start_container(
                &container_name,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start Redis after restore: {}", e))?;

        // Wait for container to be healthy.
        self.wait_for_container_health(&self.docker, &container_name)
            .await?;

        info!("Redis WAL-G restore completed successfully");
        Ok(())
    }

    /// Restore from the current RedisEngine backup format: a gzip-compressed
    /// RDB snapshot (`.rdb.gz`) stored on S3.
    ///
    /// The previous implementation mistakenly treated the gzip as a tar
    /// archive, so `tar::Archive::new()` failed immediately with "failed to
    /// iterate over archive". This version:
    ///
    /// 1. Downloads and gzip-decodes the backup to a host temp file.
    /// 2. Disables the container's restart policy then stops it (prevents
    ///    Docker from auto-restarting before the helper can write the volume).
    /// 3. Runs a short-lived helper container with `volumes_from` on the
    ///    stopped container. The helper copies the RDB, rebuilds the Redis 7+
    ///    multi-part AOF directory (`appendonlydir/`) with a manifest pointing
    ///    to the RDB as the base, and chowns everything to `redis:redis`.
    ///    A bare `dump.rdb` is ignored on startup by Redis 7 when AOF is
    ///    enabled; the manifest-based directory is required.
    /// 4. Re-enables the restart policy (always) regardless of outcome.
    /// 5. Starts the container and waits for the healthcheck.
    async fn restore_from_legacy(
        &self,
        s3_client: &aws_sdk_s3::Client,
        backup_location: &str,
        s3_source: &temps_entities::s3_sources::Model,
    ) -> Result<()> {
        info!("Restoring Redis from rdb.gz backup: {}", backup_location);

        // ── 1. Download the .rdb.gz from S3 ─────────────────────────────────
        let get_obj = s3_client
            .get_object()
            .bucket(&s3_source.bucket_name)
            .key(backup_location)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("S3 GetObject failed for {}: {}", backup_location, e))?;

        let gz_bytes = get_obj
            .body
            .collect()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read S3 body: {}", e))?
            .to_vec();

        // Decompress the gzip to raw RDB bytes.
        let rdb_bytes = {
            use std::io::Read;
            let mut decoder = GzDecoder::new(gz_bytes.as_slice());
            let mut buf = Vec::new();
            decoder
                .read_to_end(&mut buf)
                .map_err(|e| anyhow::anyhow!("Failed to gunzip Redis backup: {}", e))?;
            buf
        };

        // ── 2. Write RDB to a host temp dir (bind-mounted into the helper) ──
        let temp_dir =
            tempfile::tempdir().map_err(|e| anyhow::anyhow!("Failed to create temp dir: {}", e))?;
        let rdb_host_path = temp_dir.path().join("restore.rdb");
        tokio::fs::write(&rdb_host_path, &rdb_bytes)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to write RDB to temp dir: {}", e))?;
        let temp_dir_str = temp_dir
            .path()
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("temp dir path is not valid UTF-8"))?
            .to_string();

        // ── 3. Resolve the target container name ─────────────────────────────
        let container_name = self
            .config
            .read()
            .await
            .as_ref()
            .map(|config| self.get_live_container_name(config))
            .unwrap_or_else(|| self.get_container_name());

        // ── 4. Inspect the container to find the Redis image ─────────────────
        let inspect = self
            .docker
            .inspect_container(&container_name, None::<InspectContainerOptions>)
            .await
            .map_err(|e| {
                anyhow::anyhow!("Failed to inspect container {}: {}", container_name, e)
            })?;
        let redis_image = inspect
            .config
            .as_ref()
            .and_then(|c| c.image.as_deref())
            .unwrap_or("redis:7-alpine")
            .to_string();

        // ── 5. Disable restart policy then stop the container ─────────────────
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
            .map_err(|e| {
                warn!(
                    "Could not disable restart policy on {}: {}",
                    container_name, e
                )
            })
            .ok();

        self.docker
            .stop_container(&container_name, None::<StopContainerOptions>)
            .await
            .map_err(|e| {
                anyhow::anyhow!("Failed to stop Redis container {}: {}", container_name, e)
            })?;

        // ── 6. Run helper container to write RDB and rebuild AOF directory ───
        // The restore script:
        //   a. Copies the bind-mounted RDB to /data/dump.rdb
        //   b. Removes any stale appendonlydir
        //   c. Creates appendonlydir/ with the RDB as the AOF base file
        //   d. Writes the AOF manifest pointing to the base file
        //   e. Chowns everything to redis:redis so Redis can read on startup
        let restore_script = "cp /restore/restore.rdb /data/dump.rdb && \
             rm -rf /data/appendonlydir && \
             mkdir -p /data/appendonlydir && \
             cp /data/dump.rdb /data/appendonlydir/appendonly.aof.1.base.rdb && \
             printf 'file appendonly.aof.1.base.rdb seq 1 type b\\n' > /data/appendonlydir/appendonly.aof.manifest && \
             chown -R redis:redis /data/dump.rdb /data/appendonlydir && \
             echo 'Legacy restore helper completed successfully'"
            .to_string();

        let helper_id = uuid::Uuid::new_v4()
            .to_string()
            .split('-')
            .next()
            .unwrap_or("rr")
            .to_string();
        let helper_name = format!("temps-redis-rdb-restore-{}", helper_id);

        use bollard::models::{ContainerCreateBody, HostConfig};
        let helper_config = ContainerCreateBody {
            image: Some(redis_image.clone()),
            cmd: Some(vec!["sh".to_string(), "-c".to_string(), restore_script]),
            user: Some("root".to_string()),
            host_config: Some(HostConfig {
                volumes_from: Some(vec![container_name.clone()]),
                binds: Some(vec![format!("{}:/restore:ro", temp_dir_str)]),
                network_mode: Some("none".to_string()),
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
            .map_err(|e| anyhow::anyhow!("Failed to create rdb restore helper container: {}", e))?;

        self.docker
            .start_container(
                &helper.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start rdb restore helper container: {}", e))?;

        // Wait for the helper to finish.
        use futures::StreamExt;
        let wait_result = match tokio::time::timeout(
            REDIS_BACKUP_EXEC_TIMEOUT,
            self.docker
                .wait_container(
                    &helper.id,
                    None::<bollard::query_parameters::WaitContainerOptions>,
                )
                .next(),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => {
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
                // Re-enable restart policy even on timeout.
                let _ = self
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
                    .await;
                return Err(anyhow::anyhow!(
                    "RDB restore helper for container '{}' did not exit within {:?}",
                    container_name,
                    REDIS_BACKUP_EXEC_TIMEOUT
                ));
            }
        };

        // Capture helper logs before cleanup.
        let log_output = {
            use bollard::query_parameters::LogsOptions;
            let mut log_stream = self.docker.logs(
                &helper.id,
                Some(LogsOptions {
                    stdout: true,
                    stderr: true,
                    follow: false,
                    ..Default::default()
                }),
            );
            let mut logs = String::new();
            while let Some(Ok(chunk)) = log_stream.next().await {
                logs.push_str(&chunk.to_string());
            }
            logs
        };

        // Clean up the helper container.
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

        let restore_result = if let Some(Ok(wait_response)) = wait_result {
            if wait_response.status_code != 0 {
                Err(anyhow::anyhow!(
                    "RDB restore helper exited with code {} for container '{}'. Logs: {}",
                    wait_response.status_code,
                    container_name,
                    log_output.trim()
                ))
            } else {
                info!(
                    "RDB restore helper logs for '{}': {}",
                    container_name,
                    log_output.trim()
                );
                Ok(())
            }
        } else {
            Err(anyhow::anyhow!(
                "RDB restore helper for '{}' produced no wait status. Logs: {}",
                container_name,
                log_output.trim()
            ))
        };

        // ── 7. Re-enable restart policy (always, even on error) ──────────────
        let _ = self
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
            .await;

        // Propagate restore failure after re-enabling restart policy.
        restore_result?;

        // ── 8. Start the container and wait for it to be healthy ─────────────
        self.docker
            .start_container(
                &container_name,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start Redis container after restore: {}", e))?;

        self.wait_for_container_health(&self.docker, &container_name)
            .await?;

        info!("Redis rdb.gz restore completed successfully");
        Ok(())
    }

    /// Check if the WAL-G binary is available inside a container.
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

    /// Legacy Redis backup using BGSAVE + file copy.
    /// Fallback for containers without WAL-G (e.g., `redis:8-alpine`).
    async fn backup_to_s3_legacy(
        &self,
        s3_client: &aws_sdk_s3::Client,
        backup: temps_entities::backups::Model,
        s3_source: &temps_entities::s3_sources::Model,
        subpath: &str,
        pool: &temps_database::DbConnection,
        external_service: &temps_entities::external_services::Model,
    ) -> Result<super::BackupOutcome> {
        use chrono::Utc;
        use sea_orm::*;
        use std::io::Write;

        info!("Starting Redis backup to S3 via legacy BGSAVE");

        let backup_record = temps_entities::external_service_backups::Entity::insert(
            temps_entities::external_service_backups::ActiveModel {
                service_id: Set(external_service.id),
                backup_id: Set(backup.id),
                backup_type: Set("full".to_string()),
                state: Set("running".to_string()),
                started_at: Set(Utc::now()),
                s3_location: Set("".to_string()),
                metadata: Set(serde_json::json!({
                    "service_type": "redis",
                    "service_name": self.name,
                    "backup_tool": "bgsave",
                })),
                compression_type: Set("none".to_string()),
                created_by: Set(0),
                ..Default::default()
            },
        )
        .exec_with_returning(pool)
        .await?;

        let container_name = self
            .config
            .read()
            .await
            .as_ref()
            .map(|config| self.get_live_container_name(config))
            .unwrap_or_else(|| self.get_container_name());
        let temp_dir = tempfile::tempdir()?;
        let temp_path = temp_dir.path();

        // Execute BGSAVE
        self.docker
            .create_exec(
                &container_name,
                bollard::exec::CreateExecOptions {
                    cmd: Some(vec!["redis-cli", "BGSAVE"]),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await?;

        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        // Copy dump.rdb and appendonly.aof from container
        for file in &["dump.rdb", "appendonly.aof"] {
            let cat_exec = self
                .docker
                .create_exec(
                    &container_name,
                    bollard::exec::CreateExecOptions {
                        cmd: Some(vec!["cat", &format!("/data/{}", file)]),
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        ..Default::default()
                    },
                )
                .await?;

            let file_path = temp_path.join(file);
            let mut temp_file = std::fs::File::create(&file_path)?;

            let output = self.docker.start_exec(&cat_exec.id, None).await?;
            if let bollard::exec::StartExecResults::Attached { output, .. } = output {
                use futures::stream::StreamExt;
                let mut stream = output.boxed();
                while let Some(result) = stream.next().await {
                    match result {
                        Ok(log_output) => match log_output {
                            bollard::container::LogOutput::StdOut { message }
                            | bollard::container::LogOutput::StdErr { message } => {
                                temp_file.write_all(&message)?;
                            }
                            _ => (),
                        },
                        Err(e) => {
                            error!("Error streaming backup data for {}: {}", file, e);
                            let mut backup_update:
                                temps_entities::external_service_backups::ActiveModel =
                                backup_record.clone().into();
                            backup_update.state = Set("failed".to_string());
                            backup_update.error_message = Set(Some(e.to_string()));
                            backup_update.finished_at = Set(Some(Utc::now()));
                            backup_update.update(pool).await?;
                            return Err(anyhow::anyhow!("Failed to stream backup data: {}", e));
                        }
                    }
                }
            }
        }

        // Create tar archive
        let tar_path = temp_path.join("redis_backup.tar");
        let tar_file = std::fs::File::create(&tar_path)?;
        let mut tar_builder = tar::Builder::new(tar_file);
        for file in &["dump.rdb", "appendonly.aof"] {
            let file_path = temp_path.join(file);
            tar_builder.append_path_with_name(&file_path, file)?;
        }
        tar_builder.finish()?;

        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let backup_key = format!(
            "{}/redis_backup_{}.tar",
            subpath.trim_matches('/'),
            timestamp
        );

        let size_bytes = std::fs::metadata(&tar_path)?.len() as i64;

        if size_bytes == 0 {
            let mut backup_update: temps_entities::external_service_backups::ActiveModel =
                backup_record.clone().into();
            backup_update.state = Set("failed".to_string());
            backup_update.finished_at = Set(Some(Utc::now()));
            backup_update.error_message =
                Set(Some("Backup failed: backup file has zero size".to_string()));
            backup_update.update(pool).await?;
            return Err(anyhow::anyhow!(
                "Redis backup failed: backup file has zero size"
            ));
        }

        s3_client
            .put_object()
            .bucket(&s3_source.bucket_name)
            .key(&backup_key)
            .body(aws_sdk_s3::primitives::ByteStream::from_path(&tar_path).await?)
            .content_type("application/x-tar")
            .send()
            .await?;

        let mut backup_update: temps_entities::external_service_backups::ActiveModel =
            backup_record.clone().into();
        backup_update.state = Set("completed".to_string());
        backup_update.finished_at = Set(Some(Utc::now()));
        backup_update.size_bytes = Set(Some(size_bytes));
        backup_update.s3_location = Set(backup_key.clone());
        backup_update.update(pool).await?;

        info!("Redis legacy backup completed successfully: {}", backup_key);
        Ok(super::BackupOutcome::new(backup_key, Some(size_bytes)))
    }
}

/// Internal port used by Redis inside the container
const REDIS_INTERNAL_PORT: &str = "6379";

#[async_trait]
impl ExternalService for RedisService {
    fn get_effective_address(&self, service_config: ServiceConfig) -> Result<(String, String)> {
        self.get_effective_address_for_environment(
            service_config,
            temps_core::runtime::execution_environment_compatibility(),
        )
    }

    fn get_docker_container_name(&self) -> String {
        self.get_container_name()
    }

    fn get_docker_internal_port(&self) -> String {
        REDIS_INTERNAL_PORT.to_string()
    }

    async fn init(&self, config: ServiceConfig) -> Result<HashMap<String, String>> {
        info!(
            "Initializing Redis service (name={}, type={:?}, version={:?})",
            config.name, config.service_type, config.version
        );

        // Pull resource limits out of the raw parameters JSON before the
        // typed config consumes it. Defaults to unlimited when no
        // `resources` block is present (legacy services).
        let resource_limits = ServiceResourceLimits::from_parameters(&config.parameters);
        if let Err(e) = resource_limits.validate() {
            return Err(anyhow::anyhow!("Invalid resource limits: {}", e));
        }

        // Parse input config and transform to runtime config
        let mut redis_config = self.get_redis_config(config)?;

        info!(
            "Redis init - storing config: port={}, password_len={}",
            redis_config.port,
            redis_config.password.len()
        );

        // Store runtime config and limits so `start()` recreates correctly.
        // Gets overwritten below once the real container port is known.
        *self.config.write().await = Some(redis_config.clone());
        *self.resource_limits.write().await = resource_limits.clone();

        info!("Redis init - config stored successfully");

        if redis_config.container_name.is_none() {
            // Create Docker container (but don't start it yet)
            // Note: Connection will be established in start() method.
            // `create_container` may retry on a different host port than
            // requested (see its docs); it writes that back into
            // `redis_config`, so everything below reflects the port the
            // container is actually bound to.
            let password = redis_config.password.clone();
            self.create_container(&self.docker, &mut redis_config, &password, &resource_limits)
                .await?;
            *self.config.write().await = Some(redis_config.clone());
            info!("Redis container created, connection will be established on start");
        } else {
            info!(
                "Redis service '{}' is imported from container '{}'; skipping container creation",
                self.name,
                self.get_live_container_name(&redis_config)
            );
        }

        // Serialize the full runtime config to save to database
        // This ensures auto-generated values (password, port) are persisted
        let runtime_config_json = serde_json::to_value(&redis_config)
            .map_err(|e| anyhow::anyhow!("Failed to serialize Redis runtime config: {}", e))?;

        let runtime_config_map = runtime_config_json
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Runtime config is not an object"))
            .map_err(|e| anyhow::anyhow!("Runtime config is not an object: {}", e))?;

        let mut inferred_params = HashMap::new();
        for (key, value) in runtime_config_map {
            if let Some(str_value) = value.as_str() {
                inferred_params.insert(key.clone(), str_value.to_string());
            }
        }

        Ok(inferred_params)
    }

    fn get_local_address(&self, service_config: ServiceConfig) -> Result<String> {
        let config = self.get_redis_config(service_config)?;
        Ok(format!("localhost:{}", config.port))
    }

    async fn health_check(&self) -> Result<bool> {
        let conn = self.get_connection().await?;
        let result: Result<String, redis::RedisError> =
            redis::cmd("PING").query_async(&mut conn.clone()).await;
        Ok(result.is_ok())
    }

    async fn health_probe(&self, service_config: ServiceConfig) -> Result<HealthProbeResult> {
        use std::time::{Duration, Instant};

        const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
        const DEGRADED_MS: u128 = 2000;

        let cfg = match self.get_redis_probe_config(service_config) {
            Ok(c) => c,
            Err(e) => {
                return Ok(HealthProbeResult::down(format!(
                    "invalid redis config: {}",
                    e
                )))
            }
        };

        let url = if cfg.password.is_empty() {
            format!("redis://{}:{}", cfg.host, cfg.port)
        } else {
            format!(
                "redis://:{}@{}:{}",
                urlencoding::encode(&cfg.password),
                cfg.host,
                cfg.port
            )
        };

        let start = Instant::now();

        // `get_multiplexed_async_connection()` spawns a background pump task
        // that owns the socket and lives until every connection handle is
        // dropped. We open one per 30s health cycle, so the only safe pattern
        // is to keep the single `conn` we create bound in this scope and let it
        // drop here (which signals the pump to exit). The hazard is the outer
        // `timeout`: if it fires while the connect future is still in flight,
        // the future is cancelled and any half-established connection + pump
        // task is orphaned. Binding `conn` and only ever cancelling the
        // connect — never a live, returned connection — keeps teardown
        // deterministic. (Redis 0.28 `MultiplexedConnection` has no explicit
        // close; drop is the documented teardown.)
        let probe = async {
            let client = Client::open(url.as_str()).map_err(|e| format!("open failed: {}", e))?;
            let mut conn = client
                .get_multiplexed_async_connection()
                .await
                .map_err(|e| format!("connect failed: {}", e))?;
            let reply: String = redis::cmd("PING")
                .query_async(&mut conn)
                .await
                .map_err(|e| format!("PING failed: {}", e))?;
            if reply.to_uppercase() != "PONG" {
                return Err(format!("unexpected PING reply: {}", reply));
            }
            // Drop the connection explicitly before the future resolves so the
            // pump task is signalled to exit within this scope, not later.
            drop(conn);
            Ok::<(), String>(())
        };

        match tokio::time::timeout(PROBE_TIMEOUT, probe).await {
            Err(_) => Ok(HealthProbeResult::down(format!(
                "redis probe to {}:{} timed out after {}s",
                cfg.host,
                cfg.port,
                PROBE_TIMEOUT.as_secs()
            ))),
            Ok(Err(msg)) => Ok(HealthProbeResult::down(format!(
                "redis probe to {}:{} {}",
                cfg.host, cfg.port, msg
            ))),
            Ok(Ok(())) => {
                let elapsed_ms = start.elapsed().as_millis();
                let response_time = i32::try_from(elapsed_ms).ok();
                if elapsed_ms > DEGRADED_MS {
                    Ok(HealthProbeResult::degraded(
                        format!("redis responded in {}ms (>{}ms)", elapsed_ms, DEGRADED_MS),
                        response_time,
                    ))
                } else {
                    Ok(HealthProbeResult::operational(response_time))
                }
            }
        }
    }

    fn get_type(&self) -> ServiceType {
        ServiceType::Redis
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
            Some(cfg) => Ok(format!("redis://localhost:{}", cfg.port)),
            None => Err(anyhow::anyhow!("Redis not configured")),
        }
    }

    async fn cleanup(&self) -> Result<()> {
        // No stored connections to clean up - connections are created on-demand and auto-closed
        Ok(())
    }

    fn get_docker_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>> {
        self.get_docker_environment_variables_for_environment(
            parameters,
            temps_core::runtime::execution_environment_compatibility(),
        )
    }

    fn get_parameter_schema(&self) -> Option<serde_json::Value> {
        // Generate JSON Schema from RedisInputConfig
        let schema = schemars::schema_for!(RedisInputConfig);
        let mut schema_json = serde_json::to_value(schema).ok()?;

        // Add metadata about which fields are editable (based on RedisParameterStrategy::updateable_keys)
        if let Some(properties) = schema_json
            .get_mut("properties")
            .and_then(|p| p.as_object_mut())
        {
            for key in properties.keys().cloned().collect::<Vec<_>>() {
                // Define which fields should be editable - must match RedisParameterStrategy::updateable_keys()
                let editable = match key.as_str() {
                    "host" => false,        // Read-only
                    "port" => true,         // Updateable
                    "password" => false,    // Read-only
                    "docker_image" => true, // Updateable
                    _ => false,
                };

                if let Some(prop) = schema_json["properties"][&key].as_object_mut() {
                    prop.insert("x-editable".to_string(), serde_json::json!(editable));
                }
            }
        }

        Some(schema_json)
    }

    fn get_runtime_env_definitions(&self) -> Vec<super::RuntimeEnvVar> {
        vec![
            super::RuntimeEnvVar {
                name: "REDIS_DATABASE".to_string(),
                description: "Redis database number for this project/environment".to_string(),
                example: "1".to_string(),
                sensitive: false,
            },
            super::RuntimeEnvVar {
                name: "REDIS_URL".to_string(),
                description: "Full Redis URL including database number".to_string(),
                example: "redis://localhost:6379/1".to_string(),
                sensitive: true, // May contain password
            },
        ]
    }
    async fn get_runtime_env_vars(
        &self,
        config: ServiceConfig,
        project_id: &str,
        environment: &str,
    ) -> Result<HashMap<String, String>> {
        let resource_name = format!("{}_{}", project_id, environment);

        let db_number = self.allocate_database(&resource_name).await?;

        let mut env_vars = HashMap::new();

        // Always use container name and internal port for container-to-container
        // communication. An imported service's real container name wins over
        // the derived one.
        let effective_host = config
            .parameters
            .get("container_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.get_container_name());
        let effective_port = REDIS_INTERNAL_PORT.to_string();

        // Database number (specific to this project/environment)
        env_vars.insert("REDIS_DATABASE".to_string(), db_number.to_string());

        // Get password from service config if available (filter out empty strings)
        let password = config
            .parameters
            .get("password")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        // Connection URL with database number
        let url = if let Some(pass) = password {
            format!(
                "redis://:{}@{}:{}/{}",
                urlencoding::encode(pass),
                effective_host,
                effective_port,
                db_number
            )
        } else {
            format!(
                "redis://{}:{}/{}",
                effective_host, effective_port, db_number
            )
        };
        env_vars.insert("REDIS_URL".to_string(), url);

        // Individual connection parameters
        env_vars.insert("REDIS_HOST".to_string(), effective_host);
        env_vars.insert("REDIS_PORT".to_string(), effective_port);
        if let Some(pass) = password {
            env_vars.insert("REDIS_PASSWORD".to_string(), pass.to_string());
        }

        Ok(env_vars)
    }
    async fn start(&self) -> Result<()> {
        let existing_config = self.config.read().await.as_ref().cloned();
        let container_name = existing_config
            .as_ref()
            .map(|config| self.get_live_container_name(config))
            .unwrap_or_else(|| self.get_container_name());
        info!("Starting Redis container {}", container_name);

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
            let mut config =
                existing_config.ok_or_else(|| anyhow::anyhow!("Redis configuration not found"))?;
            if config.container_name.is_some() {
                return Err(anyhow::anyhow!(
                    "Imported Redis container '{}' not found",
                    container_name
                ));
            }
            let limits = self.resource_limits.read().await.clone();
            let password = config.password.clone();
            self.create_container(&self.docker, &mut config, &password, &limits)
                .await?;
            *self.config.write().await = Some(config);
        } else {
            self.docker
                .start_container(
                    &container_name,
                    None::<bollard::query_parameters::StartContainerOptions>,
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to start existing Redis container: {}", e))?;
        }

        self.wait_for_container_health(&self.docker, &container_name)
            .await?;

        // No connection initialization needed - connections are created on-demand when needed
        info!("Redis container started successfully");

        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        // No stored connections to clean up - they are created on-demand

        // Stop the container if Docker is available
        let container_name = self
            .config
            .read()
            .await
            .as_ref()
            .map(|config| self.get_live_container_name(config))
            .unwrap_or_else(|| self.get_container_name());
        info!("Stopping Redis container {}", container_name);

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
                .map_err(|e| anyhow::anyhow!("Failed to stop Redis container: {}", e))?;
        }

        Ok(())
    }

    async fn remove(&self) -> Result<()> {
        // First cleanup any connections
        self.cleanup().await?;

        // Then remove container and volume if Docker is available
        let container_name = self.get_container_name();
        let volume_name = format!("redis_data_{}", self.name);

        info!("Removing Redis container and volume for {}", self.name);

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
                .map_err(|e| anyhow::anyhow!("Failed to stop Redis container: {}", e))?;

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
                .map_err(|e| anyhow::anyhow!("Failed to remove Redis container: {}", e))?;
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

        let password = parameters.get("password");

        // Always use container name and internal port for container-to-container
        // communication. An imported service's real container name wins over
        // the derived one.
        let effective_host = parameters
            .get("container_name")
            .cloned()
            .unwrap_or_else(|| self.get_container_name());
        let effective_port = REDIS_INTERNAL_PORT.to_string();

        let url = if let Some(pass) = password {
            format!(
                "redis://:{}@{}:{}",
                urlencoding::encode(pass),
                effective_host,
                effective_port
            )
        } else {
            format!("redis://{}:{}", effective_host, effective_port)
        };

        env_vars.insert("REDIS_URL".to_string(), url);
        env_vars.insert("REDIS_HOST".to_string(), effective_host);
        env_vars.insert("REDIS_PORT".to_string(), effective_port);
        if let Some(pass) = password {
            env_vars.insert("REDIS_PASSWORD".to_string(), pass.clone());
        }

        Ok(env_vars)
    }

    async fn deprovision_resource(&self, project_id: &str, environment: &str) -> Result<()> {
        let resource_name = format!("{}_{}", project_id, environment);
        self.drop_database(&resource_name).await
    }

    /// Backup Redis data to S3.
    ///
    /// Detects whether the container has WAL-G installed:
    /// - **WAL-G available**: Uses `wal-g backup-push` with stream commands. Zero data
    ///   flows through the Temps process.
    /// - **WAL-G not available** (legacy images like `redis:8-alpine`): Falls back to
    ///   BGSAVE + file copy + tar upload.
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
    ) -> Result<super::BackupOutcome> {
        use chrono::Utc;
        use sea_orm::*;

        let redis_config = self.get_redis_config(service_config.clone())?;
        let container_name = self.get_live_container_name(&redis_config);

        if !self.container_has_walg(&container_name).await {
            info!(
                "WAL-G not found in container '{}', falling back to legacy BGSAVE backup",
                container_name
            );
            return self
                .backup_to_s3_legacy(
                    s3_client,
                    backup,
                    s3_source,
                    subpath,
                    pool,
                    external_service,
                )
                .await;
        }

        info!("Starting Redis backup to S3 via WAL-G");

        let metadata = serde_json::json!({
            "service_type": "redis",
            "service_name": self.name,
            "backup_tool": "wal-g",
        });

        // Create a backup record
        let backup_record = temps_entities::external_service_backups::Entity::insert(
            temps_entities::external_service_backups::ActiveModel {
                service_id: Set(external_service.id),
                backup_id: Set(backup.id),
                backup_type: Set("full".to_string()),
                state: Set("running".to_string()),
                started_at: Set(Utc::now()),
                s3_location: Set("".to_string()),
                metadata: Set(metadata),
                compression_type: Set("lz4".to_string()), // WAL-G uses LZ4 by default
                created_by: Set(0),
                ..Default::default()
            },
        )
        .exec_with_returning(pool)
        .await?;

        // Build the WAL-G S3 prefix using the STABLE subpath_root (no date component).
        // All WAL-G backups must share the same prefix for retention management to work.
        let walg_s3_prefix = format!(
            "s3://{}/{}/walg",
            s3_credentials.bucket_name,
            subpath_root.trim_matches('/')
        );
        let s3_list_prefix = format!("{}/walg/", subpath_root.trim_matches('/'));

        let result = self
            .run_walg_backup_push(
                &container_name,
                &walg_s3_prefix,
                s3_credentials,
                service_config,
            )
            .await;

        match result {
            Ok(()) => {
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
                            "Redis WAL-G backup succeeded but failed to compute size from S3: {}",
                            e
                        );
                        None
                    }
                };

                let mut backup_update: temps_entities::external_service_backups::ActiveModel =
                    backup_record.clone().into();
                backup_update.state = Set("completed".to_string());
                backup_update.finished_at = Set(Some(Utc::now()));
                backup_update.s3_location = Set(walg_s3_prefix.clone());
                backup_update.size_bytes = Set(size_bytes);
                backup_update.update(pool).await?;

                info!(
                    "Redis WAL-G backup completed successfully (prefix: {}, size: {:?})",
                    walg_s3_prefix, size_bytes
                );
                Ok(super::BackupOutcome::new(walg_s3_prefix, size_bytes))
            }
            Err(e) => {
                let error_msg = format!("Redis WAL-G backup failed: {}", e);
                error!("{}", error_msg);
                let mut backup_update: temps_entities::external_service_backups::ActiveModel =
                    backup_record.clone().into();
                backup_update.state = Set("failed".to_string());
                backup_update.error_message = Set(Some(error_msg.clone()));
                backup_update.finished_at = Set(Some(Utc::now()));
                if let Err(update_err) = backup_update.update(pool).await {
                    error!("Failed to mark Redis backup row as failed: {}", update_err);
                }
                Err(e)
            }
        }
    }

    /// Restore Redis data from S3 using WAL-G or legacy format
    ///
    /// For WAL-G backups (s3:// prefix): Runs `wal-g backup-fetch LATEST` inside the container.
    /// WAL-G downloads the backup from S3 and writes the RDB file via WALG_STREAM_RESTORE_COMMAND.
    ///
    /// For legacy backups (.tar files): Falls back to the old approach — downloads from S3,
    /// extracts dump.rdb/appendonly.aof, and copies them into the container.
    async fn restore_from_s3(
        &self,
        s3_client: &aws_sdk_s3::Client,
        s3_credentials: &super::S3Credentials,
        backup_location: &str,
        s3_source: &temps_entities::s3_sources::Model,
        _service_config: ServiceConfig,
    ) -> Result<()> {
        info!("Starting Redis restore from S3: {}", backup_location);

        if backup_location.starts_with("s3://") {
            // WAL-G backup: use wal-g backup-fetch
            self.restore_from_walg(s3_credentials, backup_location)
                .await
        } else {
            // Legacy backup: fall back to old tar-based approach
            self.restore_from_legacy(s3_client, backup_location, s3_source)
                .await
        }
    }

    /// Redis supports in-place restore and restore-to-new-service for WAL-G
    /// backups. PITR is not supported — Redis has no continuous WAL archive
    /// that would allow recovering to an arbitrary point in time.
    async fn restore_capabilities(
        &self,
        _service_config: super::ServiceConfig,
    ) -> Result<super::RestoreCapabilities> {
        Ok(super::RestoreCapabilities {
            restore_in_place: true,
            restore_to_new_service: true,
            pitr: false,
            earliest_pitr_time: None,
            latest_pitr_time: None,
        })
    }

    /// Provision a new Redis service and restore a WAL-G or RDB-gzip backup into it.
    ///
    /// Steps:
    /// 1. Clone the source config, pick a free port (or honour `parameter_overrides`).
    /// 2. Create and start the new Redis container (gets an empty data volume).
    /// 3. Disable restart policy and stop the container.
    /// 4. Run the WAL-G restore helper that writes the backup into the volume.
    /// 5. Re-enable restart policy and start the container.
    /// 6. Wait for the healthcheck to pass.
    /// 7. Return the new service's parameters and connection string.
    async fn restore_to_new_service(
        &self,
        ctx: super::RestoreContext<'_>,
        new_service_name: String,
        parameter_overrides: serde_json::Value,
    ) -> Result<super::NewServiceRestoreResult> {
        info!(
            "Provisioning new Redis service '{}' from backup at {}",
            new_service_name, ctx.backup_location
        );

        let backup_location_kind = classify_redis_backup_location(ctx.backup_location);
        if backup_location_kind == RedisBackupLocationKind::Unsupported {
            return Err(anyhow::anyhow!(
                "Redis backup location '{}' is neither a WAL-G prefix nor a current .rdb.gz object",
                ctx.backup_location
            ));
        }

        // Parse the source config and apply parameter overrides.
        let mut new_redis_config = self.get_redis_config(ctx.source_config)?;

        // Honour explicit port override; otherwise find a free port to avoid
        // colliding with the source service that's still running.
        if let Some(port_str) = parameter_overrides.get("port").and_then(|v| v.as_str()) {
            new_redis_config.port = port_str.to_string();
        } else {
            let base: u16 = new_redis_config.port.parse().map_err(|error| {
                anyhow::anyhow!(
                    "Cannot clone Redis service '{}': source port '{}' is invalid: {}",
                    new_service_name,
                    new_redis_config.port,
                    error
                )
            })?;
            new_redis_config.port = find_available_port(base.wrapping_add(1))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "No free port found for new Redis service '{}' (searched from {}+1)",
                        new_service_name,
                        base
                    )
                })?
                .to_string();
        }

        // Apply docker_image override if requested.
        if let Some(img) = parameter_overrides
            .get("docker_image")
            .and_then(|v| v.as_str())
        {
            new_redis_config.docker_image = img.to_string();
        }

        // This is a brand-new container — clear any imported-container override
        // so the derived `redis-{name}` naming takes effect.
        new_redis_config.container_name = None;

        let new_service = RedisService::new(new_service_name.clone(), self.docker.clone());
        let password = new_redis_config.password.clone();
        let resource_limits = super::super::externalsvc::ServiceResourceLimits::default();

        // `create_container` creates AND starts the container. It writes the
        // final port (which may differ from our pick if there was a race) back
        // into `new_redis_config`.
        new_service
            .create_container(
                &self.docker,
                &mut new_redis_config,
                &password,
                &resource_limits,
            )
            .await?;

        *new_service.config.write().await = Some(new_redis_config.clone());

        let new_container_name = new_service.get_container_name();
        let redis_image = new_redis_config.docker_image.clone();

        if backup_location_kind == RedisBackupLocationKind::RdbGzip {
            if let Err(error) = new_service
                .restore_from_legacy(ctx.s3_client, ctx.backup_location, ctx.s3_source)
                .await
            {
                let _ = self
                    .docker
                    .remove_container(
                        &new_container_name,
                        Some(bollard::query_parameters::RemoveContainerOptions {
                            force: true,
                            v: true,
                            ..Default::default()
                        }),
                    )
                    .await;
                return Err(anyhow::anyhow!(
                    "RDB restore into new Redis container '{}' failed, container removed: {}",
                    new_container_name,
                    error
                ));
            }
        } else {
            // Disable restart policy and stop so the WAL-G helper can write to the volume.
            info!(
                "Disabling restart policy and stopping new container '{}' for restore",
                new_container_name
            );
            self.docker
                .update_container(
                    &new_container_name,
                    bollard::models::ContainerUpdateBody {
                        restart_policy: Some(bollard::models::RestartPolicy {
                            name: Some(bollard::models::RestartPolicyNameEnum::NO),
                            maximum_retry_count: None,
                        }),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to disable restart policy on new container '{}': {}",
                        new_container_name,
                        e
                    )
                })?;

            self.docker
                .stop_container(
                    &new_container_name,
                    None::<bollard::query_parameters::StopContainerOptions>,
                )
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to stop new Redis container '{}' for restore: {}",
                        new_container_name,
                        e
                    )
                })?;

            // Run the WAL-G restore helper into the new container's volume.
            let restore_result = self
                .run_walg_restore_helper(
                    &new_container_name,
                    &redis_image,
                    ctx.backup_location,
                    ctx.s3_credentials,
                )
                .await;

            // Re-enable restart policy regardless of outcome.
            let _ = self
                .docker
                .update_container(
                    &new_container_name,
                    bollard::models::ContainerUpdateBody {
                        restart_policy: Some(bollard::models::RestartPolicy {
                            name: Some(bollard::models::RestartPolicyNameEnum::ALWAYS),
                            maximum_retry_count: None,
                        }),
                        ..Default::default()
                    },
                )
                .await;

            if let Err(e) = restore_result {
                // Clean up the new container since the restore failed.
                let _ = self
                    .docker
                    .remove_container(
                        &new_container_name,
                        Some(bollard::query_parameters::RemoveContainerOptions {
                            force: true,
                            v: true,
                            ..Default::default()
                        }),
                    )
                    .await;
                return Err(anyhow::anyhow!(
                    "WAL-G restore into new Redis container '{}' failed, container removed: {}",
                    new_container_name,
                    e
                ));
            }

            // Start the new container with restored data.
            self.docker
                .start_container(
                    &new_container_name,
                    None::<bollard::query_parameters::StartContainerOptions>,
                )
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to start new Redis container '{}' after restore: {}",
                        new_container_name,
                        e
                    )
                })?;

            // Wait for the healthcheck to pass.
            new_service
                .wait_for_container_health(&self.docker, &new_container_name)
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "New Redis container '{}' did not become healthy after restore: {}",
                        new_container_name,
                        e
                    )
                })?;
        }

        info!(
            "New Redis service '{}' provisioned and restored successfully \
             (container: {}, port: {})",
            new_service_name, new_container_name, new_redis_config.port
        );

        // Serialise the final config so every field is persisted to
        // `external_service_params` by the orchestrator.
        let config_json = serde_json::to_value(&new_redis_config)
            .map_err(|e| anyhow::anyhow!("Failed to serialise new Redis config: {}", e))?;

        let parameters: HashMap<String, String> = config_json
            .as_object()
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        let connection_info = if new_redis_config.password.is_empty() {
            format!("redis://localhost:{}", new_redis_config.port)
        } else {
            format!(
                "redis://:{}@localhost:{}",
                urlencoding::encode(&new_redis_config.password),
                new_redis_config.port
            )
        };

        Ok(super::NewServiceRestoreResult {
            parameters,
            connection_info,
        })
    }

    fn get_default_docker_image(&self) -> (String, String) {
        // Return (image_name, version)
        ("gotempsh/redis-walg".to_string(), "8-bookworm".to_string())
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
                "Failed to get current docker image for Redis container"
            ))
        }
    }

    fn get_default_version(&self) -> String {
        "8-bookworm".to_string()
    }

    async fn get_current_version(&self) -> Result<String> {
        let (_, version) = self.get_current_docker_image().await?;
        Ok(version)
    }

    async fn upgrade(&self, old_config: ServiceConfig, new_config: ServiceConfig) -> Result<()> {
        info!("Starting Redis upgrade");

        let _old_redis_config = self.get_redis_config(old_config)?;
        let mut new_redis_config = self.get_redis_config(new_config)?;

        // Verify the new image can be pulled BEFORE stopping the old container
        info!(
            "Verifying new Docker image is available: {}",
            new_redis_config.docker_image
        );
        self.verify_image_pullable(&new_redis_config.docker_image)
            .await?;
        info!("New Docker image verified and is available");

        // Stop the old container
        info!("Stopping old Redis container");
        self.stop().await?;

        // Create container with new image (keeping the same volume for data persistence)
        info!("Starting Redis container with new image");
        let limits = self.resource_limits.read().await.clone();
        let password = new_redis_config.password.clone();
        self.create_container(&self.docker, &mut new_redis_config, &password, &limits)
            .await?;
        *self.config.write().await = Some(new_redis_config);

        info!("Redis upgrade completed successfully");
        Ok(())
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
        // service must target this, not the derived `redis-{name}`.
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

        // Extract version from image name (e.g., "gotempsh/redis-walg:8-bookworm" -> "8-bookworm")
        let version = if let Some(tag_pos) = image.rfind(':') {
            image[tag_pos + 1..].to_string()
        } else {
            "8-bookworm".to_string()
        };

        // Extract port from additional config if provided, otherwise use 6379
        let port = additional_config
            .get("port")
            .and_then(|v| v.as_str())
            .unwrap_or("6379")
            .to_string();

        // Extract password if provided
        let password = credentials.get("password").cloned().unwrap_or_default();

        // Verify connection to the imported service
        let connection_url = if password.is_empty() {
            format!("redis://localhost:{}", port)
        } else {
            format!(
                "redis://:{}@localhost:{}",
                urlencoding::encode(&password),
                port
            )
        };

        // Connects directly with `.await` on the current runtime — spinning
        // up a nested `tokio::runtime::Runtime` and calling `block_on` here
        // panics with "Cannot start a runtime from within a runtime", since
        // this `async fn` is already driven by one.
        let client = redis::Client::open(connection_url.as_str())
            .map_err(|e| anyhow::anyhow!("Invalid Redis connection URL: {}", e))?;
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.get_multiplexed_async_connection(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Redis connection timed out after 5 seconds"))?
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to connect to Redis at localhost:{} with provided credentials: {}",
                port,
                e
            )
        })?;
        info!("Successfully verified Redis connection for import");

        let network_ready = match ensure_network_exists(&self.docker).await {
            Ok(()) => true,
            Err(e) => {
                warn!(
                    "Failed to ensure Temps Docker network before Redis import attach: {:?}",
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
                    "Attached imported Redis container '{}' to {}",
                    imported_container_name, network_name
                ),
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 403, ..
                }) => debug!(
                    "Imported Redis container '{}' is already attached to {}",
                    imported_container_name, network_name
                ),
                Err(e) => warn!(
                    "Failed to attach imported Redis container '{}' to {}: {}",
                    imported_container_name, network_name, e
                ),
            }
        }

        // Build the ServiceConfig for registration
        let config = ServiceConfig {
            name: service_name,
            service_type: ServiceType::Redis,
            version: Some(version),
            parameters: serde_json::json!({
                "host": "localhost",
                "port": port,
                "password": password,
                "docker_image": image,
                "container_name": imported_container_name,
            }),
        };

        info!(
            "Successfully imported Redis service '{}' from container",
            config.name
        );
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_probe_config_preserves_missing_and_short_passwords() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = RedisService::new("probe-config".to_string(), docker);
        let without_password = service
            .get_redis_probe_config(ServiceConfig {
                name: "probe-config".to_string(),
                service_type: ServiceType::Redis,
                version: None,
                parameters: serde_json::json!({"host": "localhost", "port": "6380"}),
            })
            .unwrap();
        assert_eq!(without_password.password, "");

        let short_password = service
            .get_redis_probe_config(ServiceConfig {
                name: "probe-config".to_string(),
                service_type: ServiceType::Redis,
                version: None,
                parameters: serde_json::json!({
                    "host": "localhost",
                    "port": "6380",
                    "password": "short"
                }),
            })
            .unwrap();
        assert_eq!(short_password.password, "short");
    }

    #[test]
    fn classifies_supported_redis_backup_locations() {
        assert_eq!(
            classify_redis_backup_location("s3://backups/redis/cache/walg"),
            RedisBackupLocationKind::Walg
        );
        assert_eq!(
            classify_redis_backup_location("external_services/redis/cache/backup-00000000.rdb.gz"),
            RedisBackupLocationKind::RdbGzip
        );
    }

    #[test]
    fn rejects_legacy_or_malformed_redis_backup_locations() {
        for location in [
            "",
            "redis-backup.tar",
            "snapshot.rdb",
            "snapshot.rdb.gz.tmp",
        ] {
            assert_eq!(
                classify_redis_backup_location(location),
                RedisBackupLocationKind::Unsupported
            );
        }
    }

    /// `restore_capabilities` must declare in-place and new-service restore as
    /// supported, and explicitly NOT claim PITR (Redis has no WAL archive).
    #[tokio::test]
    async fn test_restore_capabilities_in_place_and_new_service_no_pitr() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = RedisService::new("test-caps".to_string(), docker);

        let config = ServiceConfig {
            name: "test-caps".to_string(),
            service_type: ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "6379",
                "password": "testpass1"
            }),
        };

        let caps = service
            .restore_capabilities(config)
            .await
            .expect("restore_capabilities must not fail");

        assert!(caps.restore_in_place, "Redis must support in-place restore");
        assert!(
            caps.restore_to_new_service,
            "Redis must support restore-to-new-service"
        );
        assert!(!caps.pitr, "Redis must NOT claim PITR support");
        assert!(
            caps.earliest_pitr_time.is_none(),
            "earliest_pitr_time must be None"
        );
        assert!(
            caps.latest_pitr_time.is_none(),
            "latest_pitr_time must be None"
        );
    }

    #[test]
    fn test_parameter_schema_editable_fields() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = RedisService::new("test-editable".to_string(), docker);

        // Get the parameter schema
        let schema_opt = service.get_parameter_schema();
        assert!(schema_opt.is_some(), "Schema should be generated");

        let schema = schema_opt.unwrap();
        let schema_obj = schema.as_object().expect("Schema should be an object");
        let properties = schema_obj
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("Properties should be an object");

        // Define expected editable status for each field - must match RedisParameterStrategy::updateable_keys()
        let editable_status = vec![
            ("host", false),
            ("port", true),
            ("password", false),
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
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = RedisService::new("test-port-change".to_string(), docker);

        // Create initial config with a specific port
        let initial_port = "7543";
        let config1 = super::ServiceConfig {
            name: "test-redis".to_string(),
            service_type: super::ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": initial_port,
                "password": "redispass123"
            }),
        };

        // Initialize service
        let result = service.init(config1.clone()).await;
        assert!(result.is_ok(), "Service initialization failed");

        // Verify initial port is set
        let local_addr = service.get_local_address(config1.clone()).unwrap();
        assert!(local_addr.contains("7543"), "Initial port should be 7543");

        // Create new config with different port
        let new_port = "7544";
        let config2 = super::ServiceConfig {
            name: "test-redis".to_string(),
            service_type: super::ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": new_port,
                "password": "redispass123"
            }),
        };

        // Verify new port configuration is recognized
        let new_local_addr = service.get_local_address(config2).unwrap();
        assert!(new_local_addr.contains("7544"), "New port should be 7544");

        // Cleanup
        let _ = service.cleanup().await;
    }

    /// Regression coverage for the DB-collision bug this fix closes: two
    /// distinct resources must never share a logical DB, re-allocating the
    /// same resource must be idempotent, and `drop_database` must actually
    /// free the slot for reuse rather than leaking it.
    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_allocate_database_isolation_and_reuse() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = RedisService::new("test-db-alloc".to_string(), docker);

        let config = super::ServiceConfig {
            name: "test-db-alloc".to_string(),
            service_type: super::ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "7549",
                "password": "allocpass123"
            }),
        };
        service.init(config).await.expect("init should succeed");

        // Two distinct resources must get distinct DBs.
        let db_a = service
            .allocate_database("project-a/prod")
            .await
            .expect("allocate resource A");
        let db_b = service
            .allocate_database("project-b/prod")
            .await
            .expect("allocate resource B");
        assert_ne!(
            db_a, db_b,
            "distinct resources must not collide on the same DB"
        );
        assert!((1..=15).contains(&db_a), "allocated DB must be in 1-15");
        assert!((1..=15).contains(&db_b), "allocated DB must be in 1-15");

        // Re-allocating the same resource is idempotent.
        let db_a_again = service
            .allocate_database("project-a/prod")
            .await
            .expect("re-allocate resource A");
        assert_eq!(
            db_a, db_a_again,
            "re-allocating the same resource must return the same DB"
        );

        // drop_database frees the slot for reuse by a different resource.
        service
            .drop_database("project-a/prod")
            .await
            .expect("drop resource A");
        let db_c = service
            .allocate_database("project-c/prod")
            .await
            .expect("allocate resource C after drop");
        assert_eq!(db_c, db_a, "a freed DB must be reusable by a new resource");

        // Cleanup
        let _ = service.drop_database("project-b/prod").await;
        let _ = service.drop_database("project-c/prod").await;
        let _ = service.cleanup().await;
    }

    /// A Redis provisioned before ownership was tracked has workloads sitting
    /// in DBs 1-15 with no owner key to show for it. Allocating one of those
    /// to a new resource would expose the old tenant's data and let
    /// `drop_database` FLUSHDB it later, so an occupied-but-unowned DB must be
    /// skipped and its contents left alone.
    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_allocate_database_skips_legacy_unowned_data() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = RedisService::new("test-db-legacy".to_string(), docker);

        let config = super::ServiceConfig {
            name: "test-db-legacy".to_string(),
            service_type: super::ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "7551",
                "password": "legacypass123"
            }),
        };
        service.init(config).await.expect("init should succeed");

        // Stand in for the pre-metadata scheme: data in DB 1 and no owner key.
        let mut conn = service
            .get_connection()
            .await
            .expect("connection should succeed");
        redis::cmd("SELECT")
            .arg(1)
            .query_async::<()>(&mut conn)
            .await
            .expect("select DB 1");
        redis::cmd("SET")
            .arg("legacy:tenant:key")
            .arg("legacy-value")
            .query_async::<()>(&mut conn)
            .await
            .expect("seed legacy data");

        let allocated = service
            .allocate_database("project-new/prod")
            .await
            .expect("allocate should succeed by skipping the occupied DB");
        assert_ne!(
            allocated, 1,
            "a DB holding unattributable data must never be allocated"
        );

        // The legacy data must still be there, untouched.
        redis::cmd("SELECT")
            .arg(1)
            .query_async::<()>(&mut conn)
            .await
            .expect("select DB 1");
        let value: Option<String> = redis::cmd("GET")
            .arg("legacy:tenant:key")
            .query_async(&mut conn)
            .await
            .expect("read legacy key");
        assert_eq!(value.as_deref(), Some("legacy-value"));

        // Dropping the new resource must not touch the reserved DB either.
        service
            .drop_database("project-new/prod")
            .await
            .expect("drop new resource");
        redis::cmd("SELECT")
            .arg(1)
            .query_async::<()>(&mut conn)
            .await
            .expect("select DB 1");
        let value: Option<String> = redis::cmd("GET")
            .arg("legacy:tenant:key")
            .query_async(&mut conn)
            .await
            .expect("read legacy key after drop");
        assert_eq!(
            value.as_deref(),
            Some("legacy-value"),
            "deprovisioning an unrelated resource must not flush the reserved DB"
        );

        let _ = service.cleanup().await;
    }

    /// When all 15 workload DBs (1-15) are claimed, allocation for a new
    /// resource must fail closed instead of silently colliding with an
    /// existing resource's DB.
    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_allocate_database_fails_closed_when_exhausted() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = RedisService::new("test-db-exhaust".to_string(), docker);

        let config = super::ServiceConfig {
            name: "test-db-exhaust".to_string(),
            service_type: super::ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "7550",
                "password": "exhaustpass123"
            }),
        };
        service.init(config).await.expect("init should succeed");

        for i in 0..15 {
            service
                .allocate_database(&format!("resource-{i}"))
                .await
                .unwrap_or_else(|e| panic!("allocate resource-{i} should succeed: {e}"));
        }

        let result = service.allocate_database("resource-overflow").await;
        assert!(
            result.is_err(),
            "allocation must fail closed once all 15 workload DBs are claimed"
        );

        // Cleanup
        for i in 0..15 {
            let _ = service.drop_database(&format!("resource-{i}")).await;
        }
        let _ = service.cleanup().await;
    }

    #[test]
    fn test_default_docker_image() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = RedisService::new("test-image".to_string(), docker);

        let (image_name, version) = service.get_default_docker_image();
        assert_eq!(
            image_name, "gotempsh/redis-walg",
            "Default image should be gotempsh/redis-walg"
        );
        assert_eq!(
            version, "8-bookworm",
            "Default version should be 8-bookworm"
        );
    }

    #[test]
    fn test_image_and_version_in_config() {
        // Test Redis configuration with docker_image field
        let input_config = RedisInputConfig {
            host: "localhost".to_string(),
            port: Some("6379".to_string()),
            password: Some("mypassword".to_string()),
            docker_image: "gotempsh/redis-walg:8-bookworm".to_string(),
            container_name: None,
        };

        // Convert to runtime config
        let runtime_config: RedisConfig = input_config.into();

        // Verify docker_image is used directly
        assert_eq!(
            runtime_config.docker_image,
            "gotempsh/redis-walg:8-bookworm"
        );
    }

    #[test]
    fn test_docker_image_parameter() {
        // Test Redis configuration with docker_image parameter
        let input_config = RedisInputConfig {
            host: "localhost".to_string(),
            port: Some("6379".to_string()),
            password: Some("mypassword".to_string()),
            docker_image: "gotempsh/redis-walg:8-bookworm".to_string(),
            container_name: None,
        };

        // Convert to runtime config
        let runtime_config: RedisConfig = input_config.into();

        // Verify docker_image is used
        assert_eq!(
            runtime_config.docker_image, "gotempsh/redis-walg:8-bookworm",
            "Docker image should use provided docker_image"
        );
    }

    #[test]
    fn test_docker_image_without_tag() {
        // Test Redis configuration with docker_image parameter but no tag
        let input_config = RedisInputConfig {
            host: "localhost".to_string(),
            port: Some("6379".to_string()),
            password: Some("mypassword".to_string()),
            docker_image: "redis".to_string(), // No tag
            container_name: None,
        };

        // Convert to runtime config
        let runtime_config: RedisConfig = input_config.into();

        // Verify docker_image with no tag is preserved as-is
        assert_eq!(runtime_config.docker_image, "redis");
    }

    #[test]
    fn test_redis_version_upgrade_config() {
        // Test simulated upgrade from Redis 6 to 7
        let old_config = super::ServiceConfig {
            name: "test-redis".to_string(),
            service_type: super::ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": Some("6379"),
                "password": "redispass123",
                "image": "redis",
                "version": "6-alpine"
            }),
        };

        let new_config = super::ServiceConfig {
            name: "test-redis".to_string(),
            service_type: super::ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": Some("6379"),
                "password": "redispass123",
                "image": "redis",
                "version": "7-alpine"
            }),
        };

        // Verify version upgrade configuration
        let old_version = old_config
            .parameters
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let new_version = new_config
            .parameters
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        assert_eq!(old_version, "6-alpine", "Old version should be 6-alpine");
        assert_eq!(new_version, "7-alpine", "New version should be 7-alpine");
    }

    #[test]
    fn test_import_service_config_creation() {
        let config = ServiceConfig {
            name: "test-redis-import".to_string(),
            service_type: ServiceType::Redis,
            version: Some("8-bookworm".to_string()),
            parameters: serde_json::json!({
                "host": "localhost",
                "port": 6379,
                "password": "",
                "db": 0,
                "docker_image": "gotempsh/redis-walg:8-bookworm",
                "container_id": "xyz789abc123",
            }),
        };

        assert_eq!(config.name, "test-redis-import");
        assert_eq!(config.service_type, ServiceType::Redis);
        assert_eq!(config.version, Some("8-bookworm".to_string()));
        assert_eq!(config.parameters["port"], 6379);
    }

    #[test]
    fn test_import_redis_version_extraction() {
        let test_cases = vec![
            ("gotempsh/redis-walg:8-bookworm", "8-bookworm"),
            ("redis:latest", "latest"),
            ("redis:6.2", "6.2"),
            ("redis:7.0-alpine", "7.0-alpine"),
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
    fn test_import_validates_required_credentials() {
        let credentials: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        // Redis might only need port and optional password

        assert!(!credentials.contains_key("port"));
        assert!(!credentials.contains_key("password"));
    }

    #[test]
    fn test_import_connection_string_with_password() {
        let password = "redispassword";
        let port = 6379;

        let connection_url = format!("redis://{}@localhost:{}", password, port);

        assert!(connection_url.contains("redis://"));
        assert!(connection_url.contains("redispassword"));
        assert!(connection_url.contains("localhost"));
        assert!(connection_url.contains("6379"));
    }

    #[test]
    fn test_import_connection_string_without_password() {
        let port = 6379;

        let connection_url = format!("redis://localhost:{}", port);

        assert!(connection_url.contains("redis://"));
        assert!(connection_url.contains("localhost"));
        assert!(connection_url.contains("6379"));
    }

    // `flavor = "multi_thread"` is required because the test uses
    // `MinioTestContainer`, whose `Drop` impl calls
    // `tokio::task::block_in_place` to synchronously stop/remove the
    // container. `block_in_place` panics under the default current-thread
    // runtime, and panicking inside Drop while a Tokio runtime is shutting
    // down has historically wedged the whole test binary in CI.
    #[cfg(feature = "docker-tests")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_redis_backup_and_restore_to_s3() {
        // Whole-test wall-clock budget. Anything above this is a hang — fail
        // loudly with a diagnostic instead of stalling the CI runner for 90 min.
        // See incident: GitHub run 25806816492 (PR #89) burned 90 min on this
        // test because blocking redis APIs starved the tokio worker pool.
        // 300s to match the sibling postgres/mongodb backup-and-restore tests,
        // which do the same MinIO + container-lifecycle + WAL-G/dump work —
        // 180s was too tight and flaked under normal CI load (see GitHub run
        // 28684634260), not an actual hang.
        const TEST_TIMEOUT: Duration = Duration::from_secs(300);
        // Per-Redis-operation timeout. ConnectionManager retries internally,
        // so this needs only cover the cold-start window of the container.
        const REDIS_OP_TIMEOUT: Duration = Duration::from_secs(30);

        tokio::time::timeout(TEST_TIMEOUT, run_redis_backup_and_restore_to_s3(REDIS_OP_TIMEOUT))
            .await
            .expect("test_redis_backup_and_restore_to_s3 exceeded 300s — likely hung on Redis/Docker/S3 wait");
    }

    /// Body of `test_redis_backup_and_restore_to_s3`, extracted so the outer
    /// test can wrap it in `tokio::time::timeout` without a giant async block
    /// at the call site.
    #[cfg(feature = "docker-tests")]
    async fn run_redis_backup_and_restore_to_s3(op_timeout: Duration) {
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
        let minio = match MinioTestContainer::start(docker.clone(), "redis-backup-test").await {
            Ok(m) => m,
            Err(e) => {
                let error_msg = e.to_string();
                if error_msg.contains("certificate")
                    || error_msg.contains("TrustStore")
                    || error_msg.contains("panicked")
                {
                    println!("❌ Skipping Redis backup test: TLS certificate issue");
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

        // Pick a free port so parallel test runs (and leaked containers from
        // previous runs) don't collide. Previously hardcoded to 16379, which
        // caused silent hangs in CI when a leftover container held the port.
        let redis_port = match find_available_port(16379) {
            Some(p) => p,
            None => {
                println!("No available port in 16379..16479 range, skipping test");
                let _ = minio.cleanup().await;
                return;
            }
        };
        let redis_password = "redispass123";
        let service_name = format!(
            "test_redis_backup_{}",
            chrono::Utc::now().timestamp_millis()
        );

        let redis_params = serde_json::json!({
            "host": "localhost",
            "port": redis_port.to_string(),
            "password": redis_password,
            "docker_image": "gotempsh/redis-walg:8-bookworm",
        });

        let redis_config = ServiceConfig {
            name: service_name.clone(),
            service_type: ServiceType::Redis,
            version: Some("7".to_string()),
            parameters: redis_params,
        };

        let redis_service = RedisService::new(service_name.clone(), docker.clone());

        // Initialize Redis service
        match redis_service.init(redis_config.clone()).await {
            Ok(_) => println!("✓ Redis service initialized"),
            Err(e) => {
                println!("Failed to initialize Redis: {}. Skipping test", e);
                let _ = minio.cleanup().await;
                return;
            }
        }

        // Connect to Redis using the async ConnectionManager. This must NOT
        // be `redis::Client::get_connection()` — that's the blocking, no-
        // timeout sync API, and it parks a tokio worker thread on a raw
        // socket connect. Under parallel test load that exhausts the runtime
        // worker pool and the whole test binary deadlocks (with no progress
        // output) until CI kills it.
        let connection_url = format!("redis://:{}@localhost:{}", redis_password, redis_port);
        let redis_client = match Client::open(connection_url.as_str()) {
            Ok(client) => client,
            Err(e) => {
                println!("Failed to create Redis client: {}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };

        let mut conn =
            match tokio::time::timeout(op_timeout, ConnectionManager::new(redis_client.clone()))
                .await
            {
                Ok(Ok(c)) => c,
                Ok(Err(e)) => {
                    println!("Failed to connect to Redis: {}. Skipping test", e);
                    let _ = redis_service.remove().await;
                    let _ = minio.cleanup().await;
                    return;
                }
                Err(_) => {
                    println!(
                        "Redis connect timed out after {:?}. Skipping test",
                        op_timeout
                    );
                    let _ = redis_service.remove().await;
                    let _ = minio.cleanup().await;
                    return;
                }
            };

        // Helper to run a Redis command with a bounded timeout and consistent
        // skip-on-failure behaviour. Defined inline so it captures the cleanup
        // closures by reference.
        async fn redis_set(
            conn: &mut ConnectionManager,
            key: &str,
            value: &str,
            timeout: Duration,
        ) -> Result<()> {
            tokio::time::timeout(
                timeout,
                redis::cmd("SET")
                    .arg(key)
                    .arg(value)
                    .query_async::<()>(conn),
            )
            .await
            .map_err(|_| anyhow::anyhow!("SET {} timed out after {:?}", key, timeout))?
            .map_err(|e| anyhow::anyhow!("SET {} failed: {}", key, e))
        }

        async fn redis_get_string(
            conn: &mut ConnectionManager,
            key: &str,
            timeout: Duration,
        ) -> Result<String> {
            tokio::time::timeout(
                timeout,
                redis::cmd("GET").arg(key).query_async::<String>(conn),
            )
            .await
            .map_err(|_| anyhow::anyhow!("GET {} timed out after {:?}", key, timeout))?
            .map_err(|e| anyhow::anyhow!("GET {} failed: {}", key, e))
        }

        async fn redis_exists(
            conn: &mut ConnectionManager,
            key: &str,
            timeout: Duration,
        ) -> Result<bool> {
            tokio::time::timeout(
                timeout,
                redis::cmd("EXISTS").arg(key).query_async::<bool>(conn),
            )
            .await
            .map_err(|_| anyhow::anyhow!("EXISTS {} timed out after {:?}", key, timeout))?
            .map_err(|e| anyhow::anyhow!("EXISTS {} failed: {}", key, e))
        }

        // Set test data
        for (k, v) in [
            ("test_key1", "value1"),
            ("test_key2", "value2"),
            ("test_key3", "value3"),
        ] {
            if let Err(e) = redis_set(&mut conn, k, v, op_timeout).await {
                println!("{}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
            println!("✓ Set {}={}", k, v);
        }

        // Verify data exists
        let value1 = match redis_get_string(&mut conn, "test_key1", op_timeout).await {
            Ok(v) => v,
            Err(e) => {
                println!("{}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };
        assert_eq!(value1, "value1");
        println!("✓ Verified test_key1={}", value1);

        // Create mock database connection for backup/restore operations
        let mock_db = match create_mock_db().await {
            Ok(db) => db,
            Err(e) => {
                println!("Failed to create mock database: {}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };

        // Create mock backup record
        let backup = create_mock_backup("backups/redis/test");
        let external_service = create_mock_external_service(service_name.clone(), "redis", "7");

        // Perform backup to S3
        let s3_creds = minio.s3_credentials();
        let backup_location = match redis_service
            .backup_to_s3(
                &minio.s3_client,
                &s3_creds,
                backup,
                &minio.s3_source,
                "backups/redis",
                "backups",
                &mock_db,
                &external_service,
                redis_config.clone(),
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
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };

        // Delete keys to simulate data loss
        let del_result = tokio::time::timeout(
            op_timeout,
            redis::cmd("DEL")
                .arg("test_key1")
                .arg("test_key2")
                .arg("test_key3")
                .query_async::<()>(&mut conn),
        )
        .await;
        match del_result {
            Ok(Ok(_)) => println!("✓ Deleted all test keys (simulating data loss)"),
            Ok(Err(e)) => {
                println!("Failed to delete keys: {}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
            Err(_) => {
                println!("DEL timed out after {:?}. Skipping test", op_timeout);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        }

        let exists = match redis_exists(&mut conn, "test_key1", op_timeout).await {
            Ok(v) => v,
            Err(e) => {
                println!("{}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };
        assert!(!exists, "test_key1 should not exist after deletion");
        println!("✓ Verified keys were deleted");

        // Restore from S3 backup
        match redis_service
            .restore_from_s3(
                &minio.s3_client,
                &s3_creds,
                &backup_location,
                &minio.s3_source,
                redis_config.clone(),
            )
            .await
        {
            Ok(_) => println!("✓ Restore completed from: {}", backup_location),
            Err(e) => {
                println!("Restore failed: {}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };

        // Re-establish a fresh connection after restore — the prior socket
        // may have been severed when the Redis process reloaded. The
        // ConnectionManager would reconnect lazily on next command anyway,
        // but doing it explicitly bounds the wait.
        let mut conn =
            match tokio::time::timeout(op_timeout, ConnectionManager::new(redis_client.clone()))
                .await
            {
                Ok(Ok(c)) => c,
                Ok(Err(e)) => {
                    println!("Failed to reconnect after restore: {}. Skipping test", e);
                    let _ = redis_service.remove().await;
                    let _ = minio.cleanup().await;
                    return;
                }
                Err(_) => {
                    println!(
                        "Reconnect after restore timed out after {:?}. Skipping test",
                        op_timeout
                    );
                    let _ = redis_service.remove().await;
                    let _ = minio.cleanup().await;
                    return;
                }
            };

        let exists1 = match redis_exists(&mut conn, "test_key1", op_timeout).await {
            Ok(v) => v,
            Err(e) => {
                println!("{}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };
        assert!(exists1, "test_key1 should exist after restore");
        println!("✓ Verified test_key1 exists after restore");

        for (k, expected) in [
            ("test_key1", "value1"),
            ("test_key2", "value2"),
            ("test_key3", "value3"),
        ] {
            let v = match redis_get_string(&mut conn, k, op_timeout).await {
                Ok(v) => v,
                Err(e) => {
                    println!("{}. Skipping test", e);
                    let _ = redis_service.remove().await;
                    let _ = minio.cleanup().await;
                    return;
                }
            };
            assert_eq!(v, expected);
            println!("✓ Verified {}={}", k, v);
        }

        // Cleanup
        drop(conn);
        let _ = redis_service.stop().await;
        let _ = redis_service.remove().await;
        let _ = minio.cleanup().await;

        println!("✅ Redis backup and restore test passed!");
    }

    #[test]
    fn test_get_effective_address_baremetal_mode() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = RedisService::new("test-effective-addr".to_string(), docker);

        let config = ServiceConfig {
            name: "test-redis".to_string(),
            service_type: ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "6379",
                "password": "testpass",
            }),
        };

        let (host, port) = service
            .get_effective_address_for_environment(config, temps_core::ExecutionEnvironment::Host)
            .unwrap();

        // In baremetal mode, should return localhost with exposed port
        assert_eq!(host, "localhost");
        assert_eq!(port, "6379");
    }

    #[test]
    fn test_get_effective_address_docker_mode() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = RedisService::new("test-effective-addr-docker".to_string(), docker);

        let config = ServiceConfig {
            name: "test-redis".to_string(),
            service_type: ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "6380",
                "password": "testpass",
            }),
        };

        let (host, port) = service
            .get_effective_address_for_environment(config, temps_core::ExecutionEnvironment::Docker)
            .unwrap();

        // In Docker mode, should return container name with internal port
        assert_eq!(host, "redis-test-effective-addr-docker");
        assert_eq!(port, "6379"); // Internal port
    }

    #[test]
    fn test_get_effective_address_docker_mode_uses_imported_container_name() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = RedisService::new("imported-svc".to_string(), docker);

        let config = ServiceConfig {
            name: "imported-svc".to_string(),
            service_type: ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "6380",
                "password": "testpass",
                "container_name": "legacy-redis",
            }),
        };

        let (host, port) = service
            .get_effective_address_for_environment(config, temps_core::ExecutionEnvironment::Docker)
            .unwrap();
        // The imported container name wins over the derived `redis-{name}`.
        assert_eq!(host, "legacy-redis");
        assert_eq!(port, "6379");
    }

    #[test]
    fn test_container_name_is_not_a_user_input() {
        // container_name is derived from the service name at creation time
        // (`redis-{name}`), never supplied by the client — same as MariaDB
        // (see mariadb.rs's identical test). The create form is generated
        // from this schema, so the field must not appear in it.
        let schema = serde_json::to_value(schemars::schema_for!(RedisInputConfig)).unwrap();
        assert!(
            !schema.to_string().contains("container_name"),
            "container_name leaked into the Redis create schema"
        );
    }

    #[test]
    fn test_get_environment_variables_always_uses_container_name() {
        // get_environment_variables always uses container name and internal port
        // for container-to-container communication, regardless of deployment mode
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = RedisService::new("test-env-vars".to_string(), docker);

        let mut params = std::collections::HashMap::new();
        params.insert("port".to_string(), "6380".to_string());
        params.insert("password".to_string(), "testpass".to_string());

        let env_vars = service.get_environment_variables(&params).unwrap();

        // Always uses container name and internal port (6379)
        assert_eq!(env_vars.get("REDIS_HOST").unwrap(), "redis-test-env-vars");
        assert_eq!(env_vars.get("REDIS_PORT").unwrap(), "6379");
        assert!(env_vars
            .get("REDIS_URL")
            .unwrap()
            .contains("redis-test-env-vars:6379"));
    }

    #[test]
    fn test_get_docker_environment_variables_baremetal_mode() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = RedisService::new("test-docker-env".to_string(), docker);

        let mut params = std::collections::HashMap::new();
        params.insert("port".to_string(), "6381".to_string());
        params.insert("password".to_string(), "testpass".to_string());

        let env_vars = service
            .get_docker_environment_variables_for_environment(
                &params,
                temps_core::ExecutionEnvironment::Host,
            )
            .unwrap();

        // In baremetal mode, should use localhost with exposed port
        assert_eq!(env_vars.get("REDIS_HOST").unwrap(), "localhost");
        assert_eq!(env_vars.get("REDIS_PORT").unwrap(), "6381");
    }

    #[test]
    fn test_get_docker_environment_variables_docker_mode() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = RedisService::new("test-docker-env-mode".to_string(), docker);

        let mut params = std::collections::HashMap::new();
        params.insert("port".to_string(), "6381".to_string());
        params.insert("password".to_string(), "testpass".to_string());

        let env_vars = service
            .get_docker_environment_variables_for_environment(
                &params,
                temps_core::ExecutionEnvironment::Docker,
            )
            .unwrap();

        // In Docker mode, should use container name and internal port
        assert_eq!(
            env_vars.get("REDIS_HOST").unwrap(),
            "redis-test-docker-env-mode"
        );
        assert_eq!(env_vars.get("REDIS_PORT").unwrap(), "6379"); // Internal port
    }
}
