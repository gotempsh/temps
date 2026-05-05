-- Events table: derived analytical replica of Postgres `events`.
-- ReplacingMergeTree dedupes on (sort key + event_id) using `_version`,
-- so retries from the outbox worker are safe.
--
-- Sort key intentionally puts workspace_id and project_id first so the most
-- common analytics queries (everything is project-scoped) ride the index
-- prefix.
CREATE TABLE IF NOT EXISTS events (
    event_id        UUID,
    workspace_id    UUID,
    project_id      UInt32,
    environment_id  Nullable(UInt32),
    deployment_id   Nullable(UInt32),
    session_id      String,
    visitor_id      Nullable(String),
    user_id         Nullable(UUID),
    timestamp       DateTime64(3, 'UTC'),
    event_name      LowCardinality(String),
    event_type      LowCardinality(String),
    event_data      String,                   -- raw JSON; parse on read with JSONExtract
    page_url        String,
    page_title      String,
    referrer        String,
    referrer_hostname String,
    hostname        String,
    request_path    String,
    request_query   String,
    user_agent      String,
    device_type     LowCardinality(String),
    browser         LowCardinality(String),
    os              LowCardinality(String),
    country_code    LowCardinality(FixedString(2)),
    utm_source      LowCardinality(String),
    utm_medium      LowCardinality(String),
    utm_campaign    LowCardinality(String),
    utm_term        String,
    utm_content     String,
    channel         LowCardinality(String),
    -- Web Vitals
    ttfb            Nullable(Float32),
    lcp             Nullable(Float32),
    fid             Nullable(Float32),
    fcp             Nullable(Float32),
    cls             Nullable(Float32),
    inp             Nullable(Float32),
    ingested_at     DateTime64(3, 'UTC') DEFAULT now64(),
    _version        UInt64 DEFAULT toUnixTimestamp64Milli(now64())
)
ENGINE = ReplacingMergeTree(_version)
PARTITION BY toYYYYMM(timestamp)
ORDER BY (project_id, timestamp, event_name, event_id)
TTL toDateTime(timestamp) + INTERVAL 5 YEAR
SETTINGS index_granularity = 8192;

-- Bloom filter on event_name speeds up name-filtered scans. Granularity 4
-- is the default sweet spot recommended by the ClickHouse docs.
ALTER TABLE events ADD INDEX IF NOT EXISTS idx_event_name event_name TYPE bloom_filter GRANULARITY 4;
