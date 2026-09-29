// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Mesh hubs (ADR 048 D4).
//!
//! - `network_config.mesh_hub_node_id` / `mesh_hub_control_plane`: the member
//!   the operator made the hub, which relays traffic between members that
//!   cannot reach each other. At most one is set.
//! - `node_mesh_reports`: each node's latest report of when it last completed
//!   a WireGuard handshake with each peer, by public key.
//! - `mesh_links`: the control plane's routing decision per pair of members
//!   (by public key, `key_a < key_b`): direct, or through the hub because the
//!   direct link never came up. `endpoint_a`/`endpoint_b` are the endpoints
//!   the decision was made with; a change sends the pair back to direct.

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
    ADD COLUMN IF NOT EXISTS mesh_hub_node_id INTEGER REFERENCES nodes (id) ON DELETE SET NULL,
    ADD COLUMN IF NOT EXISTS mesh_hub_control_plane BOOLEAN NOT NULL DEFAULT FALSE;

CREATE TABLE IF NOT EXISTS node_mesh_reports (
    node_id INTEGER PRIMARY KEY REFERENCES nodes (id) ON DELETE CASCADE,
    handshakes JSONB NOT NULL DEFAULT '{}'::jsonb,
    reported_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS mesh_links (
    key_a TEXT NOT NULL,
    key_b TEXT NOT NULL,
    via_hub BOOLEAN NOT NULL DEFAULT FALSE,
    since TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    endpoint_a TEXT,
    endpoint_b TEXT,
    PRIMARY KEY (key_a, key_b),
    CONSTRAINT mesh_links_ordered CHECK (key_a < key_b)
);
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
DROP TABLE IF EXISTS mesh_links;
DROP TABLE IF EXISTS node_mesh_reports;
ALTER TABLE network_config
    DROP COLUMN IF EXISTS mesh_hub_control_plane,
    DROP COLUMN IF EXISTS mesh_hub_node_id;
"#,
            )
            .await?;
        Ok(())
    }
}
