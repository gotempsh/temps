// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One-paste node pairing (ADR 048 D2b).
//!
//! A pairing reserves a mesh address for a node the control plane will dial
//! at an operator-entered endpoint, holds the pairing secret (encrypted) the
//! control plane uses to prove itself to the node, and records the node's
//! WireGuard public key once the exchange succeeds. The control plane peers
//! with a pairing that has a key, so the node can register over the mesh;
//! registering with the pairing's enrollment token links it to the node,
//! which then takes over the key and address.
//!
//! `network_config.node_api_port` is the TCP port the control plane serves
//! the node API on at its mesh address; NULL means the mesh port number.

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
    ADD COLUMN IF NOT EXISTS node_api_port INTEGER;

CREATE TABLE IF NOT EXISTS node_pairings (
    id SERIAL PRIMARY KEY,
    pairing_id TEXT NOT NULL,
    name TEXT NOT NULL,
    node_endpoint TEXT NOT NULL,
    mesh_address TEXT NOT NULL,
    secret_encrypted TEXT NOT NULL,
    -- RESTRICT: tokens are revoked, never deleted; deleting one must not
    -- erase the pairing history that points at it.
    enrollment_token_id INTEGER NOT NULL
        REFERENCES node_enrollment_tokens (id) ON DELETE RESTRICT,
    public_key TEXT,
    status TEXT NOT NULL DEFAULT 'waiting',
    last_error TEXT,
    last_attempt_at TIMESTAMPTZ,
    key_received_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ NOT NULL,
    node_id INTEGER REFERENCES nodes (id) ON DELETE SET NULL,
    created_by_user_id INTEGER,
    -- A control-plane process dialing this pairing holds it until then, so
    -- two processes sharing the database do not dial the same node.
    dialing_until TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT node_pairings_status_check
        CHECK (status IN ('waiting', 'key_received', 'completed', 'expired', 'cancelled'))
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_node_pairings_pairing_id
    ON node_pairings (pairing_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_node_pairings_enrollment_token
    ON node_pairings (enrollment_token_id);
-- Only pending pairings hold an address or a key; finished ones keep theirs
-- for history without blocking reuse.
CREATE UNIQUE INDEX IF NOT EXISTS idx_node_pairings_pending_mesh_address
    ON node_pairings (mesh_address) WHERE status IN ('waiting', 'key_received');
CREATE UNIQUE INDEX IF NOT EXISTS idx_node_pairings_pending_public_key
    ON node_pairings (public_key)
    WHERE public_key IS NOT NULL AND status IN ('waiting', 'key_received');
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
DROP TABLE IF EXISTS node_pairings;
ALTER TABLE network_config DROP COLUMN IF EXISTS node_api_port;
"#,
            )
            .await?;
        Ok(())
    }
}
