-- Events table: derived analytical replica of Postgres `events`.
-- ReplacingMergeTree dedupes on the sort key (event_id is the last
-- component) using `_version`, so retries from the outbox worker are safe.
--
-- Sort key intentionally puts project_id and timestamp first so the most
-- common analytics queries (everything is project-scoped, time-windowed)
-- ride the index prefix.
--
-- Type choices match the Postgres source:
--   - event_id is `bigint` in PG → Int64 here.
--   - project_id / environment_id / deployment_id are `int` in PG → Int32.
--   - visitor_id is `int` in PG → Int32 (nullable).
--   - session_id is `text` in PG → String (nullable, but we mirror NOT NULL).
--   - timestamp is `timestamptz(3)` semantically → DateTime64(3, 'UTC').
CREATE TABLE IF NOT EXISTS events
(
    event_id            Int64,
    project_id          Int32,
    environment_id      Nullable(Int32),
    deployment_id       Nullable(Int32),
    session_id          String,
    visitor_id          Nullable(Int32),
    timestamp           DateTime64(3, 'UTC'),

    -- Page data
    hostname            String,
    pathname            String,
    page_path           String,
    href                String,
    querystring         String,
    page_title          String,
    referrer            String,
    referrer_hostname   String,

    -- Event identity
    event_type          LowCardinality(String),
    event_name          LowCardinality(String),
    -- Raw JSON properties; parse on read with JSONExtract*. Keeping it as
    -- String avoids early commitment to a Map shape that may not match
    -- what apps actually send.
    props               String,

    -- Device / browser
    user_agent          String,
    browser             LowCardinality(String),
    browser_version     String,
    operating_system    LowCardinality(String),
    operating_system_version String,
    device_type         LowCardinality(String),
    screen_width        Nullable(Int16),
    screen_height       Nullable(Int16),
    viewport_width      Nullable(Int16),
    viewport_height     Nullable(Int16),

    -- Geography. Denormalized from the Postgres `ip_geolocations` table at
    -- fan-out time so property breakdowns can group by country/region/city
    -- without a cross-database join. Empty string is the canonical
    -- "no value" sentinel (LowCardinality strings are not nullable here).
    ip_geolocation_id   Nullable(Int32),
    country             LowCardinality(String),
    region              LowCardinality(String),
    city                String,

    -- Traffic source
    channel             LowCardinality(String),
    utm_source          LowCardinality(String),
    utm_medium          LowCardinality(String),
    utm_campaign        LowCardinality(String),
    utm_term            String,
    utm_content         String,

    -- Web Vitals
    ttfb                Nullable(Float32),
    lcp                 Nullable(Float32),
    fid                 Nullable(Float32),
    fcp                 Nullable(Float32),
    cls                 Nullable(Float32),
    inp                 Nullable(Float32),

    -- Session flow
    is_entry            UInt8 DEFAULT 0,
    is_exit             UInt8 DEFAULT 0,
    is_bounce           UInt8 DEFAULT 0,
    is_crawler          UInt8 DEFAULT 0,
    time_on_page        Nullable(Int32),
    session_page_number Nullable(Int32),
    scroll_depth        Nullable(Int32),
    clicks              Nullable(Int32),

    -- Misc
    language            LowCardinality(String),
    crawler_name        String,

    ingested_at         DateTime64(3, 'UTC') DEFAULT now64(),
    _version            UInt64 DEFAULT toUnixTimestamp64Milli(now64())
)
ENGINE = ReplacingMergeTree(_version)
PARTITION BY toYYYYMM(timestamp)
ORDER BY (project_id, timestamp, event_name, event_id)
TTL toDateTime(timestamp) + INTERVAL 5 YEAR
SETTINGS index_granularity = 8192;

-- Bloom filter on event_name speeds up name-filtered scans. Granularity 4
-- is the default sweet spot recommended by the ClickHouse docs.
ALTER TABLE events ADD INDEX IF NOT EXISTS idx_event_name event_name TYPE bloom_filter GRANULARITY 4;
