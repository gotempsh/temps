//! Migration that adds `max_runtime_secs` to `backup_jobs`.
//!
//! Before this migration the runner used a single hard-coded constant
//! (`DEFAULT_JOB_MAX_RUNTIME = 30 min`) that was too short for large Postgres
//! workloads (a 200 GB cluster over a slow upload link was killed at 30m1s in
//! the May 2026 incident).
//!
//! The new column bakes the *resolved* timeout into each row at enqueue time so
//! the runner never needs to look up the engine key again at dispatch time. The
//! resolution order is: caller-supplied override → schedule-level override →
//! engine default (24 h for Postgres, 12 h for S3, 4 h for Redis / Mongo).
//!
//! `DEFAULT 86400` (24 h) is intentionally permissive: existing rows in any
//! production database will be given a 24 h timeout rather than the former 30 m.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
ALTER TABLE backup_jobs
    ADD COLUMN IF NOT EXISTS max_runtime_secs BIGINT NOT NULL DEFAULT 86400;
            "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
-- IF EXISTS on the table too: m20260517_000002 drops backup_jobs with a
-- no-op down(), so a full rollback reaches this migration with the table
-- already gone.
ALTER TABLE IF EXISTS backup_jobs DROP COLUMN IF EXISTS max_runtime_secs;
            "#,
        )
        .await?;

        Ok(())
    }
}
