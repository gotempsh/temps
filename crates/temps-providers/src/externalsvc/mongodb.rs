use anyhow::Result;
use async_trait::async_trait;
use bollard::exec::CreateExecOptions;
use bollard::query_parameters::{InspectContainerOptions, StopContainerOptions};
use bollard::{body_full, Docker};
use futures::{StreamExt, TryStreamExt};
use mongodb::bson::doc;
use mongodb::options::ClientOptions;
use mongodb::Client as MongoClient;
use schemars::JsonSchema;
use sea_orm::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

/// Bound for a single MongoDB backup `docker exec`. Mongo dumps can be
/// large; 4 hours is a reasonable middle ground.
const MONGODB_BACKUP_EXEC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4 * 3600);
use urlencoding;

use crate::utils::ensure_network_exists;

use super::{
    ExternalService, HealthProbeResult, RuntimeEnvVar, ServiceConfig, ServiceResourceLimits,
    ServiceType,
};

/// Input configuration for creating a MongoDB service
/// This is what users provide when creating the service
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(
    title = "MongoDB Configuration",
    description = "Configuration for MongoDB service"
)]
pub struct MongodbInputConfig {
    /// MongoDB host address
    #[serde(default = "default_host")]
    #[schemars(example = "example_host", default = "default_host")]
    pub host: String,

    /// MongoDB port (auto-assigned if not provided)
    #[schemars(example = "example_port")]
    pub port: Option<String>,

    /// MongoDB database name
    #[serde(default = "default_database")]
    #[schemars(example = "example_database", default = "default_database")]
    pub database: String,

    /// MongoDB username
    #[serde(default = "default_username")]
    #[schemars(example = "example_username", default = "default_username")]
    pub username: String,

    /// MongoDB password (auto-generated if not provided or empty)
    #[serde(default, deserialize_with = "deserialize_optional_password")]
    #[schemars(with = "Option<String>", example = "example_password")]
    pub password: Option<String>,

    /// Docker image to use for MongoDB (e.g., gotempsh/mongodb-walg:8.0, gotempsh/mongodb-walg:7.0)
    #[serde(default = "default_docker_image")]
    #[schemars(example = "example_docker_image", default = "default_docker_image")]
    pub docker_image: String,

    /// Optional replica set name. When set, mongod is started with `--replSet <name>`,
    /// a keyfile-protected `--auth`, and `rs.initiate()` is run after first start.
    /// Required for transactions, change streams, and oplog-based CDC.
    /// This is a single-node replica set — for multi-node HA use the cluster topology.
    /// Cannot be changed after creation: switching modes on an existing data volume corrupts state.
    #[serde(default, deserialize_with = "deserialize_optional_replica_set")]
    #[schemars(with = "Option<String>", example = "example_replica_set")]
    pub replica_set: Option<String>,

    /// Real Docker container name when this service was imported from an
    /// existing MongoDB-compatible container (set by `import_from_container`,
    /// never user-editable — omitted from the create form). Overrides the
    /// derived `temps-mongodb-{name}` container name so internal addressing
    /// targets the actual pre-existing container instead of a synthesized
    /// name that doesn't exist. Mirrors the MariaDB/Postgres/Redis fix for
    /// the same class of bug.
    #[serde(default, deserialize_with = "deserialize_optional_non_empty")]
    #[schemars(skip)]
    pub container_name: Option<String>,
}

// Example functions for schemars
fn example_host() -> &'static str {
    "localhost"
}

fn example_port() -> &'static str {
    "27017"
}

fn example_database() -> &'static str {
    "mydatabase"
}

fn example_username() -> &'static str {
    "root"
}

fn example_password() -> &'static str {
    ""
}

fn default_docker_image() -> String {
    "gotempsh/mongodb-walg:8.0".to_string()
}

fn example_docker_image() -> &'static str {
    "gotempsh/mongodb-walg:8.0"
}

fn example_replica_set() -> &'static str {
    "rs0"
}

/// Internal runtime configuration for MongoDB service
/// This is what the service uses internally after processing input
/// and what gets saved to the database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MongodbRuntimeConfig {
    pub host: String,
    pub port: String,
    pub database: String,
    pub username: String,
    pub password: String,
    pub docker_image: String,
    /// When set, mongod runs with `--replSet <name>`. None means standalone mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replica_set: Option<String>,
    /// Base64 keyfile contents used for `--keyFile` when replica_set is enabled.
    /// MongoDB requires a keyfile whenever both `--auth` and `--replSet` are set,
    /// even for a single-node replica set. Generated once at creation and persisted
    /// here so the same keyfile is written into the container on every restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyfile_content: Option<String>,
    /// Real container name for imported services — see
    /// `MongodbInputConfig::container_name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
}

impl From<MongodbInputConfig> for MongodbRuntimeConfig {
    fn from(input: MongodbInputConfig) -> Self {
        let replica_set = input.replica_set;
        let keyfile_content = if replica_set.is_some() {
            Some(generate_keyfile_content())
        } else {
            None
        };
        Self {
            host: input.host,
            port: input.port.unwrap_or_else(|| {
                find_available_port(27017)
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "27017".to_string())
            }),
            database: input.database,
            username: input.username,
            password: input.password.unwrap_or_else(generate_password),
            docker_image: input.docker_image,
            replica_set,
            keyfile_content,
            container_name: input.container_name,
        }
    }
}

fn deserialize_optional_password<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // Deserialize as Option to handle missing field
    let opt: Option<String> = Option::deserialize(deserializer)?;

    // Return None if missing or empty (will trigger auto-generation)
    Ok(match opt {
        Some(s) if !s.is_empty() => Some(s),
        _ => None,
    })
}

/// Treats a blank string the same as an absent value — see
/// `MongodbInputConfig::container_name`.
fn deserialize_optional_non_empty<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(opt.filter(|s| !s.is_empty()))
}

/// Treat empty string as `None` so the UI can submit a blank field without
/// accidentally enabling replica set mode. Validates the name contains only
/// the characters MongoDB accepts in a replica set name.
fn deserialize_optional_replica_set<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    let trimmed = opt.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if let Some(ref name) = trimmed {
        let valid = name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !valid {
            return Err(serde::de::Error::custom(
                "replica_set must contain only ASCII letters, digits, '-', or '_'",
            ));
        }
    }
    Ok(trimmed)
}

fn default_host() -> String {
    "localhost".to_string()
}

fn default_database() -> String {
    "admin".to_string()
}

fn default_username() -> String {
    "root".to_string()
}

pub fn generate_password() -> String {
    use rand::{distributions::Alphanumeric, Rng};
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(16)
        .map(char::from)
        .collect()
}

/// Generate a MongoDB keyfile body. Mongo accepts a base64-encoded shared secret
/// between 6 and 1024 characters; we use 32 random bytes (~44 chars base64) which
/// matches what `openssl rand -base64 32` produces in MongoDB's own docs.
pub fn generate_keyfile_content() -> String {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    STANDARD.encode(bytes)
}

use super::port_util::{find_available_port, find_available_port_async, is_port_conflict_error};

pub struct MongodbService {
    name: String,
    config: Arc<RwLock<Option<MongodbRuntimeConfig>>>,
    /// Resource limits captured at init time, applied to recreate paths.
    resource_limits: Arc<RwLock<ServiceResourceLimits>>,
    docker: Arc<Docker>,
}

impl MongodbService {
    pub fn new(name: String, docker: Arc<Docker>) -> Self {
        Self {
            name,
            config: Arc::new(RwLock::new(None)),
            resource_limits: Arc::new(RwLock::new(ServiceResourceLimits::default())),
            docker,
        }
    }

    /// Returns `true` when the desired config wants a replica set but the
    /// existing container was created without `--replSet`. In that case the
    /// caller must remove and recreate the container (preserving the data
    /// volume) so the new flags take effect; a plain `start_container` would
    /// just bring up the previous standalone process.
    ///
    /// Returns `false` (and never recreates) when the container already runs
    /// in replica-set mode, or when the config is standalone — downgrading
    /// from replica set back to standalone is intentionally not supported.
    async fn container_needs_replset_recreate(
        &self,
        docker: &Docker,
        container_name: &str,
        config: &MongodbRuntimeConfig,
    ) -> Result<bool> {
        if config.replica_set.is_none() {
            return Ok(false);
        }
        let inspect = docker
            .inspect_container(container_name, None::<InspectContainerOptions>)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to inspect MongoDB container '{}' for replica-set drift check: {}",
                    container_name,
                    e
                )
            })?;
        let cmd_has_replset = inspect
            .config
            .as_ref()
            .and_then(|c| c.cmd.as_ref())
            .map(|cmd| cmd.iter().any(|arg| arg.contains("--replSet")))
            .unwrap_or(false);
        Ok(!cmd_has_replset)
    }

    fn get_mongodb_config(&self, service_config: ServiceConfig) -> Result<MongodbRuntimeConfig> {
        // After init the persisted parameters carry runtime-only fields like
        // `keyfile_content`. We must NOT round-trip those through the input
        // config — that would drop the keyfile and `From<InputConfig>` would
        // regenerate a new one, breaking the live replica set's auth.
        //
        // Detect that case by looking for `keyfile_content`. If present, the
        // parameters are already runtime-shaped and we deserialize directly.
        if service_config
            .parameters
            .get("keyfile_content")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .is_some()
        {
            let runtime: MongodbRuntimeConfig = serde_json::from_value(service_config.parameters)
                .map_err(|e| {
                anyhow::anyhow!("Failed to parse MongoDB runtime configuration: {}", e)
            })?;
            return Ok(runtime);
        }

        // First-time init or standalone (no replica set): parse as input
        // config and transform. This auto-generates password/port and, if
        // replica_set is set, generates a fresh keyfile.
        let input_config: MongodbInputConfig = serde_json::from_value(service_config.parameters)
            .map_err(|e| anyhow::anyhow!("Failed to parse MongoDB input configuration: {}", e))?;
        Ok(input_config.into())
    }

    fn get_container_name(&self) -> String {
        format!("temps-mongodb-{}", self.name)
    }

    /// The container this service actually runs in: the imported container's
    /// real name when `config.container_name` is set, otherwise the derived
    /// `temps-mongodb-{name}`. Every operation that talks to the live
    /// container must resolve through this, not `get_container_name()`
    /// directly, or it targets a synthesized name that doesn't exist for
    /// imported services.
    fn get_live_container_name(&self, config: &MongodbRuntimeConfig) -> String {
        config
            .container_name
            .clone()
            .unwrap_or_else(|| self.get_container_name())
    }

    /// Creates and starts the MongoDB container, retrying with a fresh host
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
        config: &mut MongodbRuntimeConfig,
        resource_limits: &ServiceResourceLimits,
    ) -> Result<()> {
        const MAX_ATTEMPTS: u32 = 3;
        let mut attempt_config = config.clone();
        for attempt in 1..=MAX_ATTEMPTS {
            match self
                .create_container_once(docker, &attempt_config, resource_limits)
                .await
            {
                Ok(()) => {
                    *config = attempt_config;
                    return Ok(());
                }
                Err(e) if attempt < MAX_ATTEMPTS && is_port_conflict_error(&e.to_string()) => {
                    warn!(
                        "Port {} for MongoDB container was already allocated (attempt {}/{}), retrying with a fresh port: {}",
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
                    let base_port: u16 = attempt_config.port.parse().unwrap_or(27017);
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
        config: &MongodbRuntimeConfig,
        resource_limits: &ServiceResourceLimits,
    ) -> Result<()> {
        let container_name = self.get_container_name();
        let volume_name = format!("temps-mongodb-{}-data", self.name);

        let create_volume_options = bollard::models::VolumeCreateRequest {
            name: Some(volume_name.clone()),
            driver: Some("local".to_string()),
            ..Default::default()
        };
        docker
            .create_volume(create_volume_options)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create MongoDB volume: {}", e))?;

        info!("Created MongoDB volume: {}", volume_name);

        let mut env_vars: Vec<String> = vec![
            format!("MONGO_INITDB_ROOT_USERNAME={}", config.username),
            format!("MONGO_INITDB_ROOT_PASSWORD={}", config.password),
            format!("MONGO_INITDB_DATABASE={}", config.database),
        ];

        // When replica set mode is enabled, smuggle the keyfile through an env
        // var read by a small bash wrapper (see `cmd` below). Persisting the
        // keyfile in `MongodbRuntimeConfig` means restarts use the same key,
        // which is critical — a different keyfile would invalidate the
        // existing replica set's local.system.keys.
        if let (Some(_), Some(keyfile_content)) =
            (config.replica_set.as_ref(), config.keyfile_content.as_ref())
        {
            env_vars.push(format!("TEMPS_MONGO_KEYFILE_B64={}", keyfile_content));
        }

        let mut container_labels = HashMap::new();
        container_labels.insert("temps.service".to_string(), "mongodb".to_string());
        container_labels.insert("temps.name".to_string(), self.name.clone());

        let image_tag = config.docker_image.clone();

        // Pull the image first
        info!("Pulling MongoDB image: {}", image_tag);
        let mut stream = docker.create_image(
            Some(bollard::query_parameters::CreateImageOptions {
                from_image: Some(image_tag.clone()),
                ..Default::default()
            }),
            None,
            None,
        );

        while let Some(result) = stream.next().await {
            result.map_err(|e| anyhow::anyhow!("Failed to pull MongoDB image: {}", e))?;
        }

        let mut host_config = bollard::models::HostConfig {
            port_bindings: Some(crate::utils::local_port_binding("27017/tcp", &config.port)),
            mounts: Some(vec![bollard::models::Mount {
                target: Some("/data/db".to_string()),
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

        // In replica-set mode we replace the image's CMD with a bash wrapper
        // that materializes the keyfile inside the container (with strict
        // perms required by mongod), then execs the standard entrypoint with
        // `--replSet` and `--keyFile`. The official mongo entrypoint still
        // handles MONGO_INITDB_ROOT_USERNAME/PASSWORD via the localhost
        // exception during first boot.
        //
        // We deliberately avoid mounting the keyfile from the host: that path
        // would require coordinating a host-side temp file across worker
        // nodes. Writing it inside the container at start time keeps the
        // service self-contained.
        let cmd_override: Option<Vec<String>> = config.replica_set.as_ref().map(|rs_name| {
            let escaped_rs = rs_name.replace('\'', "'\\''");
            let script = format!(
                concat!(
                    "set -e; ",
                    "printf '%s' \"$TEMPS_MONGO_KEYFILE_B64\" > /etc/mongo-keyfile; ",
                    "chmod 400 /etc/mongo-keyfile; ",
                    "chown mongodb:mongodb /etc/mongo-keyfile 2>/dev/null || true; ",
                    "exec docker-entrypoint.sh mongod ",
                    "--replSet '{}' --bind_ip_all --keyFile /etc/mongo-keyfile",
                ),
                escaped_rs
            );
            vec!["bash".to_string(), "-c".to_string(), script]
        });

        let container_config = bollard::models::ContainerCreateBody {
            image: Some(image_tag),
            exposed_ports: Some(Vec::from(["27017/tcp".to_string()])),
            env: Some(env_vars.iter().map(|s| s.to_string()).collect()),
            cmd: cmd_override,
            labels: Some(container_labels),
            host_config: Some(bollard::models::HostConfig {
                restart_policy: Some(bollard::models::RestartPolicy {
                    name: Some(bollard::models::RestartPolicyNameEnum::ALWAYS),
                    maximum_retry_count: None,
                }),
                ..host_config
            }),
            networking_config,
            healthcheck: Some(bollard::models::HealthConfig {
                test: Some(vec!["CMD-SHELL".to_string(), {
                    // Properly escape credentials for shell execution by wrapping in single quotes
                    // and escaping any single quotes within the values
                    let escaped_username = config.username.replace("'", "'\"'\"'");
                    let escaped_password = config.password.replace("'", "'\"'\"'");
                    format!(
                            "mongosh --norc --eval \"db.adminCommand('ping')\" -u '{}' -p '{}' --authenticationDatabase admin || exit 1",
                            escaped_username, escaped_password
                        )
                }]),
                interval: Some(2000000000), // 2 seconds
                timeout: Some(10000000000), // 10 seconds
                retries: Some(5),
                start_period: Some(45000000000), // 45 seconds - gives MongoDB time to initialize credentials
                start_interval: Some(2000000000), // 2 seconds
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
            .map_err(|e| anyhow::anyhow!("Failed to create MongoDB container: {}", e))?;

        docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start MongoDB container: {}", e))?;

        // Wait for container to be healthy
        self.wait_for_container_health(docker, &container.id)
            .await?;

        // Replica set mode: initiate the set after first healthy boot. This
        // is idempotent — if it's already initiated (e.g., container restart),
        // mongod replies AlreadyInitialized and we treat that as success.
        if let Some(rs_name) = config.replica_set.as_ref() {
            self.initiate_replica_set(docker, &container_name, config, rs_name)
                .await?;
        }

        info!("MongoDB container {} created and started", container.id);
        Ok(())
    }

    /// Run `rs.initiate(...)` inside the container against localhost. Uses the
    /// root credentials we know mongod will accept (they were created by the
    /// entrypoint during first-boot via the localhost exception). On a
    /// container restart the replica set is already initialized — that surfaces
    /// as `AlreadyInitialized` (code 23) and we return success.
    async fn initiate_replica_set(
        &self,
        docker: &Docker,
        container_name: &str,
        config: &MongodbRuntimeConfig,
        rs_name: &str,
    ) -> Result<()> {
        use bollard::exec::{StartExecOptions, StartExecResults};

        // Pass credentials via env to avoid quoting hazards in the shell
        // command. The replica-set name is also injected as an env var so it
        // can't break out of the JSON literal.
        let env = [
            format!("INIT_USER={}", config.username),
            format!("INIT_PASS={}", config.password),
            format!("INIT_RS={}", rs_name),
        ];
        let env_refs: Vec<&str> = env.iter().map(String::as_str).collect();

        let script = "mongosh --quiet --norc \
             -u \"$INIT_USER\" -p \"$INIT_PASS\" --authenticationDatabase admin \
             --eval 'try { rs.initiate({_id: process.env.INIT_RS, members: [{_id: 0, host: \"127.0.0.1:27017\"}]}); } catch (e) { if (e.codeName !== \"AlreadyInitialized\" && !String(e).includes(\"already initialized\")) { throw e; } print(\"replica set already initialized\"); }' 2>&1";

        let exec = docker
            .create_exec(
                container_name,
                CreateExecOptions {
                    cmd: Some(vec!["sh", "-c", script]),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    env: Some(env_refs),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to create rs.initiate exec on '{}': {}",
                    container_name,
                    e
                )
            })?;

        let start = docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: false,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start rs.initiate exec: {}", e))?;

        let mut captured = String::new();
        if let StartExecResults::Attached { mut output, .. } = start {
            while let Some(chunk) = output.next().await {
                if let Ok(log) = chunk {
                    captured.push_str(&log.to_string());
                    if captured.len() > 4096 {
                        break;
                    }
                }
            }
        }

        let inspect = docker.inspect_exec(&exec.id).await?;
        let exit_code = inspect.exit_code.unwrap_or(-1);

        if exit_code == 0 {
            info!(
                "rs.initiate completed on '{}' (replica set '{}')",
                container_name, rs_name
            );
            // Wait briefly for the node to elect itself primary so subsequent
            // operations (e.g. provision_resource creating databases) don't
            // race the election. mongod typically reaches PRIMARY in <2s for
            // a single-node set; we cap the wait at 30s.
            self.wait_for_primary(docker, container_name, config)
                .await?;
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "rs.initiate failed on '{}' with exit {}: {}",
                container_name,
                exit_code,
                captured.trim()
            ))
        }
    }

    /// Poll `db.hello()` until it reports `isWritablePrimary: true` or 30s elapse.
    /// Single-node replica sets normally elect themselves primary in <2s.
    async fn wait_for_primary(
        &self,
        docker: &Docker,
        container_name: &str,
        config: &MongodbRuntimeConfig,
    ) -> Result<()> {
        use bollard::exec::{StartExecOptions, StartExecResults};

        let env = [
            format!("INIT_USER={}", config.username),
            format!("INIT_PASS={}", config.password),
        ];
        let env_refs: Vec<&str> = env.iter().map(String::as_str).collect();
        let probe_script = "mongosh --quiet --norc \
             -u \"$INIT_USER\" -p \"$INIT_PASS\" --authenticationDatabase admin \
             --eval 'const r = db.hello(); if (!r.isWritablePrimary) { quit(2); }' 2>&1";

        let max_wait = Duration::from_secs(30);
        let start = std::time::Instant::now();
        loop {
            let exec = docker
                .create_exec(
                    container_name,
                    CreateExecOptions {
                        cmd: Some(vec!["sh", "-c", probe_script]),
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        env: Some(env_refs.clone()),
                        ..Default::default()
                    },
                )
                .await?;

            if let StartExecResults::Attached { mut output, .. } = docker
                .start_exec(
                    &exec.id,
                    Some(StartExecOptions {
                        detach: false,
                        ..Default::default()
                    }),
                )
                .await?
            {
                while output.next().await.is_some() {}
            }

            let inspect = docker.inspect_exec(&exec.id).await?;
            if inspect.exit_code == Some(0) {
                return Ok(());
            }
            if start.elapsed() > max_wait {
                return Err(anyhow::anyhow!(
                    "Replica set on '{}' did not elect a primary within {}s",
                    container_name,
                    max_wait.as_secs()
                ));
            }
            sleep(Duration::from_millis(500)).await;
        }
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
                // all (e.g. an imported container from a vanilla image with
                // no HEALTHCHECK directive — requiring an explicit HEALTHY
                // here would spin until `max_wait` every time).
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
                        "MongoDB container exited unexpectedly with code {}",
                        exit_code
                    ));
                }
            }
            sleep(delay).await;
            total_wait += delay;
            delay = std::cmp::min(delay.mul_f32(1.5), max_delay);
        }

        Err(anyhow::anyhow!("MongoDB container health check timed out"))
    }

    async fn get_mongo_client(&self) -> Result<MongoClient> {
        let config = self
            .config
            .read()
            .await
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("MongoDB not configured"))?
            .clone();

        // `directConnection=true` keeps the driver pointed at exactly this
        // host:port instead of doing replica-set topology discovery. In RS
        // mode `rs.initiate` registers the member as `127.0.0.1:27017`, which
        // the driver would then try to dial — that address is the container's
        // loopback and is unreachable from the host or from sibling
        // containers. Direct connection bypasses discovery and is safe on
        // standalone too. See ReplicaSetNoPrimary failure mode.
        let connection_string = format!(
            "mongodb://{}:{}@{}:{}/?authSource=admin&directConnection=true",
            urlencoding::encode(&config.username),
            urlencoding::encode(&config.password),
            config.host,
            config.port
        );

        let client_options = ClientOptions::parse(&connection_string).await?;
        let client = MongoClient::with_options(client_options)?;

        Ok(client)
    }

    async fn create_database(&self, db_name: &str) -> Result<()> {
        let client = self.get_mongo_client().await?;
        let db = client.database(db_name);

        // Create a collection to initialize the database
        db.create_collection("_temps_init")
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create MongoDB database: {}", e))?;

        info!("Created MongoDB database: {}", db_name);
        Ok(())
    }

    async fn drop_database(&self, db_name: &str) -> Result<()> {
        let client = self.get_mongo_client().await?;
        let db = client.database(db_name);

        db.drop()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to drop MongoDB database: {}", e))?;

        info!("Dropped MongoDB database: {}", db_name);
        Ok(())
    }

    #[allow(dead_code)]
    async fn list_databases(&self) -> Result<Vec<String>> {
        let client = self.get_mongo_client().await?;

        let databases = client
            .list_database_names()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to list MongoDB databases: {}", e))?;

        Ok(databases)
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

impl MongodbService {
    /// Build wal-g env and run `wal-g backup-push` via the resilient exec
    /// helper. The MongoDB env wires up `WALG_STREAM_CREATE_COMMAND` to
    /// invoke `mongodump --archive` so wal-g consumes its stdout.
    async fn run_walg_backup_push(
        &self,
        container_name: &str,
        walg_s3_prefix: &str,
        s3_credentials: &super::S3Credentials,
        mongodb_uri: &str,
    ) -> anyhow::Result<()> {
        let stream_create_cmd = format!("mongodump --archive --uri=\"{}\"", mongodb_uri);
        let stream_restore_cmd = format!("mongorestore --archive --drop --uri=\"{}\"", mongodb_uri);

        let mut walg_env: Vec<String> = vec![
            format!("WALG_S3_PREFIX={}", walg_s3_prefix),
            format!("AWS_ACCESS_KEY_ID={}", s3_credentials.access_key_id),
            format!("AWS_SECRET_ACCESS_KEY={}", s3_credentials.secret_key),
            format!("AWS_REGION={}", s3_credentials.region),
            format!("WALG_STREAM_CREATE_COMMAND={}", stream_create_cmd),
            format!("WALG_STREAM_RESTORE_COMMAND={}", stream_restore_cmd),
            format!("MONGODB_URI={}", mongodb_uri),
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
            vec!["sh".into(), "-c".into(), "wal-g backup-push 2>&1".into()],
            Some(walg_env),
            MONGODB_BACKUP_EXEC_TIMEOUT,
        )
        .await
        .map(|_| ())
    }

    /// Restore from a WAL-G backup stored in S3.
    ///
    /// WAL-G restore runs `wal-g backup-fetch LATEST` which downloads the backup from S3
    /// and pipes it to `mongorestore --archive` via WALG_STREAM_RESTORE_COMMAND.
    async fn restore_from_walg(
        &self,
        s3_credentials: &super::S3Credentials,
        walg_s3_prefix: &str,
        service_config: ServiceConfig,
    ) -> Result<()> {
        // Pull the optional `_alt_password` fallback out of parameters
        // before `get_mongodb_config` drops it. The orchestrator sets this
        // to the origin service's password for cross-service restores so
        // we have a second credential to try if the target's stored one
        // no longer works (e.g., a prior partial restore already wrote
        // admin.system.users with the source's hashes).
        let alt_password: Option<String> = service_config
            .parameters
            .get("_alt_password")
            .and_then(|v| v.as_str())
            .map(String::from);

        let config = self.get_mongodb_config(service_config)?;
        let container_name = self.get_live_container_name(&config);

        info!(
            "Restoring MongoDB from WAL-G backup (prefix: {}) in container '{}'",
            walg_s3_prefix, container_name
        );

        // Auth probe: figure out which password the LIVE mongod actually
        // accepts before we hand one to mongorestore. mongorestore streams
        // over the wire and will fail the whole restore if its initial
        // connection auth rejects. Candidates, in order:
        //
        //   1. target's stored password (the common case)
        //   2. alt_password if provided (covers partial-retry, where a
        //      prior run already replaced admin.system.users with origin's
        //      hash)
        //
        // Whichever succeeds is used. If neither works, fail loudly with
        // a clear message so the operator can reset creds manually.
        let mut candidates: Vec<(&str, String)> = vec![("target", config.password.clone())];
        if let Some(alt) = alt_password.as_ref() {
            if *alt != config.password {
                candidates.push(("origin", alt.clone()));
            }
        }

        let chosen_password = {
            let mut chosen: Option<(&str, String)> = None;
            for (label, pw) in &candidates {
                match self
                    .probe_mongo_auth(&container_name, &config.username, pw)
                    .await
                {
                    Ok(true) => {
                        info!(
                            "Mongo auth probe: {} password accepted by live mongod on '{}'",
                            label, container_name
                        );
                        chosen = Some((*label, pw.clone()));
                        break;
                    }
                    Ok(false) => {
                        warn!(
                            "Mongo auth probe: {} password rejected by live mongod on '{}'",
                            label, container_name
                        );
                    }
                    Err(e) => {
                        warn!(
                            "Mongo auth probe: error while testing {} password on '{}': {}. Treating as rejection.",
                            label, container_name, e
                        );
                    }
                }
            }
            chosen.ok_or_else(|| {
                anyhow::anyhow!(
                    "Mongo auth probe failed for container '{}': none of the {} candidate password(s) authenticated. The target's stored password and any origin-service fallback have both been tried. The live mongod's effective credentials may have drifted — reset via `docker exec {} mongosh --quiet --eval 'db.changeUserPassword(...)' ` or redeploy the service.",
                    container_name, candidates.len(), container_name
                )
            })?
        };

        let (chosen_label, chosen_pw) = chosen_password;
        let _ = chosen_label; // used only in logs above; retain for future

        // Build the MongoDB URI with the password we just confirmed works.
        let mongodb_uri = format!(
            "mongodb://{}:{}@localhost:{}/?authSource=admin",
            urlencoding::encode(&config.username),
            urlencoding::encode(&chosen_pw),
            MONGODB_INTERNAL_PORT
        );

        let stream_create_cmd = format!("mongodump --archive --uri=\"{}\"", mongodb_uri);
        let stream_restore_cmd = format!("mongorestore --archive --drop --uri=\"{}\"", mongodb_uri);

        let mut walg_env: Vec<String> = vec![
            format!("WALG_S3_PREFIX={}", walg_s3_prefix),
            format!("AWS_ACCESS_KEY_ID={}", s3_credentials.access_key_id),
            format!("AWS_SECRET_ACCESS_KEY={}", s3_credentials.secret_key),
            format!("AWS_REGION={}", s3_credentials.region),
            format!("WALG_STREAM_CREATE_COMMAND={}", stream_create_cmd),
            format!("WALG_STREAM_RESTORE_COMMAND={}", stream_restore_cmd),
            format!("MONGODB_URI={}", mongodb_uri),
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

        let walg_env_refs: Vec<&str> = walg_env.iter().map(|s| s.as_str()).collect();

        // Run wal-g backup-fetch LATEST inside the container. WAL-G
        // downloads from S3 and pipes to mongorestore via
        // WALG_STREAM_RESTORE_COMMAND.
        //
        // ATTACH stdout+stderr (not detached) so we can:
        //   - Surface the real error when something fails. Every
        //     restore-gone-wrong before this was bare "exit code 1" with no
        //     context, forcing us to hand-repro to diagnose.
        //   - Detect the "exit 1 but mongorestore actually succeeded"
        //     pattern — mongorestore can be chatty and emit warnings that
        //     bump the exit code while still having completed every
        //     collection. In that case we salvage success.
        let restore_cmd = vec!["sh", "-c", "wal-g backup-fetch LATEST 2>&1"];

        info!(
            "Running wal-g backup-fetch LATEST in container '{}'",
            container_name
        );

        let exec = self
            .docker
            .create_exec(
                &container_name,
                CreateExecOptions {
                    cmd: Some(restore_cmd),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    env: Some(walg_env_refs),
                    ..Default::default()
                },
            )
            .await?;

        use bollard::exec::{StartExecOptions, StartExecResults};
        use futures::StreamExt;

        let start = self
            .docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: false,
                    ..Default::default()
                }),
            )
            .await?;

        // Drain the chunk stream. Keep the last ~8 KB — enough to explain a
        // failure or confirm a chatty success without unbounded memory for
        // large-archive restores.
        let mut captured = String::new();
        const CAPTURE_TAIL_BYTES: usize = 8 * 1024;
        if let StartExecResults::Attached { mut output, .. } = start {
            while let Some(chunk) = output.next().await {
                match chunk {
                    Ok(log) => {
                        let s = log.to_string();
                        captured.push_str(&s);
                        if captured.len() > CAPTURE_TAIL_BYTES * 4 {
                            let cut = captured.len() - CAPTURE_TAIL_BYTES;
                            let safe_cut = captured
                                .char_indices()
                                .find(|(i, _)| *i >= cut)
                                .map(|(i, _)| i)
                                .unwrap_or(captured.len());
                            captured = captured.split_off(safe_cut);
                        }
                    }
                    Err(e) => {
                        captured.push_str(&format!("\n[stream error: {}]\n", e));
                    }
                }
            }
        }

        let inspect = self.docker.inspect_exec(&exec.id).await?;
        let exit_code = inspect.exit_code.unwrap_or(-1);

        if exit_code == 0 {
            info!(
                "MongoDB WAL-G restore completed successfully (exit 0) on container '{}'",
                container_name
            );
            return Ok(());
        }

        // Non-zero exit. mongorestore can exit non-zero after warnings even
        // when every document landed. Look for its completion markers in the
        // captured tail and salvage success if present.
        let looks_like_success = captured.contains("done restoring")
            || captured.contains("finished restoring")
            || captured.contains("0 document(s) failed to restore");

        if looks_like_success {
            warn!(
                "wal-g backup-fetch exited {} but mongorestore output indicates the restore completed. Treating as success. Output tail:\n{}",
                exit_code,
                captured.trim()
            );
            return Ok(());
        }

        let tail = captured.trim();
        Err(anyhow::anyhow!(
            "WAL-G backup-fetch failed with exit code {} in container '{}'. Last output:\n{}",
            exit_code,
            container_name,
            if tail.is_empty() {
                "<no output captured>".to_string()
            } else {
                tail.to_string()
            }
        ))
    }

    /// Restore from a legacy backup (pre-WAL-G .gz files created by mongodump).
    /// Falls back to the old approach: download from S3, copy into container, run mongorestore.
    async fn restore_from_legacy(
        &self,
        s3_client: &aws_sdk_s3::Client,
        backup_location: &str,
        s3_source: &temps_entities::s3_sources::Model,
        service_config: ServiceConfig,
    ) -> Result<()> {
        let config = self.get_mongodb_config(service_config)?;
        let container_name = self.get_live_container_name(&config);

        info!(
            "Restoring MongoDB from legacy backup format: {}",
            backup_location
        );

        // Download backup from S3
        let response = s3_client
            .get_object()
            .bucket(&s3_source.bucket_name)
            .key(backup_location)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to download MongoDB backup from S3: {}", e))?;

        let backup_data = response
            .body
            .collect()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read backup data: {}", e))?
            .into_bytes();

        info!("Downloaded backup, size: {} bytes", backup_data.len());

        // Create a temporary file for the backup
        let temp_file = tempfile::NamedTempFile::new()?;
        let temp_path = temp_file.path().to_str().unwrap();
        std::fs::write(temp_path, &backup_data)?;

        // Copy backup file to container
        let tar_data = {
            let mut ar = tar::Builder::new(Vec::new());
            ar.append_path_with_name(temp_path, "backup.gz")?;
            ar.finish()?;
            ar.into_inner()?
        };

        self.docker
            .upload_to_container(
                &container_name,
                Some(bollard::query_parameters::UploadToContainerOptions {
                    path: "/tmp".to_string(),
                    ..Default::default()
                }),
                body_full(tar_data.into()),
            )
            .await?;

        // Execute mongorestore inside the container
        let exec_config = CreateExecOptions {
            cmd: Some(vec![
                "mongorestore",
                "--archive=/tmp/backup.gz",
                "--gzip",
                "-u",
                &config.username,
                "-p",
                &config.password,
                "--authenticationDatabase",
                "admin",
                "--drop",
            ]),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            ..Default::default()
        };

        let exec = self
            .docker
            .create_exec(&container_name, exec_config)
            .await?;

        let output = self.docker.start_exec(&exec.id, None).await?;

        if let bollard::exec::StartExecResults::Attached { mut output, .. } = output {
            while let Some(result) = output.next().await {
                match result {
                    Ok(log_output) => match log_output {
                        bollard::container::LogOutput::StdOut { message } => {
                            let stdout_str = String::from_utf8_lossy(&message);
                            info!("mongorestore stdout: {}", stdout_str);
                        }
                        bollard::container::LogOutput::StdErr { message } => {
                            let stderr_str = String::from_utf8_lossy(&message);
                            info!("mongorestore stderr: {}", stderr_str);
                        }
                        _ => {}
                    },
                    Err(e) => {
                        error!("Error reading exec output: {}", e);
                        return Err(anyhow::anyhow!("Failed to read mongorestore output: {}", e));
                    }
                }
            }
        }

        // Clean up temporary file in container
        let cleanup_exec = self
            .docker
            .create_exec(
                &container_name,
                CreateExecOptions {
                    cmd: Some(vec!["rm", "/tmp/backup.gz"]),
                    ..Default::default()
                },
            )
            .await?;

        self.docker.start_exec(&cleanup_exec.id, None).await?;

        info!("MongoDB legacy restore completed successfully");
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

    /// Authenticate against the live mongod with a candidate password.
    ///
    /// Returns `Ok(true)` on successful auth + ping, `Ok(false)` on clean
    /// auth rejection (so the caller can fall back to another candidate),
    /// and `Err(...)` only for unexpected Docker / exec-plumbing failures
    /// that aren't attributable to the credential itself.
    ///
    /// The password is passed via env var (not argv or a shell-interp'd
    /// string) to avoid breaking on special characters like `$`, `!`, `&`.
    async fn probe_mongo_auth(
        &self,
        container_name: &str,
        username: &str,
        password: &str,
    ) -> Result<bool> {
        use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
        use futures::StreamExt;

        // Probe via env var so special chars can't break the shell. mongosh
        // reads `--password "$P"` literally.
        let probe_cmd = vec![
            "sh",
            "-c",
            "mongosh --quiet -u \"$PROBE_USER\" --authenticationDatabase admin --password \"$PROBE_PASS\" --eval 'db.runCommand({ping:1})' mongodb://127.0.0.1:27017/admin 2>&1",
        ];

        let env = [
            format!("PROBE_USER={}", username),
            format!("PROBE_PASS={}", password),
        ];
        let env_refs: Vec<&str> = env.iter().map(|s| s.as_str()).collect();

        let exec = self
            .docker
            .create_exec(
                container_name,
                CreateExecOptions {
                    cmd: Some(probe_cmd),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    env: Some(env_refs),
                    ..Default::default()
                },
            )
            .await?;

        let start = self
            .docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: false,
                    ..Default::default()
                }),
            )
            .await?;

        let mut captured = String::new();
        if let StartExecResults::Attached { mut output, .. } = start {
            while let Some(chunk) = output.next().await {
                if let Ok(log) = chunk {
                    captured.push_str(&log.to_string());
                    if captured.len() > 2048 {
                        break;
                    }
                }
            }
        }

        let inspect = self.docker.inspect_exec(&exec.id).await?;
        let exit_code = inspect.exit_code.unwrap_or(-1);

        if exit_code == 0 && captured.contains("{ ok: 1 }") {
            return Ok(true);
        }
        // Treat AuthenticationFailed as a clean rejection.
        if captured.contains("Authentication failed") {
            return Ok(false);
        }
        // Unknown state — could be container not ready, mongosh missing,
        // network glitch. Surface to the caller; it logs + falls through.
        Err(anyhow::anyhow!(
            "mongo auth probe returned unexpected result (exit {}): {}",
            exit_code,
            captured.trim()
        ))
    }

    /// Legacy MongoDB backup using mongodump via Bollard exec.
    /// Fallback for containers without WAL-G (e.g., `mongo:8.0`).
    async fn backup_to_s3_legacy(
        &self,
        s3_client: &aws_sdk_s3::Client,
        s3_source: &temps_entities::s3_sources::Model,
        subpath: &str,
        service_config: ServiceConfig,
    ) -> Result<super::BackupOutcome> {
        use bollard::exec::CreateExecOptions;

        let config = self.get_mongodb_config(service_config)?;
        let container_name = self.get_live_container_name(&config);
        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let backup_file = format!("mongodb_backup_{}.gz", timestamp);
        let backup_path = format!("{}/{}", subpath, backup_file);

        info!(
            "Starting MongoDB legacy backup for database: {}",
            config.database
        );

        let exec_config = CreateExecOptions {
            cmd: Some(vec![
                "mongodump",
                "--archive",
                "--gzip",
                "-u",
                &config.username,
                "-p",
                &config.password,
                "--authenticationDatabase",
                "admin",
                "--db",
                &config.database,
            ]),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            ..Default::default()
        };

        let exec = self
            .docker
            .create_exec(&container_name, exec_config)
            .await?;

        let output = self.docker.start_exec(&exec.id, None).await?;

        // Stream mongodump output directly to a temp file instead of buffering
        // the entire dump in memory (which caused multi-GB memory spikes).
        let temp_file = tempfile::NamedTempFile::new()?;
        let mut total_bytes: u64 = 0;
        {
            let mut writer = std::io::BufWriter::new(&temp_file);
            if let bollard::exec::StartExecResults::Attached { mut output, .. } = output {
                use futures::stream::StreamExt;
                while let Some(result) = output.next().await {
                    match result {
                        Ok(log_output) => match log_output {
                            bollard::container::LogOutput::StdOut { message } => {
                                use std::io::Write;
                                writer.write_all(&message)?;
                                total_bytes += message.len() as u64;
                            }
                            bollard::container::LogOutput::StdErr { message } => {
                                let stderr_str = String::from_utf8_lossy(&message);
                                info!("mongodump stderr: {}", stderr_str);
                            }
                            _ => {}
                        },
                        Err(e) => {
                            error!("Error reading exec output: {}", e);
                            return Err(anyhow::anyhow!("Failed to read mongodump output: {}", e));
                        }
                    }
                }
            }
            use std::io::Write;
            writer.flush()?;
        }

        if total_bytes == 0 {
            return Err(anyhow::anyhow!("Backup data is empty"));
        }

        let temp_path = temp_file.path().to_str().unwrap();
        info!("MongoDB legacy backup size: {} bytes", total_bytes);

        let body = aws_sdk_s3::primitives::ByteStream::from_path(temp_path).await?;
        s3_client
            .put_object()
            .bucket(&s3_source.bucket_name)
            .key(&backup_path)
            .body(body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to upload MongoDB backup to S3: {}", e))?;

        info!("MongoDB legacy backup uploaded to S3: {}", backup_path);
        Ok(super::BackupOutcome::new(
            backup_path,
            Some(total_bytes as i64),
        ))
    }
}

/// Internal port used by MongoDB inside the container
const MONGODB_INTERNAL_PORT: &str = "27017";

/// Build the `MONGODB_URL` exposed to user containers.
///
/// `authSource=admin` is always set because Temps provisions the connection
/// user as a root user in the `admin` database and never creates per-database
/// users. When the deployment is a single-node replica set, `directConnection=true`
/// is added so the driver skips topology discovery — the rs config advertises
/// the mongod's internal hostname, which is not always routable from app
/// containers, so SDAM would otherwise fail even though the seed host works.
fn build_mongodb_url(
    username: &str,
    password: &str,
    host: &str,
    port: &str,
    database: &str,
    replica_set: Option<&str>,
) -> String {
    let mut params = vec!["authSource=admin".to_string()];
    if replica_set.is_some() {
        params.push("directConnection=true".to_string());
    }
    format!(
        "mongodb://{}:{}@{}:{}/{}?{}",
        urlencoding::encode(username),
        urlencoding::encode(password),
        host,
        port,
        database,
        params.join("&"),
    )
}

impl MongodbService {
    /// Build the `MONGODB_*` env vars for a given per-tenant database name.
    /// Shared between `get_runtime_env_vars` and `preview_runtime_env_vars`.
    async fn build_runtime_env_vars(&self, db_name: &str) -> Result<HashMap<String, String>> {
        let config_guard = self.config.read().await;
        let config = config_guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("MongoDB not configured"))?;

        let effective_host = self.get_live_container_name(config);
        let effective_port = MONGODB_INTERNAL_PORT.to_string();

        let mut env_vars = HashMap::new();
        env_vars.insert("MONGODB_HOST".to_string(), effective_host.clone());
        env_vars.insert("MONGODB_PORT".to_string(), effective_port.clone());
        env_vars.insert("MONGODB_DATABASE".to_string(), db_name.to_string());
        env_vars.insert("MONGODB_USERNAME".to_string(), config.username.clone());
        env_vars.insert("MONGODB_PASSWORD".to_string(), config.password.clone());
        env_vars.insert(
            "MONGODB_URL".to_string(),
            build_mongodb_url(
                &config.username,
                &config.password,
                &effective_host,
                &effective_port,
                db_name,
                config.replica_set.as_deref(),
            ),
        );

        Ok(env_vars)
    }
}

#[async_trait]
impl ExternalService for MongodbService {
    fn get_effective_address(&self, service_config: ServiceConfig) -> Result<(String, String)> {
        let config = self.get_mongodb_config(service_config)?;

        if temps_core::DeploymentMode::is_docker() {
            // Docker mode: use container name and internal port
            Ok((
                self.get_live_container_name(&config),
                MONGODB_INTERNAL_PORT.to_string(),
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
        MONGODB_INTERNAL_PORT.to_string()
    }

    async fn init(&self, service_config: ServiceConfig) -> Result<HashMap<String, String>> {
        // Pull resource limits out of parameters JSON before consuming the config.
        let resource_limits = ServiceResourceLimits::from_parameters(&service_config.parameters);
        if let Err(e) = resource_limits.validate() {
            return Err(anyhow::anyhow!("Invalid resource limits: {}", e));
        }
        *self.resource_limits.write().await = resource_limits;

        // Parse input config and transform to runtime config
        let mongodb_config = self.get_mongodb_config(service_config.clone())?;
        *self.config.write().await = Some(mongodb_config.clone());

        // Serialize the full runtime config to save to database
        // This ensures auto-generated values (password, port) are persisted
        let runtime_config_json = serde_json::to_value(&mongodb_config)
            .map_err(|e| anyhow::anyhow!("Failed to serialize MongoDB runtime config: {}", e))?;

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
        let client = self.get_mongo_client().await?;

        match client
            .database("admin")
            .run_command(doc! { "ping": 1 })
            .await
        {
            Ok(_) => Ok(true),
            Err(e) => {
                error!("MongoDB health check failed: {}", e);
                Ok(false)
            }
        }
    }

    async fn health_probe(&self, service_config: ServiceConfig) -> Result<HealthProbeResult> {
        use std::time::{Duration, Instant};

        const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
        const DEGRADED_MS: u128 = 2000;

        let cfg = match self.get_mongodb_config(service_config) {
            Ok(c) => c,
            Err(e) => {
                return Ok(HealthProbeResult::down(format!(
                    "invalid mongodb config: {}",
                    e
                )))
            }
        };

        // authSource=admin because that's where we create the root user.
        // directConnection=true skips replica-set topology discovery so the
        // probe checks *this* node instead of chasing the member address
        // advertised by `rs.initiate` (`127.0.0.1:27017`, unreachable from
        // outside the container). Safe on standalone too.
        let uri = format!(
            "mongodb://{}:{}@{}:{}/?authSource=admin&directConnection=true&serverSelectionTimeoutMS=3000&connectTimeoutMS=3000",
            urlencoding::encode(&cfg.username),
            urlencoding::encode(&cfg.password),
            cfg.host,
            cfg.port
        );

        let start = Instant::now();

        // The mongodb `Client` is a pooled, Arc-backed handle that spawns a
        // per-server background monitor task and holds pooled sockets. Building
        // a fresh one every 30s health cycle (and dropping it implicitly) leaks
        // connections + monitor tasks: pool/monitor teardown on `Drop` is
        // asynchronous and the runtime never waits for it. Worse, wrapping the
        // whole connect+ping in `timeout` means a slow probe (degraded Mongo —
        // exactly when we probe most) cancels the future mid-connect, orphaning
        // a half-open socket and its monitor task.
        //
        // Fix: build the client first, bound only the connect+ping with the
        // timeout (so cancellation can't strand an in-flight connect with a
        // live client), then ALWAYS call `client.shutdown().immediate()` to
        // deterministically close every pooled connection and stop the monitor
        // before the next cycle. The client is created locally with no clones
        // or cursor handles, so `shutdown` returns promptly.
        let client = match MongoClient::with_uri_str(&uri).await {
            Ok(c) => c,
            Err(e) => {
                return Ok(HealthProbeResult::down(format!(
                    "mongodb probe to {}:{} connect failed: {}",
                    cfg.host, cfg.port, e
                )));
            }
        };

        let ping = async {
            client
                .database("admin")
                .run_command(doc! { "ping": 1 })
                .await
                .map_err(|e| format!("ping failed: {}", e))?;
            Ok::<(), String>(())
        };
        let probe_outcome = tokio::time::timeout(PROBE_TIMEOUT, ping).await;

        // Tear the pool + monitor down before returning, regardless of outcome.
        // `immediate(true)` skips waiting for in-use resources (cursors/sessions) —
        // there are none here (local client, no clones), so close promptly.
        client.shutdown().immediate(true).await;

        match probe_outcome {
            Err(_) => Ok(HealthProbeResult::down(format!(
                "mongodb probe to {}:{} timed out after {}s",
                cfg.host,
                cfg.port,
                PROBE_TIMEOUT.as_secs()
            ))),
            Ok(Err(msg)) => Ok(HealthProbeResult::down(format!(
                "mongodb probe to {}:{} {}",
                cfg.host, cfg.port, msg
            ))),
            Ok(Ok(())) => {
                let elapsed_ms = start.elapsed().as_millis();
                let response_time = i32::try_from(elapsed_ms).ok();
                if elapsed_ms > DEGRADED_MS {
                    Ok(HealthProbeResult::degraded(
                        format!("mongodb responded in {}ms (>{}ms)", elapsed_ms, DEGRADED_MS),
                        response_time,
                    ))
                } else {
                    Ok(HealthProbeResult::operational(response_time))
                }
            }
        }
    }

    fn get_type(&self) -> ServiceType {
        ServiceType::Mongodb
    }

    fn get_name(&self) -> String {
        self.name.clone()
    }

    fn get_connection_info(&self) -> Result<String> {
        let config_guard = self
            .config
            .try_read()
            .map_err(|_| anyhow::anyhow!("Failed to acquire read lock on config"))?;
        let config = config_guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("MongoDB not configured"))?;

        Ok(format!(
            "mongodb://{}:{}@{}:{}",
            urlencoding::encode(&config.username),
            urlencoding::encode(&config.password),
            config.host,
            config.port
        ))
    }

    async fn cleanup(&self) -> Result<()> {
        self.stop().await?;
        self.remove().await?;
        Ok(())
    }

    fn get_parameter_schema(&self) -> Option<serde_json::Value> {
        // Generate JSON Schema from MongodbInputConfig
        let schema = schemars::schema_for!(MongodbInputConfig);
        let mut schema_json = serde_json::to_value(schema).ok()?;

        // Add metadata about which fields are editable
        if let Some(properties) = schema_json
            .get_mut("properties")
            .and_then(|p| p.as_object_mut())
        {
            for key in properties.keys().cloned().collect::<Vec<_>>() {
                // Define which fields should be editable
                let editable = match key.as_str() {
                    "host" => false,        // Don't change host after creation
                    "port" => true,         // Port can be changed
                    "database" => false,    // Don't change database name after creation
                    "username" => false,    // Don't change username after creation
                    "password" => false,    // Password is auto-generated and cannot be changed
                    "docker_image" => true, // Docker image can be upgraded
                    // One-way: standalone -> replica set is supported in-place.
                    // The merge_updates strategy rejects unsetting or renaming.
                    "replica_set" => true,
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
        let docker = &self.docker;
        let existing_config = self.config.read().await.as_ref().cloned();
        let container_name = existing_config
            .as_ref()
            .map(|config| self.get_live_container_name(config))
            .unwrap_or_else(|| self.get_container_name());
        info!("Starting MongoDB container {}", container_name);

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

        let mut config =
            existing_config.ok_or_else(|| anyhow::anyhow!("MongoDB configuration not found"))?;

        // Imported services skip the replica-set drift-reconciliation path
        // entirely: the stop+remove+recreate below assumes Temps owns the
        // container's lifecycle and volume naming. Running that against a
        // pre-existing container the operator brought in would delete their
        // real database to "fix" a mismatch that was never Temps' to manage.
        if config.container_name.is_some() {
            if containers.is_empty() {
                return Err(anyhow::anyhow!(
                    "Imported MongoDB container '{}' not found",
                    container_name
                ));
            }
            let is_running = matches!(
                containers[0].state,
                Some(bollard::models::ContainerSummaryStateEnum::RUNNING)
            );
            if !is_running {
                docker
                    .start_container(
                        &container_name,
                        None::<bollard::query_parameters::StartContainerOptions>,
                    )
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!("Failed to start imported MongoDB container: {}", e)
                    })?;
            }
            return Ok(());
        }

        let limits = self.resource_limits.read().await.clone();
        if containers.is_empty() {
            self.create_container(docker, &mut config, &limits).await?;
            *self.config.write().await = Some(config);
        } else {
            // If the persisted config now requires `--replSet` but the
            // existing container was created in standalone mode, restarting it
            // would just bring up the old standalone again. Detect drift by
            // inspecting the container's Cmd, then recreate (preserving the
            // data volume — `remove_container` does NOT touch named volumes).
            if self
                .container_needs_replset_recreate(docker, &container_name, &config)
                .await?
            {
                info!(
                    "MongoDB container {} needs recreate to apply replica_set='{}'; \
                     removing standalone container and recreating in replica-set mode \
                     (data volume preserved)",
                    container_name,
                    config.replica_set.as_deref().unwrap_or("")
                );
                let _ = docker
                    .stop_container(
                        &container_name,
                        Some(StopContainerOptions {
                            t: Some(10),
                            signal: None,
                        }),
                    )
                    .await;
                docker
                    .remove_container(
                        &container_name,
                        Some(bollard::query_parameters::RemoveContainerOptions {
                            v: false, // keep the data volume
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "Failed to remove standalone MongoDB container before \
                             replica-set recreate: {}",
                            e
                        )
                    })?;
                self.create_container(docker, &mut config, &limits).await?;
                *self.config.write().await = Some(config);
            } else {
                docker
                    .start_container(
                        &container_name,
                        None::<bollard::query_parameters::StartContainerOptions>,
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to start MongoDB container: {}", e))?;
                info!("Started existing MongoDB container: {}", container_name);
            }
        }

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
        info!("Stopping MongoDB container {}", container_name);

        self.docker
            .stop_container(
                &container_name,
                Some(StopContainerOptions {
                    t: Some(10),
                    signal: None,
                }),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to stop MongoDB container: {}", e))?;

        info!("Stopped MongoDB container: {}", container_name);
        Ok(())
    }

    async fn remove(&self) -> Result<()> {
        let container_name = self.get_container_name();
        info!("Removing MongoDB container {}", container_name);

        // Stop the container first if it's running
        let _ = self.stop().await;

        self.docker
            .remove_container(
                &container_name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    v: true,
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to remove MongoDB container: {}", e))?;

        // Remove the volume
        let volume_name = format!("temps-mongodb-{}-data", self.name);
        let _ = self
            .docker
            .remove_volume(
                &volume_name,
                Some(bollard::query_parameters::RemoveVolumeOptions { force: true }),
            )
            .await;

        info!("Removed MongoDB container and volume");
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

        // An imported service's real container name (stored raw in
        // parameters, since the typed config isn't available here) wins
        // over the derived one.
        let effective_host = parameters
            .get("container_name")
            .cloned()
            .unwrap_or_else(|| self.get_container_name());
        let effective_port = MONGODB_INTERNAL_PORT.to_string();
        let replica_set = parameters.get("replica_set").map(String::as_str);

        let mut env_vars = HashMap::new();
        env_vars.insert("MONGODB_HOST".to_string(), effective_host.clone());
        env_vars.insert("MONGODB_PORT".to_string(), effective_port.clone());
        env_vars.insert("MONGODB_DATABASE".to_string(), database.clone());
        env_vars.insert("MONGODB_USERNAME".to_string(), username.clone());
        env_vars.insert("MONGODB_PASSWORD".to_string(), password.clone());
        env_vars.insert(
            "MONGODB_URL".to_string(),
            build_mongodb_url(
                username,
                password,
                &effective_host,
                &effective_port,
                database,
                replica_set,
            ),
        );

        Ok(env_vars)
    }

    fn get_docker_environment_variables(
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

        // An imported service's real container name (stored raw in
        // parameters, since the typed config isn't available here) wins
        // over the derived one.
        let effective_host = parameters
            .get("container_name")
            .cloned()
            .unwrap_or_else(|| self.get_container_name());
        let effective_port = MONGODB_INTERNAL_PORT.to_string();
        let replica_set = parameters.get("replica_set").map(String::as_str);

        let mut env_vars = HashMap::new();
        env_vars.insert("MONGODB_HOST".to_string(), effective_host.clone());
        env_vars.insert("MONGODB_PORT".to_string(), effective_port.clone());
        env_vars.insert("MONGODB_DATABASE".to_string(), database.clone());
        env_vars.insert("MONGODB_USERNAME".to_string(), username.clone());
        env_vars.insert("MONGODB_PASSWORD".to_string(), password.clone());
        env_vars.insert(
            "MONGODB_URL".to_string(),
            build_mongodb_url(
                username,
                password,
                &effective_host,
                &effective_port,
                database,
                replica_set,
            ),
        );

        Ok(env_vars)
    }

    async fn provision_resource(
        &self,
        _service_config: ServiceConfig,
        project_id: &str,
        environment: &str,
    ) -> Result<super::LogicalResource> {
        let db_name = format!("{}_{}", project_id, environment);

        // Create the database
        self.create_database(&db_name).await?;

        let config = self
            .config
            .read()
            .await
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("MongoDB not configured"))?
            .clone();

        let mut credentials = HashMap::new();
        credentials.insert("host".to_string(), config.host);
        credentials.insert("port".to_string(), config.port);
        credentials.insert("database".to_string(), db_name.clone());
        credentials.insert("username".to_string(), config.username);
        credentials.insert("password".to_string(), config.password);

        Ok(super::LogicalResource {
            name: db_name,
            resource_type: "mongodb_database".to_string(),
            credentials,
        })
    }

    async fn deprovision_resource(&self, project_id: &str, environment: &str) -> Result<()> {
        let db_name = format!("{}_{}", project_id, environment);
        self.drop_database(&db_name).await
    }

    fn get_runtime_env_definitions(&self) -> Vec<RuntimeEnvVar> {
        vec![
            RuntimeEnvVar {
                name: "MONGODB_DATABASE".to_string(),
                description: "MongoDB database name for this project/environment".to_string(),
                example: "project1_production".to_string(),
                sensitive: false,
            },
            RuntimeEnvVar {
                name: "MONGODB_URL".to_string(),
                description: "Full MongoDB connection URL".to_string(),
                example: "mongodb://username:password@localhost:27017/project1_production"
                    .to_string(),
                sensitive: true, // Contains password
            },
            RuntimeEnvVar {
                name: "MONGODB_HOST".to_string(),
                description: "MongoDB host".to_string(),
                example: "localhost".to_string(),
                sensitive: false,
            },
            RuntimeEnvVar {
                name: "MONGODB_PORT".to_string(),
                description: "MongoDB port".to_string(),
                example: "27017".to_string(),
                sensitive: false,
            },
            RuntimeEnvVar {
                name: "MONGODB_USERNAME".to_string(),
                description: "MongoDB username".to_string(),
                example: "root".to_string(),
                sensitive: false,
            },
            RuntimeEnvVar {
                name: "MONGODB_PASSWORD".to_string(),
                description: "MongoDB password".to_string(),
                example: "password".to_string(),
                sensitive: true,
            },
        ]
    }

    async fn get_runtime_env_vars(
        &self,
        _config: ServiceConfig,
        project_id: &str,
        environment: &str,
    ) -> Result<HashMap<String, String>> {
        let db_name = format!("{}_{}", project_id, environment);

        // Create the database if it doesn't exist
        self.create_database(&db_name).await?;
        self.build_runtime_env_vars(&db_name).await
    }

    async fn preview_runtime_env_vars(
        &self,
        _config: ServiceConfig,
        project_id: &str,
        environment: &str,
    ) -> Result<HashMap<String, String>> {
        let db_name = format!("{}_{}", project_id, environment);
        // Preview: skip create_database so the UI doesn't provision DBs.
        self.build_runtime_env_vars(&db_name).await
    }

    fn get_local_address(&self, service_config: ServiceConfig) -> Result<String> {
        let port = service_config
            .parameters
            .get("port")
            .ok_or_else(|| anyhow::anyhow!("Missing port parameter"))?;

        Ok(format!("localhost:{}", port))
    }

    /// Backup MongoDB data to S3.
    ///
    /// Detects whether the container has WAL-G installed:
    /// - **WAL-G available**: Uses `wal-g backup-push` with mongodump stream. Zero data
    ///   flows through the Temps process.
    /// - **WAL-G not available** (legacy images like `mongo:8.0`): Falls back to
    ///   mongodump via Bollard exec, buffering output and uploading to S3.
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

        let mongodb_config = self.get_mongodb_config(service_config.clone())?;
        let container_name = self.get_live_container_name(&mongodb_config);

        if !self.container_has_walg(&container_name).await {
            info!(
                "WAL-G not found in container '{}', falling back to legacy mongodump backup",
                container_name
            );
            return self
                .backup_to_s3_legacy(s3_client, s3_source, subpath, service_config)
                .await;
        }

        info!("Starting MongoDB backup to S3 via WAL-G");

        let config = self.get_mongodb_config(service_config)?;

        let metadata = serde_json::json!({
            "service_type": "mongodb",
            "service_name": self.name,
            "backup_tool": "wal-g",
        });

        let backup_record = temps_entities::external_service_backups::Entity::insert(
            temps_entities::external_service_backups::ActiveModel {
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
            },
        )
        .exec_with_returning(pool)
        .await?;

        let walg_s3_prefix = format!(
            "s3://{}/{}/walg",
            s3_credentials.bucket_name,
            subpath_root.trim_matches('/')
        );
        let s3_list_prefix = format!("{}/walg/", subpath_root.trim_matches('/'));

        let mongodb_uri = format!(
            "mongodb://{}:{}@localhost:{}/?authSource=admin",
            urlencoding::encode(&config.username),
            urlencoding::encode(&config.password),
            MONGODB_INTERNAL_PORT
        );

        let result = self
            .run_walg_backup_push(
                &container_name,
                &walg_s3_prefix,
                s3_credentials,
                &mongodb_uri,
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
                            "MongoDB WAL-G backup succeeded but failed to compute size from S3: {}",
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
                    "MongoDB WAL-G backup completed successfully (prefix: {}, size: {:?})",
                    walg_s3_prefix, size_bytes
                );
                Ok(super::BackupOutcome::new(walg_s3_prefix, size_bytes))
            }
            Err(e) => {
                let error_msg = format!("MongoDB WAL-G backup failed: {}", e);
                error!("{}", error_msg);
                let mut backup_update: temps_entities::external_service_backups::ActiveModel =
                    backup_record.clone().into();
                backup_update.state = Set("failed".to_string());
                backup_update.error_message = Set(Some(error_msg.clone()));
                backup_update.finished_at = Set(Some(Utc::now()));
                if let Err(update_err) = backup_update.update(pool).await {
                    error!(
                        "Failed to mark MongoDB backup row as failed: {}",
                        update_err
                    );
                }
                Err(e)
            }
        }
    }

    /// Restore MongoDB data from S3 using WAL-G or legacy format
    ///
    /// For WAL-G backups (s3:// prefix): Runs `wal-g backup-fetch LATEST` inside the container.
    /// WAL-G downloads the backup from S3 and pipes it to mongorestore via WALG_STREAM_RESTORE_COMMAND.
    ///
    /// For legacy backups (.gz files): Falls back to the old approach — downloads from S3,
    /// copies into the container, and runs mongorestore.
    async fn restore_from_s3(
        &self,
        s3_client: &aws_sdk_s3::Client,
        s3_credentials: &super::S3Credentials,
        backup_location: &str,
        s3_source: &temps_entities::s3_sources::Model,
        service_config: ServiceConfig,
    ) -> Result<()> {
        info!("Starting MongoDB restore from S3: {}", backup_location);

        if backup_location.starts_with("s3://") {
            // WAL-G backup: use wal-g backup-fetch
            self.restore_from_walg(s3_credentials, backup_location, service_config)
                .await
        } else {
            // Legacy backup: fall back to old mongorestore approach
            self.restore_from_legacy(s3_client, backup_location, s3_source, service_config)
                .await
        }
    }

    fn get_default_docker_image(&self) -> (String, String) {
        // Return (image_name, version)
        ("gotempsh/mongodb-walg".to_string(), "8.0".to_string())
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
                "Failed to get current docker image for MongoDB container"
            ))
        }
    }

    fn get_default_version(&self) -> String {
        "8.0".to_string()
    }

    async fn get_current_version(&self) -> Result<String> {
        let (_, version) = self.get_current_docker_image().await?;
        Ok(version)
    }

    async fn upgrade(&self, old_config: ServiceConfig, new_config: ServiceConfig) -> Result<()> {
        info!("Starting MongoDB upgrade");

        let _old_mongodb_config = self.get_mongodb_config(old_config)?;
        let mut new_mongodb_config = self.get_mongodb_config(new_config)?;

        // Verify the new image can be pulled BEFORE stopping the old container
        info!(
            "Verifying new Docker image is available: {}",
            new_mongodb_config.docker_image
        );
        self.verify_image_pullable(&new_mongodb_config.docker_image)
            .await?;
        info!("New Docker image verified and is available");

        // Stop the old container
        info!("Stopping old MongoDB container");
        self.stop().await?;

        // Create container with new image (keeping the same volume for data persistence)
        info!("Starting MongoDB container with new image");
        let limits = self.resource_limits.read().await.clone();
        self.create_container(&self.docker, &mut new_mongodb_config, &limits)
            .await?;
        *self.config.write().await = Some(new_mongodb_config);

        info!("MongoDB upgrade completed successfully");
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
        // service must target this, not the derived `temps-mongodb-{name}`.
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

        // Extract version from image name (e.g., "mongo:7" -> "7")
        let version = if let Some(tag_pos) = image.rfind(':') {
            image[tag_pos + 1..].to_string()
        } else {
            "7".to_string()
        };

        // Extract port from additional config if provided, otherwise use 27017
        let port = additional_config
            .get("port")
            .and_then(|v| v.as_str())
            .unwrap_or("27017")
            .to_string();

        // Extract credentials
        let username = credentials.get("username").cloned();
        let password = credentials.get("password").cloned();
        let database = credentials
            .get("database")
            .cloned()
            .unwrap_or_else(|| "admin".to_string());

        // Build connection URL for verification
        let connection_url = if let (Some(user), Some(pass)) = (&username, &password) {
            format!(
                "mongodb://{}:{}@localhost:{}/{}",
                urlencoding::encode(user),
                urlencoding::encode(pass),
                port,
                database
            )
        } else {
            format!("mongodb://localhost:{}", port)
        };

        // Verify connection to the imported service. Connects directly with
        // `.await` on the current runtime — spinning up a nested
        // `tokio::runtime::Runtime` and calling `block_on` here panics with
        // "Cannot start a runtime from within a runtime", since this
        // `async fn` is already driven by one.
        use std::future::IntoFuture;
        let client = mongodb::Client::with_uri_str(&connection_url)
            .await
            .map_err(|e| anyhow::anyhow!("Invalid MongoDB connection URL: {}", e))?;
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.list_databases().into_future(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("MongoDB connection timed out after 5 seconds"))?
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to connect to MongoDB at localhost:{} with provided credentials: {}",
                port,
                e
            )
        })?;
        info!("Successfully verified MongoDB connection for import");

        let network_ready = match ensure_network_exists(&self.docker).await {
            Ok(()) => true,
            Err(e) => {
                warn!(
                    "Failed to ensure Temps Docker network before MongoDB import attach: {:?}",
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
                    "Attached imported MongoDB container '{}' to {}",
                    imported_container_name, network_name
                ),
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 403, ..
                }) => debug!(
                    "Imported MongoDB container '{}' is already attached to {}",
                    imported_container_name, network_name
                ),
                Err(e) => warn!(
                    "Failed to attach imported MongoDB container '{}' to {}: {}",
                    imported_container_name, network_name, e
                ),
            }
        }

        // Build the ServiceConfig for registration
        let config = ServiceConfig {
            name: service_name,
            service_type: ServiceType::Mongodb,
            version: Some(version),
            parameters: serde_json::json!({
                "host": "localhost",
                "port": port,
                "username": username.unwrap_or_default(),
                "password": password.unwrap_or_default(),
                "database": database,
                "docker_image": image,
                "container_name": imported_container_name,
            }),
        };

        info!(
            "Successfully imported MongoDB service '{}' from container",
            config.name
        );
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_values() {
        assert_eq!(default_host(), "localhost");
        assert_eq!(default_username(), "root");
        assert_eq!(
            default_docker_image(),
            "gotempsh/mongodb-walg:8.0".to_string()
        );
    }

    #[test]
    fn test_build_mongodb_url_standalone() {
        let url = build_mongodb_url("root", "p@ss/word", "mongo", "27017", "mydb", None);
        assert_eq!(
            url,
            "mongodb://root:p%40ss%2Fword@mongo:27017/mydb?authSource=admin"
        );
    }

    #[test]
    fn test_build_mongodb_url_replica_set() {
        let url = build_mongodb_url("root", "secret", "mongo", "27017", "mydb", Some("rs0"));
        assert_eq!(
            url,
            "mongodb://root:secret@mongo:27017/mydb?authSource=admin&directConnection=true"
        );
    }

    #[test]
    fn test_generate_password() {
        let password = generate_password();
        assert_eq!(password.len(), 16);
        assert!(password.chars().all(|c| c.is_alphanumeric()));
    }

    #[test]
    fn test_generate_password_uniqueness() {
        // Generate multiple passwords and verify they are unique
        let password1 = generate_password();
        let password2 = generate_password();
        let password3 = generate_password();

        assert_ne!(password1, password2, "Passwords should be unique");
        assert_ne!(password2, password3, "Passwords should be unique");
        assert_ne!(password1, password3, "Passwords should be unique");

        // All should be valid
        assert_eq!(password1.len(), 16);
        assert_eq!(password2.len(), 16);
        assert_eq!(password3.len(), 16);
    }

    #[test]
    fn test_container_name() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = MongodbService::new("test-service".to_string(), docker);
        assert_eq!(service.get_container_name(), "temps-mongodb-test-service");
    }

    #[test]
    fn test_get_effective_address_docker_mode_uses_imported_container_name() {
        let _lock = crate::externalsvc::DEPLOYMENT_MODE_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("DEPLOYMENT_MODE", "docker") };

        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = MongodbService::new("imported-svc".to_string(), docker);

        let config = ServiceConfig {
            name: "imported-svc".to_string(),
            service_type: ServiceType::Mongodb,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "27018",
                "database": "admin",
                "username": "root",
                "password": "testpass",
                "container_name": "legacy-mongo",
            }),
        };

        let (host, port) = service.get_effective_address(config).unwrap();
        // The imported container name wins over the derived
        // `temps-mongodb-{name}`.
        assert_eq!(host, "legacy-mongo");
        assert_eq!(port, "27017");

        unsafe { std::env::remove_var("DEPLOYMENT_MODE") };
    }

    #[test]
    fn test_container_name_is_not_a_user_input() {
        // container_name is derived from the service name at creation time
        // (`temps-mongodb-{name}`), never supplied by the client — same as
        // MariaDB (see mariadb.rs's identical test). The create form is
        // generated from this schema, so the field must not appear in it.
        let schema = serde_json::to_value(schemars::schema_for!(MongodbInputConfig)).unwrap();
        assert!(
            !schema.to_string().contains("container_name"),
            "container_name leaked into the MongoDB create schema"
        );
    }

    #[test]
    fn test_service_type() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = MongodbService::new("test-service".to_string(), docker);
        assert_eq!(service.get_type(), ServiceType::Mongodb);
    }

    #[test]
    fn test_parameter_schema() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = MongodbService::new("test-schema".to_string(), docker);

        // Get the parameter schema
        let schema_opt = service.get_parameter_schema();
        assert!(schema_opt.is_some(), "Schema should be generated");

        let schema = schema_opt.unwrap();

        // Verify schema structure
        let schema_obj = schema.as_object().expect("Schema should be an object");

        // Check for schema metadata
        assert!(
            schema_obj.contains_key("$schema"),
            "Should have $schema field"
        );
        assert!(schema_obj.contains_key("title"), "Should have title field");
        assert!(
            schema_obj.contains_key("description"),
            "Should have description field"
        );
        assert!(
            schema_obj.contains_key("properties"),
            "Should have properties field"
        );

        // Verify title and description
        assert_eq!(
            schema_obj.get("title").and_then(|v| v.as_str()),
            Some("MongoDB Configuration"),
            "Title should match"
        );

        // Verify properties
        let properties = schema_obj
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("Properties should be an object");

        // Check for expected fields
        let expected_fields = vec![
            "host",
            "port",
            "database",
            "username",
            "password",
            "docker_image",
            "replica_set",
        ];
        for field in &expected_fields {
            assert!(
                properties.contains_key(*field),
                "Schema should contain '{}' field",
                field
            );
        }

        // Verify host field has default
        let host_field = properties
            .get("host")
            .and_then(|v| v.as_object())
            .expect("host field should be an object");
        assert_eq!(
            host_field.get("default").and_then(|v| v.as_str()),
            Some("localhost")
        );

        // Verify password field description
        let password_field = properties
            .get("password")
            .and_then(|v| v.as_object())
            .expect("password field should be an object");
        let password_desc = password_field.get("description").and_then(|v| v.as_str());
        assert!(password_desc.is_some());
        assert!(password_desc.unwrap().contains("auto-generated"));
    }

    #[test]
    fn test_parameter_schema_editable_fields() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = MongodbService::new("test-editable".to_string(), docker);

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
            ("password", false),
            ("docker_image", true),
            ("replica_set", true),
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
    fn test_default_docker_image() {
        assert_eq!(
            default_docker_image(),
            "gotempsh/mongodb-walg:8.0".to_string(),
            "Default docker_image should be gotempsh/mongodb-walg:8.0"
        );
    }

    #[test]
    fn test_docker_image_configuration() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let _service = MongodbService::new("test-config".to_string(), docker);

        // Create config with specific docker_image
        let config = ServiceConfig {
            name: "test-mongo".to_string(),
            service_type: super::ServiceType::Mongodb,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "27017",
                "database": "testdb",
                "username": "testuser",
                "password": "testpass123",
                "docker_image": "gotempsh/mongodb-walg:8.0"
            }),
        };

        // Verify configuration contains docker_image
        assert_eq!(
            config
                .parameters
                .get("docker_image")
                .and_then(|v| v.as_str()),
            Some("gotempsh/mongodb-walg:8.0")
        );
    }

    #[test]
    fn test_mongodb_upgrade_config() {
        // Test simulated upgrade from MongoDB 7.0 to 8.0
        let old_config = ServiceConfig {
            name: "test-mongo".to_string(),
            service_type: super::ServiceType::Mongodb,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "27017",
                "database": "testdb",
                "username": "testuser",
                "password": "testpass123",
                "docker_image": "gotempsh/mongodb-walg:7.0"
            }),
        };

        let new_config = ServiceConfig {
            name: "test-mongo".to_string(),
            service_type: super::ServiceType::Mongodb,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "27017",
                "database": "testdb",
                "username": "testuser",
                "password": "testpass123",
                "docker_image": "gotempsh/mongodb-walg:8.0"
            }),
        };

        // Verify upgrade configuration
        let old_image = old_config
            .parameters
            .get("docker_image")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let new_image = new_config
            .parameters
            .get("docker_image")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        assert_eq!(
            old_image, "gotempsh/mongodb-walg:7.0",
            "Old docker_image should be gotempsh/mongodb-walg:7.0"
        );
        assert_eq!(
            new_image, "gotempsh/mongodb-walg:8.0",
            "New docker_image should be gotempsh/mongodb-walg:8.0"
        );
    }

    #[test]
    fn test_import_service_config_creation() {
        let config = ServiceConfig {
            name: "test-mongodb-import".to_string(),
            service_type: ServiceType::Mongodb,
            version: Some("7.0".to_string()),
            parameters: serde_json::json!({
                "host": "localhost",
                "port": 27017,
                "username": "mongouser",
                "password": "mongopass",
                "database": "admin",
                "docker_image": "gotempsh/mongodb-walg:7.0",
                "container_id": "def456ghi789",
            }),
        };

        assert_eq!(config.name, "test-mongodb-import");
        assert_eq!(config.service_type, ServiceType::Mongodb);
        assert_eq!(config.version, Some("7.0".to_string()));
        assert_eq!(config.parameters["port"], 27017);
    }

    #[test]
    fn test_import_mongodb_version_extraction() {
        let test_cases = vec![
            ("gotempsh/mongodb-walg:7.0", "7.0"),
            ("mongo:latest", "latest"),
            ("mongo:6.0-ubuntu", "6.0-ubuntu"),
            ("gotempsh/mongodb-walg:8.0", "8.0"),
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
        // MongoDB requires username, password, port, database

        assert!(!credentials.contains_key("username"));
        assert!(!credentials.contains_key("password"));
        assert!(!credentials.contains_key("port"));
        assert!(!credentials.contains_key("database"));
    }

    #[test]
    fn test_import_connection_string_format() {
        let username = "mongouser";
        let password = "mongopassword";
        let port = 27017;

        let connection_url = format!("mongodb://{}:{}@localhost:{}", username, password, port);

        assert!(connection_url.contains("mongodb://"));
        assert!(connection_url.contains("mongouser"));
        assert!(connection_url.contains("mongopassword"));
        assert!(connection_url.contains("localhost"));
        assert!(connection_url.contains("27017"));
    }

    #[test]
    fn test_import_credential_extraction() {
        let mut credentials = std::collections::HashMap::new();
        credentials.insert("username".to_string(), "mongouser".to_string());
        credentials.insert("password".to_string(), "mongopass".to_string());
        credentials.insert("port".to_string(), "27017".to_string());
        credentials.insert("database".to_string(), "admin".to_string());

        assert_eq!(
            credentials.get("username").map(|s| s.as_str()),
            Some("mongouser")
        );
        assert_eq!(
            credentials.get("password").map(|s| s.as_str()),
            Some("mongopass")
        );
        assert_eq!(credentials.get("port").map(|s| s.as_str()), Some("27017"));
        assert_eq!(
            credentials.get("database").map(|s| s.as_str()),
            Some("admin")
        );
    }

    #[test]
    fn test_replica_set_default_is_none() {
        let input: MongodbInputConfig = serde_json::from_value(serde_json::json!({
            "host": "localhost",
            "database": "admin",
            "username": "root",
            "docker_image": "gotempsh/mongodb-walg:8.0",
        }))
        .expect("should deserialize without replica_set");
        assert!(input.replica_set.is_none());

        let runtime: MongodbRuntimeConfig = input.into();
        assert!(runtime.replica_set.is_none());
        assert!(runtime.keyfile_content.is_none());
    }

    #[test]
    fn test_replica_set_some_generates_keyfile() {
        let input: MongodbInputConfig = serde_json::from_value(serde_json::json!({
            "host": "localhost",
            "database": "admin",
            "username": "root",
            "docker_image": "gotempsh/mongodb-walg:8.0",
            "replica_set": "rs0",
        }))
        .expect("should deserialize with replica_set");
        assert_eq!(input.replica_set.as_deref(), Some("rs0"));

        let runtime: MongodbRuntimeConfig = input.into();
        assert_eq!(runtime.replica_set.as_deref(), Some("rs0"));
        let kf = runtime
            .keyfile_content
            .expect("keyfile should be generated");
        // Base64 of 32 bytes is 44 chars including padding
        assert_eq!(kf.len(), 44);
    }

    #[test]
    fn test_replica_set_empty_string_treated_as_none() {
        let input: MongodbInputConfig = serde_json::from_value(serde_json::json!({
            "host": "localhost",
            "database": "admin",
            "username": "root",
            "docker_image": "gotempsh/mongodb-walg:8.0",
            "replica_set": "",
        }))
        .expect("empty replica_set should deserialize");
        assert!(input.replica_set.is_none());
    }

    #[test]
    fn test_replica_set_rejects_invalid_chars() {
        let result: Result<MongodbInputConfig, _> = serde_json::from_value(serde_json::json!({
            "host": "localhost",
            "database": "admin",
            "username": "root",
            "docker_image": "gotempsh/mongodb-walg:8.0",
            "replica_set": "bad name with spaces",
        }));
        assert!(result.is_err(), "spaces in replica_set should fail");
    }

    #[test]
    fn test_runtime_config_round_trip_preserves_keyfile() {
        // First-time init: input has replica_set, no keyfile
        let input: MongodbInputConfig = serde_json::from_value(serde_json::json!({
            "host": "localhost",
            "database": "admin",
            "username": "root",
            "password": "secret",
            "docker_image": "gotempsh/mongodb-walg:8.0",
            "replica_set": "rs0",
        }))
        .unwrap();
        let runtime: MongodbRuntimeConfig = input.into();
        let original_keyfile = runtime.keyfile_content.clone().unwrap();

        // Persisted as JSON, then loaded back via the runtime path
        let persisted = serde_json::to_value(&runtime).unwrap();
        assert!(persisted.get("keyfile_content").is_some());

        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => Arc::new(d),
            Err(_) => return, // No docker on host; skip this round-trip path
        };
        let service = MongodbService::new("test-rt".to_string(), docker);
        let svc_config = ServiceConfig {
            name: "test-rt".into(),
            service_type: ServiceType::Mongodb,
            version: None,
            parameters: persisted,
        };
        let reloaded = service
            .get_mongodb_config(svc_config)
            .expect("should reload runtime config");
        // Critical: the keyfile must NOT be regenerated on reload
        assert_eq!(
            reloaded.keyfile_content.as_deref(),
            Some(original_keyfile.as_str())
        );
        assert_eq!(reloaded.replica_set.as_deref(), Some("rs0"));
    }

    #[test]
    fn test_generate_keyfile_content_is_random() {
        let a = generate_keyfile_content();
        let b = generate_keyfile_content();
        assert_eq!(a.len(), 44);
        assert_eq!(b.len(), 44);
        assert_ne!(a, b);
    }

    /// Test backup and restore of MongoDB to/from S3 using real Docker containers
    /// This test uses MongoDB and MinIO (S3-compatible) containers
    /// Demonstrates the use of test_utils for backup/restore testing
    ///
    /// `flavor = "multi_thread"` is required because `MinioTestContainer`'s
    /// `Drop` impl calls `tokio::task::block_in_place`, which panics on the
    /// default current-thread runtime.
    #[cfg(feature = "docker-tests")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_mongodb_backup_and_restore_to_s3() {
        // Whole-test wall-clock budget. Anything above this is a hang — fail
        // loudly with a diagnostic instead of stalling the CI runner for 90 min.
        // See incident: GitHub run 25806816492 (PR #89) burned 90 min on this
        // test plus the Redis counterpart because something downstream of the
        // MinIO/Mongo container startup never returned.
        const TEST_TIMEOUT: Duration = Duration::from_secs(300);

        tokio::time::timeout(TEST_TIMEOUT, run_mongodb_backup_and_restore_to_s3())
            .await
            .expect("test_mongodb_backup_and_restore_to_s3 exceeded 300s — likely hung on MinIO/Mongo/S3 wait");
    }

    /// Body of `test_mongodb_backup_and_restore_to_s3`, extracted so the outer
    /// test can wrap it in `tokio::time::timeout`.
    #[cfg(feature = "docker-tests")]
    async fn run_mongodb_backup_and_restore_to_s3() {
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

        println!("✓ Docker is available");

        // Test configuration
        let service_name = format!("test-backup-{}", chrono::Utc::now().timestamp());
        let username = "testuser";
        let password = "testpass123";
        let database = "testdb";
        let test_port = find_available_port(27018).expect("No available port found");

        println!("✓ Test configuration: MongoDB port {}", test_port);

        // Step 1 & 2: Start MinIO container and set up S3 (using test utilities)
        println!("Step 1: Starting MinIO container and setting up S3...");
        let minio = match MinioTestContainer::start(docker.clone(), "test-backups").await {
            Ok(m) => m,
            Err(e) => {
                let error_msg = e.to_string();
                if error_msg.contains("certificate")
                    || error_msg.contains("TrustStore")
                    || error_msg.contains("panicked")
                {
                    println!("❌ Skipping MongoDB backup test: TLS certificate issue");
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

        // Step 3: Create MongoDB service and start container
        println!("Step 3: Starting MongoDB container...");
        let service = MongodbService::new(service_name.clone(), docker.clone());

        let mut mongodb_config = MongodbRuntimeConfig {
            host: "localhost".to_string(),
            port: test_port.to_string(),
            database: database.to_string(),
            username: username.to_string(),
            password: password.to_string(),
            docker_image: "gotempsh/mongodb-walg:8.0".to_string(),
            replica_set: None,
            keyfile_content: None,
            container_name: None,
        };

        *service.config.write().await = Some(mongodb_config.clone());

        // Create and start MongoDB container
        service
            .create_container(
                &docker,
                &mut mongodb_config,
                &ServiceResourceLimits::default(),
            )
            .await
            .expect("Failed to create MongoDB container");
        println!("✓ MongoDB container started and healthy");

        // Step 4: Insert test data into MongoDB
        println!("Step 4: Inserting test data...");
        let client = service
            .get_mongo_client()
            .await
            .expect("Failed to get MongoDB client");
        let db = client.database(database);
        let collection = db.collection::<mongodb::bson::Document>("test_collection");

        // Insert test documents
        let test_docs = vec![
            doc! { "name": "Alice", "age": 30, "city": "New York" },
            doc! { "name": "Bob", "age": 25, "city": "San Francisco" },
            doc! { "name": "Charlie", "age": 35, "city": "Boston" },
        ];
        collection
            .insert_many(&test_docs)
            .await
            .expect("Failed to insert test data");

        let count_before = collection
            .count_documents(doc! {})
            .await
            .expect("Failed to count documents");
        assert_eq!(count_before, 3, "Should have 3 documents before backup");
        println!("✓ Inserted {} test documents", count_before);

        // Step 5: Backup MongoDB to S3 (using test utilities for mock entities)
        println!("Step 5: Backing up MongoDB to S3...");

        // Create mock entities using test utilities
        let backup_record = create_mock_backup("backups/test");
        let db_conn = create_mock_db()
            .await
            .expect("Failed to create mock database");
        let external_service = create_mock_external_service(service_name.clone(), "mongodb", "8.0");

        let service_config = ServiceConfig {
            name: service_name.clone(),
            service_type: ServiceType::Mongodb,
            version: Some("8.0".to_string()),
            parameters: serde_json::to_value(&mongodb_config).expect("Failed to serialize config"),
        };

        let s3_creds = minio.s3_credentials();
        let backup_outcome = service
            .backup_to_s3(
                &minio.s3_client,
                &s3_creds,
                backup_record,
                &minio.s3_source,
                "backups/test",
                "backups",
                &db_conn,
                &external_service,
                service_config.clone(),
            )
            .await
            .expect("Failed to backup MongoDB to S3");
        let backup_path = backup_outcome.location;

        println!("✓ Backup created at: {}", backup_path);

        // Verify backup exists in S3 by listing objects under the WAL-G prefix.
        // WAL-G stores backups under the prefix (e.g., backups/test/walg/basebackups_005/...),
        // not at the exact prefix path, so we use list_objects instead of head_object.
        let walg_prefix = backup_path
            .strip_prefix(&format!("s3://{}/", minio.bucket_name))
            .unwrap_or(&backup_path);
        let list_result = minio
            .s3_client
            .list_objects_v2()
            .bucket(&minio.bucket_name)
            .prefix(walg_prefix)
            .max_keys(5)
            .send()
            .await
            .expect("Failed to list S3 objects");
        let object_count = list_result.contents().len();
        assert!(
            object_count > 0,
            "Backup files should exist in S3 under prefix '{}'",
            walg_prefix
        );
        println!(
            "✓ Backup verified in S3 ({} objects under prefix '{}')",
            object_count, walg_prefix
        );

        // Step 6: Drop the database to simulate data loss
        println!("Step 6: Dropping database to simulate data loss...");
        service
            .drop_database(database)
            .await
            .expect("Failed to drop database");

        // Verify data is gone
        let db_after_drop = client.database(database);
        let collection_after_drop =
            db_after_drop.collection::<mongodb::bson::Document>("test_collection");
        let count_after_drop = collection_after_drop
            .count_documents(doc! {})
            .await
            .expect("Failed to count documents");
        assert_eq!(count_after_drop, 0, "Should have 0 documents after drop");
        println!("✓ Database dropped successfully");

        // Step 7: Restore from S3
        println!("Step 7: Restoring MongoDB from S3...");
        service
            .restore_from_s3(
                &minio.s3_client,
                &s3_creds,
                &backup_path,
                &minio.s3_source,
                service_config,
            )
            .await
            .expect("Failed to restore MongoDB from S3");
        println!("✓ Restore completed");

        // Step 8: Verify restored data
        println!("Step 8: Verifying restored data...");
        let db_after_restore = client.database(database);
        let collection_after_restore =
            db_after_restore.collection::<mongodb::bson::Document>("test_collection");
        let count_after_restore = collection_after_restore
            .count_documents(doc! {})
            .await
            .expect("Failed to count documents");
        assert_eq!(
            count_after_restore, 3,
            "Should have 3 documents after restore"
        );

        // Verify the actual data
        let restored_docs: Vec<mongodb::bson::Document> = collection_after_restore
            .find(doc! {})
            .await
            .expect("Failed to query documents")
            .try_collect()
            .await
            .expect("Failed to collect documents");

        assert_eq!(restored_docs.len(), 3);

        let names: Vec<String> = restored_docs
            .iter()
            .filter_map(|doc| doc.get_str("name").ok().map(|s| s.to_string()))
            .collect();

        assert!(names.contains(&"Alice".to_string()));
        assert!(names.contains(&"Bob".to_string()));
        assert!(names.contains(&"Charlie".to_string()));

        println!(
            "✓ Data verified: {} documents restored correctly",
            count_after_restore
        );

        // Step 9: Cleanup
        println!("Step 9: Cleaning up...");

        // Stop and remove MongoDB container
        let _ = service.stop().await;
        let _ = service.remove().await;

        // Stop and remove MinIO container (using test utility)
        let _ = minio.cleanup().await;

        println!("✓ Cleanup completed");
        println!("\n✅ MongoDB backup and restore test completed successfully!");
    }
}
