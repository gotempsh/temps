-- Sessions table: ReplacingMergeTree because end_time is updated as the
-- session progresses. Latest `_version` wins — Postgres is the source of
-- truth, this is just the analytical replica.
CREATE TABLE IF NOT EXISTS sessions (
    session_id      String,
    project_id      UInt32,
    environment_id  Nullable(UInt32),
    visitor_id      Nullable(String),
    user_id         Nullable(UUID),
    started_at      DateTime64(3, 'UTC'),
    ended_at        Nullable(DateTime64(3, 'UTC')),
    device_type     LowCardinality(String),
    browser         LowCardinality(String),
    os              LowCardinality(String),
    country_code    LowCardinality(FixedString(2)),
    referrer        String,
    referrer_hostname String,
    utm_source      LowCardinality(String),
    utm_medium      LowCardinality(String),
    utm_campaign    LowCardinality(String),
    channel         LowCardinality(String),
    _version        UInt64 DEFAULT toUnixTimestamp64Milli(now64())
)
ENGINE = ReplacingMergeTree(_version)
PARTITION BY toYYYYMM(started_at)
ORDER BY (project_id, started_at, session_id);
