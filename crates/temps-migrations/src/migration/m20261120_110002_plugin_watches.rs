// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Store per-chat consent and deduplicate plugin outbox retries across restarts.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
CREATE TABLE plugin_chat_watches (
 id UUID PRIMARY KEY DEFAULT gen_random_uuid(), conversation_id BIGINT NOT NULL REFERENCES ai_conversations(id) ON DELETE CASCADE,
 actor_id UUID NOT NULL REFERENCES external_plugin_actors(id) ON DELETE CASCADE, digest TEXT NOT NULL,
 label TEXT, predicate JSONB NOT NULL, source TEXT NOT NULL, subject TEXT NOT NULL, mode TEXT NOT NULL CHECK(mode IN ('notify','wake')),
 active_since TIMESTAMPTZ NOT NULL DEFAULT now(), session_id INTEGER NOT NULL, paused BOOLEAN NOT NULL DEFAULT false, created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 UNIQUE(conversation_id,actor_id,source,predicate)
);
CREATE INDEX plugin_chat_watches_topic ON plugin_chat_watches(actor_id,source,subject) WHERE NOT paused;
CREATE TABLE plugin_watch_events (
 actor_id UUID NOT NULL REFERENCES external_plugin_actors(id) ON DELETE CASCADE, event_id TEXT NOT NULL,
 attributes JSONB NOT NULL, source TEXT NOT NULL, subject TEXT NOT NULL, origin_chat_id TEXT, PRIMARY KEY(actor_id,event_id)
);
CREATE TABLE plugin_watch_deliveries (
 id UUID PRIMARY KEY DEFAULT gen_random_uuid(), watch_id UUID NOT NULL REFERENCES plugin_chat_watches(id) ON DELETE CASCADE,
 event_id TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending', next_attempt TIMESTAMPTZ NOT NULL DEFAULT now(),
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), UNIQUE(watch_id,event_id)
);
-- A wake is durably delivered when its user message is persisted. A crash
-- during model preparation leaves an unconsumed delivery for the next worker.
CREATE FUNCTION claim_plugin_watch_turn() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    UPDATE plugin_watch_deliveries d SET status='delivered' FROM plugin_chat_watches w,external_plugin_actors a
    WHERE d.id=(NEW.metadata->>'plugin_watch_delivery')::uuid AND w.id=d.watch_id AND w.conversation_id=NEW.conversation_id
    AND d.status='pending' AND w.mode='wake' AND NOT w.paused AND a.id=w.actor_id AND a.active AND a.ai_tools_enabled AND a.binary_sha256=w.digest;
    IF NOT FOUND THEN RAISE EXCEPTION 'Watch delivery has already been claimed or cancelled'; END IF;
    RETURN NEW;
END; $$;
CREATE TRIGGER plugin_watch_turn_claim BEFORE INSERT ON ai_messages
 FOR EACH ROW WHEN (NEW.role='user' AND NEW.metadata ? 'plugin_watch_delivery') EXECUTE FUNCTION claim_plugin_watch_turn();
CREATE INDEX plugin_watch_deliveries_due ON plugin_watch_deliveries(next_attempt) WHERE status='pending';
"#).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("DROP TRIGGER plugin_watch_turn_claim ON ai_messages; DROP FUNCTION claim_plugin_watch_turn(); DROP TABLE plugin_watch_deliveries, plugin_watch_events, plugin_chat_watches").await?;
        Ok(())
    }
}
