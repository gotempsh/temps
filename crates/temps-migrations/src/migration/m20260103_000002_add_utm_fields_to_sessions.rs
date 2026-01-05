//! Migration to add UTM tracking fields and channel attribution to request_sessions
//!
//! This enables tracking marketing campaign attribution with UTM parameters
//! and computed channel classification (Organic Search, Paid Social, etc.)

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Add UTM fields to request_sessions
        db.execute_unprepared(
            r#"
            ALTER TABLE request_sessions
            ADD COLUMN IF NOT EXISTS utm_source VARCHAR(255),
            ADD COLUMN IF NOT EXISTS utm_medium VARCHAR(255),
            ADD COLUMN IF NOT EXISTS utm_campaign VARCHAR(255),
            ADD COLUMN IF NOT EXISTS utm_content VARCHAR(255),
            ADD COLUMN IF NOT EXISTS utm_term VARCHAR(255),
            ADD COLUMN IF NOT EXISTS channel VARCHAR(50),
            ADD COLUMN IF NOT EXISTS referrer_hostname VARCHAR(255)
            "#,
        )
        .await?;

        // Backfill referrer_hostname from existing referrer URLs
        db.execute_unprepared(
            r#"
            UPDATE request_sessions
            SET referrer_hostname = CASE
                WHEN referrer IS NULL OR referrer = '' THEN NULL
                WHEN referrer LIKE 'http://%' THEN
                    SPLIT_PART(SUBSTRING(referrer FROM 8), '/', 1)
                WHEN referrer LIKE 'https://%' THEN
                    SPLIT_PART(SUBSTRING(referrer FROM 9), '/', 1)
                ELSE
                    SPLIT_PART(referrer, '/', 1)
            END
            WHERE referrer IS NOT NULL AND referrer != '' AND referrer_hostname IS NULL
            "#,
        )
        .await?;

        // Create indexes for efficient querying (one statement per execute_unprepared)
        db.execute_unprepared(
            r#"CREATE INDEX IF NOT EXISTS idx_request_sessions_utm_source
            ON request_sessions (utm_source) WHERE utm_source IS NOT NULL"#,
        )
        .await?;

        db.execute_unprepared(
            r#"CREATE INDEX IF NOT EXISTS idx_request_sessions_utm_medium
            ON request_sessions (utm_medium) WHERE utm_medium IS NOT NULL"#,
        )
        .await?;

        db.execute_unprepared(
            r#"CREATE INDEX IF NOT EXISTS idx_request_sessions_utm_campaign
            ON request_sessions (utm_campaign) WHERE utm_campaign IS NOT NULL"#,
        )
        .await?;

        db.execute_unprepared(
            r#"CREATE INDEX IF NOT EXISTS idx_request_sessions_channel
            ON request_sessions (channel) WHERE channel IS NOT NULL"#,
        )
        .await?;

        db.execute_unprepared(
            r#"CREATE INDEX IF NOT EXISTS idx_request_sessions_referrer_hostname
            ON request_sessions (referrer_hostname) WHERE referrer_hostname IS NOT NULL"#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Drop indexes (one statement per execute_unprepared)
        db.execute_unprepared("DROP INDEX IF EXISTS idx_request_sessions_utm_source")
            .await?;
        db.execute_unprepared("DROP INDEX IF EXISTS idx_request_sessions_utm_medium")
            .await?;
        db.execute_unprepared("DROP INDEX IF EXISTS idx_request_sessions_utm_campaign")
            .await?;
        db.execute_unprepared("DROP INDEX IF EXISTS idx_request_sessions_channel")
            .await?;
        db.execute_unprepared("DROP INDEX IF EXISTS idx_request_sessions_referrer_hostname")
            .await?;

        // Drop columns (this can be done in a single ALTER TABLE)
        db.execute_unprepared(
            r#"ALTER TABLE request_sessions
            DROP COLUMN IF EXISTS utm_source,
            DROP COLUMN IF EXISTS utm_medium,
            DROP COLUMN IF EXISTS utm_campaign,
            DROP COLUMN IF EXISTS utm_content,
            DROP COLUMN IF EXISTS utm_term,
            DROP COLUMN IF EXISTS channel,
            DROP COLUMN IF EXISTS referrer_hostname"#,
        )
        .await?;

        Ok(())
    }
}
