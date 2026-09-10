// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Migration to create the `session_replay_ingest_batches` table.
//!
//! Deduplication marker for session-replay event ingest. The browser SDK holds
//! a failed batch and resends it verbatim under a stable `batch_id`; without a
//! record of which batches already landed, every retry appends the same rrweb
//! events again and the replay accumulates duplicates. That is not theoretical:
//! a slow ingest call that trips the proxy's upstream read timeout produces a
//! retry loop, and each pass through it used to write another copy.
//!
//! The marker lives in its own table rather than as a column on
//! `session_replay_events` deliberately. That table is the highest-volume one
//! in the schema; adding an id plus an index to every event row costs far more
//! than one marker row per ~100 events, and the marker is only ever read once,
//! at insert time.
//!
//! `batch_id` is client-generated and untrusted, so it is scoped by
//! `session_id` -- the unique constraint spans both. A client cannot suppress
//! another session's events by guessing an id, and a session is already
//! bound to its project before this table is consulted.
//!
//! Rows are removed with their session via `ON DELETE CASCADE`. They are only
//! useful for as long as a client might still retry a batch (seconds), so a
//! retention sweep can prune them aggressively; the cascade is what bounds
//! them in the absence of one.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(SessionReplayIngestBatches::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SessionReplayIngestBatches::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(SessionReplayIngestBatches::SessionId)
                            .integer()
                            .not_null(),
                    )
                    // Bounded, not a bare `varchar`: the value is
                    // client-supplied and indexed, and a btree tuple over
                    // ~2704 bytes would fail the insert outright. The service
                    // enforces the same 128 limit before it gets here.
                    .col(
                        ColumnDef::new(SessionReplayIngestBatches::BatchId)
                            .string_len(128)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SessionReplayIngestBatches::EventCount)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(SessionReplayIngestBatches::ReceivedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_session_replay_ingest_batches_session_id")
                            .from(
                                SessionReplayIngestBatches::Table,
                                SessionReplayIngestBatches::SessionId,
                            )
                            .to(SessionReplaySessions::Table, SessionReplaySessions::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // The dedup check is an insert against this constraint: a conflict
        // means the batch already landed. It must be unique, not merely
        // indexed, or concurrent retries of the same batch both succeed.
        manager
            .create_index(
                Index::create()
                    .name("idx_session_replay_ingest_batches_unique")
                    .table(SessionReplayIngestBatches::Table)
                    .col(SessionReplayIngestBatches::SessionId)
                    .col(SessionReplayIngestBatches::BatchId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Supports pruning by age without scanning the table.
        manager
            .create_index(
                Index::create()
                    .name("idx_session_replay_ingest_batches_received_at")
                    .table(SessionReplayIngestBatches::Table)
                    .col(SessionReplayIngestBatches::ReceivedAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(SessionReplayIngestBatches::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum SessionReplayIngestBatches {
    Table,
    Id,
    SessionId,
    BatchId,
    EventCount,
    ReceivedAt,
}

#[derive(DeriveIden)]
enum SessionReplaySessions {
    Table,
    Id,
}
