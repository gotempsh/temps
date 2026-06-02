//! ClickHouse migration runner for the OTel storage backend.
//!
//! Mirrors the pattern from `temps-analytics-backend/src/migrations.rs`:
//!
//! 1. Ensure the target database exists (`CREATE DATABASE IF NOT EXISTS`).
//! 2. Create the `_temps_ch_migrations` tracking table (ReplacingMergeTree).
//! 3. Read which migrations are already recorded (`SELECT … FINAL`).
//! 4. Apply each pending migration from the `MIGRATIONS` list in order.
//! 5. Record success per migration.
//!
//! The runner is invoked at plugin startup only when
//! `ServerConfig::is_clickhouse_enabled()` is true. Failures fail-fast —
//! CH DDL is not transactional, so partial rollback is not attempted.

use crate::error::OtelError;

/// One migration: a stable name (tracking row key) and the SQL body.
struct Migration {
    name: &'static str,
    sql: &'static str,
}

/// Ordered migration list. Add new entries to the bottom only.
const MIGRATIONS: &[Migration] = &[Migration {
    name: "0001_spans",
    sql: include_str!("../../../migrations/clickhouse/0001_spans.sql"),
}];

/// SQL for the migration tracking table. Created on first run.
///
/// `ReplacingMergeTree(_version)` means re-inserting the same `name` is
/// idempotent: the engine keeps the row with the highest `_version`. Using
/// `SELECT … FINAL` on reads ensures duplicates are collapsed before we
/// check what is applied.
const TRACKING_DDL: &str = r#"CREATE TABLE IF NOT EXISTS _temps_ch_otel_migrations
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
/// quoting. Backticks, semicolons, spaces, and any other characters that
/// could break the `CREATE DATABASE IF NOT EXISTS \`{name}\`` statement are
/// rejected. Returns an error if the name is empty or contains an invalid
/// character.
fn validate_database_name(name: &str) -> Result<(), OtelError> {
    if name.is_empty() {
        return Err(OtelError::Storage {
            message: "ClickHouse database name must not be empty".to_string(),
        });
    }
    if let Some(bad_char) = name
        .chars()
        .find(|c| !c.is_ascii_alphanumeric() && *c != '_')
    {
        return Err(OtelError::Storage {
            message: format!(
                "ClickHouse database name '{name}' contains invalid character '{bad_char}'; \
                 only [A-Za-z0-9_] are permitted"
            ),
        });
    }
    Ok(())
}

/// Apply all pending OTel ClickHouse migrations idempotently.
///
/// `database_name` is used to issue the `CREATE DATABASE IF NOT EXISTS`
/// statement before any other DDL. The `client` must already be configured
/// with the target database so subsequent DDL lands in the right place.
///
/// The function is cheap on repeated calls: the tracking-table read filters
/// to applied migrations and the loop body is skipped for each.
pub async fn apply_migrations(
    client: &::clickhouse::Client,
    database_name: &str,
) -> Result<MigrationReport, OtelError> {
    use ::clickhouse::Row;
    use serde::Deserialize;

    // 0. Validate the database name before interpolating it into DDL.
    validate_database_name(database_name)?;

    // 1. Ensure the target database exists.
    //
    // We build a temporary client without a database binding so the
    // `CREATE DATABASE` statement is valid from any context. The original
    // `client` already has `.with_database(database_name)` configured for
    // all subsequent DDL.
    //
    // The URL is extracted from the client by cloning and overriding with
    // no database; we reconstruct it from the same base URL.  Since
    // `clickhouse::Client` does not expose the base URL we use a known-safe
    // workaround: run `CREATE DATABASE` via the already-configured client.
    // ClickHouse allows this statement regardless of the currently-selected
    // database — the `DATABASE` clause in the statement names the target
    // explicitly.
    let create_db_sql = format!("CREATE DATABASE IF NOT EXISTS `{database_name}`");
    client
        .query(&create_db_sql)
        .execute()
        .await
        .map_err(|e| OtelError::Storage {
            message: format!(
                "ClickHouse OTel: failed to CREATE DATABASE IF NOT EXISTS `{database_name}`: {e}"
            ),
        })?;

    // 2. Ensure the migration tracking table exists.
    execute_multi(client, TRACKING_DDL).await?;

    // 3. Read which migrations are already applied.
    #[derive(Row, Deserialize)]
    struct AppliedRow {
        name: String,
    }

    let applied: Vec<String> = client
        .query("SELECT name FROM _temps_ch_otel_migrations FINAL")
        .fetch_all::<AppliedRow>()
        .await
        .map_err(|e| OtelError::Storage {
            message: format!("ClickHouse OTel: failed to read migration tracking table: {e}"),
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
                "ch-otel migration already applied — skipping"
            );
            report.skipped.push(migration.name);
            continue;
        }

        tracing::info!(migration = migration.name, "applying ch-otel migration");
        execute_multi(client, migration.sql).await?;

        // 5. Record success.
        //
        // If this INSERT fails after the DDL succeeded, the next runner pass
        // will see the tables already exist (CREATE IF NOT EXISTS) and
        // re-record without re-executing DDL. Idempotent.
        client
            .query("INSERT INTO _temps_ch_otel_migrations (name) VALUES (?)")
            .bind(migration.name)
            .execute()
            .await
            .map_err(|e| OtelError::Storage {
                message: format!(
                    "ClickHouse OTel: failed to record migration `{}` as applied: {e}",
                    migration.name
                ),
            })?;

        report.applied.push(migration.name);
    }

    Ok(report)
}

/// Execute a multi-statement SQL blob against ClickHouse.
///
/// ClickHouse's HTTP endpoint accepts only one statement per request, so we
/// split on `;\n` (the same delimiter used by the analytics backend runner)
/// and execute each non-empty piece separately. Leading whole-line `--`
/// comments are stripped before the emptiness check so comment-only chunks
/// are silently skipped rather than sent as empty requests.
async fn execute_multi(client: &::clickhouse::Client, sql: &str) -> Result<(), OtelError> {
    for raw in sql.split(";\n") {
        let stmt = strip_leading_line_comments(raw).trim();
        if stmt.is_empty() {
            continue;
        }
        client
            .query(stmt)
            .execute()
            .await
            .map_err(|e| OtelError::Storage {
                message: format!(
                    "ClickHouse OTel DDL failed: {e}\nstatement: {}",
                    truncate(stmt, 200)
                ),
            })?;
    }
    Ok(())
}

/// Drop leading whole-line `--` comments and blank lines from a SQL chunk.
///
/// Stops at the first non-comment/non-blank line so inline `--` comments
/// inside a statement body are preserved unchanged.
fn strip_leading_line_comments(raw: &str) -> &str {
    let mut offset = 0;
    for line in raw.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with("--") {
            offset += line.len();
        } else {
            break;
        }
    }
    &raw[offset..]
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
    fn strips_leading_comment_block_before_ddl() {
        let sql = "-- Spans table: system-of-record for OTel traces.\n\
                   -- Sort key intentionally puts project_id first.\n\
                   CREATE TABLE IF NOT EXISTS spans (id Int64) ENGINE = MergeTree ORDER BY id";
        let stripped = strip_leading_line_comments(sql).trim();
        assert!(stripped.starts_with("CREATE TABLE IF NOT EXISTS spans"));
    }

    #[test]
    fn preserves_inline_comments_inside_statement() {
        let sql = "CREATE TABLE bar (\n\
                   -- column comment\n\
                   id Int64\n\
                   ) ENGINE = MergeTree ORDER BY id";
        let stripped = strip_leading_line_comments(sql);
        assert!(stripped.contains("-- column comment"));
    }

    #[test]
    fn returns_empty_for_pure_comment_chunk() {
        let sql = "-- just a comment\n-- and another\n";
        assert!(strip_leading_line_comments(sql).trim().is_empty());
    }

    #[test]
    fn handles_blank_lines_between_leading_comments() {
        let sql = "-- header\n\n-- more header\n\nCREATE TABLE baz (id Int64) ENGINE = MergeTree ORDER BY id";
        let stripped = strip_leading_line_comments(sql).trim();
        assert!(stripped.starts_with("CREATE TABLE baz"));
    }

    // ── validate_database_name tests ──────────────────────────────────────

    #[test]
    fn valid_database_names_pass() {
        assert!(validate_database_name("otel").is_ok());
        assert!(validate_database_name("otel_traces").is_ok());
        assert!(validate_database_name("MyDb123").is_ok());
        assert!(validate_database_name("_private").is_ok());
    }

    #[test]
    fn empty_database_name_is_rejected() {
        assert!(validate_database_name("").is_err());
    }

    #[test]
    fn backtick_in_database_name_is_rejected() {
        let err = validate_database_name("otel`injection").unwrap_err();
        assert!(err.to_string().contains('`'));
    }

    #[test]
    fn semicolon_in_database_name_is_rejected() {
        let err = validate_database_name("otel;DROP TABLE spans--").unwrap_err();
        assert!(err.to_string().contains(';'));
    }

    #[test]
    fn space_in_database_name_is_rejected() {
        let err = validate_database_name("my database").unwrap_err();
        assert!(err.to_string().contains(' '));
    }

    #[test]
    fn hyphen_in_database_name_is_rejected() {
        let err = validate_database_name("my-db").unwrap_err();
        assert!(err.to_string().contains('-'));
    }

    /// Every migration in MIGRATIONS must yield at least one runnable statement
    /// after comment-stripping. Prevents silent no-ops being recorded as applied.
    #[test]
    fn every_migration_yields_at_least_one_runnable_statement() {
        for migration in MIGRATIONS {
            let runnable: Vec<&str> = migration
                .sql
                .split(";\n")
                .map(|raw| strip_leading_line_comments(raw).trim())
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
