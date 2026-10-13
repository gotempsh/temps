// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Template instantiations (ADR-051): creating a template, or importing apps
//! from a project's repository, as one server-side operation.
//!
//! Creating an app takes several steps (a project, a fork, databases, one
//! service per app, memberships, shared variables, routes, first
//! deployments). Each step's result is recorded in `plan` before the next
//! one starts, so a failed or interrupted instantiation resumes where it
//! stopped instead of creating duplicates. `(user_id, idempotency_key)` makes
//! a repeated request return the existing instantiation.
//!
//! `secrets` holds the variable values the remaining steps still need,
//! encrypted; it is cleared once the instantiation succeeds.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS template_instantiations (\
                 id SERIAL PRIMARY KEY, \
                 user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, \
                 idempotency_key VARCHAR(128) NOT NULL, \
                 kind VARCHAR(16) NOT NULL CHECK (kind IN ('template', 'import')), \
                 template_slug VARCHAR(255), \
                 name VARCHAR(255) NOT NULL, \
                 group_id INTEGER REFERENCES project_groups(id) ON DELETE SET NULL, \
                 status VARCHAR(16) NOT NULL DEFAULT 'pending' \
                     CHECK (status IN ('pending', 'running', 'succeeded', 'failed')), \
                 plan JSONB NOT NULL, \
                 secrets TEXT, \
                 error TEXT, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                 finished_at TIMESTAMPTZ, \
                 claimed_at TIMESTAMPTZ, \
                 heartbeat_at TIMESTAMPTZ, \
                 UNIQUE (user_id, idempotency_key)\
             )",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_template_instantiations_user_created \
             ON template_instantiations (user_id, created_at DESC)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_template_instantiations_group \
             ON template_instantiations (group_id) WHERE group_id IS NOT NULL",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS template_instantiations")
            .await?;
        Ok(())
    }
}
