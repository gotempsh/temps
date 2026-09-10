// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // This migration originally reset every `is_managed = TRUE` row
        // unconditionally, on the theory that the only way a row could be
        // TRUE at this point was the name-guessing bug in the first shipped
        // version of m20260831_000002 (see that file's history). That
        // premise doesn't hold in practice: m20260831_000002's guessing
        // logic was corrected before it ever shipped in a standalone
        // release, while this migration shipped one release later. Any
        // install that upgraded through that intervening release had
        // already run the corrected m20260831_000002 (which never guesses
        // ownership) and, in the normal course of reconciliation, already
        // had `ensure_monitor_for_environment` create a legitimate managed
        // monitor. This migration's blanket UPDATE then demoted that
        // legitimate monitor too — indistinguishable from a name-guessed
        // one, since both use the identical "{environment} Monitor" naming
        // convention — causing reconciliation to create a second, duplicate
        // managed monitor on the very next boot.
        //
        // There is no data-driven way to tell a name-guessed row from a
        // legitimately created one after the fact, so correcting one case
        // by construction reintroduces the other. Given the guessing bug
        // never reached a standalone release (no evidence any production
        // database ever ran it), this migration is kept registered for
        // migration-history compatibility but no longer touches data.
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // up() is now a no-op, so on a database that first ran this
        // migration's current version there is nothing to restore. But a
        // database that already ran the previous, destructive up() (before
        // this fix) has a populated
        // `_temps_m20260904_managed_monitor_ownership_backup` table and
        // demoted ownership from that run; rolling back on the new binary
        // must still restore that captured state and drop the table,
        // otherwise those monitors stay demoted forever and reconciliation
        // creates a replacement.
        let connection = manager.get_connection();
        let backup_table_exists = connection
            .query_one(sea_orm::Statement::from_string(
                manager.get_database_backend(),
                "SELECT to_regclass('_temps_m20260904_managed_monitor_ownership_backup') IS NOT NULL AS exists"
                    .to_string(),
            ))
            .await?
            .map(|row| row.try_get::<bool>("", "exists"))
            .transpose()?
            .unwrap_or(false);

        if !backup_table_exists {
            return Ok(());
        }

        connection
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
