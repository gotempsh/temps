// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A configuration revision on metric alert rules (ADR 108 D1).
//!
//! The evaluator reads a rule, queries its series (which can take seconds
//! for a log count), then opens or resolves alarms and writes the rule's
//! state. An edit landing in between used to be overwritten by the older
//! verdict when it kept the same series, for example a raised threshold.
//! `config_revision` changes with every edit that changes what the rule
//! decides, and the evaluator writes its result only while the revision it
//! read is still current.
//!
//! A trigger bumps it, so every writer is covered: the API, the
//! `.temps.yaml` reconciler and anything added later. It ignores the
//! evaluator's own state columns, the name, the severity and the
//! notification grouping, none of which changes a verdict. The new value is
//! always derived from the old row, so no writer can move it backwards.
//!
//! Additive and defaulted: an older binary ignores the column.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE metric_alert_rules \
             ADD COLUMN IF NOT EXISTS config_revision BIGINT NOT NULL DEFAULT 0",
        )
        .await?;
        db.execute_unprepared(
            r#"
            CREATE OR REPLACE FUNCTION metric_alert_rules_bump_config_revision()
            RETURNS trigger AS $$
            BEGIN
                IF ROW(
                    NEW.environment_id, NEW.metric_name, NEW.aggregation,
                    NEW.detection_kind, NEW.detection_config::jsonb,
                    NEW.label_filters::jsonb, NEW.group_by::jsonb,
                    NEW.dynamic_alerts, NEW.max_series, NEW.window_secs,
                    NEW.for_duration_secs, NEW.enabled, NEW.source,
                    NEW.saved_search_id
                ) IS DISTINCT FROM ROW(
                    OLD.environment_id, OLD.metric_name, OLD.aggregation,
                    OLD.detection_kind, OLD.detection_config::jsonb,
                    OLD.label_filters::jsonb, OLD.group_by::jsonb,
                    OLD.dynamic_alerts, OLD.max_series, OLD.window_secs,
                    OLD.for_duration_secs, OLD.enabled, OLD.source,
                    OLD.saved_search_id
                ) THEN
                    NEW.config_revision := OLD.config_revision + 1;
                ELSE
                    NEW.config_revision := OLD.config_revision;
                END IF;
                RETURN NEW;
            END;
            $$ LANGUAGE plpgsql
            "#,
        )
        .await?;
        db.execute_unprepared(
            "DROP TRIGGER IF EXISTS metric_alert_rules_config_revision ON metric_alert_rules",
        )
        .await?;
        db.execute_unprepared(
            "CREATE TRIGGER metric_alert_rules_config_revision \
             BEFORE UPDATE ON metric_alert_rules \
             FOR EACH ROW EXECUTE FUNCTION metric_alert_rules_bump_config_revision()",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "DROP TRIGGER IF EXISTS metric_alert_rules_config_revision ON metric_alert_rules",
        )
        .await?;
        db.execute_unprepared("DROP FUNCTION IF EXISTS metric_alert_rules_bump_config_revision()")
            .await?;
        db.execute_unprepared(
            "ALTER TABLE metric_alert_rules DROP COLUMN IF EXISTS config_revision",
        )
        .await?;
        Ok(())
    }
}
