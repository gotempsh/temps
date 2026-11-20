// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Saved searches over error groups (ADR 111, D1.8).
//!
//! A saved search is a named set of list filters owned by one user in one
//! project, private unless shared with everyone who can open the project.
//! The filters are columns, not a JSON blob, so a search that names a
//! deleted environment loses that filter instead of pointing at nothing.
//! Names are unique per owner and project, ignoring case.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS error_saved_searches ( \
                 id SERIAL PRIMARY KEY, \
                 project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE, \
                 owner_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, \
                 name TEXT NOT NULL, \
                 shared BOOLEAN NOT NULL DEFAULT FALSE, \
                 status TEXT, \
                 environment_id INTEGER REFERENCES environments(id) ON DELETE SET NULL, \
                 assignee TEXT, \
                 search TEXT, \
                 sort_by TEXT, \
                 sort_order TEXT NOT NULL DEFAULT 'desc', \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW())",
        )
        .await?;
        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_error_saved_searches_owner_name \
             ON error_saved_searches (project_id, owner_user_id, lower(name))",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_error_saved_searches_project \
             ON error_saved_searches (project_id, created_at DESC, id DESC)",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS error_saved_searches")
            .await?;
        Ok(())
    }
}
