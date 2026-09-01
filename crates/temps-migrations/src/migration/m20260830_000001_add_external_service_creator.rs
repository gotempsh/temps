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
                    .table(ExternalServices::Table)
                    .add_column(
                        ColumnDef::new(ExternalServices::CreatedByUserId)
                            .integer()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_external_services_created_by_user")
                    .from(ExternalServices::Table, ExternalServices::CreatedByUserId)
                    .to(Users::Table, Users::Id)
                    .on_delete(ForeignKeyAction::SetNull)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_external_services_created_by_user")
                    .table(ExternalServices::Table)
                    .col(ExternalServices::CreatedByUserId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_external_services_created_by_user")
                    .table(ExternalServices::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_foreign_key(
                ForeignKey::drop()
                    .name("fk_external_services_created_by_user")
                    .table(ExternalServices::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ExternalServices::Table)
                    .drop_column(ExternalServices::CreatedByUserId)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum ExternalServices {
    Table,
    CreatedByUserId,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}
