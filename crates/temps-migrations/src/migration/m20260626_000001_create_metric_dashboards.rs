//! Migration to create the `metric_dashboards` table.
//!
//! Backs the per-project saved metric dashboards feature. Each row holds a
//! `name` and a typed `layout` (a `DashboardLayout` struct serialized to JSONB
//! by the `temps-otel` service layer). This is config/metadata — Postgres,
//! never ClickHouse.
//!
//! The composite index `(project_id, created_at DESC)` serves the list view,
//! which is always scoped by project and ordered by `created_at` descending —
//! mirroring `otel_trace_summaries (project_id, start_time DESC)`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(MetricDashboards::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(MetricDashboards::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(MetricDashboards::ProjectId)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(MetricDashboards::Name).text().not_null())
                    .col(
                        ColumnDef::new(MetricDashboards::Layout)
                            .json_binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MetricDashboards::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(MetricDashboards::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // Composite index serving the list view: scoped by project, ordered by
        // created_at DESC. SchemaManager's Index builder can't express DESC
        // ordering, so we use raw DDL like the otel_trace_summaries migration.
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_metric_dashboards_project_created \
                 ON metric_dashboards (project_id, created_at DESC);",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(MetricDashboards::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum MetricDashboards {
    Table,
    Id,
    ProjectId,
    Name,
    Layout,
    CreatedAt,
    UpdatedAt,
}
