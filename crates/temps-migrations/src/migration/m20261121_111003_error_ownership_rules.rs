// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Ownership rules for error groups (ADR 111, D5).
//!
//! Each rule is a repository path pattern and the user who owns the code
//! under it. When an event creates a group, the first rule (by `position`)
//! matching one of its application frames assigns the group. A rule goes
//! with its project and with its owner.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS error_ownership_rules ( \
                 id SERIAL PRIMARY KEY, \
                 project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE, \
                 position INTEGER NOT NULL CHECK (position >= 0), \
                 pattern TEXT NOT NULL, \
                 owner_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                 UNIQUE (project_id, position))",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS error_ownership_rules")
            .await?;
        Ok(())
    }
}
