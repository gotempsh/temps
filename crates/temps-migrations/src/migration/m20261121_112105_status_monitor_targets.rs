// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Monitors for targets outside Temps, with typed checks (ADR 108 D3, D4).
//!
//! - `check_kind`: `environment` (the existing probe of a Temps
//!   environment) or `http`, `tcp`, `tls`, `dns` for an explicit target.
//! - `target`: the URL, `host:port` or hostname such a monitor checks.
//! - `check_spec`: the kind's typed settings (assertions, thresholds) and
//!   the names of its request headers. Never holds a header value.
//! - `request_encrypted`: the HTTP request headers and body, encrypted at
//!   rest, because they often carry credentials.
//!
//! The constraint keeps the two shapes apart: an environment monitor has no
//! target, and a target monitor has no environment and always a spec.
//! Existing rows all become `environment` monitors and keep working.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE status_monitors \
             ADD COLUMN IF NOT EXISTS check_kind TEXT NOT NULL DEFAULT 'environment', \
             ADD COLUMN IF NOT EXISTS target TEXT, \
             ADD COLUMN IF NOT EXISTS check_spec JSONB, \
             ADD COLUMN IF NOT EXISTS request_encrypted TEXT",
        )
        .await?;
        db.execute_unprepared(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT 1 FROM pg_constraint \
                              WHERE conname = 'status_monitors_check_target') THEN \
                 ALTER TABLE status_monitors ADD CONSTRAINT status_monitors_check_target CHECK ( \
                   (check_kind = 'environment' AND target IS NULL AND request_encrypted IS NULL) \
                   OR (check_kind IN ('http', 'tcp', 'tls', 'dns') \
                       AND target IS NOT NULL AND environment_id IS NULL \
                       AND check_spec IS NOT NULL)); \
               END IF; \
             END $$",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "DELETE FROM status_monitors WHERE check_kind <> 'environment'",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE status_monitors \
             DROP CONSTRAINT IF EXISTS status_monitors_check_target, \
             DROP COLUMN IF EXISTS request_encrypted, \
             DROP COLUMN IF EXISTS check_spec, \
             DROP COLUMN IF EXISTS target, \
             DROP COLUMN IF EXISTS check_kind",
        )
        .await?;
        Ok(())
    }
}
