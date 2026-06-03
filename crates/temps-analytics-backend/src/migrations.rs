//! ClickHouse migration runner.
//!
//! ClickHouse has no Sea-ORM equivalent and its dialect diverges enough that
//! sharing a Postgres migration crate would be more pain than help. This
//! module ships a minimal, embed-the-SQL approach: each `.sql` file in
//! `migrations/clickhouse/` is included via `include_str!`, applied in
//! lexical order, and tracked in a `_temps_ch_migrations` table on the
//! ClickHouse side so re-runs are idempotent.
//!
//! Statements within a file are split on `;` followed by newline. We do
//! *not* support stored statements that contain `;\n` inside a literal —
//! none of our DDL has that today. If we ever need it, switch to a real
//! parser; this is intentionally simple.
//!
//! The runner is invoked at startup by the analytics events plugin only
//! when `ServerConfig::is_clickhouse_enabled()` is true. Operators do not
//! need to rebuild Temps with a feature flag to enable ClickHouse.

use crate::error::AnalyticsBackendError;

/// One migration: a name (used for the tracking row) and the SQL body.
struct Migration {
    name: &'static str,
    sql: &'static str,
}

/// Migration list, in apply order. Add new entries to the bottom.
///
/// Order matters: `events_5m_mv` references `events`, so `events` must
/// land first. Lexical sort of file names (0001, 0002, …) keeps that
/// honest as long as we keep numbering them.
const MIGRATIONS: &[Migration] = &[
    Migration {
        name: "0001_events",
        sql: include_str!("../migrations/clickhouse/0001_events.sql"),
    },
    Migration {
        name: "0002_events_5m_mv",
        sql: include_str!("../migrations/clickhouse/0002_events_5m_mv.sql"),
    },
    Migration {
        name: "0003_sessions",
        sql: include_str!("../migrations/clickhouse/0003_sessions.sql"),
    },
    // 0004/0005 are forward DROP migrations. The original CREATE migrations
    // above are intentionally LEFT in the list: existing installs have them
    // recorded as applied, and removing them would desync the
    // `_temps_ch_migrations` tracking. On a fresh install the CREATE runs then
    // the DROP immediately removes it — a tiny, harmless churn — while on an
    // existing install only the DROP is pending. See each file's header for why
    // the object is being removed.
    Migration {
        name: "0004_drop_events_5m_mv",
        sql: include_str!("../migrations/clickhouse/0004_drop_events_5m_mv.sql"),
    },
    Migration {
        name: "0005_drop_sessions",
        sql: include_str!("../migrations/clickhouse/0005_drop_sessions.sql"),
    },
    Migration {
        name: "0006_events_codecs",
        sql: include_str!("../migrations/clickhouse/0006_events_codecs.sql"),
    },
];

/// SQL for the migration tracking table itself. Created on first run.
/// Uses ReplacingMergeTree so re-running the runner doesn't error on
/// duplicate inserts (we INSERT before checking, simpler than upsert).
const TRACKING_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS _temps_ch_migrations
(
    name        String,
    applied_at  DateTime64(3, 'UTC') DEFAULT now64(),
    _version    UInt64 DEFAULT toUnixTimestamp64Milli(now64())
)
ENGINE = ReplacingMergeTree(_version)
ORDER BY name;
"#;

/// Apply all pending migrations against the given ClickHouse client.
///
/// Idempotent: migrations already recorded in `_temps_ch_migrations` are
/// skipped. Failures fail-fast — we don't try to roll back partially
/// applied migrations because most CH DDL isn't transactional anyway.
pub async fn apply_migrations(
    client: &::clickhouse::Client,
) -> Result<MigrationReport, AnalyticsBackendError> {
    use ::clickhouse::Row;
    use serde::Deserialize;

    // 1. Ensure the tracking table exists.
    execute_multi(client, TRACKING_DDL).await?;

    // 2. Read which migrations are already applied. Use FINAL to collapse
    //    ReplacingMergeTree duplicates; the table is small (one row per
    //    migration) so this is cheap.
    #[derive(Row, Deserialize)]
    struct AppliedRow {
        name: String,
    }

    let applied: Vec<String> = client
        .query("SELECT name FROM _temps_ch_migrations FINAL")
        .fetch_all::<AppliedRow>()
        .await
        .map_err(|e| AnalyticsBackendError::BackendUnavailable {
            backend: "clickhouse".to_string(),
            reason: format!("failed to read migration tracking table: {e}"),
        })?
        .into_iter()
        .map(|r| r.name)
        .collect();

    let mut report = MigrationReport::default();

    // 3. Apply each pending migration.
    for migration in MIGRATIONS {
        if applied.iter().any(|n| n == migration.name) {
            tracing::debug!(migration = migration.name, "ch migration already applied");
            report.skipped.push(migration.name);
            continue;
        }

        tracing::info!(migration = migration.name, "applying ch migration");
        execute_multi(client, migration.sql).await?;

        // Record success. The insert is best-effort consistent — if it
        // fails after the DDL succeeded, the next runner pass will see
        // the tables already exist (CREATE IF NOT EXISTS) and re-record.
        client
            .query("INSERT INTO _temps_ch_migrations (name) VALUES (?)")
            .bind(migration.name)
            .execute()
            .await
            .map_err(|e| AnalyticsBackendError::BackendUnavailable {
                backend: "clickhouse".to_string(),
                reason: format!(
                    "failed to record migration {} as applied: {e}",
                    migration.name
                ),
            })?;

        report.applied.push(migration.name);
    }

    Ok(report)
}

/// Split a multi-statement SQL blob and execute each piece. ClickHouse
/// HTTP endpoint accepts only one statement per request, so we iterate.
async fn execute_multi(
    client: &::clickhouse::Client,
    sql: &str,
) -> Result<(), AnalyticsBackendError> {
    for raw in sql.split(";\n") {
        // Peel leading whole-line `--` comments off each chunk before
        // checking emptiness. Without this, a statement preceded by a
        // header comment block looks like a "comment fragment" and gets
        // silently skipped while still being recorded as applied — so
        // the DDL never lands and the fan-out worker fails on missing
        // tables. Inline `--` comments inside a statement are left
        // intact because CH parses them as end-of-line comments.
        let stmt = strip_leading_line_comments(raw).trim();
        if stmt.is_empty() {
            continue;
        }
        client.query(stmt).execute().await.map_err(|e| {
            AnalyticsBackendError::BackendUnavailable {
                backend: "clickhouse".to_string(),
                reason: format!(
                    "ch DDL failed: {e}\n\
                     statement: {}\n",
                    truncate(stmt, 200)
                ),
            }
        })?;
    }
    Ok(())
}

/// Drop leading whole-line `--` comments (and blank lines) from a SQL
/// chunk. Stops at the first non-comment line so embedded `--` inside
/// a statement is preserved.
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

/// Result of a migration run, useful for startup logging.
#[derive(Debug, Default)]
pub struct MigrationReport {
    pub applied: Vec<&'static str>,
    pub skipped: Vec<&'static str>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_leading_comment_block_before_ddl() {
        let sql = "-- Events table: derived analytical replica.\n\
                   -- Sort key intentionally puts project_id first.\n\
                   CREATE TABLE foo (id Int64) ENGINE = MergeTree ORDER BY id";
        let stripped = strip_leading_line_comments(sql).trim();
        assert!(stripped.starts_with("CREATE TABLE foo"));
    }

    #[test]
    fn preserves_inline_comments_after_first_real_line() {
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
        let stripped = strip_leading_line_comments(sql).trim();
        assert!(stripped.is_empty());
    }

    #[test]
    fn handles_blank_lines_between_comments() {
        let sql = "-- header\n\
                   \n\
                   -- more header\n\
                   \n\
                   CREATE TABLE baz (id Int64) ENGINE = MergeTree ORDER BY id";
        let stripped = strip_leading_line_comments(sql).trim();
        assert!(stripped.starts_with("CREATE TABLE baz"));
    }

    /// Regression guard: every shipped CH migration must contain a real
    /// DDL statement after the comment-stripping step. Catches a future
    /// migration that's entirely comments before we silently record it
    /// as applied with zero side-effect.
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
