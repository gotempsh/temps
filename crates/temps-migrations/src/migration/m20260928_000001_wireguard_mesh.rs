// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Managed WireGuard mesh as the overlay underlay.
//!
//! The VXLAN overlay needs a private path between nodes. With
//! `network_config.wireguard_enabled`, every node (the control plane
//! included) gets a kernel WireGuard interface and a private mesh address from
//! `wireguard_cidr`, and that address becomes the node's underlay, so nodes
//! that only share public IPs can still form the overlay.
//!
//! Node columns are separate from the legacy `wg_public_key`/`public_endpoint`
//! written by `temps join`: those are part of the registration identity check,
//! while the mesh key is set by the running agent over its own authenticated
//! channel and must survive re-registration.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
ALTER TABLE network_config
    ADD COLUMN IF NOT EXISTS wireguard_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS wireguard_cidr TEXT NOT NULL DEFAULT '10.201.0.0/16',
    ADD COLUMN IF NOT EXISTS wireguard_port INTEGER NOT NULL DEFAULT 51820,
    ADD COLUMN IF NOT EXISTS control_plane_wg_public_key TEXT,
    ADD COLUMN IF NOT EXISTS control_plane_wg_endpoint TEXT;

ALTER TABLE nodes
    ADD COLUMN IF NOT EXISTS mesh_wg_public_key TEXT,
    ADD COLUMN IF NOT EXISTS mesh_wg_endpoint TEXT,
    ADD COLUMN IF NOT EXISTS mesh_wg_address TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS idx_nodes_mesh_wg_address
    ON nodes (mesh_wg_address) WHERE mesh_wg_address IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_nodes_mesh_wg_public_key
    ON nodes (mesh_wg_public_key) WHERE mesh_wg_public_key IS NOT NULL;
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
DROP INDEX IF EXISTS idx_nodes_mesh_wg_public_key;
DROP INDEX IF EXISTS idx_nodes_mesh_wg_address;
ALTER TABLE nodes
    DROP COLUMN IF EXISTS mesh_wg_address,
    DROP COLUMN IF EXISTS mesh_wg_endpoint,
    DROP COLUMN IF EXISTS mesh_wg_public_key;
ALTER TABLE network_config
    DROP COLUMN IF EXISTS control_plane_wg_endpoint,
    DROP COLUMN IF EXISTS control_plane_wg_public_key,
    DROP COLUMN IF EXISTS wireguard_port,
    DROP COLUMN IF EXISTS wireguard_cidr,
    DROP COLUMN IF EXISTS wireguard_enabled;
"#,
            )
            .await?;
        Ok(())
    }
}
