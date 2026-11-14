// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Explicit environment-to-environment grants, owned by the destination project.
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("CREATE TABLE workflow_grants (
            id UUID PRIMARY KEY,
            source_environment_id INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
            target_environment_id INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
            action TEXT NOT NULL CHECK(action IN ('start','event')),
            resource TEXT NOT NULL CHECK(char_length(resource) BETWEEN 1 AND 200),
            created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
            CHECK(source_environment_id <> target_environment_id),
            UNIQUE(source_environment_id,target_environment_id,action,resource)
        ); CREATE INDEX workflow_grant_target ON workflow_grants(target_environment_id,created_at DESC,id DESC)").await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("DROP TABLE workflow_grants").await?;
        Ok(())
    }
}
