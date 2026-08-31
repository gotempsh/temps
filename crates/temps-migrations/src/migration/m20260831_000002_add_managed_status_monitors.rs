// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("status_monitors"))
                    .add_column(
                        ColumnDef::new(Alias::new("is_managed"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        // Before provenance existed, automatic monitors were created with this
        // deterministic name. Backfill only that exact convention; all other
        // monitors remain user-managed and will never be rewritten by deploys.
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE status_monitors AS monitor \
                 SET is_managed = TRUE \
                 FROM environments AS environment \
                 WHERE monitor.environment_id = environment.id \
                   AND monitor.name = environment.name || ' Monitor' \
                   AND monitor.id = ( \
                       SELECT MIN(candidate.id) \
                       FROM status_monitors AS candidate \
                       WHERE candidate.environment_id = environment.id \
                         AND candidate.name = environment.name || ' Monitor' \
                   )",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE UNIQUE INDEX idx_status_monitors_managed_environment \
                 ON status_monitors (environment_id) \
                 WHERE is_managed = TRUE",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_status_monitors_managed_environment")
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("status_monitors"))
                    .drop_column(Alias::new("is_managed"))
                    .to_owned(),
            )
            .await
    }
}
