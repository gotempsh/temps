// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Record when a workspace owner explicitly sets a limit, including a value
//! equal to its default. Older writers materialized both defaults in every
//! row. A non-default value from those writers remains an override. The
//! original numeric columns and quota accounting survive a binary rollback.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            "ALTER TABLE ai_application_workspaces
                ADD COLUMN IF NOT EXISTS cpu_limit_explicit boolean NOT NULL DEFAULT false,
                ADD COLUMN IF NOT EXISTS memory_limit_explicit boolean NOT NULL DEFAULT false"
        ).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            "ALTER TABLE ai_application_workspaces
                DROP COLUMN IF EXISTS cpu_limit_explicit,
                DROP COLUMN IF EXISTS memory_limit_explicit"
        ).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::ConnectionTrait;

    #[tokio::test]
    async fn workspace_limit_flags_preserve_legacy_rows_and_rollback() -> Result<(), Box<dyn std::error::Error>> {
        if !temps_test_utils::external_database_configured() && !temps_test_utils::docker_available().await {
            eprintln!("Skipping workspace limit migration: database unavailable");
            return Ok(());
        }
        let mut test = temps_test_utils::TestDatabase::new().await?;
        let db = test.connection_arc();
        db.execute_unprepared(
            "CREATE TABLE ai_application_workspaces (cpu_limit float8, memory_limit_mb bigint);
             INSERT INTO ai_application_workspaces VALUES (4, 8192), (1, 1024)"
        ).await?;
        let manager = SchemaManager::new(db.as_ref());
        Migration.up(&manager).await?;
        Migration.up(&manager).await?;
        let old = db.execute_unprepared("UPDATE ai_application_workspaces SET cpu_limit_explicit = true WHERE cpu_limit = 4 AND NOT cpu_limit_explicit AND NOT memory_limit_explicit").await?;
        assert_eq!(old.rows_affected(), 1);
        Migration.down(&manager).await?;
        let result = db.execute_unprepared("DELETE FROM ai_application_workspaces WHERE (cpu_limit, memory_limit_mb) IN ((4, 8192), (1, 1024))").await?;
        assert_eq!(result.rows_affected(), 2);
        test.cleanup().await;
        Ok(())
    }
}
