-- Add retention_days to proxy_logs so each row carries its own TTL value.
-- The DEFAULT (30) equals the prior hardcoded INTERVAL, so existing rows
-- are unaffected: their effective expiry does not change until the TTL
-- expression is updated by 0004_retention_ttl.sql.
ALTER TABLE proxy_logs ADD COLUMN IF NOT EXISTS retention_days UInt16 DEFAULT 30
