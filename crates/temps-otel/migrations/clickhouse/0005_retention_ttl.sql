-- Replace the fixed 90-day TTL with a per-row expression driven by the
-- retention_days column added in 0004_retention_days.sql.  This is a
-- metadata-only operation in ClickHouse; existing rows are not re-scanned
-- immediately.  Rows whose retention_days DEFAULT is 90 continue to expire
-- at the same horizon as before.
ALTER TABLE spans MODIFY TTL toDateTime(start_time) + toIntervalDay(retention_days)
