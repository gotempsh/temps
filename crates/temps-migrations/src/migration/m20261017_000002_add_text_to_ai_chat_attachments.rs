// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Keep a PDF's extracted text with the attachment row.
//!
//! The text layer is read once, at upload, so a send never re-parses an
//! untrusted document and the user is told at once when a PDF has no text.
//! Null for every other kind of file, and for a PDF that could not be read.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(AiChatAttachments::Table)
                    .add_column_if_not_exists(
                        ColumnDef::new(AiChatAttachments::ExtractedText)
                            .text()
                            .null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(AiChatAttachments::Table)
                    .drop_column(AiChatAttachments::ExtractedText)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum AiChatAttachments {
    Table,
    ExtractedText,
}
