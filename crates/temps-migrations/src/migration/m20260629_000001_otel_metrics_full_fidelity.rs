//! Migration: full-fidelity OTel metrics columns + efficient query indexes.
//!
//! ## Why this migration exists
//!
//! The initial `otel_metrics` schema (m20260225_000001) stored only the scalar
//! `value` and the explicit-histogram buckets.  Fourteen fields required for
//! full-fidelity OTel storage — temporality, monotonicity, start_time, flags,
//! description, exponential-histogram layout, summary quantiles, and exemplars
//! — were dropped on ingest.  This migration adds them all as nullable columns
//! (safe on a compressed hypertable: ADD COLUMN IF NOT EXISTS with a NULL
//! default does NOT rewrite chunks) and creates two indexes that make the new
//! query path efficient:
//!
//! - `idx_otel_metrics_proj_metric_svc_time` — the primary hot-path index for
//!   metric queries filtered by (project, metric, service) ordered by time.
//! - `idx_otel_metrics_attrs_gin` — GIN index on `attributes jsonb_path_ops`
//!   so label-filter JSONB-containment (`@>`) uses a GIN lookup instead of a
//!   sequential scan.
//!
//! ## Lock note
//!
//! `ALTER TABLE … ADD COLUMN IF NOT EXISTS` on a hypertable acquires an
//! `ACCESS EXCLUSIVE` lock on the *parent* table for the duration of the
//! statement.  On a busy production cluster this can block reads/writes for
//! a few seconds.  Chunks remain accessible during the lock (TimescaleDB
//! proxies reads through the parent, so this matters).  For installations
//! with very large `otel_metrics` hypertables, consider running `temps
//! migrate` during a maintenance window or off-peak hours via the decoupled
//! migration path rather than inline at server startup.
//!
//! The two `CREATE INDEX IF NOT EXISTS` statements use the non-concurrent
//! form so they are transactionally correct inside the migration runner.
//! For truly zero-downtime index creation on a live cluster, run them
//! manually with `CREATE INDEX CONCURRENTLY` *after* this migration has
//! been recorded in `seaql_migrations`.

use sea_orm::DatabaseBackend;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() != DatabaseBackend::Postgres {
            return Ok(());
        }

        let conn = manager.get_connection();

        // ── ADD the 14 full-fidelity columns (all nullable, safe on compressed
        //    chunks). We use a single ALTER TABLE with multiple clauses so the
        //    parent-table lock is acquired only once.
        conn.execute_unprepared(
            "ALTER TABLE otel_metrics
               ADD COLUMN IF NOT EXISTS start_time          TIMESTAMPTZ,
               ADD COLUMN IF NOT EXISTS temporality         TEXT,
               ADD COLUMN IF NOT EXISTS is_monotonic        BOOLEAN,
               ADD COLUMN IF NOT EXISTS flags               INTEGER,
               ADD COLUMN IF NOT EXISTS description         TEXT,
               ADD COLUMN IF NOT EXISTS exp_scale           INTEGER,
               ADD COLUMN IF NOT EXISTS exp_zero_count      BIGINT,
               ADD COLUMN IF NOT EXISTS exp_zero_threshold  DOUBLE PRECISION,
               ADD COLUMN IF NOT EXISTS exp_positive_offset INTEGER,
               ADD COLUMN IF NOT EXISTS exp_positive_counts JSONB,
               ADD COLUMN IF NOT EXISTS exp_negative_offset INTEGER,
               ADD COLUMN IF NOT EXISTS exp_negative_counts JSONB,
               ADD COLUMN IF NOT EXISTS summary_quantiles   JSONB,
               ADD COLUMN IF NOT EXISTS exemplars           JSONB;",
        )
        .await
        .map_err(|e| {
            DbErr::Custom(format!(
                "m20260629_000001: failed to add full-fidelity columns to otel_metrics: {e}"
            ))
        })?;

        // ── Composite btree index for the main metric query hot path
        //    (project_id, metric_name, service_name, timestamp DESC).
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_otel_metrics_proj_metric_svc_time \
             ON otel_metrics (project_id, metric_name, service_name, timestamp DESC);",
        )
        .await
        .map_err(|e| {
            DbErr::Custom(format!(
                "m20260629_000001: failed to create idx_otel_metrics_proj_metric_svc_time: {e}"
            ))
        })?;

        // ── GIN index on attributes for JSONB-containment (@>) label filters.
        //    jsonb_path_ops operator class is smaller and faster than the default
        //    for containment queries — it is the right choice here.
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_otel_metrics_attrs_gin \
             ON otel_metrics USING GIN (attributes jsonb_path_ops);",
        )
        .await
        .map_err(|e| {
            DbErr::Custom(format!(
                "m20260629_000001: failed to create idx_otel_metrics_attrs_gin: {e}"
            ))
        })?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() != DatabaseBackend::Postgres {
            return Ok(());
        }

        let conn = manager.get_connection();

        // Drop indexes first (they reference the columns we are about to drop).
        conn.execute_unprepared(
            "DROP INDEX IF EXISTS idx_otel_metrics_proj_metric_svc_time; \
             DROP INDEX IF EXISTS idx_otel_metrics_attrs_gin;",
        )
        .await
        .map_err(|e| {
            DbErr::Custom(format!(
                "m20260629_000001: failed to drop indexes on otel_metrics: {e}"
            ))
        })?;

        // Drop the 14 added columns.
        conn.execute_unprepared(
            "ALTER TABLE otel_metrics
               DROP COLUMN IF EXISTS exemplars,
               DROP COLUMN IF EXISTS summary_quantiles,
               DROP COLUMN IF EXISTS exp_negative_counts,
               DROP COLUMN IF EXISTS exp_negative_offset,
               DROP COLUMN IF EXISTS exp_positive_counts,
               DROP COLUMN IF EXISTS exp_positive_offset,
               DROP COLUMN IF EXISTS exp_zero_threshold,
               DROP COLUMN IF EXISTS exp_zero_count,
               DROP COLUMN IF EXISTS exp_scale,
               DROP COLUMN IF EXISTS description,
               DROP COLUMN IF EXISTS flags,
               DROP COLUMN IF EXISTS is_monotonic,
               DROP COLUMN IF EXISTS temporality,
               DROP COLUMN IF EXISTS start_time;",
        )
        .await
        .map_err(|e| {
            DbErr::Custom(format!(
                "m20260629_000001: failed to drop full-fidelity columns from otel_metrics: {e}"
            ))
        })?;

        Ok(())
    }
}
