// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Last proxied request timestamp for on-demand environments.
        // Persisted periodically by the idle sweep (not on every request).
        // NULL when on-demand is disabled or no traffic has been received yet.
        manager
            .alter_table(
                Table::alter()
                    .table(Environments::Table)
                    .add_column(
                        ColumnDef::new(Environments::LastActivityAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Environments::Table)
                    .drop_column(Environments::LastActivityAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Environments {
    Table,
    LastActivityAt,
}
