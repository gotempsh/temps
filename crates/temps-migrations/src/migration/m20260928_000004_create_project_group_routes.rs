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
        // service translates to a "not a member" error.
        //
        // The membership rows are read with `FOR SHARE`, not a plain SELECT.
        // Under READ COMMITTED a plain read cannot see a concurrent,
        // uncommitted membership delete or move, and that transaction's
        // cleanup trigger (below) cannot see this uncommitted route either:
        // both would commit and leave a route between ex-members that the
        // proxy keeps serving. Holding a share lock on the membership rows
        // until this transaction ends serializes the two:
        //
        //   * membership change first: the `FOR SHARE` waits for it to commit,
        //     re-evaluates `group_id = NEW.group_id` against the new row
        //     version (or finds the row gone) and raises the violation;
        //   * route insert first: the membership change waits for this
        //     transaction to commit, then its cleanup trigger runs a fresh
        //     statement that sees, and deletes, the committed route.
        //
        // Lock order. Every transaction that changes group membership (the
        // service layer in `temps-project-groups`, see `ProjectGroupService`)
        // and this trigger take locks in the same order:
        //
        //   1. `projects` rows of the affected projects, ascending id
        //      (membership writers: `FOR NO KEY UPDATE`; this trigger: `FOR
        //      SHARE`, so route inserts do not serialize with each other but
        //      do with every membership writer);
        //   2. the `project_groups` row (`FOR KEY SHARE` here and in
        //      placements, `FOR UPDATE` when the group is deleted);
        //   3. `project_group_members` rows, ascending `project_id`;
        //   4. `project_group_routes` rows.
        //
        // Step 1 is what lets a writer decide "which routes does this move
        // drop?" for a project that is in no group yet: there is no
        // membership row to lock for it, but there is always a project row,
        // so a concurrent "put it in a group and route to it" either
        // finishes before the writer reads or waits until the writer is done.
        // Steps 2-3 keep protecting writers that do not take step 1 (a plain
        // `DELETE FROM project_group_members`, the group's `ON DELETE
        // CASCADE`): the membership share lock below still serializes them
        // against this insert. Since all of them lock in one global order the
        // deadlock detector (SQLSTATE 40P01, an error, never a stale route)
        // is only a backstop.
        db.execute_unprepared(
            r#"
            CREATE OR REPLACE FUNCTION enforce_project_group_route_membership()
            RETURNS TRIGGER AS $$
            DECLARE
                member_id INTEGER;
                source_is_member BOOLEAN := FALSE;
                target_is_member BOOLEAN := FALSE;
            BEGIN
                -- Lock order steps 1 and 2: the endpoint projects, then the
                -- group, before any membership row (see above).
                PERFORM 1 FROM projects
                 WHERE id IN (NEW.source_project_id, NEW.target_project_id)
                 ORDER BY id
                   FOR SHARE;
                PERFORM 1 FROM project_groups
                 WHERE id = NEW.group_id
                   FOR KEY SHARE;
                FOR member_id IN
                    SELECT project_id FROM project_group_members
                     WHERE group_id = NEW.group_id
                       AND project_id IN (NEW.source_project_id, NEW.target_project_id)
                     ORDER BY project_id
                       FOR SHARE
                LOOP
                    IF member_id = NEW.source_project_id THEN
                        source_is_member := TRUE;
                    END IF;
                    IF member_id = NEW.target_project_id THEN
                        target_is_member := TRUE;
                    END IF;
                END LOOP;
                IF NOT source_is_member THEN
                    RAISE EXCEPTION 'source project % is not a member of group %',
                        NEW.source_project_id, NEW.group_id
                        USING ERRCODE = 'check_violation';
                END IF;
                IF NOT target_is_member THEN
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

        // A project that leaves its group takes its routes with it. This is
        // an AFTER trigger: by the time it runs the membership row is already
        // exclusively locked by the statement that changed it, which is what
        // makes the share lock in the route guard above conflict. Its DELETE
        // is a fresh READ COMMITTED statement, so it sees every route
        // committed by a transaction that held a share lock on this row.
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
