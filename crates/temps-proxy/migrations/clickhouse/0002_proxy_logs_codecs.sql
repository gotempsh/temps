-- Compression CODECs for the proxy_logs table.
--
-- The original table declared no CODECs (default LZ4 everywhere). These ALTERs
-- apply specialized codecs. ALTER MODIFY COLUMN ... CODEC is metadata-only and
-- non-destructive: existing parts are untouched, new/merged parts use the new
-- codec. Each statement restates the exact column type + DEFAULT.
--
-- Choices:
--   * timestamp (DateTime64 ms): Delta + ZSTD — request timestamps are
--     near-monotonic, so delta encoding shrinks them sharply over LZ4.
--   * _version (UInt64 ms): DoubleDelta + ZSTD — same monotonic-ms rationale.
--   * request_headers/response_headers (JSON text): ZSTD. These two columns are
--     the bulk of the table's storage and are write-only (never read); ZSTD
--     reclaims a large fraction of their footprint over LZ4.
--
-- Note: request_id is random UUID text (incompressible) and the LowCardinality
-- dimension columns already compress well, so they are intentionally left on
-- the default codec.

ALTER TABLE proxy_logs MODIFY COLUMN timestamp DateTime64(3, 'UTC') CODEC(Delta, ZSTD(1));

ALTER TABLE proxy_logs MODIFY COLUMN _version UInt64 DEFAULT toUnixTimestamp64Milli(now64()) CODEC(DoubleDelta, ZSTD(1));

ALTER TABLE proxy_logs MODIFY COLUMN request_headers String DEFAULT '{}' CODEC(ZSTD(1));

ALTER TABLE proxy_logs MODIFY COLUMN response_headers String DEFAULT '{}' CODEC(ZSTD(1));
