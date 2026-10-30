// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Take the high-volume telemetry tables out of the project delete cascade.
//!
//! `DELETE FROM projects` used to fire foreign keys from `proxy_logs`,
//! `events`, `error_events`, `request_logs` and `performance_metrics` into
//! `projects`, `environments`, `deployments` and `error_groups`. Postgres runs
//! one query per deleted parent row, and none of those queries could use an
//! index (`WHERE deployment_id = $1` without `project_id`), so deleting a
//! project scanned every request and analytics row on the instance once per
//! deployment and environment. On compressed chunks the same queries had to
//! decompress, and past TimescaleDB's default
//! `max_tuples_decompressed_per_dml_transaction` (100 000) the delete failed
//! outright. `proxy_logs.project_id` was `SET NULL`, so the project's request
//! log was rewritten rather than removed.
//!
//! After this migration:
//!
//! - those foreign keys are gone. A deleted project's telemetry rows keep its
//!   id and are unreachable (every reader is scoped to a live project, and
//!   project ids are never reused) until the purger removes them;
//! - `project_data_purges` records every deleted project whose telemetry is
//!   still on disk. The project delete inserts its row in the same
//!   transaction, so a project can't disappear without leaving one;
//! - every remaining foreign key under `projects` that lacked an index with
//!   the key as its leading column gets one ([`FK_INDEXES`]), so the
//!   control-plane part of the cascade is an index lookup per parent row
//!   instead of a scan.
//!
//! Dropping a constraint on a hypertable is a catalog change propagated to
//! its chunks: no rewrite, regardless of table size.
//!
//! The indexes are **not built here**. SeaORM runs a migration inside a
//! transaction, where only a plain `CREATE INDEX` is legal, and that blocks
//! writes for the whole build. Some of these tables take a steady stream of
//! writes and grow with history (`performance_metrics`,
//! `telemetry_client_outcomes`, `runtime_health_buckets`, `cron_monitor_runs`),
//! so `temps_database::run_post_migration_indexes` builds them after the
//! server has bound, with `CREATE INDEX CONCURRENTLY`. Until then a project
//! delete cascades the way it did before this migration.
//!
//! Every name is resolved through `search_path`, never a fixed schema, so the
//! migration also runs in the per-test schemas of `TestDatabase`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Telemetry tables whose rows are purged by `project_id` in the background.
pub const PURGED_TABLES: [&str; 5] = [
    "proxy_logs",
    "events",
    "error_events",
    "request_logs",
    "performance_metrics",
];

/// Parents the telemetry tables no longer hold foreign keys to.
const DETACHED_PARENTS: [&str; 4] = ["projects", "environments", "deployments", "error_groups"];

/// `(table, column)` foreign keys under `projects` that had no index leading
/// with the column, plus the `project_id` lookups the purger needs on the two
/// plain telemetry tables (the hypertables already lead with `project_id`).
pub const FK_INDEXES: [(&str, &str); 55] = [
    ("alarms", "container_id"),
    ("analytics_ingest_keys", "environment_id"),
    ("ci_builds", "token_id"),
    ("cron_monitor_runs", "deployment_id"),
    ("cron_monitors", "alarm_id"),
    ("cron_monitors", "environment_id"),
    ("crons", "environment_id"),
    ("crons", "project_id"),
    ("cross_project_trace_refs", "project_id"),
    ("deployment_containers", "deployment_id"),
    ("deployment_domains", "deployment_id"),
    ("deployments", "environment_id"),
    ("env_var_environments", "env_var_id"),
    ("env_var_environments", "environment_id"),
    ("env_var_history", "env_var_id"),
    ("env_vars", "project_id"),
    ("environment_domains", "environment_id"),
    ("environments", "current_deployment_id"),
    ("environments", "project_id"),
    ("error_alert_fires", "error_group_id"),
    ("error_event_source_keys", "error_group_id"),
    ("error_groups", "deployment_id"),
    ("error_groups", "environment_id"),
    ("funnel_steps", "funnel_id"),
    ("funnels", "project_id"),
    ("ingress_endpoints", "environment_id"),
    ("metric_alert_rules", "environment_id"),
    ("project_custom_domains", "environment_id"),
    ("project_custom_domains", "project_id"),
    ("project_dsns", "deployment_id"),
    ("project_dsns", "environment_id"),
    ("project_feedback", "deployment_id"),
    ("project_feedback", "environment_id"),
    ("project_group_routes", "target_project_id"),
    ("project_ip_access_rules", "environment_id"),
    ("project_services", "project_id"),
    ("reachability_plans", "environment_id"),
    ("reachability_plans", "project_id"),
    ("reachability_runs", "project_id"),
    ("runtime_health_buckets", "revision_id"),
    ("runtime_health_environments", "revision_id"),
    ("runtime_health_revisions", "project_id"),
    ("sandbox_services", "project_id"),
    ("secret_environments", "environment_id"),
    ("secret_history", "secret_id"),
    ("secrets", "environment_id"),
    ("shield_operator_entries", "project_id"),
    ("status_incidents", "monitor_id"),
    ("tailnet_serve_refs", "run_id"),
    ("telemetry_client_outcome_receipts", "deployment_id"),
    ("telemetry_client_outcome_receipts", "environment_id"),
    ("telemetry_client_outcomes", "deployment_id"),
    ("telemetry_client_outcomes", "environment_id"),
    ("request_logs", "project_id"),
    ("performance_metrics", "project_id"),
];

/// Name of the index built for `(table, column)`. The `_fk` suffix keeps it
/// apart from every existing index name, so `down` only ever drops what the
/// post-migration build created.
pub fn fk_index_name(table: &str, column: &str) -> String {
    format!("idx_{table}_{column}_fk")
}

fn sql_array(items: impl IntoIterator<Item = String>) -> String {
    let quoted: Vec<String> = items.into_iter().map(|item| format!("'{item}'")).collect();
    format!("ARRAY[{}]::text[]", quoted.join(", "))
}

fn drop_telemetry_foreign_keys_sql() -> String {
    let children = sql_array(PURGED_TABLES.iter().map(|t| String::from(*t)));
    let parents = sql_array(DETACHED_PARENTS.iter().map(|t| String::from(*t)));
    format!(
        r#"
DO $$
DECLARE
    fk record;
BEGIN
    FOR fk IN
        SELECT c.conname, c.conrelid::regclass AS child
        FROM pg_constraint c
        WHERE c.contype = 'f'
          AND c.conrelid IN (SELECT to_regclass(t) FROM unnest({children}) AS t)
          AND c.confrelid IN (SELECT to_regclass(t) FROM unnest({parents}) AS t)
    LOOP
        EXECUTE format('ALTER TABLE %s DROP CONSTRAINT %I', fk.child, fk.conname);
    END LOOP;
END $$;
"#
    )
}

const CREATE_PURGES_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS project_data_purges (
    -- No foreign key: the project row is already gone.
    project_id INTEGER PRIMARY KEY,
    project_name TEXT NOT NULL,
    requested_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- When a worker may next pick the purge up: pushed back by the settle
    -- window and by retry backoff.
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- A worker holds a purge until this time, renewed as it goes, so two
    -- control planes never purge the same project at once.
    lease_until TIMESTAMPTZ,
    passes INTEGER NOT NULL DEFAULT 0,
    consecutive_failures INTEGER NOT NULL DEFAULT 0,
    rows_deleted BIGINT NOT NULL DEFAULT 0,
    last_error TEXT,
    completed_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS project_data_purges_pending
    ON project_data_purges (next_attempt_at)
    WHERE completed_at IS NULL;
"#;

const DROP_PURGES_SQL: &str = r#"
DROP INDEX IF EXISTS project_data_purges_pending;
DROP TABLE IF EXISTS project_data_purges;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(&drop_telemetry_foreign_keys_sql())
            .await?;
        db.execute_unprepared(CREATE_PURGES_SQL).await?;
        Ok(())
    }

    /// Drops the purge queue and the indexes built for [`FK_INDEXES`]. The telemetry
    /// foreign keys are deliberately not restored: rows of projects deleted
    /// since `up` reference parents that no longer exist, so re-adding the
    /// constraints would fail, or would need those rows deleted first, which a
    /// rollback must not do silently.
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(DROP_PURGES_SQL).await?;
        let drops: String = FK_INDEXES
            .iter()
            .map(|(table, column)| {
                format!("DROP INDEX IF EXISTS {};\n", fk_index_name(table, column))
            })
            .collect();
        db.execute_unprepared(&drops).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn index_names_fit_postgres_identifier_limit() {
        for (table, column) in FK_INDEXES {
            let name = fk_index_name(table, column);
            assert!(
                name.len() <= 63,
                "index name {name} exceeds 63 bytes and would be truncated by Postgres"
            );
        }
    }

    #[test]
    fn index_targets_are_unique() {
        let unique: HashSet<_> = FK_INDEXES.iter().collect();
        assert_eq!(
            unique.len(),
            FK_INDEXES.len(),
            "duplicate (table, column) entry"
        );
    }

    #[test]
    fn index_names_carry_the_fk_suffix() {
        assert_eq!(
            fk_index_name("crons", "project_id"),
            "idx_crons_project_id_fk"
        );
    }

    #[test]
    fn foreign_key_drop_covers_every_purged_table_and_parent() {
        let sql = drop_telemetry_foreign_keys_sql();
        for table in PURGED_TABLES.iter().chain(DETACHED_PARENTS.iter()) {
            assert!(
                sql.contains(&format!("'{table}'")),
                "{table} missing from FK drop"
            );
        }
    }
}
