// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager:&SchemaManager)->Result<(),DbErr> {
        manager.get_connection().execute_unprepared("ALTER TABLE workflow_steps ADD COLUMN managed_revision TEXT;
        CREATE TABLE workflow_managed_runs(run_id UUID PRIMARY KEY REFERENCES workflow_runs(id) ON DELETE CASCADE,service_id INTEGER NOT NULL REFERENCES external_services(id) ON DELETE RESTRICT,project_id INTEGER NOT NULL);
        CREATE TABLE workflow_journal_cleanup(run_id UUID PRIMARY KEY,service_id INTEGER NOT NULL REFERENCES external_services(id) ON DELETE RESTRICT,project_id INTEGER NOT NULL,created_at TIMESTAMPTZ NOT NULL DEFAULT now());
        CREATE FUNCTION queue_workflow_journal_cleanup() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO workflow_journal_cleanup(run_id,service_id,project_id) VALUES(OLD.run_id,OLD.service_id,OLD.project_id) ON CONFLICT DO NOTHING; RETURN OLD; END $$;
        CREATE TRIGGER workflow_journal_cleanup_on_delete BEFORE DELETE ON workflow_managed_runs FOR EACH ROW EXECUTE FUNCTION queue_workflow_journal_cleanup();").await?;Ok(())
    }
    async fn down(&self,manager:&SchemaManager)->Result<(),DbErr> {
        manager.get_connection().execute_unprepared("DO $$ BEGIN IF EXISTS(SELECT 1 FROM workflow_managed_runs) OR EXISTS(SELECT 1 FROM workflow_journal_cleanup) THEN RAISE EXCEPTION 'Delete managed workflow runs and finish journal cleanup before rollback'; END IF; END $$;
        DROP TRIGGER workflow_journal_cleanup_on_delete ON workflow_managed_runs;
        DROP FUNCTION queue_workflow_journal_cleanup(); DROP TABLE workflow_journal_cleanup; DROP TABLE workflow_managed_runs; ALTER TABLE workflow_steps DROP COLUMN managed_revision;").await?;Ok(())
    }
}
