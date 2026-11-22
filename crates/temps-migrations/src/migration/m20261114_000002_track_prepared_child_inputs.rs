// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Retain cleanup ownership for inputs referenced by uncertain managed writes.
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("CREATE TABLE workflow_prepared_child_inputs (
            run_id UUID PRIMARY KEY, parent_run_id UUID, project_id INTEGER NOT NULL,
            environment_id INTEGER NOT NULL, created_at TIMESTAMPTZ NOT NULL DEFAULT now());
            CREATE INDEX workflow_prepared_child_inputs_age ON workflow_prepared_child_inputs(created_at,run_id);").await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("DO $$ BEGIN
            IF EXISTS(SELECT 1 FROM workflow_prepared_child_inputs) THEN
                RAISE EXCEPTION 'Finish prepared child input cleanup before rollback'; END IF; END $$;
            DROP TABLE workflow_prepared_child_inputs;").await?;
        Ok(())
    }
}
