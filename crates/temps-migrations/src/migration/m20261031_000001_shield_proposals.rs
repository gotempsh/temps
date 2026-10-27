// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What fleet learning finds is a proposal (ADR 066, D9).
//!
//! A `shield_observed` row no longer applies by itself: it waits as
//! `pending` until an instance admin approves or rejects it, and only
//! approved rows go into a snapshot. Rows learned before this migration
//! become pending too, so nothing learned keeps applying unreviewed.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE shield_observed
    ADD COLUMN IF NOT EXISTS status VARCHAR(16) NOT NULL DEFAULT 'pending',
    ADD COLUMN IF NOT EXISTS decided_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS decided_by INTEGER REFERENCES users(id) ON DELETE SET NULL;

DO $$ BEGIN
    ALTER TABLE shield_observed ADD CONSTRAINT shield_observed_status_check
        CHECK (status IN ('pending', 'approved', 'rejected'));
EXCEPTION WHEN duplicate_object THEN NULL;
END $$;

CREATE INDEX IF NOT EXISTS shield_observed_status ON shield_observed (status, last_seen DESC);
"#;

const DOWN: &str = r#"
DROP INDEX IF EXISTS shield_observed_status;
ALTER TABLE shield_observed
    DROP CONSTRAINT IF EXISTS shield_observed_status_check,
    DROP COLUMN IF EXISTS decided_by,
    DROP COLUMN IF EXISTS decided_at,
    DROP COLUMN IF EXISTS status;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}
