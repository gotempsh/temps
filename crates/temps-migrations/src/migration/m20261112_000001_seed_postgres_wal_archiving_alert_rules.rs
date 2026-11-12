// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Install the two WAL archiving alert rules on PostgreSQL services that
//! already had metrics enabled before they became defaults.
//!
//! `seed_default_rules` only runs when metrics are turned on for a service
//! (at creation, or from the metrics toggle), so an existing service would
//! never get "WAL archiving failing" or "WAL backlog" without this. It runs
//! once: a rule an operator deletes afterwards stays deleted.
//!
//! Same values as `temps_alarms::default_rules::postgres_default_seeds`, the
//! same `ON CONFLICT (service_id, metric_name) DO NOTHING` (a rule the
//! operator already made for either metric is kept as it is), and the same
//! per-service budget of 100 rules: a service without room for both is left
//! alone rather than given half the set.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Rules per service the seeding path allows
/// (`temps_alarms::MAX_ALERT_RULES_PER_SERVICE`).
const MAX_ALERT_RULES_PER_SERVICE: i64 = 100;

pub(crate) const SEED_SQL: &str = r#"
INSERT INTO monitoring_alert_rules
    (service_id, deployment_id, name, metric_name, threshold, comparator, severity, for_duration_secs, enabled)
SELECT s.id, NULL, v.name, v.metric_name, v.threshold, '>', 'critical', v.for_duration_secs, true
FROM external_services s
CROSS JOIN (VALUES
    ('WAL archiving failing', 'pg.archiver_failing', 0.5::float8, 600),
    ('WAL backlog: pg_wal above 4x max_wal_size', 'pg.wal_dir_to_max_wal_size_ratio', 4.0::float8, 900)
) AS v(name, metric_name, threshold, for_duration_secs)
WHERE lower(s.service_type) = 'postgres'
  AND s.metrics_enabled = true
  AND (SELECT count(*) FROM monitoring_alert_rules r WHERE r.service_id = s.id) <= $1
ON CONFLICT (service_id, metric_name) WHERE service_id IS NOT NULL DO NOTHING
"#;

pub(crate) const UNSEED_SQL: &str = r#"
DELETE FROM monitoring_alert_rules
WHERE service_id IS NOT NULL
  AND deployment_id IS NULL
  AND ((name = 'WAL archiving failing' AND metric_name = 'pg.archiver_failing'
        AND threshold = 0.5 AND comparator = '>' AND for_duration_secs = 600)
    OR (name = 'WAL backlog: pg_wal above 4x max_wal_size'
        AND metric_name = 'pg.wal_dir_to_max_wal_size_ratio'
        AND threshold = 4.0 AND comparator = '>' AND for_duration_secs = 900))
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute(sea_orm::Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                SEED_SQL,
                [(MAX_ALERT_RULES_PER_SERVICE - 2).into()],
            ))
            .await?;
        Ok(())
    }

    /// Remove the rules this migration added and the operator left as they
    /// were; an edited rule is the operator's and stays.
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(UNSEED_SQL)
            .await?;
        Ok(())
    }
}
