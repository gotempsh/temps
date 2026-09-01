// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Drop the `backups.last_heartbeat_at` column.
//!
//! The column was added by `m20260504_000001_widen_backup_size_and_heartbeat`
//! to power a UI "stalled — worker not responding" badge. The badge was a
//! false-positive engine: the temps process during a backup is parked on
//! Docker/S3 I/O and has no meaningful liveness to report, so a process-
//! side heartbeat was solving a problem that doesn't exist. The badge,
//! its threshold, the heartbeat task, and the mid-run stall sweep are all
//! gone; this column has no remaining writers or readers.
//!
//! Boot-time orphan reconciliation (the only legitimate stall signal)
//! does not depend on this column — any `state='running'` row at startup
//! is by definition orphaned and gets flipped to `failed` regardless.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Backups::Table)
                    .drop_column(Backups::LastHeartbeatAt)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Backups::Table)
                    .add_column(
                        ColumnDef::new(Backups::LastHeartbeatAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Backups {
    Table,
    LastHeartbeatAt,
}
