use sea_orm_migration::prelude::*;

/// Records each node's container platform (`linux/amd64`, `linux/arm64`, ...)
/// so the scheduler can stop placing an app image on a node that cannot run it.
///
/// Before this column, a worker of a different architecture joined the cluster
/// silently: the control plane built an amd64 image, `docker save`d it, pushed
/// the tar to an arm64 agent, and the container died with `exec format error`
/// with nothing in the deploy log explaining why.
///
/// **Nullable on purpose.** Agents older than this change don't report a
/// platform, and a node that has not heartbeated since the upgrade must keep
/// working exactly as before (the scheduler treats `NULL` as "unknown, assume
/// compatible" and logs a warning) rather than being dropped from the pool on
/// a rolling upgrade. The value is filled in by the next heartbeat.
///
/// **Safely re-runnable:** `ADD COLUMN IF NOT EXISTS`.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
ALTER TABLE nodes
    ADD COLUMN IF NOT EXISTS architecture TEXT;
"#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
ALTER TABLE nodes
    DROP COLUMN IF EXISTS architecture;
"#,
        )
        .await?;

        Ok(())
    }
}
