-- Compression CODECs for the spans table.
--
-- The original table declared no CODECs, so every column used default LZ4.
-- These ALTERs apply specialized codecs that compress far better for the
-- column's data shape. ALTER MODIFY COLUMN ... CODEC is metadata-only and
-- non-destructive: existing parts keep their current encoding, new parts (and
-- anything rewritten by a later merge) use the new codec. Each statement
-- restates the exact column type + DEFAULT so nothing else changes.
--
-- Choices:
--   * Timestamps (start_time/end_time): Delta + ZSTD. Span timestamps within a
--     trace cluster tightly, so delta-of-delta-style encoding shrinks them from
--     ~1.6x to 8-15x.
--   * _version (UInt64 ms): DoubleDelta + ZSTD — same monotonic-ms rationale.
--   * duration_ms (Float64): Gorilla + ZSTD — the standard float/metric codec.
--   * attributes/events (JSON text): ZSTD beats LZ4 on repetitive JSON.

ALTER TABLE spans MODIFY COLUMN start_time DateTime64(3, 'UTC') CODEC(Delta, ZSTD(1));

ALTER TABLE spans MODIFY COLUMN end_time DateTime64(3, 'UTC') CODEC(Delta, ZSTD(1));

ALTER TABLE spans MODIFY COLUMN _version UInt64 DEFAULT toUnixTimestamp64Milli(now64()) CODEC(DoubleDelta, ZSTD(1));

ALTER TABLE spans MODIFY COLUMN duration_ms Float64 CODEC(Gorilla, ZSTD(1));

ALTER TABLE spans MODIFY COLUMN attributes String DEFAULT '{}' CODEC(ZSTD(1));

ALTER TABLE spans MODIFY COLUMN events String DEFAULT '[]' CODEC(ZSTD(1));
