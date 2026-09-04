// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Track a failed S3 lifecycle reconcile attempt so a source stays in the
//! hourly sweep's scope until it converges.
//!
//! `S3LifecycleService::sources_in_scope` narrows the hourly sweep to
//! sources with an enabled backup schedule (plus Cloud-managed ones), so
//! that reconciling a bucket the operator never attached to Temps doesn't
//! burn a paid S3 API call. But that scoping created a gap: if disabling a
//! source's last schedule triggers an immediate reconcile and that reconcile
//! hits a transient S3 error, the source drops out of the sweep's scope in
//! the same moment its retry would have been needed, stranding a stale
//! lifecycle rule in S3 with no other mechanism to clear it.
//!
//! `lifecycle_reconcile_failed_at` closes that gap: it's set whenever a
//! reconcile attempt errors and cleared on the next success, and
//! `sources_in_scope` includes any source where it's non-null regardless of
//! schedule state — so a failed attempt keeps retrying hourly until it
//! actually converges.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE s3_sources \
             ADD COLUMN IF NOT EXISTS lifecycle_reconcile_failed_at TIMESTAMPTZ",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE s3_sources DROP COLUMN IF EXISTS lifecycle_reconcile_failed_at",
        )
        .await?;
        Ok(())
    }
}
