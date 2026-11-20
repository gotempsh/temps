// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Attachments sent with error events and feedback (ADR 111, D4).
//!
//! The bytes live in the project's blob storage; `error_attachments` holds
//! one row per stored file: who it belongs to (`owner_kind` `error` with the
//! error event id and timestamp, or `feedback` with the feedback id), its
//! name, type and size. A row is written before its object and marked
//! `stored_at` once the object is in place, so a crash between the two
//! leaves a row the sweeper removes, never an unreferenced object.
//!
//! `error_attachment_usage` holds one row per project that has attachments:
//! the bytes and files stored, and the per-project quota, which the admin
//! can change. Neither table has a foreign key to `projects`: a deleted
//! project's rows stay until the sweeper has deleted their objects, which a
//! cascade would leave behind; a soft-deleted project counts as deleted. Error events are a hypertable, so the link to
//! an event is its id and timestamp, checked by the sweeper.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS error_attachments ( \
                 id BIGSERIAL PRIMARY KEY, \
                 project_id INTEGER NOT NULL, \
                 owner_kind TEXT NOT NULL CHECK (owner_kind IN ('error', 'feedback')), \
                 owner_id BIGINT NOT NULL, \
                 ordinal SMALLINT NOT NULL, \
                 error_group_id INTEGER, \
                 event_time TIMESTAMPTZ, \
                 filename TEXT NOT NULL, \
                 content_type TEXT NOT NULL, \
                 attachment_type TEXT NOT NULL, \
                 size_bytes BIGINT NOT NULL CHECK (size_bytes >= 0), \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                 stored_at TIMESTAMPTZ, \
                 UNIQUE (project_id, owner_kind, owner_id, ordinal))",
        )
        .await?;
        // The sweeper walks error attachments oldest event first.
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_error_attachments_event_time \
             ON error_attachments (event_time, id) WHERE owner_kind = 'error'",
        )
        .await?;
        // Feedback attachments are found by their feedback, to sweep those
        // whose feedback is gone.
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_error_attachments_feedback \
             ON error_attachments (owner_id) WHERE owner_kind = 'feedback'",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_error_attachments_unstored \
             ON error_attachments (created_at) WHERE stored_at IS NULL",
        )
        .await?;
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS error_attachment_usage ( \
                 project_id INTEGER PRIMARY KEY, \
                 bytes BIGINT NOT NULL DEFAULT 0 CHECK (bytes >= 0), \
                 files BIGINT NOT NULL DEFAULT 0 CHECK (files >= 0), \
                 quota_bytes BIGINT NOT NULL DEFAULT 268435456 CHECK (quota_bytes >= 0), \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW())",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP TABLE IF EXISTS error_attachments")
            .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS error_attachment_usage")
            .await?;
        Ok(())
    }
}
