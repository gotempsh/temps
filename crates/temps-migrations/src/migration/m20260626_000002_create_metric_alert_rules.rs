//! Migration to create the `metric_alert_rules` table.
//!
//! Backs the first-class, metric-centric alert rules feature. Each row defines a
//! signal (project + metric + aggregation) plus a polymorphic, versionable
//! detector (`detection_config`, jsonb) the background evaluator checks on an
//! interval. This is config/metadata — Postgres, never ClickHouse.
//!
//! Two indexes serve the access patterns: `(project_id)` for the per-project list
//! view, and `(project_id, enabled)` for the evaluator's enabled-rules scan.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(MetricAlertRules::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(MetricAlertRules::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(MetricAlertRules::ProjectId)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(MetricAlertRules::Name).text().not_null())
                    .col(
                        ColumnDef::new(MetricAlertRules::MetricName)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MetricAlertRules::Aggregation)
                            .text()
                            .not_null(),
                    )
                    // Coarse detector discriminator (static|anomaly|forecast|
                    // outlier|auto_watch). A plain string, so new kinds need no
                    // ALTER TYPE.
                    .col(
                        ColumnDef::new(MetricAlertRules::DetectionKind)
                            .text()
                            .not_null()
                            .default("static"),
                    )
                    // Typed-in-Rust detector definition stored as jsonb. The
                    // comparator/threshold of a static rule live here (kind=static);
                    // anomaly/forecast/outlier params land here too — a new detector
                    // family is code-only, never a migration.
                    .col(
                        ColumnDef::new(MetricAlertRules::DetectionConfig)
                            .json_binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MetricAlertRules::WindowSecs)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MetricAlertRules::ForDurationSecs)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(MetricAlertRules::Severity).text().not_null())
                    .col(
                        ColumnDef::new(MetricAlertRules::Enabled)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(MetricAlertRules::LastState)
                            .text()
                            .not_null()
                            .default("unknown"),
                    )
                    .col(ColumnDef::new(MetricAlertRules::LastValue).double())
                    .col(
                        ColumnDef::new(MetricAlertRules::LastEvaluatedAt)
                            .timestamp_with_time_zone(),
                    )
                    .col(
                        ColumnDef::new(MetricAlertRules::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(MetricAlertRules::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // Per-project list index plus an evaluator-scan index on (project_id,
        // enabled). Raw DDL mirrors the metric_dashboards migration.
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_metric_alert_rules_project \
                 ON metric_alert_rules (project_id); \
                 CREATE INDEX IF NOT EXISTS idx_metric_alert_rules_project_enabled \
                 ON metric_alert_rules (project_id, enabled);",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(MetricAlertRules::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum MetricAlertRules {
    Table,
    Id,
    ProjectId,
    Name,
    MetricName,
    Aggregation,
    DetectionKind,
    DetectionConfig,
    WindowSecs,
    ForDurationSecs,
    Severity,
    Enabled,
    LastState,
    LastValue,
    LastEvaluatedAt,
    CreatedAt,
    UpdatedAt,
}
