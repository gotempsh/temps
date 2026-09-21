// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Re-evaluate existing variables with the expanded issuer policy. This
        // removes only scan markers; existing checks, pause state and history stay.
        manager
            .get_connection()
            .execute_unprepared("DELETE FROM env_check_detection")
            .await?;
        Ok(())
    }
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Scan markers are a disposable cache and cannot be reconstructed.
        Ok(())
    }
}
