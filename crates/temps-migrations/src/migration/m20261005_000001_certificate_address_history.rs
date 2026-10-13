// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Retain issued IP identities independently of live node records.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
CREATE TABLE cluster_certificate_history (
    ca_key TEXT PRIMARY KEY
);
CREATE TABLE cluster_certificate_addresses (
    ca_key TEXT NOT NULL,
    address TEXT NOT NULL,
    PRIMARY KEY (ca_key, address)
);
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DROP TABLE cluster_certificate_addresses; DROP TABLE cluster_certificate_history;",
            )
            .await?;
        Ok(())
    }
}
