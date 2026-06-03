-- Resource-metrics table: system-of-record for scraped DB/container/node metrics
-- (CPU, memory, connections, ...) when the monitoring store is set to ClickHouse.
--
-- This mirrors the PostgreSQL `service_metrics` hypertable used by
-- TimescaleMetricsStore, but follows the locked design decision for the CH
-- backend: ONE raw table + native TTL for retention + query-time rollup.
-- There are NO hourly/daily AggregatingMergeTree rollup materialized views;
-- time bucketing is done at query time with toStartOfInterval(). ClickHouse's
-- vectorised engine handles the bucketing well under 100ms at these volumes
-- (proven for the traces backend and re-confirmed by the benchmark below).
--
-- Design decisions:
--
--   ENGINE: ReplacingMergeTree(_version)
--     The TimescaleDB writer uses `INSERT ... ON CONFLICT DO NOTHING` for
--     idempotency against retried scrape batches. ReplacingMergeTree(_version)
--     is the ClickHouse analog: re-inserting the same ORDER BY key converges to
--     a single row (highest _version wins). Read paths that need dedup
--     correctness use FINAL (query_range / query_latest); this matches the OTel
--     spans backend convention. _version is the Unix-ms timestamp at ingest.
--     value is deliberately NOT part of ORDER BY, so two scrapes of the same
--     (source, name, time, labels) with different values collapse to one row.
--
--   ORDER BY (source_kind, source_id, name, time, labels)
--     Every query filters on source_kind + source_id (and usually name), so this
--     primary-index prefix eliminates all other sources/metrics cheaply before
--     the time range scan. time next keeps each metric series contiguous and
--     already time-sorted on disk, which is what makes query-time bucketing fast.
--     labels is LAST in the key (not value): a single scrape of one metric emits
--     multiple rows at the same `time` — one instance-wide aggregate row plus one
--     row per per-series label set (e.g. per `datname`). These MUST coexist (the
--     PostgreSQL table keeps them all; its index is non-unique). Putting labels in
--     the ORDER BY keeps every label series as a distinct row while still letting
--     ReplacingMergeTree dedup an EXACT retried scrape (same source/name/time/labels)
--     down to one canonical row via the highest _version.
--
--   PARTITION BY toYYYYMM(time)
--     Monthly partitions let ClickHouse drop whole TTL-expired partitions in one
--     DROP PARTITION at the next merge cycle rather than row-level deletes,
--     keeping retention cheap even at hundreds of millions of rows.
--
--   TTL toDateTime(time) + INTERVAL 90 DAY
--     IMPORTANT: this is 90 days, NOT the `retention_raw_days` default of 7.
--     With query-time rollup there is only this one raw table; query_range serves
--     the 7..90d window (TimescaleDB's hourly continuous aggregate tier) and the
--     >90d window (daily tier) from the SAME raw rows. A 7-day TTL would make
--     those wider-range chart queries return empty. 90 DAY matches the
--     TimescaleDB hourly-CA retention so the chartable history is equivalent.
--     The store sets the effective TTL from max(retention_raw_days, 90) when it
--     is configurable; this file is the safe default.
--
--   LowCardinality columns: source_kind, name, kind, engine, environment
--     All have small, bounded value sets (4 source kinds, 2 metric kinds, a
--     handful of engines/environments, and a bounded set of dotted metric
--     names). LowCardinality stores them as a dictionary + integer references,
--     cutting storage and speeding up GROUP BY / index lookups.
--
--   engine / environment String DEFAULT ''
--     These map from Rust Option<String>; the ingest layer writes '' for None
--     (LowCardinality cannot be Nullable cheaply, and '' is the canonical
--     "unset" sentinel). node_id stays Nullable(Int32) because it is a true
--     foreign-key-style value with no natural empty sentinel.
--
--   labels String DEFAULT '{}'  (JSON-as-String)
--     The Rust ingest layer serialises HashMap<String,String> via serde_json,
--     matching the `labels jsonb` column in PostgreSQL. There is no native CH
--     Map needed: read paths use JSONExtractString(labels, key) /
--     JSONHas(labels, key) / JSONExtractKeys(labels). The "instance-wide
--     aggregate series = fewest label keys" rule (load-bearing for query_range
--     and query_latest so per-datname rows don't blend into the aggregate) is
--     computed as length(JSONExtractKeys(labels)).
--
-- BENCHMARK RESULT (2026-06-02, ClickHouse 26.2.5, local single-node):
--   Dataset: 1,728,000 raw points / 4 sources x 6 metric names / 5 days
--            (1 scrape every 30s, ~14,400 scrapes/day, 2 with-label series each).
--
--   query_range gauge (toStartOfInterval 1h, avg(value) FINAL, 1 source/name,
--                       7-day window, fewest-label-keys filter):
--     best 12ms, avg ~17ms  (statistics.elapsed)
--
--   query_range counter monotonic (per-scrape max -> per-bucket max ->
--                       lagInFrame() delta -> greatest(.,0), 1h, 7-day window):
--     best 14ms, avg ~19ms  (statistics.elapsed)
--
--   DECISION: query-time bucketing chosen (this file only; no rollup MVs).
--   Both query shapes are comfortably under the 100ms threshold at ~1.7M rows.
--   The ORDER BY (source_kind, source_id, name, time, labels) prefix makes the
--   range scan a sequential read of already-sorted compressed blocks. Re-evaluate
--   if a single source accumulates many tens of millions of points.
CREATE TABLE IF NOT EXISTS service_metrics
(
    -- Sample timestamp (millisecond precision; ingest writes Unix-ms via the
    -- clickhouse Row derive, read back as chrono DateTime<Utc>).
    time            DateTime64(3, 'UTC'),

    -- Source identity. source_kind is one of database|deployment|container|node;
    -- source_id references external_services.id / deployments.id /
    -- deployment_containers.id / nodes.id depending on the kind.
    source_kind     LowCardinality(String),
    source_id       Int32,

    -- Metric identity. name is dotted (e.g. "pg.connections_active"); validated
    -- against [a-zA-Z0-9_.:-] before reaching SQL on both write and read paths.
    name            LowCardinality(String),

    -- Sample value. Counters store a non-negative DELTA already computed by the
    -- scraper (NOT a cumulative raw counter) for the scraped write path; the
    -- monotonic OTLP read path stores cumulative values and deltas are computed
    -- at query time. Not part of ORDER BY so duplicate scrapes dedup cleanly.
    value           Float64,

    -- Gauge | Counter. Drives nothing at storage time but kept for parity and
    -- so future read paths can distinguish without re-deriving from name.
    kind            LowCardinality(String),

    -- Optional context (mapped from Rust Option<String>; '' == unset).
    engine          LowCardinality(String) DEFAULT '',
    environment     LowCardinality(String) DEFAULT '',
    node_id         Nullable(Int32),

    -- Arbitrary label k/v as serde_json String (PostgreSQL `labels jsonb`
    -- analog). Read via JSONExtractString / JSONHas / JSONExtractKeys.
    labels          String DEFAULT '{}',

    -- Dedup key: Unix millisecond timestamp set at ingest. ReplacingMergeTree
    -- keeps the row with the highest _version per ORDER BY key, so retried
    -- scrape batches converge to one canonical row (CH analog of
    -- ON CONFLICT DO NOTHING).
    _version        UInt64 DEFAULT toUnixTimestamp64Milli(now64())
)
ENGINE = ReplacingMergeTree(_version)
PARTITION BY toYYYYMM(time)
ORDER BY (source_kind, source_id, name, time, labels)
-- NOTE: ClickHouse enforces this TTL at INSERT time too: rows whose `time` is
-- already older than the window are silently discarded (HTTP 200, written_rows=0,
-- no error). The live scrape path always writes current timestamps so it is
-- unaffected, but any historical BACKFILL/import of points older than the TTL
-- would be dropped. A backfill feature would need a wider TTL or an import guard.
TTL toDateTime(time) + INTERVAL 90 DAY
SETTINGS index_granularity = 8192;
