-- Add anonymous country geolocation to telemetry events.
--
-- We derive the 2-letter ISO country code from the request IP at ingest time
-- and store ONLY the country — the IP itself is NEVER persisted. This keeps the
-- anonymous-by-design contract while letting us see the geographic spread of
-- self-hosted instances.

ALTER TABLE telemetry_events
    ADD COLUMN IF NOT EXISTS country TEXT;

-- Fast country breakdowns for the dashboard.
CREATE INDEX IF NOT EXISTS idx_telemetry_events_country
    ON telemetry_events (country, occurred_at DESC);

-- Track country per instance-day too, so we can map active instances by country
-- without scanning the full events table.
ALTER TABLE telemetry_instance_days
    ADD COLUMN IF NOT EXISTS country TEXT;
