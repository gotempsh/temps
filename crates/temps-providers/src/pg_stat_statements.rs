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

use sea_orm::DbErr;
#[cfg(test)]
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, error, warn};
use utoipa::ToSchema;

use crate::externalsvc::postgres::PostgresInputConfig;
use crate::externalsvc::ServiceType;
use crate::services::ExternalServiceManager;

const RESET_FUNCTION_LOOKUP_SQL: &str = r#"
    SELECT
        n.nspname AS schema_name,
        p.pronargs::integer AS argument_count
    FROM pg_catalog.pg_extension e
    JOIN pg_catalog.pg_depend d
      ON d.refclassid = 'pg_catalog.pg_extension'::pg_catalog.regclass
     AND d.refobjid = e.oid
     AND d.deptype = 'e'
    JOIN pg_catalog.pg_proc p
      ON d.classid = 'pg_catalog.pg_proc'::pg_catalog.regclass
     AND d.objid = p.oid
    JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
    WHERE e.extname = 'pg_stat_statements'
      AND p.proname = 'pg_stat_statements_reset'
      AND p.proargtypes IN (
          '26 26 20'::pg_catalog.oidvector,
          '26 26 20 16'::pg_catalog.oidvector
      )
    ORDER BY p.pronargs DESC
    LIMIT 1
"#;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum PgStatConnectionPolicy {
    Standard,
    ManagedPrivate,
}

impl PgStatConnectionPolicy {
    async fn connect(
        self,
        host: &str,
        port: u16,
        username: &str,
        password: &str,
        database: &str,
    ) -> temps_query::Result<tokio_postgres::Client> {
        match self {
            Self::Standard => {
                temps_query_postgres::connect_with_tls_ladder(
                    host, port, username, password, database,
                )
                .await
            }
            Self::ManagedPrivate => {
                temps_query_postgres::connect_with_private_tls_ladder(
                    host, port, username, password, database,
                )
                .await
            }
        }
    }
}

fn pg_stat_connection_target(
    cluster_primary: Option<(String, u16)>,
    standalone_host: String,
    standalone_port: u16,
) -> (String, u16, PgStatConnectionPolicy) {
    match cluster_primary {
        Some((host, port)) => (host, port, PgStatConnectionPolicy::ManagedPrivate),
        None => (
            standalone_host,
            standalone_port,
            PgStatConnectionPolicy::Standard,
        ),
    }
}

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

    #[error(
        "Failed to reset pg_stat_statements statistics on service {service_id}. \
         Verify that the database role can execute the extension reset function."
    )]
    ResetFailed { service_id: i32 },

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

    /// Name of the database this query ran against. `(dropped database)`
    /// when the originating database no longer exists but
    /// `pg_stat_statements` still holds stats for it.
    pub database: String,

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

/// Column a slow-queries listing may be sorted by.
///
/// Deliberately a closed enum rather than a raw column-name string: the SQL
/// `ORDER BY` clause is built by interpolating [`SlowQuerySortKey::column`]
/// directly into the query, so only these whitelisted, hardcoded column
/// names can ever reach it — user input never touches the SQL string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlowQuerySortKey {
    Calls,
    TotalExecTime,
    MeanExecTime,
    Rows,
    CacheHitRatio,
}

impl SlowQuerySortKey {
    /// Parse the wire representation used by the API (matches the
    /// corresponding `SlowQueryRow` field names).
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "calls" => Ok(Self::Calls),
            "total_exec_time_ms" => Ok(Self::TotalExecTime),
            "mean_exec_time_ms" => Ok(Self::MeanExecTime),
            "rows" => Ok(Self::Rows),
            "cache_hit_ratio" => Ok(Self::CacheHitRatio),
            other => Err(format!(
                "invalid sort_by '{other}'; expected one of: calls, \
                 total_exec_time_ms, mean_exec_time_ms, rows, cache_hit_ratio"
            )),
        }
    }

    /// The `pg_stat_statements` column (or `SELECT`-list alias, for
    /// `cache_hit_ratio`) this sort key maps to.
    fn column(self) -> &'static str {
        match self {
            // Qualified with the `s` alias (pg_stat_statements) since the
            // query joins in pg_database as `d` for the database column.
            Self::Calls => "s.calls",
            Self::TotalExecTime => "s.total_exec_time",
            Self::MeanExecTime => "s.mean_exec_time",
            Self::Rows => "s.rows",
            // Computed SELECT-list alias, not a real column — can't be
            // qualified with a table prefix.
            Self::CacheHitRatio => "cache_hit_ratio",
        }
    }
}

/// Sort direction for the slow-queries listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Asc,
    Desc,
}

impl SortOrder {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "asc" => Ok(Self::Asc),
            "desc" => Ok(Self::Desc),
            other => Err(format!(
                "invalid sort_order '{other}'; expected 'asc' or 'desc'"
            )),
        }
    }

    fn sql(self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }
}

/// Pagination and sort parameters for the slow-queries endpoint.
#[derive(Debug, Clone)]
pub struct SlowQueryPage {
    /// 1-based page number.
    pub page: u32,
    /// Number of rows per page (1–100).
    pub page_size: u32,
    /// Column to sort by. Applied server-side so ordering is consistent
    /// across pages — sorting only the rows within an already-paginated
    /// page would show different "top" queries depending on where the
    /// page boundary happened to fall.
    pub sort_by: SlowQuerySortKey,
    /// Sort direction.
    pub sort_order: SortOrder,
}

impl Default for SlowQueryPage {
    fn default() -> Self {
        Self {
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
            sort_by: SlowQuerySortKey::MeanExecTime,
            sort_order: SortOrder::Desc,
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

    /// Build a dedicated pinned tokio-postgres client scoped to one request.
    /// Managed clusters require a private destination; standalone external
    /// services retain the verified-public TLS policy.
    async fn connect_to_service(
        &self,
        service_id: i32,
    ) -> Result<(tokio_postgres::Client, i32), PgStatStatementsError> {
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
        let (host, port, connection_policy) = match self
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
                pg_stat_connection_target(
                    Some((primary_host, primary_port)),
                    config.host.clone(),
                    5432,
                )
            }
            Ok(None) => {
                let port_str = config.port.clone().unwrap_or_else(|| "5432".to_string());
                let port = port_str.parse::<u16>().map_err(|e| {
                    PgStatStatementsError::ConfigurationError {
                        service_id,
                        reason: format!("invalid port '{}': {}", port_str, e),
                    }
                })?;
                pg_stat_connection_target(None, config.host.clone(), port)
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

        let password = config.password.unwrap_or_default();
        let connection = connection_policy
            .connect(&host, port, &config.username, &password, &config.database)
            .await
            .map_err(|error| PgStatStatementsError::ConnectionFailed {
                service_id,
                host: host.clone(),
                port,
                reason: error.to_string(),
            })?;

        Ok((connection, service_id))
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

    /// Clear all aggregate query statistics collected by
    /// `pg_stat_statements` for the target Postgres instance.
    ///
    /// PostgreSQL applies this reset across every user, database, and query
    /// visible to the extension. The service account must have permission to
    /// execute `pg_stat_statements_reset()`; managed providers may require an
    /// elevated provider-specific role.
    ///
    /// Note: the caller (handler) must perform the ownership check
    /// (`assert_service_owned_by_caller`) before invoking this method.
    pub async fn reset_pg_stat_statements(
        &self,
        service_id: i32,
    ) -> Result<(), PgStatStatementsError> {
        let (client, service_id) = self.connect_to_service(service_id).await?;
        Self::reset_on_client(&client, service_id).await
    }

    async fn reset_on_client(
        client: &tokio_postgres::Client,
        service_id: i32,
    ) -> Result<(), PgStatStatementsError> {
        let function_row = client
            .query_opt(RESET_FUNCTION_LOOKUP_SQL, &[])
            .await
            .map_err(|db_error| {
                error!(
                    service_id,
                    error = %db_error,
                    "Failed to resolve the extension-owned pg_stat_statements reset function"
                );
                PgStatStatementsError::ResetFailed { service_id }
            })?
            .ok_or(PgStatStatementsError::ExtensionNotAvailable { service_id })?;

        let schema_name: String = function_row.try_get("schema_name").map_err(|db_error| {
            error!(
                service_id,
                error = %db_error,
                "Failed to read pg_stat_statements extension schema"
            );
            PgStatStatementsError::ResetFailed { service_id }
        })?;
        let argument_count: i32 = function_row.try_get("argument_count").map_err(|db_error| {
            error!(
                service_id,
                error = %db_error,
                "Failed to read pg_stat_statements reset function signature"
            );
            PgStatStatementsError::ResetFailed { service_id }
        })?;
        let reset_sql = Self::build_reset_sql(&schema_name, argument_count, service_id)?;

        client.execute(&reset_sql, &[]).await.map_err(|db_error| {
            error!(
                service_id,
                extension_schema = schema_name,
                error = %db_error,
                "Target Postgres rejected pg_stat_statements reset"
            );
            PgStatStatementsError::ResetFailed { service_id }
        })?;
        Ok(())
    }

    fn build_reset_sql(
        schema_name: &str,
        argument_count: i32,
        service_id: i32,
    ) -> Result<String, PgStatStatementsError> {
        // The function is resolved through its pg_extension dependency, not
        // merely by schema and name, so an unrelated same-schema overload
        // cannot influence the selected signature. The schema is still quoted
        // as an identifier so unusual extension schemas remain valid.
        let quoted_schema = format!("\"{}\"", schema_name.replace('"', "\"\""));
        match argument_count {
            3 => Ok(format!(
                "SELECT {quoted_schema}.pg_stat_statements_reset(0::oid, 0::oid, 0::bigint)"
            )),
            4 => Ok(format!(
                "SELECT {quoted_schema}.pg_stat_statements_reset(0::oid, 0::oid, 0::bigint, false::boolean)"
            )),
            _ => {
                error!(
                    service_id,
                    argument_count,
                    "Resolved an unsupported pg_stat_statements reset function signature"
                );
                Err(PgStatStatementsError::ResetFailed { service_id })
            }
        }
    }

    #[cfg(test)]
    async fn reset_on_connection<C>(db: &C, service_id: i32) -> Result<(), PgStatStatementsError>
    where
        C: ConnectionTrait,
    {
        let function_row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                RESET_FUNCTION_LOOKUP_SQL.to_owned(),
            ))
            .await
            .map_err(|db_error| {
                error!(
                    service_id,
                    error = %db_error,
                    "Failed to resolve the extension-owned pg_stat_statements reset function"
                );
                PgStatStatementsError::ResetFailed { service_id }
            })?
            .ok_or(PgStatStatementsError::ExtensionNotAvailable { service_id })?;

        let schema_name: String = function_row
            .try_get("", "schema_name")
            .map_err(|db_error| {
                error!(
                    service_id,
                    error = %db_error,
                    "Failed to read pg_stat_statements extension schema"
                );
                PgStatStatementsError::ResetFailed { service_id }
            })?;
        let argument_count: i32 =
            function_row
                .try_get("", "argument_count")
                .map_err(|db_error| {
                    error!(
                        service_id,
                        error = %db_error,
                        "Failed to read pg_stat_statements reset function signature"
                    );
                    PgStatStatementsError::ResetFailed { service_id }
                })?;

        let reset_sql = Self::build_reset_sql(&schema_name, argument_count, service_id)?;

        db.execute(Statement::from_string(DatabaseBackend::Postgres, reset_sql))
            .await
            .map_err(|db_error| {
                error!(
                    service_id,
                    extension_schema = schema_name,
                    error = %db_error,
                    "Target Postgres rejected pg_stat_statements reset"
                );
                PgStatStatementsError::ResetFailed { service_id }
            })?;

        Ok(())
    }

    /// Return a paginated, sorted slice of queries for the given
    /// user-provisioned Postgres service. Sorting is applied server-side,
    /// before the `LIMIT`/`OFFSET`, so the ordering is consistent across
    /// pages regardless of which sort column the caller picked.
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

        let (client, service_id) = self.connect_to_service(service_id).await?;
        Self::top_slow_queries_on_client(&client, service_id, pagination).await
    }

    async fn top_slow_queries_on_client(
        client: &tokio_postgres::Client,
        service_id: i32,
        pagination: SlowQueryPage,
    ) -> Result<(Vec<SlowQueryRow>, u64), PgStatStatementsError> {
        let limit = pagination.page_size;
        let offset = (pagination.page - 1) * pagination.page_size;

        // Ensure the extension is installed. This is idempotent and fast when
        // it's already present. It will fail if shared_preload_libraries does
        // not yet include pg_stat_statements (requires a container restart).
        if let Err(e) = client
            .batch_execute("CREATE EXTENSION IF NOT EXISTS pg_stat_statements")
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
        let view_check = client
            .query("SELECT 1 FROM pg_stat_statements LIMIT 0", &[])
            .await;

        if view_check.is_err() {
            return Err(PgStatStatementsError::ExtensionNotAvailable { service_id });
        }

        // Total count for pagination controls.
        let count_sql = "SELECT COUNT(*)::bigint AS n FROM pg_stat_statements WHERE calls > 0";
        let count_row = client
            .query_opt(count_sql, &[])
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
                .try_get("n")
                .map_err(|e| PgStatStatementsError::QueryError {
                    service_id,
                    reason: format!("failed to read count: {}", e),
                })?;

        // `sort_by`/`sort_order` come only from the closed enums above —
        // never from an interpolated user string — so this is not
        // SQL-injectable despite the `format!`.
        let order_by_column = pagination.sort_by.column();
        let order_by_direction = pagination.sort_order.sql();

        let sql = format!(
            r#"
            SELECT
                s.query,
                s.calls,
                s.total_exec_time,
                s.mean_exec_time,
                s.rows,
                COALESCE(d.datname, '(dropped database)') AS database,
                (CASE
                    WHEN (s.shared_blks_hit + s.shared_blks_read) = 0 THEN NULL
                    ELSE ROUND(
                        (s.shared_blks_hit::numeric /
                         (s.shared_blks_hit + s.shared_blks_read)) * 100,
                        2
                    ) / 100.0
                END)::double precision AS cache_hit_ratio
            FROM pg_stat_statements s
            LEFT JOIN pg_database d ON d.oid = s.dbid
            WHERE s.calls > 0
            ORDER BY {order_by_column} {order_by_direction}
            LIMIT {limit}
            OFFSET {offset}
            "#
        );

        let rows =
            client
                .query(&sql, &[])
                .await
                .map_err(|e| PgStatStatementsError::QueryError {
                    service_id,
                    reason: e.to_string(),
                })?;

        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            let query: String =
                row.try_get("query")
                    .map_err(|e| PgStatStatementsError::QueryError {
                        service_id,
                        reason: format!("failed to read 'query' column: {}", e),
                    })?;
            let calls: i64 =
                row.try_get("calls")
                    .map_err(|e| PgStatStatementsError::QueryError {
                        service_id,
                        reason: format!("failed to read 'calls' column: {}", e),
                    })?;
            let total_exec_time: f64 =
                row.try_get("total_exec_time")
                    .map_err(|e| PgStatStatementsError::QueryError {
                        service_id,
                        reason: format!("failed to read 'total_exec_time' column: {}", e),
                    })?;
            let mean_exec_time: f64 =
                row.try_get("mean_exec_time")
                    .map_err(|e| PgStatStatementsError::QueryError {
                        service_id,
                        reason: format!("failed to read 'mean_exec_time' column: {}", e),
                    })?;
            let rows_count: i64 =
                row.try_get("rows")
                    .map_err(|e| PgStatStatementsError::QueryError {
                        service_id,
                        reason: format!("failed to read 'rows' column: {}", e),
                    })?;
            let cache_hit_ratio: Option<f64> = row.try_get("cache_hit_ratio").ok();
            let database: String =
                row.try_get("database")
                    .map_err(|e| PgStatStatementsError::QueryError {
                        service_id,
                        reason: format!("failed to read 'database' column: {}", e),
                    })?;

            result.push(SlowQueryRow {
                query,
                database,
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
    use sea_orm::{MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;
    use testcontainers::{
        core::{ContainerPort, WaitFor},
        runners::AsyncRunner,
        GenericImage, ImageExt,
    };

    fn container_runtime_unavailable(error: &str) -> bool {
        let message = error.to_ascii_lowercase();
        [
            "hyper legacy client: client error (connect)",
            "failed to connect to docker",
            "error connecting to docker",
            "docker daemon is unavailable",
            "docker client is unavailable",
            "could not find docker environment",
            "docker socket",
        ]
        .iter()
        .any(|marker| message.contains(marker))
    }

    fn extension_function_row(schema_name: &str, argument_count: i32) -> BTreeMap<String, Value> {
        let mut row = BTreeMap::new();
        row.insert(
            "schema_name".to_owned(),
            Value::String(Some(Box::new(schema_name.to_owned()))),
        );
        row.insert(
            "argument_count".to_owned(),
            Value::Int(Some(argument_count)),
        );
        row
    }

    #[test]
    fn test_pg_stat_connection_target_selects_policy_from_cluster_primary() {
        let cluster = pg_stat_connection_target(
            Some(("10.0.0.9".to_string(), 6432)),
            "public.example".to_string(),
            5432,
        );
        assert_eq!(cluster.0, "10.0.0.9");
        assert_eq!(cluster.1, 6432);
        assert_eq!(cluster.2, PgStatConnectionPolicy::ManagedPrivate);

        let standalone = pg_stat_connection_target(None, "public.example".to_string(), 5433);
        assert_eq!(standalone.0, "public.example");
        assert_eq!(standalone.1, 5433);
        assert_eq!(standalone.2, PgStatConnectionPolicy::Standard);
    }

    #[tokio::test]
    async fn test_pg_stat_connection_policy_managed_private_rejects_public_credentials() {
        let error = PgStatConnectionPolicy::ManagedPrivate
            .connect(
                "203.0.113.10",
                5432,
                "cluster_admin",
                "secret host=attacker.example sslmode=disable",
                "postgres",
            )
            .await
            .expect_err("managed cluster statistics must reject a public endpoint");

        assert!(
            error
                .to_string()
                .contains("refusing to send cluster credentials even over verified TLS"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn production_client_lists_and_resets_pg_stat_statements() {
        let container = match GenericImage::new("postgres", "18-alpine")
            .with_exposed_port(ContainerPort::Tcp(5432))
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_DB", "postgres")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
            .with_cmd([
                "postgres".to_owned(),
                "-c".to_owned(),
                "shared_preload_libraries=pg_stat_statements".to_owned(),
            ])
            .start()
            .await
        {
            Ok(container) => container,
            Err(error) if container_runtime_unavailable(&error.to_string()) => {
                eprintln!(
                    "Docker unavailable; skipping pg_stat_statements production-client test: {error}"
                );
                return;
            }
            Err(error) => panic!("failed to start pg_stat_statements test container: {error}"),
        };

        let host = container
            .get_host()
            .await
            .expect("started PostgreSQL container must expose its host")
            .to_string();
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("started PostgreSQL container must expose port 5432");

        let client = PgStatConnectionPolicy::ManagedPrivate
            .connect(&host, port, "postgres", "", "postgres")
            .await
            .expect("private transport should connect to the local PostgreSQL container");
        client
            .batch_execute("CREATE EXTENSION IF NOT EXISTS pg_stat_statements")
            .await
            .expect("pg_stat_statements should be available in the stock PostgreSQL image");
        for _ in 0..3 {
            client
                .query("SELECT 42::integer /* temps_pgstat_reset_probe */", &[])
                .await
                .expect("sample query should be recorded");
        }

        let (rows, total_count) = PgStatStatementsService::top_slow_queries_on_client(
            &client,
            42,
            SlowQueryPage::default(),
        )
        .await
        .expect("production tokio-postgres row decoding should succeed");
        assert!(total_count > 0, "sample queries should produce statistics");
        assert!(!rows.is_empty(), "statistics page should contain rows");
        assert!(rows.iter().all(|row| row.calls > 0));
        assert!(
            rows.iter()
                .any(|row| row.query.contains("temps_pgstat_reset_probe")),
            "statistics page should decode the tagged sample query"
        );

        PgStatStatementsService::reset_on_client(&client, 42)
            .await
            .expect("production reset lookup and invocation should succeed");
        let reset_probe_count: i64 = client
            .query_one(
                "SELECT COUNT(*)::bigint FROM pg_stat_statements WHERE query LIKE $1",
                &[&"%temps_pgstat_reset_probe%"],
            )
            .await
            .expect("statistics should remain queryable after reset")
            .get(0);
        assert_eq!(
            reset_probe_count, 0,
            "reset should clear the tagged sample query statistics"
        );
    }

    #[test]
    fn test_default_page_size_is_within_max() {
        const { assert!(DEFAULT_PAGE_SIZE <= MAX_PAGE_SIZE) };
    }

    #[test]
    fn test_page_default() {
        let p = SlowQueryPage::default();
        assert_eq!(p.page, 1);
        assert_eq!(p.page_size, DEFAULT_PAGE_SIZE);
        assert_eq!(p.sort_by, SlowQuerySortKey::MeanExecTime);
        assert_eq!(p.sort_order, SortOrder::Desc);
    }

    #[test]
    fn test_sort_key_parse_accepts_all_documented_values() {
        assert_eq!(
            SlowQuerySortKey::parse("calls"),
            Ok(SlowQuerySortKey::Calls)
        );
        assert_eq!(
            SlowQuerySortKey::parse("total_exec_time_ms"),
            Ok(SlowQuerySortKey::TotalExecTime)
        );
        assert_eq!(
            SlowQuerySortKey::parse("mean_exec_time_ms"),
            Ok(SlowQuerySortKey::MeanExecTime)
        );
        assert_eq!(SlowQuerySortKey::parse("rows"), Ok(SlowQuerySortKey::Rows));
        assert_eq!(
            SlowQuerySortKey::parse("cache_hit_ratio"),
            Ok(SlowQuerySortKey::CacheHitRatio)
        );
    }

    #[test]
    fn test_sort_key_parse_rejects_unknown_value() {
        // Rejecting free-form input here is what keeps `column()`'s output
        // restricted to the whitelisted strings interpolated into the SQL
        // ORDER BY clause in `top_slow_queries` — an attacker-controlled
        // sort_by must never reach that format!.
        let result = SlowQuerySortKey::parse("query; DROP TABLE pg_stat_statements;--");
        assert!(result.is_err());
    }

    #[test]
    fn test_sort_key_maps_to_expected_sql_column() {
        // Real pg_stat_statements columns are qualified with the `s` alias
        // used in the query's FROM clause; cache_hit_ratio is a computed
        // SELECT-list alias and can't be table-qualified.
        assert_eq!(SlowQuerySortKey::Calls.column(), "s.calls");
        assert_eq!(
            SlowQuerySortKey::TotalExecTime.column(),
            "s.total_exec_time"
        );
        assert_eq!(SlowQuerySortKey::MeanExecTime.column(), "s.mean_exec_time");
        assert_eq!(SlowQuerySortKey::Rows.column(), "s.rows");
        assert_eq!(SlowQuerySortKey::CacheHitRatio.column(), "cache_hit_ratio");
    }

    #[test]
    fn test_sort_order_parse() {
        assert_eq!(SortOrder::parse("asc"), Ok(SortOrder::Asc));
        assert_eq!(SortOrder::parse("desc"), Ok(SortOrder::Desc));
        assert!(SortOrder::parse("sideways").is_err());
    }

    #[tokio::test]
    async fn reset_on_connection_executes_global_reset() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![extension_function_row("extensions", 4)]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        PgStatStatementsService::reset_on_connection(&db, 42)
            .await
            .expect("reset should succeed");

        let log = db.into_transaction_log();
        let statements = log
            .iter()
            .flat_map(|transaction| transaction.statements())
            .map(|statement| statement.sql.as_str())
            .collect::<Vec<_>>();
        assert!(statements
            .iter()
            .any(|sql| sql.contains("pg_catalog.pg_depend")));
        assert!(statements.contains(
            &"SELECT \"extensions\".pg_stat_statements_reset(0::oid, 0::oid, 0::bigint, false::boolean)"
        ));
    }

    #[tokio::test]
    async fn reset_on_connection_uses_exact_legacy_three_argument_signature() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![extension_function_row("public", 3)]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        PgStatStatementsService::reset_on_connection(&db, 42)
            .await
            .expect("legacy reset should succeed");

        let log = db.into_transaction_log();
        assert!(log
            .iter()
            .flat_map(|transaction| transaction.statements())
            .any(|statement| statement.sql
                == "SELECT \"public\".pg_stat_statements_reset(0::oid, 0::oid, 0::bigint)"));
    }

    #[tokio::test]
    async fn reset_on_connection_quotes_extension_schema_identifier() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![extension_function_row("odd\"schema", 4)]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        PgStatStatementsService::reset_on_connection(&db, 42)
            .await
            .expect("quoted extension schema should be safe");

        let log = db.into_transaction_log();
        let reset_statement = log
            .iter()
            .flat_map(|transaction| transaction.statements())
            .find(|statement| statement.sql.starts_with("SELECT \"odd\"\"schema\""))
            .expect("reset statement should be recorded");
        assert_eq!(
            reset_statement.sql,
            "SELECT \"odd\"\"schema\".pg_stat_statements_reset(0::oid, 0::oid, 0::bigint, false::boolean)"
        );
    }

    #[tokio::test]
    async fn reset_on_connection_preserves_service_context_on_error() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![extension_function_row("public", 4)]])
            .append_exec_errors([DbErr::Custom("permission denied for function".to_owned())])
            .into_connection();

        let error = PgStatStatementsService::reset_on_connection(&db, 73)
            .await
            .expect_err("permission failure must be returned");

        assert!(matches!(
            &error,
            PgStatStatementsError::ResetFailed { service_id }
                if *service_id == 73
        ));
        assert!(!error.to_string().contains("permission denied"));
    }

    #[tokio::test]
    async fn reset_on_connection_reports_missing_extension() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        let error = PgStatStatementsService::reset_on_connection(&db, 91)
            .await
            .expect_err("missing extension must be reported");

        assert!(matches!(
            error,
            PgStatStatementsError::ExtensionNotAvailable { service_id: 91 }
        ));
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
