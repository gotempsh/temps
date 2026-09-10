// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add a per-environment HTTP→HTTPS redirect override. The column is
        // NULLABLE with NO default, matching the `attack_mode` tri-state:
        //
        //   NULL         → inherit the proxy default (redirect only when the
        //                  requested host has an active TLS certificate)
        //   TRUE         → always redirect plain HTTP to HTTPS for this
        //                  environment, even when no cert is registered locally
        //                  (e.g. TLS terminated by an upstream CDN)
        //   FALSE        → never redirect this environment, even when a cert
        //                  exists (HTTP-only clients, appliances, local rigs)
        //
        // Existing rows get NULL on upgrade, preserving today's behaviour exactly.
        manager
            .alter_table(
                Table::alter()
                    .table(Environments::Table)
                    .add_column(ColumnDef::new(Environments::ForceHttps).boolean().null())
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
                    .drop_column(Environments::ForceHttps)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Environments {
    Table,
    ForceHttps,
}
