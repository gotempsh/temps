-- OTel metrics table: system-of-record for metric telemetry when ClickHouse is enabled.
--
-- Design decisions (ADR-016, Phase B):
--
--   ENGINE: ReplacingMergeTree(_version)
--     Like spans, OTLP exporters retry on transient failures. _version (Unix ms
--     timestamp at ingest) lets the engine deduplicate retried batches by keeping
--     the highest _version for each ORDER BY tuple. Use FINAL on reads where dedup
--     correctness matters; skip FINAL on hot-path aggregate reads where eventual
--     consistency is acceptable.
--
--   PARTITION BY toYYYYMM(timestamp)
--     Monthly partitions allow ClickHouse to drop whole TTL-expired partitions in
--     one DROP PARTITION call instead of row-level deletes.
--
--   ORDER BY (project_id, metric_name, service_name, toUnixTimestamp(timestamp))
--     project_id first: every query is project-scoped so the primary index prefix
--     eliminates all other projects cheaply.
--     metric_name second: list_metric_names (SELECT DISTINCT metric_name) and
--     metric-filtered time-series reads read a contiguous block.
--     service_name third: most queries filter by service.
--     timestamp fourth (full DateTime64 ms): time-range scans within a metric/
--     service read sequentially, at millisecond granularity.
--     attributes_hash last: a fingerprint of the label set. Without it, two data
--     points that differ ONLY by their labels (e.g. http.method=GET vs POST) share
--     an ORDER BY tuple and the ReplacingMergeTree silently collapses them into a
--     single row — dropping a whole series. The hash keeps distinct label-sets as
--     distinct rows while still deduplicating true retries (same series + same
--     timestamp + same labels, distinguished by the higher _version).
--
--   TTL toDateTime(timestamp) + INTERVAL 90 DAY
--     Matches the existing TimescaleDB retention policy.
--
--   LowCardinality columns: service_name, service_version, deployment_environment,
--   metric_type, temporality, unit — all bounded value sets. Stored as a dictionary
--   + integer references, cutting storage and improving GROUP BY.
--
--   attributes Map(String,String)
--     The metric data-point labels. A native Map is queryable directly
--     (attributes['key']) without JSONExtract, and the ingest layer already caps
--     label count/size and strips temps.* keys at the trust boundary.
--
--   value Nullable(Float64)
--     Only Gauge/Sum points carry a scalar value; Histogram/Summary/ExpHistogram
--     leave it NULL (a synthetic sum/count value is also written by ingest so the
--     anomaly detector has a number to read).
--
--   Histogram / exponential-histogram / summary arrays
--     Stored as Array columns (empty array = sentinel, never Nullable). This keeps
--     full OTLP fidelity for later quantile reconstruction without a table rewrite.
--
-- NOTE on comments: this file is processed by execute_multi which strips
-- whole-line -- comments then splits on ';'. Do NOT put a ';' in any inline
-- after-code comment (a prior metrics migration crashed at boot exactly this way).
CREATE TABLE IF NOT EXISTS metrics
(
    -- Tenant + deployment context
    project_id              Int32,
    deployment_id           Nullable(Int32),

    -- Resource / service identity (denormalized at ingest)
    service_name            LowCardinality(String),
    service_version         LowCardinality(String),
    deployment_environment  LowCardinality(String),

    -- Metric identity + semantics
    metric_name             String,
    metric_type             LowCardinality(String),
    temporality             LowCardinality(String) DEFAULT 'unspecified',
    is_monotonic            Nullable(UInt8),
    unit                    LowCardinality(String) DEFAULT '',
    description             String                 DEFAULT '',

    -- Timing
    timestamp               DateTime64(3, 'UTC'),
    start_time              Nullable(DateTime64(3, 'UTC')),
    flags                   UInt32                 DEFAULT 0,

    -- Scalar (Gauge / Sum)
    value                   Nullable(Float64),

    -- Explicit histogram / summary aggregate fields
    histogram_count         Nullable(UInt64),
    histogram_sum           Nullable(Float64),
    histogram_min           Nullable(Float64),
    histogram_max           Nullable(Float64),
    histogram_bounds        Array(Float64)         DEFAULT [],
    histogram_bucket_counts Array(UInt64)          DEFAULT [],

    -- Exponential-histogram fields
    exp_scale               Nullable(Int32),
    exp_zero_count          Nullable(UInt64),
    exp_zero_threshold      Nullable(Float64),
    exp_positive_offset     Nullable(Int32),
    exp_positive_counts     Array(UInt64)          DEFAULT [],
    exp_negative_offset     Nullable(Int32),
    exp_negative_counts     Array(UInt64)          DEFAULT [],

    -- Summary quantiles: (quantile, value) pairs
    summary_quantiles       Array(Tuple(Float64, Float64)) DEFAULT [],

    -- Exemplars: (trace_id, span_id, value, timestamp) tuples
    exemplars               Array(Tuple(String, String, Float64, DateTime64(3, 'UTC'))) DEFAULT [],

    -- Data-point labels
    attributes              Map(String, String),

    -- Series fingerprint: a deterministic hash of the label set, included in the
    -- ORDER BY so that data points which differ ONLY by their labels are distinct
    -- rows and are never deduplicated into one another by the ReplacingMergeTree.
    -- MATERIALIZED so ClickHouse derives it from attributes (it can never drift
    -- from the map) and it is not part of the positional insert row.
    attributes_hash         UInt64                 MATERIALIZED sipHash64(arraySort(arrayZip(mapKeys(attributes), mapValues(attributes)))),

    -- Dedup key: Unix millisecond timestamp set at ingest time.
    _version                UInt64                 DEFAULT toUnixTimestamp64Milli(now64())
)
ENGINE = ReplacingMergeTree(_version)
PARTITION BY toYYYYMM(timestamp)
ORDER BY (project_id, metric_name, service_name, timestamp, attributes_hash)
TTL toDateTime(timestamp) + INTERVAL 90 DAY
SETTINGS index_granularity = 8192;

-- Compression CODECs. timestamp clusters tightly (Delta), _version is monotonic
-- ms (DoubleDelta), value is a float metric (Gorilla).
ALTER TABLE metrics MODIFY COLUMN timestamp DateTime64(3, 'UTC') CODEC(Delta, ZSTD(1));

ALTER TABLE metrics MODIFY COLUMN _version UInt64 DEFAULT toUnixTimestamp64Milli(now64()) CODEC(DoubleDelta, ZSTD(1));

ALTER TABLE metrics MODIFY COLUMN value Nullable(Float64) CODEC(Gorilla, ZSTD(1));

-- Bloom filter index on service_name for service-filtered metric queries.
ALTER TABLE metrics ADD INDEX IF NOT EXISTS idx_metrics_service_name service_name TYPE bloom_filter GRANULARITY 4;
