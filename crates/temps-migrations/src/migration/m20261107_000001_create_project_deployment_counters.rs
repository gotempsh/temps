// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-project deployment counter, so concurrent deployments get distinct
//! numbers.
//!
//! A deployment's slug (`<project>-<n>`) is also its container name and its
//! per-deployment hostname. The number used to be `count(deployments) + 1`,
//! read without a lock, so deployments started at the same instant (several
//! environments of one project, for example) all got the same slug, and the
//! deployer replaced one container with another.
//!
//! `last_number` is the last number handed out for the project, bumped with a
//! single `UPDATE ... RETURNING` (see
//! `temps_entities::project_deployment_counters::allocate_deployment_slug`),
//! which Postgres serialises on the row.
//!
//! A table rather than a column on `projects`: the counter is written on
//! every deployment, and keeping it off the `projects` row keeps those writes
//! from contending with (or looking like) project edits.
//!
//! No backfill here: a project gets its row on its first allocation after the
//! upgrade, seeded above both its deployment count and the highest number
//! already used in its slugs, so existing slugs stay valid and are never
//! handed out again. The previous binary ignores the table.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
CREATE TABLE IF NOT EXISTS project_deployment_counters (
    project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    -- The last deployment number handed out for the project.
    last_number BIGINT NOT NULL CHECK (last_number > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
)
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Only derived state: the next allocation reseeds from the
        // deployments themselves.
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS project_deployment_counters")
            .await?;
        Ok(())
    }
}
