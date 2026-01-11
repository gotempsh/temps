//! Migration to add has_activity column to visitor table
//!
//! This enables filtering out "ghost" visitors that have no sessions or page views.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Add has_activity column with default false
        db.execute_unprepared(
            r#"
            ALTER TABLE visitor
            ADD COLUMN IF NOT EXISTS has_activity BOOLEAN NOT NULL DEFAULT FALSE
            "#,
        )
        .await?;

        // Update existing visitors that have events to set has_activity = true
        db.execute_unprepared(
            r#"
            UPDATE visitor v
            SET has_activity = TRUE
            WHERE EXISTS (
                SELECT 1 FROM events e WHERE e.visitor_id = v.id
            )
            "#,
        )
        .await?;

        // Create index for efficient filtering
        db.execute_unprepared(
            r#"
            CREATE INDEX IF NOT EXISTS idx_visitor_has_activity
            ON visitor (project_id, has_activity)
            WHERE has_activity = TRUE
            "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
            DROP INDEX IF EXISTS idx_visitor_has_activity;
            ALTER TABLE visitor DROP COLUMN IF EXISTS has_activity
            "#,
        )
        .await?;

        Ok(())
    }
}
