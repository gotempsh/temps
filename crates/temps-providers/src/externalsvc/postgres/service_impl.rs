// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The `ExternalService` implementation for `PostgresService`.

use super::*;
use crate::externalsvc::port_util::find_available_port;
use crate::externalsvc::{
    ExternalService, HealthProbeResult, RuntimeEnvVar, ServiceConfig, ServiceResourceLimits,
    ServiceType,
};
use crate::utils::ensure_network_exists;
use async_trait::async_trait;
use bollard::query_parameters::StopContainerOptions;
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, info, warn};
use urlencoding;

#[async_trait]
impl ExternalService for PostgresService {
    fn recreate_plan(
        &self,
        service_config: &ServiceConfig,
    ) -> Option<(
        crate::externalsvc::recreate_guard::ManagedEngine,
        crate::externalsvc::recreate_guard::ReplacementStorage,
    )> {
        let config = self.get_postgres_config(service_config.clone()).ok()?;
        // Imported containers are never replaced by Temps.
        if config.container_name.is_some() {
            return None;
        }
        Some((
            crate::externalsvc::recreate_guard::ManagedEngine::Postgres,
            self.replacement_storage(&config).ok()?,
        ))
    }

    fn docker_resource_names(&self) -> Option<crate::externalsvc::DockerResourceNames> {
        Some(crate::externalsvc::DockerResourceNames {
            containers: vec![postgres_container_name(&self.name)],
            volumes: postgres_volume_names(&self.name).to_vec(),
        })
    }

    fn data_import(&self) -> Option<&dyn crate::data_import::DataImportEngine> {
        Some(self)
    }

    fn get_local_address(&self, service_config: ServiceConfig) -> Result<String> {
        let config = self.get_postgres_config(service_config)?;
        Ok(format!("localhost:{}", config.port))
    }

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
        POSTGRES_INTERNAL_PORT.to_string()
    }

    /// Backup PostgreSQL data to S3.
    ///
    /// Detects whether the container has WAL-G installed:
    /// - **WAL-G available**: Uses `wal-g backup-push` inside the container. Zero data flows
    ///   through the Temps process. Before the base backup, verifies continuous WAL archiving for PITR.
    /// - **WAL-G not available** (legacy images like `postgres:18-alpine`): Falls back to
    ///   pg_dump via a sidecar container, streaming to a temp file and uploading to S3.
    async fn backup_to_s3(
        &self,
        s3_client: &aws_sdk_s3::Client,
        s3_credentials: &crate::externalsvc::S3Credentials,
        backup: temps_entities::backups::Model,
        s3_source: &temps_entities::s3_sources::Model,
        subpath: &str,
        subpath_root: &str,
        pool: &temps_database::DbConnection,
        external_service: &temps_entities::external_services::Model,
        service_config: ServiceConfig,
    ) -> anyhow::Result<crate::externalsvc::BackupOutcome> {
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
            // Create Docker container. A new service starts with archiving
            // off (its volume has no walg.env); `enable_wal_archiving()`
            // recreates with archiving on when WAL-G is later configured. A
            // re-initialised service keeps what its volume records, so a
            // recreate (e.g. a /dev/shm change) does not drop archiving and
            // then need a second restart to restore it. `create_container`
            // may retry on a different host port than requested (see its
            // docs); it writes that back into `postgres_config`, so
            // everything below — `self.config` and the DB-persisted
            // parameters — reflects the port the container is actually
            // bound to.
            let enable_archiving = self.compute_desired_enable_archiving().await;
            self.create_container(
                &self.docker,
                &mut postgres_config,
                &resource_limits,
                enable_archiving,
            )
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
        let runtime_config_json = serde_json::to_value(&postgres_config).map_err(|e| {
            anyhow::anyhow!(
                "Failed to serialize PostgreSQL runtime config for service {}: {}",
                self.name,
                e
            )
        })?;

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
        let resource_name = crate::externalsvc::scoped_resource_name(project_id, environment);
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
        let resource_name = crate::externalsvc::scoped_resource_name(project_id, environment);
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

        let username = parameters.get("username").ok_or_else(|| {
            anyhow::anyhow!(
                "Missing username parameter for PostgreSQL service {}",
                self.name
            )
        })?;
        let password = parameters.get("password").ok_or_else(|| {
            anyhow::anyhow!(
                "Missing password parameter for PostgreSQL service {}",
                self.name
            )
        })?;
        let database = parameters.get("database").ok_or_else(|| {
            anyhow::anyhow!(
                "Missing database parameter for PostgreSQL service {}",
                self.name
            )
        })?;

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
        Self::parameter_schema()
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
                        vec![crate::externalsvc::exact_container_name_filter(
                            &container_name,
                        )],
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
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "Failed to start imported PostgreSQL container {}: {}",
                            container_name,
                            e
                        )
                    })?;
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
                    vec![crate::externalsvc::exact_container_name_filter(
                        &container_name,
                    )],
                )])),
                ..Default::default()
            }))
            .await?;

        let need_create = containers.is_empty();
        if let Some(container) = containers.first() {
            // Inspect the existing CMD. If it disagrees with what we'd emit
            // now, force a recreate by stopping + removing the old container
            // and falling through to the create branch. Covers both
            // archive_mode drift and shared_preload_libraries drift (e.g. a
            // container created before pg_stat_statements support, or before
            // an image-specific library like timescaledb was correctly
            // merged in) — see `container_cmd_shared_preload_libraries_differs`.
            //
            // Desired libraries are derived from the *container's own*
            // `Image` field, not `existing_config` — a freshly-constructed
            // service instance (e.g. from `start_service()`'s non-imported
            // path) has an unhydrated `self.config` (`None`), which would
            // otherwise silently skip this drift check entirely.
            let desired_preload_libraries = container
                .image
                .as_deref()
                .map(Self::shared_preload_libraries_for_image);

            let archive_drift = self
                .container_cmd_archive_mode_differs(container, desired_enable_archiving)
                .await;
            let preload_drift = match &desired_preload_libraries {
                Some(desired) => {
                    self.container_cmd_shared_preload_libraries_differs(container, desired)
                        .await
                }
                None => false,
            };
            // A container created before `POSTGRES_CONTAINER_INIT` runs the
            // postmaster as PID 1 with the old healthcheck; both are fixed
            // at creation, so only a recreate applies them.
            let init_drift = self.container_missing_init(container).await;
            let drift = archive_drift || preload_drift || init_drift;
            if drift {
                info!(
                    "Container {} has config drift (archive_mode drift={}, \
                     shared_preload_libraries drift={}, init process drift={}, \
                     desired archive_mode={}). Recreating to apply correct config.",
                    container_name,
                    archive_drift,
                    preload_drift,
                    init_drift,
                    desired_enable_archiving
                );
                // Without a hydrated config there is nothing to rebuild the
                // container from; leave the existing one alone.
                let config = self.config.read().await.as_ref().cloned();
                if let Some(config) = config {
                    let limits = self.resource_limits.read().await.clone();
                    match self
                        .recreate_container_guarded(
                            &self.docker,
                            &config,
                            &limits,
                            desired_enable_archiving,
                            "applying configuration drift on start",
                        )
                        .await
                    {
                        Ok(_) => {
                            self.wait_for_container_health(&self.docker, &container_name)
                                .await?;
                            return Ok(());
                        }
                        Err(e)
                            if e.downcast_ref::<crate::externalsvc::recreate_guard::RecreateGuardError>()
                                .is_some_and(|g| g.original_in_place()) =>
                        {
                            // Drift repair is optional; data is not. The
                            // guard refused before touching anything, or a
                            // verified rollback put the original back exactly
                            // as it was: start it unchanged. Any other
                            // failure (a rollback that did not complete) is
                            // returned: the original may still be set aside
                            // or a replacement that failed verification may
                            // hold its name, and that needs an operator.
                            warn!(
                                service = %self.name,
                                container = %container_name,
                                error = %e,
                                "Not applying configuration drift; starting the existing container unchanged"
                            );
                        }
                        Err(e) => return Err(e),
                    }
                } else {
                    warn!(
                        "Container {} has CMD drift but no service configuration is loaded; starting it unchanged",
                        container_name
                    );
                }
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
                            return Err(anyhow::anyhow!(
                                "Failed to start existing PostgreSQL container {}: {}",
                                container_name,
                                e
                            ));
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

    /// Hydrate `self.config` from `service_config` (mirroring `init()`, but
    /// without unconditionally creating a container — a container may
    /// already exist and only need recreating), then delegate to `start()`.
    /// `start()`'s reconcile-on-start drift check derives desired
    /// `shared_preload_libraries` from the *container's own* `Image` field
    /// (see `container_cmd_shared_preload_libraries_differs`), so it will
    /// detect drift and recreate correctly now that `self.config` is
    /// populated for the create step.
    async fn force_recreate(&self, service_config: ServiceConfig) -> Result<()> {
        let resource_limits = ServiceResourceLimits::from_parameters(&service_config.parameters);
        if let Err(e) = resource_limits.validate() {
            return Err(anyhow::anyhow!("Invalid resource limits: {}", e));
        }
        let postgres_config = self.get_postgres_config(service_config)?;
        *self.config.write().await = Some(postgres_config);
        *self.resource_limits.write().await = resource_limits;

        self.start().await
    }

    async fn enable_continuous_archiving(
        &self,
        service_config: ServiceConfig,
        s3_credentials: &crate::externalsvc::S3Credentials,
        walg_prefix: &str,
    ) -> Result<()> {
        // Every backup reconciles the explicit, pinned write destination.
        // enable_wal_archiving verifies the actual command and recreates only
        // when PostgreSQL's archive_mode needs a postmaster restart.
        self.write_wal_archiving_config(service_config, s3_credentials, walg_prefix, true)
            .await
            .map(|_| ())
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
                    vec![crate::externalsvc::exact_container_name_filter(
                        &container_name,
                    )],
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
        let [volume_name] = postgres_volume_names(&self.name);

        info!("Removing PostgreSQL container and volume for {}", self.name);

        // Remove container if it exists
        let containers = self
            .docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: true,
                filters: Some(HashMap::from([(
                    "name".to_string(),
                    vec![crate::externalsvc::exact_container_name_filter(
                        &container_name,
                    )],
                )])),
                ..Default::default()
            }))
            .await?;

        if !containers.is_empty() {
            // Stop container first if running
            self.docker
                .stop_container(&container_name, None::<StopContainerOptions>)
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to stop PostgreSQL container {}: {}",
                        container_name,
                        e
                    )
                })?;

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
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to remove PostgreSQL container {}: {}",
                        container_name,
                        e
                    )
                })?;
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

        let database = parameters.get("database").ok_or_else(|| {
            anyhow::anyhow!(
                "Missing database parameter for PostgreSQL service {}",
                self.name
            )
        })?;
        let username = parameters.get("username").ok_or_else(|| {
            anyhow::anyhow!(
                "Missing username parameter for PostgreSQL service {}",
                self.name
            )
        })?;
        let password = parameters.get("password").ok_or_else(|| {
            anyhow::anyhow!(
                "Missing password parameter for PostgreSQL service {}",
                self.name
            )
        })?;

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
        let resource_name = crate::externalsvc::scoped_resource_name(project_id, environment);
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
        s3_credentials: &crate::externalsvc::S3Credentials,
        backup_location: &str,
        s3_source: &temps_entities::s3_sources::Model,
        service_config: ServiceConfig,
    ) -> Result<()> {
        info!("Starting PostgreSQL restore from S3: {}", backup_location);

        // Detect if this is a WAL-G backup (s3:// prefix) or a legacy backup (.sql.gz / .pgdump.gz)
        if backup_location.starts_with("s3://") {
            // WAL-G backup: use wal-g backup-fetch
            self.restore_from_walg(
                s3_credentials,
                backup_location,
                service_config,
                None,
                None,
                None,
                &crate::externalsvc::NoopRestoreGate,
            )
            .await
        } else {
            // Legacy backup: fall back to old psql/pg_restore approach
            self.restore_from_legacy(
                s3_client,
                backup_location,
                s3_source,
                service_config,
                &crate::externalsvc::NoopRestoreGate,
            )
            .await
        }
    }

    /// Both restore paths stage the backup first — WAL-G fetches into
    /// `restore_temp` while the server keeps running, a legacy dump is
    /// downloaded to the host — and pass the gate before the first write.
    fn defers_target_writes(&self, _backup_location: &str) -> bool {
        true
    }

    async fn stop_staged_restore(&self) -> Result<()> {
        self.stop_walg_fetch(&self.get_container_name(), WALG_RESTORE_TEMP, None)
            .await
            .map_err(|reason| {
                anyhow::Error::new(crate::externalsvc::StagedRestoreStillRunning {
                    target: self.get_container_name(),
                    reason,
                })
            })
    }

    async fn restore_in_place(&self, ctx: crate::externalsvc::RestoreContext<'_>) -> Result<()> {
        if ctx.backup_location.starts_with("s3://") {
            let target_user_data = walg_target_user_data(ctx.backup)?;
            self.restore_from_walg(
                ctx.s3_credentials,
                ctx.backup_location,
                ctx.source_config,
                Some(&ctx.live_target_config),
                None,
                target_user_data.as_deref(),
                ctx.gate,
            )
            .await
        } else {
            self.restore_from_legacy(
                ctx.s3_client,
                ctx.backup_location,
                ctx.s3_source,
                ctx.source_config,
                ctx.gate,
            )
            .await
        }
    }

    async fn upgrade(&self, old_config: ServiceConfig, new_config: ServiceConfig) -> Result<()> {
        let old_pg_config = self.get_postgres_config(old_config)?;
        // The service's own limits: a provider built for this call has
        // none loaded, and the new container must keep them.
        let upgrade_limits = ServiceResourceLimits::from_parameters(&new_config.parameters);
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

        // Reject an ordinary no-op selection before any Docker/network work.
        if old_pg_config.docker_image == new_pg_config.docker_image {
            return Err(PostgresUpgradeRejected::AlreadySelected {
                image: old_pg_config.docker_image,
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

        // The swap stops the container before recreating it. Refuse first,
        // while it still runs, if the new container would not see its data.
        if new_pg_config.container_name.is_none() {
            let replacement = self.replacement_storage(&new_pg_config)?;
            crate::externalsvc::recreate_guard::preflight(
                &self.docker,
                crate::externalsvc::recreate_guard::ManagedEngine::Postgres,
                &self.name,
                &self.get_container_name(),
                &replacement,
                "changing its image",
            )
            .await?;
        }

        // Same major version — image swap only (e.g., postgres:18 -> gotempsh/postgres-walg:18).
        // No pg_upgrade needed. Just recreate the container with the new image;
        // data is preserved on the Docker volume.
        info!(
            "Same PostgreSQL major version ({}), swapping image without pg_upgrade",
            old_version
        );
        // No stop here: `create_container` replaces the running container
        // through the recreate guard, which stops it only after checking the
        // new one will see its data, and fingerprints it while it runs.
        let limits = upgrade_limits;
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
    ) -> Result<crate::externalsvc::RestoreCapabilities> {
        Ok(crate::externalsvc::RestoreCapabilities {
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
        ctx: crate::externalsvc::RestoreContext<'_>,
        new_service_name: String,
        parameter_overrides: serde_json::Value,
    ) -> Result<crate::externalsvc::NewServiceRestoreResult> {
        info!(
            "Provisioning new PostgreSQL service '{}' from backup at {}",
            new_service_name, ctx.backup_location
        );

        // Start from the source service's parameters, then apply caller overrides.
        let mut source_config = self
            .get_postgres_config(ctx.source_config.clone())?
            .for_managed_clone();

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
                // Restoring into a new service clones the source's
                // POSTGRES_PASSWORD into the new container's environment, so an
                // unchecked override starts an attacker-named image holding the
                // source database's root credentials. The instance's existing
                // image allowlist is the right gate — it is exact `image:tag`
                // equality and an operator can extend it, so this adds no new
                // policy, it just stops the restore path from being the one
                // place that skips it.
                validate_postgres_docker_image(image)?;
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
            let target_user_data = walg_target_user_data(ctx.backup)?;
            new_service
                .restore_from_walg(
                    ctx.s3_credentials,
                    ctx.backup_location,
                    new_service_config,
                    None,
                    None,
                    target_user_data.as_deref(),
                    ctx.gate,
                )
                .await?;
        } else {
            new_service
                .restore_from_legacy(
                    ctx.s3_client,
                    ctx.backup_location,
                    ctx.s3_source,
                    new_service_config,
                    ctx.gate,
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

        Ok(crate::externalsvc::NewServiceRestoreResult {
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
        ctx: crate::externalsvc::RestoreContext<'_>,
        target: crate::externalsvc::RecoveryTarget,
        to_new_service: bool,
        new_service_name: Option<String>,
    ) -> Result<Option<crate::externalsvc::NewServiceRestoreResult>> {
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
            let mut source_config = self
                .get_postgres_config(ctx.source_config.clone())?
                .for_managed_clone();
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

            let target_user_data = walg_target_user_data(ctx.backup)?;
            new_service
                .restore_from_walg(
                    ctx.s3_credentials,
                    ctx.backup_location,
                    new_service_config,
                    None,
                    Some(&target),
                    target_user_data.as_deref(),
                    ctx.gate,
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

            Ok(Some(crate::externalsvc::NewServiceRestoreResult {
                parameters,
                connection_info,
            }))
        } else {
            // In-place PITR — replay the WAL onto the existing container.
            let target_user_data = walg_target_user_data(ctx.backup)?;
            self.restore_from_walg(
                ctx.s3_credentials,
                ctx.backup_location,
                ctx.source_config.clone(),
                Some(&ctx.live_target_config),
                Some(&target),
                target_user_data.as_deref(),
                ctx.gate,
            )
            .await?;
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests;
