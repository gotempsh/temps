// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-043 §2c.2: two additive columns the shipped `cloud_telemetry_outbox`
//! schema was missing.
//!
//! The shipped `payload` column is `TEXT`, which is correct for spans (whose
//! payload is JSON `SpanRecord`) but wrong for every other entity in scope,
//! whose payload is a binary ClickHouse row (`#[derive(clickhouse::Row)]`).
//! Base64/hex-encoding a binary row into `TEXT` would inflate the
//! highest-volume queue by 33–100% and charge that inflation against the
//! operator-visible byte cap.
//!
//! This migration adds:
//!
//! - `target_table TEXT NULL` — the Cloud ClickHouse table a row is destined
//!   for (e.g. `otel_metrics`, `events`, `proxy_logs`). `NULL` for existing
//!   `span` rows, whose destination (`telemetry_spans`) is implied by
//!   `entity_type = 'span'` rather than stored.
//! - `payload_row BYTEA NULL` — the binary row payload for every entity type
//!   added after spans. `NULL` for `span` rows, which keep using the
//!   existing `payload TEXT` column unchanged.
//!
//! Both are nullable, defaulted to `NULL`, and reversible with a plain `DROP
//! COLUMN`. Nothing shipped is dropped, renamed or rewritten. `payload_bytes`
//! keeps its existing meaning: the byte length of whichever payload column a
//! row actually uses.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE cloud_telemetry_outbox ADD COLUMN target_table TEXT NULL",
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE cloud_telemetry_outbox ADD COLUMN payload_row BYTEA NULL",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE cloud_telemetry_outbox DROP COLUMN IF EXISTS payload_row",
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE cloud_telemetry_outbox DROP COLUMN IF EXISTS target_table",
            )
            .await?;

        Ok(())
    }
}
