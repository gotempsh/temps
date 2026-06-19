//! Add `alarms.service_id` so service-scoped (database) alarms record which
//! external service triggered them.
//!
//! Service-scoped alarms are fired by `AlertEvaluator` for `monitoring_alert_rules`
//! that carry a `service_id` (e.g. a Redis `memory_fragmentation_ratio` rule).
//! Until now the alarm row only stored the owning `project_id` — the service
//! identity was resolved to find the project and then discarded, so an operator
//! receiving the alarm/email could only see "Project 4" with no indication of
//! *which* service (redis, postgres, …) actually breached.
//!
//! The column is nullable: container/outage/deployment alarms have no service.
//! The FK uses `ON DELETE SET NULL` (matching `environment_id`/`deployment_id`)
//! so deleting a service leaves historical alarms intact for reporting.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'alarms' AND column_name = 'service_id'
    ) THEN
        ALTER TABLE alarms
            ADD COLUMN service_id INT REFERENCES external_services(id) ON DELETE SET NULL;
    END IF;
END
$$;
"#,
        )
        .await?;

        // Index the new column so listing alarms for a given service is cheap.
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_alarms_service_id ON alarms (service_id)",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP INDEX IF EXISTS idx_alarms_service_id")
            .await?;
        db.execute_unprepared(
            r#"
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'alarms' AND column_name = 'service_id'
    ) THEN
        ALTER TABLE alarms DROP COLUMN service_id;
    END IF;
END
$$;
"#,
        )
        .await?;
        Ok(())
    }
}
