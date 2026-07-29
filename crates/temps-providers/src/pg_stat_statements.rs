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

    #[error(
        "Self-service pg_stat_statements restart is not available for clustered Postgres \
         service {service_id}. A rolling restart across all cluster nodes is required — \
         perform the restart manually on each node or contact your administrator."
    )]
    ClusteredServiceNotSupported { service_id: i32 },

    #[error("Failed to restart service {service_id} to enable pg_stat_statements: {reason}")]
    RestartFailed { service_id: i32, reason: String },

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

/// Pagination parameters for the slow-queries endpoint.
#[derive(Debug, Clone)]
pub struct SlowQueryPage {
    /// 1-based page number.
    pub page: u32,
    /// Number of rows per page (1–100).
    pub page_size: u32,
}

impl Default for SlowQueryPage {
    fn default() -> Self {
        Self {
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
        }
    }
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// Maximum page size the caller may request.
pub const MAX_PAGE_SIZE: u32 = 100;
/// Default page size returned when none is specified.
pub const DEFAULT_PAGE_SIZE: u32 = 20;

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

    /// Enable `pg_stat_statements` on a standalone Postgres service by
    /// stopping and restarting its container so the
    /// `shared_preload_libraries=pg_stat_statements` CMD flag takes effect.
    ///
    /// # Safety constraints
    ///
    /// * **Standalone only.** Clustered (HA) services are rejected with
    ///   [`PgStatStatementsError::ClusteredServiceNotSupported`] — a blind
    ///   single-container restart bypasses controlled failover. The caller must
    ///   surface manual instructions for that case.
    /// * **Data-safe.** The container restart reuses the existing named Docker
    ///   volume (`{container_name}_data`). `create_container_once` calls
    ///   `docker.create_volume` which is idempotent — the volume is never
    ///   recreated fresh.
    ///
    /// Note: the caller (handler) must perform the ownership check
    /// (`assert_service_owned_by_caller`) before invoking this method.
    pub async fn enable_pg_stat_statements(
        &self,
        service_id: i32,
    ) -> Result<(), PgStatStatementsError> {
        // Load the service to check type and topology.
        let service = self
            .external_service_manager
            .get_service(service_id)
            .await
            .map_err(|_| PgStatStatementsError::ServiceNotFound { service_id })?;

        if service.service_type != "postgres" {
            return Err(PgStatStatementsError::NotAPostgresService {
                service_id,
                actual_type: service.service_type.clone(),
            });
        }

        if service.topology == "cluster" {
            return Err(PgStatStatementsError::ClusteredServiceNotSupported { service_id });
        }

        // `force_recreate_service_container` hydrates the engine's config
        // explicitly and recreates the container so the
        // `shared_preload_libraries` CMD flag is applied — a plain
        // stop_service()/start_service() is not sufficient here: the fresh
        // service instance each call constructs has no in-memory config, so
        // start()'s drift-reconciliation would have nothing to build the new
        // container from once it detects drift.
        self.external_service_manager
            .force_recreate_service_container(service_id)
            .await
            .map_err(|e| PgStatStatementsError::RestartFailed {
                service_id,
                reason: format!("recreate failed: {}", e),
            })?;

        Ok(())
    }

    /// Return a paginated slice of queries ordered by `total_exec_time`
    /// descending for the given user-provisioned Postgres service.
    ///
    /// Connects to the service's database using its admin credentials (stored
    /// encrypted in the control plane). Validates that the service is of type
    /// Postgres and that `pg_stat_statements` is loaded.
    ///
    /// Returns the page of rows together with the total count of qualifying
    /// rows so the caller can render pagination controls.
    pub async fn top_slow_queries(
        &self,
        service_id: i32,
        pagination: SlowQueryPage,
    ) -> Result<(Vec<SlowQueryRow>, u64), PgStatStatementsError> {
        if pagination.page == 0 {
            return Err(PgStatStatementsError::Validation {
                message: "page must be >= 1".to_owned(),
            });
        }
        if pagination.page_size == 0 || pagination.page_size > MAX_PAGE_SIZE {
            return Err(PgStatStatementsError::Validation {
                message: format!(
                    "page_size must be between 1 and {MAX_PAGE_SIZE}, got {}",
                    pagination.page_size
                ),
            });
        }

        let limit = pagination.page_size;
        let offset = (pagination.page - 1) * pagination.page_size;

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

        // Total count for pagination controls.
        let count_sql = "SELECT COUNT(*)::bigint AS n FROM pg_stat_statements WHERE calls > 0";
        let count_row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                count_sql.to_owned(),
            ))
            .await
            .map_err(|e| PgStatStatementsError::QueryError {
                service_id,
                reason: format!("failed to count pg_stat_statements rows: {}", e),
            })?
            .ok_or_else(|| PgStatStatementsError::QueryError {
                service_id,
                reason: "COUNT query returned no rows".to_owned(),
            })?;
        let total_count: i64 =
            count_row
                .try_get("", "n")
                .map_err(|e| PgStatStatementsError::QueryError {
                    service_id,
                    reason: format!("failed to read count: {}", e),
                })?;

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
            LIMIT {limit}
            OFFSET {offset}
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

        Ok((result, total_count as u64))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_page_size_is_within_max() {
        const { assert!(DEFAULT_PAGE_SIZE <= MAX_PAGE_SIZE) };
    }

    #[test]
    fn test_page_default() {
        let p = SlowQueryPage::default();
        assert_eq!(p.page, 1);
        assert_eq!(p.page_size, DEFAULT_PAGE_SIZE);
    }

    /// Validate the page_size bounds (logic extracted from top_slow_queries).
    #[test]
    fn test_page_size_validation() {
        // page_size=0 must fail
        let result = validate_page_size(0);
        assert!(matches!(
            result,
            Err(PgStatStatementsError::Validation { .. })
        ));

        // page_size > MAX must fail
        let result = validate_page_size(MAX_PAGE_SIZE + 1);
        assert!(matches!(
            result,
            Err(PgStatStatementsError::Validation { .. })
        ));

        // Valid values
        assert!(validate_page_size(1).is_ok());
        assert!(validate_page_size(MAX_PAGE_SIZE).is_ok());
        assert!(validate_page_size(20).is_ok());
    }

    fn validate_page_size(page_size: u32) -> Result<(), PgStatStatementsError> {
        if page_size == 0 || page_size > MAX_PAGE_SIZE {
            return Err(PgStatStatementsError::Validation {
                message: format!(
                    "page_size must be between 1 and {MAX_PAGE_SIZE}, got {}",
                    page_size
                ),
            });
        }
        Ok(())
    }
}
