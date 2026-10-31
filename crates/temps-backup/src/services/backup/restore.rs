// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Restoring control-plane backups (SQLite, pg_dump and WAL-G).

use super::*;

impl BackupService {
    pub async fn restore_backup(&self, backup_id: &str) -> Result<(), BackupError> {
        use sea_orm::{ConnectionTrait, DatabaseBackend};

        info!(
            "Starting backup restoration process for backup: {}",
            backup_id
        );

        // Lookup backup record
        let backup = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::BackupId.eq(backup_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "Backup".to_string(),
                detail: "Backup not found".to_string(),
            })?;

        // Get S3 source
        let s3_source = temps_entities::s3_sources::Entity::find_by_id(backup.s3_source_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: "S3 source not found".to_string(),
            })?;

        let backend = self.db.get_database_backend();
        match backend {
            DatabaseBackend::Sqlite => self.restore_sqlite_backup(&backup, &s3_source).await,
            DatabaseBackend::Postgres => self.restore_postgres_backup(&backup, &s3_source).await,
            _ => Err(BackupError::Unsupported(
                "Database restore is currently supported only for SQLite and PostgreSQL"
                    .to_string(),
            )),
        }
    }

    async fn restore_sqlite_backup(
        &self,
        backup: &temps_entities::backups::Model,
        s3_source: &temps_entities::s3_sources::Model,
    ) -> Result<(), BackupError> {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        info!("Restoring SQLite backup: {}", backup.backup_id);

        // Create S3 client
        let s3_client = self
            .create_s3_client(s3_source)
            .await
            .map_err(|e| BackupError::S3(e.to_string()))?;

        // Download backup
        let response = s3_client
            .get_object()
            .bucket(&s3_source.bucket_name)
            .key(&backup.s3_location)
            .send()
            .await
            .map_err(|e| BackupError::S3(e.to_string()))?;

        // Stream S3 response → gzip decoder → temp file on disk.
        // Previous approach downloaded the entire compressed backup into memory and then
        // decompressed into a second in-memory buffer, causing peak memory equal to
        // compressed + decompressed size.
        let temp_file = NamedTempFile::new()?;
        {
            let mut body_stream = response.body;
            let mut decoder =
                flate2::write::GzDecoder::new(std::io::BufWriter::new(temp_file.as_file()));
            while let Some(chunk) = body_stream.next().await {
                let chunk = chunk.map_err(|e| BackupError::S3(e.to_string()))?;
                std::io::Write::write_all(&mut decoder, &chunk)?;
            }
            decoder.finish()?;
        }

        // Determine the SQLite database file path from server configuration
        let database_url = &self.config_service.get_database_url();

        // Accept sqlite://path or sqlite:path and derive the OS path
        let db_path = if let Some(rem) = database_url.strip_prefix("sqlite://") {
            rem.to_string()
        } else if let Some(rem) = database_url.strip_prefix("sqlite:") {
            rem.to_string()
        } else {
            return Err(BackupError::Unsupported(format!(
                "Unsupported database URL for SQLite restore: {}",
                database_url
            )));
        };

        if db_path == ":memory:" {
            return Err(BackupError::Unsupported(
                "Cannot restore into an in-memory SQLite database".into(),
            ));
        }

        // Ensure all WAL contents are checkpointed before file replacement
        // so the on-disk main db is consistent.
        let _ = self
            .db
            .execute(Statement::from_string(
                DatabaseBackend::Sqlite,
                "PRAGMA wal_checkpoint(FULL)".to_string(),
            ))
            .await;

        info!("Replacing SQLite database file at {}", db_path);

        // Make a safety copy of the current DB file if it exists
        let db_path_buf = std::path::PathBuf::from(&db_path);
        if db_path_buf.exists() {
            let mut backup_suffix = 0usize;
            loop {
                let safety_path = db_path_buf.with_extension(format!(
                    "bak{}",
                    if backup_suffix == 0 {
                        String::new()
                    } else {
                        format!(".{}", backup_suffix)
                    }
                ));
                if !safety_path.exists() {
                    let _ = std::fs::copy(&db_path_buf, &safety_path);
                    break;
                }
                backup_suffix += 1;
            }
        }

        // Replace the DB file with the restored one
        // Note: best-effort remove first to avoid cross-device rename issues
        if db_path_buf.exists() {
            let _ = std::fs::remove_file(&db_path_buf);
        }
        std::fs::copy(temp_file.path(), &db_path_buf).map_err(BackupError::Io)?;

        // Optionally run integrity check (best-effort)
        let _ = self
            .db
            .execute(Statement::from_string(
                DatabaseBackend::Sqlite,
                "PRAGMA integrity_check".to_string(),
            ))
            .await;

        info!("SQLite backup restored successfully");
        Ok(())
    }

    async fn restore_postgres_backup(
        &self,
        backup: &temps_entities::backups::Model,
        s3_source: &temps_entities::s3_sources::Model,
    ) -> Result<(), BackupError> {
        // Route to WAL-G restore if the backup was created with WAL-G (s3:// prefix)
        if backup.s3_location.starts_with("s3://") {
            return self.restore_postgres_walg(backup, s3_source).await;
        }

        // Legacy restore path: pg_dump SQL via psql/pg_restore sidecar
        use bollard::exec::CreateExecOptions;
        use bollard::models::ContainerCreateBody as Config;
        use bollard::query_parameters::RemoveContainerOptions;
        use bollard::Docker;

        info!("Restoring PostgreSQL backup: {}", backup.backup_id);

        // Create S3 client
        let s3_client = self
            .create_s3_client(s3_source)
            .await
            .map_err(|e| BackupError::S3(e.to_string()))?;

        // Download backup (gzipped SQL)
        let response = s3_client
            .get_object()
            .bucket(&s3_source.bucket_name)
            .key(&backup.s3_location)
            .send()
            .await
            .map_err(|e| BackupError::S3(e.to_string()))?;

        // Get database URL from server configuration
        let database_url = &self.config_service.get_database_url();

        // Parse database URL to extract connection parameters
        let url = url::Url::parse(database_url).map_err(|e| BackupError::Internal {
            message: format!("Invalid DATABASE_URL format: {}", e),
        })?;

        let host = url.host_str().unwrap_or("localhost");
        let port = url.port().unwrap_or(5432);
        let database = url.path().trim_start_matches('/');
        let username = url.username();
        let password = url.password().unwrap_or("");

        // Detect backup format from S3 location path:
        // - .pgdump.gz / backup.postgresql.gz = custom format (pg_restore) [legacy backups]
        // - .sql.gz = plain SQL format (psql) [current format]
        let is_plain_format = backup.s3_location.ends_with(".sql.gz");

        // Connect to Docker — restore uses a sidecar container to ensure
        // psql/pg_restore version matches the database, avoiding host dependency
        let docker = Docker::connect_with_local_defaults().map_err(|e| BackupError::Internal {
            message: format!("Failed to connect to Docker: {}", e),
        })?;

        // Get PostgreSQL version to match the sidecar image
        let version_str = self
            .get_postgres_version()
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to get PostgreSQL version: {}", e),
            })?;
        let major_version =
            self.parse_postgres_version(&version_str)
                .map_err(|e| BackupError::Internal {
                    message: format!("Failed to parse PostgreSQL version: {}", e),
                })?;
        let image_tag = self.get_postgres_image_tag(&major_version);

        // Pull the matching PostgreSQL Docker image
        self.pull_postgres_image(&image_tag)
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to pull Docker image: {}", e),
            })?;

        // Stream S3 response → gzip decoder → temp file on disk.
        // Previous approach downloaded the entire compressed backup into memory and then
        // decompressed into a second in-memory buffer, causing peak memory equal to
        // compressed + decompressed size (e.g. 10 GB + 28 GB = 38 GB).
        let restore_dir = self
            .config_service
            .data_dir()
            .join("backups")
            .join("restore_tmp");
        tokio::fs::create_dir_all(&restore_dir)
            .await
            .map_err(|e| BackupError::Internal {
                message: format!(
                    "Failed to create restore temp directory {}: {}",
                    restore_dir.display(),
                    e
                ),
            })?;

        let restore_filename = format!("{}.sql", uuid::Uuid::new_v4());
        let host_restore_path = restore_dir.join(&restore_filename);
        let container_restore_path = format!("/restore/{}", restore_filename);

        // Stream-decompress S3 body directly to disk — constant memory usage
        {
            let mut body_stream = response.body;
            let out_file =
                std::fs::File::create(&host_restore_path).map_err(|e| BackupError::Internal {
                    message: format!(
                        "Failed to create restore file {}: {}",
                        host_restore_path.display(),
                        e
                    ),
                })?;
            let mut decoder = flate2::write::GzDecoder::new(std::io::BufWriter::new(out_file));
            while let Some(chunk) = body_stream.next().await {
                let chunk = chunk.map_err(|e| BackupError::S3(e.to_string()))?;
                std::io::Write::write_all(&mut decoder, &chunk)?;
            }
            decoder.finish()?;
        }

        // Create sidecar container name
        let container_name = format!("temps-pg-restore-{}", uuid::Uuid::new_v4());

        // URL-decode password for env var
        let decoded_password = urlencoding::decode(password)
            .map(|s| s.to_string())
            .unwrap_or_else(|_| password.to_string());
        let pgpassword_env = format!("PGPASSWORD={}", decoded_password);

        let config = Config {
            image: Some(image_tag),
            entrypoint: Some(vec!["/bin/sleep".to_string()]),
            cmd: Some(vec!["3600".to_string()]),
            env: Some(vec![pgpassword_env.clone()]),
            user: Some("root".to_string()),
            host_config: Some(bollard::models::HostConfig {
                network_mode: Some("host".to_string()),
                auto_remove: Some(true),
                binds: Some(vec![format!("{}:/restore:rw", restore_dir.display())]),
                ..Default::default()
            }),
            ..Default::default()
        };

        // Helper to remove the sidecar on any error path
        let remove_sidecar = |docker: bollard::Docker, name: String| async move {
            let _ = docker
                .remove_container(
                    &name,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;
        };

        info!("Creating temporary Docker container for PostgreSQL restore");

        docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&container_name)
                        .build(),
                ),
                config,
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to create restore container: {}", e),
            })?;

        docker
            .start_container(
                &container_name,
                Some(bollard::query_parameters::StartContainerOptionsBuilder::new().build()),
            )
            .await
            .map_err(|e| {
                let docker = docker.clone();
                let name = container_name.clone();
                tokio::spawn(async move { remove_sidecar(docker, name).await });
                BackupError::Internal {
                    message: format!("Failed to start restore container: {}", e),
                }
            })?;

        let port_str = port.to_string();

        // Build the restore command based on backup format
        let (restore_tool, restore_cmd) = if is_plain_format {
            // Plain SQL: use psql to execute the dump.
            // NOTE: We intentionally do NOT use ON_ERROR_STOP=on because pg_dumpall --clean
            // generates "DROP ... ONLY" statements that TimescaleDB rejects for hypertables.
            // These errors are benign — the actual CREATE TABLE and COPY statements succeed.
            let cmd = format!(
                "psql --no-password --host={} --port={} --username={} --dbname={} --file={}",
                shell_escape(host),
                shell_escape(&port_str),
                shell_escape(username),
                shell_escape(database),
                container_restore_path
            );
            ("psql", cmd)
        } else {
            // Custom format: use pg_restore
            let cmd = format!(
                "pg_restore --verbose --clean --if-exists --no-password --host={} --port={} --username={} --dbname={} {}",
                shell_escape(host), shell_escape(&port_str), shell_escape(username), shell_escape(database), container_restore_path
            );
            ("pg_restore", cmd)
        };

        info!(
            "Running {} in Docker sidecar for backup {}",
            restore_tool, backup.backup_id
        );

        // Capture stderr in a file for diagnostics
        let stderr_file_name = format!("{}.stderr", uuid::Uuid::new_v4());
        let stderr_path = format!("/restore/{}", stderr_file_name);
        let full_cmd = format!("{} 2>{}", restore_cmd, stderr_path);

        let exec = docker
            .create_exec(
                &container_name,
                CreateExecOptions {
                    cmd: Some(vec!["sh", "-c", &full_cmd]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    env: Some(vec![pgpassword_env.as_str()]),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to create exec for {}: {}", restore_tool, e),
            })?;

        // Start detached — no streaming through Temps process
        use bollard::exec::StartExecOptions;
        docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to start exec for {}: {}", restore_tool, e),
            })?;

        // Poll for completion
        loop {
            let inspect =
                docker
                    .inspect_exec(&exec.id)
                    .await
                    .map_err(|e| BackupError::Internal {
                        message: format!("Failed to inspect exec: {}", e),
                    })?;
            if let Some(running) = inspect.running {
                if !running {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }

        // Read stderr from bind mount for diagnostics
        let host_stderr_path = restore_dir.join(&stderr_file_name);
        let stderr_data = tokio::fs::read(&host_stderr_path).await.unwrap_or_default();
        let _ = tokio::fs::remove_file(&host_stderr_path).await;

        // Check exit code
        let exec_inspect =
            docker
                .inspect_exec(&exec.id)
                .await
                .map_err(|e| BackupError::Internal {
                    message: format!("Failed to inspect exec result: {}", e),
                })?;

        let exit_code = exec_inspect.exit_code.unwrap_or(-1);

        // Clean up sidecar and restore file
        remove_sidecar(docker.clone(), container_name.clone()).await;
        let _ = tokio::fs::remove_file(&host_restore_path).await;

        let stderr = String::from_utf8_lossy(&stderr_data);

        if exit_code != 0 {
            // For psql, exit code 1 = SQL errors in the script (may include benign
            // TimescaleDB hypertable warnings from --clean). Exit code 2 = connection error.
            // Exit code 3 = script error. For pg_restore, exit code 1 with "errors ignored"
            // is common for --clean on existing schemas.
            if is_plain_format && exit_code == 1 {
                // psql exit 1 = some SQL statements failed. This is expected when
                // pg_dumpall --clean generates "DROP ... ONLY" on TimescaleDB hypertables.
                // Log as warning, not error.
                warn!(
                    "{} completed with warnings (exit code {}): {}",
                    restore_tool, exit_code, stderr
                );
            } else if !is_plain_format && exit_code == 1 && stderr.contains("errors ignored") {
                warn!("{} completed with ignored errors: {}", restore_tool, stderr);
            } else {
                return Err(BackupError::Internal {
                    message: format!(
                        "{} failed with exit code {}: {}",
                        restore_tool, exit_code, stderr
                    ),
                });
            }
        } else if !stderr.is_empty() {
            debug!("{} stderr output: {}", restore_tool, stderr);
        }

        info!("PostgreSQL backup restored successfully via Docker sidecar");
        Ok(())
    }

    /// Restore internal database from a WAL-G backup.
    ///
    /// Multi-step process (same as external service WAL-G restore):
    /// 1. Fetch backup to temp directory on the shared volume (while PG still runs)
    /// 2. Add recovery.signal + recovery config, copy pg_wal
    /// 3. Disable restart policy, stop container
    /// 4. Swap PGDATA via ephemeral helper container (volumes_from)
    /// 5. Re-enable restart policy, start container → PG recovers → promotes
    async fn restore_postgres_walg(
        &self,
        backup: &temps_entities::backups::Model,
        s3_source: &temps_entities::s3_sources::Model,
    ) -> Result<(), BackupError> {
        use bollard::exec::{CreateExecOptions, StartExecOptions};
        use bollard::Docker;

        info!(
            "Restoring internal database from WAL-G backup: {}",
            backup.s3_location
        );

        let (container_id, pgdata) = self.find_internal_db_container().await?;

        let docker = Docker::connect_with_local_defaults().map_err(|e| BackupError::Internal {
            message: format!("Failed to connect to Docker: {}", e),
        })?;

        // Build WAL-G environment variables
        let decrypted_access_key = self
            .encryption_service
            .decrypt_string(&s3_source.access_key_id)
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to decrypt S3 access key: {}", e),
            })?;
        let decrypted_secret_key = self
            .encryption_service
            .decrypt_string(&s3_source.secret_key)
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to decrypt S3 secret key: {}", e),
            })?;

        // `None` unless this source holds a temporary (STS-style) credential.
        let decrypted_session_token = temps_entities::s3_sources::decrypt_session_token(
            self.encryption_service.as_ref(),
            s3_source,
        )
        .map_err(|e| BackupError::Internal {
            message: format!("Failed to decrypt S3 session token: {}", e),
        })?;

        let walg_s3_prefix = &backup.s3_location;
        let mut walg_env: Vec<String> = vec![
            format!("WALG_S3_PREFIX={}", walg_s3_prefix),
            format!("AWS_ACCESS_KEY_ID={}", decrypted_access_key),
            format!("AWS_SECRET_ACCESS_KEY={}", decrypted_secret_key),
            format!("AWS_REGION={}", s3_source.region),
            format!("PGDATA={}", pgdata),
        ];
        // Absent for a long-lived credential, so its environment is unchanged.
        walg_env.extend(temps_providers::externalsvc::aws_session_token_env(
            decrypted_session_token.as_deref(),
        ));

        // Resolve S3 endpoint for use inside the Docker container.
        let s3_creds = temps_providers::S3Credentials {
            access_key_id: decrypted_access_key.clone(),
            secret_key: decrypted_secret_key.clone(),
            session_token: decrypted_session_token.clone(),
            region: s3_source.region.clone(),
            endpoint: s3_source.endpoint.clone(),
            bucket_name: s3_source.bucket_name.clone(),
            bucket_path: s3_source.bucket_path.clone(),
            force_path_style: s3_source.force_path_style.unwrap_or(true),
        };
        if let Some(resolved_endpoint) = s3_creds
            .resolve_endpoint_for_container(&docker, &container_id)
            .await
        {
            walg_env.push(format!("AWS_ENDPOINT={}", resolved_endpoint));
        }
        if s3_source.force_path_style.unwrap_or(true) {
            walg_env.push("AWS_S3_FORCE_PATH_STYLE=true".to_string());
        }

        let walg_env_refs: Vec<&str> = walg_env.iter().map(|s| s.as_str()).collect();

        // Step 1: Fetch backup to temp directory on the shared volume.
        // Must be on the volume (not /tmp) so the helper container can see it via volumes_from.
        // The parent of PGDATA is typically the volume mount point (e.g., /var/lib/postgresql).
        let volume_root = std::path::Path::new(&pgdata)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "/var/lib/postgresql".to_string());
        let restore_temp = format!("{}/restore_temp", volume_root);
        // Both paths reach `sh -c` below in several commands; escape once and
        // reuse rather than risk an unescaped interpolation creeping back in
        // at one of the call sites. `pgdata` is configured per-service, so an
        // unescaped path here is a shell-injection vector into the container.
        let escaped_pgdata = shell_escape(&pgdata);
        let escaped_restore_temp = shell_escape(&restore_temp);

        info!(
            "Step 1: Fetching WAL-G backup to {} in container {}",
            restore_temp, container_id
        );
        let fetch_cmd_str = format!(
            "mkdir -p {restore_temp} && rm -rf {restore_temp}/* && wal-g backup-fetch {restore_temp} LATEST > /tmp/walg_restore.log 2>&1",
            restore_temp = escaped_restore_temp,
        );

        let exec = docker
            .create_exec(
                &container_id,
                CreateExecOptions {
                    cmd: Some(vec!["sh", "-c", &fetch_cmd_str]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    env: Some(walg_env_refs.clone()),
                    user: Some("postgres"),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to create WAL-G fetch exec: {}", e),
            })?;

        docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to start WAL-G fetch exec: {}", e),
            })?;

        // Poll for fetch completion
        loop {
            let inspect =
                docker
                    .inspect_exec(&exec.id)
                    .await
                    .map_err(|e| BackupError::Internal {
                        message: format!("Failed to inspect WAL-G fetch exec: {}", e),
                    })?;
            if let Some(running) = inspect.running {
                if !running {
                    if let Some(exit_code) = inspect.exit_code {
                        if exit_code != 0 {
                            return Err(BackupError::Internal {
                                message: format!(
                                    "WAL-G backup-fetch failed with exit code {} in container {}",
                                    exit_code, container_id
                                ),
                            });
                        }
                    }
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        info!("WAL-G backup fetched to {}", restore_temp);

        // Step 2: Prepare restored PGDATA for recovery.
        // - recovery.signal: tells PG to enter recovery mode
        // - restore_command = '/bin/true': no archived WAL to fetch
        // - recovery_target = 'immediate': stop at backup consistency point
        // - recovery_target_action = 'promote': promote to primary after recovery
        // - Copy pg_wal from running PGDATA (WAL not archived to S3)
        info!("Step 2: Preparing recovery configuration");
        let prepare_cmd_str = format!(
            concat!(
                "touch {restore_temp}/recovery.signal && ",
                "echo \"restore_command = '/bin/true'\" >> {restore_temp}/postgresql.auto.conf && ",
                "echo \"recovery_target = 'immediate'\" >> {restore_temp}/postgresql.auto.conf && ",
                "echo \"recovery_target_action = 'promote'\" >> {restore_temp}/postgresql.auto.conf && ",
                "rm -rf {restore_temp}/pg_wal && ",
                "cp -a {pgdata}/pg_wal {restore_temp}/pg_wal"
            ),
            restore_temp = escaped_restore_temp,
            pgdata = escaped_pgdata,
        );

        let exec = docker
            .create_exec(
                &container_id,
                CreateExecOptions {
                    cmd: Some(vec!["sh", "-c", &prepare_cmd_str]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    user: Some("postgres"),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to create recovery prep exec: {}", e),
            })?;

        docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to start recovery prep exec: {}", e),
            })?;

        loop {
            let inspect =
                docker
                    .inspect_exec(&exec.id)
                    .await
                    .map_err(|e| BackupError::Internal {
                        message: format!("Failed to inspect recovery prep exec: {}", e),
                    })?;
            if inspect.running == Some(false) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        // Step 3: Disable restart policy and stop container.
        // The container has restart_policy=always, so Docker would immediately restart it.
        info!("Step 3: Disabling restart policy and stopping container for PGDATA swap");
        docker
            .update_container(
                &container_id,
                bollard::models::ContainerUpdateBody {
                    restart_policy: Some(bollard::models::RestartPolicy {
                        name: Some(bollard::models::RestartPolicyNameEnum::NO),
                        maximum_retry_count: None,
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to disable restart policy: {}", e),
            })?;

        docker
            .stop_container(
                &container_id,
                Some(bollard::query_parameters::StopContainerOptions {
                    t: Some(30),
                    signal: None,
                }),
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to stop container for restore: {}", e),
            })?;

        // Step 4: Swap PGDATA via ephemeral helper container.
        // Can't exec into a stopped container, so we create a helper with volumes_from.
        info!("Step 4: Swapping PGDATA via helper container");
        let swap_script = format!(
            "rm -rf {pgdata}/* && cp -a {restore_temp}/* {pgdata}/ && rm -rf {restore_temp}",
            pgdata = escaped_pgdata,
            restore_temp = escaped_restore_temp,
        );

        // Get the image from the container's config to use the same image for the helper
        let container_inspect = docker
            .inspect_container(
                &container_id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to inspect container for helper image: {}", e),
            })?;

        let container_image = container_inspect
            .config
            .as_ref()
            .and_then(|c| c.image.clone())
            .unwrap_or_else(|| "postgres:latest".to_string());

        let helper_name = format!(
            "{}-restore-helper",
            container_id.chars().take(12).collect::<String>()
        );
        let helper_config = bollard::models::ContainerCreateBody {
            image: Some(container_image),
            cmd: Some(vec!["sh".to_string(), "-c".to_string(), swap_script]),
            host_config: Some(bollard::models::HostConfig {
                volumes_from: Some(vec![container_id.clone()]),
                ..Default::default()
            }),
            user: Some("root".to_string()),
            ..Default::default()
        };

        let helper = docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&helper_name)
                        .build(),
                ),
                helper_config,
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to create restore helper container: {}", e),
            })?;

        docker
            .start_container(
                &helper.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to start restore helper container: {}", e),
            })?;

        // Wait for helper to finish
        let wait_result = docker
            .wait_container(
                &helper.id,
                None::<bollard::query_parameters::WaitContainerOptions>,
            )
            .next()
            .await;

        // Capture helper logs before cleanup
        let helper_logs = {
            use futures::TryStreamExt;
            let log_stream = docker.logs(
                &helper.id,
                Some(bollard::query_parameters::LogsOptions {
                    stdout: true,
                    stderr: true,
                    ..Default::default()
                }),
            );
            let logs: Vec<_> = log_stream.try_collect().await.unwrap_or_default();
            logs.iter()
                .map(|l| l.to_string())
                .collect::<Vec<_>>()
                .join("")
        };

        // Clean up helper
        let _ = docker
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
                // Re-enable restart policy even on failure
                let _ = docker
                    .update_container(
                        &container_id,
                        bollard::models::ContainerUpdateBody {
                            restart_policy: Some(bollard::models::RestartPolicy {
                                name: Some(bollard::models::RestartPolicyNameEnum::ALWAYS),
                                maximum_retry_count: None,
                            }),
                            ..Default::default()
                        },
                    )
                    .await;
                let _ = docker
                    .start_container(
                        &container_id,
                        None::<bollard::query_parameters::StartContainerOptions>,
                    )
                    .await;

                return Err(BackupError::Internal {
                    message: format!(
                        "PGDATA swap helper exited with code {}. Logs:\n{}",
                        wait_response.status_code, helper_logs
                    ),
                });
            }
        }

        // Step 5: Re-enable restart policy and start the container.
        // PostgreSQL will enter recovery mode, reach consistency point, and promote.
        info!("Step 5: Re-enabling restart policy and starting container");
        docker
            .update_container(
                &container_id,
                bollard::models::ContainerUpdateBody {
                    restart_policy: Some(bollard::models::RestartPolicy {
                        name: Some(bollard::models::RestartPolicyNameEnum::ALWAYS),
                        maximum_retry_count: None,
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to re-enable restart policy: {}", e),
            })?;

        docker
            .start_container(
                &container_id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to start container after restore: {}", e),
            })?;

        // Wait for PostgreSQL to become healthy by polling the database connection.
        info!("Waiting for PostgreSQL to become ready after restore...");
        let max_wait = std::time::Duration::from_secs(120);
        let start = std::time::Instant::now();
        loop {
            if start.elapsed() > max_wait {
                return Err(BackupError::Internal {
                    message: format!(
                        "PostgreSQL did not become ready within {}s after restore",
                        max_wait.as_secs()
                    ),
                });
            }
            // Try connecting to the database
            let database_url = self.config_service.get_database_url();
            match sea_orm::Database::connect(temps_database::connect_options(&database_url)).await {
                Ok(conn) => {
                    // Try a simple query to verify it's fully operational
                    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
                    match conn
                        .execute(Statement::from_string(
                            DatabaseBackend::Postgres,
                            "SELECT 1".to_string(),
                        ))
                        .await
                    {
                        Ok(_) => {
                            info!("PostgreSQL is ready after WAL-G restore");
                            break;
                        }
                        Err(_) => {
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        }
                    }
                }
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        }

        info!("Internal database WAL-G restore completed successfully");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use temps_core::EncryptionService;

    #[tokio::test]
    async fn test_restore_postgres_from_url() {
        let docker = match bollard::Docker::connect_with_local_defaults() {
            Ok(docker) => docker,
            Err(error) => {
                println!("Docker not available, skipping test: {}", error);
                return;
            }
        };
        if let Err(error) = docker.ping().await {
            println!("Docker daemon not reachable, skipping test: {}", error);
            return;
        }

        use temps_database::test_utils::TestDatabase;
        use testcontainers::runners::AsyncRunner;

        // Start RustFS (S3-compatible) container
        let s3_container = crate::test_rustfs::rustfs_container_request()
            .start()
            .await
            .expect("Failed to start RustFS container");

        let s3_port = s3_container
            .get_host_port_ipv4(crate::test_rustfs::RUSTFS_S3_PORT)
            .await
            .expect("Failed to get RustFS port");

        let s3_endpoint = format!("http://localhost:{}", s3_port);

        crate::test_rustfs::wait_for_rustfs_ready(s3_port)
            .await
            .expect("RustFS did not become healthy");

        // Start source PostgreSQL database with migrations (isolated instance)
        let source_db = TestDatabase::new_isolated()
            .await
            .expect("Failed to create source database");

        // Start target PostgreSQL database with migrations (isolated instance)
        let target_db = TestDatabase::new_isolated()
            .await
            .expect("Failed to create target database");

        // Create S3 client for bucket creation
        let s3_config = aws_sdk_s3::config::Builder::new()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new("us-east-1"))
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                crate::test_rustfs::RUSTFS_ACCESS_KEY,
                crate::test_rustfs::RUSTFS_SECRET_KEY,
                None,
                None,
                "test",
            ))
            .endpoint_url(&s3_endpoint)
            .force_path_style(true)
            .http_client(crate::engines::v2_common::bundled_roots_http_client())
            .build();

        let s3_client = aws_sdk_s3::Client::from_conf(s3_config);

        // Create test bucket
        let bucket_name = "test-restore";
        s3_client
            .create_bucket()
            .bucket(bucket_name)
            .send()
            .await
            .expect("Failed to create bucket");

        // Give bucket time to be ready
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

        // Setup backup service for source database
        let external_service_manager = create_mock_external_service_manager(source_db.db.clone());
        let alarm_service = create_mock_alarm_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let source_config = temps_config::ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            source_db.database_url.clone(),
            None,
            None,
        )
        .unwrap();

        let source_config_service = Arc::new(temps_config::ConfigService::new(
            Arc::new(source_config),
            source_db.db.clone(),
        ));

        let source_backup_service = BackupService::new(
            source_db.db.clone(),
            external_service_manager.clone(),
            alarm_service.clone(),
            source_config_service,
            encryption_service,
            Arc::new(NoopJobQueue),
        );

        // Create a test user in source database
        use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
        use temps_entities::{projects, users};
        let test_user = users::ActiveModel {
            name: Set("Test User".to_string()),
            email: Set("test@example.com".to_string()),
            password_hash: Set(Some("test_hash".to_string())),
            email_verified: Set(true),
            ..Default::default()
        };
        let created_user = test_user
            .insert(source_db.db.as_ref())
            .await
            .expect("Failed to create test user");

        // Create a test project in source database
        use temps_entities::preset::Preset;
        let test_project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            git_url: Set(Some("https://github.com/test/repo".to_string())),
            preset: Set(Preset::Nixpacks),
            ..Default::default()
        };
        let created_project = test_project
            .insert(source_db.db.as_ref())
            .await
            .expect("Failed to create test project");

        println!("\n✓ Test data created in source database:");
        println!("  - User: {} (ID: {})", created_user.name, created_user.id);
        println!(
            "  - Project: {} (ID: {}, Slug: {})",
            created_project.name, created_project.id, created_project.slug
        );

        // Verify data exists in source database
        let user_count_before = users::Entity::find()
            .all(source_db.db.as_ref())
            .await
            .expect("Failed to count users")
            .len();
        let project_count_before = projects::Entity::find()
            .all(source_db.db.as_ref())
            .await
            .expect("Failed to count projects")
            .len();

        assert_eq!(
            user_count_before, 1,
            "Should have 1 user in source database"
        );
        assert_eq!(
            project_count_before, 1,
            "Should have 1 project in source database"
        );

        // Create S3 source
        let s3_source_request = CreateS3SourceRequest {
            name: "test-restore-source".to_string(),
            bucket_name: bucket_name.to_string(),
            bucket_path: "/backups".to_string(),
            access_key_id: SecretString::new(crate::test_rustfs::RUSTFS_ACCESS_KEY),
            secret_key: crate::test_rustfs::RUSTFS_SECRET_KEY.into(),
            region: "us-east-1".to_string(),
            endpoint: Some(s3_endpoint.clone()),
            force_path_style: Some(true),
            is_default: None,
            backing_service_id: None,
        };

        let s3_source = source_backup_service
            .create_s3_source(s3_source_request)
            .await
            .expect("Failed to create S3 source");

        // Perform backup of source database
        let backup_result = source_backup_service
            .create_backup(None, s3_source.id, "full", created_user.id)
            .await
            .expect("Failed to create backup");

        println!("\n✓ Backup created:");
        println!("  - ID: {}", backup_result.id);
        println!("  - Backup ID: {}", backup_result.backup_id);
        println!("  - State: {}", backup_result.state);
        println!("  - S3 Location: {}", backup_result.s3_location);
        println!("  - Size: {} bytes", backup_result.size_bytes.unwrap_or(0));

        // Verify backup file exists in S3
        let object_result = s3_client
            .head_object()
            .bucket(bucket_name)
            .key(&backup_result.s3_location)
            .send()
            .await;
        assert!(
            object_result.is_ok(),
            "Backup file should exist in S3: {:?}",
            object_result.err()
        );

        // Setup backup service for target database (different database URL)
        let target_config = temps_config::ServerConfig::new(
            "127.0.0.1:3001".to_string(),
            target_db.database_url.clone(),
            None,
            None,
        )
        .unwrap();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let target_config_service = Arc::new(temps_config::ConfigService::new(
            Arc::new(target_config),
            target_db.db.clone(),
        ));

        let target_backup_service = BackupService::new(
            target_db.db.clone(),
            external_service_manager,
            alarm_service,
            target_config_service,
            encryption_service,
            Arc::new(NoopJobQueue),
        );

        // Create the S3 source in the target database
        let target_s3_source_request = CreateS3SourceRequest {
            name: "test-restore-source".to_string(),
            bucket_name: bucket_name.to_string(),
            bucket_path: "/backups".to_string(),
            access_key_id: SecretString::new(crate::test_rustfs::RUSTFS_ACCESS_KEY),
            secret_key: crate::test_rustfs::RUSTFS_SECRET_KEY.into(),
            region: "us-east-1".to_string(),
            endpoint: Some(s3_endpoint.clone()),
            force_path_style: Some(true),
            is_default: None,
            backing_service_id: None,
        };

        let target_s3_source = target_backup_service
            .create_s3_source(target_s3_source_request)
            .await
            .expect("Failed to create S3 source in target database");

        // Create a user in the target database to satisfy foreign key constraint.
        // Use an explicit high ID so the dump's COPY (which uses id=1 for the source's
        // first user) doesn't collide with this row when restoring into the target.
        let target_user = users::ActiveModel {
            id: Set(999_999),
            name: Set("Target User".to_string()),
            email: Set("target@example.com".to_string()),
            password_hash: Set(Some("target_hash".to_string())),
            email_verified: Set(true),
            ..Default::default()
        };
        let target_created_user = target_user
            .insert(target_db.db.as_ref())
            .await
            .expect("Failed to create user in target database");

        // Create backup record in target database pointing to the same backup in S3
        use temps_entities::backups;
        let target_backup = backups::ActiveModel {
            id: sea_orm::NotSet,
            name: Set(backup_result.name.clone()),
            backup_id: Set(backup_result.backup_id.clone()),
            schedule_id: Set(None),
            schedule_run_id: sea_orm::NotSet,
            backup_type: Set(backup_result.backup_type.clone()),
            state: Set(backup_result.state.clone()),
            started_at: Set(backup_result.started_at),
            finished_at: Set(backup_result.finished_at),
            s3_source_id: Set(target_s3_source.id),
            s3_location: Set(backup_result.s3_location.clone()),
            compression_type: Set(backup_result.compression_type.clone()),
            created_by: Set(target_created_user.id),
            tags: Set(backup_result.tags.clone()),
            size_bytes: Set(backup_result.size_bytes),
            file_count: Set(backup_result.file_count),
            error_message: Set(backup_result.error_message.clone()),
            expires_at: Set(backup_result.expires_at),
            checksum: Set(backup_result.checksum.clone()),
            metadata: Set(backup_result.metadata.clone()),
        };

        target_backup
            .insert(target_db.db.as_ref())
            .await
            .expect("Failed to create backup record in target database");

        println!("\n✓ Backup record created in target database");

        // Restore backup to target database
        println!("\n→ Starting restore to target database...");
        let restore_result = target_backup_service
            .restore_backup(&backup_result.backup_id)
            .await;

        // Note: pg_restore may emit warnings when restoring to a database with existing schema
        // This is expected behavior and not a failure
        match restore_result {
            Ok(_) => {
                println!("✓ Restore completed successfully");
            }
            Err(e) => {
                let error_msg = e.to_string();
                // Check if error contains "errors ignored" which indicates successful restore with warnings
                if error_msg.contains("errors ignored") || error_msg.contains("pg_restore") {
                    println!("✓ Restore completed with expected schema conflicts (this is normal when restoring to an existing schema)");
                } else {
                    panic!("Unexpected restore error: {:?}", e);
                }
            }
        }

        // Verify data was restored in target database
        println!("\n→ Verifying restored data in target database...");

        let restored_users = users::Entity::find()
            .all(target_db.db.as_ref())
            .await
            .expect("Failed to query users in target database");

        let restored_projects = projects::Entity::find()
            .all(target_db.db.as_ref())
            .await
            .expect("Failed to query projects in target database");

        // Find the specific project we created
        let restored_project = projects::Entity::find()
            .filter(projects::Column::Slug.eq("test-project"))
            .one(target_db.db.as_ref())
            .await
            .expect("Failed to find project by slug")
            .expect("Project with slug 'test-project' should exist after restore");

        // Find the specific user we created
        let restored_user = users::Entity::find()
            .filter(users::Column::Email.eq("test@example.com"))
            .one(target_db.db.as_ref())
            .await
            .expect("Failed to find user by email")
            .expect("User with email 'test@example.com' should exist after restore");

        println!("\n✓ Restore verification:");
        println!("  - Source database:");
        println!("    • Users: {}", user_count_before);
        println!("    • Projects: {}", project_count_before);
        println!(
            "    • Created project: '{}' (slug: {})",
            created_project.name, created_project.slug
        );
        println!("  - Target database after restore:");
        println!("    • Users: {}", restored_users.len());
        println!("    • Projects: {}", restored_projects.len());
        println!(
            "    • Restored user: '{}' (email: {})",
            restored_user.name, restored_user.email
        );
        println!(
            "    • Restored project: '{}' (slug: {}, git_url: {})",
            restored_project.name,
            restored_project.slug,
            restored_project
                .git_url
                .as_ref()
                .unwrap_or(&"None".to_string())
        );

        // Verify the data matches
        assert_eq!(
            restored_user.email, created_user.email,
            "Restored user email should match original"
        );
        assert_eq!(
            restored_project.slug, created_project.slug,
            "Restored project slug should match original"
        );
        assert_eq!(
            restored_project.name, created_project.name,
            "Restored project name should match original"
        );
        assert_eq!(
            restored_project.repo_name, created_project.repo_name,
            "Restored project repo_name should match original"
        );
        assert_eq!(
            restored_project.repo_owner, created_project.repo_owner,
            "Restored project repo_owner should match original"
        );
        assert_eq!(
            restored_project.git_url, created_project.git_url,
            "Restored project git_url should match original"
        );
        assert_eq!(
            restored_project.main_branch, created_project.main_branch,
            "Restored project main_branch should match original"
        );

        println!("\n✓ Integration test passed:");
        println!("  - Source database created with test data (user + project)");
        println!("  - Backup created and uploaded to RustFS");
        println!("  - Target database created");
        println!("  - Backup restored to target database from URL");
        println!("  - Data verified: project and user successfully restored with matching data");
    }
}
