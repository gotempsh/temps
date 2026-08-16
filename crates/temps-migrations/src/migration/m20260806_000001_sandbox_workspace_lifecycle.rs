//! Persistent workspace sandboxes (ADR-036).
//!
//! Adds the lifecycle class that separates throwaway sandboxes from
//! long-lived workspaces:
//!
//! - `sandboxes.lifecycle`: `'ephemeral'` (default, existing behaviour) or
//!   `'workspace'`. A workspace is still suspended on idle by the
//!   expiration sweeper — the difference is that any subsequent access
//!   wakes it instead of returning `409 InvalidState`.
//! - `sandboxes.project_id` (nullable, indexed): the project a workspace
//!   was created from, so workspaces can be listed per project and the
//!   clone URL / git connection can be derived at create time. No FK —
//!   consistent with `agent_run_id`, and a deleted project must not
//!   cascade into deleting a user's working tree.
//! - `sandboxes.source_repo_url` (nullable): the repo the work dir was
//!   seeded from, recorded for display. Never contains credentials —
//!   the create path rejects URLs with embedded userinfo.
//!
//! Existing rows default to `'ephemeral'`, so behaviour is unchanged for
//! everything created before this migration.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(
            "ALTER TABLE sandboxes ADD COLUMN IF NOT EXISTS lifecycle VARCHAR(32) NOT NULL DEFAULT 'ephemeral'",
        )
        .await?;
        conn.execute_unprepared(
            "ALTER TABLE sandboxes ADD COLUMN IF NOT EXISTS project_id INTEGER",
        )
        .await?;
        conn.execute_unprepared(
            "ALTER TABLE sandboxes ADD COLUMN IF NOT EXISTS source_repo_url TEXT",
        )
        .await?;
        // Partial index: workspaces are the minority of rows, and the
        // only queries that filter on lifecycle are looking for them.
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_sandboxes_lifecycle ON sandboxes (lifecycle) WHERE lifecycle <> 'ephemeral'",
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_sandboxes_project_id ON sandboxes (project_id) WHERE project_id IS NOT NULL",
        )
        .await?;

        // Backfill the work dir that `create_sandbox` used to hardcode.
        //
        // Rows created before the fix carry '/workspace', which does not exist
        // in the sandbox image — the real directory is '/home/temps/workspace'.
        // That column is handed to API clients as `cwd`, so an SDK caller
        // using it as an exec working directory gets a path that has never
        // existed. Correcting the code without correcting the rows leaves
        // every pre-existing sandbox broken in exactly the way this PR set out
        // to fix. Scoped to the wrong value so a custom work dir is untouched.
        conn.execute_unprepared(
            "UPDATE sandboxes SET work_dir = '/home/temps/workspace' WHERE work_dir = '/workspace'",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared("DROP INDEX IF EXISTS idx_sandboxes_project_id")
            .await?;
        conn.execute_unprepared("DROP INDEX IF EXISTS idx_sandboxes_lifecycle")
            .await?;
        conn.execute_unprepared("ALTER TABLE sandboxes DROP COLUMN IF EXISTS source_repo_url")
            .await?;
        conn.execute_unprepared("ALTER TABLE sandboxes DROP COLUMN IF EXISTS project_id")
            .await?;
        conn.execute_unprepared("ALTER TABLE sandboxes DROP COLUMN IF EXISTS lifecycle")
            .await?;
        Ok(())
    }
}
