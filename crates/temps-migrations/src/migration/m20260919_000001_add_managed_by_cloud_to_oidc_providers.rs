// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-045 §4: mark an `oidc_providers` row as provisioned by Temps Cloud for
//! console access, and gate it behind a hard admin-only role check.
//!
//! Cloud-managed rows are auto-created when console access is enabled and
//! hold a credential the instance never chose and cannot rotate locally, so
//! they must be excluded from the operator-initiated
//! `PUT`/`DELETE /admin/oidc/providers/{id}` paths that ordinary OIDC
//! providers go through — the same reasoning as
//! `m20260830_000001_add_managed_by_cloud_to_s3_sources`.
//!
//! `admin_only_role_required` is paired with `managed_by_cloud` rather than
//! offered generally: it hard-rejects a login whose resolved role is not
//! `admin` instead of falling through to `default_role`, which only makes
//! sense on a provider whose whole purpose is granting instance-admin access.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE oidc_providers \
             ADD COLUMN IF NOT EXISTS managed_by_cloud BOOLEAN NOT NULL DEFAULT FALSE",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE oidc_providers \
             ADD COLUMN IF NOT EXISTS admin_only_role_required BOOLEAN NOT NULL DEFAULT FALSE",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE oidc_providers DROP COLUMN IF EXISTS admin_only_role_required",
        )
        .await?;
        db.execute_unprepared("ALTER TABLE oidc_providers DROP COLUMN IF EXISTS managed_by_cloud")
            .await?;
        Ok(())
    }
}
