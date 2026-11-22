// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Preserve the exact grant set acknowledged by the assigned worker. Existing
//! rows have no receipt, so upgrades never infer connectivity from an old intent.
//! Downgrade drops only derived observations; authorization and leases remain.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            "ALTER TABLE sandbox_access_runtimes ADD COLUMN IF NOT EXISTS applied_revision BIGINT CHECK (applied_revision >= 0), ADD COLUMN IF NOT EXISTS applied_grant_ids TEXT[] NOT NULL DEFAULT '{}' CHECK (cardinality(applied_grant_ids) <= 16)"
        ).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            "ALTER TABLE IF EXISTS sandbox_access_runtimes DROP COLUMN IF EXISTS applied_revision, DROP COLUMN IF EXISTS applied_grant_ids"
        ).await?;
        Ok(())
    }
}
