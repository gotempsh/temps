// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-041 §3a: the durable outbox for Cloud-primary span writes.
//!
//! Modelled directly on `m20260505_000001_create_events_ch_outbox`, which
//! already carries this shape in production for the analytics ClickHouse
//! fan-out. Same claim/deliver/attempts/dead-letter model, same "no foreign
//! key" reasoning, same partial index on the undelivered backlog.
//!
//! Differences from `events_ch_outbox`, all forced by this being a *primary*
//! write path rather than a derived replica:
//!
//! - The payload lives in the row. `events_ch_outbox` references an `events`
//!   row by id because Postgres is the system of record there and the outbox is
//!   only a delivery cursor. Here Postgres is the *only* copy until Cloud
//!   acknowledges, so the row has to carry the span.
//! - `payload_bytes` is stored. The cap is expressed in bytes (ADR-041 §3d) so
//!   a full queue is a bounded fraction of a 4 GB box's disk; enforcing that
//!   must not require re-reading or re-serializing the backlog.
//! - An explicit `state` column instead of `delivered_at IS NULL`, so
//!   dead-lettered and spilled rows are excluded from the worker's claim scan
//!   by the index rather than by predicate arithmetic over `attempts`.
//!
//! # Empty on an instance with no Cloud link
//!
//! Nothing writes here unless a project is explicitly set to Cloud-primary,
//! which requires a healthy link, `queryable` fidelity and the telemetry switch
//! on. Deployment shape A (no Cloud link — the default, and the large majority
//! of installs) gets one empty table and no behaviour change at all.

use sea_orm_migration::prelude::*;

/// Kept in sync with `temps_entities::cloud_span_outbox::CloudSpanOutboxState`.
const STATE_CONSTRAINT: &str = "cloud_span_outbox_state_valid";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(CloudSpanOutbox::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(CloudSpanOutbox::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(CloudSpanOutbox::ProjectId)
                            .integer()
                            .not_null(),
                    )
                    // Nullable so a dead letter can outlive its span content.
                    // A row that will never ship is kept as evidence (project,
                    // attempts, `last_error`, timestamps) but its payload is
                    // real telemetry at `Queryable` fidelity, and the retention
                    // sweep nulls it once it is past
                    // `DEAD_LETTER_PAYLOAD_RETENTION`. Every row the worker can
                    // still act on — anything `pending` — always has one.
                    .col(ColumnDef::new(CloudSpanOutbox::Payload).text().null())
                    .col(
                        ColumnDef::new(CloudSpanOutbox::PayloadBytes)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(CloudSpanOutbox::EnqueuedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(CloudSpanOutbox::Attempts)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(CloudSpanOutbox::State)
                            .text()
                            .not_null()
                            .default("pending"),
                    )
                    .col(
                        ColumnDef::new(CloudSpanOutbox::SettledAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(ColumnDef::new(CloudSpanOutbox::LastError).text().null())
                    .to_owned(),
            )
            .await?;

        // The worker's claim scan: pending rows, oldest first. Partial so the
        // index only ever holds the live backlog — a dead-lettered row an
        // operator has not yet dealt with must not slow down delivery of
        // everything behind it.
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_cloud_span_outbox_pending \
                 ON cloud_span_outbox (enqueued_at, id) \
                 WHERE state = 'pending'",
            )
            .await?;

        // Per-project queue depth, oldest-unshipped age, and the disconnect
        // path's "drain everything for these projects" sweep.
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_cloud_span_outbox_project_state \
                 ON cloud_span_outbox (project_id, state, enqueued_at)",
            )
            .await?;

        // The retention sweep deletes settled rows by age; without this it is a
        // sequential scan of the whole table on every sweep.
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_cloud_span_outbox_settled \
                 ON cloud_span_outbox (settled_at) \
                 WHERE settled_at IS NOT NULL",
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(&format!(
                "ALTER TABLE cloud_span_outbox ADD CONSTRAINT {STATE_CONSTRAINT} \
                 CHECK (state IN ('pending', 'delivered', 'dead_letter', 'spilled_to_local'))"
            ))
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(CloudSpanOutbox::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum CloudSpanOutbox {
    Table,
    Id,
    ProjectId,
    Payload,
    PayloadBytes,
    EnqueuedAt,
    Attempts,
    State,
    SettledAt,
    LastError,
}
