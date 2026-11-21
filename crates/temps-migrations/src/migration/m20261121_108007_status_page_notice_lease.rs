// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A send lease on status page notices (ADR 108 D2).
//!
//! `sending_until` is set while one console is mailing one recipient of a
//! notice and cleared when that send ends. No other console moves the
//! notice's cursor while it is set, so a failed send can always hand its
//! recipient back for a retry. A console that died mid-send holds it only
//! until it expires.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE status_page_notices \
                 ADD COLUMN IF NOT EXISTS sending_until TIMESTAMPTZ",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE status_page_notices DROP COLUMN IF EXISTS sending_until",
            )
            .await?;
        Ok(())
    }
}
