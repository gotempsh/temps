// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Keep a prepared wake retryable until its authorized chat task starts.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
ALTER TABLE plugin_chat_watches ALTER COLUMN active_since SET DEFAULT statement_timestamp();
ALTER TABLE plugin_watch_events ADD COLUMN received_at TIMESTAMPTZ NOT NULL DEFAULT now();
CREATE INDEX plugin_watch_unmatched_age ON plugin_watch_events(received_at) WHERE source='';
ALTER TABLE plugin_watch_deliveries ADD COLUMN delivered_at TIMESTAMPTZ,
 ADD COLUMN wake_message_id BIGINT REFERENCES ai_messages(id) ON DELETE SET NULL DEFERRABLE INITIALLY DEFERRED;
CREATE OR REPLACE FUNCTION claim_plugin_watch_turn() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    UPDATE plugin_watch_deliveries d SET wake_message_id=NEW.id FROM plugin_chat_watches w,external_plugin_actors a
    WHERE d.id=(NEW.metadata->>'plugin_watch_delivery')::uuid AND w.id=d.watch_id AND w.conversation_id=NEW.conversation_id
    AND d.status='pending' AND d.wake_message_id IS NULL AND w.mode='wake' AND NOT w.paused AND a.id=w.actor_id AND a.active AND a.ai_tools_enabled AND a.binary_sha256=w.digest
    AND EXISTS (SELECT 1 FROM sessions s JOIN ai_conversations c ON c.created_by=s.user_id
                WHERE c.id=w.conversation_id AND s.id=w.session_id AND NOT s.mfa_pending AND s.expires_at>statement_timestamp());
    IF NOT FOUND THEN RAISE EXCEPTION 'Watch delivery has already been claimed or cancelled'; END IF;
    RETURN NEW;
END; $$;
"#).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
LOCK TABLE plugin_watch_deliveries IN ACCESS EXCLUSIVE MODE;
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM plugin_watch_deliveries WHERE status='pending' AND wake_message_id IS NOT NULL) THEN
        RAISE EXCEPTION 'Cannot roll back plugin watch recovery while a pending wake has a prepared message. Finish or cancel the pending wake before retrying.';
    END IF;
END; $$;
CREATE OR REPLACE FUNCTION claim_plugin_watch_turn() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    UPDATE plugin_watch_deliveries d SET status='delivered' FROM plugin_chat_watches w,external_plugin_actors a
    WHERE d.id=(NEW.metadata->>'plugin_watch_delivery')::uuid AND w.id=d.watch_id AND w.conversation_id=NEW.conversation_id
    AND d.status='pending' AND w.mode='wake' AND NOT w.paused AND a.id=w.actor_id AND a.active AND a.ai_tools_enabled AND a.binary_sha256=w.digest;
    IF NOT FOUND THEN RAISE EXCEPTION 'Watch delivery has already been claimed or cancelled'; END IF;
    RETURN NEW;
END; $$;

ALTER TABLE plugin_watch_deliveries DROP COLUMN wake_message_id, DROP COLUMN delivered_at;
ALTER TABLE plugin_chat_watches ALTER COLUMN active_since SET DEFAULT now();
DROP INDEX plugin_watch_unmatched_age;
ALTER TABLE plugin_watch_events DROP COLUMN received_at;
"#).await?;
        Ok(())
    }
}
