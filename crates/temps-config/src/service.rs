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
use tracing::{debug, info, warn};
// Well-known paths relative to data_dir
pub const STATIC_DIR_NAME: &str = "static";
pub const PIPELINE_LOGS_DIR_NAME: &str = "logs";
pub const ENCRYPTION_KEY_FILE: &str = "encryption_key";
pub const AUTH_SECRET_FILE: &str = "auth_secret";
pub const SQLITE_DB_NAME: &str = "temps.db";

use rand::Rng;
use serde_derive::{Deserialize, Serialize};
use temps_core::{AppSettings, PublicHostnameStrategy};

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

    // Admin listener (optional). When set, admin/management routes bind here
    // while the `console_address` listener only serves public ingest routes
    // (analytics events, error tracking ingest, AI gateway, worker route sync,
    // etc.). When unset, both surfaces share `console_address` for backwards
    // compatibility. See [admin-listener-split] for the route classification.
    pub console_admin_address: Option<String>,
    /// Comma-separated list of IPs / CIDRs allowed to reach the admin listener.
    /// Empty / unset = no IP allowlist (admin gated only by binding address).
    pub admin_allowed_ips: Vec<String>,
    /// Comma-separated list of HTTP Host headers allowed on the admin listener.
    /// Empty / unset = no Host check.
    pub admin_allowed_hosts: Vec<String>,
    /// When true, honor `X-Forwarded-For` from loopback peers only (for
    /// reverse-proxy deployments). Defaults to false.
    pub admin_trust_forwarded_for: bool,

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

    // ClickHouse analytics backend (optional, opt-in via env vars).
    // When `clickhouse_url` is unset, Temps runs in PG-only mode and the
    // CH fan-out worker is not started. See ADR-012.
    pub clickhouse_url: Option<String>,
    pub clickhouse_database: Option<String>,
    pub clickhouse_user: Option<String>,
    pub clickhouse_password: Option<String>,
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
            Self::restrict_file_permissions(&auth_secret_path);
            secret
        };

        // Generate or load encryption_key (32 bytes in hex format)
        let encryption_key_path = data_dir.join("encryption_key");
        let encryption_key = if encryption_key_path.exists() {
            fs::read_to_string(&encryption_key_path)?.trim().to_string()
        } else {
            let key = Self::generate_encryption_key();
            fs::write(&encryption_key_path, &key)?;
            Self::restrict_file_permissions(&encryption_key_path);
            key
        };

        // Get console address - use a random available port
        let console_address = console_address.unwrap_or_else(Self::get_random_console_address);

        // Admin listener (opt-in). When unset, the existing single-listener
        // mode is used and every route binds to `console_address`.
        let console_admin_address = std::env::var("TEMPS_CONSOLE_ADMIN_ADDRESS")
            .ok()
            .filter(|s| !s.is_empty());

        let admin_allowed_ips = std::env::var("TEMPS_ADMIN_ALLOWED_IPS")
            .ok()
            .map(|s| {
                s.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();

        let admin_allowed_hosts = std::env::var("TEMPS_ADMIN_ALLOWED_HOSTS")
            .ok()
            .map(|s| {
                s.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();

        let admin_trust_forwarded_for = std::env::var("TEMPS_ADMIN_TRUST_FORWARDED_FOR")
            .ok()
            .map(|s| matches!(s.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);

        Ok(ServerConfig {
            address,
            database_url,
            tls_address,
            console_address,
            console_admin_address,
            admin_allowed_ips,
            admin_allowed_hosts,
            admin_trust_forwarded_for,
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

            // ClickHouse analytics backend. All four keys must be present
            // for CH to be considered enabled — partial config is treated
            // as off so a half-configured operator never silently loses
            // analytics.
            clickhouse_url: std::env::var("TEMPS_CLICKHOUSE_URL")
                .ok()
                .filter(|s| !s.is_empty()),
            // The database name defaults to "temps" so ALL ClickHouse-backed
            // telemetry (analytics events/sessions, OTel traces, resource
            // metrics, proxy/request logs) lives in one consistent database.
            // Operators only need to set URL/USER/PASSWORD; the name is
            // overridable via TEMPS_CLICKHOUSE_DATABASE if they prefer another.
            // Only default it when ClickHouse is actually being configured
            // (URL present) — otherwise leave None so `is_clickhouse_enabled()`
            // stays false for an unconfigured server.
            clickhouse_database: std::env::var("TEMPS_CLICKHOUSE_DATABASE")
                .ok()
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    std::env::var("TEMPS_CLICKHOUSE_URL")
                        .ok()
                        .filter(|s| !s.is_empty())
                        .map(|_| "temps".to_string())
                }),
            clickhouse_user: std::env::var("TEMPS_CLICKHOUSE_USER")
                .ok()
                .filter(|s| !s.is_empty()),
            clickhouse_password: std::env::var("TEMPS_CLICKHOUSE_PASSWORD")
                .ok()
                .filter(|s| !s.is_empty()),
        })
    }

    /// Returns true when all four ClickHouse env vars are populated and
    /// the analytics fan-out path can be enabled. Partial config returns
    /// false (fail closed).
    pub fn is_clickhouse_enabled(&self) -> bool {
        self.clickhouse_url.is_some()
            && self.clickhouse_database.is_some()
            && self.clickhouse_user.is_some()
            && self.clickhouse_password.is_some()
    }

    /// Generate a 32-byte auth secret (64 hex characters)
    fn generate_auth_secret() -> String {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill(&mut bytes);
        hex::encode(bytes)
    }

    /// Generate a 32-byte encryption key (64 hex characters)
    fn generate_encryption_key() -> String {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill(&mut bytes);
        hex::encode(bytes)
    }

    /// Set file permissions to owner-only (0o600) for sensitive files.
    #[cfg(unix)]
    fn restrict_file_permissions(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }

    #[cfg(not(unix))]
    fn restrict_file_permissions(_path: &std::path::Path) {
        // File permissions are handled differently on non-Unix platforms
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
/// How long a cached `AppSettings` snapshot is served before `get_settings`
/// re-reads from the database. Short enough that an out-of-process writer (e.g.
/// the console process in the ADR-017 split topology, which updates settings
/// while the proxy reads them) is picked up promptly; long enough that the
/// proxy's per-request hot path (`request_filter`) never hammers Postgres.
const SETTINGS_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(5);

pub struct ConfigService {
    config: Arc<ServerConfig>,
    db: Arc<DbConnection>,
    /// In-memory cache of the singleton settings row so `get_settings` does not
    /// do a DB round-trip on every call. The proxy reads settings per request
    /// (security headers, preview gateway, on-demand TLS), so an uncached read
    /// would amplify any request flood into a Postgres QPS flood. Invalidated
    /// write-through by `update_settings`; otherwise refreshed after
    /// `SETTINGS_CACHE_TTL`.
    settings_cache: tokio::sync::RwLock<Option<(AppSettings, std::time::Instant)>>,
    /// Background task that LISTENs on the Postgres `settings_change` channel and
    /// invalidates `settings_cache` the instant another process writes settings.
    /// The 5s `SETTINGS_CACHE_TTL` remains as a safety net for any missed NOTIFY.
    /// Stored so it can be aborted on `Drop`. Only the plugin singleton spawns
    /// this (via [`ConfigService::start_settings_listener`]); throwaway
    /// `ConfigService::new()` instances never start it.
    listener_handle: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl ConfigService {
    pub fn new(config: Arc<ServerConfig>, db: Arc<DbConnection>) -> Self {
        Self {
            config,
            db,
            settings_cache: tokio::sync::RwLock::new(None),
            listener_handle: std::sync::Mutex::new(None),
        }
    }

    /// Get the base data directory path
    pub fn data_dir(&self) -> PathBuf {
        PathBuf::from(self.config.get_data_dir())
    }

    /// Whether the ClickHouse backend is usable at runtime — i.e. all four
    /// `TEMPS_CLICKHOUSE_*` env vars are populated (see
    /// [`ServerConfig::is_clickhouse_enabled`]). The metrics/analytics/OTel
    /// stores fall back to TimescaleDB when this is `false`, regardless of the
    /// `monitoring.store` DB toggle. Callers use this to report the *effective*
    /// storage backend rather than the configured-but-maybe-inactive one.
    pub fn is_clickhouse_enabled(&self) -> bool {
        self.config.is_clickhouse_enabled()
    }

    /// Parse the port from the main proxy listener address (`host:port`).
    ///
    /// Internal container traffic (OTLP metrics, agent callbacks) goes through
    /// the Pingora proxy on this port — the proxy routes `/api/*` to the
    /// console/API listener via a path rule. This is the conventional single
    /// public port operators expose. Falls back to 8080 if unparsable.
    pub fn proxy_port(&self) -> u16 {
        self.config
            .address
            .rsplit(':')
            .next()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(8080)
    }

    /// Resolve the internal URL service containers use to reach the Temps API
    /// from inside the Docker network. Reads the `internal_url` setting from
    /// the DB, falling back to `TEMPS_INTERNAL_API_URL` then
    /// `http://host.docker.internal:{proxy_port}`. No trailing slash.
    pub async fn resolve_internal_url(&self) -> String {
        let port = self.proxy_port();
        match self.get_settings().await {
            Ok(settings) => settings.resolve_internal_url(port),
            Err(_) => AppSettings::default().resolve_internal_url(port),
        }
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
            // Generate new key using OS CSPRNG
            let mut bytes = [0u8; 32];
            rand::rngs::OsRng.fill(&mut bytes);
            let key = hex::encode(bytes);

            // Ensure data directory exists
            tokio_fs::create_dir_all(self.data_dir()).await?;

            // Write key to file
            let mut file = tokio_fs::File::create(&key_path).await?;
            file.write_all(key.as_bytes()).await?;
            file.sync_all().await?;

            // Restrict permissions to owner-only
            ServerConfig::restrict_file_permissions(&key_path);

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
            // Generate new secret using OS CSPRNG (32 bytes as 64 hex characters)
            let mut bytes = [0u8; 32];
            rand::rngs::OsRng.fill(&mut bytes);
            let secret = hex::encode(bytes);

            // Ensure data directory exists
            tokio_fs::create_dir_all(self.data_dir()).await?;

            // Write secret to file
            let mut file = tokio_fs::File::create(&secret_path).await?;
            file.write_all(secret.as_bytes()).await?;
            file.sync_all().await?;

            // Restrict permissions to owner-only
            ServerConfig::restrict_file_permissions(&secret_path);

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

    /// Derive the URL scheme from `external_url` — returns "http" or "https".
    /// Defaults to "https" when no external_url is configured (production
    /// assumption) and to the actual scheme otherwise, so HTTP-only sslip.io
    /// installs emit `http://` links instead of dead `https://` ones.
    pub async fn get_url_scheme(&self) -> Result<String, ConfigServiceError> {
        let settings = self.get_settings().await?;
        Ok(match settings.external_url.as_deref() {
            Some(url) if url.starts_with("http://") => "http".to_string(),
            _ => "https".to_string(),
        })
    }

    /// Get the application settings
    pub async fn get_settings(&self) -> Result<AppSettings, ConfigServiceError> {
        // Serve from the in-memory cache while it is fresh — this is what keeps
        // the proxy's per-request callers off the database (see field docs).
        {
            let cached = self.settings_cache.read().await;
            if let Some((settings, fetched_at)) = cached.as_ref() {
                if fetched_at.elapsed() < SETTINGS_CACHE_TTL {
                    return Ok(settings.clone());
                }
            }
        }

        // Cache miss or stale: load from the DB and repopulate.
        let record = settings::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await?;

        let settings = record
            .map(|r| AppSettings::from_json(r.data))
            .unwrap_or_default();
        // Republish process-wide TLS opt-in so server-side make_client()
        // callsites (deployer, agent, providers) see the latest value
        // without an explicit init step at startup.
        temps_core::tls::set_insecure_tls(settings.insecure_tls);

        *self.settings_cache.write().await = Some((settings.clone(), std::time::Instant::now()));
        Ok(settings)
    }

    /// Update the application settings
    pub async fn update_settings(&self, settings: AppSettings) -> Result<(), ConfigServiceError> {
        let now = Utc::now();
        // Refresh the TLS opt-in cache as soon as the operator toggles it
        // in the settings UI; otherwise the change wouldn't take effect
        // until the next get_settings() call.
        temps_core::tls::set_insecure_tls(settings.insecure_tls);

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

        // Write-through: refresh the cache with the just-written value so an
        // admin's change takes effect immediately in this process, rather than
        // waiting out SETTINGS_CACHE_TTL.
        *self.settings_cache.write().await = Some((settings, std::time::Instant::now()));

        Ok(())
    }

    /// Drop the cached `AppSettings` snapshot so the next `get_settings` call
    /// re-reads from the database. Called by the `settings_change` LISTEN task
    /// when another process writes settings, and on listener
    /// reconnect-recovery (so a NOTIFY missed during a connection gap can't
    /// strand stale data). Takes the async write lock, so it must be awaited.
    pub async fn invalidate_settings_cache(&self) {
        *self.settings_cache.write().await = None;
        debug!("Invalidated AppSettings cache (settings_change NOTIFY)");
    }

    /// Spawn the background task that LISTENs on the Postgres `settings_change`
    /// channel and invalidates the in-memory settings cache the instant any
    /// process writes the settings row. This makes cross-process settings
    /// changes take effect immediately instead of waiting out the 5s
    /// `SETTINGS_CACHE_TTL` (which stays as the missed-NOTIFY safety net).
    ///
    /// Invoked once from `ConfigPlugin` against the shared singleton, so the
    /// task always invalidates the cache of the instance everyone reads from.
    /// Startup failure to connect is non-fatal — the TTL still refreshes the
    /// cache, just with up to 5s of latency. Mirrors the listener structure in
    /// `temps-routes::project_change_listener`.
    pub fn start_settings_listener(self: &std::sync::Arc<Self>) {
        let service = self.clone();
        let database_url = self.get_database_url();

        let handle = tokio::spawn(async move {
            use sqlx::postgres::{PgListener, PgPool};

            // Establish the initial connection + subscription. A failure here is
            // non-fatal: the cache TTL still expires settings within 5s.
            let pool = match PgPool::connect(&database_url).await {
                Ok(pool) => pool,
                Err(e) => {
                    warn!(
                        "settings_change listener: failed to connect to Postgres ({}); \
                         falling back to {}s cache TTL only",
                        e,
                        SETTINGS_CACHE_TTL.as_secs()
                    );
                    return;
                }
            };

            let mut pg_listener = match PgListener::connect_with(&pool).await {
                Ok(listener) => listener,
                Err(e) => {
                    warn!(
                        "settings_change listener: failed to create PgListener ({}); \
                         falling back to {}s cache TTL only",
                        e,
                        SETTINGS_CACHE_TTL.as_secs()
                    );
                    return;
                }
            };

            if let Err(e) = pg_listener.listen("settings_change").await {
                warn!(
                    "settings_change listener: failed to subscribe ({}); \
                     falling back to {}s cache TTL only",
                    e,
                    SETTINGS_CACHE_TTL.as_secs()
                );
                return;
            }
            info!("Started listening for settings_change events");

            // Pure event-driven loop: invalidate on each NOTIFY. After a
            // listener error we reconnect, re-subscribe, and invalidate once to
            // catch any change missed during the gap.
            loop {
                match pg_listener.recv().await {
                    Ok(_notification) => {
                        service.invalidate_settings_cache().await;
                    }
                    Err(e) => {
                        warn!("Error receiving settings_change notification: {}", e);

                        // Back off, then attempt to reconnect.
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

                        match PgListener::connect_with(&pool).await {
                            Ok(mut new_listener) => {
                                if let Err(e) = new_listener.listen("settings_change").await {
                                    warn!("Failed to re-subscribe to settings_change: {}", e);
                                } else {
                                    pg_listener = new_listener;
                                    info!("Reconnected to settings_change listener");
                                }
                            }
                            Err(e) => {
                                warn!("Failed to reconnect settings_change listener: {}", e);
                            }
                        }

                        // Recovery: invalidate so a NOTIFY missed during the gap
                        // can't leave the cache holding stale settings.
                        service.invalidate_settings_cache().await;
                    }
                }
            }
        });

        if let Ok(mut guard) = self.listener_handle.lock() {
            *guard = Some(handle);
        }
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

        // Determine protocol and port from external_url if set.
        //
        // This MUST match how the deployment overview builds the visitable URL
        // (`temps-deployments::compute_environment_url`/`compute_deployment_url`)
        // so that uptime monitors check the same endpoint the app actually
        // serves on. When `external_url` is unset (typical local install), the
        // app is reachable over HTTP on the proxy listener port (e.g. :8080),
        // NOT over HTTPS on :443 — defaulting to https here made monitors ping
        // an unreachable URL and report a false "Major Outage".
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
            ("http".to_string(), Some(self.proxy_port()))
        };

        // Use preview_domain if set, otherwise fallback to DEFAULT_LOCAL_DOMAIN
        let preview_domain = if !settings.preview_domain.is_empty() {
            settings.preview_domain.trim_start_matches("*.").to_string()
        } else {
            DEFAULT_LOCAL_DOMAIN.to_string()
        };
        // Deployment hostnames are identical across hostname strategies (single
        // label below the base domain), so no per-domain resolution is needed here.
        let hostname =
            PublicHostnameStrategy::Standard.deployment_hostname(&preview_domain, deployment_slug);

        // Construct the URL as [protocol]://{slug}.{preview_domain}[:port]
        // Only include port if it's non-standard (not 443 for https, not 80 for http)
        let url = if let Some(port) = port {
            let is_standard_port =
                (protocol == "https" && port == 443) || (protocol == "http" && port == 80);
            if is_standard_port {
                format!("{}://{}", protocol, hostname)
            } else {
                format!("{}://{}:{}", protocol, hostname, port)
            }
        } else {
            format!("{}://{}", protocol, hostname)
        };

        Ok(url)
    }
}

impl Drop for ConfigService {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.listener_handle.lock() {
            if let Some(handle) = guard.take() {
                handle.abort();
                debug!("settings_change listener stopped");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn test_config() -> Arc<ServerConfig> {
        Arc::new(
            ServerConfig::new(
                "127.0.0.1:3000".to_string(),
                "postgresql://test".to_string(),
                None,
                Some("127.0.0.1:8000".to_string()),
            )
            .expect("ServerConfig::new"),
        )
    }

    fn settings_row(preview_domain: &str) -> settings::Model {
        let s = AppSettings {
            preview_domain: preview_domain.to_string(),
            ..AppSettings::default()
        };
        settings::Model {
            id: 1,
            data: s.to_json(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    // The proxy reads settings on the per-request hot path, so get_settings()
    // must serve from the in-memory cache and NOT hit the DB every call.
    #[tokio::test]
    async fn get_settings_serves_from_cache_after_first_load() {
        // Queue exactly ONE query result. A second DB read (cache miss) would
        // find no queued result and return AppSettings::default() instead.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![settings_row("cached.example.com")]])
            .into_connection();
        let svc = ConfigService::new(test_config(), Arc::new(db));

        let first = svc.get_settings().await.expect("first get_settings");
        assert_eq!(first.preview_domain, "cached.example.com");

        // Second call must return the SAME cached value despite no second
        // queued query result — proving it did not touch the DB.
        let second = svc.get_settings().await.expect("second get_settings");
        assert_eq!(
            second.preview_domain, "cached.example.com",
            "second call must be served from cache, not a fresh (empty) DB read"
        );
    }

    // Admin updates must take effect immediately (write-through), not after TTL.
    #[tokio::test]
    async fn update_settings_refreshes_cache_write_through() {
        // 1 query result for the initial get; update_settings does a find_by_id
        // (returns the existing row) then an UPDATE exec.
        // Over-provision query results so the test asserts on cache behavior,
        // not on update_settings' exact internal query count: initial
        // get_settings, then update_settings' existence check, plus slack.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![
                vec![settings_row("old.example.com")],
                vec![settings_row("old.example.com")],
                vec![settings_row("old.example.com")],
            ])
            .append_exec_results(vec![
                sea_orm::MockExecResult {
                    last_insert_id: 1,
                    rows_affected: 1,
                },
                sea_orm::MockExecResult {
                    last_insert_id: 1,
                    rows_affected: 1,
                },
            ])
            .into_connection();
        let svc = ConfigService::new(test_config(), Arc::new(db));

        // Prime cache.
        assert_eq!(
            svc.get_settings().await.unwrap().preview_domain,
            "old.example.com"
        );

        // Write a new value.
        let updated = AppSettings {
            preview_domain: "new.example.com".to_string(),
            ..AppSettings::default()
        };
        svc.update_settings(updated).await.expect("update_settings");

        // get_settings must now return the new value WITHOUT another DB read
        // (no further query results are queued) — i.e. served from the
        // write-through-refreshed cache.
        assert_eq!(
            svc.get_settings().await.unwrap().preview_domain,
            "new.example.com",
            "update_settings must write through to the cache"
        );
    }

    // A settings_change NOTIFY must force the next get_settings() to re-read
    // from the DB instead of serving the stale cached snapshot (the whole point
    // of the cross-process invalidation path; the listener calls this method).
    #[tokio::test]
    async fn invalidate_settings_cache_forces_db_reread() {
        // Two DIFFERENT query results: the first get_settings() consumes "v1"
        // and caches it; after invalidation the second get_settings() consumes
        // "v2". If invalidation failed, the second call would serve the cached
        // "v1" and never reach the queued "v2" result.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![settings_row("v1")], vec![settings_row("v2")]])
            .into_connection();
        let svc = ConfigService::new(test_config(), Arc::new(db));

        // First read populates the cache with "v1".
        assert_eq!(svc.get_settings().await.unwrap().preview_domain, "v1");

        // Simulate the listener firing on a cross-process write.
        svc.invalidate_settings_cache().await;

        // Next read must hit the DB again and return the new "v2" value.
        assert_eq!(
            svc.get_settings().await.unwrap().preview_domain,
            "v2",
            "invalidate_settings_cache must force a fresh DB read, not serve the cached v1"
        );
    }
}
