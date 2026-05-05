//! Outbox table for fanning Postgres `events` rows out to ClickHouse.
//!
//! Phase 2 of the hybrid PG+ClickHouse analytics plan. Postgres remains the
//! system of record; ClickHouse is a derived columnar replica. The outbox
//! decouples the synchronous PG insert (returns to client immediately) from
//! the asynchronous CH ingestion (batched by a background worker).
//!
//! - `event_id` references the events row by value. We deliberately do NOT
//!   create a Postgres FK because `events` is a TimescaleDB hypertable
//!   partitioned by `(timestamp, id)`, and PG doesn't allow FKs to point
//!   at a table whose unique constraint includes more than just `id`.
//!   Orphaning is safe: the worker treats a missing event as "skip and
//!   mark delivered" — the row just won't appear in CH, which is the
//!   correct outcome when the source event has been retention-dropped.
//! - `delivered_at NULL FIRST` index lets the worker scan only the
//!   undelivered backlog cheaply.
//!
//! The worker itself ships behind the `clickhouse` feature; this table is
//! created unconditionally so existing PG installs that later enable CH have
//! the outbox already populated and ready to flush.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(EventsChOutbox::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(EventsChOutbox::EventId)
                            .big_integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(EventsChOutbox::EnqueuedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(EventsChOutbox::DeliveredAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(EventsChOutbox::Attempts)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(EventsChOutbox::LastError).text().null())
                    .to_owned(),
            )
            .await?;

        // Worker scan index: undelivered rows ordered by enqueue time.
        manager
            .create_index(
                Index::create()
                    .name("idx_events_ch_outbox_undelivered")
                    .table(EventsChOutbox::Table)
                    .col(EventsChOutbox::DeliveredAt)
                    .col(EventsChOutbox::EnqueuedAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(EventsChOutbox::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum EventsChOutbox {
    Table,
    EventId,
    EnqueuedAt,
    DeliveredAt,
    Attempts,
    LastError,
}
