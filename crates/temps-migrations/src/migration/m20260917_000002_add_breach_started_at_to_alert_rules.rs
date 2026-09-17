// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Persist when a metric alert rule first entered breach.
//!
//! `AlertEvaluator` fires an alarm only once a rule has been breaching
//! continuously for `for_duration_secs`. That elapsed time was tracked in an
//! in-process `HashMap`, so every restart — including every routine upgrade or
//! redeploy of the control plane — reset the clock to zero. A rule with a
//! 15-minute `for_duration_secs` on an instance that restarts more often than
//! that could never fire at all, which is exactly backwards: the alarm is
//! least likely to arrive on the instance that is least stable.
//!
//! `monitoring_alert_rules` already carries a CHECK constraint that exactly
//! one of `service_id` / `deployment_id` / `node_id` is set, so a rule maps to
//! exactly one target and the breach clock is 1:1 with the rule row. That
//! makes a column the right shape here — a side table keyed by
//! `(rule_id, target)` would only ever hold one row per rule and would need
//! its own cascade delete.
//!
//! NULL means "not currently breaching". The evaluator writes the timestamp on
//! the transition into breach and clears it on recovery, so the column is
//! self-correcting and needs no backfill.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE monitoring_alert_rules \
                 ADD COLUMN IF NOT EXISTS breach_started_at TIMESTAMPTZ NULL",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE monitoring_alert_rules DROP COLUMN IF EXISTS breach_started_at",
            )
            .await?;
        Ok(())
    }
}
