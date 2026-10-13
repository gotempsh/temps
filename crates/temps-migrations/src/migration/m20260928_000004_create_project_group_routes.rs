// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Path routes inside a project group (ADR 050): requests to a source
//! project's hostnames whose path starts with `path_prefix` are served by the
//! target project's matching environment, e.g. `frontend: /api → backend`.
//!
//! The proxy never reads this table per request: the route table resolves the
//! rules into its in-memory snapshot when it reloads. Every change notifies the
//! existing `project_route_change` channel so that reload happens at once.
//!
//! A route only makes sense while both projects belong to its group, so
//! leaving the group (or moving to another one) deletes the routes involving
//! that project in the database itself, whatever code path moved it.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS project_group_routes ( \
                 id SERIAL PRIMARY KEY, \
                 group_id INTEGER NOT NULL REFERENCES project_groups(id) ON DELETE CASCADE, \
                 source_project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE, \
                 target_project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE, \
                 path_prefix VARCHAR(255) NOT NULL, \
                 strip_prefix BOOLEAN NOT NULL DEFAULT FALSE, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 CONSTRAINT ck_project_group_routes_distinct \
                     CHECK (source_project_id <> target_project_id), \
                 CONSTRAINT ck_project_group_routes_prefix \
                     CHECK (path_prefix ~ '^(/[A-Za-z0-9._~-]+)+$' \
                            AND path_prefix !~ '/\\.\\.?(/|$)'), \
                 CONSTRAINT uq_project_group_routes_source_prefix \
                     UNIQUE (source_project_id, path_prefix) \
             )",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_project_group_routes_group \
                 ON project_group_routes (group_id)",
        )
        .await?;

        // Reload the proxy's route table on every change. The payload has the
        // shape of the project trigger's (`action`, `project_id`), which the
        // listener already understands.
        db.execute_unprepared(
            r#"
            CREATE OR REPLACE FUNCTION notify_project_group_route_change()
            RETURNS TRIGGER AS $$
            BEGIN
                PERFORM pg_notify('project_route_change', json_build_object(
                    'action', 'PATH_ROUTE_' || TG_OP,
                    'project_id', COALESCE(NEW.source_project_id, OLD.source_project_id)
                )::text);
                RETURN COALESCE(NEW, OLD);
            END;
            $$ LANGUAGE plpgsql;
            "#,
        )
        .await?;
        db.execute_unprepared(
            "DROP TRIGGER IF EXISTS project_group_routes_change_trigger ON project_group_routes",
        )
        .await?;
        db.execute_unprepared(
            "CREATE TRIGGER project_group_routes_change_trigger \
                 AFTER INSERT OR UPDATE OR DELETE ON project_group_routes \
                 FOR EACH ROW EXECUTE FUNCTION notify_project_group_route_change()",
        )
        .await?;

        // Guard: both source and target must be members of the route's group
        // before a route can be inserted or updated. This prevents stale
        // snapshots in the service layer from writing invalid routes.
        // The violation maps to a CHECK-like constraint error (23514) which the
        // handler translates to 409/400.
        db.execute_unprepared(
            r#"
            CREATE OR REPLACE FUNCTION enforce_project_group_route_membership()
            RETURNS TRIGGER AS $$
            BEGIN
                IF NOT EXISTS (
                    SELECT 1 FROM project_group_members
                     WHERE group_id = NEW.group_id
                       AND project_id = NEW.source_project_id
                ) THEN
                    RAISE EXCEPTION 'source project % is not a member of group %',
                        NEW.source_project_id, NEW.group_id
                        USING ERRCODE = 'check_violation';
                END IF;
                IF NOT EXISTS (
                    SELECT 1 FROM project_group_members
                     WHERE group_id = NEW.group_id
                       AND project_id = NEW.target_project_id
                ) THEN
                    RAISE EXCEPTION 'target project % is not a member of group %',
                        NEW.target_project_id, NEW.group_id
                        USING ERRCODE = 'check_violation';
                END IF;
                RETURN NEW;
            END;
            $$ LANGUAGE plpgsql;
            "#,
        )
        .await?;
        db.execute_unprepared(
            "DROP TRIGGER IF EXISTS project_group_routes_membership_check ON project_group_routes",
        )
        .await?;
        db.execute_unprepared(
            "CREATE TRIGGER project_group_routes_membership_check \
                 BEFORE INSERT OR UPDATE ON project_group_routes \
                 FOR EACH ROW EXECUTE FUNCTION enforce_project_group_route_membership()",
        )
        .await?;

        // A project that leaves its group takes its routes with it.
        db.execute_unprepared(
            r#"
            CREATE OR REPLACE FUNCTION drop_routes_of_departed_member()
            RETURNS TRIGGER AS $$
            BEGIN
                IF TG_OP = 'DELETE' OR NEW.group_id IS DISTINCT FROM OLD.group_id THEN
                    DELETE FROM project_group_routes
                     WHERE group_id = OLD.group_id
                       AND (source_project_id = OLD.project_id
                            OR target_project_id = OLD.project_id);
                END IF;
                RETURN COALESCE(NEW, OLD);
            END;
            $$ LANGUAGE plpgsql;
            "#,
        )
        .await?;
        db.execute_unprepared(
            "DROP TRIGGER IF EXISTS project_group_members_drop_routes ON project_group_members",
        )
        .await?;
        db.execute_unprepared(
            "CREATE TRIGGER project_group_members_drop_routes \
                 AFTER UPDATE OR DELETE ON project_group_members \
                 FOR EACH ROW EXECUTE FUNCTION drop_routes_of_departed_member()",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "DROP TRIGGER IF EXISTS project_group_members_drop_routes ON project_group_members",
        )
        .await?;
        db.execute_unprepared("DROP FUNCTION IF EXISTS drop_routes_of_departed_member()")
            .await?;
        db.execute_unprepared(
            "DROP TRIGGER IF EXISTS project_group_routes_membership_check ON project_group_routes",
        )
        .await?;
        db.execute_unprepared("DROP FUNCTION IF EXISTS enforce_project_group_route_membership()")
            .await?;
        db.execute_unprepared(
            "DROP TRIGGER IF EXISTS project_group_routes_change_trigger ON project_group_routes",
        )
        .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS project_group_routes")
            .await?;
        db.execute_unprepared("DROP FUNCTION IF EXISTS notify_project_group_route_change()")
            .await?;
        Ok(())
    }
}
