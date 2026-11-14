// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Wrapped project data keys for newly sealed workflow payloads. Legacy ciphertext
//! remains readable. Tombstones retain key identity after intentional shredding.
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("CREATE TABLE IF NOT EXISTS workflow_project_keys (
            id UUID PRIMARY KEY, project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            wrapped_key TEXT, created_at TIMESTAMPTZ NOT NULL DEFAULT now(), shredded_at TIMESTAMPTZ,
            CONSTRAINT workflow_key_material CHECK ((wrapped_key IS NULL) = (shredded_at IS NOT NULL))
        ); CREATE UNIQUE INDEX IF NOT EXISTS workflow_project_active_key ON workflow_project_keys(project_id) WHERE shredded_at IS NULL;
        CREATE INDEX IF NOT EXISTS workflow_project_key_history ON workflow_project_keys(project_id, created_at DESC)").await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("DO $$ BEGIN
            IF EXISTS(SELECT 1 FROM workflow_project_keys) THEN
                RAISE EXCEPTION 'Cannot remove workflow project keys while encrypted payloads may reference them';
            END IF; END $$; DROP TABLE IF EXISTS workflow_project_keys").await?;
        Ok(())
    }
}
