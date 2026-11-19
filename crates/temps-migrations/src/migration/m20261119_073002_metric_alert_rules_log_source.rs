// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Log alerts as metric alert rules (ADR 073 D1).
//!
//! A log alert reuses the whole metric alert rule machinery (detectors,
//! evaluator state machine, alarm log, notification routes); only the
//! series differs: instead of an OTel metric it is the number of log lines
//! matching a saved search per evaluation window. Two columns carry that:
//!
//! - `source`: `metric` (every existing row, the default) or `logs`.
//! - `saved_search_id`: the `log_saved_searches` row a `logs` rule counts.
//!   `ON DELETE CASCADE`: a log rule means nothing without its search.
//!   The saved-search service refuses to delete a search while a rule
//!   counts it (a 409 naming the alert count), so the cascade only acts
//!   when the project is deleted. `metric_alert_rules` has no foreign key
//!   to `projects`, so a blocking key here (`RESTRICT` or `NO ACTION`) would
//!   make deleting a project with a log alert fail.
//!
//! Both are additive and defaulted, so an older binary keeps reading the
//! table unchanged (it evaluates a `logs` rule as a metric rule with no
//! data, which preserves state and never fires).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE metric_alert_rules \
             ADD COLUMN IF NOT EXISTS source TEXT NOT NULL DEFAULT 'metric'",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE metric_alert_rules \
             ADD COLUMN IF NOT EXISTS saved_search_id INTEGER \
             REFERENCES log_saved_searches(id) ON DELETE CASCADE",
        )
        .await?;
        // Small configuration table: a plain index is fine here.
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_metric_alert_rules_saved_search \
             ON metric_alert_rules(saved_search_id) WHERE saved_search_id IS NOT NULL",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP INDEX IF EXISTS idx_metric_alert_rules_saved_search")
            .await?;
        db.execute_unprepared(
            "ALTER TABLE metric_alert_rules DROP COLUMN IF EXISTS saved_search_id",
        )
        .await?;
        db.execute_unprepared("ALTER TABLE metric_alert_rules DROP COLUMN IF EXISTS source")
            .await?;
        Ok(())
    }
}
