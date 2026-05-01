use sea_orm_migration::prelude::*;

/// Add cross-source correlation columns so the unified Observe page can join
/// requests / spans / errors / revenue without follow-up queries. All
/// columns are nullable; old rows simply render without correlation links.
///
/// **TimescaleDB caveat**: `proxy_logs` is a hypertable with a 7-day
/// compression policy (see `m20260225_000001_add_proxy_logs_retention`).
/// Once chunks compress, `ALTER TABLE … ADD COLUMN` against the parent
/// fails with `chunk not found` because the per-chunk schema can't be
/// rewritten in place. We work around this by:
///   1. Removing the compression policy (so the background worker stops
///      compressing new chunks while we work).
///   2. Decompressing every existing chunk (cheap relative to keeping the
///      hypertable broken).
///   3. Running the ALTER, which now succeeds across every chunk.
///   4. Re-adding the same compression policy with `if_not_exists`.
///
/// `revenue_events` is also a hypertable but has no compression policy
/// today, so a plain `ALTER` is safe. `error_events` is a regular table.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // ── proxy_logs (compressed hypertable) ──────────────────────────
        // 1. Pause the compression policy. Idempotent: `if_exists` returns
        //    NULL when the policy isn't present (e.g. fresh installs that
        //    haven't run the retention migration yet).
        db.execute_unprepared("SELECT remove_compression_policy('proxy_logs', if_exists => TRUE)")
            .await?;

        // 2. Decompress every chunk so the next ALTER can rewrite per-chunk
        //    schemas. Skips the call entirely on installs that don't have
        //    the TimescaleDB extension or haven't compressed anything yet
        //    (the function returns 0 rows in that case — no error).
        db.execute_unprepared(
            "DO $$
             BEGIN
               IF EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'timescaledb') THEN
                 PERFORM decompress_chunk(c, if_compressed => TRUE)
                 FROM show_chunks('proxy_logs') c;
               END IF;
             END$$",
        )
        .await?;

        // 3. Now safe to ALTER.
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

        // 4. Re-add the compression policy with the original 7-day window.
        //    `if_not_exists` makes this safe to re-run on installs that
        //    never had the policy (no-op).
        db.execute_unprepared(
            "DO $$
             BEGIN
               IF EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'timescaledb') THEN
                 PERFORM add_compression_policy('proxy_logs', INTERVAL '7 days', if_not_exists => TRUE);
               END IF;
             END$$",
        )
        .await?;

        // ── revenue_events (uncompressed hypertable) ────────────────────
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
                    .add_column(
                        ColumnDef::new(RevenueEvents::EnvironmentId)
                            .integer()
                            .null(),
                    )
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

        // ── error_events (regular table) ────────────────────────────────
        // Promote `data.trace.trace_id` from JSONB to a top-level indexed
        // column. Cheap to maintain at write time; lets the merge query
        // join by trace_id without a JSON probe.
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
        let db = manager.get_connection();

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

        // Same compressed-hypertable dance for proxy_logs on the way down.
        db.execute_unprepared("SELECT remove_compression_policy('proxy_logs', if_exists => TRUE)")
            .await?;
        db.execute_unprepared(
            "DO $$
             BEGIN
               IF EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'timescaledb') THEN
                 PERFORM decompress_chunk(c, if_compressed => TRUE)
                 FROM show_chunks('proxy_logs') c;
               END IF;
             END$$",
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

        db.execute_unprepared(
            "DO $$
             BEGIN
               IF EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'timescaledb') THEN
                 PERFORM add_compression_policy('proxy_logs', INTERVAL '7 days', if_not_exists => TRUE);
               END IF;
             END$$",
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
