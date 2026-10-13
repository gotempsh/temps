-- SPDX-FileCopyrightText: 2024-2026 Temps Contributors
-- SPDX-License-Identifier: MIT OR Apache-2.0

-- Per-row retention for metrics, the shape spans got in 0004_retention_days
-- and 0005_retention_ttl. 0003_metrics fixed the TTL at 90 days, so the
-- observability_retention.otel_metrics_days setting had no effect on
-- ClickHouse. Ingest now stamps each row from that setting and the TTL reads
-- the column.
--
-- Rows written before this migration have no stored retention_days, so they
-- read the DEFAULT, 90, which is the interval the old TTL used: their expiry
-- does not change. materialize_ttl_after_modify = 0 skips the MATERIALIZE TTL
-- mutation MODIFY TTL would otherwise queue over every existing part only to
-- recompute those same expiry times. Parts produced by later merges get the
-- new expression anyway.
--
-- The executor strips whole-line comments and splits on semicolons, so no
-- statement or trailing comment below may contain a semicolon of its own.
ALTER TABLE metrics ADD COLUMN IF NOT EXISTS retention_days UInt16 DEFAULT 90;

ALTER TABLE metrics MODIFY TTL toDateTime(timestamp) + toIntervalDay(retention_days) SETTINGS materialize_ttl_after_modify = 0;
