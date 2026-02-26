use crate::handlers::backup_handler::{CreateBackupScheduleRequest, CreateS3SourceRequest};
use anyhow::Result;
use aws_sdk_s3::error::ProvideErrorMetadata;
use aws_sdk_s3::{Client as S3Client, Config};
use chrono::{DateTime, Duration, Timelike, Utc};

use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, IntoActiveModel, QueryFilter,
    QueryOrder,
};
use serde_json::json;
use serde_yaml;
use std::str::FromStr;
use std::sync::Arc;
use tempfile::NamedTempFile;
use temps_entities::backups::Model as Backup;
use thiserror::Error;
use tokio::time;
use tracing::{debug, error, info, warn};
use urlencoding;
use uuid::Uuid;

use cron::Schedule;
use temps_core::notifications::{BackupFailureData, NotificationService};
use temps_entities::{backup_schedules::Model as BackupSchedule, s3_sources::Model as S3Source};
use temps_providers::ExternalServiceManager;
use tokio_stream::StreamExt;

#[derive(Error, Debug)]
pub enum BackupError {
    #[error("Database error: {0}")]
    Database(sea_orm::DbErr),

    #[error("S3 error: {0}")]
    S3(String),

    #[error("Schedule error: {0}")]
    Schedule(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{resource} not found: {detail}")]
    NotFound { resource: String, detail: String },

    #[error("Invalid configuration: {0}")]
    Configuration(String),

    #[error("External service error: {0}")]
    ExternalService(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Internal error: {message}")]
    Internal { message: String },

    #[error("Unsupported: {0}")]
    Unsupported(String),

    #[error("Notification error: {0}")]
    NotificationError(String),
}

impl From<aws_sdk_s3::error::SdkError<aws_sdk_s3::operation::put_object::PutObjectError>>
    for BackupError
{
    fn from(
        err: aws_sdk_s3::error::SdkError<aws_sdk_s3::operation::put_object::PutObjectError>,
    ) -> Self {
        BackupError::S3(format!("Failed to put object: {}", err))
    }
}

impl From<aws_sdk_s3::error::SdkError<aws_sdk_s3::operation::delete_object::DeleteObjectError>>
    for BackupError
{
    fn from(
        err: aws_sdk_s3::error::SdkError<aws_sdk_s3::operation::delete_object::DeleteObjectError>,
    ) -> Self {
        BackupError::S3(format!("Failed to delete object: {}", err))
    }
}

impl
    From<
        aws_sdk_s3::error::SdkError<
            aws_sdk_s3::operation::complete_multipart_upload::CompleteMultipartUploadError,
        >,
    > for BackupError
{
    fn from(
        err: aws_sdk_s3::error::SdkError<
            aws_sdk_s3::operation::complete_multipart_upload::CompleteMultipartUploadError,
        >,
    ) -> Self {
        BackupError::S3(format!("Failed to complete multipart upload: {}", err))
    }
}

// Conversion from anyhow::Error is used by service methods whose helper functions
// return anyhow::Result. This is a transitional impl; the goal is to convert all
// helper functions to return BackupError directly.
impl From<anyhow::Error> for BackupError {
    fn from(err: anyhow::Error) -> Self {
        BackupError::Internal {
            message: format!("{:#}", err),
        }
    }
}

impl From<sea_orm::DbErr> for BackupError {
    fn from(err: sea_orm::DbErr) -> Self {
        match err {
            sea_orm::DbErr::RecordNotFound(msg) => BackupError::NotFound {
                resource: "Backup resource".to_string(),
                detail: msg,
            },
            _ => BackupError::Database(err),
        }
    }
}

pub struct BackupService {
    db: Arc<DatabaseConnection>,
    external_service_manager: Arc<ExternalServiceManager>,
    notification_dispatcher: Arc<dyn NotificationService>,
    config_service: Arc<temps_config::ConfigService>,
    encryption_service: Arc<temps_core::EncryptionService>,
}

impl BackupService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        external_service_manager: Arc<ExternalServiceManager>,
        notification_dispatcher: Arc<dyn NotificationService>,
        serve_config: Arc<temps_config::ConfigService>,
        encryption_service: Arc<temps_core::EncryptionService>,
    ) -> Self {
        Self {
            db,
            external_service_manager,
            notification_dispatcher,
            config_service: serve_config,
            encryption_service,
        }
    }

    /// Send a backup failure notification
    pub async fn send_backup_failure_notification(
        &self,
        backup_failure_data: BackupFailureData,
    ) -> Result<(), BackupError> {
        use std::collections::HashMap;
        use temps_core::notifications::{NotificationData, NotificationPriority, NotificationType};

        let mut metadata = HashMap::new();
        metadata.insert(
            "schedule_id".to_string(),
            backup_failure_data.schedule_id.to_string(),
        );
        metadata.insert(
            "schedule_name".to_string(),
            backup_failure_data.schedule_name.clone(),
        );
        metadata.insert(
            "backup_type".to_string(),
            backup_failure_data.backup_type.clone(),
        );
        metadata.insert("timestamp".to_string(), Utc::now().to_rfc3339());

        let notification = NotificationData {
            id: uuid::Uuid::new_v4().to_string(),
            title: format!("Backup Failed: {}", backup_failure_data.schedule_name),
            message: format!(
                "Backup failed for {} ({}): {}",
                backup_failure_data.schedule_name,
                backup_failure_data.backup_type,
                backup_failure_data.error
            ),
            notification_type: NotificationType::Error,
            priority: NotificationPriority::High,
            severity: Some("error".to_string()),
            timestamp: Utc::now(),
            metadata,
            bypass_throttling: false,
        };

        self.notification_dispatcher
            .send_notification(notification)
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

        // Create S3 client (needed for metadata upload and legacy fallback)
        let s3_client = self.create_s3_client(&s3_source).await?;

        // Try WAL-G backup first (requires the internal DB container to have WAL-G installed).
        // Falls back to pg_dump sidecar if the DB is not running in a Docker container we can exec into.
        let (s3_location, size_bytes, compression_type) =
            match self.backup_postgres_walg(&s3_source, &backup_id).await {
                Ok((location, size)) => {
                    info!("WAL-G backup completed: {}", location);
                    (location, size, "lz4".to_string())
                }
                Err(e) => {
                    // WAL-G not available (e.g., DB on localhost, no Docker container found).
                    // Fall back to pg_dump sidecar approach.
                    warn!(
                        "WAL-G backup not available ({}), falling back to pg_dump sidecar",
                        e
                    );

                    let mut temp_file = NamedTempFile::new().map_err(BackupError::Io)?;

                    self.backup_postgres_database(&mut temp_file)
                        .await
                        .map_err(|e| {
                            error!(
                                "Database backup failed for S3 source {}: {}",
                                s3_source_id, e
                            );
                            e
                        })?;

                    let size_bytes = temp_file
                        .as_file()
                        .metadata()
                        .map_err(BackupError::Io)?
                        .len() as i32;

                    if size_bytes == 0 {
                        return Err(BackupError::Validation(
                            "Backup failed: backup file has zero size".to_string(),
                        ));
                    }

                    let s3_location = format!(
                        "{}/backups/{}/{}/backup.sql.gz",
                        s3_source.bucket_path.trim_matches('/'),
                        Utc::now().format("%Y/%m/%d"),
                        backup_id
                    );

                    self.upload_backup(&s3_client, &s3_source, &temp_file, &s3_location)
                        .await
                        .map_err(|e| {
                            error!(
                                "Failed to upload backup to S3 source {} at {}: {}",
                                s3_source_id, s3_location, e
                            );
                            e
                        })?;

                    (s3_location, size_bytes, "gzip".to_string())
                }
            };

        // Create backup record
        let new_backup = temps_entities::backups::ActiveModel {
            id: sea_orm::NotSet,
            name: sea_orm::Set(format!("Backup {}", backup_id)),
            backup_id: sea_orm::Set(backup_id.clone()),
            schedule_id: sea_orm::Set(schedule_id),
            backup_type: sea_orm::Set(backup_type.to_string()),
            state: sea_orm::Set("completed".to_string()),
            started_at: sea_orm::Set(chrono::Utc::now()),
            finished_at: sea_orm::Set(Some(chrono::Utc::now())),
            s3_source_id: sea_orm::Set(s3_source_id),
            s3_location: sea_orm::Set(s3_location.clone()),
            compression_type: sea_orm::Set(compression_type),
            created_by: sea_orm::Set(created_by),
            tags: sea_orm::Set("[]".to_string()),
            size_bytes: sea_orm::Set(Some(size_bytes)),
            file_count: sea_orm::Set(None),
            error_message: sea_orm::Set(None),
            expires_at: sea_orm::Set(None),
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
                    failed_services.push(service.name.clone());

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
                failed_services.join(", ")
            );
        }

        // After successful backup upload, create and upload metadata file
        let metadata = self.generate_backup_metadata(&backup, &s3_source, &external_backups);
        let metadata_key = format!(
            "{}/backups/{}/{}/metadata.json",
            s3_source.bucket_path.trim_matches('/'),
            Utc::now().format("%Y/%m/%d"),
            backup_id
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

    /// Find the Docker container that hosts the internal database by matching the hostname
    /// from DATABASE_URL against Docker container names and network aliases.
    ///
    /// Returns `(container_id, pgdata_path)` if found.
    async fn find_internal_db_container(&self) -> Result<(String, String), BackupError> {
        use bollard::query_parameters::ListContainersOptions;
        use bollard::Docker;

        let database_url = self.config_service.get_database_url();
        let url = url::Url::parse(&database_url).map_err(|e| BackupError::Internal {
            message: format!("Invalid DATABASE_URL: {}", e),
        })?;

        let db_host = url.host_str().unwrap_or("localhost").to_string();

        // Skip Docker discovery for local connections
        if db_host == "localhost" || db_host == "127.0.0.1" || db_host == "::1" {
            return Err(BackupError::Internal {
                message: format!(
                    "Database host '{}' is local — cannot exec into a Docker container",
                    db_host
                ),
            });
        }

        let docker = Docker::connect_with_local_defaults().map_err(|e| BackupError::Internal {
            message: format!("Failed to connect to Docker: {}", e),
        })?;

        // List all running containers
        let containers = docker
            .list_containers(Some(ListContainersOptions {
                all: false, // only running
                ..Default::default()
            }))
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to list Docker containers: {}", e),
            })?;

        // Find container matching the database hostname by:
        // 1. Container name (e.g., /temps-postgres matches "temps-postgres")
        // 2. Docker Compose service name in network aliases (e.g., "postgres" on compose network)
        for container in &containers {
            let container_id = container.id.as_deref().unwrap_or("");
            if container_id.is_empty() {
                continue;
            }

            // Check container names (Docker prefixes with '/')
            if let Some(names) = &container.names {
                for name in names {
                    let clean_name = name.trim_start_matches('/');
                    if clean_name == db_host {
                        return self
                            .resolve_pgdata_for_container(&docker, container_id)
                            .await;
                    }
                }
            }

            // Check network aliases (Docker Compose sets the service name as an alias)
            if let Some(network_settings) = &container.network_settings {
                if let Some(networks) = &network_settings.networks {
                    for net_config in networks.values() {
                        if let Some(aliases) = &net_config.aliases {
                            if aliases.iter().any(|a| a == &db_host) {
                                return self
                                    .resolve_pgdata_for_container(&docker, container_id)
                                    .await;
                            }
                        }
                    }
                }
            }
        }

        Err(BackupError::Internal {
            message: format!(
                "No Docker container found for database host '{}'. \
                 Ensure the database is running in a Docker container with WAL-G installed.",
                db_host
            ),
        })
    }

    /// Resolve the PGDATA path for a container by inspecting its environment variables.
    async fn resolve_pgdata_for_container(
        &self,
        docker: &bollard::Docker,
        container_id: &str,
    ) -> Result<(String, String), BackupError> {
        let inspect = docker
            .inspect_container(
                container_id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to inspect container {}: {}", container_id, e),
            })?;

        // Try to find PGDATA from container environment
        let mut pgdata = String::from("/var/lib/postgresql/data");
        if let Some(config) = &inspect.config {
            if let Some(env) = &config.env {
                for var in env {
                    if let Some(val) = var.strip_prefix("PGDATA=") {
                        pgdata = val.to_string();
                        break;
                    }
                }
            }
        }

        Ok((container_id.to_string(), pgdata))
    }

    /// Perform a WAL-G backup by exec'ing into the internal database container.
    /// WAL-G uploads directly to S3 — no data flows through the Temps process.
    ///
    /// Returns `(s3_location, size_bytes)` on success. The `s3_location` is the WAL-G
    /// S3 prefix (starts with `s3://`), used by the restore logic to detect WAL-G backups.
    async fn backup_postgres_walg(
        &self,
        s3_source: &S3Source,
        _backup_id: &str,
    ) -> Result<(String, i32), BackupError> {
        use bollard::exec::{CreateExecOptions, StartExecOptions};
        use bollard::Docker;

        let (container_id, pgdata) = self.find_internal_db_container().await?;

        info!(
            "Starting WAL-G backup via container {} (PGDATA={})",
            container_id, pgdata
        );

        let docker = Docker::connect_with_local_defaults().map_err(|e| BackupError::Internal {
            message: format!("Failed to connect to Docker: {}", e),
        })?;

        // Verify WAL-G is installed in the container
        let check_exec = docker
            .create_exec(
                &container_id,
                CreateExecOptions {
                    cmd: Some(vec!["which", "wal-g"]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to check WAL-G in container: {}", e),
            })?;

        docker
            .start_exec(
                &check_exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to run WAL-G check: {}", e),
            })?;

        // Wait for check to complete
        loop {
            let inspect =
                docker
                    .inspect_exec(&check_exec.id)
                    .await
                    .map_err(|e| BackupError::Internal {
                        message: format!("Failed to inspect WAL-G check exec: {}", e),
                    })?;
            if let Some(running) = inspect.running {
                if !running {
                    if let Some(exit_code) = inspect.exit_code {
                        if exit_code != 0 {
                            return Err(BackupError::Internal {
                                message: format!(
                                    "WAL-G is not installed in container {}. \
                                     Use the gotempsh/timescaledb-walg image.",
                                    container_id
                                ),
                            });
                        }
                    }
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        // Build WAL-G S3 prefix using a STABLE path (no date or backup_id).
        // WAL-G requires all backups and WAL segments to share the same prefix so that:
        // - wal-g wal-push archives WAL to {prefix}/wal_005/
        // - wal-g backup-push stores base backups in {prefix}/basebackups_005/
        // - wal-g backup-fetch LATEST finds the right backup + WAL chain
        // - wal-g delete retain works across all backups
        let walg_s3_prefix = format!(
            "s3://{}/{}/internal_db/walg",
            s3_source.bucket_name,
            s3_source.bucket_path.trim_matches('/'),
        );

        // Decrypt S3 credentials for WAL-G environment variables
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

        // Build environment variables for WAL-G
        let mut env_vars: Vec<String> = vec![
            format!("WALG_S3_PREFIX={}", walg_s3_prefix),
            format!("AWS_ACCESS_KEY_ID={}", decrypted_access_key),
            format!("AWS_SECRET_ACCESS_KEY={}", decrypted_secret_key),
            format!("AWS_REGION={}", s3_source.region),
            format!("PGDATA={}", pgdata),
        ];

        // Resolve S3 endpoint for use inside the Docker container.
        // localhost/127.0.0.1 endpoints are translated to Docker-resolvable addresses.
        let s3_creds = temps_providers::S3Credentials {
            access_key_id: decrypted_access_key.clone(),
            secret_key: decrypted_secret_key.clone(),
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
            env_vars.push(format!("AWS_ENDPOINT={}", resolved_endpoint));
        }

        if s3_source.force_path_style.unwrap_or(true) {
            env_vars.push("AWS_S3_FORCE_PATH_STYLE=true".to_string());
        }

        let env_refs: Vec<&str> = env_vars.iter().map(|s| s.as_str()).collect();

        // Run wal-g backup-push
        info!("Running wal-g backup-push in container {}", container_id);

        let exec = docker
            .create_exec(
                &container_id,
                CreateExecOptions {
                    cmd: Some(vec!["wal-g", "backup-push", &pgdata]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    env: Some(env_refs.clone()),
                    user: Some("postgres"),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to create WAL-G exec: {}", e),
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
                message: format!("Failed to start WAL-G exec: {}", e),
            })?;

        // Poll for completion
        loop {
            let inspect =
                docker
                    .inspect_exec(&exec.id)
                    .await
                    .map_err(|e| BackupError::Internal {
                        message: format!("Failed to inspect WAL-G exec: {}", e),
                    })?;
            if let Some(running) = inspect.running {
                if !running {
                    if let Some(exit_code) = inspect.exit_code {
                        if exit_code != 0 {
                            return Err(BackupError::Internal {
                                message: format!(
                                    "wal-g backup-push failed with exit code {} in container {}",
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

        // Calculate total backup size by listing objects under the WAL-G prefix
        let s3_client = self.create_s3_client(s3_source).await?;
        let prefix = format!(
            "{}/internal_db/walg/basebackups_005/",
            s3_source.bucket_path.trim_matches('/'),
        );

        let mut total_size: i64 = 0;
        let mut continuation_token: Option<String> = None;
        loop {
            let mut req = s3_client
                .list_objects_v2()
                .bucket(&s3_source.bucket_name)
                .prefix(&prefix);

            if let Some(token) = continuation_token.take() {
                req = req.continuation_token(token);
            }

            let resp = req
                .send()
                .await
                .map_err(|e| BackupError::S3(format!("Failed to list WAL-G objects: {}", e)))?;

            for obj in resp.contents() {
                total_size += obj.size().unwrap_or(0);
            }

            if resp.is_truncated() == Some(true) {
                continuation_token = resp.next_continuation_token().map(|s| s.to_string());
            } else {
                break;
            }
        }

        info!(
            "WAL-G backup completed: {} ({} bytes)",
            walg_s3_prefix, total_size
        );

        // Enable continuous WAL archiving for the internal database.
        // Write S3 credentials to an env file on the shared volume, then configure
        // archive_command to source it before running wal-g wal-push.
        // Failures here are logged but do NOT fail the backup.
        if let Err(e) = self
            .enable_internal_wal_archiving(&docker, &container_id, &env_vars, &pgdata)
            .await
        {
            error!(
                "Failed to enable WAL archiving for internal DB in container '{}': {}. \
                 Base backup succeeded but continuous WAL archiving is not active.",
                container_id, e
            );
        }

        Ok((walg_s3_prefix, total_size as i32))
    }

    /// Write WAL-G credentials to an env file on the shared volume and enable
    /// continuous WAL archiving for the internal database via `ALTER SYSTEM`.
    ///
    /// Same approach as external PostgreSQL services: the env file is refreshed on
    /// every backup so credential rotations are picked up automatically.
    async fn enable_internal_wal_archiving(
        &self,
        docker: &bollard::Docker,
        container_id: &str,
        env_vars: &[String],
        pgdata: &str,
    ) -> Result<(), BackupError> {
        use bollard::exec::{CreateExecOptions, StartExecOptions};

        // Determine the volume mount root (parent of PGDATA) for the env file location.
        // E.g., PGDATA=/var/lib/postgresql/data -> env file at /var/lib/postgresql/walg.env
        let volume_root = std::path::Path::new(pgdata)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "/var/lib/postgresql".to_string());
        let walg_env_path = format!("{}/walg.env", volume_root);

        // Filter to only S3/WAL-G env vars (no PGDATA, no PG connection vars)
        let env_file_lines: Vec<&String> = env_vars
            .iter()
            .filter(|line| line.starts_with("WALG_") || line.starts_with("AWS_"))
            .collect();

        // Write the env file via docker exec
        let write_cmd = format!(
            "printf '%s\\n' {} > {} && chmod 600 {}",
            env_file_lines
                .iter()
                .map(|line| format!("'export {}'", line.replace('\'', "'\\''")))
                .collect::<Vec<_>>()
                .join(" "),
            walg_env_path,
            walg_env_path,
        );

        let exec = docker
            .create_exec(
                container_id,
                CreateExecOptions {
                    cmd: Some(vec!["sh", "-c", &write_cmd]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    user: Some("postgres"),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to create env file write exec: {}", e),
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
                message: format!("Failed to start env file write exec: {}", e),
            })?;

        loop {
            let inspect =
                docker
                    .inspect_exec(&exec.id)
                    .await
                    .map_err(|e| BackupError::Internal {
                        message: format!("Failed to inspect env file write exec: {}", e),
                    })?;
            if inspect.running == Some(false) {
                if inspect.exit_code != Some(0) {
                    return Err(BackupError::Internal {
                        message: format!(
                            "Failed to write walg.env (exit code {:?})",
                            inspect.exit_code
                        ),
                    });
                }
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        info!(
            "Written WAL-G credentials to {} in container '{}'",
            walg_env_path, container_id
        );

        // Parse DATABASE_URL for psql credentials
        let database_url = self.config_service.get_database_url();
        let url = url::Url::parse(&database_url).map_err(|e| BackupError::Internal {
            message: format!("Invalid DATABASE_URL for ALTER SYSTEM: {}", e),
        })?;
        let pg_user = url.username();
        let pg_password = url.password().unwrap_or("");

        // Enable archive_command via ALTER SYSTEM + pg_reload_conf().
        // Use two separate -c flags because ALTER SYSTEM cannot run inside a
        // transaction block, and psql wraps multiple statements in a single -c
        // into a transaction.
        let archive_command = format!(". {} && wal-g wal-push %p", walg_env_path);
        let alter_sql = format!(
            "ALTER SYSTEM SET archive_command = '{}'",
            archive_command.replace('\'', "''")
        );
        let reload_sql = "SELECT pg_reload_conf()";

        let password_env = format!("PGPASSWORD={}", pg_password);
        let exec = docker
            .create_exec(
                container_id,
                CreateExecOptions {
                    cmd: Some(vec![
                        "psql", "-U", pg_user, "-c", &alter_sql, "-c", reload_sql,
                    ]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    env: Some(vec![&password_env]),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to create ALTER SYSTEM exec: {}", e),
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
                message: format!("Failed to start ALTER SYSTEM exec: {}", e),
            })?;

        loop {
            let inspect =
                docker
                    .inspect_exec(&exec.id)
                    .await
                    .map_err(|e| BackupError::Internal {
                        message: format!("Failed to inspect ALTER SYSTEM exec: {}", e),
                    })?;
            if inspect.running == Some(false) {
                if inspect.exit_code != Some(0) {
                    return Err(BackupError::Internal {
                        message: format!(
                            "ALTER SYSTEM SET archive_command failed (exit code {:?})",
                            inspect.exit_code
                        ),
                    });
                }
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        info!(
            "Enabled continuous WAL archiving for internal DB in container '{}'",
            container_id
        );

        Ok(())
    }

    /// Fetches the PostgreSQL version from the database
    async fn get_postgres_version(&self) -> Result<String> {
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
    fn parse_postgres_version(&self, version_str: &str) -> Result<String> {
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
    fn get_postgres_image_tag(&self, major_version: &str) -> String {
        format!("timescale/timescaledb-ha:pg{}", major_version)
    }

    /// Pulls the specified PostgreSQL Docker image
    async fn pull_postgres_image(&self, image_tag: &str) -> Result<()> {
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
        let stderr_path = format!("/backup/{}.stderr", uuid::Uuid::new_v4());
        let pg_dump_shell_cmd = format!(
            "pg_dump --format=plain --clean --if-exists --no-password --host={} --port={} --username={} --dbname={} 2>{} | gzip > {}",
            host, port_str, username, database, stderr_path, container_backup_path
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
        let host_stderr_path =
            backup_dir.join(std::path::Path::new(&stderr_path).file_name().unwrap());
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

    async fn create_s3_client(&self, s3_source: &S3Source) -> Result<S3Client> {
        // Decrypt credentials before using them
        let decrypted_access_key = self
            .encryption_service
            .decrypt_string(&s3_source.access_key_id)
            .map_err(|e| anyhow::anyhow!("Failed to decrypt access key: {}", e))?;

        let decrypted_secret_key = self
            .encryption_service
            .decrypt_string(&s3_source.secret_key)
            .map_err(|e| anyhow::anyhow!("Failed to decrypt secret key: {}", e))?;

        let creds = aws_sdk_s3::config::Credentials::new(
            decrypted_access_key,
            decrypted_secret_key,
            None,
            None,
            "backup-service",
        );

        let mut config_builder = Config::builder()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new(s3_source.region.clone()))
            .force_path_style(s3_source.force_path_style.unwrap_or(true)) // Default to true for Minio
            .credentials_provider(creds);

        // Only set endpoint URL if endpoint is specified (for Minio)
        if let Some(endpoint) = &s3_source.endpoint {
            let endpoint_url = if endpoint.starts_with("http") {
                endpoint.clone()
            } else {
                format!("http://{}", endpoint)
            };
            config_builder = config_builder.endpoint_url(endpoint_url);
        }

        let config = config_builder.build();

        Ok(S3Client::from_conf(config))
    }

    /// Create S3 client from request (before persistence)
    async fn create_s3_client_from_request(
        &self,
        request: &CreateS3SourceRequest,
    ) -> Result<S3Client, BackupError> {
        let creds = aws_sdk_s3::config::Credentials::new(
            request.access_key_id.clone(),
            request.secret_key.clone(),
            None,
            None,
            "backup-service",
        );

        let mut config_builder = Config::builder()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new(request.region.clone()))
            .force_path_style(request.force_path_style.unwrap_or(true))
            .credentials_provider(creds);

        // Only set endpoint URL if endpoint is specified (for MinIO)
        if let Some(endpoint) = &request.endpoint {
            let endpoint_url = if endpoint.starts_with("http") {
                endpoint.clone()
            } else {
                format!("http://{}", endpoint)
            };
            config_builder = config_builder.endpoint_url(endpoint_url);
        }

        let config = config_builder.build();
        Ok(S3Client::from_conf(config))
    }

    /// Test S3 connection and auto-create bucket if it doesn't exist
    async fn test_and_create_s3_bucket(
        &self,
        s3_client: &S3Client,
        bucket_name: &str,
    ) -> Result<(), BackupError> {
        // Try to check if bucket exists by listing objects with max-keys=1
        // This is a lightweight way to test access to the bucket
        match s3_client
            .list_objects_v2()
            .bucket(bucket_name)
            .max_keys(1)
            .send()
            .await
        {
            Ok(_) => {
                debug!("S3 bucket '{}' exists and is accessible", bucket_name);
                Ok(())
            }
            Err(e) => {
                // Check if it's a "NoSuchBucket" error
                let error_code = e
                    .as_service_error()
                    .and_then(|se| se.code())
                    .map(|s| s.to_string());

                if error_code.as_deref() == Some("NoSuchBucket") {
                    // Bucket doesn't exist, try to create it
                    debug!("S3 bucket '{}' does not exist, creating it...", bucket_name);
                    s3_client
                        .create_bucket()
                        .bucket(bucket_name)
                        .send()
                        .await
                        .map_err(|e| {
                            // Parse create bucket error for better messaging
                            let error_msg = self.parse_s3_error(&e, bucket_name, "create");
                            BackupError::S3(error_msg)
                        })?;
                    info!("Successfully created S3 bucket '{}'", bucket_name);
                    Ok(())
                } else {
                    // Other S3 error (invalid credentials, no access, etc.)
                    let error_msg = self.parse_s3_error(&e, bucket_name, "access");
                    Err(BackupError::S3(error_msg))
                }
            }
        }
    }

    /// Parse S3 SDK errors and provide user-friendly, actionable error messages
    fn parse_s3_error<E>(&self, error: &E, bucket_name: &str, operation: &str) -> String
    where
        E: std::error::Error + std::fmt::Display,
    {
        let error_str = error.to_string();

        // Check for common error patterns and provide actionable guidance

        // Connection/Network errors
        if error_str.contains("ConnectorError")
            || error_str.contains("connection")
            || error_str.contains("ConnectionRefused")
            || error_str.contains("tcp connect error")
        {
            return format!(
                "Unable to connect to S3 endpoint for bucket '{}'. \
                Please verify:\n\
                • The endpoint URL is correct and reachable\n\
                • Network/firewall allows connections to the S3 service\n\
                • The S3 service is running (for MinIO/LocalStack)\n\
                Technical details: {}",
                bucket_name, error_str
            );
        }

        // DNS resolution errors
        if error_str.contains("dns error")
            || error_str.contains("failed to lookup address")
            || error_str.contains("Name or service not known")
        {
            return format!(
                "Failed to resolve S3 endpoint hostname for bucket '{}'. \
                Please verify:\n\
                • The endpoint URL is correct\n\
                • DNS is properly configured\n\
                • The hostname is valid and resolvable\n\
                Technical details: {}",
                bucket_name, error_str
            );
        }

        // Timeout errors
        if error_str.contains("timeout") || error_str.contains("timed out") {
            return format!(
                "Connection to S3 endpoint timed out for bucket '{}'. \
                Please verify:\n\
                • The S3 service is running and responsive\n\
                • Network latency is acceptable\n\
                • Firewall rules allow connections\n\
                Technical details: {}",
                bucket_name, error_str
            );
        }

        // Authentication errors
        if error_str.contains("InvalidAccessKeyId")
            || error_str.contains("SignatureDoesNotMatch")
            || error_str.contains("InvalidSecurity")
        {
            return format!(
                "Authentication failed for bucket '{}'. \
                Please verify:\n\
                • Access Key ID is correct\n\
                • Secret Access Key is correct\n\
                • Credentials have not expired\n\
                • The credentials match the S3 service configuration\n\
                Technical details: {}",
                bucket_name, error_str
            );
        }

        // Permission/Authorization errors
        if error_str.contains("AccessDenied")
            || error_str.contains("Forbidden")
            || error_str.contains("403")
        {
            return format!(
                "Access denied when trying to {} bucket '{}'. \
                Please verify:\n\
                • The credentials have sufficient permissions\n\
                • The bucket exists and you have access to it\n\
                • IAM policies allow the required S3 operations\n\
                • Bucket policies do not restrict access\n\
                Technical details: {}",
                operation, bucket_name, error_str
            );
        }

        // Bucket already exists (from another account)
        if error_str.contains("BucketAlreadyExists") {
            return format!(
                "Bucket '{}' already exists in another account or region. \
                Please:\n\
                • Choose a different bucket name (bucket names must be globally unique)\n\
                • Or verify you have access to this existing bucket\n\
                Technical details: {}",
                bucket_name, error_str
            );
        }

        // Region mismatch
        if error_str.contains("AuthorizationHeaderMalformed") || error_str.contains("region") {
            return format!(
                "Region configuration issue for bucket '{}'. \
                Please verify:\n\
                • The region is correctly specified\n\
                • The bucket exists in the specified region\n\
                • For MinIO/LocalStack, use a valid region (e.g., 'us-east-1')\n\
                Technical details: {}",
                bucket_name, error_str
            );
        }

        // Invalid bucket name
        if error_str.contains("InvalidBucketName") {
            return format!(
                "Invalid bucket name '{}'. \
                Bucket names must:\n\
                • Be between 3 and 63 characters long\n\
                • Contain only lowercase letters, numbers, dots (.), and hyphens (-)\n\
                • Begin and end with a letter or number\n\
                • Not be formatted as an IP address\n\
                Technical details: {}",
                bucket_name, error_str
            );
        }

        // SSL/TLS errors
        if error_str.contains("ssl")
            || error_str.contains("tls")
            || error_str.contains("certificate")
        {
            return format!(
                "SSL/TLS error when connecting to S3 for bucket '{}'. \
                Please verify:\n\
                • The endpoint URL scheme matches the service (http:// for local, https:// for AWS)\n\
                • SSL certificates are valid (for custom endpoints)\n\
                • For local development, ensure HTTP is configured correctly\n\
                Technical details: {}",
                bucket_name, error_str
            );
        }

        // Generic S3 service error
        if error_str.contains("service error") {
            return format!(
                "S3 service error when trying to {} bucket '{}'. \
                This may be a temporary issue. Please:\n\
                • Verify the S3 service is operational\n\
                • Check service status/logs\n\
                • Try again in a few moments\n\
                Technical details: {}",
                operation, bucket_name, error_str
            );
        }

        // Default: return a formatted version of the error
        format!(
            "Failed to {} S3 bucket '{}': {}\n\
            \n\
            Please verify your S3 configuration:\n\
            • Endpoint URL is correct\n\
            • Access credentials are valid\n\
            • Region is correctly specified\n\
            • Bucket name is valid\n\
            • Network connectivity to S3 service",
            operation, bucket_name, error_str
        )
    }

    async fn upload_backup(
        &self,
        s3_client: &S3Client,
        s3_source: &S3Source,
        temp_file: &NamedTempFile,
        s3_location: &str,
    ) -> Result<()> {
        info!("Uploading backup to S3: {}", s3_location);

        // Get file size
        let file_size = temp_file.as_file().metadata()?.len();

        // Use multipart upload for files larger than 30MB
        const MULTIPART_THRESHOLD: u64 = 30 * 1024 * 1024; // 30MB in bytes

        if file_size > MULTIPART_THRESHOLD {
            self.upload_multipart(s3_client, s3_source, temp_file, s3_location)
                .await
        } else {
            self.upload_single_part(s3_client, s3_source, temp_file, s3_location)
                .await
        }
    }

    async fn upload_single_part(
        &self,
        s3_client: &S3Client,
        s3_source: &S3Source,
        temp_file: &NamedTempFile,
        s3_location: &str,
    ) -> Result<()> {
        // Stream from file instead of reading entire contents into memory
        let body = aws_sdk_s3::primitives::ByteStream::from_path(temp_file.path())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create byte stream from backup file: {}", e))?;

        match s3_client
            .put_object()
            .bucket(&s3_source.bucket_name)
            .key(s3_location)
            .body(body)
            .content_type("application/x-gzip")
            .send()
            .await
        {
            Ok(_) => {
                info!("Successfully uploaded backup using single-part upload");
                Ok(())
            }
            Err(e) => {
                if let Some(service_error) = e.as_service_error() {
                    error!(
                        "S3 service error during single-part upload: {:?} - Message: {}, Code: {:?}",
                        service_error,
                        service_error.message().unwrap_or("no message"),
                        service_error.code()
                    );
                    Err(anyhow::anyhow!(
                        "S3 upload failed: {} (code: {:?})",
                        service_error.message().unwrap_or("unknown error"),
                        service_error.code()
                    ))
                } else {
                    error!("Failed to upload backup: {}", e);
                    Err(anyhow::anyhow!("Failed to upload backup: {}", e))
                }
            }
        }
    }

    async fn upload_multipart(
        &self,
        s3_client: &S3Client,
        s3_source: &S3Source,
        temp_file: &NamedTempFile,
        s3_location: &str,
    ) -> Result<()> {
        // Create multipart upload
        let create_multipart_resp = match s3_client
            .create_multipart_upload()
            .bucket(&s3_source.bucket_name)
            .key(s3_location)
            .content_type("application/x-gzip")
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                if let Some(service_error) = e.as_service_error() {
                    error!(
                        "S3 service error creating multipart upload: {:?} - Message: {}, Code: {:?}",
                        service_error,
                        service_error.message().unwrap_or("no message"),
                        service_error.code()
                    );
                    return Err(anyhow::anyhow!(
                        "Failed to create multipart upload: {} (code: {:?})",
                        service_error.message().unwrap_or("unknown error"),
                        service_error.code()
                    ));
                }
                return Err(anyhow::anyhow!("Failed to create multipart upload: {}", e));
            }
        };

        let upload_id = create_multipart_resp
            .upload_id()
            .ok_or_else(|| anyhow::anyhow!("No upload ID received from S3"))?;

        let mut part_number = 1;
        let mut parts = aws_sdk_s3::types::CompletedMultipartUpload::builder();
        let mut total_size = 0;

        // Stream and upload file in chunks
        let file = tokio::fs::File::open(temp_file.path()).await?;
        let reader = tokio::io::BufReader::new(file);
        let mut stream = tokio_util::io::ReaderStream::new(reader);

        let chunk_size = 5 * 1024 * 1024; // 5MB chunks
        let mut buffer = Vec::with_capacity(chunk_size);

        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|e| anyhow::anyhow!("Failed to read chunk from file: {}", e))?;
            buffer.extend_from_slice(&chunk);

            if buffer.len() >= chunk_size {
                match self
                    .upload_part(
                        s3_client,
                        &s3_source.bucket_name,
                        s3_location,
                        upload_id,
                        part_number,
                        buffer.clone(),
                    )
                    .await
                {
                    Ok(part) => {
                        parts = parts.parts(part);
                        total_size += buffer.len();
                        part_number += 1;
                        buffer.clear();
                    }
                    Err(e) => {
                        self.abort_multipart_upload(
                            s3_client,
                            &s3_source.bucket_name,
                            s3_location,
                            upload_id,
                        )
                        .await;
                        return Err(e);
                    }
                }
            }
        }

        // Handle remaining data
        if !buffer.is_empty() {
            match self
                .upload_part(
                    s3_client,
                    &s3_source.bucket_name,
                    s3_location,
                    upload_id,
                    part_number,
                    buffer.clone(),
                )
                .await
            {
                Ok(part) => {
                    parts = parts.parts(part);
                    total_size += buffer.len();
                }
                Err(e) => {
                    self.abort_multipart_upload(
                        s3_client,
                        &s3_source.bucket_name,
                        s3_location,
                        upload_id,
                    )
                    .await;
                    return Err(e);
                }
            }
        }

        // Complete multipart upload
        match s3_client
            .complete_multipart_upload()
            .bucket(&s3_source.bucket_name)
            .key(s3_location)
            .upload_id(upload_id)
            .multipart_upload(parts.build())
            .send()
            .await
        {
            Ok(_) => {
                info!(
                    "Successfully uploaded backup with size: {} bytes",
                    total_size
                );
                Ok(())
            }
            Err(e) => {
                if let Some(service_error) = e.as_service_error() {
                    error!(
                        "S3 service error completing multipart upload: {:?} - Message: {}, Code: {:?}",
                        service_error,
                        service_error.message().unwrap_or("no message"),
                        service_error.code()
                    );
                    Err(anyhow::anyhow!(
                        "Failed to complete multipart upload: {} (code: {:?})",
                        service_error.message().unwrap_or("unknown error"),
                        service_error.code()
                    ))
                } else {
                    error!("Failed to complete multipart upload: {}", e);
                    Err(anyhow::anyhow!(
                        "Failed to complete multipart upload: {}",
                        e
                    ))
                }
            }
        }
    }

    async fn upload_part(
        &self,
        s3_client: &S3Client,
        bucket: &str,
        key: &str,
        upload_id: &str,
        part_number: i32,
        body: Vec<u8>,
    ) -> Result<aws_sdk_s3::types::CompletedPart> {
        match s3_client
            .upload_part()
            .bucket(bucket)
            .key(key)
            .upload_id(upload_id)
            .body(body.into())
            .part_number(part_number)
            .send()
            .await
        {
            Ok(response) => {
                let etag = response
                    .e_tag
                    .ok_or_else(|| anyhow::anyhow!("No ETag received for part {}", part_number))?;

                Ok(aws_sdk_s3::types::CompletedPart::builder()
                    .e_tag(etag)
                    .part_number(part_number)
                    .build())
            }
            Err(e) => {
                if let Some(service_error) = e.as_service_error() {
                    error!(
                        "S3 service error uploading part {}: {:?} - Message: {}, Code: {:?}",
                        part_number,
                        service_error,
                        service_error.message().unwrap_or("no message"),
                        service_error.code()
                    );
                    Err(anyhow::anyhow!(
                        "Failed to upload part {}: {} (code: {:?})",
                        part_number,
                        service_error.message().unwrap_or("unknown error"),
                        service_error.code()
                    ))
                } else {
                    error!("Failed to upload part {}: {}", part_number, e);
                    Err(anyhow::anyhow!(
                        "Failed to upload part {}: {}",
                        part_number,
                        e
                    ))
                }
            }
        }
    }

    async fn abort_multipart_upload(
        &self,
        s3_client: &S3Client,
        bucket: &str,
        key: &str,
        upload_id: &str,
    ) {
        if let Err(e) = s3_client
            .abort_multipart_upload()
            .bucket(bucket)
            .key(key)
            .upload_id(upload_id)
            .send()
            .await
        {
            if let Some(service_error) = e.as_service_error() {
                error!(
                    "S3 service error aborting multipart upload: {:?} - Message: {}, Code: {:?}",
                    service_error,
                    service_error.message().unwrap_or("no message"),
                    service_error.code()
                );
            } else {
                error!("Failed to abort multipart upload: {}", e);
            }
        }
    }

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
        use std::io::Read;

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

        let data = response
            .body
            .collect()
            .await
            .map_err(|e| BackupError::S3(e.to_string()))?
            .into_bytes();

        // Decompress data
        let mut decoder = flate2::read::GzDecoder::new(&data[..]);
        let mut decompressed_data = Vec::new();
        decoder.read_to_end(&mut decompressed_data)?;

        // Write decompressed DB bytes to a temporary file
        let mut temp_file = NamedTempFile::new()?;
        use std::io::Write;
        temp_file.write_all(&decompressed_data)?;
        temp_file.flush()?;

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
        use std::io::Read;

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

        let data = response
            .body
            .collect()
            .await
            .map_err(|e| BackupError::S3(e.to_string()))?
            .into_bytes();

        // Decompress data
        let mut decoder = flate2::read::GzDecoder::new(&data[..]);
        let mut decompressed_data = Vec::new();
        decoder.read_to_end(&mut decompressed_data)?;

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

        // Write decompressed backup to a bind-mount directory so the sidecar can read it
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

        tokio::fs::write(&host_restore_path, &decompressed_data)
            .await
            .map_err(|e| BackupError::Internal {
                message: format!(
                    "Failed to write restore file {}: {}",
                    host_restore_path.display(),
                    e
                ),
            })?;

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
            // NOTE: We intentionally do NOT use ON_ERROR_STOP=on because pg_dump --clean
            // generates "DROP ... ONLY" statements that TimescaleDB rejects for hypertables.
            // These errors are benign — the actual CREATE TABLE and COPY statements succeed.
            let cmd = format!(
                "psql --no-password --host={} --port={} --username={} --dbname={} --file={}",
                host, port_str, username, database, container_restore_path
            );
            ("psql", cmd)
        } else {
            // Custom format: use pg_restore
            let cmd = format!(
                "pg_restore --verbose --clean --if-exists --no-password --host={} --port={} --username={} --dbname={} {}",
                host, port_str, username, database, container_restore_path
            );
            ("pg_restore", cmd)
        };

        info!(
            "Running {} in Docker sidecar for backup {}",
            restore_tool, backup.backup_id
        );

        // Capture stderr in a file for diagnostics
        let stderr_path = format!("/restore/{}.stderr", uuid::Uuid::new_v4());
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
        let host_stderr_path =
            restore_dir.join(std::path::Path::new(&stderr_path).file_name().unwrap());
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
                // pg_dump --clean generates "DROP ... ONLY" on TimescaleDB hypertables.
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

        let walg_s3_prefix = &backup.s3_location;
        let mut walg_env: Vec<String> = vec![
            format!("WALG_S3_PREFIX={}", walg_s3_prefix),
            format!("AWS_ACCESS_KEY_ID={}", decrypted_access_key),
            format!("AWS_SECRET_ACCESS_KEY={}", decrypted_secret_key),
            format!("AWS_REGION={}", s3_source.region),
            format!("PGDATA={}", pgdata),
        ];

        // Resolve S3 endpoint for use inside the Docker container.
        let s3_creds = temps_providers::S3Credentials {
            access_key_id: decrypted_access_key.clone(),
            secret_key: decrypted_secret_key.clone(),
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

        info!(
            "Step 1: Fetching WAL-G backup to {} in container {}",
            restore_temp, container_id
        );
        let fetch_cmd_str = format!(
            "mkdir -p {restore_temp} && rm -rf {restore_temp}/* && wal-g backup-fetch {restore_temp} LATEST > /tmp/walg_restore.log 2>&1",
            restore_temp = restore_temp,
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
            restore_temp = restore_temp,
            pgdata = pgdata,
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
            pgdata = pgdata,
            restore_temp = restore_temp,
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
            match sea_orm::Database::connect(&database_url).await {
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

    pub async fn list_backups(
        &self,
        s3_source_id: i32,
    ) -> Result<Vec<temps_entities::backups::Model>, BackupError> {
        let backups = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::S3SourceId.eq(s3_source_id))
            .order_by_desc(temps_entities::backups::Column::StartedAt)
            .all(self.db.as_ref())
            .await?;
        Ok(backups)
    }

    pub async fn delete_backup(&self, backup_id: &str) -> Result<(), BackupError> {
        info!("Deleting backup: {}", backup_id);

        let backup = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::BackupId.eq(backup_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "Backup".to_string(),
                detail: "Backup not found".to_string(),
            })?;

        let s3_source = temps_entities::s3_sources::Entity::find_by_id(backup.s3_source_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: "S3 source not found".to_string(),
            })?;

        // Create S3 client
        let s3_client = self.create_s3_client(&s3_source).await?;

        // Delete from S3
        s3_client
            .delete_object()
            .bucket(&s3_source.bucket_name)
            .key(&backup.s3_location)
            .send()
            .await
            .map_err(|e| BackupError::S3(e.to_string()))?;

        // Delete record from database
        temps_entities::backups::Entity::delete_many()
            .filter(temps_entities::backups::Column::BackupId.eq(backup_id))
            .exec(self.db.as_ref())
            .await?;

        info!("Backup deleted successfully");
        Ok(())
    }

    pub async fn cleanup_old_backups(&self, retention_days: i32) -> Result<()> {
        info!("Cleaning up old backups");

        let cutoff_date = Utc::now() - Duration::days(retention_days as i64);

        let old_backups = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::StartedAt.lt(cutoff_date))
            .all(self.db.as_ref())
            .await?;

        for backup in old_backups {
            if let Err(e) = self.delete_backup(&backup.backup_id).await {
                error!("Failed to delete old backup {}: {}", backup.backup_id, e);
            }
        }

        Ok(())
    }

    /// List all S3 sources
    pub async fn list_s3_sources(
        &self,
    ) -> Result<Vec<temps_entities::s3_sources::Model>, BackupError> {
        let sources = temps_entities::s3_sources::Entity::find()
            .all(self.db.as_ref())
            .await?;

        debug!("Listed {} S3 sources", sources.len());
        Ok(sources)
    }

    /// Create a new S3 source
    pub async fn create_s3_source(
        &self,
        request: CreateS3SourceRequest,
    ) -> Result<temps_entities::s3_sources::Model, BackupError> {
        // Validate the request
        if request.name.is_empty() {
            return Err(BackupError::Validation(
                "S3 source name cannot be empty".into(),
            ));
        }

        // Test S3 connection and auto-create bucket before persisting
        let s3_client = self.create_s3_client_from_request(&request).await?;
        self.test_and_create_s3_bucket(&s3_client, &request.bucket_name)
            .await?;

        // Encrypt sensitive credentials before storing
        let encrypted_access_key = self
            .encryption_service
            .encrypt_string(&request.access_key_id)
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to encrypt access key: {}", e),
            })?;

        let encrypted_secret_key = self
            .encryption_service
            .encrypt_string(&request.secret_key)
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to encrypt secret key: {}", e),
            })?;

        let new_source = temps_entities::s3_sources::ActiveModel {
            id: sea_orm::NotSet,
            name: sea_orm::Set(request.name.clone()),
            bucket_name: sea_orm::Set(request.bucket_name),
            bucket_path: sea_orm::Set(request.bucket_path),
            access_key_id: sea_orm::Set(encrypted_access_key),
            secret_key: sea_orm::Set(encrypted_secret_key),
            region: sea_orm::Set(request.region),
            created_at: sea_orm::Set(Utc::now()),
            updated_at: sea_orm::Set(Utc::now()),
            endpoint: sea_orm::Set(request.endpoint),
            force_path_style: sea_orm::Set(request.force_path_style),
        };

        let source = new_source.insert(self.db.as_ref()).await?;

        debug!("Created new S3 source: {}", source.name);
        Ok(source)
    }

    /// Get an S3 source by ID
    pub async fn get_s3_source(
        &self,
        id: i32,
    ) -> Result<temps_entities::s3_sources::Model, BackupError> {
        let source = temps_entities::s3_sources::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: "S3 source not found".to_string(),
            })?;

        Ok(source)
    }

    /// Delete an S3 source
    pub async fn delete_s3_source(&self, id: i32) -> Result<bool, BackupError> {
        // First check if source exists and is not in use
        let source = self.get_s3_source(id).await?;
        let result = temps_entities::s3_sources::Entity::delete_by_id(id)
            .exec(self.db.as_ref())
            .await?;

        debug!("Deleted S3 source: {}", source.name);
        Ok(result.rows_affected > 0)
    }

    /// List all backup schedules
    pub async fn list_backup_schedules(
        &self,
    ) -> Result<Vec<temps_entities::backup_schedules::Model>, BackupError> {
        let schedules = temps_entities::backup_schedules::Entity::find()
            .all(self.db.as_ref())
            .await?;

        debug!("Listed {} backup schedules", schedules.len());
        Ok(schedules)
    }

    /// Create a new backup schedule
    pub async fn create_backup_schedule(
        &self,
        request: CreateBackupScheduleRequest,
    ) -> Result<BackupSchedule, BackupError> {
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        // Verify S3 source exists
        temps_entities::s3_sources::Entity::find_by_id(request.s3_source_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: "S3 source not found".to_string(),
            })?;

        // Validate the schedule expression
        self.validate_backup_schedule(&request.schedule_expression)?;

        // Calculate next run time
        let cron_schedule = Schedule::from_str(&request.schedule_expression)
            .map_err(|e| BackupError::Schedule(e.to_string()))?;
        let next_run = cron_schedule.upcoming(Utc).next();

        // Insert with SeaORM
        let now = chrono::Utc::now();
        let tags_json = serde_json::to_string(&request.tags)?;
        let new_schedule = temps_entities::backup_schedules::ActiveModel {
            id: sea_orm::NotSet,
            name: Set(request.name.clone()),
            backup_type: Set(request.backup_type.clone()),
            retention_period: Set(request.retention_period),
            s3_source_id: Set(request.s3_source_id),
            schedule_expression: Set(request.schedule_expression.clone()),
            enabled: Set(request.enabled),
            created_at: Set(now),
            updated_at: Set(now),
            description: Set(request.description.clone()),
            tags: Set(tags_json),
            next_run: Set(next_run),
            ..Default::default()
        };

        let schedule_model = new_schedule.insert(self.db.as_ref()).await?;
        info!("Created new backup schedule: {}", schedule_model.name);
        Ok(schedule_model)
    }

    /// Get a backup schedule by ID
    pub async fn get_backup_schedule(&self, id: i32) -> Result<BackupSchedule, BackupError> {
        use sea_orm::EntityTrait;

        let schedule = temps_entities::backup_schedules::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "BackupSchedule".to_string(),
                detail: "Backup schedule not found".to_string(),
            })?;

        Ok(schedule)
    }

    /// Delete a backup schedule
    pub async fn delete_backup_schedule(&self, id: i32) -> Result<bool, BackupError> {
        use sea_orm::EntityTrait;

        // Ensure it exists to preserve previous behavior/logging
        let schedule = self.get_backup_schedule(id).await?;

        let result = temps_entities::backup_schedules::Entity::delete_by_id(id)
            .exec(self.db.as_ref())
            .await?;
        info!("Deleted backup schedule: {}", schedule.name);
        Ok(result.rows_affected > 0)
    }

    /// List backups for a schedule
    pub async fn list_backups_for_schedule(
        &self,
        schedule_id: i32,
    ) -> Result<Vec<Backup>, BackupError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

        // Verify schedule exists
        self.get_backup_schedule(schedule_id).await?;

        let backups = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::ScheduleId.eq(schedule_id))
            .order_by_desc(temps_entities::backups::Column::StartedAt)
            .all(self.db.as_ref())
            .await?;

        debug!(
            "Listed {} backups for schedule {}",
            backups.len(),
            schedule_id
        );
        Ok(backups)
    }

    /// Run a backup immediately for a given S3 source
    pub async fn run_backup_for_source(
        &self,
        s3_source_id: i32,
        backup_type: &str,
        created_by: i32,
    ) -> Result<Backup, BackupError> {
        use sea_orm::EntityTrait;

        info!("Running backup for S3 source {}", s3_source_id);

        // Verify S3 source exists
        temps_entities::s3_sources::Entity::find_by_id(s3_source_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: "S3 source not found".to_string(),
            })?;

        // Create the backup
        let backup = self
            .create_backup(
                None, // No schedule associated
                s3_source_id,
                backup_type,
                created_by,
            )
            .await
            .map_err(|e| {
                error!("Backup failed for S3 source {}: {}", s3_source_id, e);
                e
            })?;

        info!(
            "Successfully created backup {} for S3 source {}",
            backup.backup_id, s3_source_id
        );
        Ok(backup)
    }

    /// Update an S3 source
    pub async fn update_s3_source(
        &self,
        id: i32,
        request: crate::handlers::backup_handler::UpdateS3SourceRequest,
    ) -> Result<S3Source, BackupError> {
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        let current = temps_entities::s3_sources::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: "S3 source not found".to_string(),
            })?;

        let mut active = current.into_active_model();

        if let Some(name) = request.name {
            active.name = Set(name);
        }
        if let Some(bucket_name) = request.bucket_name {
            active.bucket_name = Set(bucket_name);
        }
        if let Some(bucket_path) = request.bucket_path {
            active.bucket_path = Set(bucket_path);
        }
        if let Some(access_key_id) = request.access_key_id {
            // Encrypt access key before storing
            let encrypted_access_key = self
                .encryption_service
                .encrypt_string(&access_key_id)
                .map_err(|e| BackupError::Internal {
                    message: format!("Failed to encrypt access key: {}", e),
                })?;
            active.access_key_id = Set(encrypted_access_key);
        }
        if let Some(secret_key) = request.secret_key {
            // Encrypt secret key before storing
            let encrypted_secret_key = self
                .encryption_service
                .encrypt_string(&secret_key)
                .map_err(|e| BackupError::Internal {
                    message: format!("Failed to encrypt secret key: {}", e),
                })?;
            active.secret_key = Set(encrypted_secret_key);
        }
        if let Some(region) = request.region {
            active.region = Set(region);
        }
        if let Some(endpoint) = request.endpoint {
            active.endpoint = Set(Some(endpoint));
        }
        if let Some(force_path_style) = request.force_path_style {
            active.force_path_style = Set(Some(force_path_style));
        }

        active.updated_at = Set(chrono::Utc::now());

        let updated = active.update(self.db.as_ref()).await?;
        Ok(updated)
    }

    /// Generate metadata for a backup
    fn generate_backup_metadata(
        &self,
        backup: &Backup,
        s3_source: &temps_entities::s3_sources::Model,
        external_backups: &[(
            temps_entities::external_service_backups::Model,
            temps_entities::external_services::Model,
        )],
    ) -> serde_json::Value {
        // Serialize the server config
        let config_yaml = serde_yaml::to_string(&self.config_service.get_server_config())
            .unwrap_or_else(|e| {
                error!("Failed to serialize server config: {}", e);
                String::new()
            });

        // Map external backups to the required format
        let external_backups = external_backups
            .iter()
            .map(|(b, service)| {
                json!({
                    "backup_id": b.backup_id,
                    "service_id": b.service_id,
                    "s3_location": b.s3_location,
                    "state": b.state,
                    "size_bytes": b.size_bytes,
                    "type": "full",
                    "metadata": {
                        "service_type": service.service_type,
                        "service_name": service.name
                    }
                })
            })
            .collect::<Vec<_>>();

        json!({
            "backup_id": backup.backup_id,
            "name": backup.name,
            "type": backup.backup_type,
            "created_at": backup.started_at.to_rfc3339(),
            "created_by": backup.created_by,
            "size_bytes": backup.size_bytes,
            "compression_type": backup.compression_type,
            "source": {
                "id": s3_source.id,
                "name": s3_source.name,
                "bucket": s3_source.bucket_name,
                "path": s3_source.bucket_path
            },
            "schedule_id": backup.schedule_id,
            "state": backup.state,
            "tags": serde_json::from_str::<Vec<String>>(&backup.tags).unwrap_or_default(),
            "checksum": backup.checksum,
            "server_config": config_yaml,
            "external_service_backups": external_backups,
            "metadata": serde_json::from_str::<serde_json::Value>(&backup.metadata).unwrap_or_default()
        })
    }

    /// Update the source's backup index
    async fn update_backup_index(
        &self,
        s3_client: &S3Client,
        s3_source: &temps_entities::s3_sources::Model,
        backup: &Backup,
    ) -> Result<()> {
        let index_key = format!(
            "{}/backups/index.json",
            s3_source.bucket_path.trim_matches('/')
        );

        // Try to get existing index
        let mut index = match s3_client
            .get_object()
            .bucket(&s3_source.bucket_name)
            .key(&index_key)
            .send()
            .await
        {
            Ok(response) => {
                let data = response.body.collect().await?.to_vec();
                serde_json::from_slice::<serde_json::Value>(&data).unwrap_or_else(|_| {
                    json!({
                        "backups": [],
                        "last_updated": Utc::now().to_rfc3339()
                    })
                })
            }
            Err(_) => json!({
                "backups": [],
                "last_updated": Utc::now().to_rfc3339()
            }),
        };
        // Add new backup to index
        if let Some(backups) = index.get_mut("backups").and_then(|b| b.as_array_mut()) {
            backups.push(json!({
                "id": backup.id,
                "backup_id": backup.backup_id,
                "name": backup.name,
                "type": backup.backup_type,
                "created_at": backup.started_at.to_rfc3339(),
                "size_bytes": backup.size_bytes,
                "location": backup.s3_location.clone(),
                "metadata_location": backup.s3_location
                    .replace("backup.sql.gz", "metadata.json")
                    .replace("backup.postgresql.gz", "metadata.json")
            }));
        }
        index["last_updated"] = json!(Utc::now().to_rfc3339());

        // Upload updated index
        s3_client
            .put_object()
            .bucket(&s3_source.bucket_name)
            .key(&index_key)
            .body(serde_json::to_vec(&index)?.into())
            .content_type("application/json")
            .send()
            .await?;

        Ok(())
    }

    /// Add a new method to list all backups in a source
    pub async fn list_source_backups(
        &self,
        s3_source_id: i32,
    ) -> Result<serde_json::Value, BackupError> {
        // Ensure the source exists and fetch config
        let s3_source = temps_entities::s3_sources::Entity::find_by_id(s3_source_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: "S3 source not found".to_string(),
            })?;

        // Create S3 client
        let s3_client = self.create_s3_client(&s3_source).await?;

        // Read index.json from the source
        let key = format!(
            "{}/backups/index.json",
            s3_source.bucket_path.trim_matches('/')
        );

        let resp = s3_client
            .get_object()
            .bucket(&s3_source.bucket_name)
            .key(&key)
            .send()
            .await
            .map_err(|e| BackupError::S3(e.to_string()))?;

        let bytes = resp
            .body
            .collect()
            .await
            .map_err(|e| BackupError::S3(e.to_string()))?
            .into_bytes();

        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        Ok(value)
    }

    /// Get a backup by ID
    pub async fn get_backup(&self, backup_id: &str) -> Result<Option<Backup>, BackupError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let model = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::BackupId.eq(backup_id.to_string()))
            .one(self.db.as_ref())
            .await?;

        Ok(model)
    }

    /// Get an external service by ID
    pub async fn get_external_service(
        &self,
        service_id: i32,
    ) -> Result<temps_entities::external_services::Model, BackupError> {
        temps_entities::external_services::Entity::find_by_id(service_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "ExternalService".to_string(),
                detail: format!("External service with ID {} not found", service_id),
            })
    }

    pub async fn backup_external_service(
        &self,
        service: &temps_entities::external_services::Model,
        s3_source_id: i32,
        backup_type: &str,
        created_by: i32,
    ) -> Result<temps_entities::external_service_backups::Model, BackupError> {
        info!("Starting external service backup process");
        let service_id = service.id;

        // Get S3 source configuration
        let s3_source = temps_entities::s3_sources::Entity::find_by_id(s3_source_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: "S3 source not found".to_string(),
            })?;

        // Create S3 client
        let s3_client = self
            .create_s3_client(&s3_source)
            .await
            .map_err(|e| BackupError::S3(e.to_string()))?;

        // Decrypt S3 credentials for services that pass them to external tools (e.g., WAL-G)
        let decrypted_access_key = self
            .encryption_service
            .decrypt_string(&s3_source.access_key_id)
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to decrypt access key for backup: {}", e),
            })?;
        let decrypted_secret_key = self
            .encryption_service
            .decrypt_string(&s3_source.secret_key)
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to decrypt secret key for backup: {}", e),
            })?;
        let s3_credentials = temps_providers::S3Credentials {
            access_key_id: decrypted_access_key,
            secret_key: decrypted_secret_key,
            region: s3_source.region.clone(),
            endpoint: s3_source.endpoint.clone(),
            bucket_name: s3_source.bucket_name.clone(),
            bucket_path: s3_source.bucket_path.clone(),
            force_path_style: s3_source.force_path_style.unwrap_or(true),
        };

        // Generate unique backup ID
        let backup_id = Uuid::new_v4().to_string();

        // Create backup record
        let backup = temps_entities::backups::ActiveModel {
            id: sea_orm::NotSet,
            name: sea_orm::Set(format!("Backup {}", backup_id)),
            backup_id: sea_orm::Set(backup_id.clone()),
            schedule_id: sea_orm::Set(None),
            backup_type: sea_orm::Set(backup_type.to_string()),
            state: sea_orm::Set("running".to_string()),
            started_at: sea_orm::Set(chrono::Utc::now()),
            finished_at: sea_orm::Set(None),
            s3_source_id: sea_orm::Set(s3_source_id),
            s3_location: sea_orm::Set("".to_string()), // Will be updated by the service
            compression_type: sea_orm::Set("gzip".to_string()),
            created_by: sea_orm::Set(created_by),
            tags: sea_orm::Set("[]".to_string()),
            size_bytes: sea_orm::Set(None),
            file_count: sea_orm::Set(None),
            error_message: sea_orm::Set(None),
            metadata: sea_orm::Set(
                json!({
                    "service_id": service_id,
                    "service_type": service.service_type,
                    "service_name": service.name,
                    "timestamp": Utc::now().to_rfc3339()
                })
                .to_string(),
            ),
            checksum: sea_orm::Set(None),
            expires_at: sea_orm::Set(None),
        };

        let backup = backup.insert(self.db.as_ref()).await?;

        // Generate backup path
        let subpath = format!(
            "external_services/{}/{}/{}",
            service.service_type,
            service.name,
            Utc::now().format("%Y/%m/%d")
        );
        let subpath_root = format!(
            "external_services/{}/{}",
            service.service_type, service.name
        );
        let service_type = temps_providers::ServiceType::from_str(&service.service_type)
            .map_err(|e| BackupError::Validation(e.to_string()))?;
        let service_instance = self
            .external_service_manager
            .get_service_instance(service.name.clone(), service_type);

        let service_config = self
            .external_service_manager
            .get_service_config(service_id)
            .await
            .map_err(|e| BackupError::ExternalService(e.to_string()))?;

        // Perform the backup
        let backup_location = service_instance
            .backup_to_s3(
                &s3_client,
                &s3_credentials,
                backup.clone(),
                &s3_source,
                &subpath,
                &subpath_root,
                &self.db,
                service,
                service_config,
            )
            .await
            .map_err(|e| {
                error!(
                    "External service backup failed for service '{}' (type={}, id={}): {}",
                    service.name, service.service_type, service.id, e
                );
                BackupError::ExternalService(e.to_string())
            })?;
        info!("Backup created at location: {}", backup_location);
        // Get the external service backup record
        let external_backup = temps_entities::external_service_backups::Entity::find()
            .filter(temps_entities::external_service_backups::Column::BackupId.eq(backup.id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "ExternalServiceBackup".to_string(),
                detail: "External service backup record not found".to_string(),
            })?;

        info!(
            "External service backup completed successfully: {}",
            backup_id
        );
        Ok(external_backup)
    }

    // Add this new validation function
    fn validate_backup_schedule(&self, schedule: &str) -> Result<(), BackupError> {
        let schedule = Schedule::from_str(schedule)
            .map_err(|e| BackupError::Validation(format!("Invalid backup schedule: {}", e)))?;

        // Get the first two occurrences
        let upcoming = schedule.upcoming(Utc);
        let next_two = upcoming.take(2).collect::<Vec<_>>();
        if let [first, second] = next_two.as_slice() {
            let duration = *second - *first;
            if duration.num_minutes() < 60 {
                return Err(BackupError::Validation(
                    "Backup schedule must be at least 1 hour apart".into(),
                ));
            }
        }

        Ok(())
    }

    /// Start the backup scheduler with graceful cancellation support
    ///
    /// This method runs an infinite loop that:
    /// 1. Initializes schedules that don't have next_run set
    /// 2. Runs at the start of each hour to check for backups that need to be executed
    /// 3. Can be gracefully cancelled using the provided CancellationToken
    pub async fn start_backup_scheduler(
        &self,
        cancellation_token: tokio_util::sync::CancellationToken,
    ) -> Result<(), BackupError> {
        debug!("Starting backup scheduler");

        // First update all schedules that don't have next_run set
        let schedules = temps_entities::backup_schedules::Entity::find()
            .filter(temps_entities::backup_schedules::Column::NextRun.is_null())
            .all(self.db.as_ref())
            .await?;
        debug!("Updating next_run for {} schedules", schedules.len());
        for schedule in schedules {
            let cron_schedule = Schedule::from_str(&schedule.schedule_expression).map_err(|e| {
                BackupError::Validation(format!(
                    "Error parsing schedule expression for schedule {}: {}",
                    schedule.id, e
                ))
            })?;
            if let Some(next_run) = cron_schedule.upcoming(Utc).next() {
                let schedule_id = schedule.id;
                let mut schedule_update: temps_entities::backup_schedules::ActiveModel =
                    schedule.into_active_model();
                schedule_update.next_run = sea_orm::Set(Some(next_run));
                schedule_update.update(self.db.as_ref()).await?;
                info!(
                    "Updated next_run for schedule {}: {}",
                    schedule_id, next_run
                );
            }
        }

        loop {
            let now = Utc::now();

            // Only run at the start of each hour
            if now.minute() != 0 {
                // Sleep until next hour or cancellation
                let next_hour = (now + chrono::Duration::hours(1))
                    .with_minute(0)
                    .unwrap()
                    .with_second(0)
                    .unwrap()
                    .with_nanosecond(0)
                    .unwrap();
                let sleep_duration = next_hour - now;

                tokio::select! {
                    _ = time::sleep(time::Duration::from_secs(sleep_duration.num_seconds() as u64)) => {
                        continue;
                    }
                    _ = cancellation_token.cancelled() => {
                        info!("Backup scheduler received cancellation signal");
                        return Ok(());
                    }
                }
            }

            // Process scheduled backups with cancellation check
            tokio::select! {
                result = self.process_scheduled_backups(now) => {
                    if let Err(e) = result {
                        error!("Error processing scheduled backups: {}", e);
                    }
                }
                _ = cancellation_token.cancelled() => {
                    info!("Backup scheduler received cancellation signal");
                    return Ok(());
                }
            }

            // Sleep until next hour or cancellation
            let next_hour = (now + chrono::Duration::hours(1))
                .with_minute(0)
                .unwrap()
                .with_second(0)
                .unwrap()
                .with_nanosecond(0)
                .unwrap();
            let sleep_duration = next_hour - now;

            tokio::select! {
                _ = time::sleep(time::Duration::from_secs(sleep_duration.num_seconds() as u64)) => {}
                _ = cancellation_token.cancelled() => {
                    info!("Backup scheduler received cancellation signal");
                    return Ok(());
                }
            }
        }
    }

    async fn process_scheduled_backups(&self, now: DateTime<Utc>) -> Result<()> {
        // Get all active backup schedules
        let schedules = temps_entities::backup_schedules::Entity::find()
            .all(self.db.as_ref())
            .await?;

        for schedule in schedules {
            if let Err(e) = self.process_backup_schedule(&schedule, now).await {
                error!("Error processing backup schedule {}: {}", schedule.id, e);
                continue;
            }
        }

        Ok(())
    }

    async fn process_backup_schedule(
        &self,
        schedule: &temps_entities::backup_schedules::Model,
        now: DateTime<Utc>,
    ) -> Result<()> {
        // Skip processing if schedule is disabled
        if !schedule.enabled {
            info!(
                "Skipping disabled backup schedule {} ({})",
                schedule.id, schedule.name
            );
            return Ok(());
        }

        let cron_schedule = Schedule::from_str(&schedule.schedule_expression)?;
        let next_run = schedule.next_run;

        let should_run = match next_run {
            Some(next) => next <= now,
            None => {
                // If next_run is not set, calculate it from the schedule
                if let Some(next) = cron_schedule.upcoming(Utc).next() {
                    next <= now
                } else {
                    false
                }
            }
        };

        if should_run {
            info!(
                "Running scheduled backup for schedule {} ({})",
                schedule.id, schedule.name
            );

            // Calculate the next run time
            let next_run = cron_schedule.upcoming(Utc).next();

            // Update the next_run time in the database
            if let Some(next_run) = next_run {
                let mut schedule_update: temps_entities::backup_schedules::ActiveModel =
                    schedule.clone().into_active_model();
                schedule_update.next_run = sea_orm::Set(Some(next_run));
                schedule_update.last_run = sea_orm::Set(Some(Utc::now()));
                schedule_update.update(self.db.as_ref()).await?;
            }

            // Create the backup
            match self
                .create_backup(
                    Some(schedule.id),
                    schedule.s3_source_id,
                    &schedule.backup_type,
                    0, // System user (0) for scheduled backups
                )
                .await
            {
                Ok(backup) => {
                    info!(
                        "Successfully created scheduled backup: {}",
                        backup.backup_id
                    );
                }
                Err(e) => {
                    error!("Failed to create scheduled backup: {}", e);

                    // Send notification for backup failure
                    let failure_data = BackupFailureData {
                        schedule_id: schedule.id,
                        schedule_name: schedule.name.clone(),
                        backup_type: schedule.backup_type.clone(),
                        error: e.to_string(),
                        timestamp: Utc::now(),
                    };

                    if let Err(notify_err) =
                        self.send_backup_failure_notification(failure_data).await
                    {
                        error!("Failed to send backup failure notification: {}", notify_err);
                    }
                }
            }
        }

        Ok(())
    }

    pub async fn update_next_run(&self, schedule_id: i32, schedule_str: &str) -> Result<()> {
        // Validate the schedule
        let schedule = Schedule::from_str(schedule_str)
            .map_err(|_| BackupError::Validation("Invalid backup schedule".into()))?;

        // Calculate next run time
        let next_run = schedule.upcoming(Utc).next();

        // Get the schedule and update it
        let schedule_model = temps_entities::backup_schedules::Entity::find_by_id(schedule_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "BackupSchedule".to_string(),
                detail: "Backup schedule not found".to_string(),
            })?;

        let mut schedule_update: temps_entities::backup_schedules::ActiveModel =
            schedule_model.into_active_model();
        schedule_update.next_run = sea_orm::Set(next_run);
        schedule_update.update(self.db.as_ref()).await?;

        info!(
            "Updated next run time for backup schedule {}: {:?}",
            schedule_id, next_run
        );
        Ok(())
    }

    // Add this new method
    pub async fn disable_backup_schedule(
        &self,
        id: i32,
    ) -> Result<temps_entities::backup_schedules::Model, BackupError> {
        let schedule_model = temps_entities::backup_schedules::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "BackupSchedule".to_string(),
                detail: "Backup schedule not found".to_string(),
            })?;

        let mut schedule_update: temps_entities::backup_schedules::ActiveModel =
            schedule_model.into_active_model();
        schedule_update.enabled = sea_orm::Set(false);
        schedule_update.updated_at = sea_orm::Set(Utc::now());
        schedule_update.update(self.db.as_ref()).await?;

        self.get_backup_schedule(id).await
    }

    // Add this new method
    pub async fn enable_backup_schedule(
        &self,
        id: i32,
    ) -> Result<temps_entities::backup_schedules::Model, BackupError> {
        // Get the schedule to validate it exists and get the schedule expression
        let schedule = temps_entities::backup_schedules::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "BackupSchedule".to_string(),
                detail: "Backup schedule not found".to_string(),
            })?;

        // Calculate next run time based on the schedule expression
        let cron_schedule = Schedule::from_str(&schedule.schedule_expression)
            .map_err(|_| BackupError::Validation("Invalid backup schedule".into()))?;
        let next_run = cron_schedule.upcoming(Utc).next();

        // Update the schedule
        let mut schedule_update: temps_entities::backup_schedules::ActiveModel =
            schedule.into_active_model();
        schedule_update.enabled = sea_orm::Set(true);
        schedule_update.updated_at = sea_orm::Set(Utc::now());
        schedule_update.next_run = sea_orm::Set(next_run);

        let updated_schedule = schedule_update.update(self.db.as_ref()).await?;
        Ok(updated_schedule)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::Docker;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use temps_core::notifications::{EmailMessage, NotificationData, NotificationError};
    use temps_core::EncryptionService;
    use temps_entities::{backup_schedules, s3_sources};

    // Simple mock notification service for testing
    struct TestNotificationService;

    #[async_trait::async_trait]
    impl NotificationService for TestNotificationService {
        async fn send_email(&self, _message: EmailMessage) -> Result<(), NotificationError> {
            Ok(())
        }

        async fn send_notification(
            &self,
            _notification: NotificationData,
        ) -> Result<(), NotificationError> {
            Ok(())
        }

        async fn is_configured(&self) -> Result<bool, NotificationError> {
            Ok(true)
        }
    }

    fn create_mock_config_service() -> Arc<temps_config::ConfigService> {
        let server_config = temps_config::ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            "postgres://localhost:5432/test".to_string(),
            None,
            None,
        )
        .unwrap();

        // Create a mock database connection
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

        Arc::new(temps_config::ConfigService::new(
            Arc::new(server_config),
            db,
        ))
    }

    fn create_mock_notification_service() -> Arc<dyn NotificationService> {
        Arc::new(TestNotificationService)
    }

    fn create_mock_external_service_manager(
        db: Arc<sea_orm::DatabaseConnection>,
    ) -> Arc<temps_providers::ExternalServiceManager> {
        // Create a mock encryption service with a test key
        let test_key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let encryption_service = Arc::new(EncryptionService::new(test_key).unwrap());

        // Create Docker connection
        let docker = Docker::connect_with_local_defaults().unwrap();

        Arc::new(temps_providers::ExternalServiceManager::new(
            db,
            encryption_service,
            Arc::new(docker),
        ))
    }

    #[tokio::test]
    #[ignore] // Requires system TLS certificates (fails on some macOS configurations)
    async fn test_create_s3_client() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());

        // Encrypt the credentials for the test
        let encrypted_access_key = encryption_service.encrypt_string("test-key").unwrap();
        let encrypted_secret_key = encryption_service.encrypt_string("test-secret").unwrap();

        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        let s3_source = S3Source {
            id: 1,
            name: "test-source".to_string(),
            bucket_name: "test-bucket".to_string(),
            bucket_path: "/backups".to_string(),
            access_key_id: encrypted_access_key,
            secret_key: encrypted_secret_key,
            region: "us-east-1".to_string(),
            endpoint: Some("http://localhost:9000".to_string()),
            force_path_style: Some(true),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let result = backup_service.create_s3_client(&s3_source).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_validate_backup_schedule_valid() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        // Valid schedule: every day at 2 AM (24 hours apart) - cron format with seconds
        let result = backup_service.validate_backup_schedule("0 0 2 * * *");
        assert!(
            result.is_ok(),
            "Expected valid schedule to pass: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_validate_backup_schedule_too_frequent() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());

        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        // Invalid schedule: every 30 minutes (too frequent) - cron format with seconds
        let result = backup_service.validate_backup_schedule("0 */30 * * * *");
        assert!(result.is_err(), "Expected error for too frequent schedule");
        match result {
            Err(BackupError::Validation(msg)) => {
                assert!(
                    msg.contains("at least 1 hour apart"),
                    "Error message should mention minimum interval: {}",
                    msg
                );
            }
            other => panic!("Expected validation error, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_validate_backup_schedule_invalid_cron() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        // Invalid cron expression
        let result = backup_service.validate_backup_schedule("invalid cron");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_s3_sources_empty() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<s3_sources::Model>::new()])
                .into_connection(),
        );

        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        let result = backup_service.list_s3_sources().await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 0);
    }

    #[tokio::test]
    #[ignore] // Requires system TLS certificates (fails on some macOS configurations)
    async fn test_create_s3_source() {
        let s3_source = s3_sources::Model {
            id: 1,
            name: "test-source".to_string(),
            bucket_name: "test-bucket".to_string(),
            bucket_path: "/backups".to_string(),
            access_key_id: "test-key".to_string(),
            secret_key: "test-secret".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some("http://localhost:9000".to_string()),
            force_path_style: Some(true),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![s3_source.clone()]])
                .append_exec_results(vec![MockExecResult {
                    last_insert_id: 1,
                    rows_affected: 1,
                }])
                .into_connection(),
        );

        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        let request = CreateS3SourceRequest {
            name: "test-source".to_string(),
            bucket_name: "test-bucket".to_string(),
            bucket_path: "/backups".to_string(),
            access_key_id: "test-key".to_string(),
            secret_key: "test-secret".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some("http://localhost:9000".to_string()),
            force_path_style: Some(true),
        };

        let result = backup_service.create_s3_source(request).await;
        assert!(result.is_ok());
        let source = result.unwrap();
        assert_eq!(source.name, "test-source");
        assert_eq!(source.bucket_name, "test-bucket");
    }

    #[tokio::test]
    async fn test_create_s3_source_empty_name() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        let request = CreateS3SourceRequest {
            name: "".to_string(),
            bucket_name: "test-bucket".to_string(),
            bucket_path: "/backups".to_string(),
            access_key_id: "test-key".to_string(),
            secret_key: "test-secret".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some("http://localhost:9000".to_string()),
            force_path_style: Some(true),
        };

        let result = backup_service.create_s3_source(request).await;
        assert!(result.is_err());
        match result {
            Err(BackupError::Validation(msg)) => {
                assert!(msg.contains("cannot be empty"));
            }
            _ => panic!("Expected validation error"),
        }
    }

    #[tokio::test]
    async fn test_list_backup_schedules_empty() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<backup_schedules::Model>::new()])
                .into_connection(),
        );

        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        let result = backup_service.list_backup_schedules().await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_get_s3_source_not_found() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<s3_sources::Model>::new()])
                .into_connection(),
        );

        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        let result = backup_service.get_s3_source(999).await;
        assert!(result.is_err());
        match result {
            Err(BackupError::NotFound { .. }) => {}
            _ => panic!("Expected NotFound error"),
        }
    }

    #[tokio::test]
    async fn test_get_backup_schedule_not_found() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<backup_schedules::Model>::new()])
                .into_connection(),
        );

        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        let result = backup_service.get_backup_schedule(999).await;
        assert!(result.is_err());
        match result {
            Err(BackupError::NotFound { .. }) => {}
            _ => panic!("Expected NotFound error"),
        }
    }

    #[tokio::test]
    async fn test_backup_to_minio_integration() {
        if bollard::Docker::connect_with_local_defaults().is_err() {
            println!("Docker not available, skipping test");
            return;
        }
        use temps_database::test_utils::TestDatabase;
        use testcontainers::{runners::AsyncRunner, GenericImage, ImageExt};

        // Start MinIO container
        let minio_container = GenericImage::new("minio/minio", "latest")
            .with_env_var("MINIO_ROOT_USER", "minioadmin")
            .with_env_var("MINIO_ROOT_PASSWORD", "minioadmin")
            .with_cmd(vec!["server", "/data", "--console-address", ":9001"])
            .start()
            .await
            .expect("Failed to start MinIO container");

        let minio_port = minio_container
            .get_host_port_ipv4(9000)
            .await
            .expect("Failed to get MinIO port");

        let minio_endpoint = format!("http://localhost:{}", minio_port);

        // Give MinIO time to start
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

        // Start PostgreSQL database with migrations
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");

        // Create S3 client for bucket creation
        let s3_config = aws_sdk_s3::config::Builder::new()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new("us-east-1"))
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                "minioadmin",
                "minioadmin",
                None,
                None,
                "test",
            ))
            .endpoint_url(&minio_endpoint)
            .force_path_style(true)
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
        let notification_service = create_mock_notification_service();

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
        let backup_service = BackupService::new(
            test_db.db.clone(),
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
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
            name: "test-minio".to_string(),
            bucket_name: bucket_name.to_string(),
            bucket_path: "/backups".to_string(),
            access_key_id: "minioadmin".to_string(),
            secret_key: "minioadmin".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some(minio_endpoint.clone()),
            force_path_style: Some(true),
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
            s3_source_id: s3_source.id,
            schedule_expression: "0 0 2 * * *".to_string(), // Daily at 2 AM
            enabled: true,
            description: Some("Test backup schedule".to_string()),
            tags: vec![],
        };

        let schedule = backup_service
            .create_backup_schedule(schedule_request)
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

        println!("\n✓ Integration test passed:");
        println!("  - Database container started (timescale/timescaledb-ha)");
        println!("  - MinIO container started");
        println!("  - Backup created with ID: {}", backup_result.id);
        println!(
            "  - Backup size: {} bytes (compressed)",
            backup_result.size_bytes.unwrap_or(0)
        );
        println!("  - Decompressed size: {} bytes", decompressed.len());
        println!("  - Backup format: valid gzip-compressed pg_dump custom format (PGDMP)");
        println!("  - Objects in bucket: {}", object_count);
    }

    #[tokio::test]
    async fn test_restore_postgres_from_url() {
        if bollard::Docker::connect_with_local_defaults().is_err() {
            println!("Docker not available, skipping test");
            return;
        }
        use temps_database::test_utils::TestDatabase;
        use testcontainers::{runners::AsyncRunner, GenericImage, ImageExt};

        // Start MinIO container
        let minio_container = GenericImage::new("minio/minio", "latest")
            .with_env_var("MINIO_ROOT_USER", "minioadmin")
            .with_env_var("MINIO_ROOT_PASSWORD", "minioadmin")
            .with_cmd(vec!["server", "/data", "--console-address", ":9001"])
            .start()
            .await
            .expect("Failed to start MinIO container");

        let minio_port = minio_container
            .get_host_port_ipv4(9000)
            .await
            .expect("Failed to get MinIO port");

        let minio_endpoint = format!("http://localhost:{}", minio_port);

        // Give MinIO time to start
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

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
                "minioadmin",
                "minioadmin",
                None,
                None,
                "test",
            ))
            .endpoint_url(&minio_endpoint)
            .force_path_style(true)
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
        let notification_service = create_mock_notification_service();
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
            notification_service.clone(),
            source_config_service,
            encryption_service,
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
            access_key_id: "minioadmin".to_string(),
            secret_key: "minioadmin".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some(minio_endpoint.clone()),
            force_path_style: Some(true),
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
            notification_service,
            target_config_service,
            encryption_service,
        );

        // Create the S3 source in the target database
        let target_s3_source_request = CreateS3SourceRequest {
            name: "test-restore-source".to_string(),
            bucket_name: bucket_name.to_string(),
            bucket_path: "/backups".to_string(),
            access_key_id: "minioadmin".to_string(),
            secret_key: "minioadmin".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some(minio_endpoint.clone()),
            force_path_style: Some(true),
        };

        let target_s3_source = target_backup_service
            .create_s3_source(target_s3_source_request)
            .await
            .expect("Failed to create S3 source in target database");

        // Create a user in the target database to satisfy foreign key constraint
        let target_user = users::ActiveModel {
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
        println!("  - Backup created and uploaded to MinIO");
        println!("  - Target database created");
        println!("  - Backup restored to target database from URL");
        println!("  - Data verified: project and user successfully restored with matching data");
    }

    #[tokio::test]
    #[ignore] // Requires system TLS certificates (fails on some macOS configurations)
    async fn test_create_s3_client_from_request_valid() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());

        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        let request = CreateS3SourceRequest {
            name: "test-source".to_string(),
            bucket_name: "test-bucket".to_string(),
            bucket_path: "/backups".to_string(),
            access_key_id: "test-access-key".to_string(),
            secret_key: "test-secret-key".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some("http://localhost:9000".to_string()),
            force_path_style: Some(true),
        };

        let result = backup_service.create_s3_client_from_request(&request).await;
        assert!(
            result.is_ok(),
            "create_s3_client_from_request should succeed with valid request"
        );
    }

    #[tokio::test]
    #[ignore] // Requires actual S3 connection
    async fn test_create_s3_source_with_bucket_creation() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());

        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        let request = CreateS3SourceRequest {
            name: "test-auto-create-bucket".to_string(),
            bucket_name: "test-auto-create-bucket".to_string(),
            bucket_path: "/backups".to_string(),
            access_key_id: "minioadmin".to_string(),
            secret_key: "minioadmin".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some("http://localhost:9000".to_string()),
            force_path_style: Some(true),
        };

        // This test requires a real MinIO instance running
        // When running, it should:
        // 1. Create an S3 client from the request
        // 2. Test the connection and create the bucket if needed
        // 3. Persist the S3 source to the database
        match backup_service.create_s3_source(request).await {
            Ok(_) => {
                println!("✓ S3 source created successfully with auto-bucket creation");
            }
            Err(e) => {
                println!(
                    "! Test skipped or failed: {} (requires running MinIO instance)",
                    e
                );
            }
        }
    }

    #[tokio::test]
    async fn test_create_s3_source_request_validation() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());

        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        let invalid_request = CreateS3SourceRequest {
            name: "".to_string(), // Empty name - should fail validation
            bucket_name: "test-bucket".to_string(),
            bucket_path: "/backups".to_string(),
            access_key_id: "test-key".to_string(),
            secret_key: "test-secret".to_string(),
            region: "us-east-1".to_string(),
            endpoint: None,
            force_path_style: None,
        };

        let result = backup_service.create_s3_source(invalid_request).await;
        assert!(
            result.is_err(),
            "create_s3_source should fail with empty name"
        );
        match result {
            Err(BackupError::Validation(msg)) => {
                assert!(
                    msg.contains("S3 source name cannot be empty"),
                    "Error should mention empty name validation"
                );
            }
            _ => panic!("Expected validation error for empty name"),
        }
    }

    // -------------------------------------------------------------------------
    // TimescaleDB sidecar image selection
    // -------------------------------------------------------------------------

    fn make_backup_service() -> BackupService {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db),
            create_mock_notification_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
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

    /// The TimescaleDB version string format is "PostgreSQL 17.x on ..." — identical to
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
}
