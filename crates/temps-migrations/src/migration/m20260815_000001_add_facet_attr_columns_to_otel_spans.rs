// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::Statement;
use sea_orm_migration::prelude::*;

/// Add the 20 pre-allocated `facet_attr_N` slot columns (and matching
/// indexes) to the TimescaleDB `otel_spans` hypertable — the TimescaleDB
/// counterpart of `0008_facet_slots.sql` on the ClickHouse `spans` table.
///
/// # Why this exists on TimescaleDB too
///
/// TimescaleDB is the *default* OTel storage backend (ClickHouse is
/// bring-your-own, selected only when `TEMPS_CLICKHOUSE_URL` is configured —
/// see `temps-otel::plugin::configure_routes`). Without this migration, the
/// facet feature would only ever work for the subset of installs that also
/// run a ClickHouse cluster, silently doing nothing for everyone else.
///
/// # Performance caveat: compression changes the story
///
/// `otel_spans` compresses chunks older than 7 days (see
/// `m20260225_000001_create_otel_tables`), and TimescaleDB's columnar
/// compression only gives true per-row indexing to `compress_segmentby`
/// columns (`project_id`, `service_name`, `trace_id`) and range-pruning to
/// `compress_orderby` (`start_time`). `facet_attr_N` is neither, so on
/// **compressed** chunks (everything older than 7 days, i.e. the majority of
/// the 90-day retention window by row count) the B-tree index below cannot
/// be used directly — TimescaleDB must decompress candidate batches (after
/// segment-pruning by project/service/trace) and filter in memory. That is
/// still faster than the `attributes->>'key'` JSON-extraction path it
/// replaces (segment pruning narrows the batch first), but it is NOT the
/// same order-of-magnitude bloom-filter win ClickHouse gets across its whole
/// retention window.
///
/// Net effect: full index-speed equality lookups on the most recent 7 days
/// (uncompressed chunks), degrading to decompress-then-filter (still an
/// improvement over today's full JSON scan) for the rest. Re-evaluate if
/// this gap matters enough to warrant promoting hot facet keys into
/// `compress_segmentby` instead (a much more invasive change: segmentby
/// changes require decompressing and recompressing every existing chunk).
///
/// # Cost on migrate
///
/// `ADD COLUMN ... NULL` (no default) is a metadata-only operation in
/// PostgreSQL/TimescaleDB — including on hypertables with compressed chunks —
/// so this step is instant regardless of table size. The `CREATE INDEX`
/// statements are NOT metadata-only: building each index requires a full
/// heap scan of every uncompressed chunk (compressed chunks are exempt — see
/// above) to confirm the all-NULL values, taking a SHARE lock that blocks
/// writes to `otel_spans` for the scan's duration. On a fresh install
/// `otel_spans` is empty, so this is instant. On a large existing
/// installation, `CREATE INDEX CONCURRENTLY` would avoid the write-blocking
/// lock, but `CONCURRENTLY` cannot run inside the transaction sea-orm wraps
/// migrations in (same constraint documented in
/// `m20260705_000001_add_visitor_unique_index`); operators with a
/// large/hot `otel_spans` table should run the `CREATE INDEX CONCURRENTLY`
/// statements from this file manually before migrating, or during a
/// maintenance window.
pub struct Migration;

const FACET_SLOTS: u8 = 20;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260815_000001_add_facet_attr_columns_to_otel_spans"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        let mut add_columns = String::from("ALTER TABLE otel_spans");
        for slot in 1..=FACET_SLOTS {
            if slot > 1 {
                add_columns.push(',');
            }
            add_columns.push_str(&format!(" ADD COLUMN IF NOT EXISTS facet_attr_{slot} TEXT"));
        }
        db.execute_unprepared(&add_columns).await?;

        // The column adds above are metadata-only and instant regardless of
        // table size. The indexes below are not — warn loudly at the point
        // an operator would actually see it (migration-run logs) rather than
        // leaving this only in a doc comment nobody reads until the ALTER is
        // already blocking. Only fires when otel_spans already has rows,
        // i.e. an existing/upgrading install, not a fresh one.
        let has_existing_rows = db
            .query_one(Statement::from_string(
                manager.get_database_backend(),
                "SELECT EXISTS (SELECT 1 FROM otel_spans LIMIT 1) AS has_rows",
            ))
            .await?
            .map(|row| row.try_get::<bool>("", "has_rows").unwrap_or(true))
            .unwrap_or(false);

        if has_existing_rows {
            tracing::warn!(
                "otel_spans already has data: building the 20 facet_attr_N indexes below will \
                 take a SHARE lock and block writes to otel_spans for the duration of a full \
                 heap scan of every uncompressed chunk (compressed chunks are exempt). On a \
                 large/hot table, consider stopping this migration now and running the \
                 `CREATE INDEX CONCURRENTLY idx_otel_spans_facet_attr_N ON otel_spans \
                 (facet_attr_N)` statements manually (from \
                 m20260815_000001_add_facet_attr_columns_to_otel_spans.rs) during a maintenance \
                 window instead, since CONCURRENTLY cannot run inside this migration's \
                 transaction."
            );
        }

        for slot in 1..=FACET_SLOTS {
            db.execute_unprepared(&format!(
                "CREATE INDEX IF NOT EXISTS idx_otel_spans_facet_attr_{slot} \
                 ON otel_spans (facet_attr_{slot})"
            ))
            .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        for slot in 1..=FACET_SLOTS {
            db.execute_unprepared(&format!(
                "DROP INDEX IF EXISTS idx_otel_spans_facet_attr_{slot}"
            ))
            .await?;
        }

        let mut drop_columns = String::from("ALTER TABLE otel_spans");
        for slot in 1..=FACET_SLOTS {
            if slot > 1 {
                drop_columns.push(',');
            }
            drop_columns.push_str(&format!(" DROP COLUMN IF EXISTS facet_attr_{slot}"));
        }
        db.execute_unprepared(&drop_columns).await?;

        Ok(())
    }
}
