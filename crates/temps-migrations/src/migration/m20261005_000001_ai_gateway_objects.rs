// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Ownership of provider-side objects created through the AI gateway.
//!
//! Files and batches that the gateway creates with an administrator-configured
//! provider key live in the operator's provider account, where every caller of
//! the gateway shares one namespace. One row per object records who created it
//! (a user, or a project for deployment tokens) and which provider key holds
//! it, so the gateway can refuse to hand one caller's batch results to another.
//! Objects created with a caller's own key (BYOK) are not recorded: the
//! provider already scopes those to the caller's account.

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
CREATE TABLE IF NOT EXISTS ai_gateway_objects (
    id BIGSERIAL PRIMARY KEY,
    kind TEXT NOT NULL,
    upstream_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    provider_key_id INTEGER NOT NULL REFERENCES ai_provider_keys (id) ON DELETE CASCADE,
    owner_user_id INTEGER,
    owner_project_id INTEGER,
    -- Model every request in a batch input file targets (one per file).
    model TEXT,
    -- Endpoint every request in a batch input file targets, e.g. /v1/responses.
    endpoint TEXT,
    -- Set once the token usage of a finished batch has been written to
    -- ai_usage_logs, so it is recorded exactly once.
    usage_recorded_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT ai_gateway_objects_kind_check CHECK (kind IN ('file', 'batch')),
    CONSTRAINT ai_gateway_objects_owner_check
        CHECK ((owner_user_id IS NOT NULL) <> (owner_project_id IS NOT NULL))
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_ai_gateway_objects_upstream
    ON ai_gateway_objects (provider_key_id, kind, upstream_id);
CREATE INDEX IF NOT EXISTS idx_ai_gateway_objects_user
    ON ai_gateway_objects (owner_user_id, kind, upstream_id)
    WHERE owner_user_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_ai_gateway_objects_project
    ON ai_gateway_objects (owner_project_id, kind, upstream_id)
    WHERE owner_project_id IS NOT NULL;
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS ai_gateway_objects;")
            .await?;
        Ok(())
    }
}
