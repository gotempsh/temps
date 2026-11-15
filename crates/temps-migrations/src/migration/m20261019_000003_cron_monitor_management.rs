// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("ALTER TABLE cron_monitors ADD COLUMN enabled BOOLEAN NOT NULL DEFAULT true, ADD COLUMN managed_config BOOLEAN NOT NULL DEFAULT false; ALTER TABLE cron_monitors ALTER COLUMN last_check_in_at DROP NOT NULL; DROP INDEX cron_monitors_due; CREATE INDEX cron_monitors_due ON cron_monitors(next_check_at) WHERE enabled").await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("DROP INDEX cron_monitors_due; CREATE INDEX cron_monitors_due ON cron_monitors(next_check_at); UPDATE cron_monitors SET last_check_in_at=NOW() WHERE last_check_in_at IS NULL; ALTER TABLE cron_monitors ALTER COLUMN last_check_in_at SET NOT NULL; ALTER TABLE cron_monitors DROP COLUMN enabled, DROP COLUMN managed_config").await?;
        Ok(())
    }
}
