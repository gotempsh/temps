// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Record which user a deployment's image runs as, so the console can warn
//! about deployments whose process is root (uid 0).
//!
//! Project containers run as the image's own `USER`; an image that declares
//! none runs as root. The deploy pipeline resolves that once per deployment,
//! after the image exists and before any replica starts, against the image's
//! own `/etc/passwd` and `/etc/group`
//! (`crates/temps-deployments/src/jobs/deploy_image/image_user.rs`), and
//! writes the answer here. Reading it is a column read: nothing inspects an
//! image on an API request.
//!
//! - `runs_as_root`: `true` only when the image's user resolved to uid 0.
//!   Existing rows default to `false`: they were never checked, and an
//!   unchecked deployment is not reported as root.
//! - `image_user`: the `USER` the image declares, as written (`nodejs`,
//!   `10001:10001`); `NULL` when it declares none or was not checked. Free
//!   text from the image, bounded at write time.
//!
//! Both are additive, so the previous binary keeps working on this schema.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE deployments \
             ADD COLUMN IF NOT EXISTS runs_as_root BOOLEAN NOT NULL DEFAULT false, \
             ADD COLUMN IF NOT EXISTS image_user TEXT",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE deployments \
             DROP COLUMN IF EXISTS image_user, \
             DROP COLUMN IF EXISTS runs_as_root",
        )
        .await?;
        Ok(())
    }
}
