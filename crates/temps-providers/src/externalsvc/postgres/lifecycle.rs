// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! PostgreSQL container lifecycle: creation, recreation, health, databases
//! and version handling.

use super::*;
use crate::externalsvc::port_util::{find_available_port_async, is_port_conflict_error};
use crate::externalsvc::{ServiceConfig, ServiceResourceLimits};
use crate::utils::ensure_network_exists;
use bollard::query_parameters::InspectContainerOptions;
use bollard::Docker;
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{error, info, warn};
use urlencoding;

/// Rejections from same-version-only [`PostgresService::upgrade`]. The
/// `ExternalService::upgrade` trait method returns `anyhow::Result<()>`
/// across every provider, so this can't be a variant of a typed
/// `Result<(), E>` without changing that trait's signature everywhere.
/// Instead it's wrapped into the `anyhow::Error` via `?`/`.into()` so
/// callers can `downcast_ref::<PostgresUpgradeRejected>()` for an exact,
/// wording-independent match instead of matching on `e.to_string()`.
#[derive(Debug, thiserror::Error)]
pub enum PostgresUpgradeRejected {
    #[error("This service already uses the selected PostgreSQL image ({image}). Choose a different image to upgrade.")]
    AlreadySelected { image: String },

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
/// `exec` replaces the `/bin/sh -c` wrapper Docker starts for a `CMD-SHELL`
/// healthcheck, so `pg_isready` is the process Docker kills when the probe
/// exceeds its timeout. Without it the image's `dash` forks `pg_isready` as a
/// child, the kill takes only the shell, and the orphan is adopted by PID 1
/// -- the postmaster. When it then gives up with exit code 2 ("no response"),
/// PostgreSQL 18 logs `untracked child process ... exited with exit code 2`
/// and treats it as a crashed backend: every connection is terminated and
/// the server runs crash recovery. One slow probe therefore became a full
/// connection reset. `POSTGRES_CONTAINER_INIT` is the second guard against
/// the same failure from any other exec'd command.
///
/// Probe TCP explicitly: the entrypoint's temporary initialization server only
/// listens on a Unix socket. A socket probe can mark the container healthy just
/// before that server shuts down, causing the first deployment to lose its
/// database connection. TCP becomes available when the final server starts.
///
/// Shared with `postgres_lifecycle` so both container-creation sites build
/// the exact same healthcheck command.
pub(crate) fn postgres_healthcheck_cmd(username: &str, database: &str) -> String {
    format!(
        "exec pg_isready -h 127.0.0.1 -p 5432 -U {} -d {}",
        shell_escape(username),
        shell_escape(database)
    )
}

/// `HostConfig.init` for a standalone PostgreSQL container: always on.
///
/// Docker's init (tini) becomes PID 1 and reaps every process orphaned
/// inside the container -- a timed-out healthcheck, a `docker exec` whose
/// shell died first, a helper left behind by `archive_command`. Without it
/// those are reparented to the postmaster, which does not recognise them and
/// (from PostgreSQL 18, for any exit code other than 0 or 1) answers with a
/// crash restart that drops every client connection, including a running
/// WAL-G backup's. Signals are forwarded unchanged, so `docker stop` still
/// produces the image's fast shutdown.
///
/// Shared with `postgres_lifecycle` so both container-creation sites agree.
pub(crate) const POSTGRES_CONTAINER_INIT: Option<bool> = Some(true);

/// Whether a container was created without the init process
/// `POSTGRES_CONTAINER_INIT` now requires. `start()` recreates such a
/// container: `HostConfig.Init` and the healthcheck command are fixed at
/// creation, so a plain restart can never apply them.
pub(super) fn host_config_missing_init(host_config: &bollard::models::HostConfig) -> bool {
    host_config.init != POSTGRES_CONTAINER_INIT
}

impl PostgresService {
    pub(super) fn get_container_name(&self) -> String {
        postgres_container_name(&self.name)
    }

    /// The container this service actually runs in: the imported container's
    /// real name when `config.container_name` is set, otherwise the derived
    /// `postgres-{name}`. Every operation that talks to the live container
    /// (admin SQL, backup, restore, start/stop) must resolve through this,
    /// not `get_container_name()` directly, or it targets a synthesized name
    /// that doesn't exist for imported services.
    pub(super) fn get_live_container_name(&self, config: &PostgresConfig) -> String {
        config
            .container_name
            .clone()
            .unwrap_or_else(|| self.get_container_name())
    }

    pub(super) fn get_effective_address_for_environment(
        &self,
        service_config: ServiceConfig,
        execution_environment: temps_core::ExecutionEnvironment,
    ) -> Result<(String, String)> {
        let config = self.get_postgres_config(service_config)?;
        Ok(match execution_environment {
            temps_core::ExecutionEnvironment::Host => ("localhost".to_string(), config.port),
            temps_core::ExecutionEnvironment::Docker => (
                self.get_live_container_name(&config),
                POSTGRES_INTERNAL_PORT.to_string(),
            ),
        })
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
    pub(super) async fn create_container(
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
                // A failed guarded recreate has already put the original
                // container back under this name; removing "the failed
                // attempt" here would remove the original.
                Err(e)
                    if attempt < MAX_ATTEMPTS
                        && e.downcast_ref::<crate::externalsvc::recreate_guard::RecreateGuardError>(
                        )
                        .is_none()
                        && is_port_conflict_error(&e.to_string()) =>
                {
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

    pub(super) async fn create_container_once(
        &self,
        docker: &Docker,
        config: &PostgresConfig,
        resource_limits: &ServiceResourceLimits,
        enable_archiving: bool,
    ) -> Result<()> {
        let container_name = self.get_container_name();

        // Check if container already exists
        let containers = docker
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
            // Compare with the image reference the container was created
            // from. The list API reports an image ID instead once the tag has
            // been re-pulled to a newer digest, which used to look like an
            // image change and replaced the container on every init.
            let existing_image =
                crate::externalsvc::recreate_guard::created_image(docker, &container_name)
                    .await
                    .or_else(|| containers[0].image.clone())
                    .unwrap_or_default();

            if existing_image == config.docker_image {
                info!(
                    "Container {} already exists with same image",
                    container_name
                );
                return Ok(());
            }
            info!(
                "Container {} exists with different image ({}), recreating it with {}",
                container_name, existing_image, config.docker_image
            );
            self.recreate_container_guarded(
                docker,
                config,
                resource_limits,
                enable_archiving,
                "changing its image",
            )
            .await?;
            return Ok(());
        }

        info!("Pulling PostgreSQL image {}", config.docker_image);
        crate::utils::pull_image_with_retry(docker, &config.docker_image, None)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        self.create_fresh_container(docker, config, resource_limits, enable_archiving)
            .await
    }

    /// The storage a container created from `config` mounts, for
    /// [`recreate_guard`](crate::externalsvc::recreate_guard).
    pub(super) fn replacement_storage(
        &self,
        config: &PostgresConfig,
    ) -> Result<crate::externalsvc::recreate_guard::ReplacementStorage> {
        Ok(crate::externalsvc::recreate_guard::ReplacementStorage {
            data_dir: Self::get_pgdata_path(&config.docker_image)?,
            mounts: vec![crate::externalsvc::recreate_guard::PlannedMount {
                volume: postgres_volume_names(&self.name)[0].clone(),
                target: "/var/lib/postgresql".to_string(),
            }],
        })
    }

    /// Replace this managed service's existing container with one built from
    /// `config`, through the shared guard: refuse unless the new container
    /// mounts the same data, keep the old one until the new one is verified
    /// to hold the same cluster, and roll back otherwise. Returns whether a
    /// container was actually replaced.
    pub(super) async fn recreate_container_guarded(
        &self,
        docker: &Docker,
        config: &PostgresConfig,
        resource_limits: &ServiceResourceLimits,
        enable_archiving: bool,
        operation: &str,
    ) -> Result<crate::externalsvc::recreate_guard::RecreateReport> {
        if let Some(imported) = &config.container_name {
            return Err(anyhow::anyhow!(
                "Refusing to recreate imported PostgreSQL container '{}' of service '{}' ({}): \
                 Temps does not manage its lifecycle or storage. Nothing was changed",
                imported,
                self.name,
                operation
            ));
        }
        let container_name = self.get_container_name();
        let replacement = self.replacement_storage(config)?;
        // Pull while the original still serves: a pull failure must not
        // cost any downtime.
        info!("Pulling PostgreSQL image {}", config.docker_image);
        crate::utils::pull_image_with_retry(docker, &config.docker_image, None)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        let report = crate::externalsvc::recreate_guard::recreate_preserving_data(
            crate::externalsvc::recreate_guard::RecreateRequest {
                docker,
                engine: crate::externalsvc::recreate_guard::ManagedEngine::Postgres,
                service_name: &self.name,
                container_name: &container_name,
                replacement,
                operation,
            },
            || self.create_fresh_container(docker, config, resource_limits, enable_archiving),
        )
        .await?;
        Ok(report)
    }

    /// Pull, create, start and wait for a new container. The caller has made
    /// sure no container with this name exists.
    async fn create_fresh_container(
        &self,
        docker: &Docker,
        config: &PostgresConfig,
        resource_limits: &ServiceResourceLimits,
        enable_archiving: bool,
    ) -> Result<()> {
        let container_name = self.get_container_name();
        let volume_name = format!("{}_data", container_name);
        docker
            .create_volume(bollard::models::VolumeCreateRequest {
                name: Some(volume_name.clone()),
                ..Default::default()
            })
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create volume {}: {:?}", volume_name, e))?;
        info!("Created or reused volume {}", volume_name);

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
            init: POSTGRES_CONTAINER_INIT,
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
                // Enable pg_stat_statements at provision time so the extension
                // can be created after startup. A restart (not just a reload)
                // is required to change shared_preload_libraries, so this is
                // set once at container-creation time. Existing services
                // provisioned before this change need a container restart
                // (via the restart endpoint) for the change to take effect.
                // Merged with any library the image itself requires (e.g.
                // `timescaledb`) — see `shared_preload_libraries_for_image`.
                "-c".to_string(),
                format!(
                    "shared_preload_libraries={}",
                    Self::shared_preload_libraries_for_image(&config.docker_image)
                ),
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

        // ADR-055 Phase 4: a database in `enforce` mode is created from its
        // admitted workload spec through the Docker driver; every other
        // database sends this request exactly as before.
        let container_id = match crate::workload::registry::enforced_create(
            docker,
            &container_name,
            &container_config,
        )
        .await
        {
            Some(created) => created
                .map_err(|e| anyhow::anyhow!("Failed to create PostgreSQL container: {}", e))?,
            None => {
                docker
                    .create_container(
                        Some(
                            bollard::query_parameters::CreateContainerOptionsBuilder::new()
                                .name(&container_name)
                                .build(),
                        ),
                        container_config,
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to create PostgreSQL container: {}", e))?
                    .id
            }
        };

        docker
            .start_container(
                &container_id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start PostgreSQL container: {}", e))?;

        // Wait for container to be healthy
        self.wait_for_container_health(docker, &container_id)
            .await?;

        info!("PostgreSQL container {} created and started", container_id);
        Ok(())
    }

    /// Returns true when the running container's CMD specifies an
    /// `archive_mode` value that disagrees with what we'd emit now.
    /// Returns false when the value matches OR when we can't determine it
    /// (don't recreate on inspection failure — stability over correctness
    /// for this branch).
    pub(super) async fn container_cmd_archive_mode_differs(
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

    /// Whether the container's `shared_preload_libraries` CMD flag disagrees
    /// with what this image now requires (see
    /// `shared_preload_libraries_for_image`).
    ///
    /// Without this check, `start()`'s drift-reconciliation only looked at
    /// `archive_mode` — a plain restart of a container created before the
    /// `pg_stat_statements` CMD flag existed (or with a stale library list)
    /// would never pick up the correct flag, since the container was never
    /// recreated, only started. `enable_pg_stat_statements` in
    /// `pg_stat_statements.rs` relies on this drift check via its stop+start
    /// call to actually recreate the container with the right CMD.
    pub(super) async fn container_cmd_shared_preload_libraries_differs(
        &self,
        container: &bollard::models::ContainerSummary,
        desired: &str,
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

        let actual = cmd
            .iter()
            .find_map(|tok| tok.trim().strip_prefix("shared_preload_libraries="));

        match actual {
            Some(actual) => {
                // Order-insensitive compare — "timescaledb,pg_stat_statements"
                // and "pg_stat_statements,timescaledb" are equivalent.
                let mut actual_libs: Vec<&str> = actual.split(',').map(str::trim).collect();
                let mut desired_libs: Vec<&str> = desired.split(',').map(str::trim).collect();
                actual_libs.sort_unstable();
                desired_libs.sort_unstable();
                actual_libs != desired_libs
            }
            // No flag at all predates pg_stat_statements support — drift.
            None => true,
        }
    }

    /// Whether the container predates `POSTGRES_CONTAINER_INIT` and still
    /// runs the postmaster as PID 1. Returns false when it can't be
    /// determined, like the CMD drift checks above.
    pub(super) async fn container_missing_init(
        &self,
        container: &bollard::models::ContainerSummary,
    ) -> bool {
        let id = match container.id.as_deref() {
            Some(id) => id,
            None => return false,
        };
        match self
            .docker
            .inspect_container(
                id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(info) => info
                .host_config
                .as_ref()
                .is_some_and(host_config_missing_init),
            Err(_) => false,
        }
    }

    pub(super) async fn wait_for_container_health(
        &self,
        docker: &Docker,
        container_id: &str,
    ) -> Result<()> {
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
    pub(super) fn validate_database_name(name: &str) -> Result<()> {
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

    pub(super) async fn create_database(
        &self,
        service_config: ServiceConfig,
        name: &str,
    ) -> Result<()> {
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

        // Read the owner of the database, if it exists, using a parameterized
        // query: a sandbox's database must never be handed to a deployment.
        let owner: Option<String> =
            sqlx::query_scalar(crate::sandbox_access::SandboxDatabasePlan::DATABASE_OWNER_QUERY)
                .bind(name)
                .fetch_optional(&pool)
                .await
                .map_err(|e| {
                    anyhow::anyhow!("Failed to check whether database '{}' exists: {}", name, e)
                })?;
        crate::sandbox_access::check_deployment_database(name, owner.as_deref())
            .map_err(anyhow::Error::new)?;

        if owner.is_none() {
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

    pub(super) async fn drop_database(&self, _name: &str) -> Result<()> {
        Ok(())
    }

    /// Build the `POSTGRES_*` env vars for a given per-tenant resource name.
    /// Shared between `get_runtime_env_vars` (which also provisions the DB)
    /// and `preview_runtime_env_vars` (which doesn't).
    pub(super) fn build_runtime_env_vars(
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

        let prefixed = if normalized.chars().next().is_some_and(char::is_numeric) {
            format!("db_{}", normalized)
        } else {
            normalized
        };

        // PostgreSQL identifiers are at most 63 bytes. `is_alphanumeric` keeps
        // non-ASCII letters, so cut at the last character boundary within that
        // limit instead of mid-character (which panicked). Every name that fit
        // before is cut at the same place, so existing databases keep theirs.
        let mut end = prefixed.len().min(63);
        while !prefixed.is_char_boundary(end) {
            end -= 1;
        }
        prefixed[..end].to_string()
    }

    /// Extract PostgreSQL major version from Docker image name
    /// Examples: "gotempsh/postgres-walg:16-bookworm" -> 16, "timescale/timescaledb-ha:pg18" -> 18
    pub(super) fn extract_postgres_version(docker_image: &str) -> Result<u32> {
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
    pub(super) fn get_pgdata_path(docker_image: &str) -> Result<String> {
        let version = Self::extract_postgres_version(docker_image)?;
        Ok(format!("/var/lib/postgresql/{}/docker", version))
    }

    /// Build the `shared_preload_libraries` value for this image.
    ///
    /// `shared_preload_libraries` is a single comma-separated GUC — the last
    /// `-c shared_preload_libraries=...` flag on the command line wins, it
    /// does not merge with earlier ones. Images that require their own
    /// preload library (e.g. `timescale/timescaledb-ha` requires
    /// `timescaledb`) must have it listed alongside `pg_stat_statements`,
    /// never overwritten by it — dropping `timescaledb` here silently
    /// disables hypertables' background workers, continuous aggregates, and
    /// compression policies with no error until a Timescale feature is used.
    pub(super) fn shared_preload_libraries_for_image(docker_image: &str) -> String {
        let mut libs = Vec::new();
        if docker_image.contains("timescaledb") {
            libs.push("timescaledb");
        }
        libs.push("pg_stat_statements");
        libs.join(",")
    }

    /// Verify that a Docker image can be pulled without actually downloading the full image
    /// Attempts to pull the image - fails if it doesn't exist or cannot be accessed
    pub(super) async fn verify_image_pullable(&self, image: &str) -> Result<()> {
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

/// Internal port used by PostgreSQL inside the container
pub(super) const POSTGRES_INTERNAL_PORT: &str = "5432";

/// Container name of a service named `name`. The single derivation shared by
/// the engine and anything that must find its container without an instance.
pub(crate) fn postgres_container_name(name: &str) -> String {
    format!("postgres-{}", name)
}

/// Data volumes of a service named `name`, as `remove()` deletes them.
pub(crate) fn postgres_volume_names(name: &str) -> [String; 1] {
    [format!("postgres-{}_data", name)]
}

#[cfg(test)]
mod tests;
