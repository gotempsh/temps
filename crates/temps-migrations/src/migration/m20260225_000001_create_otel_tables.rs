//! Migration to create OpenTelemetry tables for metrics, traces, and logs.
//!
//! Creates:
//! - `otel_metrics` hypertable for metric data points
//! - `otel_spans` hypertable for trace spans
//! - `otel_log_events` hypertable for log records
//! - `otel_insights` table for anomaly insights
//! - `otel_health_summaries` table for pre-computed health data
//! - Continuous aggregates for 1-minute and 1-hour metric rollups
//! - Compression, retention, and refresh policies

use sea_orm::DatabaseBackend;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // ── otel_metrics ────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("otel_metrics"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .big_integer()
                            .not_null()
                            .auto_increment(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("deployment_id")).integer().null())
                    .col(ColumnDef::new(Alias::new("service_name")).text().not_null())
                    .col(ColumnDef::new(Alias::new("service_version")).text().null())
                    .col(
                        ColumnDef::new(Alias::new("deployment_environment"))
                            .text()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("metric_name")).text().not_null())
                    .col(ColumnDef::new(Alias::new("metric_type")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("unit"))
                            .text()
                            .not_null()
                            .default(""),
                    )
                    .col(
                        ColumnDef::new(Alias::new("timestamp"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("value")).double().null())
                    .col(
                        ColumnDef::new(Alias::new("histogram_count"))
                            .big_integer()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("histogram_sum")).double().null())
                    .col(ColumnDef::new(Alias::new("histogram_min")).double().null())
                    .col(ColumnDef::new(Alias::new("histogram_max")).double().null())
                    .col(
                        ColumnDef::new(Alias::new("histogram_bounds"))
                            .json_binary()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("histogram_bucket_counts"))
                            .json_binary()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("attributes"))
                            .json_binary()
                            .not_null()
                            .default("{}"),
                    )
                    .primary_key(
                        Index::create()
                            .col(Alias::new("id"))
                            .col(Alias::new("timestamp")),
                    )
                    .to_owned(),
            )
            .await?;

        // ── otel_spans ──────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("otel_spans"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .big_integer()
                            .not_null()
                            .auto_increment(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("deployment_id")).integer().null())
                    .col(ColumnDef::new(Alias::new("service_name")).text().not_null())
                    .col(ColumnDef::new(Alias::new("service_version")).text().null())
                    .col(
                        ColumnDef::new(Alias::new("deployment_environment"))
                            .text()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("trace_id")).text().not_null())
                    .col(ColumnDef::new(Alias::new("span_id")).text().not_null())
                    .col(ColumnDef::new(Alias::new("parent_span_id")).text().null())
                    .col(ColumnDef::new(Alias::new("name")).text().not_null())
                    .col(ColumnDef::new(Alias::new("kind")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("start_time"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("end_time"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("duration_ms"))
                            .double()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("status_code"))
                            .text()
                            .not_null()
                            .default("UNSET"),
                    )
                    .col(
                        ColumnDef::new(Alias::new("status_message"))
                            .text()
                            .not_null()
                            .default(""),
                    )
                    .col(
                        ColumnDef::new(Alias::new("attributes"))
                            .json_binary()
                            .not_null()
                            .default("{}"),
                    )
                    .col(
                        ColumnDef::new(Alias::new("events"))
                            .json_binary()
                            .not_null()
                            .default("[]"),
                    )
                    .primary_key(
                        Index::create()
                            .col(Alias::new("id"))
                            .col(Alias::new("start_time")),
                    )
                    .to_owned(),
            )
            .await?;

        // ── otel_log_events ─────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("otel_log_events"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .big_integer()
                            .not_null()
                            .auto_increment(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("deployment_id")).integer().null())
                    .col(ColumnDef::new(Alias::new("service_name")).text().not_null())
                    .col(ColumnDef::new(Alias::new("service_version")).text().null())
                    .col(
                        ColumnDef::new(Alias::new("deployment_environment"))
                            .text()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("timestamp"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("observed_timestamp"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("severity")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("severity_text"))
                            .text()
                            .not_null()
                            .default(""),
                    )
                    .col(ColumnDef::new(Alias::new("body")).text().not_null())
                    .col(ColumnDef::new(Alias::new("trace_id")).text().null())
                    .col(ColumnDef::new(Alias::new("span_id")).text().null())
                    .col(
                        ColumnDef::new(Alias::new("attributes"))
                            .json_binary()
                            .not_null()
                            .default("{}"),
                    )
                    .primary_key(
                        Index::create()
                            .col(Alias::new("id"))
                            .col(Alias::new("timestamp")),
                    )
                    .to_owned(),
            )
            .await?;

        // ── otel_insights (regular table, not hypertable) ───────────
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("otel_insights"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("environment")).text().null())
                    .col(ColumnDef::new(Alias::new("service_name")).text().not_null())
                    .col(ColumnDef::new(Alias::new("severity")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("status"))
                            .text()
                            .not_null()
                            .default("active"),
                    )
                    .col(ColumnDef::new(Alias::new("title")).text().not_null())
                    .col(ColumnDef::new(Alias::new("description")).text().not_null())
                    .col(ColumnDef::new(Alias::new("metric_name")).text().null())
                    .col(
                        ColumnDef::new(Alias::new("correlated_deploy_id"))
                            .integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("anomaly_ids"))
                            .json_binary()
                            .not_null()
                            .default("[]"),
                    )
                    .col(
                        ColumnDef::new(Alias::new("started_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("resolved_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // ── otel_health_summaries (regular table, not hypertable) ───
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("otel_health_summaries"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("environment_id"))
                            .integer()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("service_name")).text().not_null())
                    .col(ColumnDef::new(Alias::new("status")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("uptime_pct"))
                            .double()
                            .not_null()
                            .default(0.0),
                    )
                    .col(
                        ColumnDef::new(Alias::new("error_rate"))
                            .double()
                            .not_null()
                            .default(0.0),
                    )
                    .col(
                        ColumnDef::new(Alias::new("p95_latency_ms"))
                            .double()
                            .not_null()
                            .default(0.0),
                    )
                    .col(
                        ColumnDef::new(Alias::new("cpu_usage_pct"))
                            .double()
                            .not_null()
                            .default(0.0),
                    )
                    .col(
                        ColumnDef::new(Alias::new("memory_usage_pct"))
                            .double()
                            .not_null()
                            .default(0.0),
                    )
                    .col(
                        ColumnDef::new(Alias::new("last_deploy_id"))
                            .integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("last_deploy_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("computed_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // ── Indexes for non-hypertable tables ───────────────────────

        // otel_insights indexes
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_otel_insights_project_status")
                    .table(Alias::new("otel_insights"))
                    .col(Alias::new("project_id"))
                    .col(Alias::new("status"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_otel_insights_project_created")
                    .table(Alias::new("otel_insights"))
                    .col(Alias::new("project_id"))
                    .col(Alias::new("created_at"))
                    .to_owned(),
            )
            .await?;

        // otel_health_summaries indexes
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_otel_health_project_service")
                    .table(Alias::new("otel_health_summaries"))
                    .col(Alias::new("project_id"))
                    .col(Alias::new("service_name"))
                    .col(Alias::new("computed_at"))
                    .to_owned(),
            )
            .await?;

        // ── TimescaleDB hypertables, compression, retention, aggregates
        if manager.get_database_backend() == DatabaseBackend::Postgres {
            let timescale_sql = r#"
                -- ── otel_metrics hypertable ─────────────────────────
                SELECT create_hypertable('otel_metrics', 'timestamp',
                    partitioning_column => 'id',
                    number_partitions => 4,
                    chunk_time_interval => INTERVAL '1 day',
                    if_not_exists => TRUE);

                CREATE INDEX IF NOT EXISTS idx_otel_metrics_project_timestamp
                    ON otel_metrics (project_id, timestamp DESC);
                CREATE INDEX IF NOT EXISTS idx_otel_metrics_project_name_timestamp
                    ON otel_metrics (project_id, metric_name, timestamp DESC);
                CREATE INDEX IF NOT EXISTS idx_otel_metrics_service_timestamp
                    ON otel_metrics (service_name, timestamp DESC);

                ALTER TABLE otel_metrics SET (
                    timescaledb.compress,
                    timescaledb.compress_segmentby = 'project_id,metric_name,service_name',
                    timescaledb.compress_orderby = 'timestamp DESC'
                );
                SELECT add_compression_policy('otel_metrics', INTERVAL '7 days', if_not_exists => TRUE);
                SELECT add_retention_policy('otel_metrics', INTERVAL '90 days', if_not_exists => TRUE);

                -- ── otel_spans hypertable ───────────────────────────
                SELECT create_hypertable('otel_spans', 'start_time',
                    partitioning_column => 'id',
                    number_partitions => 4,
                    chunk_time_interval => INTERVAL '1 day',
                    if_not_exists => TRUE);

                CREATE INDEX IF NOT EXISTS idx_otel_spans_project_start
                    ON otel_spans (project_id, start_time DESC);
                CREATE INDEX IF NOT EXISTS idx_otel_spans_trace_id
                    ON otel_spans (trace_id, start_time DESC);
                CREATE INDEX IF NOT EXISTS idx_otel_spans_service_start
                    ON otel_spans (service_name, start_time DESC);
                CREATE INDEX IF NOT EXISTS idx_otel_spans_status_start
                    ON otel_spans (status_code, start_time DESC);

                ALTER TABLE otel_spans SET (
                    timescaledb.compress,
                    timescaledb.compress_segmentby = 'project_id,service_name,trace_id',
                    timescaledb.compress_orderby = 'start_time DESC'
                );
                SELECT add_compression_policy('otel_spans', INTERVAL '7 days', if_not_exists => TRUE);
                SELECT add_retention_policy('otel_spans', INTERVAL '90 days', if_not_exists => TRUE);

                -- ── otel_log_events hypertable ──────────────────────
                SELECT create_hypertable('otel_log_events', 'timestamp',
                    partitioning_column => 'id',
                    number_partitions => 4,
                    chunk_time_interval => INTERVAL '1 day',
                    if_not_exists => TRUE);

                CREATE INDEX IF NOT EXISTS idx_otel_logs_project_timestamp
                    ON otel_log_events (project_id, timestamp DESC);
                CREATE INDEX IF NOT EXISTS idx_otel_logs_severity_timestamp
                    ON otel_log_events (severity, timestamp DESC);
                CREATE INDEX IF NOT EXISTS idx_otel_logs_service_timestamp
                    ON otel_log_events (service_name, timestamp DESC);
                CREATE INDEX IF NOT EXISTS idx_otel_logs_trace_id
                    ON otel_log_events (trace_id, timestamp DESC)
                    WHERE trace_id IS NOT NULL;

                ALTER TABLE otel_log_events SET (
                    timescaledb.compress,
                    timescaledb.compress_segmentby = 'project_id,severity,service_name',
                    timescaledb.compress_orderby = 'timestamp DESC'
                );
                SELECT add_compression_policy('otel_log_events', INTERVAL '7 days', if_not_exists => TRUE);
                SELECT add_retention_policy('otel_log_events', INTERVAL '90 days', if_not_exists => TRUE);
            "#;

            manager
                .get_connection()
                .execute_unprepared(timescale_sql)
                .await
                .map_err(|e| {
                    DbErr::Custom(format!(
                        "Failed to configure TimescaleDB for OTel tables: {}",
                        e
                    ))
                })?;

            // ── Continuous aggregate: otel_metrics_1min ──────────────
            let agg_1min_sql = r#"
                CREATE MATERIALIZED VIEW otel_metrics_1min
                WITH (timescaledb.continuous) AS
                SELECT
                    time_bucket('1 minute', timestamp) AS bucket,
                    project_id,
                    service_name,
                    metric_name,
                    AVG(value) AS avg_value,
                    MIN(value) AS min_value,
                    MAX(value) AS max_value,
                    COUNT(*) AS count
                FROM otel_metrics
                WHERE value IS NOT NULL
                GROUP BY bucket, project_id, service_name, metric_name
                WITH NO DATA;

                CREATE INDEX IF NOT EXISTS idx_otel_metrics_1min_project_metric
                    ON otel_metrics_1min (project_id, metric_name, bucket DESC);

                SELECT add_continuous_aggregate_policy('otel_metrics_1min',
                    start_offset => INTERVAL '1 hour',
                    end_offset => INTERVAL '1 minute',
                    schedule_interval => INTERVAL '1 minute');
            "#;

            manager
                .get_connection()
                .execute_unprepared(agg_1min_sql)
                .await
                .map_err(|e| {
                    DbErr::Custom(format!(
                        "Failed to create otel_metrics_1min aggregate: {}",
                        e
                    ))
                })?;

            // ── Continuous aggregate: otel_metrics_1hr ───────────────
            let agg_1hr_sql = r#"
                CREATE MATERIALIZED VIEW otel_metrics_1hr
                WITH (timescaledb.continuous) AS
                SELECT
                    time_bucket('1 hour', timestamp) AS bucket,
                    project_id,
                    service_name,
                    metric_name,
                    AVG(value) AS avg_value,
                    MIN(value) AS min_value,
                    MAX(value) AS max_value,
                    COUNT(*) AS count
                FROM otel_metrics
                WHERE value IS NOT NULL
                GROUP BY bucket, project_id, service_name, metric_name
                WITH NO DATA;

                CREATE INDEX IF NOT EXISTS idx_otel_metrics_1hr_project_metric
                    ON otel_metrics_1hr (project_id, metric_name, bucket DESC);

                SELECT add_continuous_aggregate_policy('otel_metrics_1hr',
                    start_offset => INTERVAL '3 hours',
                    end_offset => INTERVAL '1 hour',
                    schedule_interval => INTERVAL '10 minutes');
            "#;

            manager
                .get_connection()
                .execute_unprepared(agg_1hr_sql)
                .await
                .map_err(|e| {
                    DbErr::Custom(format!(
                        "Failed to create otel_metrics_1hr aggregate: {}",
                        e
                    ))
                })?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() == DatabaseBackend::Postgres {
            // Remove continuous aggregate policies and views (order matters)
            let teardown_sql = r#"
                SELECT remove_continuous_aggregate_policy('otel_metrics_1hr', if_exists => true);
                DROP MATERIALIZED VIEW IF EXISTS otel_metrics_1hr CASCADE;

                SELECT remove_continuous_aggregate_policy('otel_metrics_1min', if_exists => true);
                DROP MATERIALIZED VIEW IF EXISTS otel_metrics_1min CASCADE;

                -- Remove compression and retention policies before dropping tables
                SELECT remove_compression_policy('otel_log_events', if_exists => true);
                SELECT remove_retention_policy('otel_log_events', if_exists => true);

                SELECT remove_compression_policy('otel_spans', if_exists => true);
                SELECT remove_retention_policy('otel_spans', if_exists => true);

                SELECT remove_compression_policy('otel_metrics', if_exists => true);
                SELECT remove_retention_policy('otel_metrics', if_exists => true);
            "#;

            manager
                .get_connection()
                .execute_unprepared(teardown_sql)
                .await?;
        }

        // Drop tables in reverse dependency order
        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("otel_health_summaries"))
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(Alias::new("otel_insights")).to_owned())
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("otel_log_events"))
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(Alias::new("otel_spans")).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Alias::new("otel_metrics")).to_owned())
            .await?;

        Ok(())
    }
}
