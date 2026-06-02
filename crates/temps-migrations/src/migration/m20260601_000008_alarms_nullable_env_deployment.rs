//! Make `alarms.environment_id` and `alarms.deployment_id` nullable.
//!
//! Database-/service-scoped alarms (from `AlertEvaluator` rules with a
//! `service_id` and no `deployment_id`) have no real environment or deployment
//! to point at. The original schema declared both columns `NOT NULL` with
//! foreign keys, which caused every such alarm INSERT to fail with
//! `fk_alarms_environment` / `fk_alarms_deployment` violations because the
//! evaluator was writing sentinel `0` values.
//!
//! Fix: drop NOT NULL on both columns, drop the original `ON DELETE CASCADE`
//! FKs, and recreate them with `ON DELETE SET NULL` so a deleted environment
//! or deployment leaves the alarm record intact for historical reporting.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Drop old FKs (created in m20260308_000001_create_alarms_table).
        db.execute_unprepared("ALTER TABLE alarms DROP CONSTRAINT IF EXISTS fk_alarms_environment")
            .await?;
        db.execute_unprepared("ALTER TABLE alarms DROP CONSTRAINT IF EXISTS fk_alarms_deployment")
            .await?;

        // Drop NOT NULL constraints.
        db.execute_unprepared("ALTER TABLE alarms ALTER COLUMN environment_id DROP NOT NULL")
            .await?;
        db.execute_unprepared("ALTER TABLE alarms ALTER COLUMN deployment_id DROP NOT NULL")
            .await?;

        // Recreate FKs as ON DELETE SET NULL so historical alarms survive
        // environment/deployment deletion.
        db.execute_unprepared(
            "ALTER TABLE alarms ADD CONSTRAINT fk_alarms_environment \
             FOREIGN KEY (environment_id) REFERENCES environments(id) ON DELETE SET NULL",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE alarms ADD CONSTRAINT fk_alarms_deployment \
             FOREIGN KEY (deployment_id) REFERENCES deployments(id) ON DELETE SET NULL",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Revert FKs back to CASCADE and NOT NULL. Any rows with NULL
        // environment_id / deployment_id are deleted first so the NOT NULL
        // restoration succeeds — those rows can only exist as a result of the
        // new behaviour and have no meaningful FK target.
        db.execute_unprepared(
            "DELETE FROM alarms WHERE environment_id IS NULL OR deployment_id IS NULL",
        )
        .await?;

        db.execute_unprepared("ALTER TABLE alarms DROP CONSTRAINT IF EXISTS fk_alarms_environment")
            .await?;
        db.execute_unprepared("ALTER TABLE alarms DROP CONSTRAINT IF EXISTS fk_alarms_deployment")
            .await?;

        db.execute_unprepared("ALTER TABLE alarms ALTER COLUMN environment_id SET NOT NULL")
            .await?;
        db.execute_unprepared("ALTER TABLE alarms ALTER COLUMN deployment_id SET NOT NULL")
            .await?;

        db.execute_unprepared(
            "ALTER TABLE alarms ADD CONSTRAINT fk_alarms_environment \
             FOREIGN KEY (environment_id) REFERENCES environments(id) ON DELETE CASCADE",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE alarms ADD CONSTRAINT fk_alarms_deployment \
             FOREIGN KEY (deployment_id) REFERENCES deployments(id) ON DELETE CASCADE",
        )
        .await?;

        Ok(())
    }
}
