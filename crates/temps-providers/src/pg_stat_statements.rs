//! Service for querying `pg_stat_statements` on a user-provisioned Postgres
//! service.
//!
//! # Enabling `pg_stat_statements`
//!
//! The extension requires `shared_preload_libraries` to include
//! `pg_stat_statements`. For services provisioned after this change was
//! deployed the lifecycle creates the container with that flag already set.
//! Existing services need a **container restart** (via the restart endpoint)
//! for the GUC change to take effect; a reload is not sufficient.
//!
//! Once the library is loaded, this service runs
//! `CREATE EXTENSION IF NOT EXISTS pg_stat_statements` on the target database
//! automatically before querying the view.

use std::sync::Arc;

use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseBackend, DbErr, Statement};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, warn};
use utoipa::ToSchema;

use crate::externalsvc::postgres::PostgresInputConfig;
use crate::externalsvc::ServiceType;
use crate::services::ExternalServiceManager;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum PgStatStatementsError {
    #[error(
        "Service {service_id} is not a Postgres service \
         (actual type: {actual_type})"
    )]
    NotAPostgresService {
        service_id: i32,
        actual_type: String,
    },

    #[error("Service {service_id} not found")]
    ServiceNotFound { service_id: i32 },

    #[error(
        "pg_stat_statements extension is not available on service {service_id}. \
         Ensure the container was started (or restarted) with \
         shared_preload_libraries=pg_stat_statements, then retry."
    )]
    ExtensionNotAvailable { service_id: i32 },

    #[error(
        "Failed to connect to Postgres service {service_id} \
         at {host}:{port}: {reason}"
    )]
    ConnectionFailed {
        service_id: i32,
        host: String,
        port: u16,
        reason: String,
    },

    #[error("Failed to parse configuration for service {service_id}: {reason}")]
    ConfigurationError { service_id: i32, reason: String },

    #[error("Query error on service {service_id}: {reason}")]
    QueryError { service_id: i32, reason: String },

    #[error("Validation error: {message}")]
    Validation { message: String },
}

impl From<DbErr> for PgStatStatementsError {
    fn from(e: DbErr) -> Self {
        // This From is only used in contexts where we don't have service_id
        // at the call site; prefer constructing QueryError directly when the
        // ID is available.
        PgStatStatementsError::QueryError {
            service_id: 0,
            reason: e.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// A single entry from `pg_stat_statements`, representing one normalized
/// query fingerprint and its aggregate execution stats.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SlowQueryRow {
    /// Normalized query text (parameter literals replaced with `$N`).
    pub query: String,

    /// Number of times this query was executed.
    pub calls: i64,

    /// Total wall-clock time spent executing this query, in milliseconds.
    pub total_exec_time_ms: f64,

    /// Average wall-clock time per execution, in milliseconds.
    pub mean_exec_time_ms: f64,

    /// Total number of rows returned or affected.
    pub rows: i64,

    /// Shared block cache hit ratio (0.0–1.0).
    /// `None` when total block accesses are zero (e.g. function-only queries).
    pub cache_hit_ratio: Option<f64>,
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// Maximum number of rows the caller may request.
pub const MAX_LIMIT: u32 = 100;
/// Default number of rows returned when no limit is specified.
pub const DEFAULT_LIMIT: u32 = 20;

pub struct PgStatStatementsService {
    external_service_manager: Arc<ExternalServiceManager>,
}

impl PgStatStatementsService {
    pub fn new(external_service_manager: Arc<ExternalServiceManager>) -> Self {
        Self {
            external_service_manager,
        }
    }

    /// Build a dedicated `DatabaseConnection` scoped to a single request
    /// against the user-provisioned Postgres service. The connection is closed
    /// when it goes out of scope.
    async fn connect_to_service(
        &self,
        service_id: i32,
    ) -> Result<(sea_orm::DatabaseConnection, i32), PgStatStatementsError> {
        let service_config = self
            .external_service_manager
            .get_service_config(service_id)
            .await
            .map_err(|_| PgStatStatementsError::ServiceNotFound { service_id })?;

        if service_config.service_type != ServiceType::Postgres {
            return Err(PgStatStatementsError::NotAPostgresService {
                service_id,
                actual_type: service_config.service_type.to_string(),
            });
        }

        let config: PostgresInputConfig = serde_json::from_value(service_config.parameters.clone())
            .map_err(|e| PgStatStatementsError::ConfigurationError {
                service_id,
                reason: e.to_string(),
            })?;

        // Resolve host/port: for clustered services use the primary's address.
        let (host, port) = match self
            .external_service_manager
            .get_cluster_primary_address(service_id)
            .await
        {
            Ok(Some((primary_host, primary_port))) => {
                debug!(
                    service_id,
                    primary_host,
                    primary_port,
                    "Using cluster primary for pg_stat_statements query"
                );
                (primary_host, primary_port)
            }
            Ok(None) => {
                let port_str = config.port.clone().unwrap_or_else(|| "5432".to_string());
                let port = port_str.parse::<u16>().map_err(|e| {
                    PgStatStatementsError::ConfigurationError {
                        service_id,
                        reason: format!("invalid port '{}': {}", port_str, e),
                    }
                })?;
                (config.host.clone(), port)
            }
            Err(e) => {
                return Err(PgStatStatementsError::ConnectionFailed {
                    service_id,
                    host: config.host.clone(),
                    port: 5432,
                    reason: format!("failed to resolve cluster primary: {}", e),
                });
            }
        };

        let password = config.password.clone().unwrap_or_default();

        // Use urlencoding for the password so special characters don't break the URL.
        let url = format!(
            "postgres://{}:{}@{}:{}/{}",
            urlencoding::encode(&config.username),
            urlencoding::encode(&password),
            host,
            port,
            urlencoding::encode(&config.database),
        );

        let mut opts = ConnectOptions::new(url);
        // Single connection — this is a one-off admin query, not a connection pool.
        opts.max_connections(1).min_connections(0);

        let db =
            Database::connect(opts)
                .await
                .map_err(|e| PgStatStatementsError::ConnectionFailed {
                    service_id,
                    host: host.clone(),
                    port,
                    reason: e.to_string(),
                })?;

        Ok((db, service_id))
    }

    /// Return the top `limit` queries ordered by `total_exec_time` descending
    /// for the given user-provisioned Postgres service.
    ///
    /// Connects to the service's database using its admin credentials (stored
    /// encrypted in the control plane). Validates that the service is of type
    /// Postgres and that `pg_stat_statements` is loaded.
    pub async fn top_slow_queries(
        &self,
        service_id: i32,
        limit: Option<u32>,
    ) -> Result<Vec<SlowQueryRow>, PgStatStatementsError> {
        let n = match limit {
            None => DEFAULT_LIMIT,
            Some(0) => {
                return Err(PgStatStatementsError::Validation {
                    message: format!("limit must be between 1 and {MAX_LIMIT}, got 0"),
                })
            }
            Some(v) if v > MAX_LIMIT => {
                return Err(PgStatStatementsError::Validation {
                    message: format!("limit must be between 1 and {MAX_LIMIT}, got {v}"),
                })
            }
            Some(v) => v,
        };

        let (db, service_id) = self.connect_to_service(service_id).await?;

        // Ensure the extension is installed. This is idempotent and fast when
        // it's already present. It will fail if shared_preload_libraries does
        // not yet include pg_stat_statements (requires a container restart).
        if let Err(e) = db
            .execute(Statement::from_string(
                DatabaseBackend::Postgres,
                "CREATE EXTENSION IF NOT EXISTS pg_stat_statements".to_owned(),
            ))
            .await
        {
            warn!(
                service_id,
                error = %e,
                "Could not enable pg_stat_statements; \
                 container may need a restart for shared_preload_libraries to take effect"
            );
            return Err(PgStatStatementsError::ExtensionNotAvailable { service_id });
        }

        // Verify the view is accessible (the library may not be loaded yet on
        // older containers even if the extension row exists).
        let view_check = db
            .execute(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT 1 FROM pg_stat_statements LIMIT 0".to_owned(),
            ))
            .await;

        if view_check.is_err() {
            return Err(PgStatStatementsError::ExtensionNotAvailable { service_id });
        }

        let sql = format!(
            r#"
            SELECT
                query,
                calls,
                total_exec_time,
                mean_exec_time,
                rows,
                CASE
                    WHEN (shared_blks_hit + shared_blks_read) = 0 THEN NULL
                    ELSE ROUND(
                        (shared_blks_hit::numeric /
                         (shared_blks_hit + shared_blks_read)) * 100,
                        2
                    ) / 100.0
                END AS cache_hit_ratio
            FROM pg_stat_statements
            WHERE calls > 0
            ORDER BY total_exec_time DESC
            LIMIT {n}
            "#
        );

        let rows = db
            .query_all(Statement::from_string(DatabaseBackend::Postgres, sql))
            .await
            .map_err(|e| PgStatStatementsError::QueryError {
                service_id,
                reason: e.to_string(),
            })?;

        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            let query: String =
                row.try_get("", "query")
                    .map_err(|e| PgStatStatementsError::QueryError {
                        service_id,
                        reason: format!("failed to read 'query' column: {}", e),
                    })?;
            let calls: i64 =
                row.try_get("", "calls")
                    .map_err(|e| PgStatStatementsError::QueryError {
                        service_id,
                        reason: format!("failed to read 'calls' column: {}", e),
                    })?;
            let total_exec_time: f64 = row.try_get("", "total_exec_time").map_err(|e| {
                PgStatStatementsError::QueryError {
                    service_id,
                    reason: format!("failed to read 'total_exec_time' column: {}", e),
                }
            })?;
            let mean_exec_time: f64 = row.try_get("", "mean_exec_time").map_err(|e| {
                PgStatStatementsError::QueryError {
                    service_id,
                    reason: format!("failed to read 'mean_exec_time' column: {}", e),
                }
            })?;
            let rows_count: i64 =
                row.try_get("", "rows")
                    .map_err(|e| PgStatStatementsError::QueryError {
                        service_id,
                        reason: format!("failed to read 'rows' column: {}", e),
                    })?;
            let cache_hit_ratio: Option<f64> = row.try_get("", "cache_hit_ratio").ok();

            result.push(SlowQueryRow {
                query,
                calls,
                total_exec_time_ms: total_exec_time,
                mean_exec_time_ms: mean_exec_time,
                rows: rows_count,
                cache_hit_ratio,
            });
        }

        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_limit_is_within_max() {
        const { assert!(DEFAULT_LIMIT <= MAX_LIMIT) };
    }

    /// Validate the limit-check helper logic in isolation (no external deps).
    #[test]
    fn test_limit_validation_logic() {
        // Zero must fail
        assert!(matches!(
            validate_limit(Some(0)),
            Err(PgStatStatementsError::Validation { .. })
        ));
        // Over max must fail
        assert!(matches!(
            validate_limit(Some(MAX_LIMIT + 1)),
            Err(PgStatStatementsError::Validation { .. })
        ));
        // None → DEFAULT_LIMIT
        assert_eq!(validate_limit(None).unwrap(), DEFAULT_LIMIT);
        // Valid value
        assert_eq!(validate_limit(Some(10)).unwrap(), 10);
        // Max is allowed
        assert_eq!(validate_limit(Some(MAX_LIMIT)).unwrap(), MAX_LIMIT);
    }

    fn validate_limit(limit: Option<u32>) -> Result<u32, PgStatStatementsError> {
        match limit {
            None => Ok(DEFAULT_LIMIT),
            Some(0) => Err(PgStatStatementsError::Validation {
                message: format!("limit must be between 1 and {MAX_LIMIT}, got 0"),
            }),
            Some(v) if v > MAX_LIMIT => Err(PgStatStatementsError::Validation {
                message: format!("limit must be between 1 and {MAX_LIMIT}, got {v}"),
            }),
            Some(v) => Ok(v),
        }
    }
}
