// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Drop `webhook_deliveries.response_body`.
//!
//! Storing the receiver's response body let a webhook pointed at an internal
//! address read that address's response back through the delivery log, so the
//! service stopped writing the column and the API stopped returning it. The
//! column stayed only so older databases kept matching the entity. Nothing
//! reads or writes it now, and any values left from before that change are
//! exactly the data that should not be kept, so it is dropped rather than
//! migrated.
//!
//! `down` re-adds the column as nullable and empty: the dropped values are not
//! restorable, and no code needs them.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE webhook_deliveries DROP COLUMN IF EXISTS response_body",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE webhook_deliveries ADD COLUMN IF NOT EXISTS response_body TEXT NULL",
            )
            .await?;
        Ok(())
    }
}
