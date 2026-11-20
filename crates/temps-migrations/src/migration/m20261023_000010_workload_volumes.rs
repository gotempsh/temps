// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-055 Phase 9: volumes as first-class resources (§12).
//!
//! - `workload_volumes`: one row per volume. The owner is the product
//!   object (`environment/3`, `external_service/7`), so a volume outlives
//!   the revisions that mount it. `node_id` is the node holding the data:
//!   `NULL` until it is first created, `0` for the control plane (no foreign
//!   key, as everywhere node 0 is used). Deletion is soft; `deleted_at`
//!   frees the name, and `retain_on_delete` keeps the backing.
//! - `workload_volume_attachments`: where the owner's workloads mount a
//!   volume. A volume is mounted at one path.
//!
//! Nothing reads either table until a volume is added: an environment with
//! no attachment deploys exactly as before.

use sea_orm_migration::prelude::*;

/// The two tables and their indexes. Public so the plane's tests can create
/// exactly these in an isolated schema.
pub const VOLUME_TABLES_SQL: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS workload_volumes (
        id BIGSERIAL PRIMARY KEY,
        owner_kind TEXT NOT NULL,
        owner_id BIGINT NOT NULL,
        name TEXT NOT NULL,
        backing TEXT NOT NULL,
        backing_ref TEXT NOT NULL,
        node_id INTEGER,
        size_mb BIGINT,
        retain_on_delete BOOLEAN NOT NULL DEFAULT FALSE,
        created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
        updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
        deleted_at TIMESTAMPTZ,
        CONSTRAINT workload_volumes_backing_valid
            CHECK (backing IN ('named', 'block_image', 'preopened_dir')),
        CONSTRAINT workload_volumes_name_valid
            CHECK (name ~ '^[a-z0-9][a-z0-9_.-]{1,63}$'),
        CONSTRAINT workload_volumes_size_valid CHECK (size_mb IS NULL OR size_mb > 0),
        CONSTRAINT workload_volumes_node_valid CHECK (node_id IS NULL OR node_id >= 0)
    )",
    "CREATE UNIQUE INDEX IF NOT EXISTS workload_volumes_owner_name_live
        ON workload_volumes (owner_kind, owner_id, name) WHERE deleted_at IS NULL",
    "CREATE INDEX IF NOT EXISTS workload_volumes_node
        ON workload_volumes (node_id) WHERE deleted_at IS NULL",
    "CREATE TABLE IF NOT EXISTS workload_volume_attachments (
        id BIGSERIAL PRIMARY KEY,
        volume_id BIGINT NOT NULL REFERENCES workload_volumes(id) ON DELETE CASCADE,
        mount_path TEXT NOT NULL,
        read_only BOOLEAN NOT NULL DEFAULT FALSE,
        created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
        updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
        CONSTRAINT workload_volume_attachments_path_absolute CHECK (mount_path LIKE '/%'),
        UNIQUE (volume_id)
    )",
];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        for statement in VOLUME_TABLES_SQL {
            conn.execute_unprepared(statement).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DROP TABLE IF EXISTS workload_volume_attachments;
                 DROP TABLE IF EXISTS workload_volumes;",
            )
            .await?;
        Ok(())
    }
}
