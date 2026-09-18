// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::Utc;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, FromQueryResult,
    PaginatorTrait, QueryFilter, QuerySelect, Set, Statement, TransactionTrait,
};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use temps_database::DbConnection;
use temps_entities::{external_services, network_config, node_enrollment_tokens, nodes, settings};
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

/// Key of the geolocation section inside the singleton `settings.data`
/// document. Named once so the surgical, geo-only writer below cannot drift
/// from `AppSettings`' serde field name.
const GEO_SETTINGS_KEY: &str = "geo";

use serde_derive::{Deserialize, Serialize};
use temps_core::{AgentSandboxSettings, AppSettings, GeoLicenseKeyIntent, PublicHostnameStrategy};

/// Rebase credential-owned fields onto the row locked by the settings writer.
/// A bulk settings payload (including one built from an older GET) is never
/// allowed to create, restore, or verify a provider credential.
pub(crate) fn preserve_provider_credential_proof(
    incoming: &mut AppSettings,
    current: &AppSettings,
) {
    for (id, current_cfg) in &current.agent_sandbox.providers {
        match incoming.agent_sandbox.providers.get_mut(id) {
            Some(candidate) => {
                candidate.credentials_encrypted = current_cfg.credentials_encrypted.clone();
                candidate.auth_type = current_cfg.auth_type.clone();
                if !candidate.extra.is_object() {
                    candidate.extra = serde_json::json!({});
                }
                if let Some(extra) = candidate.extra.as_object_mut() {
                    extra.remove("credential_verified");
                    if let Some(proof) = current_cfg.extra.get("credential_verified") {
                        extra.insert("credential_verified".into(), proof.clone());
                    }
                }
            }
            None => {
                incoming
                    .agent_sandbox
                    .providers
                    .insert(id.clone(), current_cfg.clone());
            }
        }
    }
    for (id, candidate) in &mut incoming.agent_sandbox.providers {
        if !current.agent_sandbox.providers.contains_key(id) {
            candidate.credentials_encrypted = None;
            if let Some(extra) = candidate.extra.as_object_mut() {
                extra.remove("credential_verified");
            }
        }
    }
}

/// Rebase the geolocation section onto the row locked by the settings writer.
///
/// Two fields classes are restored:
///
/// * **Recorded state** (`source`, `build_epoch`, `last_refreshed_at`,
///   `last_check_at`, `last_check_status`, `last_error`) is written only by
///   the background refresh job, through `update_geo_settings`. A settings
///   save must never carry an older copy of it back over a check the job just
///   recorded, so the locked row always wins.
/// * **The encrypted license key** is kept from the locked row unless this
///   request explicitly set or cleared it. The handler encrypts (and consumes)
///   the plaintext before this lock is taken, so the incoming ciphertext alone
///   cannot be distinguished from one carried forward out of a stale snapshot.
pub(crate) fn preserve_geo_recorded_state(
    incoming: &mut AppSettings,
    current: &AppSettings,
    license_key_intent: GeoLicenseKeyIntent,
) {
    incoming.geo.preserve_recorded_state(&current.geo);
    match license_key_intent {
        GeoLicenseKeyIntent::Unchanged => {
            incoming.geo.maxmind_license_key_encrypted =
                current.geo.maxmind_license_key_encrypted.clone();
        }
        // The incoming value is this request's own result: fresh ciphertext
        // for `Set`, `None` for `Cleared`. Either way it is authoritative.
        GeoLicenseKeyIntent::Set | GeoLicenseKeyIntent::Cleared => {}
    }
}

#[derive(Error, Debug)]
pub enum ConfigServiceError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("AI provider '{provider_id}' credential changed during verification")]
    ProviderCredentialChanged { provider_id: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Setting not found: {key}")]
    SettingNotFound { key: String },

    #[error("Invalid configuration: {details}")]
    InvalidConfiguration { details: String },

    #[error("Settings section '{section}' is malformed")]
    MalformedSettingsSection { section: &'static str },

    #[error("Cluster CA is not initialized")]
    ClusterCaNotInitialized,

    #[error("Cluster CA fingerprint does not match the currently active trust root")]
    ClusterCaFingerprintMismatch,

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("OS randomness failed while {operation}: {reason}")]
    RandomnessFailed { operation: String, reason: String },

    #[error(
        "Failed to set TimescaleDB compression policy for {table} to {after_hours} hours: {source}"
    )]
    CompressionPolicyUpdate {
        table: &'static str,
        after_hours: u32,
        #[source]
        source: sea_orm::DbErr,
    },

    #[error(
        "Failed to set TimescaleDB retention policy for {table} to {after_days} days: {source}"
    )]
    RetentionPolicyUpdate {
        table: &'static str,
        after_days: u32,
        #[source]
        source: sea_orm::DbErr,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterCaRotationResult {
    pub previous_fingerprint: String,
    pub revoked_enrollment_tokens: u64,
}

fn fill_secure_random_bytes(operation: &str, bytes: &mut [u8]) -> Result<(), ConfigServiceError> {
    fill_random_bytes_with(&mut rand::rngs::SysRng, operation, bytes)
}

fn fill_random_bytes_with<R: rand::TryCryptoRng>(
    rng: &mut R,
    operation: &str,
    bytes: &mut [u8],
) -> Result<(), ConfigServiceError> {
    rng.try_fill_bytes(bytes)
        .map_err(|error| ConfigServiceError::RandomnessFailed {
            operation: operation.to_string(),
            reason: error.to_string(),
        })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffectiveTelemetryPolicies {
    pub metrics_raw_days: Option<u32>,
    pub metrics_hourly_days: Option<u32>,
    pub metrics_daily_years: Option<u32>,
    pub proxy_logs_compression_hours: Option<u32>,
    pub otel_spans_compression_hours: Option<u32>,
    pub proxy_logs_retention_days: Option<u32>,
    pub otel_spans_retention_days: Option<u32>,
    pub otel_logs_retention_days: Option<u32>,
    pub otel_metrics_retention_days: Option<u32>,
}

/// Effective cluster-wide container-network allocation state.
///
/// The pool is mutable only before the first control-plane or worker subnet is
/// allocated. Keeping that lock state beside the values lets operator surfaces
/// explain why an established cluster cannot be edited in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterNetworkState {
    pub compute_pool_cidr: String,
    pub subnet_prefix_len: u8,
    pub allocation_count: u64,
}

#[derive(Debug, FromQueryResult)]
struct TimescalePolicyRow {
    hypertable_name: String,
    proc_name: String,
    interval_seconds: i64,
}

const SECONDS_PER_HOUR: i64 = 60 * 60;
const SECONDS_PER_DAY: i64 = 24 * SECONDS_PER_HOUR;

fn rounded_u32(value: i64, divisor: i64) -> Option<u32> {
    if value <= 0 {
        return None;
    }
    u32::try_from((value + divisor / 2) / divisor).ok()
}

fn policies_from_rows(rows: Vec<TimescalePolicyRow>) -> EffectiveTelemetryPolicies {
    let mut policies = EffectiveTelemetryPolicies::default();

    for row in rows {
        let value = match row.proc_name.as_str() {
            "policy_compression" => rounded_u32(row.interval_seconds, SECONDS_PER_HOUR),
            "policy_retention" => rounded_u32(row.interval_seconds, SECONDS_PER_DAY),
            _ => None,
        };
        let Some(value) = value else {
            continue;
        };

        match (row.hypertable_name.as_str(), row.proc_name.as_str()) {
            ("service_metrics", "policy_retention") => policies.metrics_raw_days = Some(value),
            ("service_metrics_hourly", "policy_retention") => {
                policies.metrics_hourly_days = Some(value)
            }
            ("service_metrics_daily", "policy_retention") => {
                // The API represents this tier in whole years. Do not claim an
                // arbitrary manually configured day count (for example 500
                // days) is an exact year value. Leaving it unset falls back to
                // the configured value, and the next save reconciles the drift.
                if value % 365 == 0 {
                    policies.metrics_daily_years = Some(value / 365);
                }
            }
            ("proxy_logs", "policy_compression") => {
                policies.proxy_logs_compression_hours = Some(value)
            }
            ("otel_spans", "policy_compression") => {
                policies.otel_spans_compression_hours = Some(value)
            }
            ("proxy_logs", "policy_retention") => policies.proxy_logs_retention_days = Some(value),
            ("otel_spans", "policy_retention") => policies.otel_spans_retention_days = Some(value),
            ("otel_log_events", "policy_retention") => {
                policies.otel_logs_retention_days = Some(value)
            }
            ("otel_metrics", "policy_retention") => {
                policies.otel_metrics_retention_days = Some(value)
            }
            _ => {}
        }
    }

    policies
}

fn compression_policy_sql(table: &'static str, after_hours: u32) -> String {
    format!(
        "SELECT remove_compression_policy('{table}', if_exists => TRUE); \
         SELECT add_compression_policy(\
             '{table}', \
             compress_after => make_interval(hours => {after_hours}), \
             if_not_exists => TRUE\
         )"
    )
}

async fn replace_compression_policy<C>(
    db: &C,
    table: &'static str,
    after_hours: u32,
) -> Result<(), ConfigServiceError>
where
    C: ConnectionTrait,
{
    db.execute_unprepared(&compression_policy_sql(table, after_hours))
        .await
        .map_err(|source| ConfigServiceError::CompressionPolicyUpdate {
            table,
            after_hours,
            source,
        })?;
    Ok(())
}

fn retention_policy_sql(table: &'static str, after_days: u32) -> String {
    format!(
        "SELECT remove_retention_policy('{table}', if_exists => TRUE); \
         SELECT add_retention_policy(\
             '{table}', \
             drop_after => make_interval(days => {after_days}), \
             if_not_exists => TRUE\
         )"
    )
}

async fn replace_retention_policy<C>(
    db: &C,
    table: &'static str,
    after_days: u32,
) -> Result<(), ConfigServiceError>
where
    C: ConnectionTrait,
{
    db.execute_unprepared(&retention_policy_sql(table, after_days))
        .await
        .map_err(|source| ConfigServiceError::RetentionPolicyUpdate {
            table,
            after_days,
            source,
        })?;
    Ok(())
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

    // Required Docker networks that every deployed app container should join
    // before start, in addition to Temps' primary app network. This is useful
    // for self-hosted installs where apps depend on services exposed by a
    // sibling Docker Compose network.
    pub docker_extra_networks: Vec<String>,
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
            let secret = Self::generate_auth_secret()?;
            fs::write(&auth_secret_path, &secret)?;
            Self::restrict_file_permissions(&auth_secret_path);
            secret
        };

        // Generate or load encryption_key (32 bytes in hex format)
        let encryption_key_path = data_dir.join("encryption_key");
        let encryption_key = if encryption_key_path.exists() {
            fs::read_to_string(&encryption_key_path)?.trim().to_string()
        } else {
            let key = Self::generate_encryption_key()?;
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

            docker_extra_networks: parse_csv_env("TEMPS_DOCKER_EXTRA_NETWORKS"),
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
    fn generate_auth_secret() -> Result<String, ConfigServiceError> {
        let mut bytes = [0u8; 32];
        fill_secure_random_bytes("generating the server auth secret", &mut bytes)?;
        Ok(hex::encode(bytes))
    }

    /// Generate a 32-byte encryption key (64 hex characters)
    fn generate_encryption_key() -> Result<String, ConfigServiceError> {
        let mut bytes = [0u8; 32];
        fill_secure_random_bytes("generating the server encryption key", &mut bytes)?;
        Ok(hex::encode(bytes))
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

fn parse_csv_env(key: &str) -> Vec<String> {
    std::env::var(key)
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
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

#[derive(Default)]
struct SettingsCacheState {
    snapshot: Option<(AppSettings, std::time::Instant)>,
    /// Monotonic generation protecting publication from stale in-flight
    /// database reads. Kept under the same lock as the snapshot and global TLS
    /// publication so invalidation cannot split the generation check from its
    /// side effects.
    generation: u64,
}

pub struct ConfigService {
    config: Arc<ServerConfig>,
    db: Arc<DbConnection>,
    /// In-memory cache of the singleton settings row so `get_settings` does not
    /// do a DB round-trip on every call. The proxy reads settings per request
    /// (security headers, preview gateway, on-demand TLS), so an uncached read
    /// would amplify any request flood into a Postgres QPS flood. Invalidated
    /// write-through by `update_settings`; otherwise refreshed after
    /// `SETTINGS_CACHE_TTL`.
    settings_cache: tokio::sync::RwLock<SettingsCacheState>,
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
            settings_cache: tokio::sync::RwLock::new(SettingsCacheState::default()),
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

    /// Read the active TimescaleDB retention/compression durations in one
    /// metadata query. This view contains one row per background policy, so
    /// the query does not scan telemetry data or hypertable chunks.
    pub async fn get_effective_telemetry_policies(
        &self,
    ) -> Result<EffectiveTelemetryPolicies, ConfigServiceError> {
        if !self.is_postgres() {
            return Ok(EffectiveTelemetryPolicies::default());
        }

        let rows = TimescalePolicyRow::find_by_statement(Statement::from_string(
            DatabaseBackend::Postgres,
            r#"
SELECT
    hypertable_name,
    proc_name,
    EXTRACT(
        EPOCH FROM (
            CASE proc_name
                WHEN 'policy_compression' THEN config ->> 'compress_after'
                WHEN 'policy_retention' THEN config ->> 'drop_after'
            END
        )::interval
    )::BIGINT AS interval_seconds
FROM timescaledb_information.jobs
WHERE proc_name IN ('policy_compression', 'policy_retention')
  AND hypertable_schema = current_schema()
  AND hypertable_name IN (
      'service_metrics',
      'service_metrics_hourly',
      'service_metrics_daily',
      'proxy_logs',
      'otel_spans',
      'otel_log_events',
      'otel_metrics'
  )
"#
            .to_owned(),
        ))
        .all(self.db.as_ref())
        .await?;

        Ok(policies_from_rows(rows))
    }

    /// Count exactly the services the MetricsScraper will include in its next
    /// cycle. This is a COUNT over the control-plane service table, not a scan
    /// of metric samples.
    pub async fn count_monitored_services(&self) -> Result<u64, ConfigServiceError> {
        external_services::Entity::find()
            .filter(external_services::Column::MetricsEnabled.eq(true))
            .filter(external_services::Column::Status.eq("running"))
            .count(self.db.as_ref())
            .await
            .map_err(ConfigServiceError::from)
    }

    /// Read the cluster-wide overlay pool and whether any node already owns a
    /// subnet. This is deliberately read-only: changes must go through the
    /// allocator's exclusive-lock and no-existing-allocation guard.
    pub async fn get_cluster_network_state(
        &self,
    ) -> Result<ClusterNetworkState, ConfigServiceError> {
        let config = network_config::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ConfigServiceError::InvalidConfiguration {
                details: "network_config singleton row is missing".to_string(),
            })?;
        let subnet_prefix_len = u8::try_from(config.subnet_prefix_len).map_err(|_| {
            ConfigServiceError::InvalidConfiguration {
                details: format!(
                    "network_config subnet prefix {} is outside the supported u8 range",
                    config.subnet_prefix_len
                ),
            }
        })?;
        let worker_allocations = nodes::Entity::find()
            .filter(nodes::Column::ComputeCidr.is_not_null())
            .count(self.db.as_ref())
            .await?;

        Ok(ClusterNetworkState {
            compute_pool_cidr: config.compute_pool_cidr,
            subnet_prefix_len,
            allocation_count: worker_allocations
                + u64::from(config.control_plane_compute_cidr.is_some()),
        })
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
            fill_secure_random_bytes("creating the persisted encryption key", &mut bytes)?;
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
            fill_secure_random_bytes("creating the persisted auth secret", &mut bytes)?;
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
            if let Some((settings, fetched_at)) = cached.snapshot.as_ref() {
                if fetched_at.elapsed() < SETTINGS_CACHE_TTL {
                    return Ok(settings.clone());
                }
            }
        }

        let generation = self.settings_cache.read().await.generation;

        // Cache miss or stale: load from the DB and repopulate.
        let record = settings::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await?;

        let settings = record
            .map(|r| AppSettings::from_json(r.data))
            .unwrap_or_default();
        self.cache_settings_if_current(generation, settings.clone())
            .await;
        Ok(settings)
    }

    /// Load only the AI sandbox section from the authoritative settings row.
    /// This deliberately bypasses whole-document decoding and its cache: an
    /// unrelated malformed section must neither hide valid provider settings
    /// nor cause stale credentials to be returned.
    pub async fn get_agent_sandbox_settings(
        &self,
    ) -> Result<AgentSandboxSettings, ConfigServiceError> {
        let record = settings::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await?;
        let Some(value) = record.and_then(|row| row.data.get("agent_sandbox").cloned()) else {
            return Ok(AgentSandboxSettings::default());
        };
        serde_json::from_value(value).map_err(|_| ConfigServiceError::MalformedSettingsSection {
            section: "agent_sandbox",
        })
    }

    async fn cache_settings_if_current(&self, generation: u64, settings: AppSettings) -> bool {
        let mut cache = self.settings_cache.write().await;
        if cache.generation != generation {
            return false;
        }
        // Publish the process-wide TLS opt-in under the same generation
        // guard as the settings snapshot. A DB read started before an
        // invalidation must not be able to restore stale TLS behavior after
        // the newer settings have become authoritative.
        temps_core::tls::set_insecure_tls(settings.insecure_tls);
        cache.snapshot = Some((settings, std::time::Instant::now()));
        true
    }

    async fn publish_committed_settings_if_current(
        &self,
        generation: u64,
        settings: AppSettings,
    ) -> bool {
        let mut cache = self.settings_cache.write().await;
        if cache.generation != generation {
            return false;
        }
        cache.generation = cache.generation.wrapping_add(1);
        temps_core::tls::set_insecure_tls(settings.insecure_tls);
        cache.snapshot = Some((settings, std::time::Instant::now()));
        true
    }

    /// Update the application settings.
    ///
    /// Equivalent to [`Self::update_settings_with_geo_intent`] with
    /// [`GeoLicenseKeyIntent::Unchanged`]: a caller that does not say it is
    /// changing the MaxMind license key never changes it.
    pub async fn update_settings(&self, settings: AppSettings) -> Result<(), ConfigServiceError> {
        self.update_settings_with_geo_intent(settings, GeoLicenseKeyIntent::Unchanged)
            .await
    }

    /// Update the application settings, declaring whether this write intends
    /// to change the stored MaxMind license key.
    ///
    /// `geo_license_key_intent` exists because the geo section has a second
    /// writer: the background refresh job records `source`/`build_epoch`/
    /// `last_check_*` through [`Self::update_geo_settings`] on its own timer.
    /// An admin's settings PUT is built from a 5-second-cached snapshot, so
    /// without an explicit signal the request cannot tell "the admin submitted
    /// this key" from "this ciphertext came out of a snapshot that is already
    /// stale" — and the latter silently reverts a key saved moments earlier.
    /// The recorded freshness metadata is likewise always taken from the
    /// locked row, never from the request.
    pub async fn update_settings_with_geo_intent(
        &self,
        mut settings: AppSettings,
        geo_license_key_intent: GeoLicenseKeyIntent,
    ) -> Result<(), ConfigServiceError> {
        let now = Utc::now();
        let cache_generation = self.settings_cache.read().await.generation;

        // The settings row can drift from the actual TimescaleDB jobs (for
        // example after a manual policy change). Prefer the live, tiny policy
        // snapshot when deciding what must be replaced. If metadata is
        // temporarily unavailable, retain the previous settings comparison so
        // unrelated settings can still be saved.
        let effective_policies = if self.is_postgres() {
            match self.get_effective_telemetry_policies().await {
                Ok(policies) => Some(policies),
                Err(error) => {
                    warn!(%error, "Failed to read active TimescaleDB policies before settings update");
                    None
                }
            }
        } else {
            None
        };

        // Persist the settings and replace changed TimescaleDB policies in one
        // transaction. A policy error therefore cannot leave the API reporting
        // a delay that the database did not actually apply (or vice versa).
        let txn = self.db.begin().await?;

        // Check if record exists.
        //
        // Locked FOR UPDATE on Postgres: this is a read-modify-write of a
        // shared JSON document, and the window between the read and the write
        // below spans the TimescaleDB policy comparison — seconds, not
        // microseconds. Without the lock two concurrent writers both merge
        // onto the same pre-race snapshot and the loser's sub-document (e.g.
        // the admin gate's allowlist, saved from the console at the same
        // moment as a startup `console_version` write) is silently dropped
        // despite `to_json_merged`. SQLite serializes write transactions
        // already and has no `FOR UPDATE`, so the lock is Postgres-only.
        let existing_query = settings::Entity::find_by_id(1);
        let existing_query = if self.is_postgres() {
            existing_query.lock_exclusive()
        } else {
            existing_query
        };
        let existing = existing_query.one(&txn).await?;

        // The handler's earlier snapshot is advisory only. Rebase against the
        // authoritative row while its write lock is held, before serializing.
        let locked_settings = existing
            .as_ref()
            .map(|model| AppSettings::from_json(model.data.clone()))
            .unwrap_or_default();
        // Consent belongs to the SystemAdmin-only plugin endpoint. A generic
        // settings save must not undo a consent update committed before this lock.
        settings.plugin_installation_reporting_enabled =
            locked_settings.plugin_installation_reporting_enabled;
        preserve_provider_credential_proof(&mut settings, &locked_settings);
        // The geo section's freshness metadata belongs to the refresh job, and
        // its license key belongs to whichever request last submitted one.
        // Both are rebased here, under the lock, so a settings save built from
        // a stale snapshot can neither erase a check the job just recorded nor
        // revert a key that was stored while this request was in flight.
        preserve_geo_recorded_state(&mut settings, &locked_settings, geo_license_key_intent);

        let previous_compression = existing
            .as_ref()
            .map(|model| AppSettings::from_json(model.data.clone()).observability_compression)
            .unwrap_or_default();
        let previous_retention = existing
            .as_ref()
            .map(|model| AppSettings::from_json(model.data.clone()).observability_retention)
            .unwrap_or_default();
        let previous_monitoring = existing
            .as_ref()
            .map(|model| AppSettings::from_json(model.data.clone()).monitoring)
            .unwrap_or_default();

        if self.db.get_database_backend() == DatabaseBackend::Postgres {
            let monitoring = &settings.monitoring;
            let metric_policies = [
                (
                    "service_metrics",
                    monitoring.retention_raw_days,
                    effective_policies
                        .as_ref()
                        .map(|policies| policies.metrics_raw_days)
                        .unwrap_or(Some(previous_monitoring.retention_raw_days)),
                ),
                (
                    "service_metrics_hourly",
                    monitoring.retention_hourly_days,
                    effective_policies
                        .as_ref()
                        .map(|policies| policies.metrics_hourly_days)
                        .unwrap_or(Some(previous_monitoring.retention_hourly_days)),
                ),
                (
                    "service_metrics_daily",
                    monitoring.retention_daily_years.saturating_mul(365),
                    effective_policies
                        .as_ref()
                        .map(|policies| {
                            policies
                                .metrics_daily_years
                                .map(|years| years.saturating_mul(365))
                        })
                        .unwrap_or(Some(
                            previous_monitoring
                                .retention_daily_years
                                .saturating_mul(365),
                        )),
                ),
            ];
            for (table, after_days, previous_days) in metric_policies {
                if Some(after_days) != previous_days {
                    replace_retention_policy(&txn, table, after_days).await?;
                }
            }

            let compression = &settings.observability_compression;
            let proxy_logs_compression_hours = effective_policies
                .as_ref()
                .map(|policies| policies.proxy_logs_compression_hours)
                .unwrap_or(Some(previous_compression.proxy_logs_after_hours));
            if Some(compression.proxy_logs_after_hours) != proxy_logs_compression_hours {
                replace_compression_policy(&txn, "proxy_logs", compression.proxy_logs_after_hours)
                    .await?;
            }
            let otel_spans_compression_hours = effective_policies
                .as_ref()
                .map(|policies| policies.otel_spans_compression_hours)
                .unwrap_or(Some(previous_compression.otel_spans_after_hours));
            if Some(compression.otel_spans_after_hours) != otel_spans_compression_hours {
                replace_compression_policy(&txn, "otel_spans", compression.otel_spans_after_hours)
                    .await?;
            }

            let retention = &settings.observability_retention;
            let policies = [
                (
                    "proxy_logs",
                    retention.proxy_logs_days,
                    effective_policies
                        .as_ref()
                        .map(|policies| policies.proxy_logs_retention_days)
                        .unwrap_or(Some(previous_retention.proxy_logs_days)),
                ),
                (
                    "otel_spans",
                    retention.otel_spans_days,
                    effective_policies
                        .as_ref()
                        .map(|policies| policies.otel_spans_retention_days)
                        .unwrap_or(Some(previous_retention.otel_spans_days)),
                ),
                (
                    "otel_log_events",
                    retention.otel_logs_days,
                    effective_policies
                        .as_ref()
                        .map(|policies| policies.otel_logs_retention_days)
                        .unwrap_or(Some(previous_retention.otel_logs_days)),
                ),
                (
                    "otel_metrics",
                    retention.otel_metrics_days,
                    effective_policies
                        .as_ref()
                        .map(|policies| policies.otel_metrics_retention_days)
                        .unwrap_or(Some(previous_retention.otel_metrics_days)),
                ),
            ];
            for (table, after_days, previous_days) in policies {
                if Some(after_days) != previous_days {
                    replace_retention_policy(&txn, table, after_days).await?;
                }
            }
        }

        if let Some(existing_model) = existing {
            // Update existing settings. Merge into the stored document rather
            // than replacing it: the `settings` row carries sub-documents owned
            // by other subsystems (`admin_gate`) that `AppSettings` cannot
            // round-trip. See `AppSettings::to_json_merged`.
            let merged = settings.to_json_merged(&existing_model.data);
            let mut active_model: settings::ActiveModel = existing_model.into();
            active_model.data = Set(merged);
            active_model.updated_at = Set(now);
            active_model.update(&txn).await?;
        } else {
            // Create new settings
            let new_settings = settings::ActiveModel {
                id: Set(1),
                data: Set(settings.to_json()),
                created_at: Set(now),
                updated_at: Set(now),
            };
            new_settings.insert(&txn).await?;
        }

        txn.commit().await?;

        // A dedicated writer may commit and invalidate between our commit and
        // cache publication. Its generation change prevents this older bulk
        // snapshot from replacing the authoritative cache.
        if !self
            .publish_committed_settings_if_current(cache_generation, settings)
            .await
        {
            self.invalidate_settings_cache().await;
        }

        Ok(())
    }

    /// Drop the cached `AppSettings` snapshot so the next `get_settings` call
    /// re-reads from the database. Called by the `settings_change` LISTEN task
    /// when another process writes settings, and on listener
    /// reconnect-recovery (so a NOTIFY missed during a connection gap can't
    /// strand stale data). Takes the async write lock, so it must be awaited.
    pub async fn invalidate_settings_cache(&self) {
        {
            let mut cache = self.settings_cache.write().await;
            cache.generation = cache.generation.wrapping_add(1);
            cache.snapshot = None;
            // Fail closed until the authoritative row has been reloaded. This
            // is important when the invalidated value had insecure TLS enabled.
            temps_core::tls::set_insecure_tls(false);
        }

        // Reload immediately so cross-process settings changes (including an
        // intentional insecure-TLS opt-in) become effective when the NOTIFY is
        // processed, rather than waiting for an unrelated future caller.
        if let Err(error) = self.get_settings().await {
            warn!(%error, "Failed to reload AppSettings after cache invalidation; strict TLS remains enabled");
        }
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

    /// Atomically update only managed Cloud export consent. The settings row
    /// is shared by many subsystems, so reading through the cache and writing
    /// the whole document would lose concurrent unrelated changes.
    pub async fn update_cloud_features(
        &self,
        telemetry_enabled: bool,
        backups_enabled: bool,
        notifications_enabled: bool,
    ) -> Result<AppSettings, ConfigServiceError> {
        let transaction = self.db.begin().await?;
        let query = settings::Entity::find_by_id(1);
        let query = if self.is_postgres() {
            query.lock_exclusive()
        } else {
            query
        };
        let existing = query.one(&transaction).await?;
        let mut current = existing
            .as_ref()
            .map(|model| AppSettings::from_json(model.data.clone()))
            .unwrap_or_default();
        current.cloud.telemetry_enabled = telemetry_enabled;
        current.cloud.backups_enabled = backups_enabled;
        current.cloud.notifications_enabled = notifications_enabled;
        let now = Utc::now();
        if let Some(model) = existing {
            let merged = current.to_json_merged(&model.data);
            let mut active: settings::ActiveModel = model.into();
            active.data = Set(merged);
            active.updated_at = Set(now);
            active.update(&transaction).await?;
        } else {
            settings::ActiveModel {
                id: Set(1),
                data: Set(current.to_json()),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(&transaction)
            .await?;
        }
        transaction.commit().await?;
        // Invalidate instead of publishing this transaction's clone: another
        // writer may commit later but update the cache earlier, and publishing
        // here would then regress the cache out of commit order.
        self.invalidate_settings_cache().await;
        Ok(current)
    }

    /// Atomically update only the geolocation section of the shared settings
    /// row.
    ///
    /// The geo database refresh job records its freshness metadata
    /// (`last_check_at`, `source`, `build_epoch`, ...) on its own schedule,
    /// which can land in the same instant as an admin saving an unrelated
    /// settings page. Going through `update_setting_field` would read the whole
    /// document through the 5s cache and write it back, so whichever writer
    /// committed second would discard the other's change. Here the row is
    /// re-read under an exclusive lock and only `geo` is mutated.
    ///
    /// The closure receives the *stored* section, so callers can preserve
    /// fields they are not writing (e.g. recording a failed check must leave
    /// the last successful refresh intact).
    ///
    /// Only the document's `"geo"` key is parsed and rewritten. Deserializing
    /// the whole row into `AppSettings` would be unsafe here: `from_json` is
    /// `unwrap_or_default()`, so one malformed key anywhere in the document
    /// would turn this unattended, timer-driven write into a reset of every
    /// unrelated setting on it (MFA requirements, security headers, rate
    /// limits, IP trust, ceilings) with no admin action and no audit entry.
    /// A `geo` section that will not deserialize is therefore reported as
    /// [`ConfigServiceError::MalformedSettingsSection`] and nothing is
    /// written, rather than silently overwritten with defaults — which would
    /// discard the stored license key.
    pub async fn update_geo_settings<F>(
        &self,
        mutate: F,
    ) -> Result<temps_core::GeoSettings, ConfigServiceError>
    where
        F: FnOnce(&mut temps_core::GeoSettings),
    {
        let transaction = self.db.begin().await?;
        let query = settings::Entity::find_by_id(1);
        let query = if self.is_postgres() {
            query.lock_exclusive()
        } else {
            query
        };
        let existing = query.one(&transaction).await?;
        let now = Utc::now();

        if let Some(model) = existing {
            let mut document = model.data.clone();
            let mut geo = match document.get(GEO_SETTINGS_KEY) {
                None | Some(serde_json::Value::Null) => temps_core::GeoSettings::default(),
                Some(stored) => serde_json::from_value(stored.clone()).map_err(|error| {
                    warn!(
                        %error,
                        "Stored geolocation settings are malformed; refusing to overwrite them \
                         with defaults from the refresh job"
                    );
                    ConfigServiceError::MalformedSettingsSection {
                        section: GEO_SETTINGS_KEY,
                    }
                })?,
            };
            mutate(&mut geo);
            let updated = geo.clone();
            let geo_json = serde_json::to_value(&geo).map_err(|error| {
                ConfigServiceError::Serialization(format!(
                    "Failed to serialize the geolocation settings section: {error}"
                ))
            })?;

            match document.as_object_mut() {
                // Every other key is left byte-for-byte as it was read.
                Some(object) => {
                    object.insert(GEO_SETTINGS_KEY.to_string(), geo_json);
                }
                // Not a JSON object at all (a corrupt or `null` `data`
                // column): there is nothing to preserve, so write a document
                // that carries only the section this method owns.
                None => {
                    document = serde_json::json!({ GEO_SETTINGS_KEY: geo_json });
                }
            }

            let mut active: settings::ActiveModel = model.into();
            active.data = Set(document);
            active.updated_at = Set(now);
            active.update(&transaction).await?;
            transaction.commit().await?;
            // Invalidate rather than publish this clone, for the same
            // commit-order reason documented on `update_cloud_features`.
            self.invalidate_settings_cache().await;
            return Ok(updated);
        }

        // No row yet (a fresh install whose first geo check runs before any
        // settings save): insert the defaults with this section applied.
        let mut geo = temps_core::GeoSettings::default();
        mutate(&mut geo);
        let updated = geo.clone();
        let settings = AppSettings {
            geo,
            ..AppSettings::default()
        };
        settings::ActiveModel {
            id: Set(1),
            data: Set(settings.to_json()),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&transaction)
        .await?;
        transaction.commit().await?;
        // Invalidate rather than publish this clone, for the same commit-order
        // reason documented on `update_cloud_features`.
        self.invalidate_settings_cache().await;
        Ok(updated)
    }

    /// Atomically merge one provider credential into the shared settings row.
    /// `expected` binds a verification result to the exact saved credential
    /// that was probed; a concurrent replacement is never restored or marked
    /// verified by an older in-flight request.
    pub async fn update_agent_provider_credential(
        &self,
        provider_id: &str,
        auth_type: &str,
        encrypted: &str,
        verified: bool,
        verified_default_model: Option<&str>,
        expected: Option<(&str, &str)>,
    ) -> Result<(), ConfigServiceError> {
        let transaction = self.db.begin().await?;
        let query = settings::Entity::find_by_id(1);
        let query = if self.is_postgres() {
            query.lock_exclusive()
        } else {
            query
        };
        let existing = query.one(&transaction).await?;
        let mut data = existing
            .as_ref()
            .map(|row| row.data.clone())
            .unwrap_or_else(|| serde_json::json!({}));
        let providers = data
            .as_object_mut()
            .and_then(|root| {
                root.entry("agent_sandbox")
                    .or_insert_with(|| serde_json::json!({}))
                    .as_object_mut()
            })
            .and_then(|sandbox| {
                sandbox
                    .entry("providers")
                    .or_insert_with(|| serde_json::json!({}))
                    .as_object_mut()
            })
            .ok_or_else(|| ConfigServiceError::InvalidConfiguration {
                details: "agent_sandbox.providers is not a JSON object".into(),
            })?;
        let mut provider = providers
            .get(provider_id)
            .and_then(serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default();
        if let Some((expected_auth_type, expected_encrypted)) = expected {
            if provider
                .get("auth_type")
                .and_then(serde_json::Value::as_str)
                != Some(expected_auth_type)
                || provider
                    .get("credentials_encrypted")
                    .and_then(serde_json::Value::as_str)
                    != Some(expected_encrypted)
            {
                return Err(ConfigServiceError::ProviderCredentialChanged {
                    provider_id: provider_id.into(),
                });
            }
        }
        provider.insert(
            "auth_type".into(),
            serde_json::Value::String(auth_type.into()),
        );
        provider.insert(
            "credentials_encrypted".into(),
            serde_json::Value::String(encrypted.into()),
        );
        if let Some(model) = verified_default_model {
            provider.insert(
                "default_model".into(),
                serde_json::Value::String(model.into()),
            );
        }
        let extra = provider
            .entry("extra")
            .or_insert_with(|| serde_json::json!({}));
        if !extra.is_object() {
            *extra = serde_json::json!({});
        }
        let extra =
            extra
                .as_object_mut()
                .ok_or_else(|| ConfigServiceError::InvalidConfiguration {
                    details: format!(
                        "AI provider '{provider_id}' extra settings is not a JSON object"
                    ),
                })?;
        extra.insert(
            "credential_verified".into(),
            serde_json::Value::Bool(verified),
        );
        providers.insert(provider_id.into(), serde_json::Value::Object(provider));
        if let Some(row) = existing {
            let mut active: settings::ActiveModel = row.into();
            active.data = Set(data);
            active.updated_at = Set(Utc::now());
            active.update(&transaction).await?;
        } else {
            settings::ActiveModel {
                id: Set(1),
                data: Set(data),
                created_at: Set(Utc::now()),
                updated_at: Set(Utc::now()),
            }
            .insert(&transaction)
            .await?;
        }
        transaction.commit().await?;
        self.invalidate_settings_cache().await;
        Ok(())
    }

    /// Change the active harness without losing concurrent credential updates.
    pub async fn activate_agent_provider(
        &self,
        provider_id: &str,
    ) -> Result<(), ConfigServiceError> {
        self.mutate_agent_sandbox_json(|sandbox| {
            let has_credential = sandbox
                .get("providers")
                .and_then(serde_json::Value::as_object)
                .and_then(|providers| providers.get(provider_id))
                .and_then(|provider| provider.get("credentials_encrypted"))
                .and_then(serde_json::Value::as_str)
                .is_some();
            if !has_credential {
                return Err(ConfigServiceError::InvalidConfiguration {
                    details: format!("Provider '{provider_id}' has no saved credential"),
                });
            }
            sandbox.insert(
                "default_provider".into(),
                serde_json::Value::String(provider_id.into()),
            );
            Ok(())
        })
        .await
    }

    /// Update provider preferences while retaining its latest credential.
    pub async fn update_agent_provider_preferences(
        &self,
        provider_id: &str,
        default_auth_type: &str,
        default_model: Option<&str>,
        turns: [Option<i32>; 3],
    ) -> Result<[Option<i32>; 3], ConfigServiceError> {
        self.mutate_agent_sandbox_json(|sandbox| {
            let providers = sandbox
                .entry("providers")
                .or_insert_with(|| serde_json::json!({}))
                .as_object_mut()
                .ok_or_else(|| ConfigServiceError::InvalidConfiguration {
                    details: "agent_sandbox.providers is not a JSON object".into(),
                })?;
            let mut provider = providers
                .get(provider_id)
                .and_then(serde_json::Value::as_object)
                .cloned()
                .unwrap_or_default();
            provider.insert(
                "default_model".into(),
                default_model.map_or(serde_json::Value::Null, |model| {
                    serde_json::Value::String(model.into())
                }),
            );
            let keys = ["max_turns_analysis", "max_turns_fix", "max_turns_feedback"];
            for (key, value) in keys.into_iter().zip(turns) {
                if let Some(value) = value {
                    provider.insert(
                        key.into(),
                        if value == 0 {
                            serde_json::Value::Null
                        } else {
                            serde_json::Value::from(value)
                        },
                    );
                }
            }
            provider
                .entry("auth_type")
                .or_insert_with(|| serde_json::Value::String(default_auth_type.into()));
            provider.entry("extra").or_insert(serde_json::Value::Null);
            let result = keys.map(|key| {
                provider
                    .get(key)
                    .and_then(serde_json::Value::as_i64)
                    .map(|value| value as i32)
            });
            providers.insert(provider_id.into(), serde_json::Value::Object(provider));
            Ok(result)
        })
        .await
    }

    async fn mutate_agent_sandbox_json<T>(
        &self,
        mutate: impl FnOnce(
            &mut serde_json::Map<String, serde_json::Value>,
        ) -> Result<T, ConfigServiceError>,
    ) -> Result<T, ConfigServiceError> {
        let transaction = self.db.begin().await?;
        let query = settings::Entity::find_by_id(1);
        let query = if self.is_postgres() {
            query.lock_exclusive()
        } else {
            query
        };
        let existing = query.one(&transaction).await?;
        let mut data = existing
            .as_ref()
            .map(|row| row.data.clone())
            .unwrap_or_else(|| serde_json::json!({}));
        let sandbox = data
            .as_object_mut()
            .and_then(|root| {
                root.entry("agent_sandbox")
                    .or_insert_with(|| serde_json::json!({}))
                    .as_object_mut()
            })
            .ok_or_else(|| ConfigServiceError::InvalidConfiguration {
                details: "agent_sandbox is not a JSON object".into(),
            })?;
        let result = mutate(sandbox)?;
        if let Some(row) = existing {
            let mut active: settings::ActiveModel = row.into();
            active.data = Set(data);
            active.updated_at = Set(Utc::now());
            active.update(&transaction).await?;
        } else {
            settings::ActiveModel {
                id: Set(1),
                data: Set(data),
                created_at: Set(Utc::now()),
                updated_at: Set(Utc::now()),
            }
            .insert(&transaction)
            .await?;
        }
        transaction.commit().await?;
        self.invalidate_settings_cache().await;
        Ok(result)
    }

    /// Persist cluster CA material exactly once and return the material that
    /// owns the settings row after the transaction commits.
    ///
    /// Token minting and node registration can race during initial cluster
    /// setup. Locking the singleton settings row keeps every enrollment pinned
    /// to one trust root instead of allowing two callers to publish different
    /// CAs. A half-populated CA is never repaired implicitly because replacing
    /// either half could invalidate already-issued node identities.
    pub async fn initialize_cluster_ca_material(
        &self,
        generated_cert_pem: String,
        generated_key_encrypted: String,
    ) -> Result<(String, String), ConfigServiceError> {
        let transaction = self.db.begin().await?;
        let query = settings::Entity::find_by_id(1);
        let query = if self.is_sqlite() {
            query
        } else {
            query.lock_exclusive()
        };
        let existing = query.one(&transaction).await?;
        let mut current = existing
            .as_ref()
            .map(|model| AppSettings::from_json(model.data.clone()))
            .unwrap_or_default();

        let material = match (
            current.multi_node.cluster_ca_cert_pem.as_ref(),
            current.multi_node.cluster_ca_key_encrypted.as_ref(),
        ) {
            (Some(cert), Some(key)) => (cert.clone(), key.clone()),
            (None, None) => {
                current.multi_node.cluster_ca_cert_pem = Some(generated_cert_pem.clone());
                current.multi_node.cluster_ca_key_encrypted = Some(generated_key_encrypted.clone());
                (generated_cert_pem, generated_key_encrypted)
            }
            (Some(_), None) => {
                return Err(ConfigServiceError::InvalidConfiguration {
                    details: "cluster CA certificate exists but its encrypted private key is missing; refusing automatic replacement"
                        .to_string(),
                });
            }
            (None, Some(_)) => {
                return Err(ConfigServiceError::InvalidConfiguration {
                    details: "cluster CA encrypted private key exists but its certificate is missing; refusing automatic replacement"
                        .to_string(),
                });
            }
        };

        let needs_write = existing
            .as_ref()
            .map(|model| {
                let saved = AppSettings::from_json(model.data.clone());
                saved.multi_node.cluster_ca_cert_pem.is_none()
                    && saved.multi_node.cluster_ca_key_encrypted.is_none()
            })
            .unwrap_or(true);

        if needs_write {
            let now = Utc::now();
            if let Some(model) = existing {
                let merged = current.to_json_merged(&model.data);
                let mut active: settings::ActiveModel = model.into();
                active.data = Set(merged);
                active.updated_at = Set(now);
                active.update(&transaction).await?;
            } else {
                settings::ActiveModel {
                    id: Set(1),
                    data: Set(current.to_json()),
                    created_at: Set(now),
                    updated_at: Set(now),
                }
                .insert(&transaction)
                .await?;
            }
        }

        transaction.commit().await?;
        self.invalidate_settings_cache().await;
        Ok(material)
    }

    /// Replace a complete cluster CA pair and revoke every outstanding node
    /// enrollment token in the same transaction.
    ///
    /// The caller must provide the fingerprint it independently observed before
    /// starting recovery. This compare-and-swap guard prevents a stale browser
    /// tab or automation run from replacing a newer trust root. Existing worker
    /// certificates intentionally stop authenticating after this commits; every
    /// worker must be re-enrolled against the returned root.
    pub async fn rotate_cluster_ca_material(
        &self,
        expected_fingerprint: &str,
        replacement_cert_pem: String,
        replacement_key_encrypted: String,
    ) -> Result<ClusterCaRotationResult, ConfigServiceError> {
        let transaction = self.db.begin().await?;
        let query = settings::Entity::find_by_id(1);
        let query = if self.is_sqlite() {
            query
        } else {
            query.lock_exclusive()
        };
        let model = query
            .one(&transaction)
            .await?
            .ok_or(ConfigServiceError::ClusterCaNotInitialized)?;
        let mut current = AppSettings::from_json(model.data.clone());
        let current_cert = current
            .multi_node
            .cluster_ca_cert_pem
            .as_deref()
            .ok_or(ConfigServiceError::ClusterCaNotInitialized)?;
        if current.multi_node.cluster_ca_key_encrypted.is_none() {
            return Err(ConfigServiceError::InvalidConfiguration {
                details: "cluster CA certificate exists but its encrypted private key is missing"
                    .to_string(),
            });
        }

        let previous_fingerprint = temps_core::node_pki::ca_fingerprint_sha256(current_cert)
            .map_err(|error| ConfigServiceError::InvalidConfiguration {
                details: format!("active cluster CA certificate is invalid: {error}"),
            })?;
        if !previous_fingerprint.eq_ignore_ascii_case(expected_fingerprint.trim()) {
            return Err(ConfigServiceError::ClusterCaFingerprintMismatch);
        }

        current.multi_node.cluster_ca_cert_pem = Some(replacement_cert_pem);
        current.multi_node.cluster_ca_key_encrypted = Some(replacement_key_encrypted);
        let now = Utc::now();
        let merged = current.to_json_merged(&model.data);
        let mut active: settings::ActiveModel = model.into();
        active.data = Set(merged);
        active.updated_at = Set(now);
        active.update(&transaction).await?;

        let revoked = node_enrollment_tokens::Entity::update_many()
            .col_expr(
                node_enrollment_tokens::Column::RevokedAt,
                Expr::value(Some(now)),
            )
            .col_expr(node_enrollment_tokens::Column::UpdatedAt, Expr::value(now))
            .filter(node_enrollment_tokens::Column::RevokedAt.is_null())
            .exec(&transaction)
            .await?;

        transaction.commit().await?;
        self.invalidate_settings_cache().await;
        Ok(ClusterCaRotationResult {
            previous_fingerprint,
            revoked_enrollment_tokens: revoked.rows_affected,
        })
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
    use sea_orm::{DatabaseBackend, MockDatabase, Value};
    use std::collections::BTreeMap;

    struct FailingCryptoRng;

    impl rand::TryRng for FailingCryptoRng {
        type Error = std::io::Error;

        fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
            Err(std::io::Error::other("config entropy unavailable"))
        }

        fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
            Err(std::io::Error::other("config entropy unavailable"))
        }

        fn try_fill_bytes(&mut self, _dst: &mut [u8]) -> Result<(), Self::Error> {
            Err(std::io::Error::other("config entropy unavailable"))
        }
    }

    impl rand::TryCryptoRng for FailingCryptoRng {}

    #[test]
    fn randomness_failure_preserves_config_operation_context() {
        let error = fill_random_bytes_with(
            &mut FailingCryptoRng,
            "testing config entropy",
            &mut [0u8; 32],
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ConfigServiceError::RandomnessFailed { operation, reason }
                if operation == "testing config entropy"
                    && reason.contains("config entropy unavailable")
        ));
    }

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

    #[tokio::test]
    async fn agent_sandbox_getter_ignores_malformed_unrelated_settings() {
        let mut row = settings_row("example.test");
        let mut sandbox = AgentSandboxSettings::default();
        sandbox.providers.insert(
            "codex_cli".into(),
            temps_core::ProviderConfig {
                auth_type: "subscription".into(),
                credentials_encrypted: Some("encrypted-test-token".into()),
                ..Default::default()
            },
        );
        row.data["agent_sandbox"] = serde_json::to_value(sandbox).expect("sandbox JSON");
        row.data["preview_domain"] = serde_json::json!(false);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([[row]])
                .into_connection(),
        );
        let service = ConfigService::new(test_config(), db);

        let loaded = service
            .get_agent_sandbox_settings()
            .await
            .expect("valid scoped settings");

        assert_eq!(
            loaded
                .provider_config("codex_cli")
                .credentials_encrypted
                .as_deref(),
            Some("encrypted-test-token")
        );
    }

    #[tokio::test]
    async fn agent_sandbox_getter_rejects_a_malformed_present_section() {
        let mut row = settings_row("example.test");
        row.data["agent_sandbox"] = serde_json::json!("malformed-secret-must-not-leak");
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([[row]])
                .into_connection(),
        );
        let service = ConfigService::new(test_config(), db);

        let error = service
            .get_agent_sandbox_settings()
            .await
            .expect_err("malformed section");

        assert!(matches!(
            error,
            ConfigServiceError::MalformedSettingsSection {
                section: "agent_sandbox"
            }
        ));
        assert!(!error.to_string().contains("malformed-secret"));
    }

    #[tokio::test]
    async fn agent_sandbox_getter_defaults_only_when_row_or_section_is_absent() {
        let row_without_section = settings::Model {
            id: 1,
            data: serde_json::json!({"unrelated": true}),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        for rows in [vec![], vec![row_without_section]] {
            let db = Arc::new(
                MockDatabase::new(DatabaseBackend::Postgres)
                    .append_query_results([rows])
                    .into_connection(),
            );
            let service = ConfigService::new(test_config(), db);
            let loaded = service
                .get_agent_sandbox_settings()
                .await
                .expect("absent section default");
            assert!(loaded.providers.is_empty());
            assert_eq!(
                loaded.default_provider,
                AgentSandboxSettings::default().default_provider
            );
        }
    }

    #[test]
    fn locked_provider_rebase_rejects_stale_and_forged_credentials() {
        let mut locked = AppSettings::default();
        locked.agent_sandbox.providers.insert(
            "replaced".into(),
            temps_core::ProviderConfig {
                auth_type: "subscription".into(),
                credentials_encrypted: Some("new-ciphertext".into()),
                extra: serde_json::json!({"credential_verified": true}),
                ..Default::default()
            },
        );
        locked.agent_sandbox.providers.insert(
            "omitted".into(),
            temps_core::ProviderConfig {
                auth_type: "api_key".into(),
                credentials_encrypted: Some("omitted-ciphertext".into()),
                extra: serde_json::json!({"credential_verified": true}),
                ..Default::default()
            },
        );
        let mut incoming = AppSettings::default();
        incoming.agent_sandbox.providers.insert(
            "replaced".into(),
            temps_core::ProviderConfig {
                auth_type: "api_key".into(),
                credentials_encrypted: Some("old-ciphertext".into()),
                extra: serde_json::json!({"credential_verified": false, "custom": 1}),
                ..Default::default()
            },
        );
        incoming.agent_sandbox.providers.insert(
            "new".into(),
            temps_core::ProviderConfig {
                credentials_encrypted: Some("forged-ciphertext".into()),
                extra: serde_json::json!({"credential_verified": true}),
                ..Default::default()
            },
        );
        preserve_provider_credential_proof(&mut incoming, &locked);
        let replaced = &incoming.agent_sandbox.providers["replaced"];
        assert_eq!(replaced.auth_type, "subscription");
        assert_eq!(
            replaced.credentials_encrypted.as_deref(),
            Some("new-ciphertext")
        );
        assert_eq!(replaced.extra["credential_verified"], true);
        assert_eq!(replaced.extra["custom"], 1);
        assert_eq!(
            incoming.agent_sandbox.providers["omitted"]
                .credentials_encrypted
                .as_deref(),
            Some("omitted-ciphertext")
        );
        let new_provider = &incoming.agent_sandbox.providers["new"];
        assert_eq!(new_provider.credentials_encrypted, None);
        assert!(new_provider.extra.get("credential_verified").is_none());

        // A credential deleted by the dedicated endpoint must not be restored
        // by a bulk payload prepared before that deletion.
        let mut deleted = locked.clone();
        deleted.agent_sandbox.providers.insert(
            "replaced".into(),
            temps_core::ProviderConfig {
                auth_type: "api_key".into(),
                credentials_encrypted: None,
                extra: serde_json::json!({}),
                ..Default::default()
            },
        );
        let mut stale = incoming;
        stale
            .agent_sandbox
            .providers
            .get_mut("replaced")
            .expect("provider")
            .credentials_encrypted = Some("old-ciphertext".into());
        preserve_provider_credential_proof(&mut stale, &deleted);
        let after_deletion = &stale.agent_sandbox.providers["replaced"];
        assert_eq!(after_deletion.credentials_encrypted, None);
        assert!(after_deletion.extra.get("credential_verified").is_none());
    }

    #[tokio::test]
    async fn bulk_update_uses_locked_credential_and_publishes_rebased_cache() {
        let mut locked = settings_row("old.example.com");
        let mut locked_settings = AppSettings::from_json(locked.data.clone());
        locked_settings.plugin_installation_reporting_enabled = true;
        locked_settings.agent_sandbox.providers.insert(
            "codex_cli".into(),
            temps_core::ProviderConfig {
                auth_type: "subscription".into(),
                credentials_encrypted: Some("replacement".into()),
                extra: serde_json::json!({"credential_verified": true}),
                ..Default::default()
            },
        );
        locked.data = locked_settings.to_json();
        let db = MockDatabase::new(DatabaseBackend::Sqlite)
            .append_query_results(vec![
                vec![locked.clone()],
                vec![locked.clone()],
                vec![locked.clone()],
                vec![locked],
            ])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 1,
                rows_affected: 1,
            }])
            .into_connection();
        let db = Arc::new(db);
        let svc = ConfigService::new(test_config(), db.clone());
        let mut incoming = AppSettings {
            preview_domain: "new.example.com".into(),
            ..AppSettings::default()
        };
        incoming.agent_sandbox.providers.insert(
            "codex_cli".into(),
            temps_core::ProviderConfig {
                auth_type: "api_key".into(),
                credentials_encrypted: Some("stale".into()),
                extra: serde_json::json!({"credential_verified": false}),
                ..Default::default()
            },
        );
        svc.update_settings(incoming).await.expect("bulk update");
        let cached = svc.get_settings().await.expect("rebased cache");
        assert_eq!(cached.preview_domain, "new.example.com");
        assert!(cached.plugin_installation_reporting_enabled);
        let provider = &cached.agent_sandbox.providers["codex_cli"];
        assert_eq!(provider.auth_type, "subscription");
        assert_eq!(
            provider.credentials_encrypted.as_deref(),
            Some("replacement")
        );
        assert_eq!(provider.extra["credential_verified"], true);
        drop(svc);
        let statements = Arc::try_unwrap(db)
            .expect("test should release database connection")
            .into_transaction_log();
        let update_sql = statements
            .iter()
            .flat_map(|transaction| transaction.statements())
            .map(ToString::to_string)
            .find(|sql| sql.starts_with("UPDATE "))
            .expect("settings update statement");
        assert!(update_sql.contains("replacement"), "{update_sql}");
        assert!(
            update_sql.contains("plugin_installation_reporting_enabled"),
            "{update_sql}"
        );
        // Matched as a quoted JSON *value* so the assertion stays about the
        // forged ciphertext: unrelated field names legitimately contain
        // "stale" as a substring (e.g. `geo.stale_lookup_days`).
        assert!(!update_sql.contains(r#""stale""#), "{update_sql}");
    }

    /// The race the geo section actually has two writers for: the refresh job
    /// records a check through `update_geo_settings` while an admin's settings
    /// PUT, built from the 5s-cached snapshot, is already in flight. The PUT
    /// touched nothing geo-related, so neither the job's metadata nor the
    /// stored license key may come back to the pre-race values.
    #[tokio::test]
    async fn settings_save_cannot_clobber_the_refresh_jobs_recorded_geo_state() {
        // What both writers started from: no metadata, no key.
        let stale_snapshot = AppSettings::from_json(settings_row("example.test").data);
        assert_eq!(stale_snapshot.geo, temps_core::GeoSettings::default());

        // The job (and a concurrent key save) committed first, so this is what
        // the row holds by the time the PUT takes the lock.
        let checked_at = Utc::now();
        let mut locked = settings_row("example.test");
        let mut locked_settings = AppSettings::from_json(locked.data.clone());
        locked_settings.geo.source = Some(temps_core::GEO_SOURCE_MAXMIND_OFFICIAL.to_string());
        locked_settings.geo.build_epoch = Some(1_767_225_600);
        locked_settings.geo.last_refreshed_at = Some(checked_at);
        locked_settings.geo.last_check_at = Some(checked_at);
        locked_settings.geo.last_check_status = Some(temps_core::GEO_CHECK_STATUS_OK.to_string());
        locked_settings.geo.maxmind_license_key_encrypted =
            Some("just-saved-ciphertext".to_string());
        locked.data = locked_settings.to_json();

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Sqlite)
                .append_query_results(vec![
                    vec![locked.clone()],
                    vec![locked.clone()],
                    vec![locked.clone()],
                    vec![locked.clone()],
                ])
                .append_exec_results([sea_orm::MockExecResult {
                    last_insert_id: 1,
                    rows_affected: 1,
                }])
                .into_connection(),
        );
        let svc = ConfigService::new(test_config(), db.clone());

        // The admin's payload: an unrelated field changed, geo carried
        // forward verbatim out of the stale snapshot.
        let mut incoming = stale_snapshot.clone();
        incoming.preview_domain = "new.example.test".into();
        incoming.geo.last_check_status = Some(temps_core::GEO_CHECK_STATUS_ERROR.to_string());
        incoming.geo.last_error = Some("a failure the client invented".to_string());

        svc.update_settings(incoming).await.expect("settings save");

        let cached = svc.get_settings().await.expect("rebased settings");
        assert_eq!(cached.preview_domain, "new.example.test");
        assert_eq!(
            cached.geo.source.as_deref(),
            Some(temps_core::GEO_SOURCE_MAXMIND_OFFICIAL)
        );
        assert_eq!(cached.geo.build_epoch, Some(1_767_225_600));
        assert_eq!(
            cached.geo.last_check_status.as_deref(),
            Some(temps_core::GEO_CHECK_STATUS_OK)
        );
        assert_eq!(cached.geo.last_error, None);
        assert!(cached.geo.last_check_at.is_some());
        assert_eq!(
            cached.geo.maxmind_license_key_encrypted.as_deref(),
            Some("just-saved-ciphertext"),
            "a save that never touched the key must not revert it"
        );

        drop(svc);
        let statements = Arc::try_unwrap(db)
            .expect("test should release database connection")
            .into_transaction_log();
        let update_sql = statements
            .iter()
            .flat_map(|transaction| transaction.statements())
            .map(ToString::to_string)
            .find(|sql| sql.starts_with("UPDATE "))
            .expect("settings update statement");
        assert!(update_sql.contains("just-saved-ciphertext"), "{update_sql}");
        assert!(
            !update_sql.contains("a failure the client invented"),
            "{update_sql}"
        );
    }

    /// The counterpart: a request that *did* submit a key must still store it,
    /// otherwise the rebase above would make the field unwritable.
    #[tokio::test]
    async fn settings_save_that_submitted_a_license_key_replaces_the_stored_one() {
        let mut locked = settings_row("example.test");
        let mut locked_settings = AppSettings::from_json(locked.data.clone());
        locked_settings.geo.maxmind_license_key_encrypted = Some("previous".to_string());
        locked.data = locked_settings.to_json();

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Sqlite)
                .append_query_results(vec![
                    vec![locked.clone()],
                    vec![locked.clone()],
                    vec![locked.clone()],
                    vec![locked.clone()],
                ])
                .append_exec_results([sea_orm::MockExecResult {
                    last_insert_id: 1,
                    rows_affected: 1,
                }])
                .into_connection(),
        );
        let svc = ConfigService::new(test_config(), db.clone());

        // What the handler produces for a submitted key: ciphertext in place
        // and the intent declared explicitly.
        let mut incoming = AppSettings::from_json(settings_row("example.test").data);
        incoming.geo.maxmind_license_key_encrypted = Some("rotated-ciphertext".to_string());
        svc.update_settings_with_geo_intent(incoming, GeoLicenseKeyIntent::Set)
            .await
            .expect("settings save");

        assert_eq!(
            svc.get_settings()
                .await
                .expect("settings")
                .geo
                .maxmind_license_key_encrypted
                .as_deref(),
            Some("rotated-ciphertext")
        );

        // And clearing it is honoured rather than treated as an omission.
        let mut cleared = AppSettings::from_json(locked.data.clone());
        cleared.geo.maxmind_license_key_encrypted = None;
        preserve_geo_recorded_state(
            &mut cleared,
            &AppSettings::from_json(locked.data),
            GeoLicenseKeyIntent::Cleared,
        );
        assert_eq!(cleared.geo.maxmind_license_key_encrypted, None);
    }

    /// The refresh job's write is unattended and fires on a timer, so it must
    /// touch nothing but `geo` — a document it cannot fully parse must never
    /// become `AppSettings::default()` (which would silently reset MFA
    /// requirements, security headers, rate limits and IP trust).
    #[tokio::test]
    async fn geo_update_rewrites_only_the_geo_key_of_the_settings_document() {
        let mut settings = AppSettings {
            preview_domain: "keep.example.test".into(),
            require_mfa_for_admins: true,
            ..AppSettings::default()
        };
        settings.rate_limiting.enabled = true;
        let mut document = settings.to_json();
        // A sub-document `AppSettings` does not own, plus a key it does own
        // but whose stored shape it could not deserialize.
        if let Some(object) = document.as_object_mut() {
            object.insert(
                "admin_gate".to_string(),
                serde_json::json!({"allowed_ips": ["203.0.113.7"]}),
            );
            object.insert(
                "security_headers".to_string(),
                serde_json::json!("not the shape AppSettings expects"),
            );
        }
        let row = settings::Model {
            id: 1,
            data: document,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Sqlite)
                // Locked read, then the row Sea-ORM re-selects after UPDATE.
                .append_query_results(vec![vec![row.clone()], vec![row]])
                .append_exec_results([sea_orm::MockExecResult {
                    last_insert_id: 1,
                    rows_affected: 1,
                }])
                .into_connection(),
        );
        let svc = ConfigService::new(test_config(), db.clone());

        let updated = svc
            .update_geo_settings(|geo| {
                geo.last_check_status = Some(temps_core::GEO_CHECK_STATUS_OK.to_string());
                geo.build_epoch = Some(1_767_225_600);
            })
            .await
            .expect("record the geo check");
        assert_eq!(
            updated.last_check_status.as_deref(),
            Some(temps_core::GEO_CHECK_STATUS_OK)
        );

        drop(svc);
        let statements = Arc::try_unwrap(db)
            .expect("test should release database connection")
            .into_transaction_log();
        let update_sql = statements
            .iter()
            .flat_map(|transaction| transaction.statements())
            .map(ToString::to_string)
            .find(|sql| sql.starts_with("UPDATE "))
            .expect("geo update statement");
        assert!(update_sql.contains("keep.example.test"), "{update_sql}");
        assert!(update_sql.contains("203.0.113.7"), "{update_sql}");
        assert!(
            update_sql.contains("not the shape AppSettings expects"),
            "an unparsable unrelated key must survive byte-for-byte: {update_sql}"
        );
        assert!(update_sql.contains("1767225600"), "{update_sql}");
    }

    /// A `geo` section that will not deserialize is reported instead of being
    /// overwritten with defaults, which would discard the stored license key.
    #[tokio::test]
    async fn geo_update_refuses_to_overwrite_a_malformed_geo_section() {
        let mut document = AppSettings::default().to_json();
        if let Some(object) = document.as_object_mut() {
            object.insert(
                GEO_SETTINGS_KEY.to_string(),
                serde_json::json!("not an object"),
            );
        }
        let row = settings::Model {
            id: 1,
            data: document,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let db = MockDatabase::new(DatabaseBackend::Sqlite)
            .append_query_results(vec![vec![row]])
            .into_connection();
        let svc = ConfigService::new(test_config(), Arc::new(db));

        let error = svc
            .update_geo_settings(|geo| geo.build_epoch = Some(1))
            .await
            .expect_err("a malformed section must be reported, not reset");
        assert!(matches!(
            error,
            ConfigServiceError::MalformedSettingsSection { section: "geo" }
        ));
    }

    #[tokio::test]
    async fn late_bulk_cache_publish_cannot_restore_snapshot_after_credential_invalidation() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![settings_row("authoritative.example.com")]])
            .into_connection();
        let svc = ConfigService::new(test_config(), Arc::new(db));
        let started_generation = svc.settings_cache.read().await.generation;
        svc.invalidate_settings_cache().await;
        let stale = AppSettings {
            preview_domain: "stale.example.com".into(),
            ..AppSettings::default()
        };
        assert!(
            !svc.publish_committed_settings_if_current(started_generation, stale)
                .await
        );
        assert_eq!(
            svc.get_settings()
                .await
                .expect("authoritative cache")
                .preview_domain,
            "authoritative.example.com"
        );
    }

    fn settings_row_with_cluster_ca(
        cert_pem: Option<&str>,
        encrypted_key: Option<&str>,
    ) -> settings::Model {
        let mut app_settings = AppSettings::default();
        app_settings.multi_node.cluster_ca_cert_pem = cert_pem.map(ToString::to_string);
        app_settings.multi_node.cluster_ca_key_encrypted = encrypted_key.map(ToString::to_string);
        settings::Model {
            id: 1,
            data: app_settings.to_json(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn initialize_cluster_ca_material_persists_first_complete_pair() {
        let empty = settings_row_with_cluster_ca(None, None);
        let persisted = settings_row_with_cluster_ca(Some("generated-cert"), Some("generated-key"));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[empty], [persisted]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 1,
                rows_affected: 1,
            }])
            .into_connection();
        let service = ConfigService::new(test_config(), Arc::new(db));

        let material = service
            .initialize_cluster_ca_material(
                "generated-cert".to_string(),
                "generated-key".to_string(),
            )
            .await
            .expect("first complete cluster CA pair should be persisted");

        assert_eq!(material.0, "generated-cert");
        assert_eq!(material.1, "generated-key");
    }

    #[tokio::test]
    async fn initialize_cluster_ca_material_reuses_existing_pair() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[settings_row_with_cluster_ca(
                Some("existing-cert"),
                Some("existing-key"),
            )]])
            .into_connection();
        let service = ConfigService::new(test_config(), Arc::new(db));

        let material = service
            .initialize_cluster_ca_material("racing-cert".to_string(), "racing-key".to_string())
            .await
            .expect("existing cluster CA should win initialization race");

        assert_eq!(material.0, "existing-cert");
        assert_eq!(material.1, "existing-key");
    }

    #[tokio::test]
    async fn initialize_cluster_ca_material_rejects_partial_state() {
        for row in [
            settings_row_with_cluster_ca(Some("orphan-cert"), None),
            settings_row_with_cluster_ca(None, Some("orphan-key")),
        ] {
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([[row]])
                .into_connection();
            let service = ConfigService::new(test_config(), Arc::new(db));

            let error = service
                .initialize_cluster_ca_material(
                    "generated-cert".to_string(),
                    "generated-key".to_string(),
                )
                .await
                .expect_err("partial CA state must require operator recovery");

            assert!(matches!(
                error,
                ConfigServiceError::InvalidConfiguration { details }
                    if details.contains("refusing automatic replacement")
            ));
        }
    }

    #[tokio::test]
    async fn rotate_cluster_ca_material_replaces_pair_and_revokes_tokens() {
        let existing = temps_core::node_pki::generate_cluster_ca()
            .expect("test cluster CA generation should succeed");
        let expected = temps_core::node_pki::ca_fingerprint_sha256(&existing.cert_pem)
            .expect("test cluster CA fingerprint should succeed");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                [settings_row_with_cluster_ca(
                    Some(&existing.cert_pem),
                    Some("existing-encrypted-key"),
                )],
                [settings_row_with_cluster_ca(
                    Some("replacement-cert"),
                    Some("replacement-encrypted-key"),
                )],
            ])
            .append_exec_results([
                sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 3,
                },
                sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 3,
                },
            ])
            .into_connection();
        let service = ConfigService::new(test_config(), Arc::new(db));

        let result = service
            .rotate_cluster_ca_material(
                &expected,
                "replacement-cert".to_string(),
                "replacement-encrypted-key".to_string(),
            )
            .await
            .expect("matching expected root should rotate atomically");

        assert_eq!(result.previous_fingerprint, expected);
        assert_eq!(result.revoked_enrollment_tokens, 3);
    }

    #[tokio::test]
    async fn rotate_cluster_ca_material_rejects_stale_fingerprint() {
        let existing = temps_core::node_pki::generate_cluster_ca()
            .expect("test cluster CA generation should succeed");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[settings_row_with_cluster_ca(
                Some(&existing.cert_pem),
                Some("existing-encrypted-key"),
            )]])
            .into_connection();
        let service = ConfigService::new(test_config(), Arc::new(db));

        let error = service
            .rotate_cluster_ca_material(
                "stale-fingerprint",
                "replacement-cert".to_string(),
                "replacement-encrypted-key".to_string(),
            )
            .await
            .expect_err("stale operator state must not replace the active root");

        assert!(matches!(
            error,
            ConfigServiceError::ClusterCaFingerprintMismatch
        ));
    }

    fn timescale_policy_row(
        hypertable_name: &str,
        proc_name: &str,
        interval_seconds: i64,
    ) -> BTreeMap<String, Value> {
        BTreeMap::from([
            (
                "hypertable_name".to_string(),
                Value::String(Some(Box::new(hypertable_name.to_string()))),
            ),
            (
                "proc_name".to_string(),
                Value::String(Some(Box::new(proc_name.to_string()))),
            ),
            (
                "interval_seconds".to_string(),
                Value::BigInt(Some(interval_seconds)),
            ),
        ])
    }

    fn count_row(count: i64) -> BTreeMap<String, Value> {
        BTreeMap::from([("num_items".to_string(), Value::BigInt(Some(count)))])
    }

    fn network_config_row(
        subnet_prefix_len: i32,
        control_plane_compute_cidr: Option<&str>,
    ) -> network_config::Model {
        network_config::Model {
            id: 1,
            compute_pool_cidr: "10.240.0.0/16".to_string(),
            subnet_prefix_len,
            transport: "vxlan".to_string(),
            vxlan_vni: 42,
            vxlan_port: 4789,
            underlay_mtu: 1500,
            control_plane_compute_cidr: control_plane_compute_cidr.map(str::to_string),
            control_plane_underlay_address: Some("10.200.4.1".to_string()),
            control_plane_overlay_ready: control_plane_compute_cidr.is_some(),
            control_plane_setup_generation: 1,
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn cluster_network_state_counts_control_plane_and_worker_allocations() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([[network_config_row(24, Some("10.240.0.0/24"))]])
                .append_query_results([[count_row(2)]])
                .into_connection(),
        );
        let svc = ConfigService::new(test_config(), db);

        let state = svc
            .get_cluster_network_state()
            .await
            .expect("valid cluster network state should load");

        assert_eq!(state.compute_pool_cidr, "10.240.0.0/16");
        assert_eq!(state.subnet_prefix_len, 24);
        assert_eq!(state.allocation_count, 3);
    }

    #[tokio::test]
    async fn cluster_network_state_requires_the_singleton_row() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<network_config::Model>::new()])
            .into_connection();
        let svc = ConfigService::new(test_config(), Arc::new(db));

        let error = svc
            .get_cluster_network_state()
            .await
            .expect_err("a missing singleton must be reported");

        assert!(matches!(
            error,
            ConfigServiceError::InvalidConfiguration { details }
                if details.contains("singleton row is missing")
        ));
    }

    #[tokio::test]
    async fn cluster_network_state_rejects_an_invalid_prefix() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[network_config_row(300, None)]])
            .into_connection();
        let svc = ConfigService::new(test_config(), Arc::new(db));

        let error = svc
            .get_cluster_network_state()
            .await
            .expect_err("an out-of-range prefix must be rejected");

        assert!(matches!(
            error,
            ConfigServiceError::InvalidConfiguration { details }
                if details.contains("outside the supported u8 range")
        ));
    }

    #[tokio::test]
    async fn cluster_network_state_propagates_database_errors() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom(
                "network configuration unavailable".to_string(),
            )])
            .into_connection();
        let svc = ConfigService::new(test_config(), Arc::new(db));

        let error = svc
            .get_cluster_network_state()
            .await
            .expect_err("database errors must remain typed");

        assert!(matches!(error, ConfigServiceError::Database(_)));
    }

    #[test]
    fn compression_policy_sql_uses_an_integer_interval_and_idempotent_replace() {
        let sql = compression_policy_sql("proxy_logs", 24);
        assert!(sql.contains("remove_compression_policy('proxy_logs', if_exists => TRUE)"));
        assert!(sql.contains("make_interval(hours => 24)"));
        assert!(sql.contains("if_not_exists => TRUE"));
    }

    #[test]
    fn retention_policy_sql_uses_an_integer_interval_and_idempotent_replace() {
        let sql = retention_policy_sql("otel_spans", 90);
        assert!(sql.contains("remove_retention_policy('otel_spans', if_exists => TRUE)"));
        assert!(sql.contains("make_interval(days => 90)"));
        assert!(sql.contains("if_not_exists => TRUE"));
    }

    #[test]
    fn timescale_policy_rows_map_to_ui_units() {
        let rows = vec![
            TimescalePolicyRow {
                hypertable_name: "service_metrics".into(),
                proc_name: "policy_retention".into(),
                interval_seconds: 14 * SECONDS_PER_DAY,
            },
            TimescalePolicyRow {
                hypertable_name: "service_metrics_hourly".into(),
                proc_name: "policy_retention".into(),
                interval_seconds: 90 * SECONDS_PER_DAY,
            },
            TimescalePolicyRow {
                hypertable_name: "service_metrics_daily".into(),
                proc_name: "policy_retention".into(),
                interval_seconds: 730 * SECONDS_PER_DAY,
            },
            TimescalePolicyRow {
                hypertable_name: "proxy_logs".into(),
                proc_name: "policy_compression".into(),
                interval_seconds: 24 * SECONDS_PER_HOUR,
            },
            TimescalePolicyRow {
                hypertable_name: "otel_spans".into(),
                proc_name: "policy_compression".into(),
                interval_seconds: 12 * SECONDS_PER_HOUR,
            },
            TimescalePolicyRow {
                hypertable_name: "proxy_logs".into(),
                proc_name: "policy_retention".into(),
                interval_seconds: 30 * SECONDS_PER_DAY,
            },
            TimescalePolicyRow {
                hypertable_name: "otel_spans".into(),
                proc_name: "policy_retention".into(),
                interval_seconds: 60 * SECONDS_PER_DAY,
            },
            TimescalePolicyRow {
                hypertable_name: "otel_log_events".into(),
                proc_name: "policy_retention".into(),
                interval_seconds: 45 * SECONDS_PER_DAY,
            },
            TimescalePolicyRow {
                hypertable_name: "otel_metrics".into(),
                proc_name: "policy_retention".into(),
                interval_seconds: 90 * SECONDS_PER_DAY,
            },
        ];

        assert_eq!(
            policies_from_rows(rows),
            EffectiveTelemetryPolicies {
                metrics_raw_days: Some(14),
                metrics_hourly_days: Some(90),
                metrics_daily_years: Some(2),
                proxy_logs_compression_hours: Some(24),
                otel_spans_compression_hours: Some(12),
                proxy_logs_retention_days: Some(30),
                otel_spans_retention_days: Some(60),
                otel_logs_retention_days: Some(45),
                otel_metrics_retention_days: Some(90),
            }
        );
    }

    #[test]
    fn timescale_policy_rows_ignore_unknown_or_non_positive_intervals() {
        let rows = vec![
            TimescalePolicyRow {
                hypertable_name: "proxy_logs".into(),
                proc_name: "policy_compression".into(),
                interval_seconds: 0,
            },
            TimescalePolicyRow {
                hypertable_name: "unknown_table".into(),
                proc_name: "policy_retention".into(),
                interval_seconds: 30 * SECONDS_PER_DAY,
            },
        ];

        assert_eq!(
            policies_from_rows(rows),
            EffectiveTelemetryPolicies::default()
        );
    }

    #[test]
    fn timescale_policy_rows_do_not_round_partial_years() {
        let policies = policies_from_rows(vec![TimescalePolicyRow {
            hypertable_name: "service_metrics_daily".into(),
            proc_name: "policy_retention".into(),
            interval_seconds: 500 * SECONDS_PER_DAY,
        }]);

        assert_eq!(policies.metrics_daily_years, None);
    }

    #[tokio::test]
    async fn effective_policy_query_reads_timescale_metadata_without_telemetry_scan() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([[
                    timescale_policy_row("proxy_logs", "policy_compression", 24 * SECONDS_PER_HOUR),
                    timescale_policy_row("otel_metrics", "policy_retention", 90 * SECONDS_PER_DAY),
                ]])
                .into_connection(),
        );
        let svc = ConfigService::new(test_config(), db.clone());

        let policies = svc
            .get_effective_telemetry_policies()
            .await
            .expect("Timescale metadata query should map active jobs");

        assert_eq!(policies.proxy_logs_compression_hours, Some(24));
        assert_eq!(policies.otel_metrics_retention_days, Some(90));
        drop(svc);
        let statements = Arc::try_unwrap(db)
            .expect("test should release database connection")
            .into_transaction_log();
        let sql = statements[0].statements()[0].to_string();
        assert!(sql.contains("timescaledb_information.jobs"));
        assert!(sql.contains("hypertable_schema = current_schema()"));
        assert!(!sql.contains("FROM proxy_logs"));
        assert!(!sql.contains("FROM otel_metrics"));
    }

    #[tokio::test]
    async fn monitored_service_count_matches_scraper_predicates() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([[count_row(4)]])
                .into_connection(),
        );
        let svc = ConfigService::new(test_config(), db.clone());

        assert_eq!(svc.count_monitored_services().await.unwrap(), 4);
        drop(svc);
        let statements = Arc::try_unwrap(db)
            .expect("test should release database connection")
            .into_transaction_log();
        let sql = statements[0].statements()[0].to_string();
        assert!(sql.contains("metrics_enabled"));
        assert!(sql.contains("status"));
        assert!(sql.contains("running"));
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

    #[tokio::test]
    async fn update_settings_applies_changed_observability_compression_policies() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![
                vec![settings_row("logs.example.com")],
                vec![settings_row("logs.example.com")],
                vec![settings_row("logs.example.com")],
            ])
            // proxy policy, span policy, settings row update
            .append_exec_results(vec![
                sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                },
                sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                },
                sea_orm::MockExecResult {
                    last_insert_id: 1,
                    rows_affected: 1,
                },
            ])
            .into_connection();
        let svc = ConfigService::new(test_config(), Arc::new(db));
        let mut updated = AppSettings::default();
        updated.observability_compression.proxy_logs_after_hours = 12;
        updated.observability_compression.otel_spans_after_hours = 48;

        svc.update_settings(updated.clone())
            .await
            .expect("changed compression policies should be applied transactionally");

        let cached = svc.get_settings().await.expect("updated settings cache");
        assert_eq!(
            cached.observability_compression,
            updated.observability_compression
        );
    }

    #[tokio::test]
    async fn update_settings_recreates_a_missing_live_policy_even_when_json_matches() {
        let current = AppSettings::default();
        let live_rows = vec![
            timescale_policy_row(
                "service_metrics",
                "policy_retention",
                i64::from(current.monitoring.retention_raw_days) * SECONDS_PER_DAY,
            ),
            timescale_policy_row(
                "service_metrics_hourly",
                "policy_retention",
                i64::from(current.monitoring.retention_hourly_days) * SECONDS_PER_DAY,
            ),
            timescale_policy_row(
                "service_metrics_daily",
                "policy_retention",
                i64::from(current.monitoring.retention_daily_years) * 365 * SECONDS_PER_DAY,
            ),
            // proxy_logs compression is intentionally missing.
            timescale_policy_row(
                "otel_spans",
                "policy_compression",
                i64::from(current.observability_compression.otel_spans_after_hours)
                    * SECONDS_PER_HOUR,
            ),
            timescale_policy_row(
                "proxy_logs",
                "policy_retention",
                i64::from(current.observability_retention.proxy_logs_days) * SECONDS_PER_DAY,
            ),
            timescale_policy_row(
                "otel_spans",
                "policy_retention",
                i64::from(current.observability_retention.otel_spans_days) * SECONDS_PER_DAY,
            ),
            timescale_policy_row(
                "otel_log_events",
                "policy_retention",
                i64::from(current.observability_retention.otel_logs_days) * SECONDS_PER_DAY,
            ),
            timescale_policy_row(
                "otel_metrics",
                "policy_retention",
                i64::from(current.observability_retention.otel_metrics_days) * SECONDS_PER_DAY,
            ),
        ];
        let row = settings_row("localhost");
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([live_rows])
                .append_query_results([[row.clone()], [row]])
                .append_exec_results([
                    sea_orm::MockExecResult {
                        last_insert_id: 0,
                        rows_affected: 1,
                    },
                    sea_orm::MockExecResult {
                        last_insert_id: 1,
                        rows_affected: 1,
                    },
                ])
                .into_connection(),
        );
        let svc = ConfigService::new(test_config(), db.clone());

        svc.update_settings(current)
            .await
            .expect("missing live policy should be recreated");

        drop(svc);
        let statements = Arc::try_unwrap(db)
            .expect("test should release database connection")
            .into_transaction_log();
        let sql = statements
            .iter()
            .flat_map(|transaction| transaction.statements())
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(sql.contains("add_compression_policy"));
        assert!(sql.contains("proxy_logs"));
    }

    #[tokio::test]
    async fn compression_policy_errors_include_table_and_requested_delay() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_errors(vec![sea_orm::DbErr::Custom("Timescale job error".into())])
            .into_connection();

        let error = replace_compression_policy(&db, "otel_spans", 12)
            .await
            .expect_err("policy error should be returned");

        assert!(matches!(
            error,
            ConfigServiceError::CompressionPolicyUpdate {
                table: "otel_spans",
                after_hours: 12,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn update_settings_applies_changed_observability_retention_policies() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![
                vec![settings_row("retention.example.com")],
                vec![settings_row("retention.example.com")],
                vec![settings_row("retention.example.com")],
            ])
            // Four telemetry retention policies, then the settings row update.
            .append_exec_results(vec![
                sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                };
                5
            ])
            .into_connection();
        let svc = ConfigService::new(test_config(), Arc::new(db));
        let mut updated = AppSettings::default();
        updated.observability_retention.proxy_logs_days = 14;
        updated.observability_retention.otel_spans_days = 60;
        updated.observability_retention.otel_logs_days = 45;
        updated.observability_retention.otel_metrics_days = 30;

        svc.update_settings(updated.clone())
            .await
            .expect("changed retention policies should be applied transactionally");

        let cached = svc.get_settings().await.expect("updated settings cache");
        assert_eq!(
            cached.observability_retention,
            updated.observability_retention
        );
    }

    #[tokio::test]
    async fn retention_policy_errors_include_table_and_requested_window() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_errors(vec![sea_orm::DbErr::Custom("Timescale job error".into())])
            .into_connection();

        let error = replace_retention_policy(&db, "otel_log_events", 45)
            .await
            .expect_err("policy error should be returned");

        assert!(matches!(
            error,
            ConfigServiceError::RetentionPolicyUpdate {
                table: "otel_log_events",
                after_days: 45,
                ..
            }
        ));
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

    #[tokio::test]
    async fn invalidation_rejects_a_stale_in_flight_cache_publication() {
        temps_core::tls::set_insecure_tls(false);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![settings_row("new.example.com")]])
            .into_connection();
        let svc = ConfigService::new(test_config(), Arc::new(db));
        let stale_generation = svc.settings_cache.read().await.generation;

        svc.invalidate_settings_cache().await;

        let stale = AppSettings {
            preview_domain: "old.example.com".to_string(),
            insecure_tls: true,
            ..AppSettings::default()
        };
        assert!(!svc.cache_settings_if_current(stale_generation, stale).await);
        assert!(
            !temps_core::tls::insecure_tls_enabled(),
            "rejected stale settings must not mutate process-wide TLS behavior"
        );
        assert_eq!(
            svc.get_settings().await.unwrap().preview_domain,
            "new.example.com",
            "a read started before invalidation must not republish stale settings"
        );
        temps_core::tls::set_insecure_tls(false);
    }

    #[tokio::test]
    async fn invalidation_serializes_cache_and_tls_publication() {
        temps_core::tls::set_insecure_tls(true);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![settings_row("strict.example.com")]])
            .into_connection();
        let svc = Arc::new(ConfigService::new(test_config(), Arc::new(db)));

        // Hold the cache-publication lock while invalidation starts. The
        // invalidator must not advance the generation or mutate TLS outside
        // that lock; this is the check-to-publication interleaving that used to
        // permit a stale insecure-TLS value to survive invalidation.
        let cache_guard = svc.settings_cache.write().await;
        let invalidation = tokio::spawn({
            let svc = svc.clone();
            async move { svc.invalidate_settings_cache().await }
        });
        tokio::task::yield_now().await;
        assert!(
            !invalidation.is_finished(),
            "invalidation must wait for the cache/TLS publication lock"
        );
        drop(cache_guard);

        invalidation.await.expect("invalidation task");
        assert!(
            !temps_core::tls::insecure_tls_enabled(),
            "invalidation must finish with the reloaded strict TLS setting"
        );
        assert_eq!(
            svc.get_settings().await.unwrap().preview_domain,
            "strict.example.com"
        );
        temps_core::tls::set_insecure_tls(false);
    }
}
