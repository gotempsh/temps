//! Follow-up columns closing three ADR-026 Phase 3 gaps on `metric_alert_rules`.
//!
//! - `grouped_notification_threshold` (int, default 5): promotes the previously
//!   hardcoded notification-grouping threshold to a per-rule column. When more
//!   than this many series transition to firing in the same tick, only the first
//!   gets the expensive chart/AI enrichment. The default of 5 preserves the exact
//!   prior (constant) behaviour, so every existing row round-trips unchanged.
//! - `series_states` (jsonb, default `{}`): the full per-series state snapshot,
//!   persisted after every dynamic-rule tick, keyed by the human-readable series
//!   label (`{"method=GET": {"state":"firing","value":12.5,"alarm_id":259}}`).
//!   Supersedes the lossy `last_state`/`last_value` aggregate for external
//!   consumers that only read the row (not the live per-series API). Empty `{}`
//!   for static/aggregate rules, which never populate it.
//! - `last_dropped_series_count` (int, default 0): the number of series dropped by
//!   the cardinality cap on the LATEST dynamic tick (was a silent server-side
//!   `warn!`). 0 when nothing was dropped or for static rules.
//!
//! All three are NOT NULL with column defaults so no migration-time UPDATE is
//! needed and existing rows keep exactly their current behaviour.

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
                 ADD COLUMN IF NOT EXISTS grouped_notification_threshold integer NOT NULL DEFAULT 5, \
                 ADD COLUMN IF NOT EXISTS series_states jsonb NOT NULL DEFAULT '{}'::jsonb, \
                 ADD COLUMN IF NOT EXISTS last_dropped_series_count integer NOT NULL DEFAULT 0;",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE metric_alert_rules \
                 DROP COLUMN IF EXISTS grouped_notification_threshold, \
                 DROP COLUMN IF EXISTS series_states, \
                 DROP COLUMN IF EXISTS last_dropped_series_count;",
            )
            .await?;
        Ok(())
    }
}
