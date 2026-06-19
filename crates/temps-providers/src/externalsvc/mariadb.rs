use crate::utils::ensure_network_exists;

use super::{
    ExternalService, HealthProbeResult, LogicalResource, RuntimeEnvVar, ServiceConfig,
    ServiceResourceLimits, ServiceType,
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

    /// Existing Docker container name for imported services.
    #[serde(default)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
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

    async fn dump_all_databases_to_gzip_file(
        &self,
        config: &MariaDbConfig,
        output_path: &std::path::Path,
    ) -> Result<()> {
        use std::io::Write;

        let container_name = self.get_live_container_name(config);
        let env = vec![
            format!("MYSQL_PWD={}", config.root_password),
            format!("MARIADB_PWD={}", config.root_password),
        ];
        let cmd = vec![
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
        use std::io::Read;

        info!("Starting MariaDB restore from S3: {}", backup_location);

        let config = self.get_mariadb_config(service_config)?;
        let backup_key = Self::backup_key_from_location(backup_location, &s3_source.bucket_name);
        let response = s3_client
            .get_object()
            .bucket(&s3_source.bucket_name)
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

        self.restore_sql_file(&config, &sql_path).await?;
        info!("MariaDB restore completed successfully");
        Ok(())
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
                "container_name": imported_container_name,
            }),
        })
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
