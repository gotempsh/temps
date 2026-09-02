// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Mark an `s3_sources` row as provisioned by Temps Cloud rather than an
//! operator.
//!
//! Cloud-managed rows are auto-created when an instance links to Temps
//! Cloud and hold a credential the instance never chose and cannot rotate
//! locally, so they must be excluded from the user-initiated edit/delete
//! paths that ordinary S3 sources go through.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE s3_sources \
             ADD COLUMN IF NOT EXISTS managed_by_cloud BOOLEAN NOT NULL DEFAULT FALSE",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("ALTER TABLE s3_sources DROP COLUMN IF EXISTS managed_by_cloud")
            .await?;
        Ok(())
    }
}
