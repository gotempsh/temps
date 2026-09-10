// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add a per-environment attack_mode override. The column is NULLABLE with
        // NO default: NULL means "inherit the project-level attack_mode", while
        // true/false explicitly override it for this environment. Existing rows
        // get NULL on upgrade, preserving today's behaviour exactly.
        manager
            .alter_table(
                Table::alter()
                    .table(Environments::Table)
                    .add_column(ColumnDef::new(Environments::AttackMode).boolean().null())
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
                    .drop_column(Environments::AttackMode)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Environments {
    Table,
    AttackMode,
}
