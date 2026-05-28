use sea_orm_migration::prelude::*;

/// Speed up the proxy-logs listing endpoint (`GET /proxy-logs`).
///
/// The listing service runs two queries per page on the `proxy_logs`
/// hypertable: an unbounded `COUNT(*)` for the pagination total and an
/// `ORDER BY timestamp DESC LIMIT <page_size>` for the rows. On first load
/// the UI sends no time filter, so both queries span the full 30-day
/// retention window. The only pre-existing index usable for the sort was the
/// `(id, timestamp)` primary key, whose leading column is `id` — the planner
/// cannot walk it to satisfy `ORDER BY timestamp DESC`, so it fell back to a
/// full hypertable scan + sort (observed 8–10s).
///
/// Two indexes fix the common access patterns:
///   * `(timestamp DESC)` — the no-filter first load. Turns both the
///     `COUNT(*)` and the top-N fetch into an index scan instead of a
///     seq-scan-and-sort across every chunk.
///   * `(project_id, timestamp DESC)` — the project-scoped view, the most
///     common filtered case. This mirrors the compression layout
///     (`compress_segmentby = 'project_id'`, `compress_orderby =
///     'timestamp DESC'`), so it stays cheap as chunks compress.
///
/// On a TimescaleDB hypertable `CREATE INDEX` is applied per-chunk; Timescale
/// also propagates the index definition to future chunks automatically, so a
/// single statement covers existing and new data.
///
/// **Designed to be safely re-runnable on any prior state**: every statement
/// uses `IF NOT EXISTS`, covering installs where the index was built
/// out-of-band and partially-applied prior runs. See
/// `m20260502_000001_add_observe_correlation` for the operational note on
/// orphan chunks from raw `pg_dump`/`pg_restore` migrations — the same caveat
/// applies to any `CREATE INDEX` on a hypertable.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
DO $$
BEGIN
    CREATE INDEX IF NOT EXISTS idx_proxy_logs_timestamp_desc
        ON proxy_logs (timestamp DESC);

    CREATE INDEX IF NOT EXISTS idx_proxy_logs_project_timestamp
        ON proxy_logs (project_id, timestamp DESC);
END
$$;
"#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
DO $$
BEGIN
    DROP INDEX IF EXISTS idx_proxy_logs_project_timestamp;
    DROP INDEX IF EXISTS idx_proxy_logs_timestamp_desc;
END
$$;
"#,
        )
        .await?;

        Ok(())
    }
}
