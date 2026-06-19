use crate::utils::ensure_network_exists;

use super::{
    ExternalService, HealthProbeResult, LogicalResource, RuntimeEnvVar, ServiceConfig,
    ServiceResourceLimits, ServiceType,
};
use anyhow::Result;
use async_trait::async_trait;
use bollard::query_parameters::{InspectContainerOptions, StopContainerOptions};
use bollard::Docker;
use futures::{StreamExt, TryStreamExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{debug, info};

const MARIADB_INTERNAL_PORT: &str = "3306";
const DEFAULT_MARIADB_IMAGE: &str = "mariadb:lts";
const MIN_PASSWORD_LENGTH: usize = 8;

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

        info!("Pulling MariaDB image {}", config.docker_image);
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
            .await
            .map_err(|e| anyhow::anyhow!("Failed to pull MariaDB image: {}", e))?;

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
            port_bindings: Some(HashMap::from([(
                "3306/tcp".to_string(),
                Some(vec![bollard::models::PortBinding {
                    host_ip: Some("0.0.0.0".to_string()),
                    host_port: Some(config.port.clone()),
                }]),
            )])),
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

    async fn run_container_command(&self, cmd: Vec<String>, timeout: Duration) -> Result<String> {
        let container_name = self.get_container_name();
        tokio::time::timeout(timeout, async {
            let exec = self
                .docker
                .create_exec(
                    &container_name,
                    bollard::exec::CreateExecOptions {
                        cmd: Some(cmd),
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
        self.run_container_command(
            vec![
                "mariadb".to_string(),
                "-uroot".to_string(),
                format!("-p{}", config.root_password),
                "-e".to_string(),
                sql.to_string(),
            ],
            Duration::from_secs(15),
        )
        .await
        .map(|_| ())
    }

    async fn ping(&self, config: &MariaDbConfig) -> Result<()> {
        self.run_container_command(
            vec![
                "mariadb-admin".to_string(),
                "ping".to_string(),
                "-h".to_string(),
                "127.0.0.1".to_string(),
                "-uroot".to_string(),
                format!("-p{}", config.root_password),
                "--silent".to_string(),
            ],
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
            &self.get_container_name(),
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

        self.create_container(&self.docker, &mariadb_config, &resource_limits)
            .await?;

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
                    "host" | "database" | "username" | "password" | "root_password" => false,
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
        let container_name = self.get_container_name();
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
            let config = self
                .config
                .read()
                .await
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("MariaDB configuration not found"))?
                .clone();
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
        let container_name = self.get_container_name();
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

        Self::build_env_vars(
            &self.get_container_name(),
            MARIADB_INTERNAL_PORT,
            database,
            username,
            password,
        )
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
            Ok((self.get_container_name(), MARIADB_INTERNAL_PORT.to_string()))
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
