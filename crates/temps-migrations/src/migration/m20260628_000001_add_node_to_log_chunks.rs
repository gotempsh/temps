//! Add node attribution + container/node search indexes to `log_chunks`.
//!
//! Runtime log history was control-plane-only: the collector streamed from the
//! local Docker daemon, so containers on remote worker nodes never appeared in
//! `/api/logs/search`. The remote log collector now feeds those lines into the
//! same chunk pipeline, tagged with the node they ran on.
//!
//! - `node_id` (nullable): the worker node a chunk's container ran on. `NULL`
//!   means a control-plane-local container (collected via local Docker).
//! - `node_name` (nullable): denormalized node name so history results can show
//!   the source node without a join.
//!
//! Two indexes support the new "filter by container / filter by node" history
//! browsing across a project that spans many deployments and containers.
//!
//! `log_chunks` is a plain table (not a hypertable), so `ADD COLUMN` is cheap.
//! All statements are idempotent (`IF NOT EXISTS`) so re-runs and mixed
//! histories don't fail.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
            ALTER TABLE log_chunks
                ADD COLUMN IF NOT EXISTS node_id integer,
                ADD COLUMN IF NOT EXISTS node_name varchar;

            CREATE INDEX IF NOT EXISTS idx_log_chunks_project_container_time
                ON log_chunks (project_id, container_id, started_at);

            CREATE INDEX IF NOT EXISTS idx_log_chunks_project_node_time
                ON log_chunks (project_id, node_id, started_at);
            "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
            DROP INDEX IF EXISTS idx_log_chunks_project_node_time;
            DROP INDEX IF EXISTS idx_log_chunks_project_container_time;
            ALTER TABLE log_chunks
                DROP COLUMN IF EXISTS node_name,
                DROP COLUMN IF EXISTS node_id;
            "#,
        )
        .await?;

        Ok(())
    }
}
