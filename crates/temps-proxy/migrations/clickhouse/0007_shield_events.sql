-- SPDX-FileCopyrightText: 2024-2026 Temps Contributors
-- SPDX-License-Identifier: MIT OR Apache-2.0

-- Shield events (ADR 066, D9, D11 and I8): one row per decision other than
-- `pass`, and per verify result, from every ingress. The promoter (D9) and
-- the console's Shield tab read them back. IPv4 addresses are stored as
-- ::ffff:a.b.c.d.
CREATE TABLE IF NOT EXISTS shield_events
(
    timestamp DateTime64(3),
    project_id Int32,
    environment_id Int32,
    node_id Nullable(Int32),
    request_id String,
    kind LowCardinality(String),
    ip IPv6,
    ja4 LowCardinality(String) DEFAULT '',
    verdict Enum8('pass' = 0, 'log' = 1, 'challenge' = 2, 'block' = 3),
    mode LowCardinality(String),
    reasons Array(LowCardinality(String)),
    signature LowCardinality(String) DEFAULT '',
    host String,
    path String,
    user_agent String,
    difficulty Nullable(UInt8),
    solve_ms Nullable(UInt32)
)
ENGINE = MergeTree
ORDER BY (environment_id, timestamp)
TTL toDateTime(timestamp) + INTERVAL 30 DAY
