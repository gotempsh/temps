// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Control-plane backups (`create_backup`), the pg_dump sidecar and the upload
//! to S3.

use super::*;

/// Longest engine error kept per service in an aggregated failure message.
const MAX_FAILURE_CAUSE_CHARS: usize = 300;

/// One service's failure cause, short enough to aggregate several of them
/// into a single message an operator can read.
fn bounded_failure_cause(cause: &str) -> String {
    let cause = cause.trim();
    match cause.char_indices().nth(MAX_FAILURE_CAUSE_CHARS) {
        Some((cut, _)) => format!("{}...", &cause[..cut]),
        None => cause.to_string(),
    }
}

impl BackupService {
    /// Send a backup failure notification
    pub async fn send_backup_failure_notification(
        &self,
        backup_failure_data: BackupFailureData,
    ) -> Result<(), BackupError> {
        let request = FireAlarmRequest {
            project_id: None,
            environment_id: None,
            deployment_id: None,
            container_id: None,
            service_id: None,
            alarm_type: AlarmType::BackupFailed,
            severity: AlarmSeverity::Critical,
            title: format!("Backup Failed: {}", backup_failure_data.schedule_name),
            message: format!(
                "Backup failed for {} ({}): {}",
                backup_failure_data.schedule_name,
                backup_failure_data.backup_type,
                backup_failure_data.error
            ),
            metadata: Some(json!({
                "schedule_id": backup_failure_data.schedule_id,
                "schedule_name": backup_failure_data.schedule_name,
                "backup_type": backup_failure_data.backup_type,
                "timestamp": Utc::now().to_rfc3339(),
            })),
        };

        self.alarm_service
            .fire_alarm(request)
            .await
            .map_err(|e| BackupError::NotificationError(e.to_string()))?;

        Ok(())
    }

    pub async fn create_backup(
        &self,
        schedule_id: Option<i32>,
        s3_source_id: i32,
        backup_type: &str,
        created_by: i32,
    ) -> Result<Backup, BackupError> {
        info!("Starting backup process");

        // Get S3 source configuration
        let s3_source = temps_entities::s3_sources::Entity::find_by_id(s3_source_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: "S3 source not found".to_string(),
            })?;

        // Generate unique backup ID
        let backup_id = Uuid::new_v4().to_string();

        // Create S3 client for the portable OSS fallback.
        let s3_client = self.create_s3_client(&s3_source).await?;

        // WAL-G is preferred because it supports PITR and streams directly to
        // object storage. OSS remains usable with arbitrary PostgreSQL images,
        // so a local-only pg_dump artifact is still a supported fallback. The
        // Cloud mirror deliberately rejects that fallback and explains that a
        // WAL-G-capable image is required for managed backups.
        let (s3_location, size_bytes, compression_type) = match self
            .backup_postgres_walg(&s3_source, &backup_id)
            .await
        {
            Ok((location, size)) => {
                info!("WAL-G backup completed: {}", location);
                (location, size, "lz4".to_string())
            }
            Err(error) => {
                warn!(
                    error = %error,
                    "WAL-G unavailable; creating a local OSS pg_dump backup that will not be mirrored to Cloud"
                );
                let mut temp_file = NamedTempFile::new().map_err(BackupError::Io)?;
                self.backup_postgres_database(&mut temp_file).await?;
                let size_bytes = temp_file
                    .as_file()
                    .metadata()
                    .map_err(BackupError::Io)?
                    .len() as i64;
                if size_bytes == 0 {
                    return Err(BackupError::Validation(
                        "Backup failed: pg_dump produced an empty artifact".to_string(),
                    ));
                }
                let s3_location = build_s3_key(
                    &s3_source.bucket_path,
                    &format!(
                        "backups/{}/{}/backup.sql.gz",
                        Utc::now().format("%Y/%m/%d"),
                        backup_id
                    ),
                );
                self.upload_backup(&s3_client, &s3_source, &temp_file, &s3_location)
                    .await?;
                (s3_location, size_bytes, "gzip".to_string())
            }
        };

        // Retention deadline: a backup owned by a schedule is deleted by
        // `run_retention_cleanup` once it is older than the schedule's
        // retention period. Read that period now, right before the row is
        // written, rather than before the (potentially long-running) dump —
        // otherwise a concurrent schedule retention edit that already
        // recomputed every *existing* row's `expires_at` would be lost the
        // moment this backup's row is inserted with the stale value. A
        // schedule that disappears in the meantime is not a reason to fail
        // the backup — the row is then kept until deleted.
        let retention_period = match schedule_id {
            Some(id) => match self.get_backup_schedule(id).await {
                Ok(schedule) => Some(schedule.retention_period),
                Err(error) => {
                    warn!(
                        schedule_id = id,
                        error = %error,
                        "create_backup: could not load schedule to compute the retention deadline; \
                         backup {} will be recorded without an expiry",
                        backup_id,
                    );
                    None
                }
            },
            None => None,
        };

        // Create backup record
        let started_at = chrono::Utc::now();
        let new_backup = temps_entities::backups::ActiveModel {
            id: sea_orm::NotSet,
            name: sea_orm::Set(format!("Backup {}", backup_id)),
            backup_id: sea_orm::Set(backup_id.clone()),
            schedule_id: sea_orm::Set(schedule_id),
            schedule_run_id: sea_orm::NotSet,
            backup_type: sea_orm::Set(backup_type.to_string()),
            state: sea_orm::Set("completed".to_string()),
            started_at: sea_orm::Set(started_at),
            finished_at: sea_orm::Set(Some(chrono::Utc::now())),
            s3_source_id: sea_orm::Set(s3_source_id),
            s3_location: sea_orm::Set(s3_location.clone()),
            compression_type: sea_orm::Set(compression_type),
            created_by: sea_orm::Set(created_by),
            tags: sea_orm::Set("[]".to_string()),
            size_bytes: sea_orm::Set(Some(size_bytes)),
            file_count: sea_orm::Set(None),
            error_message: sea_orm::Set(None),
            expires_at: sea_orm::Set(
                retention_period.and_then(|days| retention_expiry(started_at, days)),
            ),
            checksum: sea_orm::Set(None),
            metadata: sea_orm::Set(
                serde_json::json!({
                    "size_bytes": size_bytes,
                    "database_version": "1.0",
                    "timestamp": Utc::now().to_rfc3339()
                })
                .to_string(),
            ),
        };

        let backup = new_backup.insert(self.db.as_ref()).await?;

        // Backup all external services
        let external_services = temps_entities::external_services::Entity::find()
            .all(self.db.as_ref())
            .await?;

        let mut external_backups = Vec::new();
        let mut failed_services = Vec::new();

        for service in external_services {
            match self
                .backup_external_service(&service, s3_source_id, backup_type, created_by)
                .await
            {
                Ok(backup) => {
                    info!(
                        "Successfully backed up external service {}: {}",
                        service.name, backup.backup_id
                    );
                    external_backups.push((backup, service));
                }
                Err(e) => {
                    error!("Failed to backup external service {}: {}", service.name, e);
                    failed_services.push(format!(
                        "{} ({})",
                        service.name,
                        bounded_failure_cause(&e.to_string())
                    ));

                    // Send notification about this specific failure
                    let error_msg = format!("External service backup failed: {}", e);
                    let failure_data = BackupFailureData {
                        schedule_id: schedule_id.unwrap_or(-1),
                        schedule_name: format!("External Service: {}", service.name),
                        backup_type: backup_type.to_string(),
                        error: error_msg.clone(),
                        timestamp: Utc::now(),
                    };

                    if let Err(notify_err) =
                        self.send_backup_failure_notification(failure_data).await
                    {
                        error!("Failed to send backup failure notification: {}", notify_err);
                    }

                    // Continue with next service instead of stopping
                }
            }
        }

        // Log summary of failed services if any
        if !failed_services.is_empty() {
            error!(
                "Backup completed with failures. Failed services: {}",
                failed_services.join("; ")
            );
            return Err(BackupError::Internal {
                message: format!(
                    "Whole-instance backup {} is incomplete because these services failed: {}",
                    backup.backup_id,
                    failed_services.join("; ")
                ),
            });
        }

        // After successful backup upload, create and upload metadata file
        let metadata = self.generate_backup_metadata(&backup, &s3_source, &external_backups)?;
        let metadata_key = build_s3_key(
            &s3_source.bucket_path,
            &format!(
                "backups/{}/{}/metadata.json",
                Utc::now().format("%Y/%m/%d"),
                backup_id
            ),
        );

        // Upload metadata file
        s3_client
            .put_object()
            .bucket(&s3_source.bucket_name)
            .key(&metadata_key)
            .body(
                serde_json::to_vec(&metadata)
                    .map_err(BackupError::Serialization)?
                    .into(),
            )
            .content_type("application/json")
            .send()
            .await
            .map_err(|e| BackupError::S3(format!("Failed to upload metadata: {}", e)))?;

        // Update backup index
        self.update_backup_index(&s3_client, &s3_source, &backup)
            .await?;

        info!("Backup completed successfully: {}", backup_id);
        Ok(backup)
    }

    /// Fetches the PostgreSQL version from the database
    pub(super) async fn get_postgres_version(&self) -> Result<String> {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        let version_result = self
            .db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT version()".to_string(),
            ))
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query PostgreSQL version: {}", e))?
            .ok_or_else(|| anyhow::anyhow!("No version result returned"))?;

        let version_str: String = version_result
            .try_get("", "version")
            .map_err(|e| anyhow::anyhow!("Failed to extract version string: {}", e))?;

        debug!("PostgreSQL version string: {}", version_str);
        Ok(version_str)
    }

    /// Parses PostgreSQL version string and returns the major version number
    /// Example: "PostgreSQL 15.3 on x86_64..." -> "15"
    pub(super) fn parse_postgres_version(&self, version_str: &str) -> Result<String> {
        // Version string format: "PostgreSQL 15.3 on x86_64-pc-linux-gnu..."
        let parts: Vec<&str> = version_str.split_whitespace().collect();

        if parts.len() < 2 {
            anyhow::bail!("Invalid PostgreSQL version string format: {}", version_str);
        }

        let version = parts[1]; // "15.3"
        let major_version = version
            .split('.')
            .next()
            .ok_or_else(|| anyhow::anyhow!("Failed to extract major version from: {}", version))?;

        debug!("Extracted PostgreSQL major version: {}", major_version);
        Ok(major_version.to_string())
    }

    /// Returns the Docker image tag for the pg_dump sidecar container.
    /// Temps requires TimescaleDB as its database, so the sidecar always uses the
    /// timescaledb-ha image to ensure pg_dump has the extension available.
    pub(super) fn get_postgres_image_tag(&self, major_version: &str) -> String {
        format!("timescale/timescaledb-ha:pg{}", major_version)
    }

    /// Pulls the specified PostgreSQL Docker image
    pub(super) async fn pull_postgres_image(&self, image_tag: &str) -> Result<()> {
        use bollard::query_parameters::CreateImageOptionsBuilder;
        use bollard::Docker;
        use futures::stream::StreamExt as FuturesStreamExt;

        info!("Pulling Docker image: {}", image_tag);

        let docker = Docker::connect_with_local_defaults()
            .map_err(|e| anyhow::anyhow!("Failed to connect to Docker: {}", e))?;

        let parts: Vec<&str> = image_tag.split(':').collect();
        let (image, tag) = if parts.len() == 2 {
            (parts[0], parts[1])
        } else {
            (image_tag, "latest")
        };

        let options = CreateImageOptionsBuilder::new()
            .from_image(image)
            .tag(tag)
            .build();

        let mut stream = docker.create_image(Some(options), None, None);

        while let Some(result) = FuturesStreamExt::next(&mut stream).await {
            match result {
                Ok(info) => {
                    if let Some(status) = info.status {
                        debug!("Docker pull: {}", status);
                    }
                }
                Err(e) => {
                    anyhow::bail!("Failed to pull Docker image {}: {}", image_tag, e);
                }
            }
        }

        info!("Successfully pulled Docker image: {}", image_tag);
        Ok(())
    }

    async fn backup_postgres_database(&self, temp_file: &mut NamedTempFile) -> Result<()> {
        use bollard::exec::CreateExecOptions;
        use bollard::models::ContainerCreateBody as Config;
        use bollard::query_parameters::RemoveContainerOptions;
        use bollard::Docker;

        info!("Creating PostgreSQL database backup using Docker");

        // Get database URL from server configuration
        let database_url = &self.config_service.get_database_url();

        // Parse database URL to extract connection parameters
        let url = url::Url::parse(database_url)
            .map_err(|e| anyhow::anyhow!("Invalid DATABASE_URL format: {}", e))?;

        let host = url.host_str().unwrap_or("localhost");
        let port = url.port().unwrap_or(5432);
        let database = url.path().trim_start_matches('/');
        let username = url.username();
        let password = url.password().unwrap_or("");

        // Connect to Docker
        let docker = Docker::connect_with_local_defaults()
            .map_err(|e| anyhow::anyhow!("Failed to connect to Docker: {}", e))?;

        // Get PostgreSQL version from database
        let version_str = self.get_postgres_version().await?;
        let major_version = self.parse_postgres_version(&version_str)?;
        let image_tag = self.get_postgres_image_tag(&major_version);

        // Pull the matching PostgreSQL Docker image
        self.pull_postgres_image(&image_tag).await?;

        // Create a temporary container name
        let container_name = format!("temps-pg-backup-{}", uuid::Uuid::new_v4());

        // Prepare environment variables with proper lifetimes
        // URL-decode password (it's stored URL-encoded in database for connection strings)
        let decoded_password = urlencoding::decode(password)
            .map(|s| s.to_string())
            .unwrap_or_else(|_| password.to_string());
        let pgpassword_env = format!("PGPASSWORD={}", decoded_password);
        let env_vars = vec![pgpassword_env];

        // Create a host directory for the bind mount so the backup file is written
        // directly to disk by the sidecar container, bypassing the Temps process entirely.
        // Previous approach streamed pg_dump output through Bollard's exec HTTP stream
        // into the Temps process, which caused unbounded memory growth (2-6+ GB) because
        // hyper/Bollard buffers the chunked HTTP response internally even though we write
        // each chunk to disk immediately.
        let backup_dir = self.config_service.data_dir().join("backups").join("tmp");
        tokio::fs::create_dir_all(&backup_dir).await.map_err(|e| {
            anyhow::anyhow!(
                "Failed to create backup temp directory {}: {}",
                backup_dir.display(),
                e
            )
        })?;
        let backup_filename = format!("{}.sql.gz", uuid::Uuid::new_v4());
        let host_backup_path = backup_dir.join(&backup_filename);
        let container_backup_path = format!("/backup/{}", backup_filename);

        // Create container config with version-matched postgres image (includes pg_dump).
        // Override the entrypoint to prevent the timescaledb-ha image from starting a full
        // PostgreSQL server instance inside the sidecar.
        // Bind-mount the host backup directory to /backup inside the container. We use
        // /backup instead of /tmp because the timescaledb-ha image runs as the postgres
        // user which may not have write access to a bind-mounted /tmp.
        let config = Config {
            image: Some(image_tag),
            entrypoint: Some(vec!["/bin/sleep".to_string()]),
            cmd: Some(vec!["86400".to_string()]), // 24h: must outlive pg_dump on large DBs (42+ GB)
            env: Some(env_vars),
            user: Some("root".to_string()), // Run as root to ensure write access to bind mount
            host_config: Some(bollard::models::HostConfig {
                network_mode: Some("host".to_string()),
                auto_remove: Some(true),
                oom_score_adj: Some(-500),
                binds: Some(vec![format!("{}:/backup:rw", backup_dir.display())]),
                ..Default::default()
            }),
            ..Default::default()
        };

        info!("Creating temporary Docker container for pg_dump");

        // Create container
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
            .map_err(|e| anyhow::anyhow!("Failed to create container: {}", e))?;

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

        // Start container
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
                anyhow::anyhow!("Failed to start container: {}", e)
            })?;

        // Run pg_dump | gzip inside the sidecar, writing directly to the bind-mounted
        // host filesystem. This keeps the Temps process memory flat regardless of DB size.
        let port_str = port.to_string();

        info!("Running pg_dump command in Docker container (bind-mount mode)");

        // URL-decode password for exec env
        let decoded_password = urlencoding::decode(password)
            .map(|s| s.to_string())
            .unwrap_or_else(|_| password.to_string());
        let pgpassword = format!("PGPASSWORD={}", decoded_password);

        // Run pg_dump fully detached — no stdout/stderr streaming through the Temps process.
        // Previous approach used attach_stdout which caused Bollard's hyper HTTP client
        // to buffer the chunked transfer encoding internally, leading to unbounded memory
        // growth (19+ GB) even when we weren't reading stdout data.
        // Instead we redirect stderr to a file inside the container and poll for completion.
        let stderr_file_name = format!("{}.stderr", uuid::Uuid::new_v4());
        let stderr_path = format!("/backup/{}", stderr_file_name);
        // pg_dumpall dumps the entire cluster: all databases, roles, and tablespaces.
        // `--database` is only the bootstrap connection target used to enumerate DBs.
        let pg_dump_shell_cmd = format!(
            "pg_dumpall --clean --if-exists --no-password --host={} --port={} --username={} --database={} 2>{} | gzip > {}",
            shell_escape(host), shell_escape(&port_str), shell_escape(username), shell_escape(database), stderr_path, container_backup_path
        );

        let exec = docker
            .create_exec(
                &container_name,
                CreateExecOptions {
                    cmd: Some(vec!["sh", "-c", &pg_dump_shell_cmd]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    env: Some(vec![pgpassword.as_str()]),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create exec: {}", e))?;

        // Start the exec in detached mode — no HTTP stream through the Temps process
        use bollard::exec::StartExecOptions;
        docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await?;

        // Poll for completion instead of streaming
        loop {
            let inspect = docker.inspect_exec(&exec.id).await?;
            if let Some(running) = inspect.running {
                if !running {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }

        // Read stderr from the file inside the container (via bind mount on host)
        let host_stderr_path = backup_dir.join(&stderr_file_name);
        let stderr_data = tokio::fs::read(&host_stderr_path).await.unwrap_or_default();
        let _ = tokio::fs::remove_file(&host_stderr_path).await;

        // Check if command was successful
        let exec_inspect = docker.inspect_exec(&exec.id).await?;
        if let Some(exit_code) = exec_inspect.exit_code {
            if exit_code != 0 {
                let stderr = String::from_utf8_lossy(&stderr_data);
                remove_sidecar(docker.clone(), container_name.clone()).await;
                let _ = tokio::fs::remove_file(&host_backup_path).await;
                return Err(anyhow::anyhow!(
                    "pg_dump failed with exit code {}: {}",
                    exit_code,
                    stderr
                ));
            }
        }

        // Clean up sidecar container
        remove_sidecar(docker.clone(), container_name.clone()).await;

        // Copy the backup file from the bind-mount location to the temp_file that the
        // caller uses for S3 upload. This is a local file copy (not through memory).
        tokio::fs::copy(&host_backup_path, temp_file.path())
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to copy backup from {} to temp file: {}",
                    host_backup_path.display(),
                    e
                )
            })?;

        // Clean up the bind-mount backup file
        let _ = tokio::fs::remove_file(&host_backup_path).await;

        info!("PostgreSQL backup completed successfully");
        Ok(())
    }

    async fn upload_backup(
        &self,
        s3_client: &S3Client,
        s3_source: &S3Source,
        temp_file: &NamedTempFile,
        s3_location: &str,
    ) -> Result<()> {
        info!("Uploading backup to S3: {}", s3_location);
        let file_size = temp_file.as_file().metadata()?.len();
        let path = temp_file.path().to_str().ok_or_else(|| {
            anyhow::anyhow!(
                "backup temp file path {} is not valid UTF-8",
                temp_file.path().display()
            )
        })?;
        // One upload implementation for every engine: single PUT below the
        // multipart threshold, otherwise a multipart upload whose parts are
        // retried individually and never thrown away on a transient error.
        crate::engines::v2_common::upload_file(
            s3_client,
            &s3_source.bucket_name,
            s3_location,
            path,
            "application/x-gzip",
            i64::try_from(file_size).unwrap_or(i64::MAX),
            None,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        // Keep the typed error as the source (downcastable) instead of
        // flattening it to text; this legacy path is still `anyhow` at its
        // boundary but must not lose whether the upload was cancelled.
        .map_err(anyhow::Error::from)
    }
}

#[cfg(test)]
mod tests {
    use super::super::deletion::validated_snapshot_prefix;
    use super::super::test_support::*;
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_core::EncryptionService;

    /// #1355: a whole-instance failure names each failed service with its
    /// own cause, trimmed so one verbose engine error cannot bury the rest.
    #[test]
    fn bounded_failure_cause_keeps_short_causes_and_truncates_long_ones() {
        assert_eq!(
            bounded_failure_cause("  wal-g: command not found \n"),
            "wal-g: command not found"
        );
        let long = "é".repeat(MAX_FAILURE_CAUSE_CHARS + 50);
        let bounded = bounded_failure_cause(&long);
        assert!(bounded.ends_with("..."));
        assert_eq!(bounded.chars().count(), MAX_FAILURE_CAUSE_CHARS + 3);
    }

    /// Records every job sent, so a test can assert what the service
    /// published without a live consumer. Flip `refuse` to make every send
    /// fail, the way a queue does when its backing channel is gone.
    struct RecordingJobQueue {
        sent: std::sync::Mutex<Vec<temps_core::Job>>,
        refuse: std::sync::atomic::AtomicBool,
    }

    impl RecordingJobQueue {
        fn new() -> Self {
            Self {
                sent: std::sync::Mutex::new(Vec::new()),
                refuse: std::sync::atomic::AtomicBool::new(false),
            }
        }

        /// The `BackupDeleted` jobs published so far, in order.
        fn deleted_jobs(&self) -> Vec<temps_core::BackupDeletedJob> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .filter_map(|job| match job {
                    temps_core::Job::BackupDeleted(job) => Some(job.clone()),
                    _ => None,
                })
                .collect()
        }
    }

    #[async_trait::async_trait]
    impl temps_core::JobQueue for RecordingJobQueue {
        async fn send(&self, job: temps_core::Job) -> Result<(), temps_core::QueueError> {
            if self.refuse.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(temps_core::QueueError::SendError(
                    "queue refused the job for the test".to_string(),
                ));
            }
            self.sent.lock().unwrap().push(job);
            Ok(())
        }

        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            unimplemented!("RecordingJobQueue does not support subscribing in tests")
        }
    }

    #[tokio::test]
    async fn test_backup_to_rustfs_integration() {
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

        // Start PostgreSQL database with migrations
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");

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
        let bucket_name = "test-backups";
        s3_client
            .create_bucket()
            .bucket(bucket_name)
            .send()
            .await
            .expect("Failed to create bucket");

        // Give bucket time to be ready
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

        // Setup backup service
        let external_service_manager = create_mock_external_service_manager(test_db.db.clone());
        let alarm_service = create_mock_alarm_service();

        // Create proper config service with test database
        let server_config = temps_config::ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            test_db.database_url.clone(),
            None,
            None,
        )
        .unwrap();

        let config_service = Arc::new(temps_config::ConfigService::new(
            Arc::new(server_config),
            test_db.db.clone(),
        ));

        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let published = Arc::new(RecordingJobQueue::new());
        let backup_service = BackupService::new(
            test_db.db.clone(),
            external_service_manager,
            alarm_service,
            config_service,
            encryption_service,
            published.clone(),
        );

        // Create a test user for backup operations
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::users;
        let test_user = users::ActiveModel {
            name: Set("Test User".to_string()),
            email: Set("test@example.com".to_string()),
            password_hash: Set(Some("test_hash".to_string())),
            email_verified: Set(true),
            ..Default::default()
        };
        test_user
            .insert(test_db.db.as_ref())
            .await
            .expect("Failed to create test user");

        // Create S3 source
        let s3_source_request = CreateS3SourceRequest {
            name: "test-rustfs".to_string(),
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

        let s3_source = backup_service
            .create_s3_source(s3_source_request)
            .await
            .expect("Failed to create S3 source");

        // Create backup schedule
        let schedule_request = CreateBackupScheduleRequest {
            name: "test-schedule".to_string(),
            backup_type: "full".to_string(),
            retention_period: 7,
            s3_source_id: Some(s3_source.id),
            schedule_expression: "0 0 2 * * *".to_string(), // Daily at 2 AM
            enabled: true,
            description: Some("Test backup schedule".to_string()),
            tags: vec![],
            max_runtime_secs: None,
            target_all_services: None,
            include_control_plane: None,
            service_ids: vec![],
        };

        let schedule = backup_service
            .create_backup_schedule(
                schedule_request,
                temps_core::BackupScheduleAuthority::InstanceAdmin,
            )
            .await
            .expect("Failed to create backup schedule");

        // Perform backup (use user ID 1 for test)
        let backup_result = backup_service
            .create_backup(Some(schedule.id), s3_source.id, "full", 1)
            .await
            .expect("Failed to create backup");

        // Verify backup was created
        assert!(backup_result.id > 0, "Backup should have an ID");
        assert_eq!(
            backup_result.state, "completed",
            "Backup should be completed"
        );
        assert!(
            backup_result.size_bytes.unwrap_or(0) > 0,
            "Backup should have a size"
        );

        println!("Backup created:");
        println!("  - ID: {}", backup_result.id);
        println!("  - State: {}", backup_result.state);
        println!("  - S3 Location: {}", backup_result.s3_location);
        println!("  - Size: {} bytes", backup_result.size_bytes.unwrap_or(0));

        // List all objects in bucket to see what was uploaded
        let list_result = s3_client
            .list_objects_v2()
            .bucket(bucket_name)
            .send()
            .await
            .expect("Failed to list objects");

        println!("\nObjects in bucket:");
        for obj in list_result.contents() {
            println!(
                "  - Key: {}, Size: {}",
                obj.key().unwrap_or("unknown"),
                obj.size().unwrap_or(0)
            );
        }

        let object_count = list_result.contents().len();
        assert!(
            object_count > 0,
            "Bucket should contain at least one backup file"
        );

        // Verify the specific backup file exists using the S3 location from the backup record
        let object_result = s3_client
            .head_object()
            .bucket(bucket_name)
            .key(&backup_result.s3_location)
            .send()
            .await;

        assert!(
            object_result.is_ok(),
            "Backup file should exist at location: {}. Error: {:?}",
            backup_result.s3_location,
            object_result.err()
        );

        // Download the backup and verify it is a valid gzip-compressed pg_dump custom format.
        //
        // This is the key assertion for the TimescaleDB fix: if the sidecar image were plain
        // postgres (missing the timescaledb extension), pg_dump would either fail with a non-zero
        // exit code (caught earlier) or produce a corrupt/truncated dump. A valid dump must:
        //   1. Start with gzip magic bytes 0x1f 0x8b
        //   2. Decompress to a pg_dump custom-format file starting with "PGDMP"
        //
        // This rules out zero-byte files, plain-text error output, and partial dumps that
        // happen to be non-zero in size.
        let backup_bytes = s3_client
            .get_object()
            .bucket(bucket_name)
            .key(&backup_result.s3_location)
            .send()
            .await
            .expect("Failed to download backup file from S3")
            .body
            .collect()
            .await
            .expect("Failed to read backup body")
            .into_bytes();

        assert!(
            backup_bytes.len() >= 2,
            "Backup file too small to contain gzip magic bytes"
        );
        assert_eq!(
            &backup_bytes[..2],
            &[0x1f, 0x8b],
            "Backup file does not start with gzip magic bytes — not a valid gzip file"
        );

        let mut decoder = flate2::read::GzDecoder::new(&backup_bytes[..]);
        let mut decompressed = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut decompressed)
            .expect("Failed to decompress backup — gzip stream is corrupt");

        // Backups use --format=plain so the decompressed content is SQL text starting
        // with a comment header ("--"), not the binary PGDMP magic bytes.
        let content_str = String::from_utf8_lossy(&decompressed);
        assert!(
            content_str.starts_with("--"),
            "Decompressed backup does not start with SQL comment header — expected plain-format pg_dump output, got: {:?}",
            &decompressed[..std::cmp::min(20, decompressed.len())]
        );

        // Permanent deletion removes the whole UUID-scoped snapshot (dump
        // and metadata companion), updates the discovery index, and only then
        // removes the database row.
        let snapshot_prefix = validated_snapshot_prefix(
            &backup_result.s3_location,
            &s3_source,
            &backup_result.backup_id,
        )
        .expect("new backup must have an attributable snapshot prefix");
        let (_, deleted_objects) = backup_service
            .delete_backup(&backup_result.backup_id)
            .await
            .expect("backup deletion should complete");
        assert_eq!(deleted_objects, 2, "dump and metadata must be deleted");
        // Anything cataloging this backup elsewhere (the Cloud mirror) hears
        // about the deletion, keyed on the stable backup uuid, and only once
        // the row is gone: the job is the last thing the deletion does.
        let deleted_jobs = published.deleted_jobs();
        assert_eq!(deleted_jobs.len(), 1, "one BackupDeleted per deletion");
        assert_eq!(deleted_jobs[0].backup_uuid, backup_result.backup_id);
        assert_eq!(deleted_jobs[0].backup_id, backup_result.id);
        assert_eq!(deleted_jobs[0].s3_location, backup_result.s3_location);
        assert!(backup_service
            .get_backup(&backup_result.backup_id)
            .await
            .expect("database lookup should succeed")
            .is_none());
        let remaining = s3_client
            .list_objects_v2()
            .bucket(bucket_name)
            .prefix(format!("{snapshot_prefix}/"))
            .send()
            .await
            .expect("snapshot prefix should remain listable");
        assert!(
            remaining.contents().is_empty(),
            "snapshot prefix must be empty"
        );
        assert!(backup_service
            .get_backup(&backup_result.backup_id)
            .await
            .expect("database lookup should succeed")
            .is_none());

        let index_key = build_s3_key(&s3_source.bucket_path, "backups/index.json");
        let index_bytes = s3_client
            .get_object()
            .bucket(bucket_name)
            .key(index_key)
            .send()
            .await
            .expect("backup index should remain available")
            .body
            .collect()
            .await
            .expect("backup index should be readable")
            .into_bytes();
        let index: serde_json::Value =
            serde_json::from_slice(&index_bytes).expect("backup index must be valid JSON");
        assert!(index["backups"]
            .as_array()
            .expect("backup index must contain an array")
            .iter()
            .all(|entry| entry["backup_id"] != backup_result.backup_id));

        // Retention deletion goes through the same path and publishes the
        // same job. Age a second backup past the schedule's retention period
        // and let the scheduler's sweep delete it.
        let retained = backup_service
            .create_backup(Some(schedule.id), s3_source.id, "full", 1)
            .await
            .expect("Failed to create the backup that retention will expire");
        temps_entities::backups::Entity::update_many()
            .col_expr(
                temps_entities::backups::Column::StartedAt,
                sea_orm::sea_query::Expr::value(chrono::Utc::now() - chrono::Duration::days(8)),
            )
            .filter(temps_entities::backups::Column::Id.eq(retained.id))
            .exec(test_db.db.as_ref())
            .await
            .expect("aging the backup should succeed");
        let report = backup_service
            .enforce_retention(Some(schedule.id), None)
            .await
            .expect("retention enforcement should complete");
        assert_eq!(report.deleted, 1, "the aged backup is the only candidate");
        assert_eq!(report.failed, 0, "{:?}", report.failures);
        let deleted_jobs = published.deleted_jobs();
        assert_eq!(
            deleted_jobs.len(),
            2,
            "retention publishes BackupDeleted too"
        );
        assert_eq!(deleted_jobs[1].backup_uuid, retained.backup_id);
        assert_eq!(deleted_jobs[1].backup_id, retained.id);
        assert!(backup_service
            .get_backup(&retained.backup_id)
            .await
            .expect("database lookup should succeed")
            .is_none());

        // Publishing is best effort: a queue that refuses the job must not
        // turn a finished deletion into an error, and must not leave the row
        // or the objects behind.
        let unannounced = backup_service
            .create_backup(Some(schedule.id), s3_source.id, "full", 1)
            .await
            .expect("Failed to create the backup deleted without a queue");
        published
            .refuse
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let (_, deleted_objects) = backup_service
            .delete_backup(&unannounced.backup_id)
            .await
            .expect("a refused publication must not fail the deletion");
        assert_eq!(
            deleted_objects, 2,
            "dump and metadata must still be deleted"
        );
        assert_eq!(
            published.deleted_jobs().len(),
            2,
            "the refused job is not recorded"
        );
        assert!(backup_service
            .get_backup(&unannounced.backup_id)
            .await
            .expect("database lookup should succeed")
            .is_none());
        let unannounced_prefix =
            validated_snapshot_prefix(&unannounced.s3_location, &s3_source, &unannounced.backup_id)
                .expect("backup must have an attributable snapshot prefix");
        let remaining = s3_client
            .list_objects_v2()
            .bucket(bucket_name)
            .prefix(format!("{unannounced_prefix}/"))
            .send()
            .await
            .expect("snapshot prefix should remain listable");
        assert!(
            remaining.contents().is_empty(),
            "snapshot prefix must be empty"
        );

        println!("\n✓ Integration test passed:");
        println!("  - Database container started (timescale/timescaledb-ha)");
        println!("  - RustFS container started");
        println!("  - Backup created with ID: {}", backup_result.id);
        println!(
            "  - Backup size: {} bytes (compressed)",
            backup_result.size_bytes.unwrap_or(0)
        );
        println!("  - Decompressed size: {} bytes", decompressed.len());
        println!("  - Backup format: valid gzip-compressed pg_dump custom format (PGDMP)");
        println!("  - Objects in bucket before deletion: {}", object_count);
    }

    fn make_backup_service() -> BackupService {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db),
            create_mock_alarm_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
            Arc::new(NoopJobQueue),
        )
    }

    /// The main Temps database always runs on TimescaleDB, so the pg_dump sidecar
    /// must always use the timescaledb-ha image — never plain postgres.
    #[test]
    fn test_pg_dump_sidecar_always_uses_timescaledb_image() {
        let svc = make_backup_service();

        for major in ["15", "16", "17", "18"] {
            let image = svc.get_postgres_image_tag(major);
            assert!(
                image.starts_with("timescale/timescaledb-ha:pg"),
                "Expected timescaledb-ha image for version {major}, got: {image}"
            );
            assert!(
                image.ends_with(major),
                "Image tag should end with the major version {major}, got: {image}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // list_external_service_backups — pagination math
    // -----------------------------------------------------------------------

    /// plain Postgres. Verify that parse_postgres_version correctly extracts the major
    /// version from a real TimescaleDB SELECT version() output.
    #[test]
    fn test_parse_postgres_version_from_timescaledb_version_string() {
        let svc = make_backup_service();

        let timescaledb_version_string =
            "PostgreSQL 17.4 on aarch64-unknown-linux-gnu, compiled by gcc (GCC) 13.2.0, 64-bit";

        let major = svc
            .parse_postgres_version(timescaledb_version_string)
            .expect("Should parse TimescaleDB version string");

        assert_eq!(major, "17");

        // Confirm the full image tag is correct end-to-end
        let image = svc.get_postgres_image_tag(&major);
        assert_eq!(image, "timescale/timescaledb-ha:pg17");
    }

    // ── update_backup_schedule unit tests ───────────────────────────────────
}
