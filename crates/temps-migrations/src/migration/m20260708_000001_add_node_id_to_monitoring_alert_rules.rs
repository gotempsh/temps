use sea_orm_migration::prelude::*;

/// Adds `node_id` as a third alert-rule target so rules can watch node-scoped
/// metrics (`SourceKind::Node`), e.g. the proxy hot-path metrics
/// (`proxy.error_rate_percent`, `proxy.request_duration_p99_ms`) written by
/// the proxy metrics sampler for the control plane.
///
/// `node_id` has **no FK** on purpose: the control plane uses the synthetic
/// node ID `0` (see `CONTROL_PLANE_NODE_ID` in temps-deployments), which has
/// no row in `nodes`.
///
/// The `single_target` CHECK constraint is widened from
/// `(service_id XOR deployment_id)` to "exactly one of service_id,
/// deployment_id, node_id".
///
/// **Safely re-runnable:** `IF NOT EXISTS` / drop-then-add constraint.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
ALTER TABLE monitoring_alert_rules
    ADD COLUMN IF NOT EXISTS node_id INT;

ALTER TABLE monitoring_alert_rules
    DROP CONSTRAINT IF EXISTS monitoring_alert_rules_single_target;

ALTER TABLE monitoring_alert_rules
    ADD CONSTRAINT monitoring_alert_rules_single_target CHECK (
        (service_id IS NOT NULL)::int
        + (deployment_id IS NOT NULL)::int
        + (node_id IS NOT NULL)::int = 1
    );

CREATE INDEX IF NOT EXISTS idx_monitoring_alert_rules_node_id
    ON monitoring_alert_rules (node_id)
    WHERE node_id IS NOT NULL;

-- Mirrors uidx_monitoring_alert_rules_{service,deployment}_metric so node
-- rule seeding can use ON CONFLICT DO NOTHING.
CREATE UNIQUE INDEX IF NOT EXISTS uidx_monitoring_alert_rules_node_metric
    ON monitoring_alert_rules (node_id, metric_name)
    WHERE node_id IS NOT NULL;
"#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
DELETE FROM monitoring_alert_rules WHERE node_id IS NOT NULL;

ALTER TABLE monitoring_alert_rules
    DROP CONSTRAINT IF EXISTS monitoring_alert_rules_single_target;

ALTER TABLE monitoring_alert_rules
    ADD CONSTRAINT monitoring_alert_rules_single_target CHECK (
        (service_id IS NOT NULL)::int + (deployment_id IS NOT NULL)::int = 1
    );

DROP INDEX IF EXISTS idx_monitoring_alert_rules_node_id;
DROP INDEX IF EXISTS uidx_monitoring_alert_rules_node_metric;

ALTER TABLE monitoring_alert_rules
    DROP COLUMN IF EXISTS node_id;
"#,
        )
        .await?;

        Ok(())
    }
}
