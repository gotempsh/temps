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
const MIGRATIONS: &[Migration] = &[
    Migration {
        name: "0001_spans",
        sql: include_str!("../../../migrations/clickhouse/0001_spans.sql"),
    },
    Migration {
        name: "0002_spans_codecs",
        sql: include_str!("../../../migrations/clickhouse/0002_spans_codecs.sql"),
    },
    Migration {
        name: "0003_metrics",
        sql: include_str!("../../../migrations/clickhouse/0003_metrics.sql"),
    },
    Migration {
        name: "0004_retention_days",
        sql: include_str!("../../../migrations/clickhouse/0004_retention_days.sql"),
    },
    Migration {
        name: "0005_retention_ttl",
        sql: include_str!("../../../migrations/clickhouse/0005_retention_ttl.sql"),
    },
];

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
    // The passed-in `client` is scoped to the target database, and ClickHouse
    // rejects requests whose session database does not yet exist — including the
    // CREATE DATABASE itself. Run this one statement on a clone scoped to the
    // always-present `default` database so the target can be bootstrapped.
    let bootstrap = client.clone().with_database("default");
    let create_db_sql = format!("CREATE DATABASE IF NOT EXISTS `{database_name}`");
    bootstrap
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
/// ClickHouse's HTTP endpoint accepts only one statement per request. We strip
/// every whole-line `--` comment from the blob FIRST, then split on `;`. Order
/// matters: a `;` inside a comment (e.g. prose like "no rollup MVs;") must not
/// become a statement boundary — splitting first would slice a CREATE TABLE in
/// half. Inline `--` comments after code on the same line are left intact.
async fn execute_multi(client: &::clickhouse::Client, sql: &str) -> Result<(), OtelError> {
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
            .map_err(|e| OtelError::Storage {
                message: format!(
                    "ClickHouse OTel DDL failed: {e}\nstatement: {}",
                    truncate(stmt, 200)
                ),
            })?;
    }
    Ok(())
}

/// Remove every whole-line `--` comment (or blank line) across the whole blob,
/// before statement splitting. A line counts as a comment if, after trimming
/// leading whitespace, it starts with `--`. Lines with code followed by a
/// trailing `--` comment are kept verbatim (ClickHouse accepts them).
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
    fn strips_leading_comment_block_before_ddl() {
        let sql = "-- Spans table: system-of-record for OTel traces.\n\
                   -- Sort key intentionally puts project_id first.\n\
                   CREATE TABLE IF NOT EXISTS spans (id Int64) ENGINE = MergeTree ORDER BY id";
        let stripped = strip_whole_line_comments(sql).trim().to_string();
        assert!(stripped.starts_with("CREATE TABLE IF NOT EXISTS spans"));
    }

    #[test]
    fn drops_whole_line_comments_inside_statement() {
        // A whole-line comment between column defs is removed, but the
        // surrounding code is preserved and joins into one statement.
        let sql = "CREATE TABLE bar (\n\
                   -- column comment\n\
                   id Int64\n\
                   ) ENGINE = MergeTree ORDER BY id";
        let stripped = strip_whole_line_comments(sql);
        assert!(!stripped.contains("-- column comment"));
        assert!(stripped.contains("id Int64"));
        assert!(stripped.contains("CREATE TABLE bar"));
    }

    #[test]
    fn returns_empty_for_pure_comment_chunk() {
        let sql = "-- just a comment\n-- and another\n";
        assert!(strip_whole_line_comments(sql).trim().is_empty());
    }

    #[test]
    fn handles_blank_lines_between_leading_comments() {
        let sql = "-- header\n\n-- more header\n\nCREATE TABLE baz (id Int64) ENGINE = MergeTree ORDER BY id";
        let stripped = strip_whole_line_comments(sql).trim().to_string();
        assert!(stripped.starts_with("CREATE TABLE baz"));
    }

    /// Regression: a `;` inside a comment must NOT split the following statement
    /// in half. This is the exact bug that crashed the metrics migration at boot
    /// (splitting on ";\n" before stripping comments sliced the CREATE TABLE).
    #[test]
    fn semicolon_inside_comment_does_not_split_statement() {
        let sql = "-- no rollup MVs; query-time bucketing instead\n\
                   CREATE TABLE t (\n\
                   -- id is the key; nothing else\n\
                   id Int64\n\
                   ) ENGINE = MergeTree ORDER BY id";
        let cleaned = strip_whole_line_comments(sql);
        let stmts: Vec<&str> = cleaned
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(stmts.len(), 1, "must be ONE statement, got: {stmts:?}");
        assert!(stmts[0].starts_with("CREATE TABLE t"));
        assert!(stmts[0].contains("id Int64"));
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
