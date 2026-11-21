// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cookieless tracking (ADR 109 D3).
//!
//! - `projects.analytics_identity_mode`: `persistent` (today's cookies and
//!   `localStorage` ids, the default for every existing and new project) or
//!   `cookieless`. A metadata-only `ADD COLUMN` with a constant default, so it
//!   rewrites no rows on PostgreSQL 11+.
//! - The project route-change trigger also fires when the mode changes, so
//!   the proxy stops (or starts) setting cookies on the next route reload
//!   instead of whenever something else happens to reload routes.
//! - `analytics_identity_salts`: the daily salt cookieless ids are derived
//!   from, one row per UTC day, encrypted at rest.
//! - `analytics_cookieless_sessions`: the session each cookieless visitor
//!   started most recently, reserved atomically so two ingest nodes that see a
//!   visitor's first events before either is stored join one session. A row is
//!   useless once it is older than the 30-minute session window, and the hourly
//!   salt sweep deletes it. No foreign key: project deletion does not cascade
//!   through telemetry, and the sweep removes a deleted project's rows.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// `notify_project_route_change()` as m20261023_000002 left it, plus
/// `extra_update_condition`.
fn notify_function_sql(extra_update_condition: &str) -> String {
    format!(
        r#"
        CREATE OR REPLACE FUNCTION notify_project_route_change()
        RETURNS TRIGGER AS $$
        BEGIN
            IF TG_OP IN ('INSERT', 'DELETE') THEN
                PERFORM pg_notify('project_route_change', json_build_object(
                    'action', TG_OP,
                    'project_id', COALESCE(NEW.id, OLD.id),
                    'field', 'project'
                )::text);
                RETURN COALESCE(NEW, OLD);
            END IF;

            IF TG_OP = 'UPDATE' THEN
                IF (OLD.is_deleted IS DISTINCT FROM NEW.is_deleted)
                   OR (OLD.slug IS DISTINCT FROM NEW.slug)
                   OR (OLD.deployment_config IS DISTINCT FROM NEW.deployment_config)
                   OR (OLD.serves_own_temps_api IS DISTINCT FROM NEW.serves_own_temps_api){extra_update_condition}
                THEN
                    PERFORM pg_notify('project_route_change', json_build_object(
                        'action', 'UPDATE',
                        'project_id', NEW.id,
                        'is_deleted', NEW.is_deleted,
                        'slug', NEW.slug,
                        'deployment_config_changed', (OLD.deployment_config IS DISTINCT FROM NEW.deployment_config),
                        'timestamp', CURRENT_TIMESTAMP
                    )::text);
                END IF;
                RETURN NEW;
            END IF;

            RETURN COALESCE(NEW, OLD);
        END;
        $$ LANGUAGE plpgsql;
        "#
    )
}

const IDENTITY_MODE_CHANGED: &str = "\n                   OR (OLD.analytics_identity_mode IS DISTINCT FROM NEW.analytics_identity_mode)";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"
ALTER TABLE projects
    ADD COLUMN IF NOT EXISTS analytics_identity_mode TEXT NOT NULL DEFAULT 'persistent';

ALTER TABLE projects DROP CONSTRAINT IF EXISTS ck_projects_analytics_identity_mode;
ALTER TABLE projects ADD CONSTRAINT ck_projects_analytics_identity_mode
    CHECK (analytics_identity_mode IN ('persistent', 'cookieless'));

CREATE TABLE IF NOT EXISTS analytics_identity_salts (
    day DATE PRIMARY KEY,
    salt TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS analytics_cookieless_sessions (
    project_id INTEGER NOT NULL,
    visitor_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (project_id, visitor_id)
);
CREATE INDEX IF NOT EXISTS idx_analytics_cookieless_sessions_started_at
    ON analytics_cookieless_sessions (started_at);
"#,
        )
        .await?;
        db.execute_unprepared(&notify_function_sql(IDENTITY_MODE_CHANGED))
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(&notify_function_sql("")).await?;
        db.execute_unprepared(
            r#"
DROP TABLE IF EXISTS analytics_cookieless_sessions;
DROP TABLE IF EXISTS analytics_identity_salts;
ALTER TABLE projects DROP CONSTRAINT IF EXISTS ck_projects_analytics_identity_mode;
ALTER TABLE projects DROP COLUMN IF EXISTS analytics_identity_mode;
"#,
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mode_notifies_a_route_reload_only_after_up() {
        let up = notify_function_sql(IDENTITY_MODE_CHANGED);
        assert!(up.contains(
            "OLD.analytics_identity_mode IS DISTINCT FROM NEW.analytics_identity_mode"
        ));
        // Everything m20261023_000002 notified on still notifies.
        for column in [
            "is_deleted",
            "slug",
            "deployment_config",
            "serves_own_temps_api",
        ] {
            assert!(up.contains(&format!("OLD.{column} IS DISTINCT FROM NEW.{column}")));
            assert!(notify_function_sql("")
                .contains(&format!("OLD.{column} IS DISTINCT FROM NEW.{column}")));
        }
        assert!(!notify_function_sql("").contains("analytics_identity_mode"));
    }
}
