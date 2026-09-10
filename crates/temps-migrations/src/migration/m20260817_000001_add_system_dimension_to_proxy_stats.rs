// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Makes the proxy dashboard aggregate distinguish user traffic from Temps'
//! own status-monitor requests.
//!
//! The project dashboard uses this aggregate for its request totals and
//! sparkline. Without `is_system_request` as a grouping dimension, the query
//! cannot remove the one status check Temps performs every minute, which makes
//! idle projects appear to receive 1,440 requests per day.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const CREATE_WITH_SYSTEM_DIMENSION: &str = r#"
    CREATE MATERIALIZED VIEW proxy_logs_stats_1m
    WITH (timescaledb.continuous) AS
    SELECT
        time_bucket('1 minute', timestamp) AS bucket,
        project_id,
        environment_id,
        is_bot,
        is_system_request,
        COUNT(*) AS request_count,
        COUNT(*) FILTER (WHERE status_code >= 400) AS error_4xx_plus_count,
        COUNT(*) FILTER (WHERE status_code >= 500) AS error_5xx_plus_count,
        COUNT(response_time_ms) AS response_time_count,
        SUM(response_time_ms) AS sum_response_time_ms,
        SUM(request_size_bytes) AS sum_request_bytes,
        SUM(response_size_bytes) AS sum_response_bytes
    FROM proxy_logs
    GROUP BY bucket, project_id, environment_id, is_bot, is_system_request
    WITH NO DATA;
"#;

const CREATE_WITHOUT_SYSTEM_DIMENSION: &str = r#"
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
"#;

async fn replace_proxy_stats_view(
    manager: &SchemaManager<'_>,
    create_sql: &str,
) -> Result<(), DbErr> {
    let db = manager.get_connection();

    db.execute_unprepared(
        "SELECT remove_retention_policy('proxy_logs_stats_1m', if_exists => TRUE);",
    )
    .await?;
    db.execute_unprepared(
        "SELECT remove_continuous_aggregate_policy('proxy_logs_stats_1m', if_exists => TRUE);",
    )
    .await?;
    db.execute_unprepared("DROP MATERIALIZED VIEW IF EXISTS proxy_logs_stats_1m CASCADE;")
        .await?;
    db.execute_unprepared(create_sql).await?;
    db.execute_unprepared(
        "ALTER MATERIALIZED VIEW proxy_logs_stats_1m SET (timescaledb.materialized_only = false);",
    )
    .await?;
    db.execute_unprepared(
        "CREATE INDEX IF NOT EXISTS idx_proxy_logs_stats_1m_project_bucket \
         ON proxy_logs_stats_1m (project_id, bucket DESC);",
    )
    .await?;
    db.execute_unprepared(
        "SELECT add_continuous_aggregate_policy('proxy_logs_stats_1m', \
         start_offset => INTERVAL '2 hours', end_offset => INTERVAL '1 minute', \
         schedule_interval => INTERVAL '1 minute');",
    )
    .await?;
    db.execute_unprepared(
        "SELECT add_retention_policy('proxy_logs_stats_1m', \
         drop_after => INTERVAL '30 days', if_not_exists => TRUE);",
    )
    .await?;

    Ok(())
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        replace_proxy_stats_view(manager, CREATE_WITH_SYSTEM_DIMENSION).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        replace_proxy_stats_view(manager, CREATE_WITHOUT_SYSTEM_DIMENSION).await
    }
}
