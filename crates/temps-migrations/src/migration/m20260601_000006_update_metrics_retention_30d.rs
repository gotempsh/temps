use sea_orm_migration::prelude::*;

/// Updates the `service_metrics` raw table retention policy from 7 days to
/// 30 days on databases provisioned before this migration.
///
/// The original migration used `if_not_exists => TRUE` so existing policies
/// were not replaced.  This migration explicitly removes the old policy and
/// re-creates it at 30 days.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
DO $$
BEGIN
    -- Remove existing raw retention policy (drop is idempotent — no error if absent).
    PERFORM remove_retention_policy('service_metrics', if_not_exists => TRUE);

    -- Re-create at 30 days.
    PERFORM add_retention_policy(
        'service_metrics',
        INTERVAL '30 days',
        if_not_exists => TRUE
    );
END
$$;
"#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
DO $$
BEGIN
    PERFORM remove_retention_policy('service_metrics', if_not_exists => TRUE);
    PERFORM add_retention_policy(
        'service_metrics',
        INTERVAL '7 days',
        if_not_exists => TRUE
    );
END
$$;
"#,
        )
        .await?;

        Ok(())
    }
}
