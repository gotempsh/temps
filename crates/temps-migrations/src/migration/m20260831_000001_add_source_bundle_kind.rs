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
                    .table(SourceBundles::Table)
                    .add_column(
                        ColumnDef::new(SourceBundles::SourceKind)
                            .string_len(32)
                            .not_null()
                            .default("uploaded_source"),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_source_bundles_project_kind_revision")
                    .table(SourceBundles::Table)
                    .col(SourceBundles::ProjectId)
                    .col(SourceBundles::SourceKind)
                    .col(SourceBundles::Id)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_source_bundles_project_kind_revision")
                    .table(SourceBundles::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(SourceBundles::Table)
                    .drop_column(SourceBundles::SourceKind)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum SourceBundles {
    Table,
    Id,
    ProjectId,
    SourceKind,
}
