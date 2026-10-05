// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
ALTER TABLE ai_gateway_objects DROP CONSTRAINT ai_gateway_objects_provider_key_id_fkey;
ALTER TABLE ai_gateway_objects ALTER COLUMN provider_key_id DROP NOT NULL;
ALTER TABLE ai_gateway_objects ADD CONSTRAINT ai_gateway_objects_provider_key_id_fkey
    FOREIGN KEY (provider_key_id) REFERENCES ai_provider_keys(id) ON DELETE RESTRICT;
ALTER TABLE ai_gateway_objects ADD COLUMN byok_key_encrypted TEXT;
ALTER TABLE ai_gateway_objects ADD COLUMN byok_base_url TEXT;
ALTER TABLE ai_gateway_objects ADD COLUMN credential_scope TEXT;
ALTER TABLE ai_gateway_objects ADD COLUMN next_poll_at TIMESTAMPTZ;
UPDATE ai_gateway_objects SET credential_scope = 'system:' || provider_key_id;
UPDATE ai_gateway_objects SET next_poll_at = NOW() WHERE kind = 'batch' AND usage_recorded_at IS NULL;
ALTER TABLE ai_gateway_objects ALTER COLUMN credential_scope SET NOT NULL;
ALTER TABLE ai_gateway_objects ADD CONSTRAINT ai_gateway_objects_credentials_check
    CHECK ((provider_key_id IS NOT NULL AND credential_scope = 'system:' || provider_key_id)
        OR (provider_key_id IS NULL AND credential_scope LIKE 'byok:%'));
DROP INDEX idx_ai_gateway_objects_upstream;
CREATE UNIQUE INDEX idx_ai_gateway_objects_upstream
    ON ai_gateway_objects (credential_scope, kind, upstream_id);
CREATE INDEX idx_ai_gateway_objects_key ON ai_gateway_objects(provider_key_id) WHERE provider_key_id IS NOT NULL;
CREATE INDEX idx_ai_gateway_objects_due ON ai_gateway_objects(next_poll_at, id)
    WHERE next_poll_at IS NOT NULL;
"#).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Refuse rollback while BYOK accounting metadata still needs the expanded schema.
        manager.get_connection().execute_unprepared(r#"
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM ai_gateway_objects WHERE provider_key_id IS NULL) THEN
        RAISE EXCEPTION 'Cannot roll back batch reconciliation while BYOK objects exist';
    END IF;
END $$;
DROP INDEX idx_ai_gateway_objects_due;
DROP INDEX idx_ai_gateway_objects_key;
DROP INDEX idx_ai_gateway_objects_upstream;
ALTER TABLE ai_gateway_objects DROP CONSTRAINT ai_gateway_objects_credentials_check;
ALTER TABLE ai_gateway_objects DROP COLUMN next_poll_at;
ALTER TABLE ai_gateway_objects DROP COLUMN credential_scope;
ALTER TABLE ai_gateway_objects DROP COLUMN byok_key_encrypted;
ALTER TABLE ai_gateway_objects DROP COLUMN byok_base_url;
ALTER TABLE ai_gateway_objects ALTER COLUMN provider_key_id SET NOT NULL;
CREATE UNIQUE INDEX idx_ai_gateway_objects_upstream ON ai_gateway_objects(provider_key_id, kind, upstream_id);
"#).await?;
        Ok(())
    }
}
