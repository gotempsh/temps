// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-host sandbox image preparation.
//!
//! The first sandbox on a host pulled its multi-gigabyte image inside the
//! create request. An operator can now prepare the hosts eligible for
//! sandbox placement ahead of time. One row per host records that choice and
//! what came of it:
//!
//! - `node_id`: the worker (`nodes.id`), or `NULL` for the control plane,
//!   which has no `nodes` row. A unique index on `COALESCE(node_id, 0)`
//!   keeps one row per host, the control plane included. Deleting a node
//!   deletes its row.
//! - `keep_prepared`: the operator's policy. When `true`, Temps keeps the
//!   host's default sandbox image pulled, re-pulling it when an upgrade
//!   changes the default and removing the previous one. `false` (the
//!   default, and every host before the operator acts) pulls nothing ahead
//!   of time: the multi-gigabyte download only ever starts from an explicit
//!   "Prepare sandboxes", never silently.
//! - `prepared_image`: the exact reference last prepared on the host, so a
//!   changed default is noticed and the old image can be removed.
//! - `stale_image`: a previously prepared image whose removal is pending
//!   because a container still used it.
//! - `last_error` / `last_error_at`: why the last preparation failed.
//!
//! A table rather than a column on `nodes`, because the control plane is a
//! host too and has no `nodes` row. No backfill: no host is prepared until
//! the operator asks. `down` drops the table; the images already pulled stay
//! on the hosts, where they are ordinary cached images.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS sandbox_image_preparations (
                id SERIAL PRIMARY KEY,
                node_id INTEGER NULL REFERENCES nodes(id) ON DELETE CASCADE,
                keep_prepared BOOLEAN NOT NULL DEFAULT FALSE,
                prepared_image TEXT NULL,
                stale_image TEXT NULL,
                last_error TEXT NULL,
                last_error_at TIMESTAMPTZ NULL,
                created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
            )",
        )
        .await?;
        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS sandbox_image_preparations_host_key \
             ON sandbox_image_preparations ((COALESCE(node_id, 0)))",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS sandbox_image_preparations")
            .await?;
        Ok(())
    }
}
