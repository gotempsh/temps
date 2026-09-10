// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Add `preview_password_encrypted` to `workspace_sessions`.
//!
//! DORMANT: the temps-workspace feature was removed; this migration is kept
//! for backward compatibility with databases that already ran it. No active
//! code reads or writes the column.
//!
//! Historical context: previously, the per-session preview password was only
//! stored as an argon2 PHC hash plus a 4-char hint. This column added an
//! AES-256-GCM ciphertext of the plaintext (using the platform
//! `EncryptionService`) so the password could be returned by subsequent
//! `GET /sessions` reads without forcing the user to regenerate.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(WorkspaceSessions::Table)
                    .add_column(
                        ColumnDef::new(WorkspaceSessions::PreviewPasswordEncrypted)
                            .text()
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
                    .table(WorkspaceSessions::Table)
                    .drop_column(WorkspaceSessions::PreviewPasswordEncrypted)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum WorkspaceSessions {
    Table,
    PreviewPasswordEncrypted,
}
