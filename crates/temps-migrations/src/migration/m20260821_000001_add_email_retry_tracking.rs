// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Add `provider_id` and `attempt_count` columns to the `emails` table.
//!
//! `provider_id` records which email provider was used for a given send attempt,
//! allowing operators to correlate delivery failures with specific provider
//! configurations without parsing log lines.
//!
//! `attempt_count` records how many provider send attempts were made before the
//! email reached a terminal state (`sent`, `captured`, `delivery_unknown`).
//! A value of 1 means the first attempt succeeded or was definitively rejected.
//! A value of 2 means the retry path ran.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();

        conn.execute_unprepared(
            r#"
ALTER TABLE emails
    ADD COLUMN provider_id INTEGER REFERENCES email_providers(id) ON DELETE SET NULL,
    ADD COLUMN attempt_count INTEGER;
"#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();

        conn.execute_unprepared(
            r#"
ALTER TABLE emails
    DROP COLUMN IF EXISTS attempt_count,
    DROP COLUMN IF EXISTS provider_id;
"#,
        )
        .await?;

        Ok(())
    }
}
