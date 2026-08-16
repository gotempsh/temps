-- Replace the fixed 30-day TTL with a per-row expression driven by the
-- retention_days column added in 0003_retention_days.sql.  This is a
-- metadata-only operation in ClickHouse; existing rows are not re-scanned
-- immediately.  Rows whose retention_days DEFAULT is 30 continue to expire
-- at the same horizon as before.
ALTER TABLE proxy_logs MODIFY TTL toDateTime(timestamp) + toIntervalDay(retention_days)
