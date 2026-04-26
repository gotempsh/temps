//! Adds compute-network fields used by the multi-host networking layer
//! (`temps-network` crate):
//!
//! 1. `nodes.compute_cidr` (TEXT, nullable) — the per-node CIDR (e.g.
//!    `"172.20.5.0/24"`) Docker uses for container IPs on this node. Other
//!    nodes route this CIDR to us via the configured transport.
//! 2. `nodes.underlay_address` (TEXT, nullable) — the address other nodes
//!    use to reach this one over the underlay (private IP for cloud
//!    private networks, public IP for cross-DC).
//! 3. Partial-unique index on `nodes.compute_cidr` — two nodes must never
//!    share a CIDR, but allowing NULL means upgrading clusters don't have
//!    to backfill before this migration applies.
//! 4. New `network_config` table — single-row cluster configuration owned
//!    by the control plane. Keyed on `id = 1` with a CHECK constraint so
//!    the row is unique by construction.
//!
//! Both columns mirror the existing string-based IP storage pattern used
//! by `nodes.private_address` and `nodes.public_endpoint` rather than
//! introducing PostgreSQL-typed `cidr` / `inet` here. The Rust layer
//! parses to `ipnet::Ipv4Net` / `std::net::IpAddr` at the boundary.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 1. Add columns to `nodes`.
        manager
            .alter_table(
                Table::alter()
                    .table(Nodes::Table)
                    .add_column(ColumnDef::new(Nodes::ComputeCidr).text().null())
                    .add_column(ColumnDef::new(Nodes::UnderlayAddress).text().null())
                    .to_owned(),
            )
            .await?;

        // 2. Partial-unique index on compute_cidr (NULLs ignored).
        let db = manager.get_connection();
        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_nodes_compute_cidr_uniq \
             ON nodes (compute_cidr) WHERE compute_cidr IS NOT NULL",
        )
        .await?;

        // 3. Cluster-singleton network_config table.
        manager
            .create_table(
                Table::create()
                    .table(NetworkConfig::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(NetworkConfig::Id)
                            .integer()
                            .not_null()
                            .primary_key()
                            .default(1),
                    )
                    // Pool we slice into per-node CIDRs (e.g. 172.20.0.0/16).
                    .col(
                        ColumnDef::new(NetworkConfig::ComputePoolCidr)
                            .text()
                            .not_null()
                            .default("172.20.0.0/16"),
                    )
                    // Prefix length per allocated /n (default /24 → 256 hosts/node).
                    .col(
                        ColumnDef::new(NetworkConfig::SubnetPrefixLen)
                            .integer()
                            .not_null()
                            .default(24),
                    )
                    // Transport mode: 'vxlan' or 'native'.
                    .col(
                        ColumnDef::new(NetworkConfig::Transport)
                            .text()
                            .not_null()
                            .default("vxlan"),
                    )
                    .col(
                        ColumnDef::new(NetworkConfig::VxlanVni)
                            .integer()
                            .not_null()
                            .default(42),
                    )
                    .col(
                        ColumnDef::new(NetworkConfig::VxlanPort)
                            .integer()
                            .not_null()
                            .default(4789),
                    )
                    .col(
                        ColumnDef::new(NetworkConfig::UnderlayMtu)
                            .integer()
                            .not_null()
                            .default(1500),
                    )
                    .col(
                        ColumnDef::new(NetworkConfig::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // Enforce single-row invariant + valid transport via CHECK.
        db.execute_unprepared(
            "ALTER TABLE network_config \
             ADD CONSTRAINT network_config_singleton CHECK (id = 1)",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE network_config \
             ADD CONSTRAINT network_config_transport_valid \
             CHECK (transport IN ('vxlan', 'native'))",
        )
        .await?;

        // Seed the singleton row using the column defaults.
        db.execute_unprepared(
            "INSERT INTO network_config (id) VALUES (1) ON CONFLICT (id) DO NOTHING",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(NetworkConfig::Table).to_owned())
            .await?;

        let db = manager.get_connection();
        db.execute_unprepared("DROP INDEX IF EXISTS idx_nodes_compute_cidr_uniq")
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Nodes::Table)
                    .drop_column(Nodes::UnderlayAddress)
                    .drop_column(Nodes::ComputeCidr)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Nodes {
    Table,
    ComputeCidr,
    UnderlayAddress,
}

#[derive(DeriveIden)]
enum NetworkConfig {
    Table,
    Id,
    ComputePoolCidr,
    SubnetPrefixLen,
    Transport,
    VxlanVni,
    VxlanPort,
    UnderlayMtu,
    UpdatedAt,
}
