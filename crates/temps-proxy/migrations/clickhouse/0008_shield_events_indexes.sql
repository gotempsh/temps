-- SPDX-FileCopyrightText: 2024-2026 Temps Contributors
-- SPDX-License-Identifier: MIT OR Apache-2.0

-- The console filters an environment's Shield events by client address,
-- verdict and kind (ADR 066, D11). None is in the sort key, so without these
-- a rare value reads all 30 days of the environment.
ALTER TABLE shield_events ADD INDEX IF NOT EXISTS shield_events_ip ip TYPE bloom_filter(0.01) GRANULARITY 4;
ALTER TABLE shield_events ADD INDEX IF NOT EXISTS shield_events_verdict verdict TYPE set(4) GRANULARITY 4;
ALTER TABLE shield_events ADD INDEX IF NOT EXISTS shield_events_kind kind TYPE set(8) GRANULARITY 4;
-- Index the rows already stored too; this runs in the background.
ALTER TABLE shield_events MATERIALIZE INDEX shield_events_ip;
ALTER TABLE shield_events MATERIALIZE INDEX shield_events_verdict;
ALTER TABLE shield_events MATERIALIZE INDEX shield_events_kind
