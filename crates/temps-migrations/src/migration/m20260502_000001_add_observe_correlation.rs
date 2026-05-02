use sea_orm_migration::prelude::*;

/// Add cross-source correlation columns so the unified Observe page can join
/// requests / spans / errors / revenue without follow-up queries. All
/// columns are nullable; old rows simply render without correlation links.
///
/// **Designed to be safely re-runnable on any prior state**, including:
///   * Fresh installs (no rows exist yet, no extension may be installed)
///   * Partially-applied prior runs (some columns present, others not — the
///     v1 of this migration would fail on the second `ALTER` against a
///     compressed `proxy_logs` chunk and leave the row in the historical
///     `seaql_migrations` table while the schema was half-done)
///   * Already-fully-applied installs (re-running is a no-op)
///   * Installs without the TimescaleDB extension (the proxy_logs
///     compression dance is skipped)
///   * Installs where `proxy_logs` was not converted to a hypertable
///     (`show_chunks` would error — guarded by an extension check)
///
/// Strategy: every step uses `IF NOT EXISTS` / `IF EXISTS` and is run via
/// raw SQL inside a single `DO $$ … $$` block so PostgreSQL handles the
/// procedural control flow. Sea-ORM's `alter_table` builder doesn't emit
/// `IF NOT EXISTS` for `ADD COLUMN`, which is why we drop down to raw SQL
/// here. The `ALTER TABLE … ADD COLUMN IF NOT EXISTS …` form has been
/// supported since Postgres 9.6 and works on TimescaleDB hypertables.
///
/// **TimescaleDB caveat (the original failure mode)**: `proxy_logs` is a
/// compressed hypertable (7-day policy from
/// `m20260225_000001_add_proxy_logs_retention`). Once chunks compress,
/// `ALTER TABLE … ADD COLUMN` against the parent fails with
/// `chunk not found` because per-chunk schemas can't be rewritten in
/// place. We pause the policy, decompress all chunks, then re-add the
/// policy with `if_not_exists`.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Single PL/pgSQL block so the whole thing is one statement and
        // we get atomic control flow (the extension check guards every
        // TimescaleDB call). Each ALTER uses `IF NOT EXISTS`, so a partial
        // prior run completes cleanly without us tracking which columns
        // already exist.
        db.execute_unprepared(
            r#"
DO $$
DECLARE
    has_timescaledb boolean := EXISTS (
        SELECT 1 FROM pg_extension WHERE extname = 'timescaledb'
    );
    proxy_logs_is_hypertable boolean := false;
BEGIN
    -- ── proxy_logs (potentially compressed hypertable) ──────────────
    IF has_timescaledb THEN
        SELECT EXISTS (
            SELECT 1 FROM timescaledb_information.hypertables
            WHERE hypertable_name = 'proxy_logs'
        ) INTO proxy_logs_is_hypertable;

        IF proxy_logs_is_hypertable THEN
            -- Pause the compression policy. `if_exists` returns NULL when
            -- the policy isn't present (fresh installs that haven't run
            -- the retention migration yet, or installs that have already
            -- removed it via a partial prior run).
            PERFORM remove_compression_policy('proxy_logs', if_exists => TRUE);

            -- Decompress every chunk so per-chunk ALTERs succeed.
            -- `if_compressed => TRUE` makes this a no-op for already-
            -- decompressed chunks.
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

    -- Restore the proxy_logs compression policy with the original 7-day
    -- window. `if_not_exists` makes this safe to re-run on installs that
    -- never had the policy (no-op).
    IF has_timescaledb AND proxy_logs_is_hypertable THEN
        PERFORM add_compression_policy(
            'proxy_logs', INTERVAL '7 days', if_not_exists => TRUE
        );
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

        // Mirror image: pause compression, drop indexes/columns, restore.
        db.execute_unprepared(
            r#"
DO $$
DECLARE
    has_timescaledb boolean := EXISTS (
        SELECT 1 FROM pg_extension WHERE extname = 'timescaledb'
    );
    proxy_logs_is_hypertable boolean := false;
BEGIN
    IF has_timescaledb THEN
        SELECT EXISTS (
            SELECT 1 FROM timescaledb_information.hypertables
            WHERE hypertable_name = 'proxy_logs'
        ) INTO proxy_logs_is_hypertable;

        IF proxy_logs_is_hypertable THEN
            PERFORM remove_compression_policy('proxy_logs', if_exists => TRUE);
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

    IF has_timescaledb AND proxy_logs_is_hypertable THEN
        PERFORM add_compression_policy(
            'proxy_logs', INTERVAL '7 days', if_not_exists => TRUE
        );
    END IF;
END
$$;
"#,
        )
        .await?;

        Ok(())
    }
}
