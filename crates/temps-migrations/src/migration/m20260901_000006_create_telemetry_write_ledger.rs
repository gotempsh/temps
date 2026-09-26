// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-041 §1/§3d: the write-mode interval ledger and the gap-window record.
//!
//! Two small append-only tables.
//!
//! # `project_telemetry_write_intervals`
//!
//! One row per contiguous period during which a project's spans went to one
//! place. The read path (ADR-041 §8) resolves a query's time range against this
//! ledger instead of estimating a local retention floor: window entirely inside
//! a `cloud` interval serves from Cloud, entirely inside a `local` interval
//! serves locally, and a window that straddles is clamped to the newest
//! interval it touches with `window_clamped_at` reported — never merged.
//!
//! A **partial unique index** enforces that at most one interval per project is
//! open. That invariant is what makes "where do this project's spans go right
//! now" a single-row question; without it a missed close would produce two
//! plausible answers and the read path would have to guess.
//!
//! # `telemetry_gap_windows`
//!
//! Spans a Cloud-primary project produced that were genuinely not captured,
//! because the durable outbox hit its byte cap while Cloud was unreachable. A
//! bounded hole with a start, an end, a count and a reason. The alternative —
//! counting drops without recording when — leaves the Traces page rendering a
//! gap that is indistinguishable from "nothing happened".
//!
//! Both tables carry `project_id` without a foreign key, matching
//! `cloud_span_outbox` and `events_ch_outbox`: deleting a project must not be
//! blocked by, or cascade into, telemetry bookkeeping.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(WriteIntervals::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(WriteIntervals::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(WriteIntervals::ProjectId)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(WriteIntervals::Mode).text().not_null())
                    .col(
                        ColumnDef::new(WriteIntervals::EffectiveFrom)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(WriteIntervals::EffectiveTo)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(WriteIntervals::Reason)
                            .text()
                            .not_null()
                            .default("operator"),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE project_telemetry_write_intervals \
                 ADD CONSTRAINT project_telemetry_write_intervals_mode_valid \
                 CHECK (mode IN ('local', 'cloud'))",
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE project_telemetry_write_intervals \
                 ADD CONSTRAINT project_telemetry_write_intervals_reason_valid \
                 CHECK (reason IN ('operator', 'cloud_disconnected', 'quota_exhausted', \
                                   'credential_rejected', 'queue_overflow_spill', \
                                   'cloud_recovered'))",
            )
            .await?;

        // An interval that ends before it starts would make the read path's
        // range comparison answer nonsense rather than fail.
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE project_telemetry_write_intervals \
                 ADD CONSTRAINT project_telemetry_write_intervals_ordered \
                 CHECK (effective_to IS NULL OR effective_to >= effective_from)",
            )
            .await?;

        // At most one open interval per project. This is the invariant the
        // whole ledger design rests on.
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_write_intervals_one_open_per_project \
                 ON project_telemetry_write_intervals (project_id) \
                 WHERE effective_to IS NULL",
            )
            .await?;

        // Range resolution for a query window, newest interval first.
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_write_intervals_project_range \
                 ON project_telemetry_write_intervals (project_id, effective_from DESC)",
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(GapWindows::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(GapWindows::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(GapWindows::ProjectId).integer().not_null())
                    .col(
                        ColumnDef::new(GapWindows::StartedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(GapWindows::EndedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(GapWindows::DroppedSpans)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(GapWindows::DroppedBytes)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(GapWindows::Reason)
                            .text()
                            .not_null()
                            .default("queue_overflow_spill"),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE telemetry_gap_windows \
                 ADD CONSTRAINT telemetry_gap_windows_ordered \
                 CHECK (ended_at >= started_at)",
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_telemetry_gap_windows_project_range \
                 ON telemetry_gap_windows (project_id, started_at DESC)",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(GapWindows::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(WriteIntervals::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum WriteIntervals {
    #[sea_orm(iden = "project_telemetry_write_intervals")]
    Table,
    Id,
    ProjectId,
    Mode,
    EffectiveFrom,
    EffectiveTo,
    Reason,
}

#[derive(DeriveIden)]
enum GapWindows {
    #[sea_orm(iden = "telemetry_gap_windows")]
    Table,
    Id,
    ProjectId,
    StartedAt,
    EndedAt,
    DroppedSpans,
    DroppedBytes,
    Reason,
}
