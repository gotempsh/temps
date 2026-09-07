// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Repair application topologies that predate or bypassed primary-project
//! selection. Every non-empty application must have exactly one primary.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE ai_application_projects AS link SET is_primary = TRUE \
                 WHERE link.id = (SELECT first_link.id FROM ai_application_projects AS first_link \
                   WHERE first_link.application_id = link.application_id \
                   ORDER BY first_link.id LIMIT 1) \
                   AND NOT EXISTS (SELECT 1 FROM ai_application_projects AS selected \
                     WHERE selected.application_id = link.application_id \
                       AND selected.is_primary = TRUE);",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Data repair is intentionally irreversible.
        Ok(())
    }
}
