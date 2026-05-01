use sea_orm_migration::prelude::*;

/// Add cross-source correlation columns so the unified Observe page can join
/// runtime logs / requests / spans / errors / revenue without follow-up
/// queries. All columns are nullable; old rows simply render without
/// correlation links.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // proxy_logs: trace_id (from OTel context) + error_group_id (when the
        // request produced a captured exception)
        manager
            .alter_table(
                Table::alter()
                    .table(ProxyLogs::Table)
                    .add_column(ColumnDef::new(ProxyLogs::TraceId).text().null())
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ProxyLogs::Table)
                    .add_column(ColumnDef::new(ProxyLogs::ErrorGroupId).integer().null())
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_proxy_logs_project_trace")
                    .table(ProxyLogs::Table)
                    .col(ProxyLogs::ProjectId)
                    .col(ProxyLogs::TraceId)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_proxy_logs_error_group")
                    .table(ProxyLogs::Table)
                    .col(ProxyLogs::ErrorGroupId)
                    .to_owned(),
            )
            .await?;

        // revenue_events: link the business event back to the deployment /
        // environment / trace it belongs to (when the SDK provides headers).
        manager
            .alter_table(
                Table::alter()
                    .table(RevenueEvents::Table)
                    .add_column(ColumnDef::new(RevenueEvents::DeploymentId).integer().null())
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(RevenueEvents::Table)
                    .add_column(ColumnDef::new(RevenueEvents::EnvironmentId).integer().null())
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(RevenueEvents::Table)
                    .add_column(ColumnDef::new(RevenueEvents::TraceId).text().null())
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_revenue_events_project_occurred")
                    .table(RevenueEvents::Table)
                    .col(RevenueEvents::ProjectId)
                    .col(RevenueEvents::OccurredAt)
                    .to_owned(),
            )
            .await?;

        // error_events: promote trace_id from the JSON `data.trace.trace_id`
        // blob to a top-level indexed column. Cheap to maintain at write
        // time; lets the merge query join by trace_id without a JSON probe.
        manager
            .alter_table(
                Table::alter()
                    .table(ErrorEvents::Table)
                    .add_column(ColumnDef::new(ErrorEvents::TraceIdIndexed).text().null())
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_error_events_project_trace")
                    .table(ErrorEvents::Table)
                    .col(ErrorEvents::ProjectId)
                    .col(ErrorEvents::TraceIdIndexed)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name("idx_error_events_project_trace")
                    .table(ErrorEvents::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ErrorEvents::Table)
                    .drop_column(ErrorEvents::TraceIdIndexed)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name("idx_revenue_events_project_occurred")
                    .table(RevenueEvents::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(RevenueEvents::Table)
                    .drop_column(RevenueEvents::TraceId)
                    .drop_column(RevenueEvents::EnvironmentId)
                    .drop_column(RevenueEvents::DeploymentId)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name("idx_proxy_logs_error_group")
                    .table(ProxyLogs::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name("idx_proxy_logs_project_trace")
                    .table(ProxyLogs::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ProxyLogs::Table)
                    .drop_column(ProxyLogs::ErrorGroupId)
                    .drop_column(ProxyLogs::TraceId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum ProxyLogs {
    Table,
    ProjectId,
    TraceId,
    ErrorGroupId,
}

#[derive(DeriveIden)]
enum RevenueEvents {
    Table,
    ProjectId,
    OccurredAt,
    DeploymentId,
    EnvironmentId,
    TraceId,
}

#[derive(DeriveIden)]
enum ErrorEvents {
    Table,
    ProjectId,
    TraceIdIndexed,
}
