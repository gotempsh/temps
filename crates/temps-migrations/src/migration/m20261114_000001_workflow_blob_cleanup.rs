// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Keep blob cleanup retryable after manual, retention or cascading run deletion.
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("CREATE TABLE workflow_blob_cleanup (
            run_id UUID PRIMARY KEY, project_id INTEGER NOT NULL, environment_id INTEGER NOT NULL,
            created_at TIMESTAMPTZ NOT NULL DEFAULT now());
            CREATE INDEX workflow_blob_cleanup_age ON workflow_blob_cleanup(created_at,run_id);
            CREATE FUNCTION queue_workflow_blob_cleanup() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
                IF OLD.has_blob_payloads THEN
                    INSERT INTO workflow_blob_cleanup(run_id,project_id,environment_id)
                    VALUES(OLD.id,OLD.project_id,OLD.environment_id) ON CONFLICT DO NOTHING;
                END IF;
                RETURN OLD;
            END $$;
            CREATE TRIGGER workflow_blob_cleanup_on_delete BEFORE DELETE ON workflow_runs
            FOR EACH ROW EXECUTE FUNCTION queue_workflow_blob_cleanup();").await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("DO $$ BEGIN
            IF EXISTS(SELECT 1 FROM workflow_blob_cleanup) THEN
                RAISE EXCEPTION 'Finish workflow blob cleanup before rollback'; END IF; END $$;
            DROP TRIGGER workflow_blob_cleanup_on_delete ON workflow_runs;
            DROP FUNCTION queue_workflow_blob_cleanup(); DROP TABLE workflow_blob_cleanup;").await?;
        Ok(())
    }
}
