//! Add `label_filters` (jsonb, default `[]`) to `metric_alert_rules`.
//!
//! Format: `[["key1","val1"],["key2","val2"]]` — an AND-combined list of
//! equality pairs. Empty array (the default) means no filtering (today's
//! behaviour), so all existing rows continue to evaluate as before.
//!
//! This is Phase 1 of ADR-026: label-scoped alert evaluation. The column is
//! NOT NULL with a `[]` default so every existing row round-trips as
//! "no filters" without a migration-time UPDATE.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE metric_alert_rules \
                 ADD COLUMN IF NOT EXISTS label_filters jsonb NOT NULL DEFAULT '[]'::jsonb;",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE metric_alert_rules DROP COLUMN IF EXISTS label_filters;",
            )
            .await?;
        Ok(())
    }
}
