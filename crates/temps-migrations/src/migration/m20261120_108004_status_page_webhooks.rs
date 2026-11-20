// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Webhooks of status pages (ADR 108 D2).
//!
//! - `status_page_webhooks`: an owner-registered HTTPS endpoint per row, at
//!   most 10 per page. The signing secret is encrypted at rest. The last
//!   delivery's outcome is kept for the console.
//! - `status_page_webhook_deliveries`: the outbox. One row per endpoint and
//!   page event (`UNIQUE`, so an event is queued once), retried a bounded
//!   number of times, then marked delivered or failed. Finished rows are
//!   pruned after 30 days.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS status_page_webhooks (
                id SERIAL PRIMARY KEY,
                status_page_id INTEGER NOT NULL REFERENCES status_pages(id) ON DELETE CASCADE,
                url TEXT NOT NULL,
                secret_encrypted TEXT NOT NULL,
                enabled BOOLEAN NOT NULL DEFAULT TRUE,
                last_attempt_at TIMESTAMPTZ,
                last_success_at TIMESTAMPTZ,
                last_failure_reason TEXT,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_status_page_webhooks_page \
             ON status_page_webhooks(status_page_id)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS status_page_webhook_deliveries (
                id SERIAL PRIMARY KEY,
                webhook_id INTEGER NOT NULL REFERENCES status_page_webhooks(id) ON DELETE CASCADE,
                event_key TEXT NOT NULL,
                payload TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                delivered_at TIMESTAMPTZ,
                failed_at TIMESTAMPTZ,
                last_error TEXT,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                CONSTRAINT status_page_webhook_deliveries_once UNIQUE (webhook_id, event_key)
            )",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_status_page_webhook_deliveries_due \
             ON status_page_webhook_deliveries(next_attempt_at) \
             WHERE delivered_at IS NULL AND failed_at IS NULL",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP TABLE IF EXISTS status_page_webhook_deliveries")
            .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS status_page_webhooks")
            .await?;
        Ok(())
    }
}
