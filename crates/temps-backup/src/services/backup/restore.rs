// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Restoring control-plane backups (SQLite, pg_dump and WAL-G).

use super::*;

impl BackupService {
    pub async fn restore_backup(&self, backup_id: &str) -> Result<(), BackupError> {
        let started = std::time::Instant::now();
        let result = self.restore_backup_inner(backup_id).await;
        self.telemetry.report(crate::telemetry::operation_outcome(
            true,
            "control_plane",
            started.elapsed(),
            result.as_ref().map(|_| ()).map_err(ToString::to_string),
        ));
        result
    }

    async fn restore_backup_inner(&self, backup_id: &str) -> Result<(), BackupError> {
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
    async fn restore_postgres_walg(
        &self,
        backup: &temps_entities::backups::Model,
        s3_source: &temps_entities::s3_sources::Model,
    ) -> Result<(), BackupError> {
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

        // A repository is shared across snapshots. Older rows need their UUID
        // matched before any restore command can touch the database.
        let client = self.create_s3_client(s3_source).await?;
        let location = if temps_providers::externalsvc::postgres::walg_snapshot(&backup.s3_location)
            .is_some()
        {
            backup.s3_location.clone()
        } else {
            let prefix = backup
                .s3_location
                .strip_prefix(&format!("s3://{}/", s3_source.bucket_name))
                .ok_or_else(|| {
                    BackupError::Validation(format!(
                        "Backup {} is outside S3 source {}",
                        backup.backup_id, s3_source.id
                    ))
                })?;
            crate::engines::walg_size::find_snapshot(
                &client, &s3_source.bucket_name, prefix.trim_start_matches('/'), &backup.backup_id,
            ).await.map_err(|error| BackupError::S3(error.to_string()))?
                .ok_or_else(|| {
                    BackupError::Validation(format!(
                        "No WAL-G snapshot in {} identifies control-plane backup {}. For an older unmarked backup, run wal-g backup-list against this repository and recover the intended base by its exact name.",
                        backup.s3_location, backup.backup_id
                    ))
                })?
        };
        let (walg_s3_prefix, snapshot) =
            temps_providers::externalsvc::postgres::walg_snapshot(&location).ok_or_else(|| {
                BackupError::Validation(format!(
                    "Invalid WAL-G snapshot for backup {}: {location}",
                    backup.backup_id
                ))
            })?;
        let sentinel_key = location
            .strip_prefix(&format!("s3://{}/", s3_source.bucket_name))
            .ok_or_else(|| {
                BackupError::Validation(format!(
                    "Catalog backup {} is outside S3 source {}",
                    backup.backup_id, s3_source.id
                ))
            })?;
        let sentinel =
            crate::engines::walg_size::read_sentinel(&client, &s3_source.bucket_name, sentinel_key)
                .await
                .map_err(|error| BackupError::S3(error.to_string()))?;
        validate_catalog_tablespaces(&sentinel, &location)?;
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
            force_path_style: s3_source.effective_path_style(true).force_path_style,
        };
        if let Some(resolved_endpoint) = s3_creds
            .resolve_endpoint_for_container(&docker, &container_id)
            .await
        {
            walg_env.push(format!("AWS_ENDPOINT={}", resolved_endpoint));
        }
        if s3_source.effective_path_style(true).force_path_style {
            walg_env.push("AWS_S3_FORCE_PATH_STYLE=true".to_string());
        }

        let snapshot = snapshot.to_owned();
        let database_url = self.config_service.get_database_url();
        // A dropped HTTP request must not interrupt a catalog swap or its rollback.
        tokio::spawn(async move {
            recover_catalog(
                &docker,
                &container_id,
                &pgdata,
                &snapshot,
                walg_env,
                &database_url,
                std::time::Duration::from_secs(120),
            )
            .await
        })
        .await
        .map_err(|error| {
            BackupError::ExternalService(format!("Catalog recovery task failed: {error}"))
        })?
    }
}

/// A tablespace in the backup can point outside the directory being swapped.
fn validate_catalog_tablespaces(bytes: &[u8], location: &str) -> Result<(), BackupError> {
    #[derive(Deserialize)]
    struct Sentinel {
        #[serde(rename = "Spec", alias = "spec")]
        spec: Option<TablespaceSpec>,
    }
    #[derive(Deserialize)]
    struct TablespaceSpec {
        tablespaces: Option<Vec<String>>,
    }
    let sentinel: Sentinel = serde_json::from_slice(bytes).map_err(|error| {
        BackupError::Validation(format!(
            "Invalid tablespace metadata in {location}: {error}"
        ))
    })?;
    if sentinel
        .spec
        .and_then(|spec| spec.tablespaces)
        .is_some_and(|names| !names.is_empty())
    {
        return Err(BackupError::Validation(format!("Catalog backup {location} uses external tablespaces. Restore the named base and its tablespaces on isolated volumes before manual cutover.")));
    }
    Ok(())
}

const RECOVER_STAGED: &str = r#"
set -eu
stage=$1
snapshot=$2
test -d "$stage"
# The staged postmaster uses a private socket and never archives into the source.
sock=$(mktemp -d "${TMPDIR:-/tmp}/pg.XXXXXXXX")
cleanup() {
    status=$?
    trap - EXIT
    set +e
    if [ "$started" = yes ] && [ -s "$stage/postmaster.pid" ]; then
        if ! pg_ctl -D "$stage" -m immediate -t 60 -w stop; then
            echo "Could not stop staged PostgreSQL. Inspect $stage and $sock/postgres.log before cleanup." >&2
            exit "$status"
        fi
    fi
    if [ "$status" -ne 0 ] && [ -s "$sock/postgres.log" ] &&
        (umask 077 && tail -c 4096 "$sock/postgres.log" > "$sock/startup-error.log"); then
        echo "Read $sock/startup-error.log in the catalog container for the PostgreSQL recovery diagnostic." >&2
        rm -f "$sock/postgres.log" "$sock/restore-spec.json"
    else
        rm -rf "$sock"
    fi
    if [ "$prepared" = no ]; then find "$stage" -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +; fi
    exit "$status"
}
prepared=no
started=no
trap cleanup EXIT
# Pin extraction to the staged directory for every member of a delta chain.
printf '{"base_prefix":"%s","tablespaces":[]}\n' "$stage" > "$sock/restore-spec.json"
wal-g backup-fetch "$stage" "$snapshot" --restore-spec "$sock/restore-spec.json"
test ! -e "$stage/postmaster.pid"
if [ -s "$stage/tablespace_map" ]; then
    echo "The selected base has external tablespaces. Recover those to isolated volumes before manual cutover." >&2
    exit 1
fi
touch "$stage/recovery.signal"
rm -f "$stage/standby.signal"
cat >> "$stage/postgresql.auto.conf" <<'CONF'
restore_command = 'wal-g wal-fetch "%f" "%p"'
recovery_target = 'immediate'
recovery_target_action = 'promote'
CONF
export PGDATA="$stage"
started=yes
pg_ctl -D "$stage" -l "$sock/postgres.log" -t 180 -w start -o "-c listen_addresses='' -c unix_socket_directories='$sock' -c data_directory='$stage' -c archive_mode=off -c external_pid_file='' -c hot_standby=off"
remaining=180
while [ -e "$stage/recovery.signal" ]; do
    pg_ctl -D "$stage" status >/dev/null
    test "$remaining" -gt 0
    remaining=$((remaining - 1))
    sleep 1
done
pg_ctl -D "$stage" -m fast -t 60 -w stop
prepared=yes
"#;

const INSTALL_CATALOG: &str = r#"
set -eu
test -d "$1"
test -d "$2"
test ! -e "$3"
mv "$1" "$3"
mv "$2" "$1" || { mv "$3" "$1"; exit 1; }
"#;

const ROLLBACK_CATALOG: &str = r#"
set -eu
if [ -d "$3" ]; then
    if [ -e "$1" ]; then mv "$1" "$2"; fi
    mv "$3" "$1"
fi
test -s "$1/PG_VERSION"
"#;

async fn recover_catalog(
    docker: &bollard::Docker,
    container: &str,
    pgdata: &str,
    snapshot: &str,
    env: Vec<String>,
    database_url: &str,
    readiness_timeout: std::time::Duration,
) -> Result<(), BackupError> {
    use bollard::query_parameters::{
        InspectContainerOptions, StartContainerOptions, StopContainerOptions,
    };
    use std::time::Duration;
    use temps_providers::externalsvc::exec_util::run_exec_as;
    let failure = |step: &str, reason: String| {
        BackupError::ExternalService(format!(
            "Catalog restore of {snapshot} on container {container} could not {step}: {reason}"
        ))
    };
    let inspected = docker
        .inspect_container(container, None::<InspectContainerOptions>)
        .await
        .map_err(|e| failure("inspect the catalog", e.to_string()))?;
    let pgdata = pgdata.trim_end_matches('/');
    let data = std::path::Path::new(pgdata);
    let mount = inspected
        .mounts
        .iter()
        .flatten()
        .filter_map(|m| m.destination.as_deref())
        .map(std::path::Path::new)
        .filter(|p| data.starts_with(p))
        .max_by_key(|p| p.components().count());
    // Renaming two sibling directories is atomic only within their shared mount.
    if mount.is_none_or(|path| path == data)
        || inspected
            .mounts
            .iter()
            .flatten()
            .filter_map(|m| m.destination.as_deref())
            .any(|path| std::path::Path::new(path).starts_with(data))
        || pgdata.split('/').any(|part| part == "." || part == "..")
        || !data.is_absolute()
        || pgdata
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || "/_-.".contains(c)))
    {
        return Err(failure("stage a reversible swap", "PGDATA must be a subdirectory of a mounted volume with a simple absolute path. Recover the named base to a new volume for manual cutover.".into()));
    }
    let image = inspected.image.ok_or_else(|| {
        failure(
            "identify the catalog image",
            "Docker returned no image ID".into(),
        )
    })?;
    let attempt = Uuid::new_v4().to_string();
    let stage = format!("{pgdata}.restore-{attempt}");
    let original = format!("{pgdata}.original-{attempt}");
    let config = bollard::models::ContainerCreateBody {
        image: Some(image),
        entrypoint: Some(vec!["sleep".into()]),
        cmd: Some(vec!["infinity".into()]),
        labels: Some(std::collections::HashMap::from([(
            "temps.backup_cleanup_id".into(),
            attempt.clone(),
        )])),
        host_config: Some(bollard::models::HostConfig {
            volumes_from: Some(vec![container.into()]),
            network_mode: Some("none".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    // Only one catalog restore may own the swap on this database container.
    let (helper, _owner) = super::walg_deletion::create_owned_walg_helper(
        docker.clone(),
        format!("temps-catalog-restore-{container}"),
        attempt,
        config,
    )
    .await?;
    docker
        .start_container(&helper, None::<StartContainerOptions>)
        .await
        .map_err(|e| failure("start the swap helper", e.to_string()))?;
    let helper_exec = |script: &'static str| {
        run_exec_as(
            docker,
            &helper,
            vec![
                "sh".into(),
                "-e".into(),
                "-c".into(),
                script.into(),
                "catalog-swap".into(),
                pgdata.into(),
                stage.clone(),
                original.clone(),
            ],
            None,
            Duration::from_secs(120),
            Some("root"),
        )
    };
    let cleanup_stage = || helper_exec(r#"rm -rf "$2""#);
    helper_exec(r#"mkdir -m 700 "$2" && chown postgres "$2""#)
        .await
        .map_err(|e| failure("create the staged data directory", e.to_string()))?;
    let prepared = run_exec_as(
        docker,
        container,
        vec![
            "sh".into(),
            "-e".into(),
            "-c".into(),
            RECOVER_STAGED.into(),
            "recover".into(),
            stage.clone(),
            snapshot.into(),
        ],
        Some(env),
        Duration::from_secs(3600),
        Some("postgres"),
    )
    .await;
    if let Err(error) = prepared {
        let _ = helper_exec(r#"rmdir "$2""#).await;
        return Err(failure(
            "recover the staged base from archived WAL",
            error.to_string(),
        ));
    }
    let stop = || async {
        match docker
            .stop_container(
                container,
                Some(StopContainerOptions {
                    t: Some(60),
                    ..Default::default()
                }),
            )
            .await
        {
            Ok(())
            | Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304, ..
            }) => Ok(()),
            Err(error) => Err(error),
        }
    };
    let start = || docker.start_container(container, None::<StartContainerOptions>);
    if let Err(stop_error) = stop().await {
        // The daemon may have stopped PostgreSQL before losing its reply.
        // No directories have been swapped. Confirm the original is serving.
        if let Err(error) =
            ensure_original_catalog_ready(docker, container, database_url, readiness_timeout).await
        {
            return Err(failure("stop the catalog", format!("{stop_error}. {error}. Inspect the catalog container and its original data at {pgdata}; the staged copy is at {stage}.")));
        }
        if let Err(error) = cleanup_stage().await {
            warn!(container, %stage, %error, "Original catalog is ready; could not remove the unused staged catalog");
        }
        return Err(failure(
            "stop the catalog",
            format!("{stop_error}. The original catalog is ready. Retry the restore."),
        ));
    }
    let installed = async {
        helper_exec(INSTALL_CATALOG)
            .await
            .map_err(|e| failure("install the recovered catalog", e.to_string()))?;
        start()
            .await
            .map_err(|e| failure("start the recovered catalog", e.to_string()))?;
        wait_for_catalog(database_url, readiness_timeout)
            .await
            .map_err(|_| failure("verify the recovered catalog", "readiness timed out".into()))
    }
    .await;
    if let Err(error) = installed {
        let rollback = async {
            stop().await.map_err(|e| e.to_string())?;
            helper_exec(ROLLBACK_CATALOG)
                .await
                .map_err(|e| e.to_string())?;
            start().await.map_err(|e| e.to_string())?;
            wait_for_catalog(database_url, readiness_timeout)
                .await
                .map_err(|_| "Original catalog readiness timed out".to_string())
        }
        .await;
        if let Err(reason) = rollback {
            return Err(failure("restart the original catalog", format!("{error}. Rollback failed: {reason}. Inspect {original} before restarting PostgreSQL.")));
        }
        let _ = cleanup_stage().await;
        return Err(error);
    }
    if let Err(error) = helper_exec(r#"rm -rf "$3""#).await {
        warn!(container, %original, %error, "Restored catalog is ready; could not remove the previous catalog");
    }
    Ok(())
}

async fn ensure_original_catalog_ready(
    docker: &bollard::Docker,
    container: &str,
    database_url: &str,
    timeout: std::time::Duration,
) -> Result<(), BackupError> {
    use bollard::query_parameters::{InspectContainerOptions, StartContainerOptions};
    let state = docker
        .inspect_container(container, None::<InspectContainerOptions>)
        .await
        .map_err(|error| {
            BackupError::ExternalService(format!(
                "Could not inspect original catalog {container}: {error}"
            ))
        })?;
    match state.state.and_then(|state| state.running) {
        Some(false) => {
            docker
                .start_container(container, None::<StartContainerOptions>)
                .await
                .map_err(|error| {
                    BackupError::ExternalService(format!(
                        "Could not restart original catalog {container}: {error}"
                    ))
                })?;
        }
        Some(true) => {}
        None => {
            return Err(BackupError::ExternalService(format!(
                "Docker did not report whether original catalog {container} is running"
            )))
        }
    }
    wait_for_catalog(database_url, timeout).await.map_err(|_| {
        BackupError::ExternalService(format!("Original catalog {container} did not become ready"))
    })
}

async fn wait_for_catalog(
    database_url: &str,
    readiness_timeout: std::time::Duration,
) -> Result<(), time::error::Elapsed> {
    use std::time::Duration;
    time::timeout(readiness_timeout, async {
        loop {
            if let Ok(db) =
                sea_orm::Database::connect(temps_database::connect_options(database_url)).await
            {
                use sea_orm::ConnectionTrait;
                if db
                    .query_one(Statement::from_string(
                        DatabaseBackend::Postgres,
                        "SELECT 1 WHERE NOT pg_is_in_recovery()",
                    ))
                    .await
                    .is_ok_and(|row| row.is_some())
                {
                    break;
                }
            }
            time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use temps_core::EncryptionService;

    #[test]
    fn external_tablespaces_require_isolated_manual_recovery() {
        for key in ["Spec", "spec"] {
            let sentinel = json!({ key: { "tablespaces": ["1234"] } }).to_string();
            assert!(
                validate_catalog_tablespaces(sentinel.as_bytes(), "base_001")
                    .is_err_and(|error| error.to_string().contains("isolated volumes"))
            );
        }
        assert!(
            validate_catalog_tablespaces(br#"{"Spec":{"tablespaces":[]}}"#, "base_001").is_ok()
        );
    }

    #[test]
    fn staged_start_failure_preserves_bounded_private_diagnostic() -> std::io::Result<()> {
        use std::{fs, os::unix::fs::PermissionsExt, process::Command};
        let root = tempfile::tempdir()?;
        let stage = root.path().join("stage");
        let tmp = root.path().join("tmp");
        fs::create_dir(&stage)?;
        fs::create_dir(&tmp)?;
        for (name, script) in [
            ("wal-g", "exit 0"),
            (
                "pg_ctl",
                r#"while [ "$1" != "-l" ]; do shift; done
dd if=/dev/zero bs=8192 count=1 > "$2" 2>/dev/null
echo "missing segment" >> "$2"
exit 42"#,
            ),
        ] {
            let path = root.path().join(name);
            fs::write(&path, format!("#!/bin/sh\n{script}\n"))?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
        let output = Command::new("sh")
            .args(["-ec", RECOVER_STAGED, "recover"])
            .arg(&stage)
            .arg("base_001")
            .env("TMPDIR", &tmp)
            .env("PATH", format!("{}:/usr/bin:/bin", root.path().display()))
            .output()?;
        assert_eq!(output.status.code(), Some(42));
        let message = String::from_utf8_lossy(&output.stderr);
        let files = fs::read_dir(&tmp)?.collect::<std::io::Result<Vec<_>>>()?;
        assert_eq!(files.len(), 1);
        let dir = files[0].path();
        let log = dir.join("startup-error.log");
        assert!(message.contains(&*log.to_string_lossy()));
        assert!(!message.contains("missing segment"));
        let bytes = fs::read(&log)?;
        assert_eq!(bytes.len(), 4096);
        assert!(bytes.ends_with(b"missing segment\n"));
        assert_eq!(fs::metadata(log)?.permissions().mode() & 0o077, 0);
        assert!(!dir.join("postgres.log").exists());
        assert_eq!(fs::read_dir(stage)?.count(), 0);
        Ok(())
    }

    #[test]
    fn rollback_puts_the_original_catalog_back_with_hidden_files() -> std::io::Result<()> {
        use std::process::Command;
        let root = tempfile::tempdir()?;
        let paths = ["data", "stage", "original"].map(|name| root.path().join(name));
        for (path, marker) in paths.iter().zip(["before", "restored"]) {
            std::fs::create_dir(path)?;
            std::fs::write(path.join("PG_VERSION"), "18")?;
            std::fs::write(path.join(".marker"), marker)?;
        }
        let run = |script| {
            Command::new("sh")
                .args(["-ec", script, "swap"])
                .args(&paths)
                .status()
                .map(|status| status.success())
        };
        assert!(run(INSTALL_CATALOG)?);
        assert_eq!(
            std::fs::read_to_string(paths[0].join(".marker"))?,
            "restored"
        );
        assert!(run(ROLLBACK_CATALOG)?);
        assert_eq!(std::fs::read_to_string(paths[0].join(".marker"))?, "before");
        Ok(())
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn catalog_readiness_failure_restores_the_live_database(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use std::time::Duration;
        use temps_providers::externalsvc::exec_util::run_exec_as;
        use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};
        if !temps_test_utils::docker_available().await {
            eprintln!("Docker unavailable, skipping catalog rollback");
            return Ok(());
        }
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))?
            .local_addr()?
            .port();
        let password = Uuid::new_v4();
        let database = GenericImage::new("gotempsh/postgres-walg", "18-bookworm")
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_mapped_port(port, testcontainers::core::ContainerPort::Tcp(5432))
            .with_env_var("POSTGRES_PASSWORD", password.to_string())
            .with_env_var("WALG_FILE_PREFIX", "/tmp/repository")
            .with_cmd([
                "postgres",
                "-c",
                "archive_mode=on",
                "-c",
                "archive_command=wal-g wal-push %p",
            ])
            .start()
            .await?;
        let url = format!("postgres://postgres:{password}@127.0.0.1:{port}/postgres");
        wait_for_catalog(&url, Duration::from_secs(30))
            .await
            .map_err(|_| "Source catalog never became ready")?;
        let docker = bollard::Docker::connect_with_local_defaults()?;
        let setup = run_exec_as(
            &docker,
            database.id(),
            vec![
                "sh".into(),
                "-ec".into(),
                r#"mkdir -p /tmp/repository
wal-g backup-push "$PGDATA"
createdb after_backup"#
                    .into(),
            ],
            None,
            Duration::from_secs(90),
            Some("postgres"),
        )
        .await?;
        let name = crate::engines::walg_size::pushed_snapshot_name(&setup.output, "")?;
        let original_only = format!("postgres://postgres:{password}@127.0.0.1:{port}/after_backup");
        // Simulate a completed stop whose acknowledgement was lost. Verify
        // restarting serves the original database created after the backup.
        database.stop().await?;
        ensure_original_catalog_ready(
            &docker,
            database.id(),
            &original_only,
            Duration::from_secs(30),
        )
        .await?;
        let error = recover_catalog(
            &docker,
            database.id(),
            "/var/lib/postgresql/18/docker",
            name,
            vec!["WALG_FILE_PREFIX=/tmp/repository".into()],
            &original_only,
            Duration::from_secs(8),
        )
        .await
        .err()
        .ok_or("Expected catalog readiness to fail")?;
        assert!(
            error.to_string().contains("verify the recovered catalog")
                && !error.to_string().contains("Rollback failed"),
            "{error}"
        );
        wait_for_catalog(&original_only, Duration::from_secs(10)).await?;
        Ok(())
    }

    #[cfg(feature = "docker-tests")]
    #[test]
    fn staged_catalog_recovers_an_older_base_without_the_live_wal() -> std::io::Result<()> {
        use std::process::Command;
        if !Command::new("docker")
            .args(["info", "--format", "{{.ServerVersion}}"])
            .output()
            .is_ok_and(|r| r.status.success())
        {
            eprintln!("Skipping staged catalog recovery: Docker unavailable");
            return Ok(());
        }
        // Stop the source and move all of its WAL out of reach. The selected
        // base must reach consistency using wal-fetch from the archive alone.
        let setup = r#"
set -eu
export PGDATA=/tmp/live PGHOST=/tmp WALG_FILE_PREFIX=/tmp/repository
mkdir -p "$WALG_FILE_PREFIX"
initdb -A trust -D "$PGDATA" >/dev/null
pg_ctl -D "$PGDATA" -l /tmp/live.log -w start -o "-k /tmp -c archive_mode=on -c 'archive_command=wal-g wal-push %p'"
psql -d postgres -c 'CREATE TABLE marker AS SELECT 1 AS n'
wal-g backup-push "$PGDATA"
name=$(wal-g backup-list | awk '$1 ~ /^base_/ {print $1; exit}')
test -n "$name"
psql -d postgres -c 'INSERT INTO marker VALUES (2)'
wal-g backup-push "$PGDATA"
psql -d postgres -c 'SELECT pg_switch_wal()'
pg_ctl -D "$PGDATA" -m fast -w stop
mv "$PGDATA/pg_wal" /tmp/unavailable_wal
mkdir -m 700 /tmp/staged
sh -ec "$1" recover /tmp/staged "$name"
pg_ctl -D /tmp/staged -l /tmp/restored.log -w start -o "-k /tmp -c archive_mode=off"
test "$(psql -d postgres -Atc 'SELECT sum(n) FROM marker')" = 1
"#;
        let output = Command::new("docker")
            .args([
                "run",
                "--rm",
                "--user",
                "postgres",
                "--entrypoint",
                "sh",
                "gotempsh/postgres-walg:18-bookworm",
                "-ec",
                setup,
                "test",
                RECOVER_STAGED,
            ])
            .output()?;
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }

    #[tokio::test]
    async fn legacy_restore_reports_one_failure_on_early_database_error() {
        #[derive(Default)]
        struct Recorder(std::sync::Mutex<Vec<temps_core::telemetry::TelemetryEvent>>);
        impl temps_core::telemetry::TelemetryReporter for Recorder {
            fn report(&self, event: temps_core::telemetry::TelemetryEvent) {
                self.0.lock().unwrap().push(event);
            }
            fn is_enabled(&self) -> bool {
                true
            }
        }
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("database unavailable".into())])
            .into_connection();
        let recorder = Arc::new(Recorder::default());
        let service = make_scope_test_service(Arc::new(db)).with_telemetry(recorder.clone());
        assert!(service.restore_backup("sample-backup").await.is_err());
        let events = recorder.0.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "restore_failed");
        assert_eq!(events[0].properties["engine"], "control_plane");
        assert!(!serde_json::to_string(&events[0])
            .unwrap()
            .contains("sample-backup"));
    }

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

        use temps_test_utils::TestDatabase;
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
            worker_credentials: None,
            use_tls: None,
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
            worker_credentials: None,
            use_tls: None,
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
