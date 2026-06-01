use sea_orm_migration::prelude::*;

/// Updates metrics retention on databases provisioned before this migration:
/// - raw `service_metrics`: 7 days → 30 days
/// - `service_metrics_daily`: 2 years (730d) → 1 year (365d)
///
/// The original migration used `if_not_exists => TRUE` so existing policies
/// were not replaced.  This migration explicitly removes the old policies and
/// re-creates them at the new intervals.
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
    -- Raw: 7 days → 30 days. (remove is idempotent via if_exists)
    PERFORM remove_retention_policy('service_metrics', if_exists => TRUE);
    PERFORM add_retention_policy(
        'service_metrics',
        INTERVAL '30 days',
        if_not_exists => TRUE
    );

    -- Daily aggregate: 2 years → 1 year.
    PERFORM remove_retention_policy('service_metrics_daily', if_exists => TRUE);
    PERFORM add_retention_policy(
        'service_metrics_daily',
        INTERVAL '365 days',
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
    PERFORM remove_retention_policy('service_metrics', if_exists => TRUE);
    PERFORM add_retention_policy(
        'service_metrics',
        INTERVAL '7 days',
        if_not_exists => TRUE
    );

    PERFORM remove_retention_policy('service_metrics_daily', if_exists => TRUE);
    PERFORM add_retention_policy(
        'service_metrics_daily',
        INTERVAL '730 days',
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
