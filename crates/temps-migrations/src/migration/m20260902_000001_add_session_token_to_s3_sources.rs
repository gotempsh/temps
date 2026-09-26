// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Let an `s3_sources` row hold an STS-style *temporary* credential.
//!
//! A prefix-scoped credential (the only way to isolate tenants inside one
//! shared bucket, since R2/S3 API tokens scope to a bucket and never to an
//! object prefix) can only be minted through a temporary-credentials API, and
//! SigV4 rejects the resulting key pair unless a session token travels with it
//! as `X-Amz-Security-Token`. Without somewhere to store that token the
//! credential is unusable.
//!
//! Both columns are nullable with no default, so every existing row — every
//! long-lived credential an operator configured themselves — keeps a NULL
//! session token and behaves exactly as it did. `session_token` holds
//! ciphertext produced by `EncryptionService`, never plaintext, the same as
//! `access_key_id` and `secret_key` on this table.
//!
//! `credentials_expire_at` is not a secret, and is write-only for now: the
//! Cloud enrolment path records it but nothing reads it yet — it is not in
//! `S3SourceResponse` and the rotation loop does not consult it. The column
//! exists so a future console surface can warn about a lapse before an upload
//! fails, and so rotation can eventually be scheduled off the real expiry
//! rather than a fixed interval.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE s3_sources \
             ADD COLUMN IF NOT EXISTS session_token TEXT",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE s3_sources \
             ADD COLUMN IF NOT EXISTS credentials_expire_at TIMESTAMPTZ",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("ALTER TABLE s3_sources DROP COLUMN IF EXISTS session_token")
            .await?;
        db.execute_unprepared("ALTER TABLE s3_sources DROP COLUMN IF EXISTS credentials_expire_at")
            .await?;
        Ok(())
    }
}
