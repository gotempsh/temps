//! DB-backed implementation of [`PostgresContainerLifecycle`].
//!
//! Reads service parameters via the shared `ExternalServiceManager`
//! (which owns encryption/decryption of the service config blob), drives
//! Docker via Bollard to create/remove containers, and persists image
//! swaps back through the manager. The upgrade orchestrator depends on
//! this trait (not on `PostgresService` or the manager directly) so it
//! stays stateless and service-agnostic — mirrors the
//! `PreUpgradeBackupProvider` pattern used for pre-upgrade backups.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bollard::Docker;
use futures::{StreamExt, TryStreamExt};
use sea_orm::{ActiveModelTrait, ActiveValue::Set, DatabaseConnection, EntityTrait};
use temps_core::EncryptionService;
use temps_entities::external_services;

use crate::externalsvc::postgres::{
    postgres_healthcheck_cmd, shell_escape, PostgresConfig, PostgresInputConfig,
};
use crate::externalsvc::postgres_upgrade::{PostgresConnection, PostgresContainerLifecycle};
use crate::services::ExternalServiceManager;
use crate::utils::ensure_network_exists;

/// Adapter that fulfils `PostgresContainerLifecycle` by delegating
/// config-read and image-persist operations to `ExternalServiceManager`
/// (which handles encryption/decryption of the config blob).
pub struct PostgresLifecycleAdapter {
    db: Arc<DatabaseConnection>,
    docker: Arc<Docker>,
    manager: Arc<ExternalServiceManager>,
    encryption_service: Arc<EncryptionService>,
}

impl PostgresLifecycleAdapter {
    pub fn new(
        db: Arc<DatabaseConnection>,
        docker: Arc<Docker>,
        manager: Arc<ExternalServiceManager>,
        encryption_service: Arc<EncryptionService>,
    ) -> Self {
        Self {
            db,
            docker,
            manager,
            encryption_service,
        }
    }

    async fn load_service_row(&self, service_id: i32) -> Result<external_services::Model, String> {
        external_services::Entity::find_by_id(service_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| format!("db error loading service {}: {}", service_id, e))?
            .ok_or_else(|| format!("service {} not found", service_id))
    }

    /// Load + decrypt + parse the Postgres config blob.
    async fn load_postgres_config(&self, service_id: i32) -> Result<PostgresConfig, String> {
        let cfg = self
            .manager
            .get_service_config(service_id)
            .await
            .map_err(|e| format!("get_service_config({}) failed: {}", service_id, e))?;

        let input: PostgresInputConfig = serde_json::from_value(cfg.parameters)
            .map_err(|e| format!("service {} has invalid postgres params: {}", service_id, e))?;
        Ok(PostgresConfig::from(input))
    }

    /// Mirrors `PostgresService::get_pgdata_path`.
    fn pgdata_path_for(image: &str) -> Result<String, String> {
        let tag = image
            .split(':')
            .nth(1)
            .ok_or_else(|| format!("image '{}' has no tag", image))?;
        let version = tag
            .trim_start_matches("pg")
            .split('-')
            .next()
            .and_then(|v| v.split('.').next())
            .ok_or_else(|| format!("could not extract version from '{}'", image))?
            .parse::<u32>()
            .map_err(|e| format!("bad version in '{}': {}", image, e))?;
        Ok(format!("/var/lib/postgresql/{}/docker", version))
    }

    fn sql_string_literal(s: &str) -> String {
        format!("'{}'", s.replace('\'', "''"))
    }

    async fn exec_in_container(
        &self,
        container_name: &str,
        cmd: Vec<String>,
        env: Option<Vec<String>>,
    ) -> Result<(Option<i64>, String), String> {
        let exec = self
            .docker
            .create_exec(
                container_name,
                bollard::models::ExecConfig {
                    cmd: Some(cmd),
                    env,
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| format!("create_exec({}) failed: {}", container_name, e))?;

        let mut output_text = String::new();
        if let Ok(bollard::exec::StartExecResults::Attached { mut output, .. }) =
            self.docker.start_exec(&exec.id, None).await
        {
            while let Some(chunk) = output.next().await {
                match chunk {
                    Ok(chunk) => output_text.push_str(&chunk.to_string()),
                    Err(e) => output_text.push_str(&format!("<exec output error: {}>", e)),
                }
            }
        }

        let inspect = self
            .docker
            .inspect_exec(&exec.id)
            .await
            .map_err(|e| format!("inspect_exec({}) failed: {}", container_name, e))?;
        Ok((inspect.exit_code, output_text))
    }

    async fn container_logs(&self, container_name: &str) -> String {
        self.docker
            .logs(
                container_name,
                Some(
                    bollard::query_parameters::LogsOptionsBuilder::new()
                        .stdout(true)
                        .stderr(true)
                        .build(),
                ),
            )
            .try_collect::<Vec<_>>()
            .await
            .map(|v| v.into_iter().map(|c| c.to_string()).collect::<String>())
            .unwrap_or_else(|e| format!("<failed to read logs: {}>", e))
    }

    async fn wait_for_psql_database(
        &self,
        container_name: &str,
        cfg: &PostgresConfig,
        database: &str,
        timeout: Duration,
    ) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        let mut last_exit = None;
        let mut last_output = String::new();
        while Instant::now() < deadline {
            let result = self
                .exec_in_container(
                    container_name,
                    vec![
                        "psql".to_string(),
                        "-v".to_string(),
                        "ON_ERROR_STOP=1".to_string(),
                        "-U".to_string(),
                        cfg.username.clone(),
                        "-d".to_string(),
                        database.to_string(),
                        "-tAc".to_string(),
                        "SELECT 1".to_string(),
                    ],
                    Some(vec![format!("PGPASSWORD={}", cfg.password)]),
                )
                .await;

            if let Ok((exit_code, output)) = result {
                last_exit = exit_code;
                last_output = output;
                if exit_code == Some(0) {
                    return Ok(());
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        let logs = self.container_logs(container_name).await;
        Err(format!(
            "container '{}' failed psql readiness for database '{}' within {}s \
             (last exit={:?}, output={}):\n{}",
            container_name,
            database,
            timeout.as_secs(),
            last_exit,
            last_output.trim(),
            logs
        ))
    }

    async fn wait_for_container_healthy(
        &self,
        container_name: &str,
        timeout: Duration,
    ) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        let mut last_state = String::new();
        let mut last_health = String::new();
        let mut last_error = String::new();

        while Instant::now() < deadline {
            let inspect = match self
                .docker
                .inspect_container(
                    container_name,
                    None::<bollard::query_parameters::InspectContainerOptions>,
                )
                .await
            {
                Ok(inspect) => inspect,
                Err(e) => {
                    // Tolerate transient inspect errors (daemon hiccup, a
                    // brief connection reset right after start_container)
                    // the same way wait_for_psql_database tolerates
                    // transient exec errors above -- retry until the
                    // deadline instead of aborting the whole readiness wait
                    // on one hiccup.
                    last_error = e.to_string();
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
            };

            if let Some(state) = inspect.state {
                last_state = state
                    .status
                    .map(|status| status.to_string())
                    .unwrap_or_default();
                last_health = state
                    .health
                    .and_then(|health| health.status)
                    .map(|status| status.to_string())
                    .unwrap_or_default();

                if state.running == Some(false) || state.dead == Some(true) {
                    let logs = self.container_logs(container_name).await;
                    return Err(format!(
                        "container '{}' stopped before becoming healthy \
                         (state='{}', health='{}', exit={:?}, error={:?}):\n{}",
                        container_name, last_state, last_health, state.exit_code, state.error, logs
                    ));
                }

                if last_health == "healthy" {
                    return Ok(());
                }
            }

            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        let logs = self.container_logs(container_name).await;
        Err(format!(
            "container '{}' did not become healthy within {}s \
             (last state='{}', health='{}', last inspect error='{}'):\n{}",
            container_name,
            timeout.as_secs(),
            last_state,
            last_health,
            last_error,
            logs
        ))
    }

    async fn ensure_database_exists(
        &self,
        container_name: &str,
        cfg: &PostgresConfig,
    ) -> Result<(), String> {
        let sql = format!(
            "SELECT 1 FROM pg_database WHERE datname = {}",
            Self::sql_string_literal(&cfg.database)
        );
        let cmd = format!(
            "set -e; \
             if psql -v ON_ERROR_STOP=1 -U {user} -d postgres -tAc {sql} | grep -q 1; then \
               exit 0; \
             fi; \
             if createdb -U {user} {db} 2>/tmp/createdb.err; then \
               exit 0; \
             fi; \
             psql -v ON_ERROR_STOP=1 -U {user} -d postgres -tAc {sql} | grep -q 1 \
               || {{ cat /tmp/createdb.err >&2; exit 1; }}",
            user = shell_escape(&cfg.username),
            sql = shell_escape(&sql),
            db = shell_escape(&cfg.database),
        );
        // A single exec here can race the postgres entrypoint's own
        // transient init-phase server: `wait_for_psql_database` above only
        // requires one successful `SELECT 1`, which can land against that
        // temporary server moments before it's replaced by the final one.
        // Retry for a short bounded window instead of failing the whole
        // readiness phase on that one-shot race.
        let timeout = Duration::from_secs(30);
        let deadline = Instant::now() + timeout;
        let (last_exit, last_output) = loop {
            let attempt = self
                .exec_in_container(
                    container_name,
                    vec!["sh".to_string(), "-lc".to_string(), cmd.clone()],
                    Some(vec![format!("PGPASSWORD={}", cfg.password)]),
                )
                .await;

            let (exit_code, output) = match attempt {
                Ok((Some(0), _)) => return Ok(()),
                Ok((exit_code, output)) => (exit_code, output),
                Err(e) => (None, e),
            };

            if Instant::now() >= deadline {
                break (exit_code, output);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        };

        let logs = self.container_logs(container_name).await;
        Err(format!(
            "failed to ensure database '{}' exists in '{}' within {}s \
             (last exit={:?}, output={}):\n{}",
            cfg.database,
            container_name,
            timeout.as_secs(),
            last_exit,
            last_output.trim(),
            logs
        ))
    }
}

#[async_trait]
impl PostgresContainerLifecycle for PostgresLifecycleAdapter {
    async fn container_name(&self, service_id: i32) -> Result<String, String> {
        let svc = self.load_service_row(service_id).await?;
        Ok(format!("postgres-{}", svc.name))
    }

    async fn connection_params(&self, service_id: i32) -> Result<PostgresConnection, String> {
        let cfg = self.load_postgres_config(service_id).await?;
        Ok(PostgresConnection {
            username: cfg.username,
            password: cfg.password,
            database: cfg.database,
            port: cfg.port,
        })
    }

    async fn stop_and_remove(&self, service_id: i32) -> Result<(), String> {
        let svc = self.load_service_row(service_id).await?;
        let container_name = format!("postgres-{}", svc.name);

        let _ = self
            .docker
            .stop_container(
                &container_name,
                None::<bollard::query_parameters::StopContainerOptions>,
            )
            .await;
        let remove = self
            .docker
            .remove_container(
                &container_name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
        match remove {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("No such container") || msg.contains("no such container") {
                    Ok(())
                } else {
                    Err(format!(
                        "remove_container({}) failed: {}",
                        container_name, msg
                    ))
                }
            }
        }
    }

    async fn create_and_start(&self, service_id: i32, image: &str) -> Result<(), String> {
        let svc = self.load_service_row(service_id).await?;
        let cfg = self.load_postgres_config(service_id).await?;
        let container_name = format!("postgres-{}", svc.name);
        let volume_name = format!("{}_data", container_name);
        let pgdata_path = Self::pgdata_path_for(image)?;

        // Pull image first for clear fail-fast errors.
        let (image_name, tag) = match image.split_once(':') {
            Some((n, t)) => (n.to_string(), t.to_string()),
            None => (image.to_string(), "latest".to_string()),
        };
        self.docker
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
            .map_err(|e| format!("pull image '{}' failed: {}", image, e))?;

        // Create volume if missing — idempotent.
        self.docker
            .create_volume(bollard::models::VolumeCreateRequest {
                name: Some(volume_name.clone()),
                ..Default::default()
            })
            .await
            .map_err(|e| format!("create_volume({}) failed: {:?}", volume_name, e))?;

        ensure_network_exists(&self.docker)
            .await
            .map_err(|e| format!("ensure_network_exists: {:?}", e))?;

        let service_label_key = format!("{}service_type", temps_core::DOCKER_LABEL_PREFIX);
        let name_label_key = format!("{}service_name", temps_core::DOCKER_LABEL_PREFIX);
        let labels = HashMap::from([
            (service_label_key, "postgres".to_string()),
            (name_label_key, svc.name.clone()),
        ]);

        let env_vars = vec![
            format!("POSTGRES_USER={}", cfg.username),
            format!("POSTGRES_PASSWORD={}", cfg.password),
            format!("POSTGRES_DB={}", cfg.database),
            format!("PGDATA={}", pgdata_path),
            "POSTGRES_HOST_AUTH_METHOD=md5".to_string(),
        ];

        let host_config = bollard::models::HostConfig {
            port_bindings: Some(crate::utils::local_port_binding("5432/tcp", &cfg.port)),
            mounts: Some(vec![bollard::models::Mount {
                target: Some("/var/lib/postgresql".to_string()),
                source: Some(volume_name.clone()),
                typ: Some(bollard::models::MountTypeEnum::VOLUME),
                ..Default::default()
            }]),
            log_config: Some(crate::utils::default_service_log_config()),
            security_opt: Some(vec!["no-new-privileges:true".to_string()]),
            pids_limit: Some(512),
            restart_policy: Some(bollard::models::RestartPolicy {
                name: Some(bollard::models::RestartPolicyNameEnum::ALWAYS),
                maximum_retry_count: None,
            }),
            ..Default::default()
        };

        let networking_config = Some(bollard::models::NetworkingConfig {
            endpoints_config: Some(HashMap::from([(
                temps_core::NETWORK_NAME.to_string(),
                bollard::models::EndpointSettings::default(),
            )])),
        });

        let container_cfg = bollard::models::ContainerCreateBody {
            image: Some(image.to_string()),
            exposed_ports: Some(vec!["5432/tcp".to_string()]),
            env: Some(env_vars),
            labels: Some(labels),
            cmd: Some(vec![
                "postgres".to_string(),
                "-c".to_string(),
                format!("max_connections={}", cfg.max_connections),
            ]),
            host_config: Some(host_config),
            networking_config,
            healthcheck: Some(bollard::models::HealthConfig {
                test: Some(vec![
                    "CMD-SHELL".to_string(),
                    // Without -d, pg_isready (via libpq) defaults the target
                    // database to the username. For any service where
                    // database != username, that's a nonexistent database,
                    // so every healthcheck tick (interval=1s) logs a FATAL
                    // "database does not exist" forever — pg_isready still
                    // reports healthy (the server did respond), but the
                    // container's logs fill up with noise for its entire
                    // lifetime. Pin -d explicitly to the configured database.
                    postgres_healthcheck_cmd(&cfg.username, &cfg.database),
                ]),
                interval: Some(1_000_000_000),
                timeout: Some(3_000_000_000),
                retries: Some(3),
                start_period: Some(30_000_000_000),
                start_interval: Some(1_000_000_000),
            }),
            ..Default::default()
        };

        // Remove any pre-existing container with the same name so create
        // doesn't 409.
        let _ = self
            .docker
            .stop_container(
                &container_name,
                None::<bollard::query_parameters::StopContainerOptions>,
            )
            .await;
        let _ = self
            .docker
            .remove_container(
                &container_name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        self.docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&container_name)
                        .build(),
                ),
                container_cfg,
            )
            .await
            .map_err(|e| format!("create_container({}) failed: {}", container_name, e))?;

        self.docker
            .start_container(
                &container_name,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| format!("start_container({}) failed: {}", container_name, e))?;

        // Block until Docker's healthcheck sees the final post-entrypoint
        // server, then make sure the configured application database exists.
        // A transient init server can accept SQL briefly before shutdown.
        self.wait_for_container_healthy(&container_name, Duration::from_secs(120))
            .await?;
        self.wait_for_psql_database(&container_name, &cfg, "postgres", Duration::from_secs(120))
            .await?;
        self.ensure_database_exists(&container_name, &cfg).await?;
        self.wait_for_psql_database(
            &container_name,
            &cfg,
            &cfg.database,
            Duration::from_secs(30),
        )
        .await?;

        Ok(())
    }

    async fn set_docker_image(&self, service_id: i32, image: &str) -> Result<(), String> {
        // Read current decrypted config, swap docker_image, re-encrypt,
        // and write directly via Sea-ORM. We intentionally bypass
        // `ExternalServiceManager::update_service` because it triggers a
        // service reinitialize that would stop/remove the container we
        // just upgraded.
        let cfg = self
            .manager
            .get_service_config(service_id)
            .await
            .map_err(|e| format!("get_service_config({}) failed: {}", service_id, e))?;

        let mut params = match cfg.parameters {
            serde_json::Value::Object(m) => m,
            other => {
                return Err(format!(
                    "service {} parameters not a JSON object (got {})",
                    service_id, other
                ))
            }
        };
        params.insert(
            "docker_image".to_string(),
            serde_json::Value::String(image.to_string()),
        );

        let config_json = serde_json::to_string(&serde_json::Value::Object(params))
            .map_err(|e| format!("serialize config for service {}: {}", service_id, e))?;
        let encrypted = self
            .encryption_service
            .encrypt_string(&config_json)
            .map_err(|e| format!("encrypt config for service {}: {}", service_id, e))?;

        let svc = self.load_service_row(service_id).await?;
        let mut active: external_services::ActiveModel = svc.into();
        active.config = Set(Some(encrypted));
        active.updated_at = Set(chrono::Utc::now());
        active
            .update(self.db.as_ref())
            .await
            .map_err(|e| format!("update service {} config: {}", service_id, e))?;
        Ok(())
    }
}

// `postgres_healthcheck_cmd`/`shell_escape` are shared with
// `externalsvc::postgres`, which owns their unit tests
// (`healthcheck_cmd_pins_database_when_it_differs_from_username`,
// `healthcheck_cmd_escapes_single_quotes_in_username_and_database`).
