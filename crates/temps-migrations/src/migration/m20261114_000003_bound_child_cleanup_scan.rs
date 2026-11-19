// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Persist a bounded scan position without rewriting existing prepared inputs.
//! Rollback loses only the scan position; retained cleanup ownership is unchanged.
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("CREATE TABLE IF NOT EXISTS workflow_child_cleanup_cursor (
            singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK(singleton),
            last_run_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000');
            INSERT INTO workflow_child_cleanup_cursor(singleton) VALUES(TRUE) ON CONFLICT DO NOTHING;").await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("DROP TABLE IF EXISTS workflow_child_cleanup_cursor").await?;
        Ok(())
    }
}
