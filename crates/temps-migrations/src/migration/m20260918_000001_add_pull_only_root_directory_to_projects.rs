// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Opt-in sparse checkout of `projects.directory` on deploy.
//!
//! Adds `projects.pull_only_root_directory` (default false). When true and
//! the project root is a subdirectory, deploy clones only that path.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .add_column(
                        ColumnDef::new(Projects::PullOnlyRootDirectory)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .drop_column(Projects::PullOnlyRootDirectory)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    PullOnlyRootDirectory,
}
