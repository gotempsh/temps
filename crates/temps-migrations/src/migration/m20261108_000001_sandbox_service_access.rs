// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Additive control-plane intent storage. These rows grant no runtime access;
//! there is deliberately no enforcement acknowledgement in this migration.
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"CREATE TABLE sandbox_access_services (
    id TEXT PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 80),
    policy JSONB NOT NULL,
    max_ttl_seconds INTEGER NOT NULL CHECK (max_ttl_seconds BETWEEN 60 AND 86400),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    credential_encrypted TEXT,
    version INTEGER NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (id, project_id)
);
CREATE INDEX sandbox_access_services_project ON sandbox_access_services (project_id, created_at DESC, id DESC);
CREATE TABLE sandbox_service_grants (
    id TEXT PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    service_id TEXT NOT NULL,
    sandbox_id INTEGER NOT NULL REFERENCES sandboxes(id) ON DELETE CASCADE,
    service_version INTEGER NOT NULL CHECK (service_version > 0),
    policy_snapshot JSONB NOT NULL,
    created_by INTEGER REFERENCES users(id) ON DELETE SET NULL,
    version INTEGER NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL CHECK (expires_at > created_at AND expires_at <= created_at + INTERVAL '24 hours'),
    revoked_at TIMESTAMPTZ,
    FOREIGN KEY (service_id, project_id) REFERENCES sandbox_access_services(id, project_id) ON DELETE CASCADE
);
CREATE INDEX sandbox_service_grants_scope ON sandbox_service_grants (project_id, sandbox_id, created_at DESC, id DESC);"#).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
            DO $$ BEGIN
                IF EXISTS (SELECT 1 FROM sandbox_access_services) OR EXISTS (SELECT 1 FROM sandbox_service_grants) THEN
                    RAISE EXCEPTION 'Cannot remove sandbox service access: remove saved services and requests explicitly before downgrading';
                END IF;
            END $$;
            DROP TABLE sandbox_service_grants;
            DROP TABLE sandbox_access_services;
        "#).await?;
        Ok(())
    }
}
