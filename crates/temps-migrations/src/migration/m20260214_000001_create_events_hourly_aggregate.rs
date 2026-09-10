// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Migration to create a TimescaleDB continuous aggregate for the events hypertable.
//!
//! Creates `events_hourly` materialized view that pre-computes hourly counts per project:
//! - unique visitors (COUNT DISTINCT visitor_id)
//! - unique sessions (COUNT DISTINCT session_id)
//! - page views (COUNT with event_type = 'page_view' filter)
//! - total events (COUNT *)
//!
//! This eliminates full table scans on the raw `events` hypertable for dashboard
//! analytics queries, reducing query time from seconds to milliseconds.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Create continuous aggregate: events_hourly
        // Pre-computes hourly visitor/session/pageview/event counts per project.
        //
        // Note: TimescaleDB continuous aggregates only support a subset of SQL.
        // COUNT(DISTINCT ...) is supported in TimescaleDB 2.7+.
        // FILTER (WHERE ...) is supported in continuous aggregates.
        let create_aggregate_sql = r#"
            CREATE MATERIALIZED VIEW events_hourly
            WITH (timescaledb.continuous) AS
            SELECT
                time_bucket('1 hour', timestamp) AS bucket,
                project_id,
                COUNT(DISTINCT visitor_id) FILTER (WHERE visitor_id IS NOT NULL) AS unique_visitors,
                COUNT(DISTINCT session_id) AS unique_sessions,
                COUNT(*) FILTER (WHERE event_type = 'page_view') AS page_views,
                COUNT(*) AS total_events
            FROM events
            GROUP BY bucket, project_id
            WITH NO DATA;
        "#;

        manager
            .get_connection()
            .execute_unprepared(create_aggregate_sql)
            .await?;

        // Create index for efficient per-project time-range queries
        let create_index_sql = r#"
            CREATE INDEX IF NOT EXISTS idx_events_hourly_project_bucket
                ON events_hourly (project_id, bucket DESC);
        "#;

        manager
            .get_connection()
            .execute_unprepared(create_index_sql)
            .await?;

        // Add refresh policy: refresh every 10 minutes, covering the last 3 hours,
        // with a 1-hour end offset (recent data may still be arriving).
        // This means data older than 1 hour is always up-to-date within 10 minutes.
        let add_policy_sql = r#"
            SELECT add_continuous_aggregate_policy('events_hourly',
                start_offset => INTERVAL '3 hours',
                end_offset => INTERVAL '1 hour',
                schedule_interval => INTERVAL '10 minutes');
        "#;

        manager
            .get_connection()
            .execute_unprepared(add_policy_sql)
            .await?;

        // Note: Backfill of historical data is handled by `run_post_migration_backfill()`
        // in temps-database/src/connection.rs, which runs AFTER the migration transaction
        // commits. `CALL refresh_continuous_aggregate()` cannot run inside a transaction
        // block, so it must be executed outside the migration.
        //
        // The refresh policy also automatically populates new data going forward.

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Remove the refresh policy first (required before dropping the view)
        let remove_policy_sql = r#"
            SELECT remove_continuous_aggregate_policy('events_hourly', if_exists => true);
        "#;

        manager
            .get_connection()
            .execute_unprepared(remove_policy_sql)
            .await?;

        // Drop the continuous aggregate materialized view
        let drop_sql = r#"
            DROP MATERIALIZED VIEW IF EXISTS events_hourly CASCADE;
        "#;

        manager
            .get_connection()
            .execute_unprepared(drop_sql)
            .await?;

        Ok(())
    }
}
