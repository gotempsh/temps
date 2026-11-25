// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Store the sandbox a platform-observed log record belongs to in its own
//! column, so a sandbox's records (its recorded outbound connections) are
//! read by an index instead of by filtering its project's logs on a JSON
//! attribute.
//!
//! The records stay OpenTelemetry log records in `otel_log_events`, with
//! the same columns, attributes and trace correlation. Only the platform
//! fills `sandbox_id`; workload logs leave it `NULL`.
//!
//! Adding a nullable column without a default changes only the catalog, so
//! existing chunks, compressed ones included, are not rewritten. The index
//! is partial: rows without a sandbox, which are almost all of them, add
//! nothing to it.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE otel_log_events ADD COLUMN IF NOT EXISTS sandbox_id INTEGER; \
                 CREATE INDEX IF NOT EXISTS idx_otel_logs_sandbox_timestamp \
                     ON otel_log_events (sandbox_id, timestamp DESC) \
                     WHERE sandbox_id IS NOT NULL;",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DROP INDEX IF EXISTS idx_otel_logs_sandbox_timestamp; \
                 ALTER TABLE otel_log_events DROP COLUMN IF EXISTS sandbox_id;",
            )
            .await?;
        Ok(())
    }
}
