// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-055 Phase 10: functions, retired before release.
//!
//! This migration created a `functions` table for scale-to-zero functions.
//! The feature was removed before it shipped; the migration stays, as a
//! no-op, so a database that already applied it still finds it in the list.
//! `m20261028_000002_drop_functions` removes what it created there.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
DELETE FROM ingress_endpoints WHERE owner_kind = 'function';
DROP TABLE IF EXISTS functions;
"#,
            )
            .await?;
        Ok(())
    }
}
