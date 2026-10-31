// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! WAL-G setup and continuous archiving for managed PostgreSQL.

use super::*;
use crate::externalsvc::{ExternalService, ServiceConfig, ServiceResourceLimits};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{info, warn};
use urlencoding;

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

pub(super) fn shell_export_assignment(line: &str) -> Option<String> {
    let (key, value) = line.split_once('=')?;
    let valid_key = key
        .chars()
        .all(|character| character == '_' || character.is_ascii_alphanumeric())
        && key
            .chars()
            .next()
            .is_some_and(|character| character == '_' || character.is_ascii_alphabetic());
    valid_key.then(|| format!("export {key}={}", shell_escape(value)))
}

/// What enabling WAL archiving did to the running service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalArchivingOutcome {
    /// PostgreSQL was restarted in a recreated container (archive_mode was
    /// off). The data was verified to be the same cluster before this was
    /// reported.
    pub restarted: bool,
}

/// Enabling WAL archiving would restart PostgreSQL, and the caller did not
/// agree to a restart (a manual backup run without `confirm_restart`).
/// Decided before anything is changed, or with the change undone.
#[derive(Debug, thiserror::Error)]
#[error(
    "Enabling WAL archiving for PostgreSQL service '{service_name}' needs a restart (archive_mode \
     is off), and a restart was not confirmed. Nothing was restarted. Run the backup again and \
     confirm the restart"
)]
pub struct PostgresRestartNotConfirmed {
    pub service_name: String,
}

#[derive(Debug, thiserror::Error)]
#[error("Failed to verify WAL archiving for PostgreSQL service '{service_name}': {reason}. Check this service's logs and retry this operation")]
pub(super) struct PostgresArchiveVerificationError {
    pub(super) service_name: String,
    pub(super) reason: String,
}

pub(super) fn walg_target_user_data(
    backup: &temps_entities::backups::Model,
) -> Result<Option<String>> {
    let metadata: serde_json::Value = serde_json::from_str(&backup.metadata).map_err(|e| {
        anyhow::anyhow!(
            "Backup {} has invalid metadata JSON: {}",
            backup.backup_id,
            e
        )
    })?;
    let Some(version) = metadata.get("walg_identity_version") else {
        return Ok(None);
    };
    if version.as_u64() != Some(1) {
        return Err(anyhow::anyhow!(
            "Backup {} uses unsupported WAL-G identity version {}",
            backup.backup_id,
            version
        ));
    }
    let value = metadata.get("walg_target_user_data").ok_or_else(|| {
        anyhow::anyhow!(
            "Backup {} is missing its WAL-G target user data",
            backup.backup_id
        )
    })?;
    if value
        .get("temps_backup_id")
        .and_then(serde_json::Value::as_str)
        != Some(backup.backup_id.as_str())
    {
        return Err(anyhow::anyhow!(
            "Backup {} has WAL-G target user data for a different backup",
            backup.backup_id
        ));
    }
    Ok(Some(serde_json::to_string(value).map_err(|e| {
        anyhow::anyhow!(
            "Failed to serialize WAL-G target user data for backup {}: {}",
            backup.backup_id,
            e
        )
    })?))
}

impl PostgresService {
    /// Read a file from inside a container and return its contents as a String.
    /// Used for capturing log output on failure. Returns a fallback message on error.
    pub(super) async fn read_container_file(&self, container_name: &str, path: &str) -> String {
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
    pub(super) async fn container_has_walg(&self, container_name: &str) -> bool {
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
    /// `enable_wal_archiving` before a recoverable WAL-G backup.
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
    pub(super) async fn write_walg_restore_env_file(
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
                .filter_map(|line| shell_export_assignment(line)
                    .map(|assignment| shell_escape(&assignment)))
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
    pub(super) async fn enable_wal_archiving(
        &self,
        container_name: &str,
        walg_env: &[String],
        postgres_config: &PostgresConfig,
        allow_restart: bool,
    ) -> Result<WalArchivingOutcome> {
        use bollard::exec::{CreateExecOptions, StartExecOptions};

        if postgres_config.container_name.is_some() {
            let (mode, _) = self.archive_settings(postgres_config, None).await?;
            if mode == "off" {
                return Err(PostgresArchiveVerificationError {
                    service_name: self.name.clone(),
                    reason: "the imported database has archive_mode off. Enable archiving in its external startup configuration and restart it, or back up a new managed service; the imported container and data have not been changed".to_string(),
                }
                .into());
            }
        }

        // A managed service with archive_mode off will need its container
        // recreated (step 3). Refuse before writing anything if the new
        // container would not mount the data the current one uses (#1415):
        // a refusal must leave the service exactly as it was, and a stray
        // walg.env would make every later start try the same recreate.
        if postgres_config.container_name.is_none() {
            let (mode, _) = self.archive_settings(postgres_config, None).await?;
            if mode == "off" {
                if !allow_restart {
                    return Err(PostgresRestartNotConfirmed {
                        service_name: self.name.clone(),
                    }
                    .into());
                }
                let replacement = self.replacement_storage(postgres_config)?;
                crate::externalsvc::recreate_guard::preflight(
                    &self.docker,
                    crate::externalsvc::recreate_guard::ManagedEngine::Postgres,
                    &self.name,
                    &self.get_container_name(),
                    &replacement,
                    "enabling WAL archiving",
                )
                .await?;
            }
        }

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

        // Marker presence describes configured credentials, not active
        // PostgreSQL settings. Verify the reload before deciding whether a
        // postmaster restart is required; an ordinary backup must not cause
        // downtime when archiving already works.
        let (mode, command) = self
            .active_archive_settings(postgres_config, &archive_command)
            .await?;
        if matches!(mode.as_str(), "on" | "always") && command != archive_command {
            return Err(PostgresArchiveVerificationError {
                service_name: self.name.clone(),
                reason: "the WAL-G archive command did not apply after reload".to_string(),
            }
            .into());
        }
        if mode == "on" || mode == "always" {
            return Ok(WalArchivingOutcome { restarted: false });
        }
        // An operator can restart an imported workload while the settings
        // reload is in flight. Its physical container must never enter the
        // managed recreation path, regardless of either settings snapshot.
        if postgres_config.container_name.is_some() {
            return Err(PostgresArchiveVerificationError {
                service_name: self.name.clone(),
                reason: "the imported database now has archive_mode off. Enable archiving and restart it using its external lifecycle before retrying; Temps will not recreate this imported container".to_string(),
            }
            .into());
        }
        info!(
            "Wrote walg.env + archive_command in container '{}'. Recreating container so archive_mode=on lands in CMD.",
            container_name
        );

        // archive_mode can have been turned off since the check above (an
        // in-place restore finishing meanwhile): the restart is decided here,
        // so the caller's consent is enforced here too. Undo walg.env, or
        // every later start would try the same recreate.
        if !allow_restart {
            self.remove_walg_env(container_name).await;
            return Err(PostgresRestartNotConfirmed {
                service_name: self.name.clone(),
            }
            .into());
        }

        // Step 3: recreate so archive_mode=on lands in CMD args. A
        // command-line `-c archive_mode=off` outranks ALTER SYSTEM, so a
        // restart alone cannot turn it on for a container created with it.
        // The shared guard refuses unless the new container mounts the same
        // data, keeps the old container until the new one is verified to
        // hold the same cluster, and puts the old one back otherwise.
        warn!(
            service = %self.name,
            container = container_name,
            "Enabling WAL archiving restarts PostgreSQL: recreating its container with archive_mode=on"
        );
        let limits = self.resource_limits.read().await.clone();
        // A backup's provider has no persistence path for a new published
        // port. Preserve this registered endpoint instead of silently choosing
        // a different port on a bind conflict; its data volume remains intact.
        if let Err(e) = self
            .recreate_container_guarded(
                &self.docker,
                postgres_config,
                &limits,
                true,
                "enabling WAL archiving",
            )
            .await
        {
            // The original container is back (or was never touched). Take
            // walg.env away again, or every later start would see "archiving
            // configured" and try the same recreate.
            self.remove_walg_env(container_name).await;
            return Err(e);
        }

        let (mode, command) = self
            .active_archive_settings(postgres_config, &archive_command)
            .await?;
        if !matches!(mode.as_str(), "on" | "always") || command != archive_command {
            return Err(PostgresArchiveVerificationError {
                service_name: self.name.clone(),
                reason: "the service restarted without active WAL-G archiving".to_string(),
            }
            .into());
        }

        info!(
            "Recreated container '{}' with archive_mode=on. WAL-G archiving active.",
            container_name
        );

        Ok(WalArchivingOutcome { restarted: true })
    }

    /// Remove `walg.env` from the service's volume, best effort.
    async fn remove_walg_env(&self, container_name: &str) {
        if let Err(cleanup) = crate::externalsvc::exec_util::run_exec(
            &self.docker,
            container_name,
            vec![
                "rm".to_string(),
                "-f".to_string(),
                "/var/lib/postgresql/walg.env".to_string(),
            ],
            None,
            Duration::from_secs(30),
        )
        .await
        {
            warn!(
                service = %self.name,
                container = container_name,
                error = %cleanup,
                "Could not remove walg.env after WAL archiving was not enabled"
            );
        }
    }

    /// Read settings from PostgreSQL itself, rather than inferring them
    /// from a credential file. Bounded control-plane work before a backup.
    pub(super) async fn active_archive_settings(
        &self,
        config: &PostgresConfig,
        expected_command: &str,
    ) -> std::result::Result<(String, String), PostgresArchiveVerificationError> {
        self.archive_settings(config, Some(expected_command)).await
    }

    async fn archive_settings(
        &self,
        config: &PostgresConfig,
        expected_command: Option<&str>,
    ) -> std::result::Result<(String, String), PostgresArchiveVerificationError> {
        let failure = |reason: String| PostgresArchiveVerificationError {
            service_name: self.name.clone(),
            reason,
        };
        let connection = format!(
            "postgres://{}:{}@{}:{}/{}?sslmode={}",
            urlencoding::encode(&config.username),
            urlencoding::encode(&config.password),
            config.host,
            config.port,
            urlencoding::encode(&config.database),
            urlencoding::encode(config.ssl_mode.as_deref().unwrap_or("disable")),
        );
        let pool = tokio::time::timeout(
            Duration::from_secs(5),
            sqlx::postgres::PgPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(Duration::from_secs(5))
                .connect(&connection),
        )
        .await
        .map_err(|_| failure("database connection timed out after 5 seconds".to_string()))?
        .map_err(|error| failure(error.to_string()))?;
        // pg_reload_conf signals the postmaster asynchronously. Wait for
        // its reload to become visible before taking the setting snapshot.
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let settings: (String, String) = sqlx::query_as(
                    "SELECT current_setting('archive_mode'), current_setting('archive_command')",
                )
                .fetch_one(&pool)
                .await?;
                // PostgreSQL's archive_command show hook returns "(disabled)"
                // while archive_mode is off, even after ALTER SYSTEM and reload
                // applied the command. Return the disabled policy so the caller
                // can perform the postmaster restart; verify the visible command
                // once archiving is active.
                if settings.0 == "off"
                    || expected_command.is_none_or(|command| settings.1 == command)
                {
                    return Ok::<_, sqlx::Error>(settings);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await;
        pool.close().await;
        result
            .map_err(|_| {
                failure("the expected archive command did not apply within 5 seconds".to_string())
            })?
            .map_err(|error| failure(error.to_string()))
    }

    /// Ask the running database whether startup arguments override its
    /// archiving policy. Entrypoint wrappers can hide PostgreSQL arguments
    /// from Docker's CMD; reject them before fetching or replacing data.
    pub(super) async fn imported_archive_mode_source(
        &self,
        config: &PostgresConfig,
    ) -> std::result::Result<bool, PostgresArchiveVerificationError> {
        let failure = |reason: String| {
            PostgresArchiveVerificationError {
            service_name: self.name.clone(),
            reason: format!(
                "Cannot verify the imported container's startup archive policy before restore: {reason}; the original database has not been changed"
            ),
        }
        };
        let connection = format!(
            "postgres://{}:{}@{}:{}/{}?sslmode={}",
            urlencoding::encode(&config.username),
            urlencoding::encode(&config.password),
            config.host,
            config.port,
            urlencoding::encode(&config.database),
            urlencoding::encode(config.ssl_mode.as_deref().unwrap_or("disable")),
        );
        let pool = tokio::time::timeout(
            Duration::from_secs(5),
            sqlx::postgres::PgPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(Duration::from_secs(5))
                .connect(&connection),
        )
        .await
        .map_err(|_| failure("database connection timed out after 5 seconds".to_string()))?
        .map_err(|error| failure(error.to_string()))?;
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            sqlx::query_as::<_, (String, String)>(
                "SELECT setting, source FROM pg_settings WHERE name = 'archive_mode'",
            )
            .fetch_one(&pool),
        )
        .await;
        pool.close().await;
        let (mode, source) = result
            .map_err(|_| failure("settings query timed out after 5 seconds".to_string()))?
            .map_err(|error| failure(error.to_string()))?;
        Ok(imported_archive_mode_is_command_line(&mode, &source))
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
    pub(super) async fn compute_desired_enable_archiving(&self) -> bool {
        let container_name = self.get_container_name();
        self.walg_env_exists_in_container(&container_name).await
    }

    /// Returns true iff `/var/lib/postgresql/walg.env` exists on this
    /// service's data volume. Docker's archive API can read a stopped
    /// container's mounted volume without pulling or executing a mutable
    /// helper image against database data.
    ///
    /// The live container is probed directly when it exists. Otherwise
    /// (fresh create, `force_recreate`, an externally-removed container, or
    /// node failover) there's no container to probe, but the volume can
    /// still hold a `walg.env` from a prior life -- probed via a throwaway
    /// container that only mounts the named volume and is never started
    /// (see `walg_env_exists_on_volume`).
    async fn walg_env_exists_in_container(&self, container_name: &str) -> bool {
        let live = self
            .docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: true,
                filters: Some(HashMap::from([(
                    "name".to_string(),
                    vec![crate::externalsvc::exact_container_name_filter(
                        container_name,
                    )],
                )])),
                ..Default::default()
            }))
            .await
            .map(|containers| !containers.is_empty())
            .unwrap_or(false);

        if live {
            return self.download_walg_env(container_name).await;
        }

        let volume_name = format!("{container_name}_data");
        let probe_image = self
            .config
            .read()
            .await
            .as_ref()
            .map(|c| c.docker_image.clone())
            .unwrap_or_else(|| {
                let (image, tag) = self.get_default_docker_image();
                format!("{image}:{tag}")
            });
        self.walg_env_exists_on_volume(&volume_name, &probe_image)
            .await
    }

    /// Reads `/var/lib/postgresql/walg.env` from a live container's
    /// filesystem via Docker's archive API.
    async fn download_walg_env(&self, container_name: &str) -> bool {
        use bollard::query_parameters::DownloadFromContainerOptions;
        use futures::StreamExt;

        let mut archive_stream = self.docker.download_from_container(
            container_name,
            Some(DownloadFromContainerOptions {
                path: "/var/lib/postgresql/walg.env".to_string(),
            }),
        );
        let mut saw_archive_data = false;
        while let Some(chunk) = archive_stream.next().await {
            match chunk {
                Ok(bytes) if !bytes.is_empty() => saw_archive_data = true,
                Ok(_) => {}
                Err(_) => return false,
            }
        }

        saw_archive_data
    }

    /// Probes `volume_name` for `walg.env` by creating a never-started
    /// container that mounts it at `/var/lib/postgresql`, reading it via
    /// the same archive API `download_walg_env` uses, then removing the
    /// probe container. The probe container is never started, so
    /// `probe_image`'s content never executes -- it only needs to exist so
    /// Docker can materialize the container's filesystem for the archive
    /// read. `probe_image` is the service's own already-vetted image
    /// (already local from the container that used to run against this
    /// volume), so this never pulls or trusts anything new.
    async fn walg_env_exists_on_volume(&self, volume_name: &str, probe_image: &str) -> bool {
        let probe_name = format!("{volume_name}-walg-probe");

        let _ = self
            .docker
            .remove_container(
                &probe_name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        let create_result = self
            .docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&probe_name)
                        .build(),
                ),
                bollard::models::ContainerCreateBody {
                    image: Some(probe_image.to_string()),
                    host_config: Some(bollard::models::HostConfig {
                        mounts: Some(vec![bollard::models::Mount {
                            target: Some("/var/lib/postgresql".to_string()),
                            source: Some(volume_name.to_string()),
                            typ: Some(bollard::models::MountTypeEnum::VOLUME),
                            ..Default::default()
                        }]),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await;

        let exists = match create_result {
            Ok(_) => self.download_walg_env(&probe_name).await,
            Err(_) => false,
        };

        let _ = self
            .docker
            .remove_container(
                &probe_name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        exists
    }
}

pub(super) fn imported_archive_mode_is_command_line(mode: &str, source: &str) -> bool {
    matches!(mode, "on" | "always") && source == "command line"
}

pub(super) fn postgres_cmd_pins_archive_mode_on(command: &[String]) -> bool {
    command
        .iter()
        .flat_map(|part| part.split_whitespace())
        .filter_map(|part| {
            let part = part.trim_start_matches("--").trim_start_matches("-c");
            let (key, value) = part.split_once('=')?;
            (key.replace('-', "_").eq_ignore_ascii_case("archive_mode"))
                .then(|| value.trim_matches(['\'', '"']).to_ascii_lowercase())
        })
        .next_back()
        .is_some_and(|mode| matches!(mode.as_str(), "on" | "always" | "true" | "yes" | "1"))
}

impl PostgresService {
    /// Point continuous WAL-G archiving at `s3_credentials`/`walg_prefix`
    /// explicitly. The normal backup path uses the pinned destination;
    /// repointing accepts a different destination after an audited decision.
    ///
    /// Only the explicit, operator-initiated WAL archive source repoint
    /// (`ExternalServiceManager::repoint_continuous_archive_source`) should call
    /// this. Ordinary backups are guarded by the persisted archive source pin.
    pub async fn force_reenable_continuous_archiving(
        &self,
        service_config: ServiceConfig,
        s3_credentials: &crate::externalsvc::S3Credentials,
        walg_prefix: &str,
    ) -> Result<()> {
        self.write_wal_archiving_config(service_config, s3_credentials, walg_prefix, true)
            .await
            .map(|_| ())
    }

    /// [`ExternalService::enable_continuous_archiving`], also reporting
    /// whether PostgreSQL had to be restarted, so a backup can say so. With
    /// `allow_restart` false it never restarts PostgreSQL: when turning
    /// archiving on would need one it returns [`PostgresRestartNotConfirmed`]
    /// and changes nothing.
    pub async fn enable_continuous_archiving_reporting(
        &self,
        service_config: ServiceConfig,
        s3_credentials: &crate::externalsvc::S3Credentials,
        walg_prefix: &str,
        allow_restart: bool,
    ) -> Result<WalArchivingOutcome> {
        self.write_wal_archiving_config(service_config, s3_credentials, walg_prefix, allow_restart)
            .await
    }

    /// Whether running a WAL-G backup now would restart PostgreSQL: true when
    /// this managed service has archive_mode off, so enabling archiving has
    /// to recreate its container. Bounded: one SQL round trip.
    pub async fn backup_requires_restart(&self, service_config: ServiceConfig) -> Result<bool> {
        let postgres_config = self.get_postgres_config(service_config)?;
        if postgres_config.container_name.is_some() {
            // Imported services are never recreated; enabling refuses instead.
            return Ok(false);
        }
        let (mode, _) = self.archive_settings(&postgres_config, None).await?;
        Ok(mode == "off")
    }

    /// When the restart [`Self::backup_requires_restart`] reports cannot be
    /// done safely (the new container would not see this service's data),
    /// the guard's refusal, so a client can say the backup will be refused
    /// instead of promising a restart. Read-only.
    pub async fn backup_restart_refusal(
        &self,
        service_config: ServiceConfig,
    ) -> Result<Option<String>> {
        let postgres_config = self.get_postgres_config(service_config)?;
        if postgres_config.container_name.is_some() {
            return Ok(None);
        }
        let replacement = self.replacement_storage(&postgres_config)?;
        match crate::externalsvc::recreate_guard::preflight(
            &self.docker,
            crate::externalsvc::recreate_guard::ManagedEngine::Postgres,
            &self.name,
            &self.get_container_name(),
            &replacement,
            "enabling WAL archiving",
        )
        .await
        {
            Ok(_) => Ok(None),
            Err(e) if e.is_refusal() => Ok(Some(e.to_string())),
            Err(e) => Err(e.into()),
        }
    }

    pub(super) async fn write_wal_archiving_config(
        &self,
        service_config: ServiceConfig,
        s3_credentials: &crate::externalsvc::S3Credentials,
        walg_prefix: &str,
        allow_restart: bool,
    ) -> Result<WalArchivingOutcome> {
        // Backup engines construct a fresh provider. Hydrate its limits from
        // the registered service before an off-policy managed recreation so
        // enabling archiving cannot remove the workload's resource bounds.
        let limits = ServiceResourceLimits::from_parameters(&service_config.parameters);
        limits
            .validate()
            .map_err(|reason| PostgresArchiveVerificationError {
                service_name: self.name.clone(),
                reason: format!("invalid WAL archive service resource limits: {reason}"),
            })?;
        *self.resource_limits.write().await = limits;
        let postgres_config = self.get_postgres_config(service_config)?;
        let container_name = self.get_live_container_name(&postgres_config);

        let mut walg_env: Vec<String> = vec![
            format!("WALG_S3_PREFIX={}", walg_prefix),
            format!("AWS_ACCESS_KEY_ID={}", s3_credentials.access_key_id),
            format!("AWS_SECRET_ACCESS_KEY={}", s3_credentials.secret_key),
            format!("AWS_REGION={}", s3_credentials.region),
        ];
        // Absent unless this source holds a temporary (STS-style)
        // credential, so a long-lived one produces the exact environment
        // it always did.
        walg_env.extend(s3_credentials.session_token_env());
        if let Some(resolved_endpoint) = s3_credentials
            .resolve_endpoint_for_container(&self.docker, &container_name)
            .await
        {
            walg_env.push(format!("AWS_ENDPOINT={}", resolved_endpoint));
        }
        if s3_credentials.force_path_style {
            walg_env.push("AWS_S3_FORCE_PATH_STYLE=true".to_string());
        }

        self.enable_wal_archiving(&container_name, &walg_env, &postgres_config, allow_restart)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "docker-tests")]
    use crate::externalsvc::{ExternalService, ServiceConfig};
    use bollard::Docker;
    use std::sync::Arc;

    /// A private, run-owned PostgreSQL fixture exercises PostgreSQL's real
    /// archive_command show hook. The fixture supplies separate off/on servers;
    /// this test only reads their settings and never changes their workloads.
    #[tokio::test]
    async fn archive_settings_handles_postgres_disabled_command_display() {
        let Some(fixture_path) = std::env::var_os("TEMPS_TEST_ARCHIVE_SETTINGS_FIXTURE") else {
            eprintln!(
                "Skipping archive settings integration regression: no owned fixture supplied"
            );
            return;
        };
        #[derive(serde::Deserialize)]
        struct Fixture {
            off: PostgresConfig,
            on: PostgresConfig,
        }
        let fixture: Fixture = serde_json::from_slice(
            &std::fs::read(fixture_path).expect("read private owned PostgreSQL fixture"),
        )
        .expect("parse private owned PostgreSQL fixture");
        let docker = Arc::new(Docker::connect_with_local_defaults().expect("Docker configuration"));
        let service = PostgresService::new("archive-settings-regression".to_string(), docker);
        let off = service
            .active_archive_settings(&fixture.off, "/bin/true")
            .await
            .expect("off policy must not wait for a command hidden by PostgreSQL");
        assert_eq!(off, ("off".to_string(), "(disabled)".to_string()));
        assert!(fixture.off.container_name.is_some());
        let refusal = service
            .enable_wal_archiving(
                fixture.off.container_name.as_deref().unwrap(),
                &["WALG_S3_PREFIX=s3://test-bucket/unused".to_string()],
                &fixture.off,
                true,
            )
            .await
            .expect_err("an imported off-policy container must never be recreated");
        assert!(refusal
            .to_string()
            .contains("imported database has archive_mode off"));
        assert_eq!(
            service
                .active_archive_settings(&fixture.off, "/bin/true")
                .await
                .expect("the refused imported workload remains usable"),
            off
        );
        let on = service
            .active_archive_settings(&fixture.on, "/bin/true")
            .await
            .expect("on policy must expose the exact configured command");
        assert_eq!(on, ("on".to_string(), "/bin/true".to_string()));
        let error = service
            .active_archive_settings(&fixture.on, "wal-g wal-push %p")
            .await
            .expect_err("active archiving must reject a different command");
        assert!(error
            .reason
            .contains("expected archive command did not apply"));
    }

    #[test]
    fn imported_restore_rejects_effective_command_line_archive_policy() {
        assert!(imported_archive_mode_is_command_line("on", "command line"));
        assert!(imported_archive_mode_is_command_line(
            "always",
            "command line"
        ));
        assert!(!imported_archive_mode_is_command_line(
            "off",
            "command line"
        ));
        assert!(!imported_archive_mode_is_command_line(
            "on",
            "configuration file"
        ));
    }

    #[test]
    fn imported_restore_detects_startup_archive_mode_override() {
        let command = |args: &[&str]| {
            args.iter()
                .map(|arg| String::from(*arg))
                .collect::<Vec<_>>()
        };
        assert!(postgres_cmd_pins_archive_mode_on(&command(&[
            "postgres",
            "-c",
            "archive_mode=on"
        ])));
        assert!(postgres_cmd_pins_archive_mode_on(&command(&[
            "postgres",
            "-carchive_mode=always"
        ])));
        assert!(postgres_cmd_pins_archive_mode_on(&command(&[
            "postgres",
            "--archive_mode=on"
        ])));
        assert!(!postgres_cmd_pins_archive_mode_on(&command(&[
            "postgres",
            "-c",
            "archive_mode=off"
        ])));
        assert!(!postgres_cmd_pins_archive_mode_on(&command(&[
            "postgres",
            "-c",
            "archive_mode=on",
            "-c",
            "archive_mode=off"
        ])));
        assert!(!postgres_cmd_pins_archive_mode_on(&command(&[
            "postgres",
            "-c",
            "archive_command=/bin/true"
        ])));
        assert!(postgres_cmd_pins_archive_mode_on(&command(&[
            "postgres",
            "-c",
            "ARCHIVE_MODE=ON"
        ])));
        assert!(postgres_cmd_pins_archive_mode_on(&command(&[
            "postgres",
            "-cARCHIVE_MODE=ALWAYS"
        ])));
        assert!(!postgres_cmd_pins_archive_mode_on(&command(&[
            "postgres",
            "-c",
            "ARCHIVE_MODE=on",
            "-c",
            "archive_mode=off"
        ])));
        assert!(postgres_cmd_pins_archive_mode_on(&command(&[
            "postgres",
            "-c",
            "archive_mode=off",
            "-c",
            "ARCHIVE_MODE=always"
        ])));

        assert!(postgres_cmd_pins_archive_mode_on(&command(&[
            "postgres",
            "--archive-mode=on"
        ])));
        assert!(!postgres_cmd_pins_archive_mode_on(&command(&[
            "postgres",
            "--ARCHIVE-MODE=always",
            "--archive_mode=off"
        ])));
    }

    fn backup_with_metadata(metadata: serde_json::Value) -> temps_entities::backups::Model {
        temps_entities::backups::Model {
            id: 1,
            name: "backup".into(),
            backup_id: "selected-backup".into(),
            schedule_id: None,
            schedule_run_id: None,
            backup_type: "external_service".into(),
            state: "completed".into(),
            started_at: chrono::Utc::now(),
            finished_at: Some(chrono::Utc::now()),
            size_bytes: None,
            file_count: None,
            s3_source_id: 1,
            s3_location: "s3://bucket/repository".into(),
            error_message: None,
            metadata: metadata.to_string(),
            checksum: None,
            compression_type: "lz4".into(),
            created_by: 1,
            expires_at: None,
            tags: "[]".into(),
        }
    }

    #[test]
    fn walg_restore_selector_uses_persisted_backup_identity() {
        let backup = backup_with_metadata(serde_json::json!({
            "walg_identity_version": 1,
            "walg_target_user_data": {"temps_backup_id": "selected-backup"}
        }));
        assert_eq!(
            walg_target_user_data(&backup).expect("valid metadata should parse"),
            Some(r#"{"temps_backup_id":"selected-backup"}"#.to_string())
        );
    }

    #[test]
    fn walg_restore_selector_rejects_incomplete_identity_metadata() {
        let backup = backup_with_metadata(serde_json::json!({"walg_identity_version": 1}));
        let error = walg_target_user_data(&backup).expect_err("missing selector must fail closed");
        assert!(error
            .to_string()
            .contains("missing its WAL-G target user data"));
    }

    #[test]
    fn walg_restore_selector_rejects_mismatched_backup_identity() {
        let backup = backup_with_metadata(serde_json::json!({
            "walg_identity_version": 1,
            "walg_target_user_data": {"temps_backup_id": "different-backup"}
        }));
        let error = walg_target_user_data(&backup).expect_err("mismatched selector must fail");
        assert!(error.to_string().contains("for a different backup"));
    }

    #[test]
    fn sourced_walg_values_are_shell_quoted() {
        assert_eq!(
            shell_export_assignment("WALG_S3_PREFIX=s3://bucket/ok;touch${IFS}/tmp/pwn;#"),
            Some("export WALG_S3_PREFIX='s3://bucket/ok;touch${IFS}/tmp/pwn;#'".to_string())
        );
        assert_eq!(
            shell_export_assignment("AWS_SECRET_ACCESS_KEY=abc'def"),
            Some("export AWS_SECRET_ACCESS_KEY='abc'\\''def'".to_string())
        );
        assert!(shell_export_assignment("BAD-KEY=value").is_none());
    }

    /// Regression test: `compute_desired_enable_archiving()` must find a
    /// pre-existing `walg.env` on the data volume even when there is no
    /// live container to probe directly. Without the volume-probe fallback,
    /// `start()`'s `need_create` path (force_recreate, node failover, or an
    /// externally-removed container) would silently bake `archive_mode=off`
    /// into the recreated container even though WAL-G was already
    /// configured on the volume.
    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_walg_env_probed_from_volume_when_container_missing() {
        use std::net::TcpListener;
        let port = TcpListener::bind("127.0.0.1:0")
            .expect("failed to bind for port allocation")
            .local_addr()
            .expect("failed to read local addr")
            .port();

        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = PostgresService::new("test-walg-volume-probe".to_string(), docker.clone());

        let config = ServiceConfig {
            name: "test-walg-volume-probe".to_string(),
            service_type: crate::externalsvc::ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": port.to_string(),
                "database": "testdb",
                "username": "testuser",
                "password": "testpass123",
                "max_connections": 100,
                "ssl_mode": "disable",
                "docker_image": "gotempsh/postgres-walg:18-bookworm"
            }),
        };

        service.init(config).await.expect("init should succeed");
        let container_name = service.get_container_name();

        // No walg.env written yet -- archiving must read as not desired.
        assert!(
            !service.compute_desired_enable_archiving().await,
            "fresh service should not have archiving enabled"
        );

        service
            .write_walg_env_file(&container_name, &["AWS_ACCESS_KEY_ID=test".to_string()])
            .await
            .expect("failed to write walg.env");

        // Sanity check: the live container reports archiving desired.
        assert!(
            service.compute_desired_enable_archiving().await,
            "live container should report walg.env as present"
        );

        // Remove the container but keep the volume, simulating
        // force_recreate / node failover / an externally-removed container.
        docker
            .remove_container(
                &container_name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .expect("failed to remove container");

        assert!(
            service.compute_desired_enable_archiving().await,
            "archiving must still read as desired from the volume once the container is gone"
        );

        // Cleanup: volume (container is already gone).
        let _ = docker
            .remove_volume(
                &format!("{container_name}_data"),
                None::<bollard::query_parameters::RemoveVolumeOptions>,
            )
            .await;
    }
}
