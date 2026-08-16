use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Create the `otel_span_facets` table.
///
/// Each row maps an OTel attribute key (arbitrary string) to a pre-allocated
/// generic slot column in a `facet_attr_1`..`facet_attr_20` set of columns —
/// present on the ClickHouse `spans` table when ClickHouse is configured, and
/// on the TimescaleDB `otel_spans` hypertable otherwise (see
/// `m20260815_000001_add_facet_attr_columns_to_otel_spans`). The slot value is
/// 1-indexed and bounded by a CHECK constraint.
///
/// The `attribute_key` and `slot` columns both have UNIQUE constraints to
/// prevent two keys mapping to the same slot and to prevent the same key being
/// registered twice.
///
/// # Backfill status tracking
///
/// Registering a facet triggers a backfill of historical rows so
/// already-ingested spans get the attribute populated into their slot column,
/// not just spans ingested after registration. That backfill runs entirely
/// in the background, driven by a periodic poller (mirrors the existing
/// OTel retention-cleanup loop) rather than a task spawned from the HTTP
/// handler — spawning from the handler would silently lose progress on a
/// process restart, since the state would live only in memory. Instead,
/// every non-terminal facet is advanced by one bounded unit of work per
/// poller tick, and all progress lives in this row, so a restart just
/// resumes from whatever `status`/`backfill_cursor` was last persisted:
///
///   - ClickHouse backend: the poller issues the `ALTER TABLE ... UPDATE`
///     mutation (which returns immediately — `mutations_sync=0`), records
///     `ch_mutation_id`, and on later ticks polls `system.mutations` for
///     completion/failure.
///   - TimescaleDB backend: the poller runs one batch of a keyset-paginated
///     `UPDATE` per tick, advancing `backfill_cursor` (the last processed
///     `otel_spans.id`) and `rows_backfilled` (a cumulative count, for
///     progress display) until the cursor reaches the end.
///
/// `status` + `error_message` let the API and UI report real progress
/// instead of lying about completion, and `backend` records which storage
/// engine the facet was created against (a deployment only ever runs one,
/// but stamping it keeps the status fields unambiguous).
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"CREATE TABLE IF NOT EXISTS otel_span_facets (
                id               SERIAL PRIMARY KEY,
                attribute_key    VARCHAR(200) NOT NULL,
                slot             SMALLINT NOT NULL,
                created_by       INTEGER NULL,
                created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                -- 'pending' until the backfill task has issued its first
                -- mutation/batch; 'running' while in progress; terminal states
                -- 'completed' / 'failed'. 'deleting' means the facet was
                -- unregistered but the ClickHouse slot-clear mutation (or
                -- Postgres clear loop) hasn't finished yet — the row (and its
                -- slot) stays reserved so a concurrent create_facet can't reuse
                -- the slot while stale data might still be in it.
                status           VARCHAR(20) NOT NULL DEFAULT 'pending',
                backend          VARCHAR(20) NOT NULL DEFAULT 'clickhouse',
                ch_mutation_id   VARCHAR(64) NULL,
                -- Resume point for the TimescaleDB batched backfill/clear
                -- loop: the last-processed otel_spans.id. NULL means that
                -- backend's loop hasn't started yet. Unused (stays NULL) for
                -- the clickhouse backend, which tracks progress via
                -- ch_mutation_id + system.mutations instead.
                backfill_cursor  BIGINT NULL,
                rows_backfilled  BIGINT NOT NULL DEFAULT 0,
                error_message    TEXT NULL,
                updated_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                CONSTRAINT uq_otel_span_facets_key  UNIQUE (attribute_key),
                CONSTRAINT uq_otel_span_facets_slot UNIQUE (slot),
                CONSTRAINT ck_otel_span_facets_slot CHECK (slot BETWEEN 1 AND 20),
                CONSTRAINT ck_otel_span_facets_status CHECK (
                    status IN ('pending', 'running', 'completed', 'failed', 'deleting')
                ),
                CONSTRAINT ck_otel_span_facets_backend CHECK (
                    backend IN ('clickhouse', 'timescaledb')
                )
            )"#,
        )
        .await?;

        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_otel_span_facets_created_at \
             ON otel_span_facets (created_at DESC)",
        )
        .await?;

        // Polled by the background mutation-status loop for every facet whose
        // backfill or delete-clear is still in flight.
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_otel_span_facets_status \
             ON otel_span_facets (status) WHERE status IN ('running', 'deleting')",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP TABLE IF EXISTS otel_span_facets")
            .await?;
        Ok(())
    }
}
