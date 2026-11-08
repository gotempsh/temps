// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Server-issued, single-use `state` tokens for the GitHub App manifest flow.
//!
//! When an operator starts "create a GitHub App", the console asks the server
//! for a state, sends it to GitHub, and GitHub returns it to the callback with
//! the manifest `code`. The callback accepts a code only together with a state
//! the server issued and has not yet consumed, as the GitLab callback does.
//!
//! Only a SHA-256 of each state is stored (`state_hash`), so a pending state
//! cannot be read back from the table.
//!
//! This is a separate table from `oauth_states`: that table requires a
//! `provider_id`, and in this flow the provider does not exist yet. Keeping the
//! two apart also means a manifest state can never be accepted by an OAuth
//! callback, or the reverse.
//!
//! The table is new and empty, so there is nothing to backfill. A flow started
//! before the upgrade has no stored state; the operator starts it again.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS github_manifest_states (
                id serial PRIMARY KEY,
                state_hash varchar(64) NOT NULL UNIQUE,
                user_id integer NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                expires_at timestamptz NOT NULL,
                created_at timestamptz NOT NULL DEFAULT now()
            )",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_github_manifest_states_expires_at
                ON github_manifest_states (expires_at)",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Pending states are short-lived (minutes) and only matter to a flow
        // that is in progress, so dropping them loses nothing durable.
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS github_manifest_states")
            .await?;
        Ok(())
    }
}
