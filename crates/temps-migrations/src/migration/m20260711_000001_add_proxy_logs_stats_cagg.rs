//! Continuous aggregate for per-project proxy-log statistics.
//!
//! The proxy dashboard endpoints (`GET /proxy-logs/stats/projects-health` and
//! `GET /proxy-logs/stats/time-buckets`) aggregate raw `proxy_logs` rows at
//! request time: `COUNT(*)`, error counts, and `AVG(response_time_ms)` over
//! the selected window. On high-traffic installs (millions of rows per hour)
//! that is a 20s+ query even with the `(project_id, timestamp DESC)` index —
//! the index narrows the scan but every matching row still needs a heap fetch
//! for `response_time_ms` / `status_code`.
//!
//! `proxy_logs_stats_1m` pre-computes 1-minute buckets per
//! `(project_id, environment_id, is_bot)`, so those endpoints read
//! O(minutes × projects) aggregate rows instead of O(requests) raw rows.
//! Sums and counts are stored (never averages) so buckets roll up losslessly
//! to any coarser interval; `avg = sum_response_time_ms / response_time_count`
//! reproduces the raw `AVG(response_time_ms)` exactly (both ignore NULLs).
//!
//! Real-time aggregation (`materialized_only = false`) keeps results fresh:
//! queries transparently union the materialized buckets with raw rows newer
//! than the refresh watermark, which the every-minute refresh policy keeps to
//! ~1–2 minutes of raw data.
//!
//! Backfill of pre-existing data is handled by `run_post_migration_backfill()`
//! in temps-database (a `CALL refresh_continuous_aggregate()` cannot run
//! inside the migration transaction). It refreshes in 1-day windows, newest
//! first, so recent dashboard ranges become correct within the first chunk
//! and no single refresh transaction has to chew through the whole retention
//! window at once. Regions the backfill hasn't reached yet under-report once
//! the refresh policy has advanced the watermark; that window is transient
//! and self-heals as the chunks complete.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // `WITH NO DATA` is what allows creation inside the migration
        // transaction; populating happens via the refresh policy + the
        // post-migration backfill.
        db.execute_unprepared(
            r#"
            CREATE MATERIALIZED VIEW proxy_logs_stats_1m
            WITH (timescaledb.continuous) AS
            SELECT
                time_bucket('1 minute', timestamp) AS bucket,
                project_id,
                environment_id,
                is_bot,
                COUNT(*) AS request_count,
                COUNT(*) FILTER (WHERE status_code >= 400) AS error_4xx_plus_count,
                COUNT(*) FILTER (WHERE status_code >= 500) AS error_5xx_plus_count,
                COUNT(response_time_ms) AS response_time_count,
                SUM(response_time_ms) AS sum_response_time_ms,
                SUM(request_size_bytes) AS sum_request_bytes,
                SUM(response_size_bytes) AS sum_response_bytes
            FROM proxy_logs
            GROUP BY bucket, project_id, environment_id, is_bot
            WITH NO DATA;
            "#,
        )
        .await?;

        // Real-time aggregation: query results include raw rows newer than
        // the materialization watermark, so dashboards never lag behind the
        // refresh schedule.
        db.execute_unprepared(
            "ALTER MATERIALIZED VIEW proxy_logs_stats_1m SET (timescaledb.materialized_only = false);",
        )
        .await?;

        // The projects-health query filters `project_id IN (…)` + bucket range.
        db.execute_unprepared(
            r#"
            CREATE INDEX IF NOT EXISTS idx_proxy_logs_stats_1m_project_bucket
                ON proxy_logs_stats_1m (project_id, bucket DESC);
            "#,
        )
        .await?;

        // Refresh every minute with a 1-minute end offset: the real-time
        // union then only has to scan ~1-2 minutes of raw rows. The 2-hour
        // start offset re-covers late-arriving rows from the batch writer.
        db.execute_unprepared(
            r#"
            SELECT add_continuous_aggregate_policy('proxy_logs_stats_1m',
                start_offset => INTERVAL '2 hours',
                end_offset => INTERVAL '1 minute',
                schedule_interval => INTERVAL '1 minute');
            "#,
        )
        .await?;

        // Match the raw table's 30-day retention
        // (m20260225_000001_add_proxy_logs_retention); the dashboard offers
        // at most a 7-day window.
        db.execute_unprepared(
            "SELECT add_retention_policy('proxy_logs_stats_1m', drop_after => INTERVAL '30 days', if_not_exists => TRUE);",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            "SELECT remove_retention_policy('proxy_logs_stats_1m', if_exists => TRUE);",
        )
        .await?;

        db.execute_unprepared(
            "SELECT remove_continuous_aggregate_policy('proxy_logs_stats_1m', if_exists => true);",
        )
        .await?;

        db.execute_unprepared("DROP MATERIALIZED VIEW IF EXISTS proxy_logs_stats_1m CASCADE;")
            .await?;

        Ok(())
    }
}
