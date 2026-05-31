use sea_orm_migration::prelude::*;

/// Adds a partial unique index on `monitoring_alert_rules(service_id, metric_name)`
/// and a separate one on `(deployment_id, metric_name)` so that
/// `seed_default_rules` can use `INSERT … ON CONFLICT DO NOTHING` to be
/// truly idempotent even under concurrent calls (the previous SELECT-then-INSERT
/// check had a TOCTOU race).
///
/// The indexes are partial (WHERE clause) to correctly handle NULLs: in
/// PostgreSQL, a standard UNIQUE constraint treats NULLs as distinct, so a
/// partial index with `WHERE service_id IS NOT NULL` is the correct pattern.
///
/// **Safely re-runnable:** wrapped in `CREATE INDEX IF NOT EXISTS`.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
CREATE UNIQUE INDEX IF NOT EXISTS uidx_monitoring_alert_rules_service_metric
    ON monitoring_alert_rules (service_id, metric_name)
    WHERE service_id IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS uidx_monitoring_alert_rules_deployment_metric
    ON monitoring_alert_rules (deployment_id, metric_name)
    WHERE deployment_id IS NOT NULL;
"#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
DROP INDEX IF EXISTS uidx_monitoring_alert_rules_service_metric;
DROP INDEX IF EXISTS uidx_monitoring_alert_rules_deployment_metric;
"#,
        )
        .await?;

        Ok(())
    }
}
