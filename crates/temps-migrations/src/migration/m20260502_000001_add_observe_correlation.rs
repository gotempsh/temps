use sea_orm_migration::prelude::*;

/// Add cross-source correlation columns so the unified Observe page can join
/// requests / spans / errors / revenue without follow-up queries. All
/// columns are nullable; old rows simply render without correlation links.
///
/// **Designed to be safely re-runnable on any prior state**, including:
///   * Fresh installs (no rows yet, extension may not be installed)
///   * Partially-applied prior runs (any subset of columns/indexes present)
///   * Already-fully-applied installs (re-runs are no-ops)
///   * Installs without TimescaleDB
///
/// Strategy: every step uses `IF NOT EXISTS` and is run via raw SQL. The
/// `ALTER TABLE ... ADD COLUMN IF NOT EXISTS` form has been supported since
/// Postgres 9.6 and works correctly on TimescaleDB hypertables — Timescale
/// propagates the new column to every chunk transparently, no per-chunk
/// enumeration required from us.
///
/// **What this deliberately does NOT do**:
///
///   * **No `decompress_chunk` loop**. We previously paused the compression
///     policy and decompressed every chunk before ALTER, on the assumption
///     that ALTER on a compressed hypertable would fail. In practice the
///     decompress loop itself raced with the background retention worker
///     (`drop_chunks` runs daily on `proxy_logs` past 30 days) and produced
///     `chunk not found` — confirmed in prod logs. `ALTER TABLE ... ADD
///     COLUMN` against a compressed hypertable works fine in current
///     Timescale versions; the new column is added to all chunks
///     atomically.
///
///   * **No `CREATE INDEX ON proxy_logs / revenue_events`**. `CREATE INDEX`
///     on a hypertable enumerates every chunk to build per-chunk indexes,
///     and that enumeration races with `drop_chunks` the same way. The
///     Observe page works without these indexes (queries are time-bounded
///     and chunk pruning handles them); they're a pure performance
///     optimization to be added later via a separate operational tool when
///     the cluster is quiet.
///
///   * **No job pausing / `LOCK TABLE`**. With nothing in this migration
///     enumerating chunks, there's no race surface left to defend.
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
    -- Idempotent column additions. `IF NOT EXISTS` covers the partial-
    -- prior-run case where v1/v2/v3 of this migration added some columns
    -- but failed before recording success in seaql_migrations.
    --
    -- Timescale handles `ADD COLUMN` on hypertables transparently: the
    -- new column appears on every chunk (current and future) atomically,
    -- with no per-chunk DDL needed from us.
    ALTER TABLE proxy_logs    ADD COLUMN IF NOT EXISTS trace_id         text;
    ALTER TABLE proxy_logs    ADD COLUMN IF NOT EXISTS error_group_id   integer;

    ALTER TABLE revenue_events ADD COLUMN IF NOT EXISTS deployment_id   integer;
    ALTER TABLE revenue_events ADD COLUMN IF NOT EXISTS environment_id  integer;
    ALTER TABLE revenue_events ADD COLUMN IF NOT EXISTS trace_id        text;

    ALTER TABLE error_events   ADD COLUMN IF NOT EXISTS trace_id_indexed text;

    -- Index only on the regular table. Hypertable indexes are skipped
    -- here to avoid racing with TimescaleDB's background retention
    -- worker (see header doc comment).
    CREATE INDEX IF NOT EXISTS idx_error_events_project_trace
        ON error_events (project_id, trace_id_indexed);
END
$$;
"#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Mirror image. `DROP INDEX IF EXISTS` for the hypertable indexes
        // is harmless even on installs that never had them — covers
        // operators who created them manually.
        db.execute_unprepared(
            r#"
DO $$
BEGIN
    DROP INDEX IF EXISTS idx_error_events_project_trace;
    ALTER TABLE error_events   DROP COLUMN IF EXISTS trace_id_indexed;

    DROP INDEX IF EXISTS idx_revenue_events_project_occurred;
    ALTER TABLE revenue_events DROP COLUMN IF EXISTS trace_id;
    ALTER TABLE revenue_events DROP COLUMN IF EXISTS environment_id;
    ALTER TABLE revenue_events DROP COLUMN IF EXISTS deployment_id;

    DROP INDEX IF EXISTS idx_proxy_logs_error_group;
    DROP INDEX IF EXISTS idx_proxy_logs_project_trace;
    ALTER TABLE proxy_logs     DROP COLUMN IF EXISTS error_group_id;
    ALTER TABLE proxy_logs     DROP COLUMN IF EXISTS trace_id;
END
$$;
"#,
        )
        .await?;

        Ok(())
    }
}
