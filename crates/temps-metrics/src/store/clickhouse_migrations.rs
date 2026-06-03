//! ClickHouse migration runner for the resource-metrics storage backend.
//!
//! Mirrors `temps-otel/src/storage/clickhouse/migrations.rs`:
//!
//! 1. Ensure the target database exists (`CREATE DATABASE IF NOT EXISTS`).
//! 2. Create the `_temps_ch_metrics_migrations` tracking table.
//! 3. Read which migrations are already recorded (`SELECT … FINAL`).
//! 4. Apply each pending migration from the `MIGRATIONS` list in order.
//! 5. Record success per migration.
//!
//! The runner is invoked at console startup only when the monitoring store is
//! ClickHouse AND `ServerConfig::is_clickhouse_enabled()` is true. Failures
//! fail-fast — CH DDL is not transactional, so partial rollback is not
//! attempted; the first write/read surfaces any error per-call.
//!
//! # Tracking-table name
//!
//! This runner uses `_temps_ch_metrics_migrations` — DISTINCT from the OTel
//! backend's `_temps_ch_otel_migrations`. Both run against the SAME ClickHouse
//! database (`otel` in the canonical deployment), so a shared tracking-table
//! name would collide and one backend would think the other's migrations were
//! already applied.

use crate::error::MetricsError;

/// One migration: a stable name (tracking row key) and the SQL body.
struct Migration {
    name: &'static str,
    sql: &'static str,
}

/// Ordered migration list. Add new entries to the bottom only.
const MIGRATIONS: &[Migration] = &[
    Migration {
        name: "0001_service_metrics",
        sql: include_str!("../../migrations/clickhouse/0001_service_metrics.sql"),
    },
    Migration {
        name: "0002_service_metrics_codecs",
        sql: include_str!("../../migrations/clickhouse/0002_service_metrics_codecs.sql"),
    },
];

/// SQL for the migration tracking table. Created on first run.
///
/// `ReplacingMergeTree(_version)` makes re-inserting the same `name`
/// idempotent (highest `_version` wins). Reads use `SELECT … FINAL` so
/// duplicates collapse before we check what is applied.
const TRACKING_DDL: &str = r#"CREATE TABLE IF NOT EXISTS _temps_ch_metrics_migrations
(
    name        String,
    applied_at  DateTime64(3, 'UTC') DEFAULT now64(),
    _version    UInt64 DEFAULT toUnixTimestamp64Milli(now64())
)
ENGINE = ReplacingMergeTree(_version)
ORDER BY name"#;

/// Result of one migration run, useful for startup logging.
#[derive(Debug, Default)]
pub struct MigrationReport {
    pub applied: Vec<&'static str>,
    pub skipped: Vec<&'static str>,
}

/// Validate that `name` is a safe ClickHouse database identifier.
///
/// Allows only `[A-Za-z0-9_]` — the characters ClickHouse accepts without
/// quoting. Backticks, semicolons, spaces, hyphens, and any other characters
/// that could break `CREATE DATABASE IF NOT EXISTS \`{name}\`` are rejected.
fn validate_database_name(name: &str) -> Result<(), MetricsError> {
    if name.is_empty() {
        return Err(MetricsError::ClickHouse {
            operation: "apply_migrations".to_string(),
            reason: "ClickHouse database name must not be empty".to_string(),
        });
    }
    if let Some(bad_char) = name
        .chars()
        .find(|c| !c.is_ascii_alphanumeric() && *c != '_')
    {
        return Err(MetricsError::ClickHouse {
            operation: "apply_migrations".to_string(),
            reason: format!(
                "ClickHouse database name '{name}' contains invalid character '{bad_char}'; \
                 only [A-Za-z0-9_] are permitted"
            ),
        });
    }
    Ok(())
}

/// Apply all pending resource-metrics ClickHouse migrations idempotently.
///
/// `database_name` is issued via `CREATE DATABASE IF NOT EXISTS` before any
/// other DDL. The `client` must already be configured with the target
/// database so subsequent DDL lands in the right place. Cheap on repeated
/// calls: the tracking-table read filters to applied migrations.
pub async fn apply_migrations(
    client: &::clickhouse::Client,
    database_name: &str,
) -> Result<MigrationReport, MetricsError> {
    use ::clickhouse::Row;
    use serde::Deserialize;

    // 0. Validate the database name before interpolating it into DDL.
    validate_database_name(database_name)?;

    // 1. Ensure the target database exists. The passed-in `client` is scoped to
    //    the target database, and CH rejects requests whose session database
    //    does not yet exist — including the CREATE DATABASE itself. So run this
    //    one statement on a clone scoped to the always-present `default` db.
    let bootstrap = client.clone().with_database("default");
    let create_db_sql = format!("CREATE DATABASE IF NOT EXISTS `{database_name}`");
    bootstrap
        .query(&create_db_sql)
        .execute()
        .await
        .map_err(|e| MetricsError::ClickHouse {
            operation: "apply_migrations (create database)".to_string(),
            reason: format!("CREATE DATABASE IF NOT EXISTS `{database_name}` failed: {e}"),
        })?;

    // 2. Ensure the migration tracking table exists.
    execute_multi(client, TRACKING_DDL).await?;

    // 3. Read which migrations are already applied.
    #[derive(Row, Deserialize)]
    struct AppliedRow {
        name: String,
    }

    let applied: Vec<String> = client
        .query("SELECT name FROM _temps_ch_metrics_migrations FINAL")
        .fetch_all::<AppliedRow>()
        .await
        .map_err(|e| MetricsError::ClickHouse {
            operation: "apply_migrations (read tracking table)".to_string(),
            reason: e.to_string(),
        })?
        .into_iter()
        .map(|r| r.name)
        .collect();

    let mut report = MigrationReport::default();

    // 4. Apply each pending migration.
    for migration in MIGRATIONS {
        if applied.iter().any(|n| n == migration.name) {
            tracing::debug!(
                migration = migration.name,
                "ch-metrics migration already applied — skipping"
            );
            report.skipped.push(migration.name);
            continue;
        }

        tracing::info!(migration = migration.name, "applying ch-metrics migration");
        execute_multi(client, migration.sql).await?;

        // 5. Record success. Idempotent: if this INSERT fails after the DDL
        //    succeeded, the next pass sees the tables already exist
        //    (CREATE IF NOT EXISTS) and re-records without re-executing DDL.
        client
            .query("INSERT INTO _temps_ch_metrics_migrations (name) VALUES (?)")
            .bind(migration.name)
            .execute()
            .await
            .map_err(|e| MetricsError::ClickHouse {
                operation: "apply_migrations (record applied)".to_string(),
                reason: format!("failed to record migration `{}`: {e}", migration.name),
            })?;

        report.applied.push(migration.name);
    }

    Ok(report)
}

/// Execute a multi-statement SQL blob against ClickHouse.
///
/// ClickHouse's HTTP endpoint accepts only one statement per request. We first
/// strip every whole-line `--` comment from the blob, THEN split on `;`. Doing
/// it in that order is essential: a `;` appearing inside a comment (e.g. a
/// benchmark note like "no rollup MVs;") must never become a statement
/// boundary — splitting first would slice a CREATE TABLE in half. Inline `--`
/// comments after code on the same line are left intact (ClickHouse parses
/// them fine within a statement).
async fn execute_multi(client: &::clickhouse::Client, sql: &str) -> Result<(), MetricsError> {
    let cleaned = strip_whole_line_comments(sql);
    for raw in cleaned.split(';') {
        let stmt = raw.trim();
        if stmt.is_empty() {
            continue;
        }
        client
            .query(stmt)
            .execute()
            .await
            .map_err(|e| MetricsError::ClickHouse {
                operation: "apply_migrations (DDL)".to_string(),
                reason: format!("{e}\nstatement: {}", truncate(stmt, 200)),
            })?;
    }
    Ok(())
}

/// Remove every line that is entirely a `--` comment (or blank), across the
/// whole blob, before statement splitting. A line counts as a comment line if,
/// after trimming leading whitespace, it starts with `--`. Lines with code
/// followed by a trailing `--` comment are kept verbatim (the comment stays in
/// the statement body, which ClickHouse accepts).
fn strip_whole_line_comments(sql: &str) -> String {
    sql.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.is_empty() && !trimmed.starts_with("--")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_database_names_pass() {
        assert!(validate_database_name("otel").is_ok());
        assert!(validate_database_name("otel_metrics").is_ok());
        assert!(validate_database_name("MyDb123").is_ok());
        assert!(validate_database_name("_private").is_ok());
    }

    #[test]
    fn empty_database_name_is_rejected() {
        assert!(validate_database_name("").is_err());
    }

    #[test]
    fn unsafe_database_names_are_rejected() {
        assert!(validate_database_name("otel`injection").is_err());
        assert!(validate_database_name("otel;DROP TABLE service_metrics--").is_err());
        assert!(validate_database_name("my database").is_err());
        assert!(validate_database_name("my-db").is_err());
    }

    #[test]
    fn strips_leading_comment_block_before_ddl() {
        let sql = "-- header comment\n\
                   -- another\n\
                   CREATE TABLE IF NOT EXISTS service_metrics (id Int64) ENGINE = MergeTree ORDER BY id";
        let stripped = strip_whole_line_comments(sql).trim().to_string();
        assert!(stripped.starts_with("CREATE TABLE IF NOT EXISTS service_metrics"));
    }

    /// Regression: a `;` inside a comment must NOT split the following statement.
    /// This is the exact bug that crashed the real service_metrics migration at
    /// boot (comment line "...rollup materialized views;" sliced the CREATE TABLE
    /// when the runner split on ";\n" before stripping comments).
    #[test]
    fn semicolon_inside_comment_does_not_split_statement() {
        let sql = "-- no rollup materialized views; query-time bucketing instead\n\
                   CREATE TABLE service_metrics (\n\
                   -- source_kind is database|deployment|container|node; validated\n\
                   source_kind String\n\
                   ) ENGINE = MergeTree ORDER BY source_kind";
        let cleaned = strip_whole_line_comments(sql);
        let stmts: Vec<&str> = cleaned
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(stmts.len(), 1, "must be ONE statement, got: {stmts:?}");
        assert!(stmts[0].starts_with("CREATE TABLE service_metrics"));
        assert!(stmts[0].contains("source_kind String"));
    }

    /// Every migration must yield at least one runnable statement after
    /// comment-stripping. Prevents a silent no-op being recorded as applied.
    #[test]
    fn every_migration_yields_at_least_one_runnable_statement() {
        for migration in MIGRATIONS {
            let cleaned = strip_whole_line_comments(migration.sql);
            let runnable: Vec<&str> = cleaned
                .split(';')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            assert!(
                !runnable.is_empty(),
                "migration {} produced no runnable statements after comment strip",
                migration.name
            );
        }
    }
}
