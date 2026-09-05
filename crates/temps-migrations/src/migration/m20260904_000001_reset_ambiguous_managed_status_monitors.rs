// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // The first shipped version of the managed-monitor migration inferred
        // ownership from an editable monitor name. There is no durable field
        // that can distinguish those user monitors from monitors created by
        // Temps during the affected window. Resetting ownership is the only
        // non-destructive correction: a later deployment creates a new,
        // explicitly managed monitor while every existing monitor is preserved.
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TABLE _temps_m20260904_managed_monitor_ownership_backup (\
                     monitor_id INTEGER PRIMARY KEY \
                         REFERENCES status_monitors(id) ON DELETE CASCADE\
                 ); \
                 INSERT INTO _temps_m20260904_managed_monitor_ownership_backup (monitor_id) \
                 SELECT id FROM status_monitors WHERE is_managed = TRUE; \
                 UPDATE status_monitors SET is_managed = FALSE WHERE is_managed = TRUE",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Restore exactly the ownership state captured by up(). If the
        // application created a replacement managed monitor in the meantime,
        // demote it first within only the affected environment so the partial
        // unique index remains valid.
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE status_monitors AS current \
                 SET is_managed = FALSE \
                 WHERE current.is_managed = TRUE \
                   AND EXISTS ( \
                       SELECT 1 \
                       FROM _temps_m20260904_managed_monitor_ownership_backup AS backup \
                       JOIN status_monitors AS original ON original.id = backup.monitor_id \
                       WHERE original.environment_id = current.environment_id \
                   ); \
                 UPDATE status_monitors AS monitor \
                 SET is_managed = TRUE \
                 FROM _temps_m20260904_managed_monitor_ownership_backup AS backup \
                 WHERE monitor.id = backup.monitor_id; \
                 DROP TABLE _temps_m20260904_managed_monitor_ownership_backup",
            )
            .await?;

        Ok(())
    }
}
