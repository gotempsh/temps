-- Pre-aggregated 5-minute rollups for dashboard widgets.
-- Streams from `events` automatically — every insert into events triggers
-- the SummingMergeTree state update.
CREATE MATERIALIZED VIEW IF NOT EXISTS events_5m_mv
ENGINE = SummingMergeTree
PARTITION BY toYYYYMM(bucket)
ORDER BY (project_id, event_name, bucket)
AS
SELECT
    project_id,
    event_name,
    toStartOfFiveMinute(timestamp)              AS bucket,
    count()                                     AS event_count,
    uniqState(session_id)                       AS sessions_state,
    uniqState(visitor_id)                       AS visitors_state
FROM events
GROUP BY project_id, event_name, bucket;
