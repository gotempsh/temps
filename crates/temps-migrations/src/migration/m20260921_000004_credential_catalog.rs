// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            "CREATE TABLE IF NOT EXISTS env_check_suppressions (env_var_id INTEGER NOT NULL REFERENCES env_vars(id) ON DELETE CASCADE,automatic_provider TEXT NOT NULL,PRIMARY KEY(env_var_id,automatic_provider)); \
             INSERT INTO env_check_suppressions(env_var_id,automatic_provider) \
             SELECT detection.env_var_id,'*' FROM env_check_detection AS detection \
             WHERE NOT EXISTS(SELECT 1 FROM http_checks WHERE env_var_id=detection.env_var_id) \
               AND EXISTS(SELECT 1 FROM env_var_history AS removed WHERE removed.env_var_id=detection.env_var_id AND removed.kind='check_removed' AND NOT EXISTS(SELECT 1 FROM env_var_history AS changed WHERE changed.env_var_id=detection.env_var_id AND changed.kind IN ('value_changed','settings_changed') AND changed.id>removed.id)) \
             ON CONFLICT(env_var_id,automatic_provider) DO NOTHING; \
             DELETE FROM env_check_detection WHERE NOT EXISTS(SELECT 1 FROM env_check_suppressions WHERE env_check_suppressions.env_var_id=env_check_detection.env_var_id)"
        ).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS env_check_suppressions")
            .await?;
        Ok(())
    }
}
