-- SPDX-FileCopyrightText: 2024-2026 Temps Contributors
-- SPDX-License-Identifier: MIT OR Apache-2.0

-- Attribute-key counts written by the server instead of a materialized view.
--
-- `log_attr_keys_mv` ran `ARRAY JOIN JSONAllPaths(attrs)` over every
-- inserted log line and wrote one aggregate row per (project, day, key) per
-- insert. At ~1 insert a second that was thousands of rows per insert, all
-- of them collapsed again by the merges: ~90 GiB written a day for a table
-- that ends up a few kilobytes.
--
-- The server now counts keys in memory as it indexes chunks and writes one
-- row per (project, day, key) about once a minute. A plain sum column
-- (instead of `AggregateFunction(count)`) is what lets it insert a count
-- directly.
CREATE TABLE IF NOT EXISTS log_attr_key_counts
(
    project_id Int32,
    day        Date,
    key        LowCardinality(String),
    lines      SimpleAggregateFunction(sum, UInt64)
)
ENGINE = AggregatingMergeTree
ORDER BY (project_id, day, key)
TTL day + INTERVAL 30 DAY;

DROP VIEW IF EXISTS log_attr_keys_mv;

-- Carry over the counts collected so far. `log_attr_keys` itself is left to
-- expire by its own TTL. If this migration is interrupted after this insert
-- and runs again, the carried-over counts are doubled. They only rank
-- attribute keys in the facet sidebar, so that is harmless.
INSERT INTO log_attr_key_counts
SELECT project_id, day, key, countMerge(lines) AS lines
FROM log_attr_keys
GROUP BY project_id, day, key;
