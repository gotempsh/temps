use chrono::Utc;
use sea_orm::{ActiveModelTrait, DatabaseBackend, EntityTrait, Set};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use temps_database::DbConnection;
use temps_entities::settings;
use thiserror::Error;
use tokio::{
    fs as tokio_fs,
    io::{AsyncReadExt, AsyncWriteExt},
};
// Well-known paths relative to data_dir
pub const STATIC_DIR_NAME: &str = "static";
pub const PIPELINE_LOGS_DIR_NAME: &str = "logs";
pub const ENCRYPTION_KEY_FILE: &str = "encryption_key";
pub const AUTH_SECRET_FILE: &str = "auth_secret";
pub const SQLITE_DB_NAME: &str = "temps.db";

use rand::Rng;
use serde_derive::{Deserialize, Serialize};
use temps_core::AppSettings;

#[derive(Error, Debug)]
pub enum ConfigServiceError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Setting not found: {key}")]
    SettingNotFound { key: String },

    #[error("Invalid configuration: {details}")]
    InvalidConfiguration { details: String },

    #[error("Serialization error: {0}")]
    Serialization(String),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ServerConfig {
    // Required fields
    pub address: String,
    pub database_url: String,

    // Optional fields
    pub tls_address: Option<String>,
    pub console_address: String,

    // Generated/derived fields
    pub data_dir: PathBuf,
    pub auth_secret: String,
    pub encryption_key: String,

    // Fixed value
    pub api_base_url: String,

    // PostgreSQL connection pool settings (all optional with defaults)
    pub postgres_max_connections: Option<u32>,
    pub postgres_min_connections: Option<u32>,
    pub postgres_connect_timeout_secs: Option<u64>,
    pub postgres_acquire_timeout_secs: Option<u64>,
    pub postgres_idle_timeout_secs: Option<u64>,
    pub postgres_max_lifetime_secs: Option<u64>,
}

impl ServerConfig {
    /// Create a new configuration with minimal parameters
    pub fn new(
        address: String,
        database_url: String,
        tls_address: Option<String>,
        console_address: Option<String>,
    ) -> anyhow::Result<Self> {
        // Determine data directory from env or use default
        let data_dir = std::env::var("TEMPS_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .expect("Could not find home directory")
                    .join(".temps")
            });

        // Create data directory if it doesn't exist
        fs::create_dir_all(&data_dir)?;

        // Generate or load auth_secret (32 bytes in hex format)
        let auth_secret_path = data_dir.join("auth_secret");
        let auth_secret = if auth_secret_path.exists() {
            fs::read_to_string(&auth_secret_path)?.trim().to_string()
        } else {
            let secret = Self::generate_auth_secret();
            fs::write(&auth_secret_path, &secret)?;
            secret
        };

        // Generate or load encryption_key (32 bytes in hex format)
        let encryption_key_path = data_dir.join("encryption_key");
        let encryption_key = if encryption_key_path.exists() {
            fs::read_to_string(&encryption_key_path)?.trim().to_string()
        } else {
            let key = Self::generate_encryption_key();
            fs::write(&encryption_key_path, &key)?;
            key
        };

        // Get console address - use a random available port
        let console_address = console_address.unwrap_or_else(Self::get_random_console_address);

        Ok(ServerConfig {
            address,
            database_url,
            tls_address,
            console_address,
            data_dir,
            auth_secret,
            encryption_key,
            api_base_url: "/api".to_string(),

            // PostgreSQL settings from env or defaults
            postgres_max_connections: std::env::var("TEMPS_POSTGRES_MAX_CONNECTIONS")
                .ok()
                .and_then(|s| s.parse().ok())
                .or(Some(100)),
            postgres_min_connections: std::env::var("TEMPS_POSTGRES_MIN_CONNECTIONS")
                .ok()
                .and_then(|s| s.parse().ok())
                .or(Some(10)),
            postgres_connect_timeout_secs: std::env::var("TEMPS_POSTGRES_CONNECT_TIMEOUT")
                .ok()
                .and_then(|s| s.parse().ok())
                .or(Some(30)),
            postgres_acquire_timeout_secs: std::env::var("TEMPS_POSTGRES_ACQUIRE_TIMEOUT")
                .ok()
                .and_then(|s| s.parse().ok())
                .or(Some(30)),
            postgres_idle_timeout_secs: std::env::var("TEMPS_POSTGRES_IDLE_TIMEOUT")
                .ok()
                .and_then(|s| s.parse().ok())
                .or(Some(600)),
            postgres_max_lifetime_secs: std::env::var("TEMPS_POSTGRES_MAX_LIFETIME")
                .ok()
                .and_then(|s| s.parse().ok())
                .or(Some(1800)),
        })
    }

    /// Generate a 32-byte auth secret (64 hex characters)
    fn generate_auth_secret() -> String {
        // Generate 32 random bytes and encode as 64 hex characters
        let mut rng = rand::thread_rng();
        let bytes: Vec<u8> = (0..32).map(|_| rng.gen::<u8>()).collect();
        hex::encode(bytes)
    }

    /// Generate a 32-byte encryption key (64 hex characters)
    fn generate_encryption_key() -> String {
        // Generate 32 random bytes and encode as 64 hex characters
        let mut rng = rand::thread_rng();
        let bytes: Vec<u8> = (0..32).map(|_| rng.gen::<u8>()).collect();
        hex::encode(bytes)
    }

    /// Get a random available port for console address
    fn get_random_console_address() -> String {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("Failed to bind to random port");
        let port = listener.local_addr().unwrap().port();
        format!("127.0.0.1:{}", port)
    }

    // Helper methods
    pub fn get_data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }

    // PostgreSQL connection pool getters with defaults
    pub fn get_postgres_max_connections(&self) -> u32 {
        self.postgres_max_connections.unwrap_or(100)
    }

    pub fn get_postgres_min_connections(&self) -> u32 {
        self.postgres_min_connections.unwrap_or(10)
    }

    pub fn get_postgres_connect_timeout_secs(&self) -> u64 {
        self.postgres_connect_timeout_secs.unwrap_or(30)
    }

    pub fn get_postgres_acquire_timeout_secs(&self) -> u64 {
        self.postgres_acquire_timeout_secs.unwrap_or(30)
    }

    pub fn get_postgres_idle_timeout_secs(&self) -> u64 {
        self.postgres_idle_timeout_secs.unwrap_or(600)
    }

    pub fn get_postgres_max_lifetime_secs(&self) -> u64 {
        self.postgres_max_lifetime_secs.unwrap_or(1800)
    }
}

// Default domain for local development (resolves to 127.0.0.1)
pub const DEFAULT_LOCAL_DOMAIN: &str = "localho.st";

/// Service that provides centralized access to configuration paths and settings
/// Handles path resolution, persistent settings, and ensures consistency across the application
pub struct ConfigService {
    config: Arc<ServerConfig>,
    db: Arc<DbConnection>,
}

impl ConfigService {
    pub fn new(config: Arc<ServerConfig>, db: Arc<DbConnection>) -> Self {
        Self { config, db }
    }

    /// Get the base data directory path
    pub fn data_dir(&self) -> PathBuf {
        PathBuf::from(self.config.get_data_dir())
    }

    /// Get the static files directory path (always under data_dir/static)
    pub fn static_dir(&self) -> PathBuf {
        self.data_dir().join(STATIC_DIR_NAME)
    }

    /// Get the pipeline logs directory path (always under data_dir/logs)
    pub fn pipeline_logs_path(&self) -> PathBuf {
        self.data_dir().join(PIPELINE_LOGS_DIR_NAME)
    }

    /// Get the log data directory path (always under data_dir/logs)
    pub fn log_data_dir(&self) -> PathBuf {
        self.data_dir().join("logs")
    }

    /// Get the SQLite database file path (if using SQLite)
    pub fn sqlite_db_path(&self) -> Option<PathBuf> {
        if self.config.database_url.starts_with("sqlite:") {
            Some(self.data_dir().join(SQLITE_DB_NAME))
        } else {
            None
        }
    }
    pub fn get_database_url(&self) -> String {
        self.config.database_url.clone()
    }
    pub fn get_server_config(&self) -> Arc<ServerConfig> {
        self.config.clone()
    }
    /// Get the database backend type from the configured database URL
    pub fn get_database_backend(&self) -> DatabaseBackend {
        let database_url = &self.config.database_url;

        if database_url.starts_with("sqlite://") || database_url.starts_with("sqlite:") {
            DatabaseBackend::Sqlite
        } else if database_url.starts_with("postgres://")
            || database_url.starts_with("postgresql://")
        {
            DatabaseBackend::Postgres
        } else if database_url.starts_with("mysql://") || database_url.starts_with("mariadb://") {
            DatabaseBackend::MySql
        } else {
            // Default to SQLite for unknown URLs
            tracing::warn!(
                "Unknown database URL scheme, defaulting to SQLite: {}",
                database_url
            );
            DatabaseBackend::Sqlite
        }
    }

    /// Check if using SQLite database
    pub fn is_sqlite(&self) -> bool {
        matches!(self.get_database_backend(), DatabaseBackend::Sqlite)
    }

    /// Check if using PostgreSQL database
    pub fn is_postgres(&self) -> bool {
        matches!(self.get_database_backend(), DatabaseBackend::Postgres)
    }

    /// Check if using MySQL/MariaDB database
    pub fn is_mysql(&self) -> bool {
        matches!(self.get_database_backend(), DatabaseBackend::MySql)
    }

    /// Ensure all required directories exist
    pub async fn ensure_directories(&self) -> Result<(), ConfigServiceError> {
        // Create data directory
        tokio::fs::create_dir_all(self.data_dir()).await?;

        // Create static directory
        tokio::fs::create_dir_all(self.static_dir()).await?;

        // Create pipeline logs directory
        tokio::fs::create_dir_all(self.pipeline_logs_path()).await?;

        Ok(())
    }

    /// Get a specific subdirectory under data_dir
    pub fn get_data_subdir(&self, subdir: &str) -> PathBuf {
        self.data_dir().join(subdir)
    }

    /// Check if a path exists
    pub async fn path_exists(&self, path: &PathBuf) -> bool {
        tokio::fs::metadata(path).await.is_ok()
    }

    /// Get or create the encryption key
    /// Loads from data_dir/encryption_key if exists, otherwise generates and saves a new one
    pub async fn get_or_create_encryption_key(&self) -> Result<String, ConfigServiceError> {
        let key_path = self.data_dir().join(ENCRYPTION_KEY_FILE);

        if self.path_exists(&key_path).await {
            // Read existing key
            let mut file = tokio_fs::File::open(&key_path).await?;
            let mut key = String::new();
            file.read_to_string(&mut key).await?;
            Ok(key.trim().to_string())
        } else {
            // Generate new key and save it
            let key = uuid::Uuid::new_v4().to_string().replace("-", "");

            // Ensure data directory exists
            tokio_fs::create_dir_all(self.data_dir()).await?;

            // Write key to file
            let mut file = tokio_fs::File::create(&key_path).await?;
            file.write_all(key.as_bytes()).await?;
            file.sync_all().await?;

            Ok(key)
        }
    }

    /// Get or create the auth secret
    /// Loads from data_dir/auth_secret if exists, otherwise generates and saves a new one
    pub async fn get_or_create_auth_secret(&self) -> Result<String, ConfigServiceError> {
        let secret_path = self.data_dir().join(AUTH_SECRET_FILE);

        if self.path_exists(&secret_path).await {
            // Read existing secret
            let mut file = tokio_fs::File::open(&secret_path).await?;
            let mut secret = String::new();
            file.read_to_string(&mut secret).await?;
            Ok(secret.trim().to_string())
        } else {
            // Generate new secret and save it (32 bytes as 64 hex characters)
            let mut rng = rand::thread_rng();
            let bytes: Vec<u8> = (0..32).map(|_| rng.gen::<u8>()).collect();
            let secret = hex::encode(bytes);

            // Ensure data directory exists
            tokio_fs::create_dir_all(self.data_dir()).await?;

            // Write secret to file
            let mut file = tokio_fs::File::create(&secret_path).await?;
            file.write_all(secret.as_bytes()).await?;
            file.sync_all().await?;

            Ok(secret)
        }
    }
    pub async fn get_external_url(&self) -> Result<Option<String>, ConfigServiceError> {
        let settings = self.get_settings().await?;
        Ok(settings.external_url)
    }

    /// Get the external URL with a default fallback to http://localho.st
    /// This ensures there's always a valid URL even when not configured
    pub async fn get_external_url_or_default(&self) -> Result<String, ConfigServiceError> {
        let settings = self.get_settings().await?;
        Ok(settings
            .external_url
            .unwrap_or_else(|| "http://localho.st".to_string()))
    }

    /// Get the application settings
    pub async fn get_settings(&self) -> Result<AppSettings, ConfigServiceError> {
        let record = settings::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await?;

        Ok(record
            .map(|r| AppSettings::from_json(r.data))
            .unwrap_or_default())
    }

    /// Update the application settings
    pub async fn update_settings(&self, settings: AppSettings) -> Result<(), ConfigServiceError> {
        let now = Utc::now();

        // Check if record exists
        let existing = settings::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await?;

        if let Some(existing_model) = existing {
            // Update existing settings
            let mut active_model: settings::ActiveModel = existing_model.into();
            active_model.data = Set(settings.to_json());
            active_model.updated_at = Set(now);
            active_model.update(self.db.as_ref()).await?;
        } else {
            // Create new settings
            let new_settings = settings::ActiveModel {
                id: Set(1),
                data: Set(settings.to_json()),
                created_at: Set(now),
                updated_at: Set(now),
            };
            new_settings.insert(self.db.as_ref()).await?;
        }

        Ok(())
    }

    /// Update a specific field in the settings
    pub async fn update_setting_field<F>(&self, update_fn: F) -> Result<(), ConfigServiceError>
    where
        F: FnOnce(&mut AppSettings),
    {
        let mut settings = self.get_settings().await?;
        update_fn(&mut settings);
        self.update_settings(settings).await
    }

    /// Initialize default settings if they don't exist
    pub async fn initialize_defaults(&self) -> Result<(), ConfigServiceError> {
        // Check if settings exist
        let existing = settings::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await?;

        if existing.is_none() {
            // Create default settings
            let default_settings = AppSettings::default();
            self.update_settings(default_settings).await?;
        }

        Ok(())
    }

    /// Get a specific setting value (convenience methods)
    pub async fn get_setting(&self, key: &str) -> Result<Option<String>, ConfigServiceError> {
        let settings = self.get_settings().await?;
        Ok(match key {
            "external_url" => settings.external_url,
            "preview_domain" => Some(settings.preview_domain),
            "letsencrypt_email" => settings.letsencrypt.email,
            "letsencrypt_environment" => Some(settings.letsencrypt.environment),
            "letsencrypt_dns_provider" => Some(settings.dns_provider.provider),
            "cloudflare_api_key" => settings.dns_provider.cloudflare_api_key,
            "screenshot_url" => Some(settings.screenshots.url),
            _ => None,
        })
    }

    /// Get or default setting - returns the setting value or a default if not found
    pub async fn get_setting_or_default(&self, key: &str, default: &str) -> String {
        self.get_setting(key)
            .await
            .unwrap_or(None)
            .unwrap_or_else(|| default.to_string())
    }

    /// Check if screenshots are enabled
    pub async fn is_screenshots_enabled(&self) -> bool {
        self.get_settings()
            .await
            .map(|s| s.screenshots.enabled)
            .unwrap_or(false)
    }

    /// Get screenshot URL
    pub async fn get_screenshot_url(&self) -> String {
        self.get_settings()
            .await
            .map(|s| s.screenshots.url)
            .unwrap_or_else(|_| "".to_string())
    }

    /// Check if preview domain is configured and is a wildcard
    pub async fn has_wildcard_domain(&self) -> bool {
        self.get_settings()
            .await
            .map(|s| s.preview_domain.starts_with("*."))
            .unwrap_or(false)
    }

    /// Auto-detect and set external_url from the first incoming request
    pub async fn auto_set_external_url(&self, request_url: &str) -> Result<(), ConfigServiceError> {
        let settings = self.get_settings().await?;

        // Only set if not already set
        if settings.external_url.is_none() {
            // Extract the base URL from the request
            if let Ok(parsed) = url::Url::parse(request_url) {
                let external_url = format!(
                    "{}://{}",
                    parsed.scheme(),
                    parsed.host_str().unwrap_or("localhost")
                );
                self.update_setting_field(|s| {
                    s.external_url = Some(external_url);
                })
                .await?;
            }
        }
        Ok(())
    }

    /// Get the full deployment URL for a given deployment slug
    /// Always returns [protocol]://{slug}.{preview_domain}
    /// Get the deployment URL by deployment ID
    pub async fn get_deployment_url(
        &self,
        deployment_id: i32,
    ) -> Result<String, ConfigServiceError> {
        use sea_orm::EntityTrait;
        use temps_entities::prelude::Deployments;

        // Get the deployment to find its slug
        let deployment = Deployments::find_by_id(deployment_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ConfigServiceError::SettingNotFound {
                key: format!("deployment_{}", deployment_id),
            })?;

        self.get_deployment_url_by_slug(&deployment.slug).await
    }

    /// Get the deployment URL by deployment slug
    pub async fn get_deployment_url_by_slug(
        &self,
        deployment_slug: &str,
    ) -> Result<String, ConfigServiceError> {
        let settings = self.get_settings().await?;

        // Determine protocol and port from external_url if set, otherwise default to https
        let (protocol, port) = if let Some(ref external_url) = settings.external_url {
            if let Ok(parsed) = url::Url::parse(external_url) {
                (parsed.scheme().to_string(), parsed.port())
            } else if external_url.starts_with("https://") {
                ("https".to_string(), None)
            } else if external_url.starts_with("http://") {
                ("http".to_string(), None)
            } else {
                ("https".to_string(), None)
            }
        } else {
            ("https".to_string(), None)
        };

        // Use preview_domain if set, otherwise fallback to DEFAULT_LOCAL_DOMAIN
        let preview_domain = if !settings.preview_domain.is_empty() {
            settings.preview_domain.trim_start_matches("*.").to_string()
        } else {
            DEFAULT_LOCAL_DOMAIN.to_string()
        };

        // Construct the URL as [protocol]://{slug}.{preview_domain}[:port]
        // Only include port if it's non-standard (not 443 for https, not 80 for http)
        let url = if let Some(port) = port {
            let is_standard_port =
                (protocol == "https" && port == 443) || (protocol == "http" && port == 80);
            if is_standard_port {
                format!("{}://{}.{}", protocol, deployment_slug, preview_domain)
            } else {
                format!(
                    "{}://{}.{}:{}",
                    protocol, deployment_slug, preview_domain, port
                )
            }
        } else {
            format!("{}://{}.{}", protocol, deployment_slug, preview_domain)
        };

        Ok(url)
    }
}
