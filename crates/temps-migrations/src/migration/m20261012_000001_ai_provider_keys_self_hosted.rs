// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Self-hosted (OpenAI-compatible) AI provider keys.
//!
//! * `trusted_private_address` binds a per-key trust to one exact `ip:port`,
//!   so one self-hosted model server can be reached without widening the
//!   instance-wide trusted private networks. Who set it and when are kept on
//!   the row so the console can show it next to the key.
//! * `capabilities` holds the capabilities an administrator declared for a
//!   self-hosted key (streaming, tools, embeddings, Responses API); built-in
//!   vendor keys leave it NULL.
//! * Requests are routed by model id against the catalog (looked up ignoring
//!   case, so a self-hosted listing that differs only in capitalisation still
//!   blocks the name heuristic), so `LOWER(ai_provider_models.model_id)` gets
//!   its own index.
//!
//! Existing rows need no backfill: every new column is nullable and NULL
//! means "no per-key trust" / "vendor-defined capabilities".

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
ALTER TABLE ai_provider_keys ADD COLUMN IF NOT EXISTS trusted_private_address TEXT;
ALTER TABLE ai_provider_keys ADD COLUMN IF NOT EXISTS trusted_private_address_set_by INTEGER;
ALTER TABLE ai_provider_keys ADD COLUMN IF NOT EXISTS trusted_private_address_set_at TIMESTAMPTZ;
ALTER TABLE ai_provider_keys ADD COLUMN IF NOT EXISTS capabilities JSONB;
CREATE INDEX IF NOT EXISTS idx_ai_provider_models_model_id_lower ON ai_provider_models(LOWER(model_id));
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
DROP INDEX IF EXISTS idx_ai_provider_models_model_id_lower;
ALTER TABLE ai_provider_keys DROP COLUMN IF EXISTS capabilities;
ALTER TABLE ai_provider_keys DROP COLUMN IF EXISTS trusted_private_address_set_at;
ALTER TABLE ai_provider_keys DROP COLUMN IF EXISTS trusted_private_address_set_by;
ALTER TABLE ai_provider_keys DROP COLUMN IF EXISTS trusted_private_address;
"#,
            )
            .await?;
        Ok(())
    }
}
