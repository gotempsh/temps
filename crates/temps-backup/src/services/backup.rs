use crate::handlers::backup_handler::{CreateBackupScheduleRequest, CreateS3SourceRequest};
use anyhow::{Context, Result};
use aws_sdk_s3::error::ProvideErrorMetadata;
use aws_sdk_s3::{Client as S3Client, Config};
use chrono::{DateTime, Duration, Timelike, Utc};
use flate2::Compression;
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
use tracing::{debug, error, info};
use uuid::Uuid;

use cron::Schedule;
use flate2::write::GzEncoder;
use temps_core::notifications::{BackupFailureData, NotificationService};
use temps_entities::{backup_schedules::Model as BackupSchedule, s3_sources::Model as S3Source};
use temps_providers::ExternalServiceManager;
use tokio_stream::StreamExt;

#[derive(Error, Debug)]
pub enum BackupError {
    #[error("Database connection error")]
    DatabaseConnectionError(String),

    #[error("Database error: {0}")]
    Database(sea_orm::DbErr),

    #[error("S3 error: {0}")]
    S3(String),

    #[error("Schedule error: {0}")]
    Schedule(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Resource not found: {0}")]
    NotFound(String),

    #[error("Invalid configuration: {0}")]
    Configuration(String),

    #[error("External service error: {0}")]
    ExternalService(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Backup operation failed: {0}")]
    Operation(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Unsupported: {0}")]
    Unsupported(String),

    #[error("Notification error: {0}")]
    NotificationError(String),
}

// Implementation to convert anyhow errors to BackupError
impl From<anyhow::Error> for BackupError {
    fn from(err: anyhow::Error) -> Self {
        BackupError::Internal(err.to_string())
    }
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

impl From<sea_orm::DbErr> for BackupError {
    fn from(err: sea_orm::DbErr) -> Self {
        match err {
            sea_orm::DbErr::RecordNotFound(_) => {
                BackupError::NotFound("Resource not found".to_string())
            }
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
            .ok_or_else(|| BackupError::NotFound("S3 source not found".to_string()))?;

        // Create a temporary file for the backup
        let mut temp_file = NamedTempFile::new().map_err(BackupError::Io)?;

        // Perform database backup
        self.backup_database(&mut temp_file).await?;

        // Generate unique backup ID
        let backup_id = Uuid::new_v4().to_string();

        // Calculate file size
        let size_bytes = temp_file
            .as_file()
            .metadata()
            .map_err(BackupError::Io)?
            .len() as i32;

        // Generate S3 location
        let s3_location = format!(
            "{}/backups/{}/{}/backup.postgresql.gz",
            s3_source.bucket_path.trim_matches('/'),
            Utc::now().format("%Y/%m/%d"),
            backup_id
        );

        // Create S3 client
        let s3_client = self.create_s3_client(&s3_source).await?;

        // Compress and upload the backup
        self.upload_backup(&s3_client, &s3_source, &temp_file, &s3_location)
            .await?;

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
            compression_type: sea_orm::Set("gzip".to_string()),
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
                    // Convert the error and send notification
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

                    return Err(BackupError::ExternalService(error_msg));
                }
            }
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

    async fn backup_database(&self, temp_file: &mut NamedTempFile) -> Result<()> {
        use sea_orm::{ConnectionTrait, DatabaseBackend};

        info!("Creating database backup");

        let backend = self.db.get_database_backend();
        match backend {
            DatabaseBackend::Postgres => self.backup_postgres_database(temp_file).await,
            _ => {
                anyhow::bail!(
                    "Database backup is currently supported only for SQLite and PostgreSQL"
                );
            }
        }
    }

    async fn backup_postgres_database(&self, temp_file: &mut NamedTempFile) -> Result<()> {
        use bollard::exec::{CreateExecOptions, StartExecResults};
        use bollard::models::ContainerCreateBody as Config;
        use bollard::query_parameters::RemoveContainerOptions;
        use bollard::Docker;
        use futures::stream::StreamExt as FuturesStreamExt;

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

        // Create a temporary container name
        let container_name = format!("temps-pg-backup-{}", uuid::Uuid::new_v4());

        // Prepare environment variables with proper lifetimes
        let pgpassword_env = format!("PGPASSWORD={}", password);
        let env_vars = vec![pgpassword_env];

        // Create container config with postgres image (includes pg_dump)
        // Use postgres:latest to ensure compatibility
        let config = Config {
            image: Some("postgres:latest".to_string()),
            cmd: Some(vec!["sleep".to_string(), "300".to_string()]), // Keep container alive for exec
            env: Some(env_vars),
            host_config: Some(bollard::models::HostConfig {
                network_mode: Some("host".to_string()), // Use host network to access database
                auto_remove: Some(true),
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

        // Start container
        docker
            .start_container(
                &container_name,
                Some(bollard::query_parameters::StartContainerOptionsBuilder::new().build()),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start container: {}", e))?;

        // Build pg_dump command with proper lifetimes
        let port_str = port.to_string();
        let pg_dump_cmd = vec![
            "pg_dump",
            "--format=custom",
            "--compress=0",
            "--no-password",
            "--host",
            host,
            "--port",
            &port_str,
            "--username",
            username,
            "--dbname",
            database,
        ];

        info!("Running pg_dump command in Docker container");

        // Create exec instance
        let pgpassword = format!("PGPASSWORD={}", password);
        let exec = docker
            .create_exec(
                &container_name,
                CreateExecOptions {
                    cmd: Some(pg_dump_cmd),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    env: Some(vec![pgpassword.as_str()]),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create exec: {}", e))?;

        // Start exec and collect output
        let mut output_data = Vec::new();
        let mut stderr_data = Vec::new();

        if let StartExecResults::Attached { mut output, .. } =
            docker.start_exec(&exec.id, None).await?
        {
            while let Some(chunk) = FuturesStreamExt::next(&mut output).await {
                match chunk {
                    Ok(bollard::container::LogOutput::StdOut { message }) => {
                        output_data.extend_from_slice(&message);
                    }
                    Ok(bollard::container::LogOutput::StdErr { message }) => {
                        stderr_data.extend_from_slice(&message);
                    }
                    Ok(bollard::container::LogOutput::Console { message }) => {
                        output_data.extend_from_slice(&message);
                    }
                    Err(e) => {
                        // Clean up container before returning error
                        let _ = docker
                            .remove_container(
                                &container_name,
                                Some(RemoveContainerOptions {
                                    force: true,
                                    ..Default::default()
                                }),
                            )
                            .await;
                        return Err(anyhow::anyhow!("Error reading pg_dump output: {}", e));
                    }
                    _ => {}
                }
            }
        } else {
            // Clean up container
            let _ = docker
                .remove_container(
                    &container_name,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;
            return Err(anyhow::anyhow!("Unexpected exec result type"));
        }

        // Check if command was successful
        let exec_inspect = docker.inspect_exec(&exec.id).await?;
        if let Some(exit_code) = exec_inspect.exit_code {
            if exit_code != 0 {
                let stderr = String::from_utf8_lossy(&stderr_data);
                // Clean up container
                let _ = docker
                    .remove_container(
                        &container_name,
                        Some(RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await;
                return Err(anyhow::anyhow!(
                    "pg_dump failed with exit code {}: {}",
                    exit_code,
                    stderr
                ));
            }
        }

        // Clean up container
        docker
            .remove_container(
                &container_name,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to remove container: {}", e))?;

        info!("Compressing PostgreSQL database backup");

        // Compress the backup data using gzip
        let mut encoder = GzEncoder::new(temp_file, Compression::default());
        std::io::Write::write_all(&mut encoder, &output_data)?;
        encoder.finish()?;

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
        let file_content = tokio::fs::read(temp_file.path()).await?;

        match s3_client
            .put_object()
            .bucket(&s3_source.bucket_name)
            .key(s3_location)
            .body(file_content.into())
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
            let chunk = chunk.context("Failed to read chunk from file")?;
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
            .ok_or_else(|| BackupError::NotFound("Backup not found".to_string()))?;

        // Get S3 source
        let s3_source = temps_entities::s3_sources::Entity::find_by_id(backup.s3_source_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound("S3 source not found".to_string()))?;

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
        use std::io::Read;

        info!("Restoring PostgreSQL backup: {}", backup.backup_id);

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

        // Write decompressed backup to a temporary file
        let mut temp_file = NamedTempFile::new()?;
        use std::io::Write;
        temp_file.write_all(&decompressed_data)?;
        temp_file.flush()?;

        // Get database URL from server configuration
        let database_url = &self.config_service.get_database_url();

        // Parse database URL to extract connection parameters
        let url = url::Url::parse(database_url)
            .map_err(|e| BackupError::Internal(format!("Invalid DATABASE_URL format: {}", e)))?;

        let host = url.host_str().unwrap_or("localhost");
        let port = url.port().unwrap_or(5432);
        let database = url.path().trim_start_matches('/');
        let username = url.username();
        let password = url.password();

        // Build pg_restore command
        let mut cmd = tokio::process::Command::new("pg_restore");
        cmd.arg("--verbose")
            .arg("--clean") // Drop existing objects before recreating
            .arg("--if-exists") // Don't error if objects don't exist
            .arg("--no-password")
            .arg("--host")
            .arg(host)
            .arg("--port")
            .arg(port.to_string())
            .arg("--username")
            .arg(username)
            .arg("--dbname")
            .arg(database)
            .arg(temp_file.path());

        // Set password via environment variable if provided
        if let Some(pwd) = password {
            cmd.env("PGPASSWORD", pwd);
        }

        info!("Running pg_restore command");
        let output = cmd.output().await.map_err(|e| {
            BackupError::Internal(format!(
                "Failed to execute pg_restore: {}. Make sure pg_restore is installed and in PATH",
                e
            ))
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(BackupError::Internal(format!(
                "pg_restore failed: {}",
                stderr
            )));
        }

        info!("PostgreSQL backup restored successfully");
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
            .ok_or_else(|| BackupError::NotFound("Backup not found".to_string()))?;

        let s3_source = temps_entities::s3_sources::Entity::find_by_id(backup.s3_source_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound("S3 source not found".to_string()))?;

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

        // Encrypt sensitive credentials before storing
        let encrypted_access_key = self
            .encryption_service
            .encrypt_string(&request.access_key_id)
            .map_err(|e| BackupError::Internal(format!("Failed to encrypt access key: {}", e)))?;

        let encrypted_secret_key = self
            .encryption_service
            .encrypt_string(&request.secret_key)
            .map_err(|e| BackupError::Internal(format!("Failed to encrypt secret key: {}", e)))?;

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
            .ok_or_else(|| BackupError::NotFound("S3 source not found".to_string()))?;

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
            .ok_or_else(|| BackupError::NotFound("S3 source not found".to_string()))?;

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
            .ok_or_else(|| BackupError::NotFound("Backup schedule not found".to_string()))?;

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
            .ok_or_else(|| BackupError::NotFound("S3 source not found".to_string()))?;

        // Create the backup
        let backup = self
            .create_backup(
                None, // No schedule associated
                s3_source_id,
                backup_type,
                created_by,
            )
            .await?;

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
            .ok_or_else(|| BackupError::NotFound("S3 source not found".to_string()))?;

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
            active.access_key_id = Set(access_key_id);
        }
        if let Some(secret_key) = request.secret_key {
            active.secret_key = Set(secret_key);
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
                "metadata_location": backup.s3_location.replace("backup.sqlite.gz", "metadata.json")
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
            .ok_or_else(|| BackupError::NotFound("S3 source not found".to_string()))?;

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
            .ok_or_else(|| BackupError::NotFound("S3 source not found".to_string()))?;

        // Create S3 client
        let s3_client = self
            .create_s3_client(&s3_source)
            .await
            .map_err(|e| BackupError::S3(e.to_string()))?;

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
                backup.clone(),
                &s3_source,
                &subpath,
                &subpath_root,
                &self.db,
                service,
                service_config,
            )
            .await
            .map_err(|e| BackupError::ExternalService(e.to_string()))?;
        info!("Backup created at location: {}", backup_location);
        // Get the external service backup record
        let external_backup = temps_entities::external_service_backups::Entity::find()
            .filter(temps_entities::external_service_backups::Column::BackupId.eq(backup.id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                BackupError::NotFound("External service backup record not found".to_string())
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
            .ok_or_else(|| BackupError::NotFound("Backup schedule not found".to_string()))?;

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
            .ok_or_else(|| BackupError::NotFound("Backup schedule not found".to_string()))?;

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
            .ok_or_else(|| BackupError::NotFound("Backup schedule not found".to_string()))?;

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
            Err(BackupError::NotFound(_)) => {}
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
            Err(BackupError::NotFound(_)) => {}
            _ => panic!("Expected NotFound error"),
        }
    }

    #[tokio::test]
    #[ignore] // Requires Docker (MinIO and PostgreSQL containers)
    async fn test_backup_to_minio_integration() {
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

        println!("\n✓ Integration test passed:");
        println!("  - Database container started");
        println!("  - MinIO container started");
        println!("  - Backup created with ID: {}", backup_result.id);
        println!(
            "  - Backup size: {} bytes",
            backup_result.size_bytes.unwrap_or(0)
        );
        println!("  - Objects in bucket: {}", object_count);
    }

    #[tokio::test]
    #[ignore] // Requires Docker (PostgreSQL container)
    async fn test_restore_postgres_from_url() {
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
        use temps_entities::types::ProjectType;
        let test_project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_name: Set(Some("test-repo".to_string())),
            repo_owner: Set(Some("test-owner".to_string())),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            git_url: Set(Some("https://github.com/test/repo".to_string())),
            project_type: Set(ProjectType::Server),
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
}
