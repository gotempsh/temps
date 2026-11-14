// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One executor may consume a lease, even across replicas. Existing leases
//! have no admission until an updated SDK claims them. No history backfill.
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("CREATE TABLE IF NOT EXISTS workflow_invocation_admissions (
            run_id UUID PRIMARY KEY REFERENCES workflow_runs(id) ON DELETE CASCADE,
            lease BIGINT NOT NULL);").await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("DO $$ BEGIN
            IF EXISTS(SELECT 1 FROM workflow_invocation_admissions a JOIN workflow_runs r ON r.id=a.run_id WHERE r.status='running') THEN
                RAISE EXCEPTION 'Drain admitted workflow invocations before rollback'; END IF;
            END $$; DROP TABLE IF EXISTS workflow_invocation_admissions;").await?;
        Ok(())
    }
}
