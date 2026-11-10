// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Give each cluster its own VXLAN network ID (ADR 062).
//!
//! `network_config.vxlan_vni` was seeded 42 for every cluster, and the kernel
//! allows one VXLAN device per network ID and port on a host, so two
//! clusters' overlays could never share one.
//!
//! Only a brand-new database gets a random ID in `4096..=16777215`: no user,
//! no node and no control-plane overlay allocation yet, which is the state
//! right after the migrations of a first boot (no migration creates a user).
//! Every existing install keeps the ID it has, 42 unless an operator changed
//! it, whether or not it ever added a worker: nothing about a running
//! cluster's network changes until an operator asks for a new ID.
//!
//! `nodes.overlay_vxlan_vni` is the network ID a node's agent reports it
//! follows (NULL: never reported, an agent older than this). A change of the
//! cluster's ID is refused while any node has not reported one.
//!
//! `down` drops the column and the checks and puts the cluster back on 42:
//! binaries older than this run every agent's overlay on 42, whatever the
//! row says.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// The largest VXLAN network ID (24 bits). Kept in step with
/// `temps_network::MAX_VXLAN_VNI`.
const MAX_VNI: i32 = 0x00FF_FFFF;
/// The smallest randomly chosen ID: `temps_network::MIN_RANDOM_VXLAN_VNI`.
const MIN_RANDOM_VNI: i32 = 4096;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE nodes ADD COLUMN IF NOT EXISTS overlay_vxlan_vni integer NULL",
        )
        .await?;
        db.execute_unprepared(&format!(
            "ALTER TABLE nodes DROP CONSTRAINT IF EXISTS nodes_overlay_vxlan_vni_check; \
             ALTER TABLE nodes ADD CONSTRAINT nodes_overlay_vxlan_vni_check \
             CHECK (overlay_vxlan_vni IS NULL OR overlay_vxlan_vni BETWEEN 1 AND {MAX_VNI})"
        ))
        .await?;
        db.execute_unprepared(&format!(
            "ALTER TABLE network_config DROP CONSTRAINT IF EXISTS network_config_vxlan_vni_check; \
             ALTER TABLE network_config ADD CONSTRAINT network_config_vxlan_vni_check \
             CHECK (vxlan_vni BETWEEN 1 AND {MAX_VNI})"
        ))
        .await?;
        // A brand-new database only: no user, no node, no control-plane
        // allocation. Any install that already ran keeps its network ID.
        db.execute_unprepared(&format!(
            "UPDATE network_config \
             SET vxlan_vni = {MIN_RANDOM_VNI} + floor(random() * ({MAX_VNI} - {MIN_RANDOM_VNI} + 1))::int \
             WHERE id = 1 AND vxlan_vni = 42 AND control_plane_compute_cidr IS NULL \
             AND NOT EXISTS (SELECT 1 FROM nodes) \
             AND NOT EXISTS (SELECT 1 FROM users)"
        ))
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE network_config DROP CONSTRAINT IF EXISTS network_config_vxlan_vni_check",
        )
        .await?;
        db.execute_unprepared("UPDATE network_config SET vxlan_vni = 42 WHERE id = 1")
            .await?;
        db.execute_unprepared("ALTER TABLE nodes DROP COLUMN IF EXISTS overlay_vxlan_vni")
            .await?;
        Ok(())
    }
}
