// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! An error group's assignee is a user or a team, and no longer a status
//! (ADR 074, D1.6).
//!
//! `assignee_user_id` and `assignee_team_id` reference the assignee; at most
//! one is set. Existing free-text `assigned_to` values are matched to a user
//! by email, case-insensitively; a value that matches no user is left in
//! `assigned_to`, which is kept read-only so nothing typed by hand is lost.
//!
//! Groups in the old `assigned` status move to `unresolved`: an assigned
//! group was an open issue someone was working on, and `unresolved` is the
//! open state. Their assignee is kept through the columns above.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE error_groups \
             ADD COLUMN IF NOT EXISTS assignee_user_id INTEGER \
                 REFERENCES users(id) ON DELETE SET NULL, \
             ADD COLUMN IF NOT EXISTS assignee_team_id INTEGER \
                 REFERENCES teams(id) ON DELETE SET NULL",
        )
        .await?;
        db.execute_unprepared(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'error_groups_one_assignee') THEN \
                 ALTER TABLE error_groups ADD CONSTRAINT error_groups_one_assignee \
                   CHECK (assignee_user_id IS NULL OR assignee_team_id IS NULL); \
               END IF; \
             END $$",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_error_groups_assignee_user \
               ON error_groups (project_id, assignee_user_id) WHERE assignee_user_id IS NOT NULL",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_error_groups_assignee_team \
               ON error_groups (project_id, assignee_team_id) WHERE assignee_team_id IS NOT NULL",
        )
        .await?;
        db.execute_unprepared(
            "UPDATE error_groups g SET assignee_user_id = ( \
                 SELECT u.id FROM users u \
                 WHERE lower(u.email) = lower(btrim(g.assigned_to)) AND u.deleted_at IS NULL \
                 ORDER BY u.id LIMIT 1) \
             WHERE g.assigned_to IS NOT NULL AND btrim(g.assigned_to) <> '' \
               AND g.assignee_user_id IS NULL AND g.assignee_team_id IS NULL",
        )
        .await?;
        db.execute_unprepared(
            "UPDATE error_groups SET assigned_to = NULL WHERE assignee_user_id IS NOT NULL",
        )
        .await?;
        db.execute_unprepared(
            "UPDATE error_groups SET status = 'unresolved', updated_at = NOW() \
             WHERE status = 'assigned'",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        // Put matched users back as text. The old `assigned` status is not
        // restored: which groups had it is not recorded.
        db.execute_unprepared(
            "UPDATE error_groups g SET assigned_to = u.email FROM users u \
             WHERE g.assignee_user_id = u.id AND g.assigned_to IS NULL",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE error_groups \
             DROP CONSTRAINT IF EXISTS error_groups_one_assignee, \
             DROP COLUMN IF EXISTS assignee_user_id, \
             DROP COLUMN IF EXISTS assignee_team_id",
        )
        .await?;
        Ok(())
    }
}
