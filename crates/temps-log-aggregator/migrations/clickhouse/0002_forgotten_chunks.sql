-- SPDX-FileCopyrightText: 2024-2026 Temps Contributors
-- SPDX-License-Identifier: MIT OR Apache-2.0

-- Forgetting chunks without a table-wide mutation per call.
--
-- `log_lines_index` is partitioned by day but ordered by
-- (project_id, service, chunk_seq, line_index), so a bare
-- `DELETE ... WHERE chunk_seq IN (...)` can prune nothing: every call
-- mutated parts in every daily partition. At compaction cadence that was
-- ~44k MutatePart operations rewriting ~420 GiB a day for a ~2 GiB table.
--
-- Now a forget is two cheap steps:
--
-- 1. `forget_chunks` inserts a tombstone here, and every read excludes
--    tombstoned seqs immediately — the same masking the Cloud target has
--    always used, minus the instance correlation (a local table only ever
--    holds this instance's chunks).
-- 2. A periodic reclaim pass looks up which day partitions hold the
--    tombstoned chunks (via `log_chunk_days` below), issues one
--    `DELETE ... IN PARTITION` per affected day, then clears the
--    reclaimed seqs from both small tables.
--
-- Both small tables carry the same TTL as the lines table (realigned with
-- the instance log retention by `set_retention_days`), so nothing here can
-- outlive the rows it describes.
CREATE TABLE IF NOT EXISTS log_forgotten_chunks
(
    chunk_seq    UInt64,
    forgotten_at DateTime64(3, 'UTC')
)
ENGINE = ReplacingMergeTree
ORDER BY chunk_seq
TTL toDateTime(forgotten_at) + INTERVAL 30 DAY;

-- Which day partitions of `log_lines_index` hold each chunk's lines. One or
-- two rows per chunk (a chunk can straddle midnight), filled by the view
-- below as part of the same insert that writes the lines, so a chunk that
-- has been acknowledged as indexed always has its row here.
CREATE TABLE IF NOT EXISTS log_chunk_days
(
    chunk_seq UInt64,
    day       Date
)
ENGINE = ReplacingMergeTree
ORDER BY (chunk_seq, day)
TTL day + INTERVAL 30 DAY;

CREATE MATERIALIZED VIEW IF NOT EXISTS log_chunk_days_mv TO log_chunk_days AS
SELECT DISTINCT
    chunk_seq,
    toDate(ts) AS day
FROM log_lines_index;

-- Chunks indexed before this migration. A one-time read of two columns;
-- duplicates of anything the view wrote meanwhile collapse on merge.
INSERT INTO log_chunk_days
SELECT DISTINCT chunk_seq, toDate(ts) AS day
FROM log_lines_index;
