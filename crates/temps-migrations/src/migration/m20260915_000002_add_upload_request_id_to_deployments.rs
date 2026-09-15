// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Give local-image-upload deployments a client-supplied correlation ID.
//!
//! The CLI streams a Docker archive and then waits for the server to import
//! it and create a deployment. If that wait times out, the archive has
//! already been fully received and the server may still finish the import
//! and create a deployment after the CLI gives up — reporting a bare failure
//! at that point would invite a retry that deploys the same image twice
//! (there was no way to tell a genuinely new attempt from a slow-but-successful
//! one). `upload_request_id` is a UUID the CLI generates once per upload
//! attempt and sends with the request; the server stores it on the resulting
//! deployment so a timed-out client can look up the exact deployment this
//! upload produced instead of guessing from timing. The partial unique index
//! also lets the server reject a literal retry (same ID resubmitted) instead
//! of importing and deploying the image a second time.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE deployments \
             ADD COLUMN IF NOT EXISTS upload_request_id VARCHAR NULL",
        )
        .await?;
        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_deployments_upload_request_id \
             ON deployments (project_id, environment_id, upload_request_id) \
             WHERE upload_request_id IS NOT NULL",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP INDEX IF EXISTS idx_deployments_upload_request_id")
            .await?;
        db.execute_unprepared("ALTER TABLE deployments DROP COLUMN IF EXISTS upload_request_id")
            .await?;
        Ok(())
    }
}
