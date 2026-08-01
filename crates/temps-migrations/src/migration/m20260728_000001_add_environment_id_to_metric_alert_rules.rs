//! Add `environment_id` (nullable FK) to `metric_alert_rules`.
//!
//! Config-as-code metric alert rules (declared in `.temps.yaml`) are
//! environment-scoped: they carry the environment they were deployed from so
//! the reconciliation job can upsert by `(project_id, environment_id, name)`
//! and cleanly disable orphaned rules without touching rules from other
//! environments.
//!
//! Existing project-scoped rules (created via the UI/API) keep
//! `environment_id = NULL` and continue to work exactly as before — the
//! evaluator and service layer treat NULL as "not environment-scoped". The
//! column is therefore nullable; no migration-time UPDATE is required.
//!
//! A composite index on `(project_id, environment_id)` is added to keep
//! per-environment lookups efficient.

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
                 ADD COLUMN IF NOT EXISTS environment_id integer \
                     REFERENCES environments(id) ON DELETE CASCADE; \
                 CREATE INDEX IF NOT EXISTS idx_metric_alert_rules_project_env \
                     ON metric_alert_rules (project_id, environment_id);",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DROP INDEX IF EXISTS idx_metric_alert_rules_project_env; \
                 ALTER TABLE metric_alert_rules \
                     DROP COLUMN IF EXISTS environment_id;",
            )
            .await?;
        Ok(())
    }
}
