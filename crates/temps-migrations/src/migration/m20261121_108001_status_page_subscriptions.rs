// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Email subscribers of a public status page (ADR 108 D2).
//!
//! `status_pages` gains the per-page switch and sender address, so the setting
//! lives on the row it configures, and whether a project-scoped token chose
//! that sender (its domain grant is then checked again at delivery).
//! `status_incident_updates.published_at` records when an update became
//! public, so a draft published later is announced from then. `subscriptions_enabled_at` bounds what is
//! announced: only updates published after subscriptions were switched on are
//! ever sent, so enabling them never mails out the page's history.
//!
//! `status_page_subscribers` holds one row per address per page. Addresses are
//! personal data: rows cascade with the page, are deleted one at a time by the
//! owner or by the subscriber's own unsubscribe link, and a pending row that is
//! never confirmed is deleted once its request expires. No token is stored in
//! plaintext: `confirm_token_hash` is the SHA-256 of the one-time confirmation
//! token (minted when the confirmation mail is sent), and
//! `unsubscribe_token_hash` the SHA-256 of a token derived from the server key
//! and `unsubscribe_nonce`, so every mail can carry the link without the token
//! being stored.
//!
//! `status_page_notices` is the outbox: one row per public update (or
//! resolution) to announce, with a cursor over subscriber ids so delivery is
//! resumable, batched and bounded per tick. Recipients are the subscribers
//! confirmed before the notice was queued.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"
            ALTER TABLE status_pages
                ADD COLUMN IF NOT EXISTS subscriptions_enabled BOOLEAN NOT NULL DEFAULT FALSE,
                ADD COLUMN IF NOT EXISTS subscriptions_enabled_at TIMESTAMPTZ,
                ADD COLUMN IF NOT EXISTS subscription_sender TEXT,
                ADD COLUMN IF NOT EXISTS subscription_sender_scoped BOOLEAN NOT NULL DEFAULT FALSE
            "#,
        )
        .await?;
        // An update is announced from when it became public, which can be
        // long after it was written as a private draft. Updates that are
        // already public count as published when they were written.
        db.execute_unprepared(
            "ALTER TABLE status_incident_updates ADD COLUMN IF NOT EXISTS published_at TIMESTAMPTZ",
        )
        .await?;
        db.execute_unprepared(
            "UPDATE status_incident_updates SET published_at = created_at \
             WHERE is_public AND published_at IS NULL",
        )
        .await?;
        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS status_page_subscribers (
                id SERIAL PRIMARY KEY,
                status_page_id INTEGER NOT NULL REFERENCES status_pages(id) ON DELETE CASCADE,
                email TEXT NOT NULL,
                state TEXT NOT NULL DEFAULT 'pending'
                    CHECK (state IN ('pending', 'confirmed')),
                confirm_token_hash TEXT,
                confirm_expires_at TIMESTAMPTZ,
                confirmation_sent_at TIMESTAMPTZ,
                requested_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                unsubscribe_nonce TEXT NOT NULL,
                unsubscribe_token_hash TEXT NOT NULL,
                confirmed_at TIMESTAMPTZ,
                created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                CONSTRAINT uq_status_page_subscribers_page_email UNIQUE (status_page_id, email)
            )
            "#,
        )
        .await?;
        for statement in [
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_status_page_subscribers_unsubscribe \
             ON status_page_subscribers(unsubscribe_token_hash)",
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_status_page_subscribers_confirm \
             ON status_page_subscribers(confirm_token_hash) WHERE confirm_token_hash IS NOT NULL",
            "CREATE INDEX IF NOT EXISTS idx_status_page_subscribers_page_state \
             ON status_page_subscribers(status_page_id, state, id)",
        ] {
            db.execute_unprepared(statement).await?;
        }
        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS status_page_notices (
                id SERIAL PRIMARY KEY,
                status_page_id INTEGER NOT NULL REFERENCES status_pages(id) ON DELETE CASCADE,
                notice_key TEXT NOT NULL,
                subject TEXT NOT NULL,
                body_text TEXT NOT NULL,
                body_html TEXT NOT NULL,
                cursor_subscriber_id INTEGER NOT NULL DEFAULT 0,
                sent_count INTEGER NOT NULL DEFAULT 0,
                created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                completed_at TIMESTAMPTZ,
                CONSTRAINT uq_status_page_notices_page_key UNIQUE (status_page_id, notice_key)
            )
            "#,
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_status_page_notices_open \
             ON status_page_notices(id) WHERE completed_at IS NULL",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP TABLE IF EXISTS status_page_notices")
            .await?;
        db.execute_unprepared(
            "ALTER TABLE status_incident_updates DROP COLUMN IF EXISTS published_at",
        )
        .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS status_page_subscribers")
            .await?;
        db.execute_unprepared(
            r#"
            ALTER TABLE status_pages
                DROP COLUMN IF EXISTS subscription_sender_scoped,
                DROP COLUMN IF EXISTS subscription_sender,
                DROP COLUMN IF EXISTS subscriptions_enabled_at,
                DROP COLUMN IF EXISTS subscriptions_enabled
            "#,
        )
        .await?;
        Ok(())
    }
}
