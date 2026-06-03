use sea_orm_migration::prelude::*;

/// Creates `otel_trace_summaries`: a pre-aggregated, one-row-per-trace table
/// that backs the traces *list* view.
///
/// # Why this table exists
///
/// The list view previously ran `GROUP BY trace_id` over the `otel_spans`
/// hypertable on every request and sorted on a computed aggregate
/// (`MAX(duration_ms)`). That sort can't use an index, so at millions of
/// traces the database had to materialize and sort the entire grouped set
/// before applying LIMIT/OFFSET — O(n log n) in the number of distinct
/// traces in the window. This table moves the aggregation to write time:
/// the span-ingest path upserts one summary row per trace, and the list view
/// becomes a plain indexed `SELECT … ORDER BY duration_ms DESC LIMIT 20`.
///
/// # Why a plain table, not a continuous aggregate
///
/// A TimescaleDB continuous aggregate must bucket by a time dimension. Spans
/// dribble in over time with no "trace complete" signal, so a span arriving a
/// minute after the trace's root would land in a *different* bucket than the
/// rest of the trace — splitting one logical trace into several rows with
/// wrong `span_count` and `MAX(duration)`. An upsert keyed by
/// `(project_id, trace_id)` is always correct under late-arriving spans: a
/// late span just updates the existing row.
///
/// # Retention
///
/// This is NOT a hypertable, so the native `add_retention_policy` on
/// `otel_spans` does not cover it. Cleanup is handled in the otel retention
/// task (`apply_retention`, see `plugin.rs`); the table carries `start_time`
/// precisely so that task can `DELETE … WHERE start_time < now() - retention`.
///
/// **Safely re-runnable:** all DDL uses `IF NOT EXISTS`, and the backfill uses
/// `ON CONFLICT DO NOTHING` so it composes with any live upserts already
/// writing rows.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // The table itself is portable DDL; the hypertable join and chunked
        // backfill below are Postgres/TimescaleDB-specific.
        db.execute_unprepared(
            r#"
CREATE TABLE IF NOT EXISTS otel_trace_summaries (
    project_id              INTEGER          NOT NULL,
    trace_id                TEXT             NOT NULL,
    root_span_name          TEXT             NOT NULL DEFAULT '',
    service_name            TEXT             NOT NULL DEFAULT '',
    kind                    TEXT             NOT NULL DEFAULT 'Internal',
    deployment_environment  TEXT,
    deployment_id           INTEGER,
    -- earliest span start in the trace (the trace's start time)
    start_time              TIMESTAMPTZ      NOT NULL,
    -- longest span duration in the trace (the trace's duration)
    duration_ms             DOUBLE PRECISION NOT NULL DEFAULT 0,
    span_count              BIGINT           NOT NULL DEFAULT 0,
    error_count             BIGINT           NOT NULL DEFAULT 0,
    -- whether a root span (parent_span_id IS NULL) has been seen yet; once
    -- true, late non-root spans must not overwrite the root-derived fields
    has_root                BOOLEAN          NOT NULL DEFAULT FALSE,
    -- updated on every upsert; lets the retention/maintenance task and
    -- debugging see staleness independent of the trace's own start_time
    last_seen               TIMESTAMPTZ      NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, trace_id)
);

-- List view: default sort is start_time DESC, scoped by project + window.
CREATE INDEX IF NOT EXISTS idx_otel_trace_summaries_project_start
    ON otel_trace_summaries (project_id, start_time DESC);

-- Duration sort — the whole point of this table. Now an index scan.
CREATE INDEX IF NOT EXISTS idx_otel_trace_summaries_project_duration
    ON otel_trace_summaries (project_id, duration_ms DESC);

-- Service filter + time sort.
CREATE INDEX IF NOT EXISTS idx_otel_trace_summaries_project_service_start
    ON otel_trace_summaries (project_id, service_name, start_time DESC);

-- Error-only filter (status=ERROR) stays index-backed via a partial index.
CREATE INDEX IF NOT EXISTS idx_otel_trace_summaries_project_errors_start
    ON otel_trace_summaries (project_id, start_time DESC)
    WHERE error_count > 0;

-- Retention sweeps by start_time across all projects.
CREATE INDEX IF NOT EXISTS idx_otel_trace_summaries_start
    ON otel_trace_summaries (start_time);
"#,
        )
        .await?;

        // ── Chunked backfill from existing otel_spans ──────────────────────
        //
        // Only on Postgres (TimescaleDB). We aggregate per (project_id,
        // trace_id) one UTC day at a time, walking backwards over the span
        // time range. Day-sized chunks keep each INSERT bounded so a large
        // hypertable doesn't lock for minutes or blow up work_mem, and each
        // chunk aligns with the hypertable's 1-day `chunk_time_interval` so the
        // planner prunes to a single chunk per pass.
        //
        // The loop runs server-side in a single PL/pgSQL `DO` block — no
        // chrono dependency, no per-day round-trips. It walks from the most
        // recent day backwards so the freshest traces (most likely to be
        // viewed) get summaries first and the list view becomes useful before
        // the whole backfill finishes.
        //
        // `ON CONFLICT DO NOTHING`: if a live ingest upsert has already written
        // a (newer, authoritative) summary row for a trace, we leave it alone —
        // the backfill only fills gaps for traces ingested before this
        // migration ran. Re-running the migration is therefore safe.
        //
        // Root-span field selection mirrors the old query-time logic exactly
        // (`array_agg(... ORDER BY root-first, duration DESC)[1]`) so display
        // values are byte-identical to what the list view showed before.
        if manager.get_database_backend() == sea_orm::DatabaseBackend::Postgres {
            db.execute_unprepared(
                r#"
DO $$
DECLARE
    min_day TIMESTAMPTZ;
    max_day TIMESTAMPTZ;
    cur_day TIMESTAMPTZ;
BEGIN
    SELECT date_trunc('day', MIN(start_time)),
           date_trunc('day', MAX(start_time))
      INTO min_day, max_day
      FROM otel_spans;

    -- No spans yet: nothing to backfill.
    IF min_day IS NULL THEN
        RETURN;
    END IF;

    cur_day := max_day;
    WHILE cur_day >= min_day LOOP
        INSERT INTO otel_trace_summaries (
            project_id, trace_id, root_span_name, service_name, kind,
            deployment_environment, deployment_id, start_time, duration_ms,
            span_count, error_count, has_root, last_seen
        )
        SELECT
            s.project_id,
            s.trace_id,
            (array_agg(s.name ORDER BY
                CASE WHEN s.parent_span_id IS NULL THEN 0 ELSE 1 END,
                s.duration_ms DESC))[1],
            (array_agg(s.service_name ORDER BY
                CASE WHEN s.parent_span_id IS NULL THEN 0 ELSE 1 END,
                s.duration_ms DESC))[1],
            (array_agg(s.kind ORDER BY
                CASE WHEN s.parent_span_id IS NULL THEN 0 ELSE 1 END,
                s.duration_ms DESC))[1],
            (array_agg(s.deployment_environment ORDER BY
                CASE WHEN s.parent_span_id IS NULL THEN 0 ELSE 1 END,
                s.duration_ms DESC))[1],
            (array_agg(s.deployment_id ORDER BY
                CASE WHEN s.parent_span_id IS NULL THEN 0 ELSE 1 END,
                s.duration_ms DESC))[1],
            MIN(s.start_time),
            MAX(s.duration_ms),
            COUNT(*)::bigint,
            COUNT(*) FILTER (WHERE s.status_code = 'ERROR')::bigint,
            bool_or(s.parent_span_id IS NULL),
            now()
        FROM otel_spans s
        WHERE s.start_time >= cur_day
          AND s.start_time <  cur_day + INTERVAL '1 day'
        GROUP BY s.project_id, s.trace_id
        ON CONFLICT (project_id, trace_id) DO NOTHING;

        cur_day := cur_day - INTERVAL '1 day';
    END LOOP;
END $$;
"#,
            )
            .await
            .map_err(|e| DbErr::Custom(format!("otel_trace_summaries backfill failed: {e}")))?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS otel_trace_summaries CASCADE;")
            .await?;
        Ok(())
    }
}
