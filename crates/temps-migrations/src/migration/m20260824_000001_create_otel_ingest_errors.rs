// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Migration: durable record of *why* OTel ingest batches were dropped.
//!
//! The pipeline-stats counters answer "how many spans were dropped?"; this
//! table answers "dropped because of what?" without an operator having to grep
//! server logs. A row is written only when a storage write has already
//! exhausted its bounded retry, so the write rate is bounded by the failure
//! rate, not by ingest volume.
//!
//! Deliberately an *aggregate* table, not an append-only event log: the unique
//! constraint on `(signal_type, error_class)` lets the writer upsert
//! (`ON CONFLICT ... DO UPDATE SET count = count + 1`), so the table's size is
//! bounded by (number of signals x number of error classes) — a few dozen rows
//! at absolute worst, forever. That is why it needs no partitioning, no TTL and
//! no prune job: readers filter on `last_seen` for a rolling window instead.
//!
//! Lives in Postgres even when the ClickHouse OTel backend is enabled. The
//! failure this table records is usually *ClickHouse being unreachable*, so
//! storing it in ClickHouse would lose the record in exactly the case it exists
//! to explain.
//!
//! All DDL is idempotent (IF NOT EXISTS).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
CREATE TABLE IF NOT EXISTS otel_ingest_errors (
    id             BIGSERIAL   PRIMARY KEY,
    signal_type    TEXT        NOT NULL,
    error_class    TEXT        NOT NULL,
    sample_message TEXT        NOT NULL,
    count          BIGINT      NOT NULL DEFAULT 1,
    first_seen     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT otel_ingest_errors_signal_class_key UNIQUE (signal_type, error_class)
);

CREATE INDEX IF NOT EXISTS otel_ingest_errors_by_last_seen
    ON otel_ingest_errors (last_seen DESC);
"#,
            )
            .await
            .map_err(|e| {
                DbErr::Custom(format!(
                    "Failed to create otel_ingest_errors table/indexes: {e}"
                ))
            })?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS otel_ingest_errors CASCADE;")
            .await
            .map_err(|e| DbErr::Custom(format!("Failed to drop otel_ingest_errors table: {e}")))?;

        Ok(())
    }
}
