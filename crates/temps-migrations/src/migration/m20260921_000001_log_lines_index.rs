// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `log_lines_index`: the Postgres/TimescaleDB backend for the Global Logs
//! per-line index (ADR-047).
//!
//! Mirrors the ClickHouse `log_lines_index` table
//! (`temps-log-aggregator/migrations/clickhouse/0001_log_lines_index.sql`)
//! column for column so the two backends answer the same analytics
//! questions with the same semantics. One row per sealed log line; the
//! message bytes are **not** here — they live once in the chunk object
//! (ADR-046) and `(chunk_seq, line_index)` points back at them.
//!
//! This is the index an operator gets without running ClickHouse. It is a
//! real hypertable with compression and a retention policy, so a
//! single-node install keeps a bounded, compressed index instead of an
//! unbounded heap.
//!
//! **Guarded on the extension.** Every TimescaleDB-specific statement lives
//! in a `DO` block that checks `pg_extension` first, so an install on plain
//! PostgreSQL still gets a working (if uncompressed, unpartitioned) table
//! rather than a failed migration.

use sea_orm::DatabaseBackend;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() != DatabaseBackend::Postgres {
            return Ok(());
        }
        manager
            .get_connection()
            .execute_unprepared(
                r#"
-- ============================================================
-- Per-line index of sealed log chunks (ADR-047 §2)
-- ============================================================
--
-- Column-for-column mirror of the ClickHouse table. Levels use the same
-- encoding as `chunk::level_to_u8` (0 trace … 4 error) and `stream` is
-- 0 = stdout / 1 = stderr, so a row written by either backend decodes the
-- same way.
--
-- `status_code` is INT2 rather than INT4: the writer clamps the parsed
-- value, and no HTTP status needs more than three digits. `duration_ms` is
-- REAL to match ClickHouse's Float32 — the index only filters and
-- aggregates durations, the chunk keeps whatever the line said.
CREATE TABLE IF NOT EXISTS log_lines_index (
    project_id          INT4        NOT NULL,
    -- 0, never NULL, is "no external service". Matches the ClickHouse
    -- sentinel so `external_service_id = 0` means the same thing in both
    -- backends' scope predicates (the manifest table uses NULL, which is
    -- why the scope fragment differs between the two by design).
    external_service_id INT4        NOT NULL DEFAULT 0,
    env                 TEXT        NOT NULL DEFAULT '',
    service             TEXT        NOT NULL DEFAULT '',
    deploy_id           INT4        NOT NULL DEFAULT 0,
    container_id        TEXT        NOT NULL DEFAULT '',
    node_id             INT4        NOT NULL DEFAULT 0,

    -- Full microsecond precision (the ClickHouse index rounds to ms to save
    -- 3 B/line; Postgres timestamptz is 8 B either way, so there is nothing
    -- to gain from rounding and the keyset pagination gets exact ties).
    ts                  TIMESTAMPTZ NOT NULL,
    level               INT2        NOT NULL DEFAULT 2,
    stream              INT2        NOT NULL DEFAULT 0,

    chunk_seq           INT8        NOT NULL,
    line_index          INT4        NOT NULL,

    -- Canonical ("well-known") attributes in fixed columns (ADR-047 §3):
    -- a filter on one of these never touches the JSONB bag.
    trace_id            TEXT        NOT NULL DEFAULT '',
    span_id             TEXT        NOT NULL DEFAULT '',
    request_id          TEXT        NOT NULL DEFAULT '',
    status_code         INT2        NOT NULL DEFAULT 0,
    http_method         TEXT        NOT NULL DEFAULT '',
    http_route          TEXT        NOT NULL DEFAULT '',
    duration_ms         REAL        NOT NULL DEFAULT 0,

    -- Everything else the parser extracted, minus the canonical keys above
    -- (they are not stored twice). Capped at ingest: <= 32 keys per line.
    attrs               JSONB       NOT NULL DEFAULT '{}'::jsonb,

    -- Operator-promoted attribute keys. NULL until a key is promoted into
    -- the slot; the key -> slot mapping lives in Postgres alongside the
    -- ClickHouse one, so promoting a key means the same thing on either
    -- backend.
    facet_attr_1  TEXT, facet_attr_2  TEXT, facet_attr_3  TEXT, facet_attr_4  TEXT,
    facet_attr_5  TEXT, facet_attr_6  TEXT, facet_attr_7  TEXT, facet_attr_8  TEXT,
    facet_attr_9  TEXT, facet_attr_10 TEXT, facet_attr_11 TEXT, facet_attr_12 TEXT,
    facet_attr_13 TEXT, facet_attr_14 TEXT, facet_attr_15 TEXT, facet_attr_16 TEXT,
    facet_attr_17 TEXT, facet_attr_18 TEXT, facet_attr_19 TEXT, facet_attr_20 TEXT
);

-- The line's identity, and the dedup key. This is what `ReplacingMergeTree`
-- gives the ClickHouse backend for free, and it is not optional: the seal
-- pipeline is idempotent by design and re-inserts a chunk whenever
-- `mark_indexed` fails after a successful insert, whenever a batch fails
-- mid-chunk and is retried, whenever the backend flaps, and whenever the
-- compactor re-indexes (see `services/reindexer.rs`). Without a unique key
-- each of those double-counts every facet, histogram and aggregate for the
-- life of the row, silently. Every insert is `ON CONFLICT DO NOTHING`.
--
-- Column order is chosen so this index also serves `forget_chunks`
-- (`DELETE ... WHERE chunk_seq = ANY(...)`), which is why no separate
-- `(chunk_seq)` index exists.
--
-- `ts` is in the key because TimescaleDB requires the partition column in
-- every unique index on a hypertable. The three columns are exactly
-- `compress_segmentby` (chunk_seq) plus `compress_orderby` (ts, line_index),
-- which is also what Timescale requires for a unique index to survive
-- compression.
CREATE UNIQUE INDEX IF NOT EXISTS idx_log_lines_index_line
    ON log_lines_index (chunk_seq, line_index, ts);

-- The two shapes every analytics query has: scoped to a set of projects, or
-- to a set of external services, always over a time window. The service
-- index is partial: application lines (the vast majority) carry
-- external_service_id = 0 and are answered by the project index; indexing
-- the sentinel too measured 47 MB per 1.2M lines for nothing.
CREATE INDEX IF NOT EXISTS idx_log_lines_index_project_ts
    ON log_lines_index (project_id, ts DESC);
CREATE INDEX IF NOT EXISTS idx_log_lines_index_external_service_ts
    ON log_lines_index (external_service_id, ts DESC)
    WHERE external_service_id <> 0;

-- Trace/request correlation: partial, because the overwhelming majority of
-- lines carry neither and indexing '' would just be a fat duplicate of the
-- table.
CREATE INDEX IF NOT EXISTS idx_log_lines_index_trace
    ON log_lines_index (trace_id, ts DESC) WHERE trace_id <> '';
CREATE INDEX IF NOT EXISTS idx_log_lines_index_request
    ON log_lines_index (request_id, ts DESC) WHERE request_id <> '';

-- NOTE: no GIN index on `attrs`, deliberately — same reasoning as the
-- omitted GIN on `service_metrics.labels`
-- (m20260601_000001_create_service_metrics).
--
--   1. Write amplification. This table takes every log line of every
--      container on the instance; a GIN pending-list flush on that write
--      rate is a bottleneck long before any read benefits from it.
--   2. It would not serve the predicates we actually generate. A
--      `jsonb_path_ops` GIN answers containment (`@>`) only. Of the six
--      attribute operators the analytics builder emits, containment could
--      serve exactly one (`eq`) — `neq`, `prefix`, `>` and `<` all need the
--      extracted value, and `exists` needs the default `jsonb_ops` opclass,
--      not `jsonb_path_ops`.
--   3. The bounded window plus the scope indexes above already cap the scan,
--      and promoted facet slots are the supported answer for a hot attribute.
--
-- If a specific deployment proves it needs one, the targeted fix is a
-- partial expression index on that one key
-- (`((attrs->>'tenant')) WHERE attrs ? 'tenant'`), not a blanket GIN.
"#,
            )
            .await?;

        // TimescaleDB-specific: hypertable, compression, retention. Skipped
        // entirely when the extension is absent so a plain-PostgreSQL
        // install still ends up with a usable table.
        manager
            .get_connection()
            .execute_unprepared(
                r#"
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'timescaledb') THEN
        RAISE NOTICE 'timescaledb extension not installed; log_lines_index stays a plain table';
        RETURN;
    END IF;

    -- No default `(ts DESC)` index: chunks are already a day wide and every
    -- query carries a project/service scope that the composite indexes
    -- above serve; the default index measured 34 MB per 1.2M lines of pure
    -- duplication. Uncompressed rows cost ~220 B heap + ~100 B index; once
    -- the policy compresses a chunk it is ~15 B/row (measured, 2M lines).
    PERFORM create_hypertable(
        'log_lines_index', 'ts',
        chunk_time_interval    => INTERVAL '1 day',
        create_default_indexes => FALSE,
        if_not_exists          => TRUE
    );

    -- Compression segments by (project_id, service, chunk_seq).
    --
    -- `chunk_seq` in segmentby is the load-bearing part: from TimescaleDB
    -- 2.14 a DELETE whose WHERE touches only segmentby columns is executed
    -- directly against compressed chunks, dropping whole compressed batches
    -- instead of decompressing them. `forget_chunks` is exactly
    -- `DELETE ... WHERE chunk_seq = ANY(...)`, so compaction and purge stay
    -- cheap on compressed history. It also keeps a chunk's rows in one
    -- segment, which is what makes the ordering below compress well.
    --
    -- `chunk_seq` is coarse (thousands of lines per chunk), unlike the
    -- near-unique `trace_id` that had to be removed from `otel_spans`
    -- segmentby for exactly this reason.
    EXECUTE 'ALTER TABLE log_lines_index SET ('
         || 'timescaledb.compress, '
         || 'timescaledb.compress_segmentby = ''project_id, service, chunk_seq'', '
         || 'timescaledb.compress_orderby = ''ts DESC, line_index'')';

    -- Two hours: the index is written once at seal and then only read, so
    -- there is no reason to leave it uncompressed for days.
    PERFORM add_compression_policy('log_lines_index', INTERVAL '2 hours', if_not_exists => TRUE);

    -- Default window only. The aggregator realigns this with the instance's
    -- configured log retention on every retention tick
    -- (`LineIndexSink::set_retention_days`), which is the single source of
    -- truth (ADR-047 §6).
    PERFORM add_retention_policy('log_lines_index', INTERVAL '30 days', if_not_exists => TRUE);
END $$;
"#,
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() != DatabaseBackend::Postgres {
            return Ok(());
        }
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS log_lines_index CASCADE")
            .await?;
        Ok(())
    }
}
