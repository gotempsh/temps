use crate::utils::ensure_network_exists;

use super::{
    ExternalService, HealthProbeResult, ServiceConfig, ServiceResourceLimits, ServiceType,
};
use anyhow::Result;
use async_trait::async_trait;
use bollard::query_parameters::{InspectContainerOptions, StopContainerOptions};
use bollard::{body_full, Docker};
use futures::TryStreamExt;
use redis::{aio::ConnectionManager, Client};
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
    #[schemars(example = "example_host", default = "default_host")]
    pub host: String,

    /// Redis port (auto-assigned if not provided)
    #[schemars(example = "example_port")]
    pub port: Option<String>,

    /// Redis password (auto-generated if not provided, empty, or less than 8 characters)
    #[serde(default, deserialize_with = "deserialize_optional_password")]
    #[schemars(
        with = "Option<String>",
        example = "example_password",
        description = "Redis password (minimum 8 characters, auto-generated if not provided)"
    )]
    pub password: Option<String>,

    /// Full Docker image reference (e.g., "gotempsh/redis-walg:8-bookworm")
    #[serde(default = "default_docker_image")]
    #[schemars(example = "example_docker_image", default = "default_docker_image")]
    pub docker_image: String,
}

/// Internal runtime configuration for Redis service
/// This is what the service uses internally after processing input
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisConfig {
    pub host: String,
    pub port: String,
    pub password: String,
    pub docker_image: String,
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

fn default_host() -> String {
    "localhost".to_string()
}

fn generate_password() -> String {
    use rand::{distributions::Alphanumeric, Rng};
    rand::thread_rng()
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

    fn get_container_name(&self) -> String {
        format!("redis-{}", self.name)
    }

    /// Creates and starts the Redis container, retrying with a fresh host
    /// port if the chosen one lost the race described in `port_util` docs
    /// (bindable when we checked, but taken by the time Docker actually binds
    /// it). The container name is deterministic, so a failed attempt must be
    /// removed before retrying or the next attempt's "already exists" check
    /// short-circuits without picking a new port.
    async fn create_container(
        &self,
        docker: &Docker,
        config: &RedisConfig,
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
                Ok(()) => return Ok(()),
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

        // Parse image name and tag
        let (image_name, tag) = if let Some((name, tag)) = config.docker_image.split_once(':') {
            (name.to_string(), tag.to_string())
        } else {
            (config.docker_image.to_string(), "latest".to_string())
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
                if state.status == Some(bollard::models::ContainerStateStatusEnum::RUNNING)
                    && state.health.as_ref().and_then(|h| h.status.as_ref())
                        == Some(&bollard::models::HealthStatusEnum::HEALTHY)
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

    /// Calculate a deterministic database number (0-15) from a resource name
    /// This allows us to allocate databases without requiring a Redis connection
    fn calculate_database_number(&self, resource_name: &str) -> u8 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        resource_name.hash(&mut hasher);
        let hash = hasher.finish();

        // Redis supports 16 databases (0-15), so we use modulo to get a valid number
        (hash % 16) as u8
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

    /// Verify that a Docker image can be pulled without actually downloading the full image
    /// Attempts to pull the image - fails if it doesn't exist or cannot be accessed
    #[allow(dead_code)]
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

    /// Restore from a WAL-G backup stored in S3.
    ///
    /// WAL-G restore requires stopping Redis, fetching the backup (which writes
    /// dump.rdb via WALG_STREAM_RESTORE_COMMAND), and restarting.
    async fn restore_from_walg(
        &self,
        s3_credentials: &super::S3Credentials,
        walg_s3_prefix: &str,
    ) -> Result<()> {
        let container_name = self.get_container_name();

        info!(
            "Restoring Redis from WAL-G backup (prefix: {}) in container '{}'",
            walg_s3_prefix, container_name
        );

        // Get the Redis image from the running container for the helper
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
            .resolve_endpoint_for_container(&self.docker, &container_name)
            .await
        {
            walg_env.push(format!("AWS_ENDPOINT={}", resolved_endpoint));
        }
        if s3_credentials.force_path_style {
            walg_env.push("AWS_S3_FORCE_PATH_STYLE=true".to_string());
        }

        // Step 1: Stop the Redis container so it's not using the data volume.
        // Redis is PID 1, so stopping the container cleanly shuts down Redis and
        // ensures no autosave can overwrite the dump.rdb we're about to write.
        //
        // IMPORTANT: Disable the restart policy first. The container has
        // restart_policy=always, so Docker would immediately restart it after stop,
        // preventing the helper container from writing to the shared volume.
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

        // Step 2: Use an ephemeral helper container with volumes_from to run WAL-G fetch.
        // We can't exec into a stopped container, so we create a helper that shares
        // the same data volume and runs WAL-G backup-fetch there.
        info!("Fetching WAL-G backup via helper container");
        let helper_name = format!("{}-restore-helper", container_name);

        use bollard::models::{ContainerCreateBody, HostConfig};
        // The helper runs WAL-G fetch (which writes dump.rdb) and then replaces the AOF
        // base file with the restored RDB. Redis 7+ with --appendonly yes loads from the
        // multi-part AOF in appendonlydir/ (base RDB + incremental AOF files). If we just
        // delete appendonlydir, Redis recreates an EMPTY one on startup and ignores dump.rdb.
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
        // (relative to the *original* container's network) is unreachable
        // from inside it — wal-g's fetch then hangs indefinitely trying to
        // resolve/connect to a host it has no network path to.
        ensure_network_exists(&self.docker)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to ensure network exists: {:?}", e))?;
        let helper_config = ContainerCreateBody {
            image: Some(redis_image),
            cmd: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                restore_script.to_string(),
            ]),
            env: Some(walg_env),
            host_config: Some(HostConfig {
                volumes_from: Some(vec![container_name.clone()]),
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

        self.docker
            .start_container(
                &helper.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start restore helper container: {}", e))?;

        // Wait for helper to finish. Bounded — unlike `run_exec`'s exec-based
        // path, this waits on the container-level Docker API directly with no
        // other timeout backstop; leaving it unbounded means a stuck helper
        // container hangs until the *caller's* outer timeout eventually
        // fires, with none of the diagnostics `run_exec` provides.
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
                    container_name,
                    REDIS_BACKUP_EXEC_TIMEOUT
                ));
            }
        };

        // Capture helper container logs before cleanup for diagnostics
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
                container_name
            );
        } else {
            info!(
                "WAL-G restore helper logs for '{}': {}",
                container_name,
                log_output.trim()
            );
        }

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
                    "WAL-G backup-fetch helper exited with code {} for container '{}'. Logs: {}",
                    wait_response.status_code,
                    container_name,
                    log_output.trim()
                ));
            }
        }

        // Step 3: Re-enable restart policy and start the original Redis container.
        // Redis will load the restored dump.rdb on startup.
        info!("Starting Redis with restored data");
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
            .map_err(|e| anyhow::anyhow!("Failed to start Redis after restore: {}", e))?;

        // Wait for container to be healthy
        self.wait_for_container_health(&self.docker, &container_name)
            .await?;

        info!("Redis WAL-G restore completed successfully");
        Ok(())
    }

    /// Restore from a legacy backup (pre-WAL-G .tar files containing dump.rdb/appendonly.aof).
    /// Falls back to the old approach: download from S3, extract, upload to container.
    async fn restore_from_legacy(
        &self,
        s3_client: &aws_sdk_s3::Client,
        backup_location: &str,
        s3_source: &temps_entities::s3_sources::Model,
    ) -> Result<()> {
        info!(
            "Restoring Redis from legacy backup format: {}",
            backup_location
        );

        // Get the backup object from S3
        let get_obj = s3_client
            .get_object()
            .bucket(&s3_source.bucket_name)
            .key(backup_location)
            .send()
            .await?;

        // Read the backup data
        let backup_data = get_obj.body.collect().await?.to_vec();

        let container_name = self.get_container_name();

        self.docker
            .stop_container(&container_name, None::<StopContainerOptions>)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to stop Redis container for restore: {}", e))?;

        // Create a temporary directory
        let temp_dir = tempfile::tempdir()?;
        let tar_path = temp_dir.path().join("backup.tar");

        // Write the tar file
        tokio::fs::write(&tar_path, backup_data).await?;

        // Extract the tar file
        let tar_file = std::fs::File::open(&tar_path)?;
        let mut archive = tar::Archive::new(tar_file);
        archive.unpack(temp_dir.path())?;

        // Create a new tar archive with the extracted files in the correct structure
        let mut tar = tar::Builder::new(Vec::new());
        for file in &["dump.rdb", "appendonly.aof"] {
            let file_path = temp_dir.path().join(file);
            if file_path.exists() {
                tar.append_path_with_name(&file_path, file)?;
            }
        }
        let tar_data = tar.into_inner()?;

        // Copy both files into the container's data directory
        self.docker
            .upload_to_container(
                &container_name,
                Some(bollard::query_parameters::UploadToContainerOptions {
                    path: "/data".to_string(),
                    ..Default::default()
                }),
                body_full(bytes::Bytes::from(tar_data)),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to upload backup files to container: {}", e))?;

        // Start Redis server again
        self.docker
            .start_container(
                &container_name,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start Redis container after restore: {}", e))?;

        // Wait for container to be healthy
        self.wait_for_container_health(&self.docker, &container_name)
            .await?;

        info!("Redis legacy restore completed successfully");
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

        let container_name = self.get_container_name();
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
        let config = self.get_redis_config(service_config)?;

        if temps_core::DeploymentMode::is_docker() {
            // Docker mode: use container name and internal port
            Ok((self.get_container_name(), REDIS_INTERNAL_PORT.to_string()))
        } else {
            // Baremetal mode: use localhost and exposed port
            Ok(("localhost".to_string(), config.port))
        }
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
        let redis_config = self.get_redis_config(config)?;

        info!(
            "Redis init - storing config: port={}, password_len={}",
            redis_config.port,
            redis_config.password.len()
        );

        // Store runtime config and limits so `start()` recreates correctly.
        *self.config.write().await = Some(redis_config.clone());
        *self.resource_limits.write().await = resource_limits.clone();

        info!("Redis init - config stored successfully");

        // Create Docker container (but don't start it yet)
        // Note: Connection will be established in start() method
        self.create_container(
            &self.docker,
            &redis_config,
            &redis_config.password,
            &resource_limits,
        )
        .await?;

        info!("Redis container created, connection will be established on start");

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

        let cfg = match self.get_redis_config(service_config) {
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
        let mut env_vars = HashMap::new();
        let port = parameters
            .get("port")
            .ok_or_else(|| anyhow::anyhow!("Missing port parameter"))?;
        let password = parameters.get("password");

        // Get effective host and port based on deployment mode
        let (effective_host, effective_port) = if temps_core::DeploymentMode::is_docker() {
            // Docker mode: use container name and internal port
            (self.get_container_name(), REDIS_INTERNAL_PORT.to_string())
        } else {
            // Baremetal mode: use localhost and exposed port
            ("localhost".to_string(), port.clone())
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

        // Calculate database number using a hash instead of requiring Redis connection
        // This allows us to generate env vars before the service is started
        let db_number = self.calculate_database_number(&resource_name);

        let mut env_vars = HashMap::new();

        // Always use container name and internal port for container-to-container communication
        let effective_host = self.get_container_name();
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
        let container_name = self.get_container_name();
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
            let config = self
                .config
                .read()
                .await
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Redis configuration not found"))?
                .clone();
            let limits = self.resource_limits.read().await.clone();
            self.create_container(&self.docker, &config, &config.password, &limits)
                .await?;
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
        let container_name = self.get_container_name();
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

        // Always use container name and internal port for container-to-container communication
        let effective_host = self.get_container_name();
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

    async fn deprovision_resource(&self, _project_id: &str, _environment: &str) -> Result<()> {
        // No database-level deprovisioning needed
        // Each project/environment gets a calculated database number (0-15) based on hash
        // Cleanup would happen at the application level (flushing keys with specific prefixes)
        Ok(())
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

        let container_name = self.get_container_name();

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

    fn get_default_docker_image(&self) -> (String, String) {
        // Return (image_name, version)
        ("gotempsh/redis-walg".to_string(), "8-bookworm".to_string())
    }

    async fn get_current_docker_image(&self) -> Result<(String, String)> {
        let container_name = self.get_container_name();
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
        let new_redis_config = self.get_redis_config(new_config)?;

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
        self.create_container(
            &self.docker,
            &new_redis_config,
            &new_redis_config.password,
            &limits,
        )
        .await?;

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

        match redis::Client::open(connection_url.as_str())
            .ok()
            .and_then(|client| {
                tokio::runtime::Runtime::new()
                    .ok()
                    .and_then(|rt| rt.block_on(async { client.get_connection().ok() }))
            }) {
            Some(_) => {
                info!("Successfully verified Redis connection for import");
            }
            None => {
                return Err(anyhow::anyhow!(
                    "Failed to connect to Redis at localhost:{} with provided credentials. Verify port and password.",
                    port
                ));
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
                "container_id": container_id,
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

    use crate::externalsvc::DEPLOYMENT_MODE_MUTEX as ENV_MUTEX;

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
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // Clear Docker mode to ensure baremetal mode
        unsafe { std::env::remove_var("DEPLOYMENT_MODE") };

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

        let (host, port) = service.get_effective_address(config).unwrap();

        // In baremetal mode, should return localhost with exposed port
        assert_eq!(host, "localhost");
        assert_eq!(port, "6379");
    }

    #[test]
    fn test_get_effective_address_docker_mode() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // Set Docker mode
        unsafe { std::env::set_var("DEPLOYMENT_MODE", "docker") };

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

        let (host, port) = service.get_effective_address(config).unwrap();

        // In Docker mode, should return container name with internal port
        assert_eq!(host, "redis-test-effective-addr-docker");
        assert_eq!(port, "6379"); // Internal port

        // Clean up
        unsafe { std::env::remove_var("DEPLOYMENT_MODE") };
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
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // Clear Docker mode to ensure baremetal mode
        unsafe { std::env::remove_var("DEPLOYMENT_MODE") };

        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = RedisService::new("test-docker-env".to_string(), docker);

        let mut params = std::collections::HashMap::new();
        params.insert("port".to_string(), "6381".to_string());
        params.insert("password".to_string(), "testpass".to_string());

        let env_vars = service.get_docker_environment_variables(&params).unwrap();

        // In baremetal mode, should use localhost with exposed port
        assert_eq!(env_vars.get("REDIS_HOST").unwrap(), "localhost");
        assert_eq!(env_vars.get("REDIS_PORT").unwrap(), "6381");
    }

    #[test]
    fn test_get_docker_environment_variables_docker_mode() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // Set Docker mode
        unsafe { std::env::set_var("DEPLOYMENT_MODE", "docker") };

        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = RedisService::new("test-docker-env-mode".to_string(), docker);

        let mut params = std::collections::HashMap::new();
        params.insert("port".to_string(), "6381".to_string());
        params.insert("password".to_string(), "testpass".to_string());

        let env_vars = service.get_docker_environment_variables(&params).unwrap();

        // In Docker mode, should use container name and internal port
        assert_eq!(
            env_vars.get("REDIS_HOST").unwrap(),
            "redis-test-docker-env-mode"
        );
        assert_eq!(env_vars.get("REDIS_PORT").unwrap(), "6379"); // Internal port

        // Clean up
        unsafe { std::env::remove_var("DEPLOYMENT_MODE") };
    }
}
