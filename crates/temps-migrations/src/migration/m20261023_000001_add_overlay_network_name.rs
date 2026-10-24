// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Make the overlay network name configurable (ADR 059).
//!
//! `network_config.docker_network_name` is the cluster's overlay network name,
//! `temps0` as before. `nodes.overlay_network_name` overrides it for one node:
//! a host that already has a network of the cluster's name (often left by
//! another cluster it belonged to) gets its own name instead of having that
//! network removed. Both are checked against the alphabet and length the
//! derived interface names (`br-<name>`, `vxlan-<name>`) need.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Starts with a lowercase letter, then lowercase letters, digits, `-`, `_`;
/// at most 9 characters so `vxlan-<name>` fits Linux's 15-byte limit. Kept
/// in step with `temps_network::overlay_name::validate_overlay_network_name`.
const NAME_PATTERN: &str = "^[a-z][a-z0-9_-]{0,8}$";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(NetworkConfig::Table)
                    .add_column(
                        ColumnDef::new(NetworkConfig::DockerNetworkName)
                            .string()
                            .not_null()
                            .default("temps0"),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Nodes::Table)
                    .add_column(ColumnDef::new(Nodes::OverlayNetworkName).string().null())
                    .to_owned(),
            )
            .await?;
        let db = manager.get_connection();
        db.execute_unprepared(&format!(
            "ALTER TABLE network_config ADD CONSTRAINT network_config_docker_network_name_check \
             CHECK (docker_network_name ~ '{NAME_PATTERN}')"
        ))
        .await?;
        db.execute_unprepared(&format!(
            "ALTER TABLE nodes ADD CONSTRAINT nodes_overlay_network_name_check \
             CHECK (overlay_network_name IS NULL OR overlay_network_name ~ '{NAME_PATTERN}')"
        ))
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Nodes::Table)
                    .drop_column(Nodes::OverlayNetworkName)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(NetworkConfig::Table)
                    .drop_column(NetworkConfig::DockerNetworkName)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum NetworkConfig {
    Table,
    DockerNetworkName,
}

#[derive(DeriveIden)]
enum Nodes {
    Table,
    OverlayNetworkName,
}
