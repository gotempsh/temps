// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Persist fail-closed application workspace quarantine independently of the
//! current container state.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE ai_application_workspaces \
                   DROP CONSTRAINT IF EXISTS ai_application_workspaces_desired_state_check, \
                   ADD CONSTRAINT ai_application_workspaces_desired_state_check \
                     CHECK (desired_state IN ('running', 'paused', 'quarantined'));",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE ai_application_workspaces SET desired_state = 'paused' \
                   WHERE desired_state = 'quarantined'; \
                 ALTER TABLE ai_application_workspaces \
                   DROP CONSTRAINT IF EXISTS ai_application_workspaces_desired_state_check, \
                   ADD CONSTRAINT ai_application_workspaces_desired_state_check \
                     CHECK (desired_state IN ('running', 'paused'));",
            )
            .await?;
        Ok(())
    }
}
