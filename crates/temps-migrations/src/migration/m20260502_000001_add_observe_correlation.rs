use sea_orm_migration::prelude::*;

/// Add cross-source correlation columns + lookup indexes so the unified
/// Observe page can join requests / spans / errors / revenue without
/// follow-up queries. All columns are nullable; old rows simply render
/// without correlation links.
///
/// **Designed to be safely re-runnable on any prior state**, including:
///   * Fresh installs (no rows yet, extension may not be installed)
///   * Partially-applied prior runs (any subset of columns/indexes present)
///   * Already-fully-applied installs (re-runs are no-ops)
///   * Installs without TimescaleDB
///
/// Strategy: every step uses `IF NOT EXISTS` and is run via raw SQL.
/// `ALTER TABLE ... ADD COLUMN IF NOT EXISTS` is propagated to every
/// hypertable chunk atomically by Timescale; `CREATE INDEX IF NOT EXISTS`
/// is per-chunk and safe on a healthy hypertable (see operational note
/// below for the one prod scenario where it isn't).
///
/// **Operational note for installs migrated via raw `pg_dump`/`pg_restore`**:
/// if the source database was migrated to a new server *without* wrapping
/// the restore in `timescaledb_pre_restore()` / `timescaledb_post_restore()`,
/// the new database can have orphan chunks — relations attached via
/// `pg_inherits` but missing from `_timescaledb_catalog.chunk`. `CREATE
/// INDEX` on the parent hypertable will then fail with `chunk not found`
/// because Postgres tries to build the per-chunk index on a chunk Timescale
/// no longer knows about. The fix is operational, not migration-level:
/// detach + drop the orphan child relations, then re-run. See
/// `docs/runbooks/timescaledb-orphan-chunks.md` (or git history of this
/// migration) for the cleanup script. We do not attempt that cleanup from
/// inside the migration because it would silently delete user data.
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

    -- Lookup indexes for the Observe page's cross-source correlation.
    -- `IF NOT EXISTS` makes this safe on installs where the index was
    -- already created manually (e.g. prod, where these were built
    -- out-of-band after the orphan-chunk cleanup — see header comment).
    CREATE INDEX IF NOT EXISTS idx_error_events_project_trace
        ON error_events (project_id, trace_id_indexed);

    CREATE INDEX IF NOT EXISTS idx_proxy_logs_project_trace
        ON proxy_logs (project_id, trace_id);

    CREATE INDEX IF NOT EXISTS idx_proxy_logs_error_group
        ON proxy_logs (error_group_id)
        WHERE error_group_id IS NOT NULL;

    CREATE INDEX IF NOT EXISTS idx_revenue_events_project_occurred
        ON revenue_events (project_id, occurred_at DESC);
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
