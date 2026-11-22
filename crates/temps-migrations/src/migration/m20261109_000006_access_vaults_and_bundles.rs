// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Named, project-scoped service collections and reusable access selections.
//! Existing services remain available to add to a vault. No grants are created
//! by migration. Downgrade refuses to discard configured vaults or bundles.
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
CREATE TABLE IF NOT EXISTS access_vaults (
 id text PRIMARY KEY, project_id integer NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
 name text NOT NULL CHECK (length(name) BETWEEN 1 AND 80),
 service_ids text[] NOT NULL DEFAULT '{}' CHECK (cardinality(service_ids) <= 100),
 version integer NOT NULL DEFAULT 1 CHECK (version > 0),
 created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now(),
 UNIQUE (project_id, id)
);
CREATE INDEX IF NOT EXISTS access_vaults_project_page ON access_vaults(project_id, created_at DESC, id DESC);
CREATE TABLE IF NOT EXISTS access_bundles (
 id text PRIMARY KEY, project_id integer NOT NULL, vault_id text NOT NULL,
 name text NOT NULL CHECK (length(name) BETWEEN 1 AND 80),
 service_ids text[] NOT NULL DEFAULT '{}' CHECK (cardinality(service_ids) BETWEEN 1 AND 100),
 ttl_seconds integer NOT NULL CHECK (ttl_seconds BETWEEN 60 AND 86400),
 version integer NOT NULL DEFAULT 1 CHECK (version > 0),
 created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now(),
 FOREIGN KEY (project_id, vault_id) REFERENCES access_vaults(project_id, id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS access_bundles_vault_page ON access_bundles(project_id, vault_id, created_at DESC, id DESC);
"#).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
DO $$ BEGIN
 IF EXISTS (SELECT 1 FROM access_vaults) THEN
  RAISE EXCEPTION 'Cannot downgrade while access vaults exist; export and remove their configuration first';
 END IF;
END $$;
DROP TABLE IF EXISTS access_bundles;
DROP TABLE IF EXISTS access_vaults;
"#).await?;
        Ok(())
    }
}
