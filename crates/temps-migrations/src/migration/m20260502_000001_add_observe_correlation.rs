use sea_orm_migration::prelude::*;

/// Add cross-source correlation columns so the unified Observe page can join
/// requests / spans / errors / revenue without follow-up queries. All
/// columns are nullable; old rows simply render without correlation links.
///
/// **Designed to be safely re-runnable on any prior state**, including:
///   * Fresh installs (no rows yet, extension may not be installed)
///   * Partially-applied prior runs (any subset of columns/indexes present)
///   * Already-fully-applied installs (re-runs are no-ops)
///   * Installs without TimescaleDB (the proxy_logs compression dance is
///     skipped via an extension check)
///   * Installs where `proxy_logs` was never converted to a hypertable
///
/// Strategy: every step uses `IF NOT EXISTS` / `IF EXISTS` and is run via
/// raw SQL inside a single `DO $$ … $$` block so PostgreSQL handles the
/// procedural control flow.
///
/// **The "chunk not found" failure mode**: `proxy_logs` is a TimescaleDB
/// hypertable with two background jobs from
/// `m20260225_000001_add_proxy_logs_retention`:
///   * a 7-day compression policy
///   * a 30-day retention policy that DROPS old chunks
///
/// The original failure was a race between the migration enumerating chunks
/// (via `show_chunks()`) and the background retention worker dropping a
/// chunk that the enumeration just saw. By the time
/// `decompress_chunk(stale_oid)` ran, the chunk was gone → `chunk not
/// found`. The same race window exists between any two operations that
/// touch chunk metadata concurrently.
///
/// The fix:
///   1. **Pause every TimescaleDB job on the hypertable** via
///      `alter_job(scheduled => false)` — both the compression and
///      retention policies. Snapshot the job IDs first so we can restore
///      exactly the set that was active.
///   2. Take an exclusive lock on `proxy_logs` so no other session can
///      ALTER, INSERT, or query while we work. Locks block until the
///      transaction ends, so the `DO` block scopes the lock to this work.
///   3. Decompress every chunk, run the ALTERs, create the indexes —
///      now race-free because no background worker can mutate chunks.
///   4. Re-enable the jobs we paused.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
DO $$
DECLARE
    has_timescaledb boolean := EXISTS (
        SELECT 1 FROM pg_extension WHERE extname = 'timescaledb'
    );
    proxy_logs_is_hypertable boolean := false;
    paused_job_ids integer[] := ARRAY[]::integer[];
BEGIN
    -- ── proxy_logs (potentially compressed hypertable) ──────────────
    IF has_timescaledb THEN
        SELECT EXISTS (
            SELECT 1 FROM timescaledb_information.hypertables
            WHERE hypertable_name = 'proxy_logs'
        ) INTO proxy_logs_is_hypertable;

        IF proxy_logs_is_hypertable THEN
            -- 1. Pause every active background job on this hypertable
            --    (compression policy + retention policy). Snapshot the
            --    list so we restore exactly what was active.
            SELECT array_agg(job_id::integer) INTO paused_job_ids
            FROM timescaledb_information.jobs
            WHERE hypertable_name = 'proxy_logs' AND scheduled = true;

            IF paused_job_ids IS NOT NULL THEN
                PERFORM alter_job(j::integer, scheduled => false)
                FROM unnest(paused_job_ids) j;
            END IF;

            -- 2. Take an exclusive lock on the hypertable. Blocks any
            --    concurrent DDL/DML and waits for any in-flight job
            --    to finish. The lock is held until this DO block (and
            --    the surrounding migration transaction) ends.
            LOCK TABLE proxy_logs IN ACCESS EXCLUSIVE MODE;

            -- 3. Decompress every chunk so per-chunk ALTERs succeed.
            --    `if_compressed => TRUE` makes this a no-op for already-
            --    decompressed chunks (idempotent on partial-prior-runs).
            PERFORM decompress_chunk(c, if_compressed => TRUE)
            FROM show_chunks('proxy_logs') c;
        END IF;
    END IF;

    -- Idempotent column additions. `IF NOT EXISTS` covers the partial-
    -- prior-run case where v1 of this migration added `trace_id` but
    -- failed on `error_group_id` (or vice versa).
    ALTER TABLE proxy_logs    ADD COLUMN IF NOT EXISTS trace_id         text;
    ALTER TABLE proxy_logs    ADD COLUMN IF NOT EXISTS error_group_id   integer;

    ALTER TABLE revenue_events ADD COLUMN IF NOT EXISTS deployment_id   integer;
    ALTER TABLE revenue_events ADD COLUMN IF NOT EXISTS environment_id  integer;
    ALTER TABLE revenue_events ADD COLUMN IF NOT EXISTS trace_id        text;

    ALTER TABLE error_events   ADD COLUMN IF NOT EXISTS trace_id_indexed text;

    -- Indexes (also idempotent).
    CREATE INDEX IF NOT EXISTS idx_proxy_logs_project_trace
        ON proxy_logs (project_id, trace_id);
    CREATE INDEX IF NOT EXISTS idx_proxy_logs_error_group
        ON proxy_logs (error_group_id);
    CREATE INDEX IF NOT EXISTS idx_revenue_events_project_occurred
        ON revenue_events (project_id, occurred_at);
    CREATE INDEX IF NOT EXISTS idx_error_events_project_trace
        ON error_events (project_id, trace_id_indexed);

    -- 4. Restore the jobs we paused. Done unconditionally — if the
    --    array is empty (fresh install / no jobs), unnest produces zero
    --    rows and PERFORM is a no-op.
    IF has_timescaledb AND proxy_logs_is_hypertable
       AND paused_job_ids IS NOT NULL THEN
        PERFORM alter_job(j::integer, scheduled => true)
        FROM unnest(paused_job_ids) j;
    END IF;
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
DECLARE
    has_timescaledb boolean := EXISTS (
        SELECT 1 FROM pg_extension WHERE extname = 'timescaledb'
    );
    proxy_logs_is_hypertable boolean := false;
    paused_job_ids integer[] := ARRAY[]::integer[];
BEGIN
    IF has_timescaledb THEN
        SELECT EXISTS (
            SELECT 1 FROM timescaledb_information.hypertables
            WHERE hypertable_name = 'proxy_logs'
        ) INTO proxy_logs_is_hypertable;

        IF proxy_logs_is_hypertable THEN
            SELECT array_agg(job_id::integer) INTO paused_job_ids
            FROM timescaledb_information.jobs
            WHERE hypertable_name = 'proxy_logs' AND scheduled = true;

            IF paused_job_ids IS NOT NULL THEN
                PERFORM alter_job(j::integer, scheduled => false)
                FROM unnest(paused_job_ids) j;
            END IF;

            LOCK TABLE proxy_logs IN ACCESS EXCLUSIVE MODE;
            PERFORM decompress_chunk(c, if_compressed => TRUE)
            FROM show_chunks('proxy_logs') c;
        END IF;
    END IF;

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

    IF has_timescaledb AND proxy_logs_is_hypertable
       AND paused_job_ids IS NOT NULL THEN
        PERFORM alter_job(j::integer, scheduled => true)
        FROM unnest(paused_job_ids) j;
    END IF;
END
$$;
"#,
        )
        .await?;

        Ok(())
    }
}
