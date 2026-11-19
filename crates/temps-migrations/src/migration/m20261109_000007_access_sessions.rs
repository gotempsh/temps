// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Vault sessions group atomically issued sandbox grants under a fixed deadline.
//! Existing individual grants remain unchanged. No authorization is backfilled.
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
CREATE TABLE IF NOT EXISTS access_sessions (
 id text PRIMARY KEY, project_id integer NOT NULL,
 vault_id text NOT NULL, bundle_id text NOT NULL REFERENCES access_bundles(id) ON DELETE RESTRICT,
 bundle_name text NOT NULL, bundle_version integer NOT NULL CHECK (bundle_version > 0),
 consumer_kind text NOT NULL CHECK (consumer_kind = 'sandbox'),
 consumer_id text NOT NULL, sandbox_id integer NOT NULL REFERENCES sandboxes(id) ON DELETE CASCADE,
 created_by integer REFERENCES users(id) ON DELETE SET NULL,
 consent jsonb NOT NULL,
 grant_ids text[] NOT NULL CHECK (cardinality(grant_ids) BETWEEN 1 AND 16),
 created_at timestamptz NOT NULL, expires_at timestamptz NOT NULL CHECK (expires_at > created_at),
 revoked_at timestamptz,
 FOREIGN KEY (project_id, vault_id) REFERENCES access_vaults(project_id, id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS access_sessions_vault_page ON access_sessions(project_id, vault_id, created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS access_sessions_sandbox_page ON access_sessions(project_id, sandbox_id, created_at DESC, id DESC);
"#).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
DO $$ BEGIN
 IF EXISTS (SELECT 1 FROM access_sessions) THEN
  RAISE EXCEPTION 'Cannot downgrade while access sessions exist; retain their authorization history';
 END IF;
END $$;
DROP TABLE IF EXISTS access_sessions;
"#).await?;
        Ok(())
    }
}
