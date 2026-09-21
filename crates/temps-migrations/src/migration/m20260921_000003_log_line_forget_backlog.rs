// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Durable retry state for line-index forgets (ADR-047 §8a).
//!
//! Retiring a chunk (compaction, purge, retention) must remove its rows from
//! the line index — otherwise deleted or superseded lines keep counting in
//! facets, histograms and aggregates, and search can return pointers into
//! chunks that no longer exist. The immediate forget attempt at the retiring
//! call site is still best-effort (a transient ClickHouse blip or a rejected
//! Cloud insert must not block compaction/purge), but a failure is no longer
//! the end of the story: the chunk is enqueued here first, resolved on
//! success, and `ForgetSweeper` retries whatever is still pending on its own
//! schedule until the index confirms it.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TABLE IF NOT EXISTS log_line_forget_backlog (\
                    chunk_seq BIGINT PRIMARY KEY, \
                    requested_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                    attempts INT NOT NULL DEFAULT 0, \
                    last_error TEXT, \
                    last_attempted_at TIMESTAMPTZ)",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS log_line_forget_backlog")
            .await?;
        Ok(())
    }
}
