use clap::{Args, Subcommand};
use colored::Colorize;
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::PathBuf;
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
    backup_id: String,
    name: String,
    #[serde(rename = "type")]
    backup_type: String,
    created_at: String,
    size_bytes: i64,
    server_config: String,
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
    #[arg(long, env = "S3_SECRET_ACCESS_KEY")]
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
    /// Database connection URL to restore to
    #[arg(long, env = "TEMPS_DATABASE_URL")]
    database_url: String,

    /// S3 access key ID
    #[arg(long, env = "S3_ACCESS_KEY_ID")]
    access_key_id: String,

    /// S3 secret access key
    #[arg(long, env = "S3_SECRET_ACCESS_KEY")]
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
    #[arg(long, env = "S3_SECRET_ACCESS_KEY")]
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
    #[arg(long, env = "TEMPS_ENCRYPTION_KEY")]
    encryption_key: String,

    /// Database URL for the temps database (needed to query service config)
    #[arg(long, env = "TEMPS_DATABASE_URL")]
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
        let binding = metadata_key.replace("backup.postgresql.gz", "metadata.json");
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

        let metadata: BackupMetadata = serde_json::from_slice(&metadata_data).map_err(|e| {
            anyhow::anyhow!(
                "Failed to parse metadata.json Path: {} Error: {}",
                metadata_key,
                e
            )
        })?;

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
            args.database_url.bright_white()
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
        let temp_dir = std::env::temp_dir();
        let backup_file_path = temp_dir.join(format!(
            "temps_restore_{}.backup",
            chrono::Utc::now().timestamp()
        ));

        rt.block_on(async {
            let response = s3_client
                .get_object()
                .bucket(&args.bucket_name)
                .key(backup_key)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to download backup from S3: {}", e))?;

            let data = response
                .body
                .collect()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to read backup data: {}", e))?;

            std::fs::write(&backup_file_path, data.into_bytes())
                .map_err(|e| anyhow::anyhow!("Failed to write backup file: {}", e))?;

            Ok::<(), anyhow::Error>(())
        })?;

        println!("{}", "✓ Backup downloaded successfully".bright_green());
        println!();

        // Check if database URL is PostgreSQL
        if !args.database_url.starts_with("postgres://")
            && !args.database_url.starts_with("postgresql://")
        {
            return Err(anyhow::anyhow!(
                "Only PostgreSQL databases are supported. Database URL must start with postgres:// or postgresql://"
            ));
        }

        // Check if backup is gzipped and decompress if needed
        let final_backup_path = if backup_key.ends_with(".gz") {
            println!("{}", "Decompressing backup file...".bright_white());
            let decompressed_path = temp_dir.join(format!(
                "temps_restore_decompressed_{}.backup",
                chrono::Utc::now().timestamp()
            ));

            let gz_file = std::fs::File::open(&backup_file_path)
                .map_err(|e| anyhow::anyhow!("Failed to open gzipped backup: {}", e))?;
            let mut decoder = GzDecoder::new(gz_file);
            let mut decompressed_data = Vec::new();
            decoder
                .read_to_end(&mut decompressed_data)
                .map_err(|e| anyhow::anyhow!("Failed to decompress backup: {}", e))?;

            std::fs::write(&decompressed_path, &decompressed_data)
                .map_err(|e| anyhow::anyhow!("Failed to write decompressed backup: {}", e))?;

            // Clean up the original gzipped file
            let _ = std::fs::remove_file(&backup_file_path);

            println!(
                "{}",
                format!(
                    "✓ Backup decompressed successfully ({:.2} MB)",
                    decompressed_data.len() as f64 / (1024.0 * 1024.0)
                )
                .bright_green()
            );
            println!(
                "{}",
                format!("Decompressed file: {}", decompressed_path.display()).bright_white()
            );

            // Verify the decompressed file exists
            if !decompressed_path.exists() {
                return Err(anyhow::anyhow!(
                    "Decompressed file was not created at: {}",
                    decompressed_path.display()
                ));
            }

            decompressed_path
        } else {
            backup_file_path
        };

        println!("{}", "Restoring PostgreSQL database...".bright_white());
        Self::restore_postgres(&args.database_url, &final_backup_path)?;

        // Clean up temporary file
        let _ = std::fs::remove_file(&final_backup_path);

        // Extract encryption key from server_config
        let encryption_key = Self::extract_encryption_key(&metadata.server_config)?;

        // Restore external services
        rt.block_on(Self::restore_external_services(
            &rt,
            &args.database_url,
            &s3_client,
            &args.bucket_name,
            &metadata,
            &encryption_key,
        ))?;

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

    fn restore_postgres(database_url: &str, backup_file: &PathBuf) -> anyhow::Result<()> {
        use url::Url;

        let url = Url::parse(database_url)
            .map_err(|e| anyhow::anyhow!("Failed to parse database URL: {}", e))?;

        let host = url.host_str().unwrap_or("localhost");
        let port = url.port().unwrap_or(5432);
        let database = url.path().trim_start_matches('/');
        let username = url.username();
        let password = url.password();

        // Build pg_restore command
        let mut cmd = std::process::Command::new("pg_restore");
        cmd.arg("--verbose")
            .arg("--clean") // Drop existing objects before recreating
            .arg("--if-exists") // Don't error if objects don't exist
            .arg("--no-owner") // Don't set ownership
            .arg("--no-acl") // Don't restore access privileges
            .arg("--dbname")
            .arg(database)
            .arg("--host")
            .arg(host)
            .arg("--port")
            .arg(port.to_string())
            .arg("--username")
            .arg(username)
            .arg(backup_file);

        if let Some(pwd) = password {
            cmd.env("PGPASSWORD", pwd);
        }

        println!("{}", "Running pg_restore...".bright_white());
        let output = cmd.output().map_err(|e| {
            anyhow::anyhow!(
                "Failed to execute pg_restore: {}. Make sure pg_restore is installed and in PATH",
                e
            )
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("pg_restore failed: {}", stderr));
        }

        println!(
            "{}",
            "✓ PostgreSQL database restored successfully".bright_green()
        );
        Ok(())
    }

    fn extract_encryption_key(server_config: &str) -> anyhow::Result<String> {
        // Parse YAML-like server_config to extract encryption_key
        for line in server_config.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("encryption_key:") {
                let key = trimmed
                    .trim_start_matches("encryption_key:")
                    .trim()
                    .to_string();
                if key.is_empty() {
                    return Err(anyhow::anyhow!("Encryption key is empty in server_config"));
                }
                return Ok(key);
            }
        }
        Err(anyhow::anyhow!("Encryption key not found in server_config"))
    }

    async fn restore_external_services(
        rt: &tokio::runtime::Runtime,
        database_url: &str,
        s3_client: &aws_sdk_s3::Client,
        bucket_name: &str,
        metadata: &BackupMetadata,
        encryption_key: &str,
    ) -> anyhow::Result<()> {
        use sea_orm::{Database, EntityTrait};
        use temps_core::EncryptionService;
        use temps_entities::external_services;

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
            rt.block_on(Database::connect(database_url))
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
        );

        // Query all external services from the restored database
        let all_services = rt
            .block_on(external_services::Entity::find().all(db.as_ref()))
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
            Self::restore_service_from_s3(
                &manager,
                s3_client,
                bucket_name,
                ext_backup,
                &decrypted_config,
            )
            .await?;
        }

        println!();
        println!("{}", "✓ All external services restored".bright_green());
        Ok(())
    }

    async fn restore_service_from_s3(
        manager: &temps_providers::ExternalServiceManager,
        s3_client: &aws_sdk_s3::Client,
        bucket_name: &str,
        ext_backup: &ExternalServiceBackup,
        decrypted_config: &str,
    ) -> anyhow::Result<()> {
        use temps_providers::externalsvc::{ServiceConfig, ServiceType};

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
            bucket_name: bucket_name.to_string(),
            region: std::env::var("S3_REGION").unwrap_or_else(|_| "us-east-1".to_string()),
            endpoint: std::env::var("S3_ENDPOINT").ok(),
            bucket_path: "".to_string(),
            access_key_id: std::env::var("S3_ACCESS_KEY_ID")
                .map_err(|_| anyhow::anyhow!("S3_ACCESS_KEY_ID not set"))?,
            secret_key: std::env::var("S3_SECRET_ACCESS_KEY")
                .map_err(|_| anyhow::anyhow!("S3_SECRET_ACCESS_KEY not set"))?,
            force_path_style: std::env::var("S3_FORCE_PATH_STYLE")
                .ok()
                .and_then(|v| v.parse().ok()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        // Call the trait's restore_from_s3 method
        let backup_location = ext_backup.s3_location.trim_start_matches('/');
        service
            .restore_from_s3(s3_client, backup_location, &s3_source, service_config)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to restore service: {}", e))?;

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
        let binding = metadata_key.replace("backup.postgresql.gz", "metadata.json");
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

        let metadata: BackupMetadata = serde_json::from_slice(&metadata_data)
            .map_err(|e| anyhow::anyhow!("Failed to parse metadata.json: {}", e))?;

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
        use temps_entities::external_services;

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

        let manager =
            temps_providers::ExternalServiceManager::new(Arc::new(db), encryption_service, docker);

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

        rt.block_on(Self::restore_service_from_s3(
            &manager,
            &s3_client,
            &args.bucket_name,
            ext_backup,
            &decrypted_config,
        ))?;

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
