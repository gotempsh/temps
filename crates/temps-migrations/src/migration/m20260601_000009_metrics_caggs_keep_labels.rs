//! Recreate the metrics continuous aggregates with `labels` in the GROUP BY.
//!
//! The original `service_metrics_hourly` / `service_metrics_daily` aggregates
//! grouped by `(bucket, source_kind, source_id, name, engine, environment)` —
//! **without `labels`**. That silently blended every label-series of a metric
//! into one averaged row. For Postgres, which emits per-`datname` series (cache
//! hit ratio, database size, deadlocks, …) plus an unlabelled instance-wide
//! aggregate, this made every chart over a range > 7 days (the path that reads
//! the aggregates) average all databases together — meaningless for a ratio and
//! double-counting a size.
//!
//! Adding `labels` to the GROUP BY preserves the per-series rows, so the
//! `query_range` label filter (prefer the unlabelled `{}` series when one
//! exists) works identically on the aggregates as it does on the raw table.
//!
//! A continuous aggregate's GROUP BY cannot be `ALTER`ed, so both views are
//! dropped and recreated. Dropping a CAGG also drops its refresh and retention
//! policies, so those are re-added at the CURRENT intervals (raw 30d / hourly
//! 90d / daily 365d — see m20260601_000006). Recreated `WITH NO DATA`; the
//! refresh policies re-materialise from the still-intact raw table (30d
//! retention), so recent history is rebuilt automatically. Rolled-up history
//! older than the raw retention window is not recoverable — acceptable, since
//! the pre-existing rows were wrong for multi-series metrics anyway.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Drop daily first (it reads from hourly), then hourly. CASCADE removes
        // the attached refresh/retention policies. IF EXISTS keeps this safe on
        // installs where a prior partial run already dropped one.
        db.execute_unprepared(
            r#"
DROP MATERIALIZED VIEW IF EXISTS service_metrics_daily CASCADE;
DROP MATERIALIZED VIEW IF EXISTS service_metrics_hourly CASCADE;
"#,
        )
        .await?;

        // Recreate hourly with `labels` in the projection and GROUP BY.
        db.execute_unprepared(
            r#"
CREATE MATERIALIZED VIEW IF NOT EXISTS service_metrics_hourly
WITH (timescaledb.continuous) AS
SELECT
    time_bucket('1 hour', time) AS bucket,
    source_kind,
    source_id,
    name,
    engine,
    environment,
    labels,
    AVG(value)   AS avg_value,
    MIN(value)   AS min_value,
    MAX(value)   AS max_value,
    COUNT(*)     AS sample_count
FROM service_metrics
GROUP BY bucket, source_kind, source_id, name, engine, environment, labels
WITH NO DATA;

SELECT add_continuous_aggregate_policy(
    'service_metrics_hourly',
    start_offset => INTERVAL '3 days',
    end_offset   => INTERVAL '1 day',
    schedule_interval => INTERVAL '1 hour',
    if_not_exists => TRUE
);

SELECT add_retention_policy(
    'service_metrics_hourly',
    INTERVAL '90 days',
    if_not_exists => TRUE
);
"#,
        )
        .await?;

        // Recreate daily (hierarchical, reads from hourly) with `labels`.
        db.execute_unprepared(
            r#"
CREATE MATERIALIZED VIEW IF NOT EXISTS service_metrics_daily
WITH (timescaledb.continuous) AS
SELECT
    time_bucket('1 day', bucket) AS bucket,
    source_kind,
    source_id,
    name,
    engine,
    environment,
    labels,
    AVG(avg_value)    AS avg_value,
    MIN(min_value)    AS min_value,
    MAX(max_value)    AS max_value,
    SUM(sample_count) AS sample_count
FROM service_metrics_hourly
GROUP BY time_bucket('1 day', bucket), source_kind, source_id, name, engine, environment, labels
WITH NO DATA;

SELECT add_continuous_aggregate_policy(
    'service_metrics_daily',
    start_offset => INTERVAL '7 days',
    end_offset   => INTERVAL '2 days',
    schedule_interval => INTERVAL '1 day',
    if_not_exists => TRUE
);

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

        // Revert to the label-less aggregates (the m20260601_000001 shape, with
        // the m20260601_000006 retention intervals).
        db.execute_unprepared(
            r#"
DROP MATERIALIZED VIEW IF EXISTS service_metrics_daily CASCADE;
DROP MATERIALIZED VIEW IF EXISTS service_metrics_hourly CASCADE;
"#,
        )
        .await?;

        db.execute_unprepared(
            r#"
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

SELECT add_continuous_aggregate_policy(
    'service_metrics_hourly',
    start_offset => INTERVAL '3 days',
    end_offset   => INTERVAL '1 day',
    schedule_interval => INTERVAL '1 hour',
    if_not_exists => TRUE
);

SELECT add_retention_policy(
    'service_metrics_hourly',
    INTERVAL '90 days',
    if_not_exists => TRUE
);
"#,
        )
        .await?;

        db.execute_unprepared(
            r#"
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

SELECT add_continuous_aggregate_policy(
    'service_metrics_daily',
    start_offset => INTERVAL '7 days',
    end_offset   => INTERVAL '2 days',
    schedule_interval => INTERVAL '1 day',
    if_not_exists => TRUE
);

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
}
