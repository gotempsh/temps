use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// The email_events table was already created by m20260320_000001_add_email_tracking.
/// This migration adds columns that were missing from the original schema:
/// provider_message_id, recipient, metadata — needed for SNS webhook processing.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Add missing columns (IF NOT EXISTS for idempotency)
        db.execute_unprepared(
            "ALTER TABLE email_events ADD COLUMN IF NOT EXISTS provider_message_id VARCHAR(255) NULL"
        ).await?;

        db.execute_unprepared(
            "ALTER TABLE email_events ADD COLUMN IF NOT EXISTS recipient VARCHAR(255) NULL",
        )
        .await?;

        db.execute_unprepared(
            "ALTER TABLE email_events ADD COLUMN IF NOT EXISTS metadata JSONB NULL",
        )
        .await?;

        // Index: fast lookup for events per email (IF NOT EXISTS for idempotency)
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_email_events_email_created ON email_events (email_id, created_at DESC)"
        ).await?;

        // Index: aggregate stats by event type
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_email_events_type_created ON email_events (event_type, created_at DESC)"
        ).await?;

        // Unique partial index for SNS dedup
        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_email_events_provider_msg_id ON email_events (provider_message_id) WHERE provider_message_id IS NOT NULL"
        ).await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("ALTER TABLE email_events DROP COLUMN IF EXISTS provider_message_id")
            .await?;
        db.execute_unprepared("ALTER TABLE email_events DROP COLUMN IF EXISTS recipient")
            .await?;
        db.execute_unprepared("ALTER TABLE email_events DROP COLUMN IF EXISTS metadata")
            .await?;
        Ok(())
    }
}
