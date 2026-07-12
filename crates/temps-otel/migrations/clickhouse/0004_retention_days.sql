-- Add retention_days to spans so each row carries its own TTL value.
-- The DEFAULT (90) equals the prior hardcoded INTERVAL, so existing rows
-- are unaffected: their effective expiry does not change until the TTL
-- expression is updated by 0005_retention_ttl.sql.
ALTER TABLE spans ADD COLUMN IF NOT EXISTS retention_days UInt16 DEFAULT 90
