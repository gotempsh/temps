-- Compression CODECs for the events table.
--
-- The original table declared no CODECs (default LZ4). With a 5-year retention
-- horizon, the missing specialized codecs are the single biggest storage lever.
-- ALTER MODIFY COLUMN ... CODEC is metadata-only and non-destructive: existing
-- parts keep their encoding, new/merged parts use the new codec. Each statement
-- restates the exact column type + DEFAULT.
--
-- Choices:
--   * timestamp / ingested_at (DateTime64 ms): Delta + ZSTD — event timestamps
--     are near-monotonic, so delta encoding shrinks them sharply over LZ4.
--   * _version (UInt64 ms): DoubleDelta + ZSTD — same monotonic-ms rationale.
--   * props + the high-bulk free-text columns (href, referrer, user_agent,
--     page_title, querystring): ZSTD over LZ4. These are the largest
--     variable-length columns; ZSTD typically adds 1.5-2x over LZ4 on URL/JSON/
--     UA text at trivial CPU cost.
--
-- The LowCardinality dimension columns (event_name, country, browser, utm_*,
-- etc.) already compress well via their dictionary encoding and are left as-is.

ALTER TABLE events MODIFY COLUMN timestamp DateTime64(3, 'UTC') CODEC(Delta, ZSTD(1));

ALTER TABLE events MODIFY COLUMN ingested_at DateTime64(3, 'UTC') DEFAULT now64() CODEC(Delta, ZSTD(1));

ALTER TABLE events MODIFY COLUMN _version UInt64 DEFAULT toUnixTimestamp64Milli(now64()) CODEC(DoubleDelta, ZSTD(1));

ALTER TABLE events MODIFY COLUMN props String CODEC(ZSTD(1));

ALTER TABLE events MODIFY COLUMN href String CODEC(ZSTD(1));

ALTER TABLE events MODIFY COLUMN referrer String CODEC(ZSTD(1));

ALTER TABLE events MODIFY COLUMN user_agent String CODEC(ZSTD(1));

ALTER TABLE events MODIFY COLUMN page_title String CODEC(ZSTD(1));

ALTER TABLE events MODIFY COLUMN querystring String CODEC(ZSTD(1));
