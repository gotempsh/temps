// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            "CREATE TABLE compose_security_policies (
                project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                policy JSONB NOT NULL,
                accepted_by INTEGER NOT NULL,
                updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE compose_security_policy_changes (
                id BIGSERIAL PRIMARY KEY,
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                previous JSONB NOT NULL,
                policy JSONB NOT NULL,
                accepted_by INTEGER NOT NULL,
                created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE compose_security_legacy_migrations (
                project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE
            );
            INSERT INTO compose_security_legacy_migrations(project_id)
            SELECT id FROM projects WHERE preset = 'docker-compose'
              AND (COALESCE(preset_config->'unsandboxedServices', preset_config->'unsandboxed_services', '[]'::jsonb) <> '[]'::jsonb)
              AND jsonb_typeof(COALESCE(preset_config->'unsandboxedServices', preset_config->'unsandboxed_services')) = 'array';
            CREATE INDEX compose_security_policy_changes_project_idx ON compose_security_policy_changes(project_id, created_at)"
        ).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DROP TABLE compose_security_legacy_migrations; DROP TABLE compose_security_policy_changes; DROP TABLE compose_security_policies",
            )
            .await?;
        Ok(())
    }
}
