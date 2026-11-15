// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Direct worker ingress configured from the console (ADR 058 D7).
//!
//! One row per node that has ever been configured or reported. `mode` is what
//! the worker allows (`off`, `fixed`, `delegated`), reported by the agent and
//! never set here. Listen address and local ports apply only to `delegated`
//! workers; the public address and the last check apply to all. Kept off
//! `nodes` so the node row stays about identity and scheduling.

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
CREATE TABLE IF NOT EXISTS node_public_ingress (
    node_id INTEGER PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    listen_address VARCHAR(64),
    http_port INTEGER NOT NULL DEFAULT 80,
    https_port INTEGER NOT NULL DEFAULT 443,
    public_address VARCHAR(64),
    interfaces JSONB,
    listening JSONB,
    mode VARCHAR(20),
    last_check JSONB,
    checked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS node_public_ingress;")
            .await?;
        Ok(())
    }
}
