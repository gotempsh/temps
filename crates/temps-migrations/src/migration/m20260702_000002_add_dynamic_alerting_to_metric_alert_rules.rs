//! Add per-series ("dynamic") alerting columns to `metric_alert_rules`.
//!
//! Phase 3 of ADR-026:
//! - `group_by` (jsonb, default `[]`): label keys to break the metric down by,
//!   e.g. `["endpoint","region"]`. Empty (the default) = one aggregate stream,
//!   today's behaviour, so every existing row round-trips unchanged.
//! - `dynamic_alerts` (bool, default `false`): when true, the evaluator runs one
//!   state machine per distinct series (keyed by `(rule_id, series_key)`) and
//!   fires an independent alarm per breaching series. When false (the default),
//!   a set `group_by` still collapses to a single "alert if any series breaches"
//!   aggregate — so the column is purely additive.
//! - `max_series` (int, default 20): the cardinality cap — at most this many
//!   series (top by `|value|`) are evaluated/tracked per tick. Hard-capped at 100
//!   by the service layer.
//!
//! All three are NOT NULL with column defaults so no migration-time UPDATE is
//! needed and existing rows keep exactly their current (aggregate) behaviour.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE metric_alert_rules \
                 ADD COLUMN IF NOT EXISTS group_by jsonb NOT NULL DEFAULT '[]'::jsonb, \
                 ADD COLUMN IF NOT EXISTS dynamic_alerts boolean NOT NULL DEFAULT false, \
                 ADD COLUMN IF NOT EXISTS max_series integer NOT NULL DEFAULT 20;",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE metric_alert_rules \
                 DROP COLUMN IF EXISTS group_by, \
                 DROP COLUMN IF EXISTS dynamic_alerts, \
                 DROP COLUMN IF EXISTS max_series;",
            )
            .await?;
        Ok(())
    }
}
