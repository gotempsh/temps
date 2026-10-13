// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project groups: an optional parent that gathers several projects (e.g. a
//! frontend, an API and a worker) into one unit.
//!
//! Membership lives in its own table instead of a `projects.group_id` column
//! so the grouping stays purely additive: an ungrouped project has no row and
//! behaves exactly as before, and the whole feature can be removed by dropping
//! these two tables. `project_id` is the primary key of the membership table,
//! so a project belongs to at most one group.
//!
//! Deleting a group deletes only the membership rows (the projects become
//! ungrouped); deleting a project removes its membership row.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS project_groups (\
                 id SERIAL PRIMARY KEY, \
                 name VARCHAR(100) NOT NULL, \
                 slug VARCHAR(100) NOT NULL UNIQUE, \
                 description TEXT, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()\
             )",
        )
        .await?;
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS project_group_members (\
                 project_id INTEGER PRIMARY KEY \
                     REFERENCES projects(id) ON DELETE CASCADE, \
                 group_id INTEGER NOT NULL \
                     REFERENCES project_groups(id) ON DELETE CASCADE, \
                 position INTEGER NOT NULL DEFAULT 0, \
                 added_at TIMESTAMPTZ NOT NULL DEFAULT NOW()\
             )",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_project_group_members_group \
             ON project_group_members (group_id, position)",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP TABLE IF EXISTS project_group_members")
            .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS project_groups")
            .await?;
        Ok(())
    }
}
