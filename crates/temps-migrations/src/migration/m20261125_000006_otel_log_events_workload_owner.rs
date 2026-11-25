// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Let a log record be owned by a workload, with or without a project.
//!
//! Records the platform observes about a workload (its recorded outbound
//! connections, for a start) are stored with the workload's kind and id in
//! their own columns, the same identity network rules use
//! (`temps_workload::WorkloadRef`: `sandbox/42`, `deployment/7` for
//! environment 7). So one workload's records are read by an index instead
//! of by filtering its project's logs on a JSON attribute, and every
//! workload kind is stored the same way.
//!
//! `project_id` becomes nullable: a sandbox that belongs to no project
//! still has records, owned by the workload alone. Everything a workload
//! sends about itself keeps its project.
//!
//! The records stay OpenTelemetry log records in `otel_log_events`, with
//! the same columns, attributes and trace correlation. Only the platform
//! fills the workload columns; workload logs leave them `NULL`.
//!
//! Adding nullable columns without a default and dropping a `NOT NULL`
//! change only the catalog, so existing chunks, compressed ones included,
//! are not rewritten and ingestion is not held up.
//!
//! The index a workload's records are read by
//! (`idx_otel_logs_workload_timestamp`, partial: rows without a workload,
//! which are almost all of them, add nothing to it) is not built here. On
//! an install with months of logs a `CREATE INDEX` inside the migration
//! would block log ingestion for the whole build, so it is built after boot,
//! one chunk per transaction, by `temps_schema`'s post-migration step. Until
//! then a workload's records are still read correctly, by a scan.
//!
//! Down deletes the records that have no project, since the column cannot
//! hold them once it is `NOT NULL` again.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE otel_log_events \
                     ADD COLUMN IF NOT EXISTS workload_kind TEXT, \
                     ADD COLUMN IF NOT EXISTS workload_id BIGINT; \
                 ALTER TABLE otel_log_events ALTER COLUMN project_id DROP NOT NULL;",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DROP INDEX IF EXISTS idx_otel_logs_workload_timestamp; \
                 DELETE FROM otel_log_events WHERE project_id IS NULL; \
                 ALTER TABLE otel_log_events ALTER COLUMN project_id SET NOT NULL; \
                 ALTER TABLE otel_log_events \
                     DROP COLUMN IF EXISTS workload_kind, \
                     DROP COLUMN IF EXISTS workload_id;",
            )
            .await?;
        Ok(())
    }
}
