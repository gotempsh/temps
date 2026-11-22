// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Error events are time/id-partitioned hypertables: their SDK identity
        // cannot be a standalone unique index. Store only native identity and
        // receipt metadata here, atomically with the existing event write.
        manager.get_connection().execute_unprepared(r#"
            CREATE TABLE error_event_source_keys (
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                sdk_event_id VARCHAR(32) NOT NULL,
                environment_id INTEGER,
                deployment_id INTEGER,
                error_group_id INTEGER NOT NULL REFERENCES error_groups(id) ON DELETE CASCADE,
                error_event_id BIGINT NOT NULL,
                received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                PRIMARY KEY(project_id, sdk_event_id)
            );
            CREATE INDEX error_event_source_keys_expiry ON error_event_source_keys(received_at);
            CREATE INDEX error_events_sdk_identity ON error_events(project_id,(data #>> '{sentry,event_id}'),timestamp DESC);
            CREATE INDEX error_events_native_user ON error_events(project_id,(data #>> '{user,user_id}'),timestamp DESC);
            CREATE INDEX error_events_native_tags ON error_events USING GIN ((data #> '{trace,extra,tags}') jsonb_path_ops)
        "#).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("DROP INDEX IF EXISTS error_events_sdk_identity; DROP INDEX IF EXISTS error_events_native_tags; DROP INDEX IF EXISTS error_events_native_user; DROP TABLE error_event_source_keys").await?;
        Ok(())
    }
}
