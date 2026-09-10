// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Step 1: Add first-visit attribution columns to visitor table
        db.execute_unprepared(
            r#"
            ALTER TABLE visitor
            ADD COLUMN IF NOT EXISTS first_referrer TEXT,
            ADD COLUMN IF NOT EXISTS first_referrer_hostname TEXT,
            ADD COLUMN IF NOT EXISTS first_channel TEXT,
            ADD COLUMN IF NOT EXISTS first_utm_source TEXT,
            ADD COLUMN IF NOT EXISTS first_utm_medium TEXT,
            ADD COLUMN IF NOT EXISTS first_utm_campaign TEXT
            "#,
        )
        .await?;

        // Step 2: Backfill existing visitors from their earliest request_sessions record
        db.execute_unprepared(
            r#"
            UPDATE visitor v
            SET first_referrer = rs.referrer,
                first_referrer_hostname = rs.referrer_hostname,
                first_channel = rs.channel,
                first_utm_source = rs.utm_source,
                first_utm_medium = rs.utm_medium,
                first_utm_campaign = rs.utm_campaign
            FROM (
                SELECT DISTINCT ON (visitor_id)
                    visitor_id,
                    referrer,
                    referrer_hostname,
                    channel,
                    utm_source,
                    utm_medium,
                    utm_campaign
                FROM request_sessions
                WHERE visitor_id IS NOT NULL
                ORDER BY visitor_id, started_at ASC
            ) rs
            WHERE v.id = rs.visitor_id
              AND v.first_referrer IS NULL
            "#,
        )
        .await?;

        // Step 3: Create index on first_channel for filtering/grouping visitors by acquisition channel
        db.execute_unprepared(
            r#"
            CREATE INDEX IF NOT EXISTS idx_visitor_first_channel
            ON visitor (project_id, first_channel)
            WHERE first_channel IS NOT NULL
            "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
            DROP INDEX IF EXISTS idx_visitor_first_channel;
            ALTER TABLE visitor
            DROP COLUMN IF EXISTS first_referrer,
            DROP COLUMN IF EXISTS first_referrer_hostname,
            DROP COLUMN IF EXISTS first_channel,
            DROP COLUMN IF EXISTS first_utm_source,
            DROP COLUMN IF EXISTS first_utm_medium,
            DROP COLUMN IF EXISTS first_utm_campaign
            "#,
        )
        .await?;

        Ok(())
    }
}
