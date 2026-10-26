// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! "Only these tailnet users" (ADR 065 D6).
//!
//! - `audience` accepts `tailnet_users` on `project_custom_domains` and
//!   `environment_domains`: private in every way `private` is, and
//!   additionally only for the listed people or devices. The 3a check on
//!   `private_via` already requires a way in for it.
//! - `address_tailnet_principals`: who a `tailnet_users` address admits, one
//!   row per Tailscale login (`alice@example.com`, `alice@github`) or tag
//!   (`tag:ci`), keyed like `tailnet_serve_entries` by
//!   `temps_core::worker_ingress::private_address_id`. Values are stored
//!   ASCII-lower-cased, which is how they are compared.
//!
//! Nothing changes for an install that never uses it: the table stays empty
//! and no row's audience changes.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE project_custom_domains
    DROP CONSTRAINT IF EXISTS project_custom_domains_audience_check;
ALTER TABLE project_custom_domains
    ADD CONSTRAINT project_custom_domains_audience_check
    CHECK (audience IN ('public', 'private', 'signed_in', 'tailnet_users'));

ALTER TABLE environment_domains
    DROP CONSTRAINT IF EXISTS environment_domains_audience_check;
ALTER TABLE environment_domains
    ADD CONSTRAINT environment_domains_audience_check
    CHECK (audience IN ('public', 'private', 'signed_in', 'tailnet_users'));

CREATE TABLE IF NOT EXISTS address_tailnet_principals (
    id SERIAL PRIMARY KEY,
    address_id VARCHAR(32) NOT NULL,
    kind VARCHAR(8) NOT NULL CHECK (kind IN ('login', 'tag')),
    value VARCHAR(320) NOT NULL CHECK (value <> ''),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT address_tailnet_principals_unique UNIQUE (address_id, kind, value)
);
"#;

// A row left `tailnet_users` keeps its value: an unknown audience is served
// to nobody, so rolling back never widens it to the whole range. The
// constraint is restored `NOT VALID` so those rows stay as they are.
const DOWN: &str = r#"
DROP TABLE IF EXISTS address_tailnet_principals;
ALTER TABLE environment_domains
    DROP CONSTRAINT IF EXISTS environment_domains_audience_check;
ALTER TABLE environment_domains
    ADD CONSTRAINT environment_domains_audience_check
    CHECK (audience IN ('public', 'private', 'signed_in')) NOT VALID;
ALTER TABLE project_custom_domains
    DROP CONSTRAINT IF EXISTS project_custom_domains_audience_check;
ALTER TABLE project_custom_domains
    ADD CONSTRAINT project_custom_domains_audience_check
    CHECK (audience IN ('public', 'private', 'signed_in')) NOT VALID;
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
