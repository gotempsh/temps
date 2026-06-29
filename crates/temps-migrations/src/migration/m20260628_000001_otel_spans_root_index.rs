//! Migration: partial index on root spans for the Observe feed.
//!
//! The unified Observe feed lists only ROOT spans (`parent_span_id IS NULL`) —
//! one row per trace (see `temps-observability::ObservabilityService::fetch_spans`).
//! The existing `idx_otel_spans_project_start (project_id, start_time DESC)`
//! already serves the query, but it has to skip every child span to satisfy the
//! `LIMIT`; with deep traces children are the bulk of the index, so the scan
//! reads many entries it then discards. This PARTIAL index covers only root
//! spans, so the feed reads (project-scoped, time-ordered) exactly the rows it
//! returns. Search (`name ILIKE`) intentionally spans all rows and is bounded by
//! the time window via chunk pruning, so it doesn't use this index.
//!
//! TimescaleDB-only (`otel_spans` is a hypertable); additive and idempotent.

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
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_otel_spans_root_project_start \
                 ON otel_spans (project_id, start_time DESC) \
                 WHERE parent_span_id IS NULL;",
            )
            .await
            .map_err(|e| {
                DbErr::Custom(format!(
                    "Failed to create root-span index on otel_spans: {e}"
                ))
            })?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() != DatabaseBackend::Postgres {
            return Ok(());
        }
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_otel_spans_root_project_start;")
            .await
            .map_err(|e| {
                DbErr::Custom(format!("Failed to drop root-span index on otel_spans: {e}"))
            })?;
        Ok(())
    }
}
