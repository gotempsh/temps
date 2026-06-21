//! Migration to create the `telemetry_milestones` table.
//!
//! Backs the once-per-instance guard for "first-touch" anonymous telemetry
//! events (e.g. `analytics_first_event_received`, `ai_gateway_first_request`).
//! Those events are meant to fire EXACTLY ONCE in an instance's lifetime, but
//! were previously emitted on every pageview / AI request / deploy — turning a
//! "first touch" signal into a per-action firehose and making telemetry volume
//! scale with the self-hoster's production traffic.
//!
//! The reporter claims a milestone by inserting its name here (`INSERT ... ON
//! CONFLICT DO NOTHING`); only the first claimant emits the event. The row holds
//! nothing identifying — just the milestone name and when it was first claimed —
//! and is never sent anywhere. An in-process cache fronts this table so the hot
//! path (pageview/AI/deploy ingestion) never touches the DB after the first
//! claim per process.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(TelemetryMilestones::Table)
                    .if_not_exists()
                    // The milestone name (the telemetry event's wire name, e.g.
                    // "analytics_first_event_received"). PK gives us the
                    // once-ever guarantee via ON CONFLICT DO NOTHING.
                    .col(
                        ColumnDef::new(TelemetryMilestones::Milestone)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    // When this instance first reached the milestone. Local-only,
                    // never transmitted.
                    .col(
                        ColumnDef::new(TelemetryMilestones::FirstEmittedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(TelemetryMilestones::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum TelemetryMilestones {
    Table,
    Milestone,
    FirstEmittedAt,
}
