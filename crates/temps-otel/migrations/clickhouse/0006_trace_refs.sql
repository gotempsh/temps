-- Cross-project trace ref index (ADR-027 Phase 0) — ClickHouse edition.
--
-- Answers "which projects hold spans for this trace_id?" WITHOUT knowing the
-- project up front. The `spans` table cannot serve this: its primary index is
-- (project_id, trace_id, span_id), so a trace_id-only lookup is a full scan.
-- This table flips the key order and stores one tiny row per
-- (trace_id, project_id) pair, replacing the PostgreSQL
-- `cross_project_trace_refs` control table when ClickHouse is enabled
-- (that table reached 125 GB in three weeks; the same tuples compress to a
-- small fraction of that here).
--
--   ORDER BY (trace_id, project_id)
--     trace_id first: discovery lookups (`WHERE trace_id = ?`) read a
--     contiguous primary-index range. project_id second: the pair is the
--     logical primary key, so ReplacingMergeTree dedupes per pair.
--
--   ENGINE = ReplacingMergeTree(_version) with an INVERTED version
--     The PG table used `INSERT … ON CONFLICT DO NOTHING`: the FIRST insert
--     wins, so `first_seen` means "earliest observation". ReplacingMergeTree
--     keeps the HIGHEST _version, so writers stamp
--     `_version = UInt64.MAX - first_seen_ms` — the earliest observation gets
--     the highest version and survives merges. Readers additionally take
--     min(first_seen) per project at query time so correctness never depends
--     on merge state.
--
--   TTL toDateTime(first_seen) + toIntervalDay(retention_days)
--     Per-row TTL driven by retention_days, stamped from the SAME resolver
--     value as span rows (RetentionTable::Spans). Trace refs MUST expire on
--     the same horizon as the traces they point to — earlier would dangle the
--     "also in" banner at live traces; later would point at expired ones.
--
--   PARTITION BY toYYYYMM(first_seen)
--     Whole-partition drops once every row in a month falls outside its TTL,
--     matching the `spans` table's partitioning scheme.
CREATE TABLE IF NOT EXISTS cross_project_trace_refs
(
    trace_id        String,
    project_id      Int32,
    first_seen      DateTime64(3, 'UTC') DEFAULT now64(3),
    retention_days  UInt16 DEFAULT 90,
    -- Inverted so the EARLIEST insert has the HIGHEST version (first-wins).
    _version        UInt64 DEFAULT (18446744073709551615 - toUnixTimestamp64Milli(now64()))
)
ENGINE = ReplacingMergeTree(_version)
PARTITION BY toYYYYMM(first_seen)
ORDER BY (trace_id, project_id)
TTL toDateTime(first_seen) + toIntervalDay(retention_days)
SETTINGS index_granularity = 8192;
