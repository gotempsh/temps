// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use clap::{Args, Subcommand};
use colored::Colorize;
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::info;

#[derive(Deserialize, Serialize, Debug)]
struct BackupIndex {
    backups: Vec<BackupEntry>,
    last_updated: String,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct BackupEntry {
    id: i32,
    backup_id: String,
    name: String,
    #[serde(rename = "type")]
    backup_type: String,
    created_at: String,
    size_bytes: i64,
    location: String,
    metadata_location: String,
}

#[derive(Deserialize, Serialize, Debug)]
struct BackupMetadata {
    #[serde(default)]
    recovery_set_version: Option<u32>,
    #[serde(default)]
    complete: Option<bool>,
    backup_id: String,
    name: String,
    #[serde(rename = "type")]
    backup_type: String,
    created_at: String,
    size_bytes: i64,
    #[serde(default)]
    artifact_sha256: Option<String>,
    #[serde(default)]
    server_config: Option<String>,
    #[serde(default)]
    server_config_encrypted: Option<String>,
    #[serde(default)]
    server_config_encryption: Option<String>,
    #[serde(default)]
    manifest_authentication: Option<String>,
    external_service_backups: Vec<ExternalServiceBackup>,
}

#[derive(Deserialize, Serialize, Debug)]
struct ExternalServiceBackup {
    backup_id: i32,
    service_id: i32,
    s3_location: String,
    state: String,
    size_bytes: Option<i64>,
    #[serde(rename = "type")]
    backup_type: String,
    metadata: ExternalServiceMetadata,
}

#[derive(Deserialize, Serialize, Debug)]
struct ExternalServiceMetadata {
    service_type: String,
    service_name: String,
}

struct ExternalServiceRestoreInput<'a> {
    manager: &'a temps_providers::ExternalServiceManager,
    s3_client: &'a aws_sdk_s3::Client,
    s3_credentials: &'a temps_providers::S3Credentials,
    external_backup: &'a ExternalServiceBackup,
    backup_model: &'a temps_entities::backups::Model,
    service_model: &'a temps_entities::external_services::Model,
    database: &'a sea_orm::DatabaseConnection,
    decrypted_config: &'a str,
}

#[derive(Args)]
pub struct BackupCommand {
    #[command(subcommand)]
    command: BackupCommands,
}

#[derive(Subcommand)]
enum BackupCommands {
    /// List available backups from S3 bucket
    List(ListBackupsArgs),
    /// Restore a backup from S3 to database
    Restore(RestoreBackupArgs),
    /// Restore a specific external service from a backup
    RestoreService(RestoreServiceArgs),
}

#[derive(Args)]
struct ListBackupsArgs {
    /// S3 access key ID
    #[arg(long, env = "S3_ACCESS_KEY_ID")]
    access_key_id: String,

    /// S3 secret access key
    #[arg(long, env = "S3_SECRET_ACCESS_KEY", hide_env_values = true)]
    secret_access_key: String,

    /// S3 bucket name
    #[arg(long, env = "S3_BUCKET_NAME")]
    bucket_name: String,

    /// S3 bucket path/prefix (optional)
    #[arg(long, env = "S3_BUCKET_PATH", default_value = "backups")]
    bucket_path: String,

    /// S3 region
    #[arg(long, env = "S3_REGION", default_value = "us-east-1")]
    region: String,

    /// S3 endpoint URL (for MinIO/custom S3)
    #[arg(long, env = "S3_ENDPOINT")]
    endpoint: Option<String>,

    /// Force path style (needed for MinIO)
    #[arg(long, env = "S3_FORCE_PATH_STYLE", default_value = "true")]
    force_path_style: bool,
}

#[derive(Args)]
struct RestoreBackupArgs {
    /// Database connection URL to restore to (set via TEMPS_DATABASE_URL env var; not accepted as a flag to prevent credentials leaking into process listings)
    #[arg(long, env = "TEMPS_DATABASE_URL", hide_env_values = true)]
    database_url: String,

    /// Temps data directory whose recovery secrets will be restored
    #[arg(long, env = "TEMPS_DATA_DIR")]
    data_dir: Option<PathBuf>,

    /// S3 access key ID
    #[arg(long, env = "S3_ACCESS_KEY_ID")]
    access_key_id: String,

    /// S3 secret access key
    #[arg(long, env = "S3_SECRET_ACCESS_KEY", hide_env_values = true)]
    secret_access_key: String,

    /// S3 bucket name
    #[arg(long, env = "S3_BUCKET_NAME")]
    bucket_name: String,

    /// S3 bucket path/prefix (optional)
    #[arg(long, env = "S3_BUCKET_PATH", default_value = "backups")]
    bucket_path: String,

    /// Backup ID (UUID) from index.json to restore
    #[arg(long)]
    backup_id: String,

    /// Validate the recovery set and target prerequisites without changing data
    #[arg(long)]
    dry_run: bool,

    /// S3 region
    #[arg(long, env = "S3_REGION", default_value = "us-east-1")]
    region: String,

    /// S3 endpoint URL (for MinIO/custom S3)
    #[arg(long, env = "S3_ENDPOINT")]
    endpoint: Option<String>,

    /// Force path style (needed for MinIO)
    #[arg(long, env = "S3_FORCE_PATH_STYLE", default_value = "true")]
    force_path_style: bool,
}

#[derive(Args)]
struct RestoreServiceArgs {
    /// S3 access key ID
    #[arg(long, env = "S3_ACCESS_KEY_ID")]
    access_key_id: String,

    /// S3 secret access key
    #[arg(long, env = "S3_SECRET_ACCESS_KEY", hide_env_values = true)]
    secret_access_key: String,

    /// S3 bucket name
    #[arg(long, env = "S3_BUCKET_NAME")]
    bucket_name: String,

    /// S3 bucket path/prefix (optional)
    #[arg(long, env = "S3_BUCKET_PATH", default_value = "backups")]
    bucket_path: String,

    /// Backup ID (UUID) from index.json
    #[arg(long)]
    backup_id: String,

    /// Service name to restore (e.g., "postgres-heex", "s3-0fn9")
    #[arg(long)]
    service_name: String,

    /// Encryption key from the backup (required to decrypt service configs)
    #[arg(long, env = "TEMPS_ENCRYPTION_KEY", hide_env_values = true)]
    encryption_key: String,

    /// Database URL for the temps database (needed to query service config) (set via TEMPS_DATABASE_URL env var; not accepted as a flag to prevent credentials leaking into process listings)
    #[arg(long, env = "TEMPS_DATABASE_URL", hide_env_values = true)]
    database_url: String,

    /// S3 region
    #[arg(long, env = "S3_REGION", default_value = "us-east-1")]
    region: String,

    /// S3 endpoint URL (for MinIO/custom S3)
    #[arg(long, env = "S3_ENDPOINT")]
    endpoint: Option<String>,

    /// Force path style (needed for MinIO)
    #[arg(long, env = "S3_FORCE_PATH_STYLE", default_value = "true")]
    force_path_style: bool,
}

impl BackupCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        match self.command {
            BackupCommands::List(args) => Self::execute_list(args),
            BackupCommands::Restore(args) => Self::execute_restore(args),
            BackupCommands::RestoreService(args) => Self::execute_restore_service(args),
        }
    }

    fn execute_list(args: ListBackupsArgs) -> anyhow::Result<()> {
        info!("Listing backups from S3");

        // Create tokio runtime for async operations
        let rt = tokio::runtime::Runtime::new()?;

        // Create S3 client
        let s3_client = rt.block_on(Self::create_s3_client(
            &args.access_key_id,
            &args.secret_access_key,
            &args.region,
            args.endpoint.as_deref(),
            args.force_path_style,
        ))?;
        // Construct index.json key
        let index_key = if args.bucket_path.is_empty() {
            "index.json".to_string()
        } else {
            format!("{}/index.json", args.bucket_path.trim_matches('/'))
        };

        // Download index.json from S3
        let index_data = rt.block_on(async {
            let response = s3_client
                .get_object()
                .bucket(&args.bucket_name)
                .key(&index_key)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to download index.json from S3: {}. Make sure the file exists in the bucket.", e))?;

            let data = response.body.collect().await
                .map_err(|e| anyhow::anyhow!("Failed to read index.json data: {}", e))?;

            Ok::<Vec<u8>, anyhow::Error>(data.into_bytes().to_vec())
        })?;

        // Parse index.json
        let index: BackupIndex = serde_json::from_slice(&index_data)
            .map_err(|e| anyhow::anyhow!("Failed to parse index.json: {}", e))?;

        if index.backups.is_empty() {
            println!();
            println!("{}", "No backups found in index.json.".bright_yellow());
            println!();
            return Ok(());
        }

        // Sort backups by created_at in descending order (newest first)
        let mut sorted_backups = index.backups.clone();
        sorted_backups.sort_by(|a, b| b.created_at.cmp(&a.created_at));

        println!();
        println!(
            "{}",
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_green()
        );
        println!(
            "{}",
            format!(
                "   📦 Backups in s3://{}/{}",
                args.bucket_name, args.bucket_path
            )
            .bright_white()
            .bold()
        );
        println!(
            "   {} {}",
            "Last Updated:".bright_white(),
            index.last_updated.bright_white()
        );
        println!(
            "{}",
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_green()
        );
        println!();

        for backup in &sorted_backups {
            let size_mb = backup.size_bytes as f64 / (1024.0 * 1024.0);

            println!(
                "{} {} (ID: {})",
                "Backup:".bright_white().bold(),
                backup.name.bright_cyan(),
                backup.id.to_string().bright_yellow()
            );
            println!(
                "  {} {}",
                "Backup ID:".bright_white(),
                backup.backup_id.bright_cyan()
            );
            println!(
                "  {} {}",
                "Type:".bright_white(),
                backup.backup_type.bright_white()
            );
            println!(
                "  {} {}",
                "Created:".bright_white(),
                backup.created_at.bright_white()
            );
            println!("  {} {:.2} MB", "Size:".bright_white(), size_mb);
            println!(
                "  {} {}",
                "Location:".bright_white(),
                backup.location.bright_green()
            );
            println!();
        }

        println!(
            "{} {}",
            "Total backups:".bright_white().bold(),
            sorted_backups.len().to_string().bright_cyan()
        );
        println!();

        Ok(())
    }

    fn execute_restore(args: RestoreBackupArgs) -> anyhow::Result<()> {
        info!("Restoring backup from S3: {}", args.backup_id);

        // Create tokio runtime for async operations
        let rt = tokio::runtime::Runtime::new()?;

        // Create S3 client
        let s3_client = rt.block_on(Self::create_s3_client(
            &args.access_key_id,
            &args.secret_access_key,
            &args.region,
            args.endpoint.as_deref(),
            args.force_path_style,
        ))?;
        let s3_credentials = Self::restore_s3_credentials(
            &args.access_key_id,
            &args.secret_access_key,
            &args.region,
            args.endpoint.as_deref(),
            &args.bucket_name,
            args.force_path_style,
        );

        // Construct index.json key
        let index_key = if args.bucket_path.is_empty() {
            "index.json".to_string()
        } else {
            format!("{}/index.json", args.bucket_path.trim_matches('/'))
        };

        // Download and parse index.json
        println!("{}", "Reading backup index...".bright_white());
        let index_data = rt.block_on(async {
            let response = s3_client
                .get_object()
                .bucket(&args.bucket_name)
                .key(&index_key)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to download index.json from S3: {}", e))?;

            let data = response
                .body
                .collect()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to read index.json data: {}", e))?;

            Ok::<Vec<u8>, anyhow::Error>(data.into_bytes().to_vec())
        })?;

        let index: BackupIndex = serde_json::from_slice(&index_data)
            .map_err(|e| anyhow::anyhow!("Failed to parse index.json: {}", e))?;

        // Find the backup by backup_id
        let backup = index
            .backups
            .iter()
            .find(|b| b.backup_id == args.backup_id)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Backup with ID '{}' not found in index.json",
                    args.backup_id
                )
            })?;

        // Download and parse metadata.json
        println!(
            "{}",
            format!("Reading backup metadata from: {}", backup.metadata_location).bright_white()
        );
        let metadata_key = backup.metadata_location.trim_start_matches('/').to_string();
        let binding = metadata_key
            .replace("backup.sql.gz", "metadata.json")
            .replace("backup.postgresql.gz", "metadata.json");
        let metadata_key = binding;
        let metadata_key = metadata_key.as_str();
        let metadata_data = rt.block_on(async {
            let response = s3_client
                .get_object()
                .bucket(&args.bucket_name)
                .key(metadata_key)
                .send()
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to download metadata.json from S3 with key {}: {}",
                        metadata_key,
                        e
                    )
                })?;

            let data = response
                .body
                .collect()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to read metadata.json data: {}", e))?;

            Ok::<Vec<u8>, anyhow::Error>(data.into_bytes().to_vec())
        })?;

        let metadata_value: serde_json::Value =
            serde_json::from_slice(&metadata_data).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to parse metadata.json Path: {} Error: {}",
                    metadata_key,
                    e
                )
            })?;
        Self::verify_manifest_authentication(
            &metadata_value,
            &backup.backup_id,
            &args.secret_access_key,
        )?;
        let metadata: BackupMetadata = serde_json::from_value(metadata_value).map_err(|e| {
            anyhow::anyhow!(
                "Failed to decode recovery metadata from {}: {}",
                metadata_key,
                e
            )
        })?;
        Self::validate_recovery_set(&metadata, &backup.backup_id)?;
        let server_config = Self::resolve_server_config(&metadata, &args.secret_access_key)?;
        let encryption_key = Self::extract_encryption_key(&server_config)?;
        let auth_secret = Self::extract_auth_secret(&server_config)?;
        let data_dir = Self::resolve_data_dir(args.data_dir.as_deref())?;

        if !args.database_url.starts_with("postgres://")
            && !args.database_url.starts_with("postgresql://")
        {
            return Err(anyhow::anyhow!(
                "Only PostgreSQL databases are supported. Database URL must start with postgres:// or postgresql://"
            ));
        }

        rt.block_on(Self::validate_restore_dry_run(
            &args.database_url,
            &s3_client,
            &args.bucket_name,
            backup,
            &metadata,
            &data_dir,
        ))?;

        if args.dry_run {
            println!();
            println!(
                "{}",
                "✓ Dry run passed: recovery set and target prerequisites are valid"
                    .bright_green()
                    .bold()
            );
            println!("{}", "No data was changed.".bright_white());
            println!();
            return Ok(());
        }

        println!(
            "{}",
            format!(
                "✓ Found {} external service backups",
                metadata.external_service_backups.len()
            )
            .bright_green()
        );
        println!();

        // Display backup information and confirmation
        println!();
        println!(
            "{}",
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_yellow()
        );
        println!("{}", "   ⚠️  Restore Backup from S3".bright_white().bold());
        println!(
            "{}",
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_yellow()
        );
        println!();
        println!(
            "{} {}",
            "Backup ID:".bright_white().bold(),
            backup.backup_id.bright_cyan()
        );
        println!(
            "{} {}",
            "Backup Name:".bright_white(),
            backup.name.bright_white()
        );
        println!(
            "{} {}",
            "Created:".bright_white(),
            backup.created_at.bright_white()
        );
        println!(
            "{} {:.2} MB",
            "Size:".bright_white(),
            backup.size_bytes as f64 / (1024.0 * 1024.0)
        );
        println!(
            "{} {}",
            "Location:".bright_white(),
            backup.location.bright_green()
        );
        println!(
            "{} {}",
            "Target Database:".bright_white(),
            Self::redact_database_url(&args.database_url).bright_white()
        );
        println!();
        println!(
            "{}",
            "⚠️  WARNING: This will restore the backup to the specified database!"
                .bright_yellow()
                .bold()
        );
        println!(
            "{}",
            "This operation may overwrite existing data.".bright_yellow()
        );
        println!();

        // Ask for confirmation
        print!(
            "{} ",
            "Are you sure you want to continue? (y/n):"
                .bright_white()
                .bold()
        );
        use std::io::{self, Write};
        io::stdout().flush()?;

        let mut response = String::new();
        io::stdin().read_line(&mut response)?;
        let response = response.trim().to_lowercase();

        if response != "y" && response != "yes" {
            println!();
            println!("{}", "Restore cancelled.".bright_yellow());
            println!();
            return Ok(());
        }

        println!();
        println!("{}", "Starting restore...".bright_white());

        // Use the location from index.json (strip leading slash if present)
        let backup_key = backup.location.trim_start_matches('/');

        // Download backup from S3 to temporary location
        println!("{}", "Downloading backup from S3...".bright_white());
        let compressed_backup = tempfile::Builder::new()
            .prefix("temps_restore_")
            .suffix(".backup")
            .tempfile()
            .map_err(|e| anyhow::anyhow!("Failed to create restore temp file: {}", e))?;
        let backup_file_path = compressed_backup.path().to_path_buf();

        rt.block_on(async {
            use tokio::io::AsyncWriteExt;

            let response = s3_client
                .get_object()
                .bucket(&args.bucket_name)
                .key(backup_key)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to download backup from S3: {}", e))?;

            let mut reader = response.body.into_async_read();
            let mut output = tokio::fs::File::create(&backup_file_path)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create backup file: {}", e))?;
            tokio::io::copy(&mut reader, &mut output)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to stream backup data to disk: {}", e))?;
            output
                .flush()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to flush backup file: {}", e))?;

            Ok::<(), anyhow::Error>(())
        })?;

        println!("{}", "✓ Backup downloaded successfully".bright_green());
        Self::verify_control_plane_artifact(&metadata, &backup_file_path)?;
        println!();

        // Check if backup is gzipped and decompress if needed
        let mut decompressed_backup = None;
        let final_backup_path = if backup_key.ends_with(".gz") {
            println!("{}", "Decompressing backup file...".bright_white());
            let mut output = tempfile::Builder::new()
                .prefix("temps_restore_decompressed_")
                .suffix(".backup")
                .tempfile()
                .map_err(|e| anyhow::anyhow!("Failed to create decompressed temp file: {}", e))?;

            let gz_file = std::fs::File::open(&backup_file_path)
                .map_err(|e| anyhow::anyhow!("Failed to open gzipped backup: {}", e))?;
            let mut decoder = GzDecoder::new(BufReader::new(gz_file));
            let mut writer = BufWriter::new(output.as_file_mut());
            let decompressed_size = std::io::copy(&mut decoder, &mut writer)
                .map_err(|e| anyhow::anyhow!("Failed to decompress backup: {}", e))?;
            std::io::Write::flush(&mut writer)
                .map_err(|e| anyhow::anyhow!("Failed to flush decompressed backup: {}", e))?;
            drop(writer);

            println!(
                "{}",
                format!(
                    "✓ Backup decompressed successfully ({:.2} MB)",
                    decompressed_size as f64 / (1024.0 * 1024.0)
                )
                .bright_green()
            );
            let decompressed_path = output.path().to_path_buf();
            decompressed_backup = Some(output);
            decompressed_path
        } else {
            backup_file_path.clone()
        };

        println!("{}", "Restoring PostgreSQL database...".bright_white());
        let is_plain_sql = backup_key.ends_with(".sql.gz") || backup_key.ends_with(".sql");
        Self::restore_postgres(&args.database_url, &final_backup_path, is_plain_sql)?;
        drop(decompressed_backup);

        // Restore external services
        rt.block_on(Self::restore_external_services(
            &args.database_url,
            &s3_client,
            &s3_credentials,
            &metadata,
            &encryption_key,
        ))?;
        Self::install_recovery_secrets(&data_dir, &encryption_key, &auth_secret)?;

        println!();
        println!(
            "{}",
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_green()
        );
        println!(
            "{}",
            "   ✅ Backup restored successfully!".bright_green().bold()
        );
        println!(
            "{}",
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_green()
        );
        println!();

        Ok(())
    }

    async fn create_s3_client(
        access_key_id: &str,
        secret_access_key: &str,
        region: &str,
        endpoint: Option<&str>,
        force_path_style: bool,
    ) -> anyhow::Result<aws_sdk_s3::Client> {
        use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
        use aws_sdk_s3::Config;

        let creds = Credentials::new(
            access_key_id,
            secret_access_key,
            None,
            None,
            "temps-cli-backup",
        );

        let mut config_builder = Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(region.to_string()))
            .force_path_style(force_path_style)
            .credentials_provider(creds);

        if let Some(endpoint_url) = endpoint {
            config_builder = config_builder.endpoint_url(endpoint_url);
        }

        Ok(aws_sdk_s3::Client::from_conf(config_builder.build()))
    }

    fn restore_s3_credentials(
        access_key_id: &str,
        secret_access_key: &str,
        region: &str,
        endpoint: Option<&str>,
        bucket_name: &str,
        force_path_style: bool,
    ) -> temps_providers::S3Credentials {
        temps_providers::S3Credentials {
            access_key_id: access_key_id.to_string(),
            secret_key: secret_access_key.to_string(),
            region: region.to_string(),
            endpoint: endpoint.map(str::to_owned),
            bucket_name: bucket_name.to_string(),
            // Recovery entries already carry their complete object key or
            // WAL-G prefix, so engines must not prepend the index directory.
            bucket_path: String::new(),
            force_path_style,
        }
    }

    async fn validate_restore_dry_run(
        database_url: &str,
        s3_client: &aws_sdk_s3::Client,
        bucket_name: &str,
        backup: &BackupEntry,
        metadata: &BackupMetadata,
        data_dir: &Path,
    ) -> anyhow::Result<()> {
        use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement};

        println!("{}", "Validating recovery artifacts...".bright_white());
        Self::validate_s3_artifact(s3_client, bucket_name, &backup.location).await?;
        let control_plane_sha256 =
            Self::sha256_s3_object(s3_client, bucket_name, &backup.location).await?;
        Self::verify_control_plane_digest(metadata, &control_plane_sha256)?;
        for external in &metadata.external_service_backups {
            Self::validate_s3_artifact(s3_client, bucket_name, &external.s3_location).await?;
        }

        println!("{}", "Validating Docker access...".bright_white());
        let docker = bollard::Docker::connect_with_local_defaults()
            .map_err(|error| anyhow::anyhow!("Failed to connect to Docker: {}", error))?;
        docker
            .version()
            .await
            .map_err(|error| anyhow::anyhow!("Docker is not reachable: {}", error))?;

        let restore_tool =
            if backup.location.ends_with(".sql.gz") || backup.location.ends_with(".sql") {
                "psql"
            } else {
                "pg_restore"
            };
        let tool_status = std::process::Command::new(restore_tool)
            .arg("--version")
            .status()
            .map_err(|error| {
                anyhow::anyhow!(
                    "{} is required for this backup format but could not be executed: {}",
                    restore_tool,
                    error
                )
            })?;
        if !tool_status.success() {
            return Err(anyhow::anyhow!(
                "{} --version exited unsuccessfully",
                restore_tool
            ));
        }

        println!(
            "{}",
            "Validating target TimescaleDB connection...".bright_white()
        );
        let db = Database::connect(database_url).await.map_err(|error| {
            anyhow::anyhow!(
                "Failed to connect to target control-plane database: {}",
                error
            )
        })?;
        db.query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT 1".to_string(),
        ))
        .await
        .map_err(|error| {
            anyhow::anyhow!("Target control-plane database is not reachable: {}", error)
        })?;
        let timescale = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT extversion FROM pg_extension WHERE extname = 'timescaledb'".to_string(),
            ))
            .await
            .map_err(|error| {
                anyhow::anyhow!("Failed to inspect target TimescaleDB extension: {}", error)
            })?;
        if timescale.is_none() {
            return Err(anyhow::anyhow!(
                "Target control-plane database does not have the TimescaleDB extension installed"
            ));
        }

        let secret_parent = Self::recovery_secret_parent(data_dir)?;
        let secret_parent_metadata = std::fs::metadata(secret_parent).map_err(|error| {
            anyhow::anyhow!(
                "Cannot inspect recovery-secret directory '{}': {}",
                secret_parent.display(),
                error
            )
        })?;
        if secret_parent_metadata.permissions().readonly() {
            return Err(anyhow::anyhow!(
                "Recovery-secret directory '{}' is read-only",
                secret_parent.display()
            ));
        }

        Ok(())
    }

    async fn sha256_s3_object(
        s3_client: &aws_sdk_s3::Client,
        bucket_name: &str,
        location: &str,
    ) -> anyhow::Result<String> {
        use sha2::{Digest, Sha256};
        use tokio::io::AsyncReadExt;

        let key = Self::s3_key_from_location(location, bucket_name)?;
        let response = s3_client
            .get_object()
            .bucket(bucket_name)
            .key(&key)
            .send()
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "Failed to download control-plane backup for integrity verification: {}",
                    error
                )
            })?;
        let mut reader = response.body.into_async_read();
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let read = reader.read(&mut buffer).await.map_err(|error| {
                anyhow::anyhow!("Failed to verify control-plane backup: {}", error)
            })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(hex::encode(hasher.finalize()))
    }

    fn recovery_secret_parent(data_dir: &Path) -> anyhow::Result<&Path> {
        match std::fs::symlink_metadata(data_dir) {
            Ok(metadata) => {
                if !metadata.file_type().is_dir() {
                    return Err(anyhow::anyhow!(
                        "Temps data directory '{}' exists but is not a directory or is a symlink",
                        data_dir.display()
                    ));
                }
                Ok(data_dir)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                data_dir.parent().ok_or_else(|| {
                    anyhow::anyhow!(
                        "Temps data directory '{}' has no parent directory",
                        data_dir.display()
                    )
                })
            }
            Err(error) => Err(anyhow::anyhow!(
                "Cannot inspect Temps data directory '{}': {}",
                data_dir.display(),
                error
            )),
        }
    }

    async fn validate_s3_artifact(
        s3_client: &aws_sdk_s3::Client,
        bucket_name: &str,
        location: &str,
    ) -> anyhow::Result<()> {
        let key = Self::s3_key_from_location(location, bucket_name)?;
        let last_segment = key.rsplit('/').next().unwrap_or_default();
        let looks_like_file = last_segment.contains('.');

        if looks_like_file {
            s3_client
                .head_object()
                .bucket(bucket_name)
                .key(&key)
                .send()
                .await
                .map_err(|error| {
                    anyhow::anyhow!(
                        "Required recovery object s3://{}/{} is unavailable: {}",
                        bucket_name,
                        key,
                        error
                    )
                })?;
        } else {
            let prefix = format!("{}/", key.trim_end_matches('/'));
            let response = s3_client
                .list_objects_v2()
                .bucket(bucket_name)
                .prefix(&prefix)
                .max_keys(1)
                .send()
                .await
                .map_err(|error| {
                    anyhow::anyhow!(
                        "Failed to inspect recovery prefix s3://{}/{}: {}",
                        bucket_name,
                        prefix,
                        error
                    )
                })?;
            if response.key_count().unwrap_or_default() == 0 {
                return Err(anyhow::anyhow!(
                    "Required recovery prefix s3://{}/{} is empty",
                    bucket_name,
                    prefix
                ));
            }
        }
        Ok(())
    }

    fn s3_key_from_location(location: &str, expected_bucket: &str) -> anyhow::Result<String> {
        let location = location.trim();
        if let Some(value) = location.strip_prefix("s3://") {
            let (bucket, key) = value
                .split_once('/')
                .ok_or_else(|| anyhow::anyhow!("Invalid S3 recovery location '{}'", location))?;
            if bucket != expected_bucket {
                return Err(anyhow::anyhow!(
                    "Recovery location bucket '{}' does not match configured bucket '{}'",
                    bucket,
                    expected_bucket
                ));
            }
            if key.trim_matches('/').is_empty() {
                return Err(anyhow::anyhow!(
                    "Recovery location '{}' has no object key",
                    location
                ));
            }
            return Ok(key.trim_start_matches('/').to_string());
        }
        let key = location.trim_start_matches('/');
        if key.is_empty() {
            return Err(anyhow::anyhow!("Recovery location is empty"));
        }
        Ok(key.to_string())
    }

    fn validate_recovery_set(
        metadata: &BackupMetadata,
        expected_backup_id: &str,
    ) -> anyhow::Result<()> {
        if metadata.backup_id != expected_backup_id {
            return Err(anyhow::anyhow!(
                "Recovery metadata identity mismatch: index selected '{}', metadata contains '{}'",
                expected_backup_id,
                metadata.backup_id
            ));
        }
        match metadata.recovery_set_version.unwrap_or(1) {
            1 => {
                return Err(anyhow::anyhow!(
                    "Recovery set '{}' uses the legacy unauthenticated manifest format; refusing a whole-instance restore",
                    metadata.backup_id
                ));
            }
            2 if metadata.complete == Some(true) => {}
            2 => {
                return Err(anyhow::anyhow!(
                    "Recovery set '{}' is not marked complete; refusing a whole-instance restore",
                    metadata.backup_id
                ));
            }
            version => {
                return Err(anyhow::anyhow!(
                    "Recovery set '{}' uses unsupported manifest version {}",
                    metadata.backup_id,
                    version
                ));
            }
        }

        let incomplete_services = metadata
            .external_service_backups
            .iter()
            .filter(|backup| backup.state != "completed" || backup.s3_location.trim().is_empty())
            .map(|backup| backup.metadata.service_name.as_str())
            .collect::<Vec<_>>();
        if !incomplete_services.is_empty() {
            return Err(anyhow::anyhow!(
                "Recovery set '{}' has incomplete external-service backups: {}",
                metadata.backup_id,
                incomplete_services.join(", ")
            ));
        }
        Ok(())
    }

    fn verify_manifest_authentication(
        metadata: &serde_json::Value,
        expected_backup_id: &str,
        s3_secret_access_key: &str,
    ) -> anyhow::Result<()> {
        let version = metadata
            .get("recovery_set_version")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1);
        if version == 1 {
            return Ok(());
        }
        if version != 2 {
            return Err(anyhow::anyhow!(
                "Recovery set '{}' uses unsupported manifest version {}",
                expected_backup_id,
                version
            ));
        }

        let authentication = metadata
            .get("manifest_authentication")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Recovery set '{}' has no authenticated manifest",
                    expected_backup_id
                )
            })?;
        let mut unsigned = metadata.clone();
        unsigned
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("Recovery metadata must be a JSON object"))?
            .remove("manifest_authentication");
        let expected_payload = serde_json::to_string(&unsigned)?;
        let authenticated_payload =
            temps_core::EncryptionService::new_from_password(s3_secret_access_key)
                .decrypt_string(authentication)
                .map_err(|error| {
                    anyhow::anyhow!(
                        "Recovery manifest authentication failed for '{}': {}",
                        expected_backup_id,
                        error
                    )
                })?;
        if authenticated_payload != expected_payload {
            return Err(anyhow::anyhow!(
                "Recovery manifest authentication failed for '{}': content mismatch",
                expected_backup_id
            ));
        }
        Ok(())
    }

    fn resolve_server_config(
        metadata: &BackupMetadata,
        s3_secret_access_key: &str,
    ) -> anyhow::Result<String> {
        if let Some(ciphertext) = metadata.server_config_encrypted.as_deref() {
            let method = metadata
                .server_config_encryption
                .as_deref()
                .unwrap_or("aes-256-gcm+s3-secret-sha256");
            if method != "aes-256-gcm+s3-secret-sha256" {
                return Err(anyhow::anyhow!(
                    "Recovery set '{}' uses unsupported server-config encryption '{}'",
                    metadata.backup_id,
                    method
                ));
            }
            return temps_core::EncryptionService::new_from_password(s3_secret_access_key)
                .decrypt_string(ciphertext)
                .map_err(|error| {
                    anyhow::anyhow!(
                        "Failed to decrypt server configuration for recovery set '{}': {}",
                        metadata.backup_id,
                        error
                    )
                });
        }

        if metadata.recovery_set_version == Some(2) {
            return Err(anyhow::anyhow!(
                "Recovery set '{}' is version 2 but has no encrypted server configuration",
                metadata.backup_id
            ));
        }

        metadata
            .server_config
            .as_deref()
            .filter(|config| !config.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Recovery set '{}' has no restorable server configuration",
                    metadata.backup_id
                )
            })
    }

    fn redact_database_url(database_url: &str) -> String {
        let Ok(mut url) = url::Url::parse(database_url) else {
            return "<invalid database URL>".to_string();
        };
        if url.password().is_some() {
            let _ = url.set_password(Some("REDACTED"));
        }
        url.set_query(None);
        url.set_fragment(None);
        url.to_string()
    }

    fn verify_control_plane_artifact(
        metadata: &BackupMetadata,
        backup_file: &Path,
    ) -> anyhow::Result<()> {
        use sha2::{Digest, Sha256};
        use std::io::Read;

        let file = std::fs::File::open(backup_file).map_err(|error| {
            anyhow::anyhow!(
                "Failed to open downloaded control-plane backup for verification: {}",
                error
            )
        })?;
        let mut reader = BufReader::new(file);
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let read = reader.read(&mut buffer).map_err(|error| {
                anyhow::anyhow!("Failed to verify control-plane backup: {}", error)
            })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let actual = hex::encode(hasher.finalize());
        Self::verify_control_plane_digest(metadata, &actual)
    }

    fn verify_control_plane_digest(metadata: &BackupMetadata, actual: &str) -> anyhow::Result<()> {
        let Some(expected) = metadata.artifact_sha256.as_deref() else {
            if metadata.recovery_set_version == Some(2) {
                return Err(anyhow::anyhow!(
                    "Recovery set '{}' has no authenticated control-plane digest",
                    metadata.backup_id
                ));
            }
            return Ok(());
        };
        if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(anyhow::anyhow!(
                "Recovery set '{}' has an invalid control-plane SHA-256 digest",
                metadata.backup_id
            ));
        }

        if !actual.eq_ignore_ascii_case(expected) {
            return Err(anyhow::anyhow!(
                "Control-plane backup integrity verification failed for recovery set '{}'",
                metadata.backup_id
            ));
        }
        Ok(())
    }

    fn restore_postgres(
        database_url: &str,
        backup_file: &Path,
        is_plain_sql: bool,
    ) -> anyhow::Result<()> {
        use url::Url;

        let url = Url::parse(database_url)
            .map_err(|e| anyhow::anyhow!("Failed to parse database URL: {}", e))?;

        let host = url.host_str().unwrap_or("localhost");
        let port = url.port().unwrap_or(5432);
        let database = url.path().trim_start_matches('/');
        let username = url.username();
        let password = url.password();

        let tool = if is_plain_sql { "psql" } else { "pg_restore" };
        let mut cmd = std::process::Command::new(tool);
        if is_plain_sql {
            // Current control-plane backups are pg_dumpall globals followed
            // by a plain pg_dump stream. `pg_restore` cannot read that format.
            cmd.arg("--no-password")
                .arg("--set=ON_ERROR_STOP=on")
                .arg("--host")
                .arg(host)
                .arg("--port")
                .arg(port.to_string())
                .arg("--username")
                .arg(username)
                .arg("--dbname")
                .arg(database)
                .arg("--file")
                .arg(backup_file);
        } else {
            cmd.arg("--verbose")
                .arg("--clean")
                .arg("--if-exists")
                .arg("--no-owner")
                .arg("--no-acl")
                .arg("--dbname")
                .arg(database)
                .arg("--host")
                .arg(host)
                .arg("--port")
                .arg(port.to_string())
                .arg("--username")
                .arg(username)
                .arg(backup_file);
        }

        if let Some(pwd) = password {
            cmd.env("PGPASSWORD", pwd);
        }

        println!("{}", format!("Running {}...", tool).bright_white());
        let output = cmd.output().map_err(|e| {
            anyhow::anyhow!(
                "Failed to execute {}: {}. Make sure the PostgreSQL client tools are installed and in PATH",
                tool,
                e,
            )
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("{} failed: {}", tool, stderr));
        }

        println!(
            "{}",
            "✓ PostgreSQL database restored successfully".bright_green()
        );
        Ok(())
    }

    fn extract_encryption_key(server_config: &str) -> anyhow::Result<String> {
        Self::extract_server_config_secret(server_config, "encryption_key")
    }

    fn extract_auth_secret(server_config: &str) -> anyhow::Result<String> {
        Self::extract_server_config_secret(server_config, "auth_secret")
    }

    fn extract_server_config_secret(server_config: &str, field: &str) -> anyhow::Result<String> {
        let value = serde_yaml::from_str::<serde_yaml::Value>(server_config).map_err(|error| {
            anyhow::anyhow!("Failed to parse recovery server configuration: {}", error)
        })?;
        value
            .get(field)
            .and_then(serde_yaml::Value::as_str)
            .map(str::trim)
            .filter(|secret| !secret.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("{} not found in recovery server configuration", field))
    }

    fn resolve_data_dir(configured: Option<&Path>) -> anyhow::Result<PathBuf> {
        match configured {
            Some(path) => Ok(path.to_path_buf()),
            None => dirs::home_dir()
                .map(|home| home.join(".temps"))
                .ok_or_else(|| anyhow::anyhow!("Could not determine the Temps data directory")),
        }
    }

    fn install_recovery_secrets(
        data_dir: &Path,
        encryption_key: &str,
        auth_secret: &str,
    ) -> anyhow::Result<()> {
        Self::validate_recovery_secret_value("encryption_key", encryption_key)?;
        Self::validate_recovery_secret_value("auth_secret", auth_secret)?;
        match std::fs::symlink_metadata(data_dir) {
            Ok(metadata) if !metadata.file_type().is_dir() => {
                return Err(anyhow::anyhow!(
                    "Temps data directory '{}' exists but is not a directory or is a symlink",
                    data_dir.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(data_dir).map_err(|error| {
                    anyhow::anyhow!(
                        "Failed to create Temps data directory '{}': {}",
                        data_dir.display(),
                        error
                    )
                })?;
            }
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "Failed to inspect Temps data directory '{}': {}",
                    data_dir.display(),
                    error
                ));
            }
        }
        Self::restrict_data_dir_permissions(data_dir)?;
        Self::install_recovery_secret(data_dir, "encryption_key", encryption_key)?;
        Self::install_recovery_secret(data_dir, "auth_secret", auth_secret)?;
        println!(
            "{}",
            format!(
                "✓ Restored source instance secrets into {}",
                data_dir.display()
            )
            .bright_green()
        );
        Ok(())
    }

    fn install_recovery_secret(data_dir: &Path, name: &str, secret: &str) -> anyhow::Result<()> {
        let target = data_dir.join(name);
        let normalized = secret.trim();
        Self::validate_recovery_secret_value(name, secret)?;

        match std::fs::symlink_metadata(&target) {
            Ok(metadata) => {
                if !metadata.file_type().is_file() {
                    return Err(anyhow::anyhow!(
                        "Recovery secret '{}' exists but is not a regular file",
                        target.display()
                    ));
                }
                let current = std::fs::read_to_string(&target).map_err(|error| {
                    anyhow::anyhow!(
                        "Failed to read existing recovery secret '{}': {}",
                        target.display(),
                        error
                    )
                })?;
                if current.trim() == normalized {
                    Self::restrict_secret_permissions(&target)?;
                    return Ok(());
                }
                let backup = data_dir.join(format!(
                    "{}.pre-restore-{}-{}",
                    name,
                    chrono::Utc::now().format("%Y%m%d%H%M%S"),
                    uuid::Uuid::new_v4()
                ));
                let mut backup_file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&backup)
                    .map_err(|error| {
                        anyhow::anyhow!(
                            "Failed to create recovery-secret rollback file '{}': {}",
                            backup.display(),
                            error
                        )
                    })?;
                Self::restrict_secret_permissions(&backup)?;
                let mut source = std::fs::File::open(&target).map_err(|error| {
                    anyhow::anyhow!(
                        "Failed to open existing recovery secret '{}': {}",
                        target.display(),
                        error
                    )
                })?;
                std::io::copy(&mut source, &mut backup_file).map_err(|error| {
                    anyhow::anyhow!(
                        "Failed to preserve existing recovery secret '{}' at '{}': {}",
                        target.display(),
                        backup.display(),
                        error
                    )
                })?;
                std::io::Write::flush(&mut backup_file).map_err(|error| {
                    anyhow::anyhow!(
                        "Failed to flush recovery-secret rollback file '{}': {}",
                        backup.display(),
                        error
                    )
                })?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "Failed to inspect recovery secret '{}': {}",
                    target.display(),
                    error
                ));
            }
        }

        let mut temporary = tempfile::NamedTempFile::new_in(data_dir).map_err(|error| {
            anyhow::anyhow!(
                "Failed to create temporary recovery secret in '{}': {}",
                data_dir.display(),
                error
            )
        })?;
        std::io::Write::write_all(temporary.as_file_mut(), normalized.as_bytes()).map_err(
            |error| anyhow::anyhow!("Failed to write recovery secret '{}': {}", name, error),
        )?;
        temporary.as_file_mut().sync_all().map_err(|error| {
            anyhow::anyhow!("Failed to sync recovery secret '{}': {}", name, error)
        })?;
        Self::restrict_secret_permissions(temporary.path())?;
        temporary.persist(&target).map_err(|error| {
            anyhow::anyhow!(
                "Failed to atomically install recovery secret '{}': {}",
                target.display(),
                error.error
            )
        })?;
        Self::restrict_secret_permissions(&target)?;
        Ok(())
    }

    fn validate_recovery_secret_value(name: &str, secret: &str) -> anyhow::Result<()> {
        let normalized = secret.trim();
        if normalized.is_empty() || normalized.chars().any(|value| matches!(value, '\r' | '\n')) {
            return Err(anyhow::anyhow!(
                "Recovery secret '{}' is empty or contains a newline",
                name
            ));
        }
        Ok(())
    }

    #[cfg(unix)]
    fn restrict_data_dir_permissions(path: &Path) -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(|error| {
            anyhow::anyhow!(
                "Failed to set owner-only permissions on Temps data directory '{}': {}",
                path.display(),
                error
            )
        })
    }

    #[cfg(not(unix))]
    fn restrict_data_dir_permissions(_path: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    #[cfg(unix)]
    fn restrict_secret_permissions(path: &Path) -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|error| {
            anyhow::anyhow!(
                "Failed to set owner-only permissions on '{}': {}",
                path.display(),
                error
            )
        })
    }

    #[cfg(not(unix))]
    fn restrict_secret_permissions(_path: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    async fn restore_external_services(
        database_url: &str,
        s3_client: &aws_sdk_s3::Client,
        s3_credentials: &temps_providers::S3Credentials,
        metadata: &BackupMetadata,
        encryption_key: &str,
    ) -> anyhow::Result<()> {
        use sea_orm::{Database, EntityTrait};
        use temps_core::EncryptionService;
        use temps_entities::{backups, external_services};

        if metadata.external_service_backups.is_empty() {
            println!("{}", "No external services to restore".bright_white());
            return Ok(());
        }

        println!();
        println!(
            "{}",
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_cyan()
        );
        println!("{}", "   Restoring External Services".bright_white().bold());
        println!(
            "{}",
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_cyan()
        );
        println!();

        // Connect to the restored database
        println!("{}", "Connecting to restored database...".bright_white());
        let db = Arc::new(
            Database::connect(database_url)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to connect to restored database: {}", e))?,
        );

        // Create encryption service
        let encryption_service = Arc::new(
            EncryptionService::new(encryption_key)
                .map_err(|e| anyhow::anyhow!("Failed to create encryption service: {}", e))?,
        );

        // Create Docker client and ExternalServiceManager
        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults()
                .map_err(|e| anyhow::anyhow!("Failed to connect to Docker: {}", e))?,
        );

        let manager = temps_providers::ExternalServiceManager::new(
            db.clone(),
            encryption_service.clone(),
            docker,
            std::sync::Arc::new(temps_providers::DnsRegistry::new(db.clone())),
        );

        // Query all external services from the restored database
        let all_services = external_services::Entity::find()
            .all(db.as_ref())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query external services: {}", e))?;

        // Create a map of service_id -> service for quick lookup
        let services_map: std::collections::HashMap<
            i32,
            &temps_entities::external_services::Model,
        > = all_services.iter().map(|s| (s.id, s)).collect();

        // Process each external service backup
        for ext_backup in &metadata.external_service_backups {
            if ext_backup.state != "completed" {
                println!(
                    "{} {} {}",
                    "Skipping".bright_yellow(),
                    ext_backup.metadata.service_name.bright_white(),
                    format!("(state: {})", ext_backup.state).bright_yellow()
                );
                continue;
            }

            println!(
                "{} {} {}",
                "Restoring".bright_white(),
                ext_backup.metadata.service_name.bright_cyan(),
                format!("({})", ext_backup.metadata.service_type).bright_white()
            );

            // Get service config from database
            let service = services_map.get(&ext_backup.service_id).ok_or_else(|| {
                anyhow::anyhow!(
                    "Service ID {} not found in restored database",
                    ext_backup.service_id
                )
            })?;
            if service.name != ext_backup.metadata.service_name
                || service.service_type != ext_backup.metadata.service_type
            {
                return Err(anyhow::anyhow!(
                    "Recovery manifest service identity mismatch for id {}: manifest is '{}/{}', restored control plane is '{}/{}'",
                    ext_backup.service_id,
                    ext_backup.metadata.service_name,
                    ext_backup.metadata.service_type,
                    service.name,
                    service.service_type,
                ));
            }

            let backup_model = backups::Entity::find_by_id(ext_backup.backup_id)
                .one(db.as_ref())
                .await
                .map_err(|error| {
                    anyhow::anyhow!(
                        "Failed to query backup row {} for service '{}': {}",
                        ext_backup.backup_id,
                        ext_backup.metadata.service_name,
                        error
                    )
                })?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Backup row {} for service '{}' is absent from the restored control plane",
                        ext_backup.backup_id,
                        ext_backup.metadata.service_name
                    )
                })?;
            if backup_model.state != "completed"
                || backup_model.s3_location != ext_backup.s3_location
            {
                return Err(anyhow::anyhow!(
                    "Recovery manifest backup identity mismatch for service '{}' (backup row {})",
                    ext_backup.metadata.service_name,
                    ext_backup.backup_id
                ));
            }

            // A clean disaster-recovery host has the service rows from the
            // restored control plane but no containers yet. `start_service`
            // falls back to `initialize_service` when the container is
            // missing, recreating it from the restored encrypted config before
            // the engine-specific restore writes the backup data.
            manager
                .start_service(ext_backup.service_id)
                .await
                .map_err(|error| {
                    anyhow::anyhow!(
                        "Failed to provision target service '{}' (id {}) before restore: {}",
                        ext_backup.metadata.service_name,
                        ext_backup.service_id,
                        error
                    )
                })?;

            // Decrypt service config
            let decrypted_config = if let Some(ref config) = service.config {
                encryption_service
                    .decrypt_string(config)
                    .map_err(|e| anyhow::anyhow!("Failed to decrypt service config: {}", e))?
            } else {
                return Err(anyhow::anyhow!(
                    "Service {} has no config",
                    ext_backup.metadata.service_name
                ));
            };

            // Restore using the ExternalService trait method
            Self::restore_service_from_s3(ExternalServiceRestoreInput {
                manager: &manager,
                s3_client,
                s3_credentials,
                external_backup: ext_backup,
                backup_model: &backup_model,
                service_model: service,
                database: db.as_ref(),
                decrypted_config: &decrypted_config,
            })
            .await?;
        }

        println!();
        println!("{}", "✓ All external services restored".bright_green());
        Ok(())
    }

    async fn restore_service_from_s3(input: ExternalServiceRestoreInput<'_>) -> anyhow::Result<()> {
        use temps_providers::externalsvc::{RestoreContext, ServiceConfig, ServiceType};

        let ExternalServiceRestoreInput {
            manager,
            s3_client,
            s3_credentials,
            external_backup: ext_backup,
            backup_model,
            service_model,
            database: db,
            decrypted_config,
        } = input;

        println!(
            "  {} {}",
            "Restoring:".bright_white(),
            ext_backup.metadata.service_name.bright_cyan()
        );

        // Parse service type
        let svc_type = ServiceType::from_str(&ext_backup.metadata.service_type).map_err(|e| {
            anyhow::anyhow!(
                "Invalid service type {}: {}",
                ext_backup.metadata.service_type,
                e
            )
        })?;

        // Get service instance from manager
        let service =
            manager.get_service_instance(ext_backup.metadata.service_name.clone(), svc_type);

        // Parse the decrypted config into a JSON value
        let parameters: serde_json::Value = serde_json::from_str(decrypted_config)
            .map_err(|e| anyhow::anyhow!("Failed to parse service config: {}", e))?;

        // Create ServiceConfig with the parsed parameters
        let service_config = ServiceConfig {
            name: ext_backup.metadata.service_name.clone(),
            service_type: svc_type,
            version: None, // Version is managed by the service itself
            parameters,
        };

        // Create a temporary s3_source entity for the restore operation
        // This is used by the restore_from_s3 method to know where to fetch the backup
        let s3_source = temps_entities::s3_sources::Model {
            id: 0, // Temporary ID
            name: "CLI Restore Source".to_string(),
            bucket_name: s3_credentials.bucket_name.clone(),
            region: s3_credentials.region.clone(),
            endpoint: s3_credentials.endpoint.clone(),
            bucket_path: s3_credentials.bucket_path.clone(),
            access_key_id: s3_credentials.access_key_id.clone(),
            secret_key: s3_credentials.secret_key.clone(),
            force_path_style: Some(s3_credentials.force_path_style),
            is_default: false,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        if service_model.topology == "cluster" && service_model.service_type == "postgres" {
            let metadata = serde_json::from_str::<serde_json::Value>(&backup_model.metadata)
                .map_err(|error| anyhow::anyhow!("Invalid cluster backup metadata: {}", error))?;
            let target_user_data = metadata
                .get("walg_target_user_data")
                .map(serde_json::to_string)
                .transpose()
                .map_err(|error| anyhow::anyhow!("Invalid WAL-G snapshot selector: {}", error))?;
            manager
                .restore_postgres_cluster(
                    service_model,
                    &backup_model.s3_location,
                    s3_credentials,
                    target_user_data.as_deref(),
                )
                .await
                .map_err(|error| anyhow::anyhow!("Failed to restore cluster: {}", error))?;
        } else {
            let restore_context = RestoreContext {
                s3_client,
                s3_credentials,
                s3_source: &s3_source,
                backup: backup_model,
                backup_location: &backup_model.s3_location,
                source_service: service_model,
                source_config: service_config,
                pool: db,
            };
            service
                .restore_in_place(restore_context)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to restore service: {}", e))?;
        }

        println!("  {} Service restored successfully", "✓".bright_green());
        Ok(())
    }

    fn execute_restore_service(args: RestoreServiceArgs) -> anyhow::Result<()> {
        info!(
            "Restoring external service from backup: {}",
            args.service_name
        );

        // Create tokio runtime for async operations
        let rt = tokio::runtime::Runtime::new()?;

        // Create S3 client
        let s3_client = rt.block_on(Self::create_s3_client(
            &args.access_key_id,
            &args.secret_access_key,
            &args.region,
            args.endpoint.as_deref(),
            args.force_path_style,
        ))?;
        let s3_credentials = Self::restore_s3_credentials(
            &args.access_key_id,
            &args.secret_access_key,
            &args.region,
            args.endpoint.as_deref(),
            &args.bucket_name,
            args.force_path_style,
        );

        // Construct index.json key
        let index_key = if args.bucket_path.is_empty() {
            "index.json".to_string()
        } else {
            format!("{}/index.json", args.bucket_path.trim_matches('/'))
        };

        // Download and parse index.json
        println!("{}", "Reading backup index...".bright_white());
        let index_data = rt.block_on(async {
            let response = s3_client
                .get_object()
                .bucket(&args.bucket_name)
                .key(&index_key)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to download index.json from S3: {}", e))?;

            let data = response
                .body
                .collect()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to read index.json data: {}", e))?;

            Ok::<Vec<u8>, anyhow::Error>(data.into_bytes().to_vec())
        })?;

        let index: BackupIndex = serde_json::from_slice(&index_data)
            .map_err(|e| anyhow::anyhow!("Failed to parse index.json: {}", e))?;

        // Find the backup by backup_id
        let backup = index
            .backups
            .iter()
            .find(|b| b.backup_id == args.backup_id)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Backup with ID '{}' not found in index.json",
                    args.backup_id
                )
            })?;

        // Download and parse metadata.json
        println!("{}", "Reading backup metadata...".bright_white());
        let metadata_key = backup.metadata_location.trim_start_matches('/').to_string();
        let binding = metadata_key
            .replace("backup.sql.gz", "metadata.json")
            .replace("backup.postgresql.gz", "metadata.json");
        let metadata_key = binding;
        let metadata_key = metadata_key.as_str();

        let metadata_data = rt.block_on(async {
            let response = s3_client
                .get_object()
                .bucket(&args.bucket_name)
                .key(metadata_key)
                .send()
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to download metadata.json from S3 with key {}: {}",
                        metadata_key,
                        e
                    )
                })?;

            let data = response
                .body
                .collect()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to read metadata.json data: {}", e))?;

            Ok::<Vec<u8>, anyhow::Error>(data.into_bytes().to_vec())
        })?;

        let metadata_value: serde_json::Value = serde_json::from_slice(&metadata_data)
            .map_err(|e| anyhow::anyhow!("Failed to parse metadata.json: {}", e))?;
        Self::verify_manifest_authentication(
            &metadata_value,
            &backup.backup_id,
            &args.secret_access_key,
        )?;
        let metadata: BackupMetadata = serde_json::from_value(metadata_value)
            .map_err(|e| anyhow::anyhow!("Failed to decode recovery metadata: {}", e))?;

        // Find the specific external service backup by service name
        let ext_backup = metadata
            .external_service_backups
            .iter()
            .find(|b| b.metadata.service_name == args.service_name)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Service '{}' not found in backup. Available services: {}",
                    args.service_name,
                    metadata
                        .external_service_backups
                        .iter()
                        .map(|b| b.metadata.service_name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;

        println!(
            "{} {} {}",
            "Found service:".bright_white(),
            ext_backup.metadata.service_name.bright_cyan(),
            format!("({})", ext_backup.metadata.service_type).bright_white()
        );
        println!();

        // Connect to temps database to get service config
        use sea_orm::{Database, EntityTrait};
        use temps_core::EncryptionService;
        use temps_entities::{backups, external_services};

        println!("{}", "Connecting to temps database...".bright_white());
        let db = rt
            .block_on(Database::connect(&args.database_url))
            .map_err(|e| anyhow::anyhow!("Failed to connect to temps database: {}", e))?;

        // Query the external service
        use sea_orm::ColumnTrait;
        use sea_orm::QueryFilter;

        let service = rt
            .block_on(async {
                external_services::Entity::find()
                    .filter(external_services::Column::Id.eq(ext_backup.service_id))
                    .one(&db)
                    .await
            })
            .map_err(|e| anyhow::anyhow!("Failed to query external service: {}", e))?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Service ID {} not found in temps database",
                    ext_backup.service_id
                )
            })?;
        let backup_model = rt
            .block_on(backups::Entity::find_by_id(ext_backup.backup_id).one(&db))
            .map_err(|error| {
                anyhow::anyhow!(
                    "Failed to query backup row {}: {}",
                    ext_backup.backup_id,
                    error
                )
            })?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Backup row {} not found in Temps database",
                    ext_backup.backup_id
                )
            })?;
        if backup_model.state != "completed"
            || backup_model.s3_location != ext_backup.s3_location
            || service.name != ext_backup.metadata.service_name
            || service.service_type != ext_backup.metadata.service_type
        {
            return Err(anyhow::anyhow!(
                "Recovery manifest identity does not match backup row {} and service id {}",
                ext_backup.backup_id,
                ext_backup.service_id
            ));
        }

        // Create encryption service and decrypt config
        let encryption_service = Arc::new(
            EncryptionService::new(&args.encryption_key)
                .map_err(|e| anyhow::anyhow!("Failed to create encryption service: {}", e))?,
        );

        let decrypted_config = if let Some(config) = &service.config {
            encryption_service
                .decrypt_string(config)
                .map_err(|e| anyhow::anyhow!("Failed to decrypt service config: {}", e))?
        } else {
            return Err(anyhow::anyhow!(
                "Service {} has no config",
                ext_backup.metadata.service_name
            ));
        };

        // Create Docker client and ExternalServiceManager
        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults()
                .map_err(|e| anyhow::anyhow!("Failed to connect to Docker: {}", e))?,
        );

        let db_arc = Arc::new(db);
        let dns_registry = Arc::new(temps_providers::DnsRegistry::new(db_arc.clone()));
        let manager = temps_providers::ExternalServiceManager::new(
            db_arc.clone(),
            encryption_service,
            docker,
            dns_registry,
        );

        // Restore using the ExternalService trait method
        println!();
        println!(
            "{}",
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_cyan()
        );
        println!(
            "{}",
            format!("   Restoring {}", ext_backup.metadata.service_name)
                .bright_white()
                .bold()
        );
        println!(
            "{}",
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_cyan()
        );
        println!();

        rt.block_on(Self::restore_service_from_s3(ExternalServiceRestoreInput {
            manager: &manager,
            s3_client: &s3_client,
            s3_credentials: &s3_credentials,
            external_backup: ext_backup,
            backup_model: &backup_model,
            service_model: &service,
            database: db_arc.as_ref(),
            decrypted_config: &decrypted_config,
        }))?;

        println!();
        println!(
            "{}",
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_green()
        );
        println!(
            "{}",
            format!(
                "   ✅ Service '{}' restored successfully!",
                ext_backup.metadata.service_name
            )
            .bright_green()
            .bold()
        );
        println!(
            "{}",
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_green()
        );
        println!();

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata() -> BackupMetadata {
        BackupMetadata {
            recovery_set_version: Some(2),
            complete: Some(true),
            backup_id: "recovery-uuid".to_string(),
            name: "Recovery set".to_string(),
            backup_type: "full".to_string(),
            created_at: "2026-09-02T12:00:00Z".to_string(),
            size_bytes: 42,
            artifact_sha256: None,
            server_config: None,
            server_config_encrypted: None,
            server_config_encryption: None,
            manifest_authentication: None,
            external_service_backups: vec![ExternalServiceBackup {
                backup_id: 12,
                service_id: 7,
                s3_location: "external_services/postgres/db/backup.sql.gz".to_string(),
                state: "completed".to_string(),
                size_bytes: Some(42),
                backup_type: "full".to_string(),
                metadata: ExternalServiceMetadata {
                    service_type: "postgres".to_string(),
                    service_name: "postgres-test".to_string(),
                },
            }],
        }
    }

    #[test]
    fn v2_recovery_set_must_be_complete() {
        let mut value = metadata();
        value.complete = Some(false);

        let error = BackupCommand::validate_recovery_set(&value, "recovery-uuid")
            .expect_err("an incomplete v2 recovery set must be rejected");

        assert!(error.to_string().contains("is not marked complete"));
    }

    #[test]
    fn recovery_set_rejects_incomplete_external_service() {
        let mut value = metadata();
        value.external_service_backups[0].state = "failed".to_string();

        let error = BackupCommand::validate_recovery_set(&value, "recovery-uuid")
            .expect_err("a failed child must make the recovery set unusable");

        assert!(error.to_string().contains("postgres-test"));
    }

    #[test]
    fn recovery_set_rejects_metadata_identity_mismatch() {
        let value = metadata();

        let error = BackupCommand::validate_recovery_set(&value, "different-uuid")
            .expect_err("index and metadata identities must match");

        assert!(error.to_string().contains("identity mismatch"));
    }

    #[test]
    fn encrypted_server_config_uses_s3_secret() {
        let secret = "s3-secret-for-test";
        let config = "encryption_key: 01234567890123456789012345678901\n";
        let ciphertext = temps_core::EncryptionService::new_from_password(secret)
            .encrypt_string(config)
            .expect("test config should encrypt");
        let mut value = metadata();
        value.server_config_encrypted = Some(ciphertext);
        value.server_config_encryption = Some("aes-256-gcm+s3-secret-sha256".to_string());

        let restored = BackupCommand::resolve_server_config(&value, secret)
            .expect("matching S3 secret should decrypt the config");

        assert_eq!(restored, config);
        assert!(BackupCommand::resolve_server_config(&value, "wrong-secret").is_err());
    }

    #[test]
    fn authenticated_manifest_rejects_tampering() {
        let secret = "manifest-test-secret";
        let mut value = serde_json::to_value(metadata()).expect("metadata should serialize");
        value
            .as_object_mut()
            .expect("metadata should be an object")
            .remove("manifest_authentication");
        let payload = serde_json::to_string(&value).expect("metadata should serialize");
        let authentication = temps_core::EncryptionService::new_from_password(secret)
            .encrypt_string(&payload)
            .expect("manifest should authenticate");
        value["manifest_authentication"] = serde_json::json!(authentication);

        BackupCommand::verify_manifest_authentication(&value, "recovery-uuid", secret)
            .expect("untouched manifest should authenticate");
        value["name"] = serde_json::json!("tampered");
        assert!(
            BackupCommand::verify_manifest_authentication(&value, "recovery-uuid", secret).is_err()
        );
    }

    #[test]
    fn v2_manifest_requires_encrypted_server_config() {
        let mut value = metadata();
        value.server_config = Some("auth_secret: plaintext".to_string());

        let error = BackupCommand::resolve_server_config(&value, "unused")
            .expect_err("v2 must not downgrade to plaintext configuration");

        assert!(error
            .to_string()
            .contains("no encrypted server configuration"));
    }

    #[test]
    fn unsupported_manifest_version_is_rejected() {
        let mut value = metadata();
        value.recovery_set_version = Some(3);

        let error = BackupCommand::validate_recovery_set(&value, "recovery-uuid")
            .expect_err("unknown manifest versions must fail closed");

        assert!(error.to_string().contains("unsupported manifest version"));
    }

    #[test]
    fn control_plane_digest_detects_modified_bytes() {
        use sha2::{Digest, Sha256};

        let directory = tempfile::tempdir().expect("test directory should be created");
        let backup = directory.path().join("backup.sql.gz");
        std::fs::write(&backup, b"trusted-backup").expect("backup should be written");
        let mut value = metadata();
        value.artifact_sha256 = Some(hex::encode(Sha256::digest(b"trusted-backup")));

        BackupCommand::verify_control_plane_artifact(&value, &backup)
            .expect("matching artifact should verify");
        std::fs::write(&backup, b"modified-backup").expect("backup should be modified");
        assert!(BackupCommand::verify_control_plane_artifact(&value, &backup).is_err());
    }

    #[test]
    fn legacy_plaintext_server_config_remains_readable() {
        let mut value = metadata();
        value.recovery_set_version = None;
        value.complete = None;
        value.server_config = Some("encryption_key: legacy-key".to_string());

        assert_eq!(
            BackupCommand::resolve_server_config(&value, "unused")
                .expect("legacy config should remain supported"),
            "encryption_key: legacy-key"
        );
    }

    #[test]
    fn server_config_secrets_are_parsed_as_yaml() {
        let config =
            "encryption_key: '01234567890123456789012345678901'\nauth_secret: \"auth-value\"\n";

        assert_eq!(
            BackupCommand::extract_encryption_key(config)
                .expect("quoted encryption key should parse"),
            "01234567890123456789012345678901"
        );
        assert_eq!(
            BackupCommand::extract_auth_secret(config).expect("quoted auth secret should parse"),
            "auth-value"
        );
    }

    #[test]
    fn installing_recovery_secret_preserves_previous_value() {
        let directory = tempfile::tempdir().expect("test directory should be created");
        let target = directory.path().join("encryption_key");
        std::fs::write(&target, "old-value").expect("old value should be written");

        BackupCommand::install_recovery_secret(directory.path(), "encryption_key", "new-value")
            .expect("recovery secret should be installed");

        assert_eq!(
            std::fs::read_to_string(&target).expect("new secret should be readable"),
            "new-value"
        );
        let preserved = std::fs::read_dir(directory.path())
            .expect("test directory should be readable")
            .filter_map(Result::ok)
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("encryption_key.pre-restore-")
            })
            .expect("previous secret should be preserved");
        assert_eq!(
            std::fs::read_to_string(preserved.path()).expect("preserved secret should be readable"),
            "old-value"
        );
    }

    #[test]
    fn database_url_redaction_hides_password() {
        let redacted = BackupCommand::redact_database_url(
            "postgresql://temps:super-secret@127.0.0.1:5432/temps?sslpassword=query-secret#fragment-secret",
        );

        assert!(!redacted.contains("super-secret"));
        assert!(!redacted.contains("query-secret"));
        assert!(!redacted.contains("fragment-secret"));
        assert!(redacted.contains("REDACTED"));
    }

    #[test]
    fn restore_credentials_use_parsed_cli_values_without_environment_lookup() {
        let credentials = BackupCommand::restore_s3_credentials(
            "flag-access-key",
            "flag-secret-key",
            "eu-central-1",
            Some("https://objects.example.test"),
            "recovery-bucket",
            true,
        );

        assert_eq!(credentials.access_key_id, "flag-access-key");
        assert_eq!(credentials.secret_key, "flag-secret-key");
        assert_eq!(credentials.region, "eu-central-1");
        assert_eq!(
            credentials.endpoint.as_deref(),
            Some("https://objects.example.test")
        );
        assert_eq!(credentials.bucket_name, "recovery-bucket");
        assert!(credentials.bucket_path.is_empty());
        assert!(credentials.force_path_style);
    }

    #[test]
    fn recovery_secret_parent_rejects_a_file_as_data_directory() {
        let directory = tempfile::tempdir().expect("test directory should be created");
        let file = directory.path().join("not-a-directory");
        std::fs::write(&file, "data").expect("test file should be written");

        let error = BackupCommand::recovery_secret_parent(&file)
            .expect_err("a file cannot be used as the Temps data directory");

        assert!(error.to_string().contains("is not a directory"));
    }

    #[cfg(unix)]
    #[test]
    fn recovery_secret_install_rejects_a_symlinked_data_directory() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("test directory should be created");
        let actual = directory.path().join("actual");
        let linked = directory.path().join("linked");
        std::fs::create_dir(&actual).expect("actual directory should be created");
        symlink(&actual, &linked).expect("directory symlink should be created");

        let preflight_error = BackupCommand::recovery_secret_parent(&linked)
            .expect_err("preflight must reject a symlinked data directory");
        assert!(preflight_error.to_string().contains("symlink"));

        let install_error = BackupCommand::install_recovery_secrets(
            &linked,
            "01234567890123456789012345678901",
            "test-auth-secret",
        )
        .expect_err("secret installation must reject a symlinked data directory");
        assert!(install_error.to_string().contains("symlink"));
        assert_eq!(
            std::fs::read_dir(&actual)
                .expect("actual directory should be readable")
                .count(),
            0
        );
    }

    #[test]
    fn s3_location_parser_accepts_key_and_matching_uri() {
        assert_eq!(
            BackupCommand::s3_key_from_location("/root/backup.sql.gz", "bucket")
                .expect("plain object key should parse"),
            "root/backup.sql.gz"
        );
        assert_eq!(
            BackupCommand::s3_key_from_location(
                "s3://bucket/root/external/postgres/walg",
                "bucket",
            )
            .expect("matching S3 URI should parse"),
            "root/external/postgres/walg"
        );
    }

    #[test]
    fn s3_location_parser_rejects_cross_bucket_uri() {
        let error = BackupCommand::s3_key_from_location("s3://other/root/backup.sql.gz", "bucket")
            .expect_err("cross-bucket artifact references must fail closed");

        assert!(error.to_string().contains("does not match"));
    }
}
