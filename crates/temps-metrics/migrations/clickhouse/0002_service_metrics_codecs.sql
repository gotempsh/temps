-- Compression CODECs for the service_metrics table.
--
-- The original table declared no CODECs (default LZ4). These ALTERs apply the
-- standard time-series codecs. ALTER MODIFY COLUMN ... CODEC is metadata-only
-- and non-destructive: existing parts keep their encoding, new/merged parts use
-- the new codec. Each statement restates the exact column type + DEFAULT.
--
-- Choices:
--   * time (DateTime64 ms): DoubleDelta + ZSTD — scrape timestamps advance at a
--     near-constant cadence (~30s), the textbook delta-of-delta case.
--   * _version (UInt64 ms): DoubleDelta + ZSTD — same monotonic-ms rationale.
--   * value (Float64): Gorilla + ZSTD — Gorilla is purpose-built for the slowly
--     varying float series that metric values are; the single biggest win here.
--   * labels (JSON text): ZSTD over LZ4 for the repetitive label JSON.
--
-- time and _version together are the table's largest storage cost, and `value`
-- is the second largest, so these four codecs target the bulk of the footprint.

ALTER TABLE service_metrics MODIFY COLUMN time DateTime64(3, 'UTC') CODEC(DoubleDelta, ZSTD(1));

ALTER TABLE service_metrics MODIFY COLUMN _version UInt64 DEFAULT toUnixTimestamp64Milli(now64()) CODEC(DoubleDelta, ZSTD(1));

ALTER TABLE service_metrics MODIFY COLUMN value Float64 CODEC(Gorilla, ZSTD(1));

ALTER TABLE service_metrics MODIFY COLUMN labels String DEFAULT '{}' CODEC(ZSTD(1));
