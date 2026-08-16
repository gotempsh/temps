//! Fix the `otel_spans` compression settings so compression is actually
//! effective.
//!
//! The original DDL (`m20260225_000001_create_otel_tables`) declared
//! `compress_segmentby = 'project_id,service_name,trace_id'`. `trace_id` is
//! near-unique (one value per trace, a handful of spans each), so every
//! compressed segment holds only a few rows instead of the up-to-1000-row
//! batches TimescaleDB compression is built around. Measured on a
//! representative 1M-span workload, the old settings compress ~707 B/span to
//! only ~543 B/span (1.3x); dropping `trace_id` from segmentby yields
//! ~63 B/span (11x).
//!
//! This migration is metadata-only and completes in milliseconds. However:
//!
//! * The new settings apply to chunks compressed **after** this migration.
//!   Chunks that were already compressed keep the old (ineffective) layout
//!   until they age out via the retention policy, or until an operator
//!   manually runs `SELECT compress_chunk(c, recompress => true) FROM
//!   show_chunks('otel_spans') c;`.
//! * The **next run of the compression policy** may take a long time (hours
//!   on large tables) and consume significant CPU and transient disk while it
//!   works through the backlog of uncompressed chunks with the new settings.
//!   This is background work; the server stays up while it runs.
//!
//! Trace-by-id lookups on compressed chunks can no longer seek directly to a
//! `trace_id` segment after this change; the paired `get_trace` change bounds
//! those scans by the trace's time window from `otel_trace_summaries` so
//! chunk exclusion keeps them cheap.

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

        tracing::warn!(
            "Applying otel_spans compression fix: the ALTER itself is instant, but the next \
             compression-policy run will recompress the uncompressed backlog with the new \
             settings and may take a long time (hours on large tables) while using significant \
             CPU. The server stays up; this is background work. Chunks compressed under the old \
             settings keep the old layout until retention drops them or an operator recompresses \
             them manually."
        );

        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE otel_spans SET (
                    timescaledb.compress,
                    timescaledb.compress_segmentby = 'project_id,service_name',
                    timescaledb.compress_orderby = 'start_time DESC'
                )",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() != DatabaseBackend::Postgres {
            return Ok(());
        }

        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE otel_spans SET (
                    timescaledb.compress,
                    timescaledb.compress_segmentby = 'project_id,service_name,trace_id',
                    timescaledb.compress_orderby = 'start_time DESC'
                )",
            )
            .await?;

        Ok(())
    }
}
