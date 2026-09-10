// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Migration: create `backup_schedule_services` join table.
//!
//! ## Purpose
//!
//! Lets a backup schedule target one or more external services explicitly.
//! Before this table, [`enqueue_scheduled_run`] fanned out to *every* external
//! service the host knew about — users had no way to say "this schedule backs
//! up these databases."
//!
//! ## Behaviour change on upgrade
//!
//! This migration intentionally **does not** backfill existing schedules.
//! Existing schedules will produce only the control-plane backup until users
//! attach services via `POST /api/backups/schedules/{id}/services` (or the UI).
//! This is the deliberate fix for the "schedules silently back up every DB"
//! bug — the next operator action must be explicit.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        manager
            .create_table(
                Table::create()
                    .table(BackupScheduleServices::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(BackupScheduleServices::ScheduleId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(BackupScheduleServices::ServiceId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(BackupScheduleServices::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .primary_key(
                        Index::create()
                            .name("backup_schedule_services_pkey")
                            .col(BackupScheduleServices::ScheduleId)
                            .col(BackupScheduleServices::ServiceId),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_backup_schedule_services_schedule_id")
                            .from(
                                BackupScheduleServices::Table,
                                BackupScheduleServices::ScheduleId,
                            )
                            .to(BackupSchedules::Table, BackupSchedules::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_backup_schedule_services_service_id")
                            .from(
                                BackupScheduleServices::Table,
                                BackupScheduleServices::ServiceId,
                            )
                            .to(ExternalServices::Table, ExternalServices::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Reverse-lookup index: "which schedules back up this service?"
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS backup_schedule_services_service_id_idx \
             ON backup_schedule_services (service_id)",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(BackupScheduleServices::Table)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum BackupScheduleServices {
    Table,
    ScheduleId,
    ServiceId,
    CreatedAt,
}

#[derive(DeriveIden)]
enum BackupSchedules {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum ExternalServices {
    Table,
    Id,
}
