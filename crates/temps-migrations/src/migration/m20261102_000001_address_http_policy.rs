// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The user's choice of what a tunnel address answers at Cloudflare's edge
//! (ADR 070): `https_only`, `allow_http` or `allow_all`.
//!
//! Nullable, with no default: an existing address has no recorded choice
//! and keeps working exactly as before until a *Make it reachable* run
//! records one.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE project_custom_domains
    ADD COLUMN IF NOT EXISTS http_policy VARCHAR(16);
"#;

const DOWN: &str = r#"
ALTER TABLE project_custom_domains DROP COLUMN IF EXISTS http_policy;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}
