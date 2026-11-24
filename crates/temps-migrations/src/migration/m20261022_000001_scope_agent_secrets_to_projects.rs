// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Give agent secrets an optional project scope.
//!
//! `agent_secrets` was instance-wide only: every row was decrypted into every
//! agent sandbox of every project, so any member who could run an agent could
//! read secrets meant for a different project. A secret can now be bound to
//! one project (`project_id`) and is then resolved only for that project's
//! runs.
//!
//! Existing rows keep `project_id = NULL`, which stays the explicit
//! "instance-wide" scope. That is what the operator configured them as -- the
//! console has always described these secrets as "injected into all agent
//! sandboxes" -- and we cannot infer which project a secret was meant for, so
//! guessing a project would break every other project's agents silently.
//! Operators re-scope a secret by saving it again with a `project_id`.
//!
//! Names stay unique per scope: one instance-wide `GITHUB_TOKEN` and one per
//! project may coexist, and the project-scoped one wins for that project.

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
                ALTER TABLE agent_secrets
                    ADD COLUMN IF NOT EXISTS project_id INTEGER
                        REFERENCES projects(id) ON DELETE CASCADE;

                -- The table may predate the squash (renamed from
                -- project_secrets), so the single-column unique constraint on
                -- `name` is dropped by shape, not by a guessed name.
                DO $$
                DECLARE
                    constraint_name TEXT;
                BEGIN
                    FOR constraint_name IN
                        SELECT c.conname
                        FROM pg_constraint c
                        JOIN pg_attribute a
                          ON a.attrelid = c.conrelid AND a.attnum = ANY (c.conkey)
                        WHERE c.conrelid = 'agent_secrets'::regclass
                          AND c.contype = 'u'
                          AND array_length(c.conkey, 1) = 1
                          AND a.attname = 'name'
                    LOOP
                        EXECUTE format('ALTER TABLE agent_secrets DROP CONSTRAINT %I', constraint_name);
                    END LOOP;
                END $$;

                CREATE UNIQUE INDEX IF NOT EXISTS agent_secrets_scope_name_unique
                    ON agent_secrets (project_id, name) NULLS NOT DISTINCT;
                CREATE INDEX IF NOT EXISTS agent_secrets_project_id
                    ON agent_secrets (project_id);
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Project-scoped rows cannot be folded back into a global namespace
        // without losing one side of a name collision, so they are dropped.
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                DELETE FROM agent_secrets WHERE project_id IS NOT NULL;
                DROP INDEX IF EXISTS agent_secrets_project_id;
                DROP INDEX IF EXISTS agent_secrets_scope_name_unique;
                ALTER TABLE agent_secrets DROP COLUMN IF EXISTS project_id;
                ALTER TABLE agent_secrets ADD CONSTRAINT agent_secrets_name_key UNIQUE (name);
                "#,
            )
            .await?;
        Ok(())
    }
}
