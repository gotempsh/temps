-- Telemetry schema for Temps product analytics
-- All data is anonymous: no emails, IPs, or PII stored
-- anonymous_id is a stable random UUID generated on the instance at first boot

CREATE TABLE IF NOT EXISTS telemetry_events (
    id              BIGSERIAL PRIMARY KEY,
    -- Stable anonymous identifier generated at instance boot, never tied to a user
    anonymous_id    TEXT        NOT NULL,
    -- Event type: deploy_attempted, deploy_succeeded, deploy_failed,
    --             project_created, instance_started, instance_setup_completed, etc.
    event_type      TEXT        NOT NULL,
    -- Loose schema: arbitrary properties per event type
    properties      JSONB       NOT NULL DEFAULT '{}',
    -- Temps version that sent the event
    temps_version   TEXT,
    -- Timestamp the event occurred on the instance (sent by client)
    occurred_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Timestamp we received it
    received_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Fast lookups by anonymous_id for funnel queries
CREATE INDEX IF NOT EXISTS idx_telemetry_events_anon_id
    ON telemetry_events (anonymous_id, occurred_at DESC);

-- Fast lookups by event type for aggregate dashboards
CREATE INDEX IF NOT EXISTS idx_telemetry_events_type
    ON telemetry_events (event_type, occurred_at DESC);

-- Table to track daily active instance counts (materialized from events)
-- This is denormalized for cheap dashboard queries without scanning the full events table
CREATE TABLE IF NOT EXISTS telemetry_instance_days (
    anonymous_id    TEXT        NOT NULL,
    day             DATE        NOT NULL,
    temps_version   TEXT,
    PRIMARY KEY (anonymous_id, day)
);

CREATE INDEX IF NOT EXISTS idx_telemetry_instance_days_day
    ON telemetry_instance_days (day DESC);
