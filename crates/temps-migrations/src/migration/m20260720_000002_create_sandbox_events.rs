//! Create `sandbox_events` — the per-sandbox operations timeline.
//!
//! Records lifecycle operations (create/stop/resume/restart/extend/resize/
//! preview-password/destroy), NOT shell activity. Rows are append-only.

use sea_orm_migration::prelude::*;

const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS sandbox_events (
    id          SERIAL PRIMARY KEY,
    sandbox_id  INTEGER NOT NULL,
    event_type  VARCHAR NOT NULL,
    detail      JSONB,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_sandbox_events_sandbox_id
    ON sandbox_events (sandbox_id, created_at DESC);
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS sandbox_events")
            .await?;
        Ok(())
    }
}
