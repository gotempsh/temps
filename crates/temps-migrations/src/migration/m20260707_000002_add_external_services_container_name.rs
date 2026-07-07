//! Add a plaintext `container_name` column to `external_services` so the log
//! collector can map a running container back to its service.
//!
//! Imported external-service containers pre-date Temps: they carry no
//! `temps.*` Docker labels (labels are immutable, and import only attaches the
//! container to the Temps network — it can't relabel), and their real
//! container name lives inside the ENCRYPTED `config` blob, which the
//! log-aggregator can't decrypt. This plaintext column lets the collector
//! resolve `inspected container name -> external_services.id` for imported
//! services. Created services still resolve via the `temps.service_name`
//! Docker label, so this stays NULL for them.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260707_000002_add_external_services_container_name"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE external_services
                    ADD COLUMN IF NOT EXISTS container_name text;
                -- Collector resolves imported containers by exact name match.
                CREATE INDEX IF NOT EXISTS idx_external_services_container_name
                    ON external_services (container_name)
                    WHERE container_name IS NOT NULL;
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                DROP INDEX IF EXISTS idx_external_services_container_name;
                ALTER TABLE external_services DROP COLUMN IF EXISTS container_name;
                "#,
            )
            .await?;
        Ok(())
    }
}
