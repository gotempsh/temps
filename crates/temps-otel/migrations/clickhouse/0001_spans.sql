-- OTel spans table: system-of-record for trace telemetry when ClickHouse is enabled.
--
-- Design decisions (ADR-016, Phase 0):
--
--   ENGINE: ReplacingMergeTree(_version)
--     OTLP exporters retry on transient failures; _version (Unix ms timestamp at
--     ingest) lets the engine deduplicate retried batches by keeping the highest
--     _version for each (project_id, trace_id, span_id) tuple.
--
--     NO READ PATH USES `FINAL` ANY MORE. This header used to say "use FINAL on
--     reads where dedup correctness is required (trace summaries list)" — that
--     advice is withdrawn. FINAL forces a read-time merge across parts and, worse
--     for us, disables projection use, which is what made both the Observe feed
--     (0007) and the trace-summaries list read a whole month to return 50 rows.
--
--     Dedup correctness is now the reader's job, and there are two valid ways:
--       * Duplicate-insensitive aggregates, where the answer cannot depend on
--         which copy wins (e.g. uniqExact(trace_id) for a distinct-trace count).
--       * An explicit `ORDER BY _version DESC LIMIT 1 BY (trace_id, span_id)`
--         subquery, which keeps exactly the row FINAL would have kept. Use this
--         whenever a duplicate's CONTENT could differ, not just its _version —
--         `ch_backfill_domains` writes Postgres-sourced copies with a lower
--         _version, so "retried rows are byte-identical" is not a safe global
--         assumption. Apply it after narrowing to a page, not across a window.
--     Hot-path ingest metrics still skip both; eventual consistency is fine there.
--
--   PARTITION BY toYYYYMM(start_time)
--     Monthly partitions allow ClickHouse to drop whole TTL-expired partitions in
--     one DROP PARTITION call instead of row-level deletes. Keeps the TTL mutation
--     cheap even at billions of rows.
--
--   ORDER BY (project_id, trace_id, span_id)
--     project_id first: every query is project-scoped so the primary index prefix
--     eliminates all other projects cheaply.
--     trace_id second: get_trace() lookups (fetch all spans for one trace_id) read
--     a contiguous block on disk.
--     span_id last: the dedup key — each span is unique within its trace.
--
--   TTL toDateTime(start_time) + INTERVAL 90 DAY
--     Matches the existing TimescaleDB retention policy. Partitions that fall
--     entirely outside the TTL window are dropped at the next merge cycle.
--
--   LowCardinality columns: service_name, service_version, deployment_environment,
--   kind, status_code — all have small, bounded value sets. LowCardinality stores
--   them as a dictionary + integer references, cutting storage and improving GROUP BY.
--
--   parent_span_id String DEFAULT ''
--     Root spans have no parent; empty string is the canonical sentinel (matches
--     ADR-016 spec). Rust ingest code maps Option<String> -> unwrap_or_default().
--
--   attributes String / events String
--     JSON-as-String. Avoids committing to a Map(String,String) shape early; the
--     Rust ingest layer serialises BTreeMap<String,String> / Vec<SpanEvent> via
--     serde_json. JSONExtract* functions handle ad-hoc attribute queries at read
--     time. If a dedicated attribute column proves necessary it can be added as a
--     materialized column in a later migration without a full table rewrite.
--
-- BENCHMARK RESULT (2026-06-02, ClickHouse 26.2.5, local single-node):
--   Dataset: 2,000,000 spans / 402,599 distinct traces / 10 projects.
--
--   query_trace_summaries (project-scoped GROUP BY trace_id + ORDER BY duration DESC):
--     Approach A — query-time GROUP BY on spans FINAL:  best 23ms, avg ~28ms
--     Approach B — AggregatingMergeTree MV + argMaxMerge: best 25ms, avg ~32ms
--
--   DECISION: query-time GROUP BY chosen (0001_spans.sql only; no MV migration).
--   Both approaches are well under the 100ms threshold at 400k traces.
--   The MV adds write amplification, a separate backfill step, and argMaxState
--   binary serialisation complexity with no measurable read benefit at this scale.
--   The ORDER BY (project_id, trace_id, span_id) primary index makes GROUP BY
--   trace_id a sequential scan of already-sorted compressed blocks — ClickHouse's
--   vectorised engine handles it in ~25ms without pre-aggregation.
--   Re-evaluate at >10M distinct traces if query time degrades.
--
-- RE-EVALUATED (2026-07-31, ClickHouse 24.8.14, 32-core host, 16 threads):
--   That re-evaluation trigger was reached and the conclusion above did NOT
--   hold. Dataset: 860,000,000 spans / 286,666,667 distinct traces / 30 days /
--   64.2 GiB. Warm, statistics.elapsed + rows_read from FORMAT JSON:
--
--                              1h window                24h window
--     single-stage + FINAL     9039 ms / 847.5M rows   10560 ms / 861.1M rows
--     two-stage + dedup          54 ms /   4.0M rows      613 ms /  32.4M rows
--
--     count, single-stage      6211 ms / 847.5M rows    7830 ms / 861.1M rows
--     count, uniqExact           18 ms /   1.2M rows      464 ms /  28.8M rows
--
--   "two-stage + dedup" is the shipped shape, INCLUDING the
--   `ORDER BY _version DESC LIMIT 1 BY (trace_id, span_id)` subquery that
--   replaces FINAL. Measured against the same query without that subquery, the
--   dedup is free (237 ms vs 244 ms on a 1h window): it only ever sees the few
--   hundred rows belonging to the page's traces, which is the entire reason it
--   is affordable where FINAL was not.
--
--   The rows_read column is the point: the single-stage form reads the same
--   ~850M rows whether the window is 1h or 24h, because neither the partition
--   key (month granularity) nor the primary key (trace_id is a random hash)
--   can prune on start_time. The fix was not pre-aggregation but the proj_recent
--   projection from 0007 plus a two-stage read that is narrow enough to use it —
--   see query_trace_summaries in storage/clickhouse/mod.rs.
--
--   An MV is still not indicated: the two-stage read is proportional to the
--   window rather than to table size, so it does not degrade as the table grows.
CREATE TABLE IF NOT EXISTS spans
(
    -- Tenant + deployment context
    project_id              Int32,
    deployment_id           Nullable(Int32),

    -- Resource / service identity (denormalized at ingest)
    service_name            LowCardinality(String),
    service_version         LowCardinality(String),
    deployment_environment  LowCardinality(String),

    -- Span identity
    trace_id                String,
    span_id                 String,
    parent_span_id          String              DEFAULT '',

    -- Span semantics
    name                    String,
    kind                    LowCardinality(String),

    -- Timing
    start_time              DateTime64(3, 'UTC'),
    end_time                DateTime64(3, 'UTC'),
    duration_ms             Float64,

    -- Status
    status_code             LowCardinality(String),
    status_message          String              DEFAULT '',

    -- Payload (JSON serialised at ingest, JSONExtract* on read)
    attributes              String              DEFAULT '{}',
    events                  String              DEFAULT '[]',

    -- Dedup key: Unix millisecond timestamp set at ingest time.
    -- ReplacingMergeTree keeps the row with the highest _version for each
    -- ORDER BY key, so retried OTLP batches converge to one canonical row.
    _version                UInt64              DEFAULT toUnixTimestamp64Milli(now64())
)
ENGINE = ReplacingMergeTree(_version)
PARTITION BY toYYYYMM(start_time)
ORDER BY (project_id, trace_id, span_id)
TTL toDateTime(start_time) + INTERVAL 90 DAY
SETTINGS index_granularity = 8192;

-- Bloom filter index on service_name for service-filtered trace list queries.
-- Granularity 4 is the ClickHouse-recommended sweet spot for LowCardinality columns.
ALTER TABLE spans ADD INDEX IF NOT EXISTS idx_service_name service_name TYPE bloom_filter GRANULARITY 4;

-- Bloom filter index on status_code for error-filtered queries (status_code = 'ERROR').
ALTER TABLE spans ADD INDEX IF NOT EXISTS idx_status_code status_code TYPE bloom_filter GRANULARITY 4;
