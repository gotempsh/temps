// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("CREATE TABLE IF NOT EXISTS ci_build_executor (id INTEGER PRIMARY KEY CHECK (id = 1), config_encrypted TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS ci_builds (
                id UUID PRIMARY KEY, deployment_id INTEGER NOT NULL REFERENCES deployments(id) ON DELETE CASCADE,
                state TEXT NOT NULL DEFAULT 'queued', run_id TEXT, token_id INTEGER REFERENCES deployment_tokens(id) ON DELETE SET NULL,
                spec_encrypted TEXT NOT NULL, executor_encrypted TEXT NOT NULL, image_repository TEXT NOT NULL,
                result JSONB, failure TEXT, cancellation_reconciled_at TIMESTAMPTZ, expires_at TIMESTAMPTZ NOT NULL,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW());
            CREATE INDEX IF NOT EXISTS ci_builds_active ON ci_builds(state, expires_at) WHERE state IN ('queued', 'dispatched', 'running');
            CREATE INDEX IF NOT EXISTS ci_builds_reconcile ON ci_builds(updated_at, id) WHERE state IN ('queued', 'dispatched', 'running') OR (state = 'failed' AND cancellation_reconciled_at IS NULL);
            CREATE INDEX IF NOT EXISTS ci_builds_deployment ON ci_builds(deployment_id);").await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DROP TABLE IF EXISTS ci_builds; DROP TABLE IF EXISTS ci_build_executor;",
            )
            .await?;
        Ok(())
    }
}
