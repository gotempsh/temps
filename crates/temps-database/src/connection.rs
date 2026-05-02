//! Database connection management

use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};
use std::sync::Arc;
use std::time::Duration;
use temps_core::{ServiceError, ServiceResult};
use temps_migrations::{Migrator, MigratorTrait};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::debug;

pub type DbConnection = DatabaseConnection;

/// Default timeout for database connectivity check (5 seconds)
const CONNECTIVITY_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// Default timeout for database connection establishment (30 seconds)
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);

/// Default timeout for running migrations (120 seconds)
const MIGRATION_TIMEOUT: Duration = Duration::from_secs(120);

/// Parse database URL and extract host and port
fn parse_database_url(database_url: &str) -> Result<(String, u16), String> {
    // Handle postgres:// or postgresql:// URLs
    let url =
        if database_url.starts_with("postgres://") || database_url.starts_with("postgresql://") {
            database_url.to_string()
        } else {
            return Err("Database URL must start with postgres:// or postgresql://".to_string());
        };

    // Parse the URL to extract host and port
    // Format: postgres://user:password@host:port/database
    let without_scheme = url
        .strip_prefix("postgres://")
        .or_else(|| url.strip_prefix("postgresql://"))
        .ok_or("Invalid database URL scheme")?;

    // Find the @ separator (after credentials)
    let host_part = if let Some(at_pos) = without_scheme.rfind('@') {
        &without_scheme[at_pos + 1..]
    } else {
        without_scheme
    };

    // Remove database name (everything after /)
    let host_port = if let Some(slash_pos) = host_part.find('/') {
        &host_part[..slash_pos]
    } else {
        host_part
    };

    // Remove query parameters (everything after ?)
    let host_port = if let Some(query_pos) = host_port.find('?') {
        &host_port[..query_pos]
    } else {
        host_port
    };

    // Parse host and port
    // Handle IPv6 addresses like [::1]:5432
    let (host, port) = if host_port.starts_with('[') {
        // IPv6 address
        if let Some(bracket_end) = host_port.find(']') {
            let ipv6_host = &host_port[1..bracket_end];
            let port_part = &host_port[bracket_end + 1..];
            let port = if let Some(stripped) = port_part.strip_prefix(':') {
                stripped.parse::<u16>().unwrap_or(5432)
            } else {
                5432
            };
            (ipv6_host.to_string(), port)
        } else {
            return Err("Invalid IPv6 address format in database URL".to_string());
        }
    } else if let Some(colon_pos) = host_port.rfind(':') {
        let host = &host_port[..colon_pos];
        let port = host_port[colon_pos + 1..].parse::<u16>().unwrap_or(5432);
        (host.to_string(), port)
    } else {
        (host_port.to_string(), 5432)
    };

    if host.is_empty() {
        return Err("Empty host in database URL".to_string());
    }

    Ok((host, port))
}

/// Check if the database host:port is reachable via TCP
async fn check_database_connectivity(host: &str, port: u16) -> Result<(), String> {
    let addr = format!("{}:{}", host, port);

    match timeout(CONNECTIVITY_CHECK_TIMEOUT, TcpStream::connect(&addr)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(format!("Cannot connect to database at {}: {}", addr, e)),
        Err(_) => Err(format!(
            "Connection to database at {} timed out after {} seconds",
            addr,
            CONNECTIVITY_CHECK_TIMEOUT.as_secs()
        )),
    }
}

pub async fn establish_connection(database_url: &str) -> ServiceResult<Arc<DbConnection>> {
    // Parse the database URL to extract host and port
    let (host, port) = parse_database_url(database_url)
        .map_err(|e| ServiceError::Database(format!("Invalid database URL: {}", e)))?;

    // Check if the database is reachable before attempting to connect
    check_database_connectivity(&host, port)
        .await
        .map_err(ServiceError::Database)?;

    let max_conn: u32 = std::env::var("TEMPS_DB_MAX_CONNECTIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let min_conn: u32 = std::env::var("TEMPS_DB_MIN_CONNECTIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let acquire_timeout_secs: u64 = std::env::var("TEMPS_DB_ACQUIRE_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    let idle_timeout_secs: u64 = std::env::var("TEMPS_DB_IDLE_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600);

    let mut opt = ConnectOptions::new(database_url);
    opt.max_connections(max_conn)
        .min_connections(min_conn)
        .connect_timeout(Duration::from_secs(acquire_timeout_secs))
        .idle_timeout(Duration::from_secs(idle_timeout_secs))
        .sqlx_logging(false);

    // Connect with timeout
    let db = match timeout(CONNECTION_TIMEOUT, Database::connect(opt)).await {
        Ok(Ok(db)) => db,
        Ok(Err(e)) => {
            return Err(ServiceError::Database(format!(
                "Failed to connect to database: {}",
                e
            )));
        }
        Err(_) => {
            return Err(ServiceError::Database(format!(
                "Database connection timed out after {} seconds",
                CONNECTION_TIMEOUT.as_secs()
            )));
        }
    };

    // Self-heal `seaql_migrations` before running migrations. Operators
    // upgrading from earlier builds may have rows recorded for migrations
    // that have since been squashed and whose source files were removed.
    // Sea-ORM panics during the load phase when it sees an applied
    // version with no matching file, so we strip those rows first.
    if let Err(e) = cleanup_orphaned_migrations(&db).await {
        return Err(ServiceError::Database(format!(
            "Failed to clean up orphaned migration rows: {}",
            e
        )));
    }

    // Run migrations with timeout
    match timeout(MIGRATION_TIMEOUT, Migrator::up(&db, None)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            return Err(ServiceError::Database(format!(
                "Failed to run migrations: {}",
                e
            )));
        }
        Err(_) => {
            return Err(ServiceError::Database(format!(
                "Database migrations timed out after {} seconds",
                MIGRATION_TIMEOUT.as_secs()
            )));
        }
    }

    // Post-migration: backfill continuous aggregates that require CALL outside a transaction.
    // This is idempotent — refreshing an already-populated aggregate just updates it.
    run_post_migration_backfill(&db).await?;

    Ok(Arc::new(db))
}

/// Run post-migration backfill for continuous aggregates.
/// Strip rows from `seaql_migrations` whose `version` no longer matches a
/// migration shipped with this binary. Sea-ORM's loader panics with
/// `Migration file of version 'X' is missing, this migration has been
/// applied but its file is missing` when it sees an applied row with no
/// matching file — that breaks every upgrade across a release that
/// squashed or removed migrations.
///
/// Safe because Sea-ORM only uses `seaql_migrations` to track which
/// versions to skip on `up()`. The actual schema state lives in the DB
/// and is unchanged. If a row referenced a migration that built schema
/// the squash now creates idempotently (the typical case), the squash's
/// `IF NOT EXISTS` clauses make re-application a no-op. If a row
/// referenced something the new binary doesn't know about at all, the
/// state is preserved as-is.
///
/// Idempotent: a clean DB has nothing to delete and the function returns
/// immediately. A first-boot DB without `seaql_migrations` is also a
/// no-op (the table doesn't exist yet → Sea-ORM will create it).
pub async fn cleanup_orphaned_migrations(db: &DatabaseConnection) -> ServiceResult<()> {
    // Check that seaql_migrations exists before touching it. On a fresh
    // install the table won't exist yet — Sea-ORM creates it during the
    // first `up()` call.
    let exists_row = db
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT EXISTS (SELECT 1 FROM information_schema.tables \
             WHERE table_name = 'seaql_migrations') AS exists"
                .to_string(),
        ))
        .await
        .map_err(|e| {
            ServiceError::Database(format!(
                "Failed to probe seaql_migrations existence: {}",
                e
            ))
        })?;

    let table_exists: bool = match exists_row {
        Some(row) => row.try_get("", "exists").unwrap_or(false),
        None => false,
    };

    if !table_exists {
        debug!("seaql_migrations does not exist yet — skipping orphan cleanup");
        return Ok(());
    }

    // Build the canonical version list from the migrator's compiled-in
    // migration set. Anything in `seaql_migrations` that isn't in this
    // list is an orphan from a previous build.
    let known_versions: Vec<String> = Migrator::migrations()
        .iter()
        .map(|m| m.name().to_string())
        .collect();

    if known_versions.is_empty() {
        return Ok(());
    }

    // Count orphans first so we can log the action and skip the DELETE
    // when there's nothing to do (avoids log noise on every startup).
    let placeholders: String = (1..=known_versions.len())
        .map(|i| format!("${}", i))
        .collect::<Vec<_>>()
        .join(",");

    let count_sql = format!(
        "SELECT count(*) AS n FROM seaql_migrations WHERE version NOT IN ({})",
        placeholders
    );
    let values: Vec<sea_orm::Value> = known_versions
        .iter()
        .map(|v| sea_orm::Value::from(v.clone()))
        .collect();

    let count_row = db
        .query_one(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            &count_sql,
            values.clone(),
        ))
        .await
        .map_err(|e| {
            ServiceError::Database(format!(
                "Failed to count orphaned migration rows: {}",
                e
            ))
        })?;

    let orphan_count: i64 = count_row
        .and_then(|row| row.try_get::<i64>("", "n").ok())
        .unwrap_or(0);

    if orphan_count == 0 {
        debug!("No orphaned migration rows to clean up");
        return Ok(());
    }

    // Log the orphan list before deletion so operators see what was
    // removed if anything goes sideways.
    let list_sql = format!(
        "SELECT version FROM seaql_migrations \
         WHERE version NOT IN ({}) ORDER BY version",
        placeholders
    );
    if let Ok(rows) = db
        .query_all(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            &list_sql,
            values.clone(),
        ))
        .await
    {
        let names: Vec<String> = rows
            .iter()
            .filter_map(|r| r.try_get::<String>("", "version").ok())
            .collect();
        tracing::warn!(
            "Cleaning up {} orphaned migration row(s) from seaql_migrations: {}",
            orphan_count,
            names.join(", ")
        );
    }

    let delete_sql = format!(
        "DELETE FROM seaql_migrations WHERE version NOT IN ({})",
        placeholders
    );
    db.execute(Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Postgres,
        &delete_sql,
        values,
    ))
    .await
    .map_err(|e| {
        ServiceError::Database(format!(
            "Failed to delete orphaned migration rows: {}",
            e
        ))
    })?;

    Ok(())
}

///
/// `CALL refresh_continuous_aggregate()` cannot run inside a transaction block,
/// but Sea-ORM migrations run inside transactions. This function runs the backfill
/// after the migration transaction has been committed.
///
/// This is idempotent — refreshing an already-populated aggregate is a no-op for
/// unchanged data, so it's safe to call on every startup.
async fn run_post_migration_backfill(db: &DatabaseConnection) -> ServiceResult<()> {
    // Check if the events_hourly continuous aggregate exists before attempting backfill
    let check_sql = r#"
        SELECT EXISTS (
            SELECT 1 FROM timescaledb_information.continuous_aggregates
            WHERE view_name = 'events_hourly'
        ) as exists
    "#;

    let row = db
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            check_sql,
        ))
        .await
        .map_err(|e| {
            ServiceError::Database(format!(
                "Failed to check for events_hourly aggregate: {}",
                e
            ))
        })?;

    if let Some(row) = row {
        let exists: bool = row.try_get("", "exists").unwrap_or(false);
        if exists {
            debug!("Backfilling events_hourly continuous aggregate");
            let backfill_sql =
                "CALL refresh_continuous_aggregate('events_hourly', NULL, NOW() - INTERVAL '1 hour')";
            if let Err(e) = db
                .execute(Statement::from_string(
                    sea_orm::DatabaseBackend::Postgres,
                    backfill_sql,
                ))
                .await
            {
                // Log but don't fail startup — the refresh policy will catch up
                tracing::warn!(
                    "Failed to backfill events_hourly aggregate (refresh policy will catch up): {}",
                    e
                );
            } else {
                debug!("events_hourly continuous aggregate backfill complete");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_database_url_basic() {
        let (host, port) = parse_database_url("postgres://user:pass@localhost:5432/db").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 5432);
    }

    #[test]
    fn test_parse_database_url_default_port() {
        let (host, port) = parse_database_url("postgres://user:pass@localhost/db").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 5432);
    }

    #[test]
    fn test_parse_database_url_custom_port() {
        let (host, port) =
            parse_database_url("postgresql://user:pass@db.example.com:5433/mydb").unwrap();
        assert_eq!(host, "db.example.com");
        assert_eq!(port, 5433);
    }

    #[test]
    fn test_parse_database_url_with_query_params() {
        let (host, port) =
            parse_database_url("postgres://user:pass@localhost:5432/db?sslmode=require").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 5432);
    }

    #[test]
    fn test_parse_database_url_no_credentials() {
        let (host, port) = parse_database_url("postgres://localhost:5432/db").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 5432);
    }

    #[test]
    fn test_parse_database_url_ipv6() {
        let (host, port) = parse_database_url("postgres://user:pass@[::1]:5432/db").unwrap();
        assert_eq!(host, "::1");
        assert_eq!(port, 5432);
    }

    #[test]
    fn test_parse_database_url_ipv6_default_port() {
        let (host, port) = parse_database_url("postgres://user:pass@[::1]/db").unwrap();
        assert_eq!(host, "::1");
        assert_eq!(port, 5432);
    }

    #[test]
    fn test_parse_database_url_invalid_scheme() {
        let result = parse_database_url("mysql://user:pass@localhost:3306/db");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_database_url_special_chars_in_password() {
        // Password with @ symbol should still work (using rfind for @)
        let (host, port) = parse_database_url("postgres://user:p%40ss@localhost:5432/db").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 5432);
    }
}
