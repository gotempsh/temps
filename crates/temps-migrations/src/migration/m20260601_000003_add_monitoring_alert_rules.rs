use sea_orm_migration::prelude::*;

/// Creates the `monitoring_alert_rules` table used by the AlertEvaluator
/// background task.
///
/// Each rule watches a single metric name for a specific entity
/// (either an `external_service` OR a `deployment` — never both).
/// The CHECK constraint enforces this mutual-exclusion at the database level.
///
/// Columns:
/// - `service_id`      — nullable FK to `external_services.id` (CASCADE delete)
/// - `deployment_id`   — nullable FK to `deployments.id` (CASCADE delete)
/// - `name`            — human-readable rule label
/// - `metric_name`     — dotted metric name to evaluate, e.g. `"pg.conn_count"`
/// - `threshold`       — numeric threshold value
/// - `comparator`      — one of `'>'`, `'<'`, `'>='`, `'<='`
/// - `severity`        — `'warning'` or `'critical'`
/// - `for_duration_secs` — how many consecutive seconds the breach must persist
///   before firing (0 = fire immediately on first evaluation)
/// - `enabled`         — soft-disable without deleting the rule
/// - `silenced_until`  — temporary silence (AlertEvaluator skips while active)
///
/// **Safely re-runnable:** wrapped in a PL/pgSQL `IF NOT EXISTS` guard.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
CREATE TABLE IF NOT EXISTS monitoring_alert_rules (
    id                  SERIAL          PRIMARY KEY,
    service_id          INT             REFERENCES external_services(id) ON DELETE CASCADE,
    deployment_id       INT             REFERENCES deployments(id) ON DELETE CASCADE,
    name                TEXT            NOT NULL,
    metric_name         TEXT            NOT NULL,
    threshold           FLOAT8          NOT NULL,
    comparator          TEXT            NOT NULL CHECK (comparator IN ('>', '<', '>=', '<=')),
    severity            TEXT            NOT NULL CHECK (severity IN ('warning', 'critical')),
    for_duration_secs   INT             NOT NULL DEFAULT 0,
    enabled             BOOL            NOT NULL DEFAULT true,
    silenced_until      TIMESTAMPTZ,

    -- Exactly one of service_id or deployment_id must be set.
    CONSTRAINT monitoring_alert_rules_single_target CHECK (
        (service_id IS NOT NULL)::int + (deployment_id IS NOT NULL)::int = 1
    )
);

CREATE INDEX IF NOT EXISTS idx_monitoring_alert_rules_service_id
    ON monitoring_alert_rules (service_id)
    WHERE service_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_monitoring_alert_rules_deployment_id
    ON monitoring_alert_rules (deployment_id)
    WHERE deployment_id IS NOT NULL;
"#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("DROP TABLE IF EXISTS monitoring_alert_rules CASCADE;")
            .await?;

        Ok(())
    }
}
