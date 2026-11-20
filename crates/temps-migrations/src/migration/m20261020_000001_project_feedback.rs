// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
            CREATE TABLE project_feedback (
                id BIGSERIAL PRIMARY KEY,
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                environment_id INTEGER REFERENCES environments(id) ON DELETE CASCADE,
                deployment_id INTEGER REFERENCES deployments(id) ON DELETE CASCADE,
                source TEXT NOT NULL,
                source_id VARCHAR(32) NOT NULL,
                timestamp TIMESTAMPTZ NOT NULL,
                received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                status TEXT NOT NULL DEFAULT 'open' CHECK(status IN ('open','resolved','ignored')),
                message TEXT NOT NULL,
                name TEXT,
                email TEXT,
                url TEXT,
                associated_event_id VARCHAR(32),
                replay_id VARCHAR(32),
                release TEXT,
                environment TEXT,
                context JSONB NOT NULL DEFAULT '{}',
                UNIQUE(project_id,source,source_id)
            );
            CREATE INDEX project_feedback_inbox ON project_feedback(project_id,status,received_at DESC,id DESC);
            CREATE INDEX project_feedback_recent ON project_feedback(project_id,received_at DESC,id DESC);
        "#).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE project_feedback")
            .await?;
        Ok(())
    }
}
