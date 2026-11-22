// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-055 Phase 4: databases onto the workload plane.
//!
//! `external_services.workload_mode` (`legacy` | `shadow` | `enforce`,
//! default `legacy`) says how a database's containers are created. Every
//! existing database stays on the engine's own create path; rolling back is
//! setting the row back to `legacy`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
ALTER TABLE external_services
    ADD COLUMN IF NOT EXISTS workload_mode TEXT NOT NULL DEFAULT 'legacy';
ALTER TABLE external_services
    DROP CONSTRAINT IF EXISTS external_services_workload_mode_valid;
ALTER TABLE external_services
    ADD CONSTRAINT external_services_workload_mode_valid
        CHECK (workload_mode IN ('legacy', 'shadow', 'enforce'));
-- The shadow pass reads only the rows that opted in.
CREATE INDEX IF NOT EXISTS external_services_workload_mode_active
    ON external_services (id) WHERE workload_mode <> 'legacy';
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
DROP INDEX IF EXISTS external_services_workload_mode_active;
ALTER TABLE external_services
    DROP CONSTRAINT IF EXISTS external_services_workload_mode_valid;
ALTER TABLE external_services DROP COLUMN IF EXISTS workload_mode;
"#,
            )
            .await?;
        Ok(())
    }
}
