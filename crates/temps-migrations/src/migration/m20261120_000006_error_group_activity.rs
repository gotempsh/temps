// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Comments and an activity log on error groups (ADR 111, D1.7).
//!
//! One row per thing that happened to a group: a comment, a status change by
//! a person, a regression reopened by an event, an assignment, a merge, an
//! unmerge or a split. `details` holds the typed specifics of each kind as
//! JSON. Rows are written when the group changes, never per event, so the
//! table grows with triage activity, not with event volume. They go with the
//! group when it is deleted.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS error_group_activity ( \
                 id BIGSERIAL PRIMARY KEY, \
                 project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE, \
                 error_group_id INTEGER NOT NULL REFERENCES error_groups(id) ON DELETE CASCADE, \
                 kind TEXT NOT NULL, \
                 actor_user_id INTEGER REFERENCES users(id) ON DELETE SET NULL, \
                 body TEXT, \
                 details JSONB, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                 edited_at TIMESTAMPTZ)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_error_group_activity_group \
             ON error_group_activity (error_group_id, created_at DESC, id DESC)",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS error_group_activity")
            .await?;
        Ok(())
    }
}
