-- OTel spans table: system-of-record for trace telemetry when ClickHouse is enabled.
--
-- Design decisions (ADR-016, Phase 0):
--
--   ENGINE: ReplacingMergeTree(_version)
--     OTLP exporters retry on transient failures; _version (Unix ms timestamp at
--     ingest) lets the engine deduplicate retried batches by keeping the highest
--     _version for each (project_id, trace_id, span_id) tuple. Use FINAL on reads
--     where dedup correctness is required (trace summaries list); skip FINAL for
--     hot-path ingest metrics where eventual consistency is acceptable.
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
