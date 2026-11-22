// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Indexes for Shield's console reads and its learning pass (ADR 066).
//!
//! - Proposals list newest first, with or without a status, and learning
//!   reads recent rows every 30 seconds: both order by `last_seen DESC, id
//!   DESC`, so each has an index in that order and no sort.
//! - The Shield tab counts verdicts and sources over `proxy_logs`. Only
//!   requests with a verdict other than `pass` carry one, a small share of
//!   the table, so a partial index keeps the count off every other row.
//! - Operator entries are listed by scope and service, and both tables'
//!   foreign keys cascade from a deleted service or environment.
//!
//! On a large `proxy_logs`, creating its index holds writes to the table
//! for as long as the build takes.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE INDEX IF NOT EXISTS shield_observed_last_seen
    ON shield_observed (last_seen DESC, id DESC);
DROP INDEX IF EXISTS shield_observed_status;
CREATE INDEX IF NOT EXISTS shield_observed_status_last_seen
    ON shield_observed (status, last_seen DESC, id DESC);

CREATE INDEX IF NOT EXISTS shield_operator_entries_owner
    ON shield_operator_entries (scope, COALESCE(project_id, 0), created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS shield_bypass_tokens_environment
    ON shield_bypass_tokens (environment_id);

CREATE INDEX IF NOT EXISTS idx_proxy_logs_shield_verdict
    ON proxy_logs (project_id, environment_id, timestamp DESC)
    WHERE shield_verdict IS NOT NULL AND shield_verdict <> '';
"#;

const DOWN: &str = r#"
DROP INDEX IF EXISTS idx_proxy_logs_shield_verdict;
DROP INDEX IF EXISTS shield_bypass_tokens_environment;
DROP INDEX IF EXISTS shield_operator_entries_owner;
DROP INDEX IF EXISTS shield_observed_status_last_seen;
CREATE INDEX IF NOT EXISTS shield_observed_status ON shield_observed (status, last_seen DESC);
DROP INDEX IF EXISTS shield_observed_last_seen;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}
