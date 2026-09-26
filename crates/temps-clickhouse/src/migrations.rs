// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Generic ClickHouse migration runner.
//!
//! Mirrors the pattern each of the four vendored copies (`temps-otel`,
//! `temps-analytics-backend`, `temps-proxy`, `temps-metrics`) already
//! implements independently:
//!
//! 1. Ensure the target database exists (`CREATE DATABASE IF NOT EXISTS`).
//! 2. Create the caller-named tracking table (`ReplacingMergeTree`).
//! 3. Read which migrations are already recorded (`SELECT … FINAL`).
//! 4. Apply each pending migration from the caller's `migrations` list, in
//!    order.
//! 5. Record success per migration.
//!
//! Unlike the vendored copies, the tracking table name is a parameter
//! (`tracking_table`) so multiple independent migration lists — one per
//! ClickHouse-backed subsystem — can share one database without colliding on
//! `_temps_ch_migrations`.
//!
//! Failures fail-fast: ClickHouse DDL is not transactional, so partial
//! rollback is not attempted. See the crate-level docs for why migration
//! lists must be append-only.

use crate::ClickHouseError;

/// One migration: a stable name (tracking-table row key) and the SQL body.
///
/// `name` must never be reused for different SQL, and existing entries must
/// never be removed or reordered — see the crate-level docs.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub name: &'static str,
    pub sql: &'static str,
}

/// Result of one migration run, useful for startup logging.
#[derive(Debug, Default, Clone)]
pub struct MigrationReport {
    pub applied: Vec<String>,
    pub skipped: usize,
}

/// Validate that `name` is a safe ClickHouse database identifier.
///
/// Allows only `[A-Za-z0-9_]` — the characters ClickHouse accepts without
/// quoting. Backticks, semicolons, spaces, and any other characters that
/// could break the `CREATE DATABASE IF NOT EXISTS \`{name}\`` statement are
/// rejected. Returns an error if the name is empty or contains an invalid
/// character.
pub(crate) fn validate_database_name(name: &str) -> Result<(), ClickHouseError> {
    if name.is_empty() {
        return Err(ClickHouseError::InvalidDatabaseName(name.to_string()));
    }
    if name.chars().any(|c| !c.is_ascii_alphanumeric() && c != '_') {
        return Err(ClickHouseError::InvalidDatabaseName(name.to_string()));
    }
    Ok(())
}

/// Apply all pending migrations idempotently.
///
/// `database` is used to issue the `CREATE DATABASE IF NOT EXISTS` statement
/// before any other DDL; `client` must already be configured with the target
/// database so subsequent DDL lands in the right place. `tracking_table` is
/// the name of the per-subsystem tracking table this call owns — pass a
/// distinct name per independent `migrations` list so multiple subsystems
/// sharing one ClickHouse database don't collide on tracking rows.
///
/// The function is cheap on repeated calls: the tracking-table read filters
/// to applied migrations and the loop body is skipped for each.
pub async fn apply_migrations(
    client: &clickhouse::Client,
    database: &str,
    tracking_table: &str,
    migrations: &[Migration],
) -> Result<MigrationReport, ClickHouseError> {
    use clickhouse::Row;
    use serde::Deserialize;

    // 0. Validate the identifiers before interpolating them into DDL.
    validate_database_name(database)?;
    validate_database_name(tracking_table)?;

    // 1. Ensure the target database exists.
    //
    // The passed-in `client` is scoped to the target database, and ClickHouse
    // rejects requests whose session database does not yet exist — including
    // the CREATE DATABASE itself. Run this one statement on a clone scoped to
    // the always-present `default` database so the target can be bootstrapped.
    let bootstrap = client.clone().with_database("default");
    let create_db_sql = format!("CREATE DATABASE IF NOT EXISTS `{database}`");
    bootstrap
        .query(&create_db_sql)
        .execute()
        .await
        .map_err(ClickHouseError::Query)?;

    // 2. Ensure the migration tracking table exists.
    let tracking_ddl = format!(
        r#"CREATE TABLE IF NOT EXISTS `{tracking_table}`
(
    name        String,
    applied_at  DateTime64(3, 'UTC') DEFAULT now64(),
    _version    UInt64 DEFAULT toUnixTimestamp64Milli(now64())
)
ENGINE = ReplacingMergeTree(_version)
ORDER BY name"#
    );
    execute_multi(client, &tracking_ddl).await?;

    // 3. Read which migrations are already applied.
    #[derive(Row, Deserialize)]
    struct AppliedRow {
        name: String,
    }

    let applied: Vec<String> = client
        .query(&format!("SELECT name FROM `{tracking_table}` FINAL"))
        .fetch_all::<AppliedRow>()
        .await
        .map_err(ClickHouseError::Query)?
        .into_iter()
        .map(|r| r.name)
        .collect();

    let mut report = MigrationReport::default();

    // 4. Apply each pending migration.
    for migration in migrations {
        if applied.iter().any(|n| n == migration.name) {
            tracing::debug!(
                migration = migration.name,
                tracking_table,
                "ClickHouse migration already applied — skipping"
            );
            report.skipped += 1;
            continue;
        }

        tracing::info!(
            migration = migration.name,
            tracking_table,
            "applying ClickHouse migration"
        );
        execute_multi(client, migration.sql)
            .await
            .map_err(|e| ClickHouseError::Migration {
                name: migration.name.to_string(),
                source: e.into_query_error(),
            })?;

        // 5. Record success.
        //
        // If this INSERT fails after the DDL succeeded, the next runner pass
        // will see the tables already exist (CREATE IF NOT EXISTS) and
        // re-record without re-executing DDL. Idempotent.
        client
            .query(&format!("INSERT INTO `{tracking_table}` (name) VALUES (?)"))
            .bind(migration.name)
            .execute()
            .await
            .map_err(ClickHouseError::Query)?;

        report.applied.push(migration.name.to_string());
    }

    Ok(report)
}

/// Execute a multi-statement SQL blob against ClickHouse.
///
/// ClickHouse's HTTP endpoint accepts only one statement per request. We
/// strip every whole-line `--` comment from the blob FIRST, then split on
/// `;`. Order matters: a `;` inside a comment (e.g. prose like "no rollup
/// MVs;") must not become a statement boundary — splitting first would slice
/// a CREATE TABLE in half. Inline `--` comments after code on the same line
/// are left intact.
async fn execute_multi(client: &clickhouse::Client, sql: &str) -> Result<(), ClickHouseError> {
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
            .map_err(ClickHouseError::Query)?;
    }
    Ok(())
}

/// Remove every whole-line `--` comment (or blank line) across the whole
/// blob, before statement splitting. A line counts as a comment if, after
/// trimming leading whitespace, it starts with `--`. Lines with code followed
/// by a trailing `--` comment are kept verbatim (ClickHouse accepts them).
fn strip_whole_line_comments(sql: &str) -> String {
    sql.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.is_empty() && !trimmed.starts_with("--")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── strip_whole_line_comments / statement splitting ─────────────────────

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
        let sql =
            "-- header\n\n-- more header\n\nCREATE TABLE baz (id Int64) ENGINE = MergeTree ORDER BY id";
        let stripped = strip_whole_line_comments(sql).trim().to_string();
        assert!(stripped.starts_with("CREATE TABLE baz"));
    }

    /// Regression: a `;` inside a comment must NOT split the following
    /// statement in half.
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

    #[test]
    fn splits_multiple_statements_on_semicolon() {
        let sql = "CREATE TABLE a (id Int64) ENGINE = MergeTree ORDER BY id;\n\
                   CREATE TABLE b (id Int64) ENGINE = MergeTree ORDER BY id;";
        let cleaned = strip_whole_line_comments(sql);
        let stmts: Vec<&str> = cleaned
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(stmts.len(), 2);
    }

    // ── validate_database_name ───────────────────────────────────────────────

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
        assert!(validate_database_name("otel;DROP TABLE spans--").is_err());
    }

    #[test]
    fn space_in_database_name_is_rejected() {
        assert!(validate_database_name("my database").is_err());
    }

    #[test]
    fn hyphen_in_database_name_is_rejected() {
        assert!(validate_database_name("my-db").is_err());
    }

    // ── apply_migrations integration test (real ClickHouse, opt-in) ─────────

    /// Runs against a real ClickHouse server when `TEMPS_TEST_CLICKHOUSE_URL`
    /// is set; skips gracefully otherwise. Does not spin up testcontainers.
    #[tokio::test]
    async fn apply_migrations_against_real_clickhouse() {
        let Ok(url) = std::env::var("TEMPS_TEST_CLICKHOUSE_URL") else {
            eprintln!("skipping: TEMPS_TEST_CLICKHOUSE_URL not set");
            return;
        };
        let user =
            std::env::var("TEMPS_TEST_CLICKHOUSE_USER").unwrap_or_else(|_| "default".to_string());
        let password = std::env::var("TEMPS_TEST_CLICKHOUSE_PASSWORD").unwrap_or_default();
        let database = std::env::var("TEMPS_TEST_CLICKHOUSE_DATABASE")
            .unwrap_or_else(|_| "temps_clickhouse_test".to_string());

        let cfg = crate::ClickHouseConfig::new(url, database.clone(), user, password);
        let client = cfg.client();

        let migrations = [Migration {
            name: "0001_test_table",
            sql: "CREATE TABLE IF NOT EXISTS test_table (id UInt64) ENGINE = MergeTree ORDER BY id",
        }];

        let report = apply_migrations(&client, &database, "_temps_ch_test_migrations", &migrations)
            .await
            .expect("first run should apply the migration");
        assert_eq!(report.applied, vec!["0001_test_table".to_string()]);
        assert_eq!(report.skipped, 0);

        let report_again =
            apply_migrations(&client, &database, "_temps_ch_test_migrations", &migrations)
                .await
                .expect("second run should be a no-op");
        assert!(report_again.applied.is_empty());
        assert_eq!(report_again.skipped, 1);

        let version = crate::server_version(&client)
            .await
            .expect("server_version should succeed");
        assert!(version.major > 0);
    }
}
