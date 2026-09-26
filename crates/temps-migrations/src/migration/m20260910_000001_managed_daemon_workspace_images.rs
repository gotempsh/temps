// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE ai_application_workspaces
             DROP CONSTRAINT ai_application_workspaces_image_check,
             ADD CONSTRAINT ai_application_workspaces_image_check CHECK (image IS NULL OR image IN (
               'ghcr.io/gotempsh/temps-sandbox-nodejs:0.2.0',
               'ghcr.io/gotempsh/temps-sandbox-python:0.2.0',
               'ghcr.io/gotempsh/temps-sandbox-all:0.2.0'))",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Fail without changing records if an operator still uses daemon images.
        // Downgrade must not silently replace a workspace's selected runtime.
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE ai_application_workspaces
             DROP CONSTRAINT ai_application_workspaces_image_check,
             ADD CONSTRAINT ai_application_workspaces_image_check CHECK (image IS NULL)",
            )
            .await?;
        Ok(())
    }
}
