// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Makes a custom domain's hostname unique, so concurrent creates can't each
//! store a copy and leave routing and TLS to pick one.
//!
//! The index can't be built while duplicates exist, and the rows that lose may
//! belong to another project, so nothing is deleted: each loser is renamed to an
//! unroutable `duplicate-<id>.invalid` hostname and marked `failed`, with a message
//! naming the hostname it held and the row that kept it, for its owner to delete.
//! The winner is the row serving traffic, then the one with delivery configured or
//! a certificate, then the oldest.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
WITH ranked AS (
    SELECT
        d.id,
        first_value(d.id) OVER claim AS kept_id,
        row_number() OVER claim AS position
    FROM project_custom_domains d
    WINDOW claim AS (
        PARTITION BY lower(d.domain)
        ORDER BY
            (d.status = 'active') DESC,
            EXISTS (
                SELECT 1 FROM domain_delivery_bindings b WHERE b.custom_domain_id = d.id
            ) DESC,
            (d.certificate_id IS NOT NULL) DESC,
            d.created_at ASC,
            d.id ASC
    )
)
UPDATE project_custom_domains d
SET
    message = 'Duplicate of custom domain ' || ranked.kept_id || ', which kept the hostname '
        || d.domain || '. This entry no longer routes traffic and can be deleted.',
    domain = 'duplicate-' || d.id || '.invalid',
    status = 'failed',
    certificate_id = NULL,
    updated_at = now()
FROM ranked
WHERE d.id = ranked.id AND ranked.position > 1;

CREATE UNIQUE INDEX IF NOT EXISTS idx_project_custom_domains_unique_hostname
    ON project_custom_domains (lower(domain));
"#;

const DOWN: &str = "DROP INDEX IF EXISTS idx_project_custom_domains_unique_hostname;";

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
