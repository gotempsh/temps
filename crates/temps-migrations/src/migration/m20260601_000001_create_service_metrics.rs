use sea_orm_migration::prelude::*;

/// Creates the `service_metrics` and `service_metrics_histogram` hypertables
/// with indexes, continuous aggregates, and retention/refresh policies.
///
/// **Tiered retention:**
/// - Raw (`service_metrics`): 30 days
/// - Hourly aggregate (`service_metrics_hourly`): 90 days
/// - Daily aggregate (`service_metrics_daily`): 1 year (365 days)
///
/// **Continuous aggregate hierarchy:**
/// - `service_metrics_hourly` is built from the raw table (1-hour buckets).
/// - `service_metrics_daily` is built from `service_metrics_hourly` (1-day
///   buckets), not from the raw table. This avoids re-scanning millions of
///   30-second raw rows during the daily refresh and requires TimescaleDB ≥ 2.9.
///
/// **Safely re-runnable:** all DDL uses `IF NOT EXISTS` / `IF EXISTS` guards.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
-- ============================================================
-- Raw metrics table (gauge / counter deltas)
-- ============================================================
CREATE TABLE IF NOT EXISTS service_metrics (
    time            TIMESTAMPTZ     NOT NULL,
    source_kind     TEXT            NOT NULL,   -- 'database' | 'deployment' | 'container' | 'node'
    source_id       INT             NOT NULL,   -- FK to the entity in its own table
    name            TEXT            NOT NULL,   -- e.g. 'pg.connections_active'
    value           FLOAT8          NOT NULL,
    engine          TEXT,                       -- e.g. 'postgres', 'redis'
    environment     TEXT,                       -- deployment environment name
    node_id         INT,                        -- FK nodes.id (nullable)
    labels          JSONB           NOT NULL DEFAULT '{}'::jsonb
);

-- Convert to hypertable (no-op if already one)
SELECT create_hypertable(
    'service_metrics', 'time',
    chunk_time_interval => INTERVAL '1 day',
    if_not_exists       => TRUE
);

-- Composite index for point lookups and range scans per entity.
-- Includes source_kind so the optimizer can filter it via the index prefix
-- rather than as a post-scan heap filter. This is important when source_id
-- values are not globally unique across entity types (e.g. database.id=1 and
-- node.id=1 both exist).
--
-- TODO(metrics): Issue 9 — verify via EXPLAIN ANALYZE that this index is
-- chosen for the DISTINCT ON (name) query in query_latest(). On PostgreSQL ≤14
-- the planner may prefer a Seq Scan when the index has 4 leading columns.
CREATE INDEX IF NOT EXISTS idx_service_metrics_source_name_time
    ON service_metrics (source_id, source_kind, name, time DESC);

-- NOTE: GIN index on the labels column has been intentionally omitted.
--
-- TODO(metrics): Issue 7 — at 10 000 inserts / 30 s the GIN pending-list
-- flush becomes a bottleneck (~60 000 GIN entry writes / 30 s). The top-level
-- columns (environment, node_id) already cover the hot alert-evaluation query
-- path. Restore the GIN index only if dashboard label-filter queries require
-- it, and consider adding it on a read replica rather than the primary:
--
--   CREATE INDEX IF NOT EXISTS idx_service_metrics_labels
--       ON service_metrics USING GIN (labels);

-- ============================================================
-- Histogram metrics table (for OTLP histogram metric type)
-- ============================================================
CREATE TABLE IF NOT EXISTS service_metrics_histogram (
    time            TIMESTAMPTZ     NOT NULL,
    source_kind     TEXT            NOT NULL,
    source_id       INT             NOT NULL,
    name            TEXT            NOT NULL,
    count_delta     BIGINT          NOT NULL,
    sum_delta       FLOAT8          NOT NULL,
    -- Parallel arrays: bucket_bounds[i] is the upper bound,
    -- bucket_counts[i] is the delta count for that bucket.
    -- Deltas (not cumulative) so scrape resets are handled upstream.
    bucket_bounds   FLOAT8[]        NOT NULL DEFAULT '{}',
    bucket_counts   BIGINT[]        NOT NULL DEFAULT '{}',
    engine          TEXT,
    environment     TEXT,
    labels          JSONB           NOT NULL DEFAULT '{}'::jsonb
);

SELECT create_hypertable(
    'service_metrics_histogram', 'time',
    chunk_time_interval => INTERVAL '1 day',
    if_not_exists       => TRUE
);

CREATE INDEX IF NOT EXISTS idx_service_metrics_histogram_source_name_time
    ON service_metrics_histogram (source_id, source_kind, name, time DESC);

-- ============================================================
-- Continuous aggregate: 1-hour buckets (raw → hourly)
-- ============================================================
CREATE MATERIALIZED VIEW IF NOT EXISTS service_metrics_hourly
WITH (timescaledb.continuous) AS
SELECT
    time_bucket('1 hour', time) AS bucket,
    source_kind,
    source_id,
    name,
    engine,
    environment,
    AVG(value)   AS avg_value,
    MIN(value)   AS min_value,
    MAX(value)   AS max_value,
    COUNT(*)     AS sample_count
FROM service_metrics
GROUP BY bucket, source_kind, source_id, name, engine, environment
WITH NO DATA;

-- Refresh policy: keep the hourly aggregate up-to-date every hour.
-- end_offset must be >= chunk_time_interval (1 day) per TimescaleDB requirements.
-- Using 1 day end_offset means the last ~24h of hourly data may lag by up to
-- 1 hour, which is acceptable for historical trend charts.
SELECT add_continuous_aggregate_policy(
    'service_metrics_hourly',
    start_offset => INTERVAL '3 days',
    end_offset   => INTERVAL '1 day',
    schedule_interval => INTERVAL '1 hour',
    if_not_exists => TRUE
);

-- ============================================================
-- Continuous aggregate: 1-day buckets (hourly → daily)
--
-- This view is hierarchical: it aggregates from service_metrics_hourly
-- rather than from the raw table. This avoids re-scanning millions of
-- 30-second raw rows during the daily refresh (Issue 6).
--
-- Requires TimescaleDB ≥ 2.9. If migration fails with "hierarchical
-- continuous aggregates not supported", upgrade TimescaleDB.
-- ============================================================
CREATE MATERIALIZED VIEW IF NOT EXISTS service_metrics_daily
WITH (timescaledb.continuous) AS
SELECT
    time_bucket('1 day', bucket) AS bucket,
    source_kind,
    source_id,
    name,
    engine,
    environment,
    AVG(avg_value)    AS avg_value,
    MIN(min_value)    AS min_value,
    MAX(max_value)    AS max_value,
    SUM(sample_count) AS sample_count
FROM service_metrics_hourly
GROUP BY time_bucket('1 day', bucket), source_kind, source_id, name, engine, environment
WITH NO DATA;

-- Refresh policy: update the daily aggregate every day.
-- end_offset must be >= the source view's chunk interval (1 day).
-- Using 2 days end_offset gives the hourly aggregate time to fully materialize
-- before the daily rollup reads from it.
SELECT add_continuous_aggregate_policy(
    'service_metrics_daily',
    start_offset => INTERVAL '7 days',
    end_offset   => INTERVAL '2 days',
    schedule_interval => INTERVAL '1 day',
    if_not_exists => TRUE
);

-- ============================================================
-- Retention policies
-- ============================================================

-- Raw: 30 days
-- TimescaleDB's retention policy drops whole chunks atomically (O(1)).
-- Do NOT use DELETE WHERE time < X on this table — it competes with
-- drop_chunks() via a lock convoy and is orders of magnitude more
-- expensive (Issue 5).
SELECT add_retention_policy(
    'service_metrics',
    INTERVAL '30 days',
    if_not_exists => TRUE
);

-- Hourly aggregate: 90 days
-- TODO(metrics): Issue 10 — retention policy chunk drops may race with
-- concurrent query_range() calls in TimescaleDB < 2.10, producing
-- "could not open relation with OID ..." errors. Upgrade to TimescaleDB
-- ≥ 2.10 for MVCC-safe chunk drops, or add a single retry in query_range().
SELECT add_retention_policy(
    'service_metrics_hourly',
    INTERVAL '90 days',
    if_not_exists => TRUE
);

-- Daily aggregate: 1 year
-- Kept primarily for long-term capacity/growth trends (storage size, DB size).
-- Operational metrics rarely need multi-year history, so 1 year is plenty.
SELECT add_retention_policy(
    'service_metrics_daily',
    INTERVAL '365 days',
    if_not_exists => TRUE
);
"#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
DROP MATERIALIZED VIEW IF EXISTS service_metrics_daily CASCADE;
DROP MATERIALIZED VIEW IF EXISTS service_metrics_hourly CASCADE;
DROP TABLE IF EXISTS service_metrics_histogram CASCADE;
DROP TABLE IF EXISTS service_metrics CASCADE;
"#,
        )
        .await?;

        Ok(())
    }
}
