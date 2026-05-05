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
        let stmt = raw.trim();
        if stmt.is_empty() || stmt.starts_with("--") {
            // Skip empty fragments and pure-comment fragments.
            // Inline comments inside a real statement still travel with it.
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
