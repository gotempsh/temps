// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Preserve known run IDs and encrypted executor snapshots as durable
        // cancellation tombstones after deployment/project deletion. The
        // reconciler removes orphan secrets only after provider cancellation.
        manager.get_connection().execute_unprepared(
            "ALTER TABLE ci_builds DROP CONSTRAINT IF EXISTS ci_builds_deployment_id_fkey;
             ALTER TABLE ci_builds ALTER COLUMN deployment_id DROP NOT NULL;
             ALTER TABLE ci_builds ADD CONSTRAINT ci_builds_deployment_id_fkey FOREIGN KEY (deployment_id) REFERENCES deployments(id) ON DELETE SET NULL;
             CREATE INDEX IF NOT EXISTS ci_builds_orphan_cleanup ON ci_builds(updated_at, id) WHERE deployment_id IS NULL;"
        ).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Old binaries require a deployment ID on every build. Refuse rollback
        // until reconciliation has cancelled and removed all orphan records.
        manager.get_connection().execute_unprepared(
            "DO $$ BEGIN
                 IF EXISTS (SELECT 1 FROM ci_builds WHERE deployment_id IS NULL) THEN
                     RAISE EXCEPTION 'Cannot roll back CI cancellation retention while orphan builds remain; run cancellation reconciliation first';
                 END IF;
             END $$;
             DROP INDEX IF EXISTS ci_builds_orphan_cleanup;
             ALTER TABLE ci_builds DROP CONSTRAINT IF EXISTS ci_builds_deployment_id_fkey;
             ALTER TABLE ci_builds ALTER COLUMN deployment_id SET NOT NULL;
             ALTER TABLE ci_builds ADD CONSTRAINT ci_builds_deployment_id_fkey FOREIGN KEY (deployment_id) REFERENCES deployments(id) ON DELETE CASCADE;"
        ).await?;
        Ok(())
    }
}
