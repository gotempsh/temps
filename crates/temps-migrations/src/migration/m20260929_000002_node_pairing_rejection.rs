// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Why the control plane last refused a paired node's key (ADR 048 D2b).
//!
//! `last_error` describes the latest attempt, so once a refused node stops
//! answering, "no answer yet" would replace the one explanation the operator
//! needs (its key belongs to another node). The refusal is kept here until a
//! key is accepted.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE node_pairings ADD COLUMN IF NOT EXISTS last_rejection TEXT;",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE node_pairings DROP COLUMN IF EXISTS last_rejection;")
            .await?;
        Ok(())
    }
}
