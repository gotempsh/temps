// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
//! `ServerConfig::is_clickhouse_enabled()` is true. A run fails fast — CH
//! DDL is not transactional, so partial rollback is not attempted — and the
//! plugin re-runs it with [`retry_until_applied`]; already-applied
//! migrations are skipped, so a re-run resumes where the last one stopped.

use crate::error::OtelError;
use crate::error::StorageErrorKind;
use crate::storage::clickhouse::ch_err_kind;

/// A table the OTel storage writes rows into.
///
/// Writes are held per table until the migrations its insert rows depend on
/// have applied, so a failed migration only blocks the writes it actually
/// breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteTable {
    Spans,
    Metrics,
    TraceRefs,
}

impl WriteTable {
    pub const ALL: [WriteTable; 3] = [
        WriteTable::Spans,
        WriteTable::Metrics,
        WriteTable::TraceRefs,
    ];

    /// The ClickHouse table name.
    pub fn table_name(self) -> &'static str {
        match self {
            WriteTable::Spans => "spans",
            WriteTable::Metrics => "metrics",
            WriteTable::TraceRefs => "cross_project_trace_refs",
        }
    }

    /// Position in [`Self::ALL`], for per-table state arrays.
    pub fn index(self) -> usize {
        match self {
            WriteTable::Spans => 0,
            WriteTable::Metrics => 1,
            WriteTable::TraceRefs => 2,
        }
    }
}

/// One migration: a stable name (tracking row key), the SQL body, and the
/// tables whose insert rows depend on it.
struct Migration {
    name: &'static str,
    sql: &'static str,
    /// Tables whose rows cannot be written until this has applied: it
    /// creates the table or adds a column the row type carries. Codec, TTL,
    /// index and projection changes do not change what an insert sends, so
    /// they list nothing. `every_table_or_column_change_declares_its_writes`
    /// keeps this honest.
    writes_need: &'static [WriteTable],
}

/// Ordered migration list. Add new entries to the bottom only.
const MIGRATIONS: &[Migration] = &[
    Migration {
        name: "0001_spans",
        sql: include_str!("../../../migrations/clickhouse/0001_spans.sql"),
        writes_need: &[WriteTable::Spans],
    },
    Migration {
        name: "0002_spans_codecs",
        sql: include_str!("../../../migrations/clickhouse/0002_spans_codecs.sql"),
        writes_need: &[],
    },
    Migration {
        name: "0003_metrics",
        sql: include_str!("../../../migrations/clickhouse/0003_metrics.sql"),
        writes_need: &[WriteTable::Metrics],
    },
    Migration {
        name: "0004_retention_days",
        sql: include_str!("../../../migrations/clickhouse/0004_retention_days.sql"),
        writes_need: &[WriteTable::Spans],
    },
    Migration {
        name: "0005_retention_ttl",
        sql: include_str!("../../../migrations/clickhouse/0005_retention_ttl.sql"),
        writes_need: &[],
    },
    Migration {
        name: "0006_trace_refs",
        sql: include_str!("../../../migrations/clickhouse/0006_trace_refs.sql"),
        writes_need: &[WriteTable::TraceRefs],
    },
    Migration {
        name: "0007_spans_recent_projection",
        sql: include_str!("../../../migrations/clickhouse/0007_spans_recent_projection.sql"),
        writes_need: &[],
    },
    Migration {
        name: "0008_facet_slots",
        sql: include_str!("../../../migrations/clickhouse/0008_facet_slots.sql"),
        writes_need: &[WriteTable::Spans],
    },
    Migration {
        name: "0009_metrics_retention_days",
        sql: include_str!("../../../migrations/clickhouse/0009_metrics_retention_days.sql"),
        writes_need: &[WriteTable::Metrics],
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
            // Operator configuration, not a transport failure.
            kind: StorageErrorKind::Precondition,
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
            // Operator configuration, not a transport failure.
            kind: StorageErrorKind::Precondition,
        });
    }
    Ok(())
}

/// Whether every migration `table`'s insert rows depend on is in `applied`.
pub fn writes_ready(table: WriteTable, applied: &std::collections::HashSet<&'static str>) -> bool {
    MIGRATIONS
        .iter()
        .filter(|m| m.writes_need.contains(&table))
        .all(|m| applied.contains(m.name))
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
    apply_migrations_reporting(client, database_name, |_| {}).await
}

/// [`apply_migrations`], calling `on_applied` with the name of every
/// migration known to be applied: those recorded by an earlier run as soon
/// as the tracking table is read, then each new one as it lands. A run that
/// fails part-way has still reported everything before the failure, which
/// is what lets the storage release the writes that failure does not break.
pub async fn apply_migrations_reporting(
    client: &::clickhouse::Client,
    database_name: &str,
    on_applied: impl Fn(&'static str),
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
            kind: ch_err_kind(&e),
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
            kind: ch_err_kind(&e),
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
            on_applied(migration.name);
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
                kind: ch_err_kind(&e),
                message: format!(
                    "ClickHouse OTel: failed to record migration `{}` as applied: {e}",
                    migration.name
                ),
            })?;

        report.applied.push(migration.name);
        on_applied(migration.name);
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
                kind: ch_err_kind(&e),
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

/// First delay between failed migration attempts; doubles up to
/// [`MIGRATION_RETRY_MAX_DELAY`].
pub const MIGRATION_RETRY_INITIAL_DELAY: std::time::Duration = std::time::Duration::from_secs(5);
/// Longest delay between failed migration attempts.
pub const MIGRATION_RETRY_MAX_DELAY: std::time::Duration = std::time::Duration::from_secs(60);

/// Run `apply` until it succeeds, backing off between failures.
///
/// The storage refuses writes until the migrations have applied, so giving
/// up after a failure would leave ingest refused (or, if writes were let
/// through, failing against an old schema) for the life of the process. The
/// commonest failure is ClickHouse not yet accepting connections when Temps
/// boots, which a later attempt survives; a failure that persists is
/// logged at `error` on every attempt so the operator sees it.
///
/// `first_attempt_done` is signalled after the first attempt, whatever its
/// outcome, so startup can wait for one attempt without waiting out the
/// backoff.
pub async fn retry_until_applied<T, F, Fut>(
    mut apply: F,
    first_attempt_done: Option<tokio::sync::oneshot::Sender<()>>,
) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, OtelError>>,
{
    let mut first_attempt_done = first_attempt_done;
    let mut delay = MIGRATION_RETRY_INITIAL_DELAY;
    let mut attempt: u32 = 1;
    loop {
        let result = apply().await;
        if let Some(done) = first_attempt_done.take() {
            // The receiver is gone once startup stopped waiting; nothing to do.
            let _ = done.send(());
        }
        match result {
            Ok(applied) => return applied,
            Err(error) => {
                tracing::error!(
                    attempt,
                    retry_in_secs = delay.as_secs(),
                    %error,
                    "ClickHouse OTel migrations failed; OTLP writes are answered 503 \
                     until they apply, retrying"
                );
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(MIGRATION_RETRY_MAX_DELAY);
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure() -> OtelError {
        OtelError::Storage {
            message: "ClickHouse migration 0009_metrics_retention_days failed: connection refused"
                .into(),
            kind: StorageErrorKind::ClickHouseNetwork,
        }
    }

    fn applied_through(last: &str) -> std::collections::HashSet<&'static str> {
        let end = MIGRATIONS
            .iter()
            .position(|m| m.name == last)
            .expect("migration exists");
        MIGRATIONS[..=end].iter().map(|m| m.name).collect()
    }

    #[test]
    fn a_failed_metrics_migration_does_not_hold_span_writes() {
        // 0009 failed; everything before it applied.
        let applied = applied_through("0008_facet_slots");
        assert!(writes_ready(WriteTable::Spans, &applied));
        assert!(writes_ready(WriteTable::TraceRefs, &applied));
        assert!(!writes_ready(WriteTable::Metrics, &applied));
    }

    #[test]
    fn a_failed_span_column_migration_holds_span_writes_only() {
        // 0008 (facet columns) failed on a fresh install.
        let applied = applied_through("0007_spans_recent_projection");
        assert!(!writes_ready(WriteTable::Spans, &applied));
        assert!(writes_ready(WriteTable::TraceRefs, &applied));
        // Metrics still need 0009, which comes after the failure.
        assert!(!writes_ready(WriteTable::Metrics, &applied));
    }

    #[test]
    fn every_write_is_ready_once_everything_applied() {
        let applied = applied_through("0009_metrics_retention_days");
        for table in WriteTable::ALL {
            assert!(writes_ready(table, &applied), "{table:?}");
        }
        assert!(WriteTable::ALL
            .iter()
            .enumerate()
            .all(|(i, t)| t.index() == i));
    }

    /// A migration that creates a written table or adds a column to one
    /// changes what an insert must send, so it has to hold that table's
    /// writes. Guards new migrations against forgetting `writes_need`.
    #[test]
    fn every_table_or_column_change_declares_its_writes() {
        for migration in MIGRATIONS {
            let sql = strip_whole_line_comments(migration.sql).to_lowercase();
            for table in WriteTable::ALL {
                let name = table.table_name();
                let creates = sql.contains(&format!("create table if not exists {name} "))
                    || sql.contains(&format!("create table if not exists {name}\n"))
                    || sql.contains(&format!("create table {name} "));
                let adds_column = sql.contains(&format!("alter table {name} add column"));
                if creates || adds_column {
                    assert!(
                        migration.writes_need.contains(&table),
                        "{} changes the shape of `{name}` rows but does not list {table:?} \
                         in writes_need",
                        migration.name
                    );
                }
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn retries_failed_migrations_with_backoff_until_they_apply() {
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let started = tokio::time::Instant::now();

        let applied = retry_until_applied(
            || {
                let attempts = attempts.clone();
                async move {
                    let n = attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    if n < 4 {
                        Err(failure())
                    } else {
                        Ok(n)
                    }
                }
            },
            None,
        )
        .await;

        assert_eq!(applied, 4, "must keep going until an attempt succeeds");
        // 5s + 10s + 20s between the three failures.
        assert_eq!(started.elapsed(), std::time::Duration::from_secs(35));
    }

    #[tokio::test(start_paused = true)]
    async fn backoff_is_capped() {
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let started = tokio::time::Instant::now();

        retry_until_applied(
            || {
                let attempts = attempts.clone();
                async move {
                    let n = attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    if n < 7 {
                        Err(failure())
                    } else {
                        Ok(())
                    }
                }
            },
            None,
        )
        .await;

        // 5 + 10 + 20 + 40, then capped: 60 + 60.
        assert_eq!(started.elapsed(), std::time::Duration::from_secs(195));
    }

    #[tokio::test(start_paused = true)]
    async fn signals_after_the_first_attempt_even_when_it_fails() {
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));

        let task = tokio::spawn({
            let attempts = attempts.clone();
            async move {
                retry_until_applied(
                    || {
                        let attempts = attempts.clone();
                        async move {
                            let n = attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                            if n < 3 {
                                Err(failure())
                            } else {
                                Ok(())
                            }
                        }
                    },
                    Some(done_tx),
                )
                .await
            }
        });

        done_rx.await.expect("first attempt must be signalled");
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
        task.await.expect("retry task must finish");
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

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
