-- SPDX-FileCopyrightText: 2024-2026 Temps Contributors
-- SPDX-License-Identifier: MIT OR Apache-2.0

-- ADR-047 §2: per-line index of sealed log chunks.
--
-- One row per log line. The message bytes are NOT here — they live once in
-- the chunk object (ADR-046); `chunk_seq`/`line_index` point back at them
-- (`line_id = chunk_seq << 20 | line_index`). This table exists for
-- attribute facets, histograms and GROUP BY analytics.
--
-- Requires ClickHouse >= 25.3 for the `JSON` column type (typed dynamic
-- subcolumns: a filter on one attribute reads one column, not the whole bag).
--
-- ReplacingMergeTree keyed on (…, chunk_seq, line_index): the seal pipeline
-- is idempotent and the reindexer may re-insert any chunk at any time;
-- duplicates collapse on merge. Aggregations tolerate a not-yet-merged
-- duplicate (at most one chunk's worth, only between a retry and the next
-- merge), so queries do not use FINAL.

CREATE TABLE IF NOT EXISTS log_lines_index
(
    -- Codecs: label columns are constant over long runs (ZSTD collapses
    -- them), line_index is sequential within a chunk (Delta), timestamps
    -- are near-monotonic (Delta). Measured on 2M lines: 19 B/line with
    -- defaults, ~5 B/line with these — but only if the sort key keeps a
    -- chunk's rows contiguous; see ORDER BY below.
    project_id           Int32                        CODEC(ZSTD(1)),
    external_service_id  Int32                        DEFAULT 0 CODEC(ZSTD(1)),
    env                  LowCardinality(String),
    service              LowCardinality(String),
    deploy_id            Int32                        DEFAULT 0 CODEC(ZSTD(1)),
    container_id         LowCardinality(String),
    node_id              Int32                        DEFAULT 0 CODEC(ZSTD(1)),

    -- Millisecond precision on purpose: nanoseconds cost 3 B/line and
    -- compress 11x worse; the chunk keeps the exact timestamp and is what
    -- the reader returns. The index only filters, buckets and orders.
    ts                   DateTime64(3, 'UTC')         CODEC(Delta(8), ZSTD(1)),
    level                Enum8('trace' = 0, 'debug' = 1, 'info' = 2, 'warn' = 3, 'error' = 4) CODEC(ZSTD(1)),
    stream               Enum8('stdout' = 0, 'stderr' = 1) CODEC(ZSTD(1)),

    chunk_seq            UInt64                       CODEC(ZSTD(1)),
    line_index           UInt32                       CODEC(Delta(4), ZSTD(1)),

    -- universal attributes (ADR-047 §3 canonical keys), always fast
    trace_id             String                       DEFAULT '' CODEC(ZSTD(1)),
    span_id              String                       DEFAULT '' CODEC(ZSTD(1)),
    request_id           String                       DEFAULT '' CODEC(ZSTD(1)),
    status_code          UInt16                       DEFAULT 0 CODEC(ZSTD(1)),
    http_method          LowCardinality(String)       DEFAULT '',
    http_route           String                       DEFAULT '' CODEC(ZSTD(1)),
    duration_ms          Float32                      DEFAULT 0 CODEC(ZSTD(1)),

    -- everything else the parser extracted (capped at ingest: <= 32 keys/line,
    -- key regex + length caps). Plain `JSON` (server default of 1024 dynamic
    -- paths): the Rust client's header validation cannot express type
    -- parameters, and the ingest caps are what actually bound the path set.
    attrs                JSON,

    -- operator-promoted attribute keys -> bloom-indexed slots (same pattern
    -- as spans' 0008_facet_slots.sql; mapping lives in Postgres
    -- `log_line_facets`). NULL for every line until a key is promoted.
    facet_attr_1  Nullable(String) CODEC(ZSTD(1)),
    facet_attr_2  Nullable(String) CODEC(ZSTD(1)),
    facet_attr_3  Nullable(String) CODEC(ZSTD(1)),
    facet_attr_4  Nullable(String) CODEC(ZSTD(1)),
    facet_attr_5  Nullable(String) CODEC(ZSTD(1)),
    facet_attr_6  Nullable(String) CODEC(ZSTD(1)),
    facet_attr_7  Nullable(String) CODEC(ZSTD(1)),
    facet_attr_8  Nullable(String) CODEC(ZSTD(1)),
    facet_attr_9  Nullable(String) CODEC(ZSTD(1)),
    facet_attr_10 Nullable(String) CODEC(ZSTD(1)),
    facet_attr_11 Nullable(String) CODEC(ZSTD(1)),
    facet_attr_12 Nullable(String) CODEC(ZSTD(1)),
    facet_attr_13 Nullable(String) CODEC(ZSTD(1)),
    facet_attr_14 Nullable(String) CODEC(ZSTD(1)),
    facet_attr_15 Nullable(String) CODEC(ZSTD(1)),
    facet_attr_16 Nullable(String) CODEC(ZSTD(1)),
    facet_attr_17 Nullable(String) CODEC(ZSTD(1)),
    facet_attr_18 Nullable(String) CODEC(ZSTD(1)),
    facet_attr_19 Nullable(String) CODEC(ZSTD(1)),
    facet_attr_20 Nullable(String) CODEC(ZSTD(1)),

    -- No message bytes here, not even a preview: that was 45 % of the index
    -- in measurement and the chunk serves a block in ~1 ms. Hit-lists
    -- resolve pointers through the reader.

    -- Time-range pruning. Chunks seal in time order, so `chunk_seq` is
    -- nearly monotonic in `ts` and the granules the sort key produces are
    -- time-clustered; a minmax on `ts` per granule prunes as well as having
    -- `ts` in the key did, without the interleaving cost.
    INDEX idx_ts      ts         TYPE minmax GRANULARITY 1,
    INDEX idx_trace   trace_id   TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_request request_id TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_route   http_route TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_f1  facet_attr_1  TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_f2  facet_attr_2  TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_f3  facet_attr_3  TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_f4  facet_attr_4  TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_f5  facet_attr_5  TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_f6  facet_attr_6  TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_f7  facet_attr_7  TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_f8  facet_attr_8  TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_f9  facet_attr_9  TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_f10 facet_attr_10 TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_f11 facet_attr_11 TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_f12 facet_attr_12 TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_f13 facet_attr_13 TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_f14 facet_attr_14 TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_f15 facet_attr_15 TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_f16 facet_attr_16 TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_f17 facet_attr_17 TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_f18 facet_attr_18 TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_f19 facet_attr_19 TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_f20 facet_attr_20 TYPE bloom_filter(0.01) GRANULARITY 4
)
ENGINE = ReplacingMergeTree
PARTITION BY toDate(ts)
-- `chunk_seq, line_index` — NOT `ts` — right after the labels: every row of a
-- chunk is then contiguous, so `container_id`/`deploy_id`/`env`/`chunk_seq`
-- are constant runs and `line_index` is 0,1,2,… (Delta → ~0 bytes). With
-- `ts` in the key, the containers of one service interleave line by line and
-- those five columns cost 3.1 B/line between them (measured at 50M lines:
-- 8.5 B/line total vs ~5 with this order). `(chunk_seq, line_index)` is
-- also the line's identity, which is what ReplacingMergeTree collapses on.
ORDER BY (project_id, service, chunk_seq, line_index)
-- Table-level default; ADR-047 §6 mirrors the instance log retention via
-- ALTER TABLE ... MODIFY TTL when the setting changes.
TTL toDateTime(ts) + INTERVAL 30 DAY
SETTINGS index_granularity = 8192, ttl_only_drop_parts = 1;

-- Which attribute keys exist, per project and day, and in how many lines:
-- feeds the facet sidebar without scanning the index. JSONAllPaths lists the
-- dynamic paths present in a row's `attrs`. Value cardinality/top values are
-- computed on demand for one key at a time (a typed-subcolumn read), not here
-- — a dynamic-path value cannot be referenced by a runtime key in a
-- materialized view without re-serialising the whole object per row.
CREATE TABLE IF NOT EXISTS log_attr_keys
(
    project_id Int32,
    day        Date,
    key        LowCardinality(String),
    lines      AggregateFunction(count)
)
ENGINE = AggregatingMergeTree
ORDER BY (project_id, day, key)
TTL day + INTERVAL 30 DAY;

CREATE MATERIALIZED VIEW IF NOT EXISTS log_attr_keys_mv TO log_attr_keys AS
SELECT
    project_id,
    toDate(ts)   AS day,
    key,
    countState() AS lines
FROM log_lines_index
ARRAY JOIN JSONAllPaths(attrs) AS key
GROUP BY project_id, day, key;
