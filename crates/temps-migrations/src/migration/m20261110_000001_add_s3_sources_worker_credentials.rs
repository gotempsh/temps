// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Whether a worker node may receive an S3 source's own credential when the
//! source cannot mint a scoped one (ADR-093).
//!
//! A worker job (a backup or restore of a service on a worker node) gets a
//! credential the control plane derives for it: limited to the job's key
//! prefixes, to reading or writing, and short-lived. A store that cannot
//! derive one (no STS) refuses worker jobs unless the operator accepts
//! handing the worker the source's full credential, per source.
//!
//! `scoped` (the default for every existing and new row): never send the
//! source's credential to a worker. `allow_full`: send it when scoping is
//! impossible, with a warning on every job. A CHECK keeps the column to
//! those two values; the entity reads it as text.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE s3_sources \
             ADD COLUMN IF NOT EXISTS worker_credentials TEXT NOT NULL DEFAULT 'scoped'",
        )
        .await?;
        db.execute_unprepared(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT 1 FROM pg_constraint \
                              WHERE conname = 's3_sources_worker_credentials_check') THEN \
                 ALTER TABLE s3_sources ADD CONSTRAINT s3_sources_worker_credentials_check \
                   CHECK (worker_credentials IN ('scoped', 'allow_full')); \
               END IF; \
             END $$",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE s3_sources DROP CONSTRAINT IF EXISTS s3_sources_worker_credentials_check",
        )
        .await?;
        db.execute_unprepared("ALTER TABLE s3_sources DROP COLUMN IF EXISTS worker_credentials")
            .await?;
        Ok(())
    }
}
