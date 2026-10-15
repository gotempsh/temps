// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A project group's repository, for monorepos: the group names the repo and
//! branch once, and members created from it inherit them. The deploy pipeline
//! keeps reading each project's own git columns; the group is the source of
//! truth and writes its repo/branch through to every inheriting member.
//!
//! The repository is read either through a git connection or, for public
//! GitHub/GitLab repositories, anonymously (`public_provider`); never both.
//! The connection is nullable on its own so deleting a git connection does
//! not fail on this table: the group keeps naming the repo, and a repo with
//! neither a connection nor a public provider tells the console to ask for a
//! connection again.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE project_groups \
                 ADD COLUMN IF NOT EXISTS git_provider_connection_id INTEGER \
                     REFERENCES git_provider_connections(id) ON DELETE SET NULL, \
                 ADD COLUMN IF NOT EXISTS repo_owner VARCHAR(255), \
                 ADD COLUMN IF NOT EXISTS repo_name VARCHAR(255), \
                 ADD COLUMN IF NOT EXISTS main_branch VARCHAR(255), \
                 ADD COLUMN IF NOT EXISTS public_provider VARCHAR(32)",
        )
        .await?;
        db.execute_unprepared(
            "DO $$ BEGIN \
                 ALTER TABLE project_groups ADD CONSTRAINT ck_project_groups_repository_source \
                     CHECK (public_provider IS NULL \
                         OR (public_provider IN ('github', 'gitlab') \
                             AND git_provider_connection_id IS NULL \
                             AND repo_owner IS NOT NULL)); \
             EXCEPTION WHEN duplicate_object THEN NULL; END $$",
        )
        .await?;
        db.execute_unprepared(
            "DO $$ BEGIN \
                 ALTER TABLE project_groups ADD CONSTRAINT ck_project_groups_repository \
                     CHECK ((repo_owner IS NULL AND repo_name IS NULL AND main_branch IS NULL) \
                         OR (repo_owner IS NOT NULL AND repo_name IS NOT NULL AND main_branch IS NOT NULL)); \
             EXCEPTION WHEN duplicate_object THEN NULL; END $$",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE project_group_members \
                 ADD COLUMN IF NOT EXISTS inherits_repository BOOLEAN NOT NULL DEFAULT FALSE",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE project_group_members DROP COLUMN IF EXISTS inherits_repository",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE project_groups \
                 DROP CONSTRAINT IF EXISTS ck_project_groups_repository_source, \
                 DROP CONSTRAINT IF EXISTS ck_project_groups_repository, \
                 DROP COLUMN IF EXISTS public_provider, \
                 DROP COLUMN IF EXISTS main_branch, \
                 DROP COLUMN IF EXISTS repo_name, \
                 DROP COLUMN IF EXISTS repo_owner, \
                 DROP COLUMN IF EXISTS git_provider_connection_id",
        )
        .await?;
        Ok(())
    }
}
