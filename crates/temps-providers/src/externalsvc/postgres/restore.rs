// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Restoring managed PostgreSQL from WAL-G and pg_dump backups.

use super::*;
use crate::externalsvc::{ExternalService, ServiceConfig, ServiceResourceLimits};
use bollard::Docker;
use futures::StreamExt;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};
use urlencoding;

/// Where a WAL-G restore records the PID of its `backup-fetch`, so a
/// cancelled restore can stop the fetch without relying on `pkill`.
pub(super) const WALG_FETCH_PID_FILE: &str = "/tmp/walg_fetch.pid";

/// Scratch directory on the data volume a WAL-G restore fetches into.
pub(super) const WALG_RESTORE_TEMP: &str = "/var/lib/postgresql/restore_temp";

pub(super) fn postgres_startup_error_is_transient(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Io(error) => matches!(
            error.kind(),
            std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::NotConnected
                | std::io::ErrorKind::UnexpectedEof
                | std::io::ErrorKind::TimedOut
        ),
        sqlx::Error::Database(error) => {
            matches!(error.code().as_deref(), Some("57P01" | "57P02" | "57P03"))
        }
        _ => false,
    }
}

/// Called under the restore's readiness deadline. A running imported container
/// may have no Docker healthcheck and still be starting or replaying WAL.
pub(super) async fn wait_for_restored_postgres<F, Fut>(
    mut probe: F,
    delay: Duration,
) -> Result<(), sqlx::Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<bool, sqlx::Error>>,
{
    loop {
        match probe().await {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) if postgres_startup_error_is_transient(&error) => {}
            Err(error) => return Err(error),
        }
        sleep(delay).await;
    }
}

impl PostgresService {
    /// Shell script that stops a running WAL-G `backup-fetch` (by the PID it
    /// recorded), escalating to SIGKILL, and only then deletes what a
    /// cancelled restore staged on the volume. Exits 0 once the fetch is
    /// confirmed gone (or none was running) and 3 if it is still alive, in
    /// which case nothing is deleted.
    ///
    /// A recorded PID is only signalled while it still belongs to a
    /// `backup-fetch` process: the file can outlive the fetch (a crash, a
    /// container restart) and its PID be reused by a PostgreSQL backend,
    /// which must never be killed. The identity comes from
    /// `/proc/<pid>/cmdline`, falling back to `ps` where `/proc` is absent.
    fn walg_fetch_abort_script(restore_temp: &str) -> String {
        Self::walg_fetch_abort_script_with(restore_temp, WALG_FETCH_PID_FILE)
    }

    /// The command that runs WAL-G `backup-fetch` into `restore_temp`.
    ///
    /// The fetch runs in the background of its shell so its PID can be
    /// recorded: a cancelled restore stops it by PID (see `stop_walg_fetch`)
    /// instead of depending on pkill being installed. The PID file is removed
    /// as soon as the fetch exits (keeping its exit code), so it never names
    /// a process that is not the fetch.
    fn walg_fetch_command(restore_temp: &str, fetch_target: &str) -> String {
        format!(
            "mkdir -p {restore_temp} && rm -rf {restore_temp}/* && {{ wal-g backup-fetch {restore_temp} {fetch_target} > /tmp/walg_restore.log 2>&1 & echo $! > {WALG_FETCH_PID_FILE}; wait $!; rc=$?; rm -f {WALG_FETCH_PID_FILE}; exit $rc; }}"
        )
    }

    fn walg_fetch_abort_script_with(restore_temp: &str, pid_file: &str) -> String {
        format!(
            "is_fetch() {{ if [ -r /proc/$1/cmdline ]; then tr '\\000' ' ' < /proc/$1/cmdline | grep -q backup-fetch; \
             else ps -p \"$1\" -o args= 2>/dev/null | grep -q backup-fetch; fi; }}; \
             if [ -f {pid_file} ]; then pid=$(cat {pid_file}); \
             if [ -n \"$pid\" ] && is_fetch \"$pid\"; then kill \"$pid\" 2>/dev/null; \
             i=0; while is_fetch \"$pid\" && [ $i -lt 10 ]; do sleep 1; i=$((i+1)); done; \
             if is_fetch \"$pid\"; then kill -9 \"$pid\" 2>/dev/null; \
             i=0; while is_fetch \"$pid\" && [ $i -lt 5 ]; do sleep 1; i=$((i+1)); done; fi; \
             if is_fetch \"$pid\"; then echo \"wal-g backup-fetch (pid $pid) is still running\" >&2; exit 3; fi; fi; fi; \
             rm -rf {restore_temp} /var/lib/postgresql/walg-restore.env {pid_file}",
            pid_file = pid_file,
            restore_temp = restore_temp,
        )
    }

    /// Stop a WAL-G restore download in `container_name` and confirm it has
    /// exited, then remove what it staged. `fetch_exec` is the Docker exec
    /// that started the fetch, when known: it must also report not running.
    ///
    /// `Err` means the fetch may still be writing; callers must not release
    /// the service. A container that does not exist or is not running has no
    /// fetch left, which counts as stopped.
    pub(super) async fn stop_walg_fetch(
        &self,
        container_name: &str,
        restore_temp: &str,
        fetch_exec: Option<&str>,
    ) -> std::result::Result<(), String> {
        use bollard::errors::Error as DockerError;
        use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};

        info!(
            "Stopping WAL-G fetch and removing '{}' in container '{}'",
            restore_temp, container_name
        );
        let gone = |e: &DockerError| {
            matches!(
                e,
                DockerError::DockerResponseServerError {
                    status_code: 404 | 409,
                    ..
                }
            )
        };
        let script = Self::walg_fetch_abort_script(restore_temp);
        let exec = match self
            .docker
            .create_exec(
                container_name,
                CreateExecOptions {
                    cmd: Some(vec!["sh", "-c", script.as_str()]),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    user: Some("postgres"),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(exec) => exec,
            // No container, or a stopped one: no process can be running in it.
            Err(e) if gone(&e) => return Ok(()),
            Err(e) => {
                return Err(format!(
                    "could not run the stop script in container '{}': {}",
                    container_name, e
                ))
            }
        };
        match self
            .docker
            .start_exec(&exec.id, None::<StartExecOptions>)
            .await
        {
            Ok(StartExecResults::Attached { mut output, .. }) => {
                // Drain output so the script runs to completion first.
                while output.next().await.is_some() {}
            }
            Ok(StartExecResults::Detached) => {}
            Err(e) if gone(&e) => return Ok(()),
            Err(e) => {
                return Err(format!(
                    "could not run the stop script in container '{}': {}",
                    container_name, e
                ))
            }
        }
        let exit_code = self
            .docker
            .inspect_exec(&exec.id)
            .await
            .map_err(|e| {
                format!(
                    "could not read the stop script's result in container '{}': {}",
                    container_name, e
                )
            })?
            .exit_code;
        if exit_code != Some(0) {
            return Err(format!(
                "the stop script in container '{}' exited with {:?}: the wal-g fetch is still running",
                container_name, exit_code
            ));
        }
        // The process is gone; the exec that launched it must agree.
        if let Some(fetch_exec) = fetch_exec {
            for _ in 0..50 {
                match self.docker.inspect_exec(fetch_exec).await {
                    Ok(inspect) if inspect.running == Some(true) => {
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    }
                    Ok(_) => return Ok(()),
                    Err(e) if gone(&e) => return Ok(()),
                    Err(e) => {
                        return Err(format!(
                            "could not confirm the wal-g fetch exec in container '{}' stopped: {}",
                            container_name, e
                        ))
                    }
                }
            }
            return Err(format!(
                "the wal-g fetch exec in container '{}' still reports running 10s after it was killed",
                container_name
            ));
        }
        Ok(())
    }

    async fn wait_for_restored_database(
        &self,
        config: &PostgresConfig,
    ) -> std::result::Result<(), PostgresArchiveVerificationError> {
        use sqlx::Connection;

        let connection = format!(
            "postgres://{}:{}@{}:{}/{}?sslmode={}",
            urlencoding::encode(&config.username),
            urlencoding::encode(&config.password),
            config.host,
            config.port,
            urlencoding::encode(&config.database),
            urlencoding::encode(config.ssl_mode.as_deref().unwrap_or("disable")),
        );
        let result = tokio::time::timeout(
            Duration::from_secs(90),
            wait_for_restored_postgres(
                || async {
                    tokio::time::timeout(Duration::from_secs(5), async {
                        let mut database = sqlx::PgConnection::connect(&connection).await?;
                        let ready = sqlx::query_scalar::<_, bool>("SELECT NOT pg_is_in_recovery()")
                            .fetch_one(&mut database)
                            .await;
                        let _ = database.close().await;
                        ready
                    })
                    .await
                    .map_err(|_| {
                        sqlx::Error::Io(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "restored PostgreSQL readiness probe timed out",
                        ))
                    })?
                },
                Duration::from_millis(500),
            ),
        )
        .await;
        result
            .map_err(|_| PostgresArchiveVerificationError {
                service_name: self.name.clone(),
                reason:
                    "the restored database did not finish starting and recovering within 90 seconds"
                        .to_string(),
            })?
            .map_err(|error| PostgresArchiveVerificationError {
                service_name: self.name.clone(),
                reason: format!("could not verify the restored database's readiness: {error}"),
            })
    }

    async fn restore_backup_file(
        &self,
        docker: &Docker,
        container_name: &str,
        backup_path: &std::path::Path,
        username: &str,
        password: &str,
    ) -> Result<()> {
        // Stream the staged dump into the container: constant memory, and no
        // host path has to be shared with the Docker daemon.
        crate::externalsvc::container_upload::upload_file_to_container(
            docker,
            container_name,
            backup_path,
            "/",
            "backup.sql",
            0o644,
        )
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to upload backup file to container {}: {}",
                container_name,
                e
            )
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
        backup_path: &std::path::Path,
        username: &str,
        password: &str,
    ) -> Result<()> {
        crate::externalsvc::container_upload::upload_file_to_container(
            docker,
            container_name,
            backup_path,
            "/",
            "backup.pgdump",
            0o644,
        )
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to upload backup file to container {}: {}",
                container_name,
                e
            )
        })?;

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

    /// Restore from a WAL-G backup stored in S3.
    ///
    /// WAL-G restore requires stopping PostgreSQL, clearing PGDATA, fetching the backup,
    /// and restarting. This is done via `docker exec` commands.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn restore_from_walg(
        &self,
        s3_credentials: &crate::externalsvc::S3Credentials,
        walg_s3_prefix: &str,
        service_config: ServiceConfig,
        live_target_config: Option<&ServiceConfig>,
        recovery_target: Option<&crate::externalsvc::RecoveryTarget>,
        target_user_data: Option<&str>,
        gate: &dyn crate::externalsvc::RestoreGate,
    ) -> Result<()> {
        use bollard::exec::CreateExecOptions;

        let postgres_config = self.get_postgres_config(service_config.clone())?;
        let container_name = self.get_live_container_name(&postgres_config);
        if postgres_config.container_name.is_some() {
            let container = self
                .docker
                .inspect_container(
                    &container_name,
                    None::<bollard::query_parameters::InspectContainerOptions>,
                )
                .await
                .map_err(|error| PostgresArchiveVerificationError {
                    service_name: self.name.clone(),
                    reason: format!(
                        "could not verify the imported container's startup policy: {error}"
                    ),
                })?;
            let startup = container
                .config
                .as_ref()
                .map(|config| {
                    config
                        .entrypoint
                        .iter()
                        .flatten()
                        .chain(config.cmd.iter().flatten())
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            // The data swap inherits the backup origin's login, but this
            // running imported target still authenticates with its own login.
            let live_config = live_target_config
                .map(|config| self.get_postgres_config(config.clone()))
                .transpose()?
                .unwrap_or_else(|| postgres_config.clone());
            let command_line_archiving = self.imported_archive_mode_source(&live_config).await?;
            if postgres_cmd_pins_archive_mode_on(&startup) || command_line_archiving {
                return Err(PostgresArchiveVerificationError {
                    service_name: self.name.clone(),
                    reason: "the imported container's startup command pins archive_mode on. Change its startup setting to off, or restore into a new managed service before retrying; the original database has not been changed".to_string(),
                }.into());
            }
        }

        // The restore ends by recreating a managed container from the
        // service's specification. Refuse up front, before fetching or
        // touching anything, when that container would not mount the storage
        // the current one uses: the restored data would land where the new
        // container never looks (#1415).
        if postgres_config.container_name.is_none() {
            let replacement = self.replacement_storage(&postgres_config)?;
            crate::externalsvc::recreate_guard::preflight(
                &self.docker,
                crate::externalsvc::recreate_guard::ManagedEngine::Postgres,
                &self.name,
                &container_name,
                &replacement,
                "restoring a backup in place",
            )
            .await?;
        }

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
        // Absent unless this source holds a temporary (STS-style)
        // credential, so a long-lived one produces the exact environment
        // it always did.
        walg_env.extend(s3_credentials.session_token_env());

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
        if let Some(target_user_data) = target_user_data {
            walg_env.push(format!("WALG_FETCH_TARGET_USER_DATA={target_user_data}"));
        }

        use bollard::exec::StartExecOptions;
        let walg_env_refs: Vec<&str> = walg_env.iter().map(|s| s.as_str()).collect();

        // Step 1: Fetch backup to a temporary directory while PostgreSQL is still running.
        // We cannot stop PostgreSQL first because it's the container's main process — stopping it
        // would stop the container entirely or leave shared memory blocks behind.
        //
        // IMPORTANT: The temp directory MUST be on the shared volume (/var/lib/postgresql),
        // NOT in /tmp (which is in the container's writable layer). The helper container
        // in step 4 uses `volumes_from` to share the named volume, and it can only see
        // paths on that volume — not the original container's writable layer.
        info!("Fetching WAL-G backup to temporary directory on shared volume");
        let restore_temp = WALG_RESTORE_TEMP;
        let fetch_target = if target_user_data.is_some() {
            "--target-user-data \"$WALG_FETCH_TARGET_USER_DATA\""
        } else {
            "LATEST"
        };
        let fetch_cmd_str = Self::walg_fetch_command(restore_temp, fetch_target);
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

        // Poll for fetch completion. Nothing outside `restore_temp` has been
        // written yet, so a cancellation here stops the fetch and removes it.
        loop {
            if gate.is_cancelled() {
                if let Err(reason) = self
                    .stop_walg_fetch(&container_name, restore_temp, Some(&exec.id))
                    .await
                {
                    return Err(crate::externalsvc::StagedRestoreStillRunning {
                        target: container_name.clone(),
                        reason,
                    }
                    .into());
                }
                return Err(crate::externalsvc::RestoreCancelled {
                    target: container_name.clone(),
                }
                .into());
            }
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

        let recovery_target_line = postgres_recovery_target_setting(recovery_target);

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

        // Last safe point. Everything so far lives in `restore_temp`; the
        // next step stops the live server. A cancellation recorded before
        // this call wins and the target is left exactly as it was.
        if let Err(cancelled) = gate.begin_target_writes().await {
            // The fetch already exited; only its staged files remain, and the
            // next restore clears them before fetching, so a failed cleanup
            // is logged rather than holding the service.
            if let Err(reason) = self
                .stop_walg_fetch(&container_name, restore_temp, None)
                .await
            {
                warn!(
                    "Cancelled restore of '{}' could not remove its staged files: {}",
                    container_name, reason
                );
            }
            return Err(cancelled.into());
        }

        // Step 3: Stop the container. This cleanly shuts down PostgreSQL (the container's main process)
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
            "rm -rf {pgdata}/* && cp -a {restore_temp}/* {pgdata}/ && rm -rf {restore_temp} && rm -f /var/lib/postgresql/walg.env",
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
            labels: Some(crate::externalsvc::restore_helper::restore_helper_labels(
                &container_name,
            )),
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

        // Restored PGDATA deliberately suspends continuous archiving. The
        // committed swap removes only the target's write credential marker;
        // recovery still uses its separate read-only source credentials.
        // Reconcile the managed container CMD now so archive_mode=off survives
        // restarts. A later full backup explicitly restores the pinned write
        // destination and verifies active archiving before it can succeed.
        if postgres_config.container_name.is_some() {
            // Imported containers keep their external lifecycle; start() will
            // not recreate them, so restore the policy disabled for the swap.
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
                .map_err(|error| PostgresArchiveVerificationError {
                    service_name: self.name.clone(),
                    reason: format!(
                        "could not restore the imported container's restart policy: {error}"
                    ),
                })?;
        }
        if postgres_config.container_name.is_some() {
            self.force_recreate(service_config).await?;
        } else {
            // Restore orchestration does not persist a replacement host port.
            // Recreate at the registered endpoint, retaining PGDATA if that
            // endpoint cannot bind, instead of silently changing its address.
            let limits = ServiceResourceLimits::from_parameters(&service_config.parameters);
            limits
                .validate()
                .map_err(|reason| PostgresArchiveVerificationError {
                    service_name: self.name.clone(),
                    reason: format!("invalid restored service resource limits: {reason}"),
                })?;
            self.docker
                .remove_container(
                    &container_name,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await
                .map_err(|error| PostgresArchiveVerificationError {
                    service_name: self.name.clone(),
                    reason: format!("could not recreate the restored managed container: {error}"),
                })?;
            self.create_container_once(&self.docker, &postgres_config, &limits, false)
                .await?;
        }
        // Docker's running state alone is not SQL readiness for an imported
        // container without a healthcheck. Authenticate with the restored login
        // and wait for recovery before checking policy or reporting success.
        self.wait_for_restored_database(&postgres_config).await?;
        let (mode, _) = self
            .active_archive_settings(&postgres_config, "/bin/true")
            .await?;
        if mode != "off" {
            return Err(PostgresArchiveVerificationError {
                service_name: self.name.clone(),
                reason: "the restored database did not suspend WAL archiving. Check the container's startup command and PostgreSQL configuration before retrying; restore into a new managed service if the imported container's policy cannot be changed safely".to_string(),
            }.into());
        }

        info!("PostgreSQL WAL-G restore completed successfully");
        Ok(())
    }

    /// Restore from a legacy backup (pre-WAL-G .sql.gz or .pgdump.gz files in S3).
    /// Falls back to the old approach: download from S3, decompress, psql/pg_restore.
    pub(super) async fn restore_from_legacy(
        &self,
        s3_client: &aws_sdk_s3::Client,
        backup_location: &str,
        s3_source: &temps_entities::s3_sources::Model,
        service_config: ServiceConfig,
        gate: &dyn crate::externalsvc::RestoreGate,
    ) -> Result<()> {
        info!("Restoring from legacy backup format: {}", backup_location);

        // Ensure container is running before attempting restore
        self.start().await?;

        let postgres_config = self.get_postgres_config(service_config)?;

        // Stage the backup on disk, file to file, so memory stays constant
        // however large the dump is. The temp dir is owned by this attempt
        // and removed when it drops.
        let staging = tempfile::tempdir().map_err(|e| {
            anyhow::anyhow!(
                "Failed to create a temp dir to stage PostgreSQL backup {}: {}",
                backup_location,
                e
            )
        })?;
        let compressed_path = staging.path().join("backup.gz");
        let dump_path = staging.path().join("backup.dump");
        crate::externalsvc::restore_staging::download_s3_object_to_file(
            s3_client,
            &s3_source.bucket_name,
            backup_location,
            &compressed_path,
            gate,
        )
        .await?;
        crate::externalsvc::restore_staging::gunzip_file(
            &compressed_path,
            &dump_path,
            &format!("PostgreSQL backup {}", backup_location),
        )
        .await?;
        let _ = tokio::fs::remove_file(&compressed_path).await;

        // Last safe point: the dump is staged on the host and nothing has
        // been loaded yet. The staging dir is removed when it drops.
        gate.begin_target_writes().await?;

        let container_name = self.get_live_container_name(&postgres_config);

        // Detect backup format from the S3 location
        let is_plain_format = backup_location.ends_with(".sql.gz");

        if is_plain_format {
            self.restore_backup_file(
                &self.docker,
                &container_name,
                &dump_path,
                &postgres_config.username,
                &postgres_config.password,
            )
            .await?;
        } else {
            self.restore_custom_backup_file(
                &self.docker,
                &container_name,
                &dump_path,
                &postgres_config.username,
                &postgres_config.password,
            )
            .await?;
        }

        info!("Legacy PostgreSQL restore completed successfully");
        Ok(())
    }
}

pub(super) fn postgres_recovery_target_setting(
    recovery_target: Option<&crate::externalsvc::RecoveryTarget>,
) -> String {
    match recovery_target {
        None => "recovery_target = 'immediate'".to_string(),
        Some(crate::externalsvc::RecoveryTarget::Time { time }) => format!(
            // PostgreSQL's recovery_target_time GUC is parsed by a stricter
            // datetime parser than SQL's ::timestamptz cast: it rejects the
            // ISO 8601 'T' separator / 'Z' suffix (`invalid value for
            // parameter "recovery_target_time"`), even though the same
            // string casts fine in a query. Use the space-separated,
            // explicit-offset form Postgres's own output uses, with
            // microsecond precision preserved.
            "recovery_target_time = '{}'",
            time.format("%Y-%m-%d %H:%M:%S%.6f%:z")
        ),
        Some(crate::externalsvc::RecoveryTarget::Xid { xid }) => {
            format!("recovery_target_xid = '{}'", xid.replace('\'', ""))
        }
        Some(crate::externalsvc::RecoveryTarget::Lsn { lsn }) => {
            format!("recovery_target_lsn = '{}'", lsn.replace('\'', ""))
        }
        Some(crate::externalsvc::RecoveryTarget::Name { name }) => {
            format!("recovery_target_name = '{}'", name.replace('\'', ""))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "docker-tests")]
    use bollard::Docker;
    #[cfg(feature = "docker-tests")]
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn restored_postgres_readiness_retries_transport_and_recovery() {
        let mut answers = std::collections::VecDeque::from([
            Err(sqlx::Error::Io(std::io::Error::from(
                std::io::ErrorKind::UnexpectedEof,
            ))),
            Ok(false),
            Ok(true),
        ]);
        wait_for_restored_postgres(
            || std::future::ready(answers.pop_front().unwrap()),
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(answers.is_empty());
    }

    #[tokio::test]
    async fn restored_postgres_readiness_rejects_permanent_errors_without_retry() {
        let mut attempts = 0;
        let error = wait_for_restored_postgres(
            || {
                attempts += 1;
                std::future::ready(Err(sqlx::Error::Configuration(
                    "invalid database configuration".into(),
                )))
            },
            Duration::ZERO,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, sqlx::Error::Configuration(_)));
        assert_eq!(attempts, 1);
    }

    #[tokio::test]
    async fn restored_postgres_readiness_stops_at_its_deadline() {
        let result = tokio::time::timeout(
            Duration::from_millis(20),
            wait_for_restored_postgres(|| std::future::ready(Ok(false)), Duration::from_millis(1)),
        )
        .await;
        assert!(result.is_err());
    }

    #[test]
    fn walg_fetch_abort_script_kills_the_recorded_fetch_and_removes_staged_data() {
        let script = PostgresService::walg_fetch_abort_script("/var/lib/postgresql/restore_temp");

        assert!(script.contains(&format!("pid=$(cat {})", WALG_FETCH_PID_FILE)));
        assert!(script.contains("kill \"$pid\""));
        assert!(
            script.contains(
                "rm -rf /var/lib/postgresql/restore_temp /var/lib/postgresql/walg-restore.env"
            ),
            "{script}"
        );
        // It must never touch the live data directory.
        assert!(
            !script.contains("PGDATA") && !script.contains("/data"),
            "{script}"
        );
    }

    /// Runs the real stop script against a local stand-in fetch, started the
    /// way the restore starts WAL-G (`cmd & echo $! > pidfile; wait $!`, so
    /// the wrapping shell reaps it): the fetch must be confirmed gone (exit
    /// 0) and only then the staged data removed.
    #[cfg(unix)]
    fn run_abort_script(fetch_cmd: &str, tag: &str) -> (std::process::ExitStatus, bool) {
        let dir = std::env::temp_dir().join(format!("walg-abort-{}-{}", tag, std::process::id()));
        let staged = dir.join("restore_temp");
        std::fs::create_dir_all(&staged).expect("create staged dir");
        let pid_file = dir.join("walg_fetch.pid");
        let mut wrapper = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                "{} & echo $! > {}; wait $!",
                fetch_cmd,
                pid_file.display()
            ))
            .spawn()
            .expect("spawn stand-in fetch");
        for _ in 0..100 {
            if std::fs::read_to_string(&pid_file).is_ok_and(|p| !p.trim().is_empty()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let script = PostgresService::walg_fetch_abort_script_with(
            &staged.display().to_string(),
            &pid_file.display().to_string(),
        );
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(&script)
            .status()
            .expect("run abort script");
        let _ = wrapper.wait();
        let staged_left = staged.exists();
        let _ = std::fs::remove_dir_all(&dir);
        (status, staged_left)
    }

    /// REGRESSION (Greptile on #1295): a cancelled download must be
    /// confirmed stopped before the run is released.
    #[cfg(unix)]
    #[test]
    fn abort_script_confirms_the_fetch_is_gone_before_deleting_staged_data() {
        let (status, staged_left) = run_abort_script("sh -c 'sleep 60; :' backup-fetch", "term");
        assert!(status.success(), "{status:?}");
        assert!(
            !staged_left,
            "staged data must be removed once the fetch is gone"
        );
    }

    #[cfg(unix)]
    #[test]
    fn abort_script_escalates_to_sigkill_for_a_fetch_that_ignores_sigterm() {
        let (status, staged_left) = run_abort_script(
            "sh -c \"trap '' TERM; while :; do sleep 1; done\" backup-fetch",
            "kill",
        );
        assert!(status.success(), "{status:?}");
        assert!(!staged_left);
    }

    /// REGRESSION (Greptile on #1295): a PID file left behind may name a
    /// process that is no longer the fetch (a reused PID, e.g. a PostgreSQL
    /// backend). It must never be signalled; the stale file is just removed.
    #[cfg(unix)]
    #[test]
    fn abort_script_never_signals_a_reused_pid() {
        let dir = std::env::temp_dir().join(format!("walg-abort-stale-{}", std::process::id()));
        let staged = dir.join("restore_temp");
        std::fs::create_dir_all(&staged).expect("create staged dir");
        let pid_file = dir.join("walg_fetch.pid");
        let mut unrelated = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn unrelated process");
        std::fs::write(&pid_file, unrelated.id().to_string()).expect("write stale pid");
        let script = PostgresService::walg_fetch_abort_script_with(
            &staged.display().to_string(),
            &pid_file.display().to_string(),
        );
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(&script)
            .status()
            .expect("run abort script");
        let still_alive = unrelated
            .try_wait()
            .expect("poll unrelated process")
            .is_none();
        let _ = unrelated.kill();
        let _ = unrelated.wait();
        let staged_left = staged.exists();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(status.success(), "{status:?}");
        assert!(
            still_alive,
            "an unrelated process with the recorded PID was killed"
        );
        assert!(!staged_left);
    }

    /// REGRESSION (Greptile on #1295): a finished fetch must not leave its
    /// PID file behind for a later stop to act on.
    #[test]
    fn walg_fetch_removes_its_pid_file_when_it_exits() {
        let command = PostgresService::walg_fetch_command(WALG_RESTORE_TEMP, "LATEST");
        let wait = command.find("wait $!").expect("waits for the fetch");
        let cleanup = command
            .find(&format!("rm -f {}", WALG_FETCH_PID_FILE))
            .expect("removes the pid file");
        assert!(wait < cleanup, "{command}");
        assert!(
            command.ends_with("exit $rc; }"),
            "keeps the fetch exit code: {command}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn walg_fetch_command_runs_and_cleans_up_its_pid_file() {
        // The real command with `wal-g` replaced by a stand-in that fails, run
        // against a temp dir: the pid file is gone and the exit code kept.
        let dir = std::env::temp_dir().join(format!("walg-fetch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create dir");
        let pid_file = dir.join("walg_fetch.pid");
        let command =
            PostgresService::walg_fetch_command(&dir.join("t").display().to_string(), "LATEST")
                .replace("wal-g backup-fetch", "false")
                .replace(
                    "/tmp/walg_restore.log",
                    &dir.join("log").display().to_string(),
                )
                .replace(WALG_FETCH_PID_FILE, &pid_file.display().to_string());
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .status()
            .expect("run fetch command");
        let pid_file_left = pid_file.exists();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(status.code(), Some(1), "the fetch's exit code is kept");
        assert!(
            !pid_file_left,
            "the pid file must be removed when the fetch exits"
        );
    }

    #[test]
    fn abort_script_refuses_to_delete_while_the_fetch_survives() {
        let script = PostgresService::walg_fetch_abort_script("/var/lib/postgresql/restore_temp");
        assert!(script.contains("kill -9"), "{script}");
        // The deletion only runs after the liveness check that exits 3.
        let survives = script.find("exit 3").expect("exit 3 when still alive");
        let delete = script.find("rm -rf").expect("rm -rf");
        assert!(survives < delete, "{script}");
    }

    #[test]
    fn recovery_target_setting_preserves_fractional_seconds() {
        let target = chrono::DateTime::parse_from_rfc3339("2026-09-02T17:11:48.133085Z")
            .expect("test timestamp must parse")
            .with_timezone(&chrono::Utc);

        assert_eq!(
            postgres_recovery_target_setting(Some(&crate::externalsvc::RecoveryTarget::Time {
                time: target,
            })),
            "recovery_target_time = '2026-09-02 17:11:48.133085+00:00'"
        );
    }

    #[test]
    fn recovery_target_setting_keeps_exact_whole_second_boundary() {
        let target = chrono::DateTime::parse_from_rfc3339("2026-09-02T17:11:48Z")
            .expect("test timestamp must parse")
            .with_timezone(&chrono::Utc);

        assert_eq!(
            postgres_recovery_target_setting(Some(&crate::externalsvc::RecoveryTarget::Time {
                time: target,
            })),
            "recovery_target_time = '2026-09-02 17:11:48.000000+00:00'"
        );
    }

    #[test]
    fn recovery_target_setting_preserves_microsecond_boundaries() {
        for (input, expected) in [
            (
                "2026-09-02T17:11:48.000001Z",
                "2026-09-02 17:11:48.000001+00:00",
            ),
            (
                "2026-09-02T17:11:48.999999Z",
                "2026-09-02 17:11:48.999999+00:00",
            ),
        ] {
            let target = chrono::DateTime::parse_from_rfc3339(input)
                .expect("test timestamp must parse")
                .with_timezone(&chrono::Utc);
            assert_eq!(
                postgres_recovery_target_setting(Some(&crate::externalsvc::RecoveryTarget::Time {
                    time: target
                })),
                format!("recovery_target_time = '{expected}'")
            );
        }
    }

    #[test]
    fn recovery_target_setting_never_emits_iso8601_t_or_z() {
        // Regression test: PostgreSQL's recovery_target_time GUC rejects the
        // ISO 8601 'T' date/time separator and 'Z' UTC suffix with
        // `FATAL: configuration file "postgresql.auto.conf" contains errors`
        // / `invalid value for parameter "recovery_target_time"`, even
        // though the identical string parses fine via `::timestamptz` in
        // SQL. Confirmed against a real postgres:18-bookworm container.
        let target = chrono::DateTime::parse_from_rfc3339("2026-09-02T17:11:48.133085Z")
            .expect("test timestamp must parse")
            .with_timezone(&chrono::Utc);
        let setting =
            postgres_recovery_target_setting(Some(&crate::externalsvc::RecoveryTarget::Time {
                time: target,
            }));
        assert!(
            !setting.contains('T'),
            "must not use ISO 8601 'T' separator: {setting}"
        );
        assert!(
            !setting.contains('Z'),
            "must not use ISO 8601 'Z' UTC suffix: {setting}"
        );
    }

    // -----------------------------------------------------------------
    // shared_preload_libraries_for_image
    // -----------------------------------------------------------------
    //
    // Regression coverage for the bug where this value was unconditionally
    // overwritten to just "pg_stat_statements", silently dropping any
    // image-required library (e.g. TimescaleDB requires "timescaledb" to be
    // preloaded for hypertables' background workers, continuous aggregates,
    // and compression policies to function at all).

    /// pg_dump restores (the in-place path for every non-WAL-G Postgres
    /// backup) stream the staged dump into the container: constant memory,
    /// and the staging file lives in the OS default temp dir, which a Docker
    /// daemon in a VM (Colima, Docker Desktop) cannot see.
    #[tokio::test]
    #[cfg(feature = "docker-tests")]
    async fn pg_dump_restores_stream_staged_files_into_the_container() {
        use crate::externalsvc::exec_util::run_exec;
        use futures::StreamExt;
        use std::io::Read;

        let Ok(docker) = Docker::connect_with_local_defaults() else {
            println!("Docker not available, skipping");
            return;
        };
        if docker.ping().await.is_err() {
            println!("Docker not available, skipping");
            return;
        }
        let image = "postgres:17-alpine";
        if crate::utils::pull_image_with_retry(&docker, image, None)
            .await
            .is_err()
        {
            println!("Could not pull {image}, skipping");
            return;
        }
        let suffix = &uuid::Uuid::new_v4().simple().to_string()[..10];
        let container = format!("temps-pgdump-restore-test-{suffix}");
        let password = "restore-test-pw";
        if docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&container)
                        .build(),
                ),
                bollard::models::ContainerCreateBody {
                    image: Some(image.to_string()),
                    env: Some(vec![format!("POSTGRES_PASSWORD={password}")]),
                    ..Default::default()
                },
            )
            .await
            .is_err()
        {
            println!("Could not create the test Postgres, skipping");
            return;
        }
        let _ = docker
            .start_container(
                &container,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await;

        let timeout = Duration::from_secs(60);
        let psql = |sql: &str| {
            vec![
                "psql".to_string(),
                "-U".to_string(),
                "postgres".to_string(),
                "-tAc".to_string(),
                sql.to_string(),
            ]
        };
        let outcome: anyhow::Result<(String, String)> = async {
            let mut ready = false;
            for _ in 0..60 {
                if let Ok(r) = run_exec(&docker, &container, psql("SELECT 1"), None, timeout).await {
                    if r.output.trim().ends_with('1') {
                        ready = true;
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            anyhow::ensure!(ready, "test Postgres never became ready");
            let service = PostgresService::new(format!("pgdump-test-{suffix}"), Arc::new(docker.clone()));

            // Plain SQL dump, staged in the default temp dir.
            let staging = tempfile::tempdir()?;
            let sql_path = staging.path().join("backup.dump");
            std::fs::write(
                &sql_path,
                "CREATE TABLE plain_items (n int);\nINSERT INTO plain_items VALUES (1), (2), (3);\n",
            )?;
            service
                .restore_backup_file(&docker, &container, &sql_path, "postgres", password)
                .await?;
            let plain = run_exec(&docker, &container, psql("SELECT count(*) FROM plain_items"), None, timeout)
                .await?
                .output;

            // Custom-format dump: produce one, take it out, drop the table,
            // then restore it from a host file.
            run_exec(&docker, &container, psql("CREATE TABLE custom_items AS SELECT generate_series(1, 5) AS n"), None, timeout).await?;
            run_exec(
                &docker,
                &container,
                vec!["pg_dump".into(), "-U".into(), "postgres".into(), "-Fc".into(), "-t".into(), "custom_items".into(), "-f".into(), "/tmp/custom.pgdump".into()],
                None,
                timeout,
            )
            .await?;
            let mut tar_bytes = Vec::new();
            let mut stream = docker.download_from_container(
                &container,
                Some(
                    bollard::query_parameters::DownloadFromContainerOptionsBuilder::new()
                        .path("/tmp/custom.pgdump")
                        .build(),
                ),
            );
            while let Some(chunk) = stream.next().await {
                tar_bytes.extend_from_slice(&chunk?);
            }
            let mut archive = tar::Archive::new(tar_bytes.as_slice());
            let mut entry = archive
                .entries()?
                .next()
                .ok_or_else(|| anyhow::anyhow!("empty pg_dump download"))??;
            let mut dump = Vec::new();
            entry.read_to_end(&mut dump)?;
            let custom_path = staging.path().join("custom.pgdump");
            std::fs::write(&custom_path, &dump)?;
            run_exec(&docker, &container, psql("DROP TABLE custom_items"), None, timeout).await?;
            service
                .restore_custom_backup_file(&docker, &container, &custom_path, "postgres", password)
                .await?;
            let custom = run_exec(&docker, &container, psql("SELECT count(*) FROM custom_items"), None, timeout)
                .await?
                .output;
            Ok((plain, custom))
        }
        .await;

        let _ = docker
            .remove_container(
                &container,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    v: true,
                    ..Default::default()
                }),
            )
            .await;

        let (plain, custom) = outcome.expect("pg_dump restore round trip");
        assert_eq!(plain.trim(), "3", "plain-format restore");
        assert_eq!(custom.trim(), "5", "custom-format restore");
    }

    // ── #1415: replacing the container never loses the data ─────────────
    //
    // Each test builds a container named exactly like a managed service's
    // (`postgres-<name>`) in a shape some release, import or operator could
    // have produced, seeds it, and drives the real provider path. The
    // outcome is either a refusal with the container and data untouched, or a
    // recreate that keeps the same cluster.
}
