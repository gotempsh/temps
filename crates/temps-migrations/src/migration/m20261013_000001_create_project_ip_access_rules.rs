// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-project / per-environment client-IP allow/deny lists.
//!
//! One row is one scope's policy: `environment_id IS NULL` is the project-wide
//! default, a non-NULL `environment_id` overrides it for that environment
//! (including `mode = 'disabled'`, which exempts the environment from a
//! project-wide list). The proxy loads every row into an in-memory snapshot,
//! so the table is only ever read off the request path.

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
CREATE TABLE project_ip_access_rules (
    id SERIAL PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- NULL = the project-wide policy; set = override for one environment.
    environment_id INTEGER REFERENCES environments(id) ON DELETE CASCADE,
    -- 'disabled' keeps the entries but enforces nothing.
    mode VARCHAR(16) NOT NULL DEFAULT 'disabled',
    -- JSON array of {"cidr": "10.0.0.0/8", "description": "Office"}; the
    -- service validates, canonicalises and bounds it before every write.
    cidrs JSONB NOT NULL DEFAULT '[]'::jsonb,
    updated_by INTEGER REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT project_ip_access_rules_mode_valid
        CHECK (mode IN ('disabled', 'allow', 'deny')),
    CONSTRAINT project_ip_access_rules_cidrs_is_array
        CHECK (jsonb_typeof(cidrs) = 'array')
);

-- One policy per scope. NULL environment ids never collide in a plain UNIQUE
-- constraint, so the project-wide row is keyed through COALESCE (environment
-- ids are SERIAL, so 0 is never a real environment).
CREATE UNIQUE INDEX project_ip_access_rules_scope_unique
    ON project_ip_access_rules (project_id, COALESCE(environment_id, 0));

-- The proxy's change-detection poll reads count + max(updated_at).
CREATE INDEX project_ip_access_rules_updated_at
    ON project_ip_access_rules (updated_at);
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS project_ip_access_rules;")
            .await?;
        Ok(())
    }
}
