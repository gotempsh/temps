// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Ingress connectors (ADR 058): a way into the cluster that is not a public
//! address, such as a Cloudflare Tunnel.
//!
//! - `ingress_connectors`: one tunnel at its provider, the credential it was
//!   created with (through a DNS provider row) and the connector secret the
//!   nodes run it with, encrypted.
//! - `ingress_connector_nodes`: which nodes run a replica, and what each
//!   reported last.
//! - `ingress_publications`: the hostnames published through a connector, so
//!   reconciliation knows what it created and can remove exactly that.

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
CREATE TABLE IF NOT EXISTS ingress_connectors (
    id SERIAL PRIMARY KEY,
    name VARCHAR(100) NOT NULL UNIQUE,
    provider VARCHAR(50) NOT NULL,
    dns_provider_id INTEGER REFERENCES dns_providers(id) ON DELETE SET NULL,
    domain VARCHAR(253) NOT NULL,
    external_id VARCHAR(255),
    settings JSONB NOT NULL DEFAULT '{}'::jsonb,
    secret_encrypted TEXT,
    edge_port INTEGER NOT NULL DEFAULT 7480,
    status VARCHAR(30) NOT NULL DEFAULT 'pending',
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS ingress_connector_nodes (
    id SERIAL PRIMARY KEY,
    connector_id INTEGER NOT NULL REFERENCES ingress_connectors(id) ON DELETE CASCADE,
    node_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    driver VARCHAR(30),
    state VARCHAR(30) NOT NULL DEFAULT 'pending',
    ready_connections INTEGER,
    connector_ref VARCHAR(255),
    last_error TEXT,
    reported_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (connector_id, node_id)
);
CREATE INDEX IF NOT EXISTS idx_ingress_connector_nodes_node
    ON ingress_connector_nodes (node_id);

CREATE TABLE IF NOT EXISTS ingress_publications (
    id SERIAL PRIMARY KEY,
    connector_id INTEGER NOT NULL REFERENCES ingress_connectors(id) ON DELETE CASCADE,
    hostname VARCHAR(253) NOT NULL UNIQUE,
    state VARCHAR(30) NOT NULL DEFAULT 'pending',
    last_error TEXT,
    published_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
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
DROP TABLE IF EXISTS ingress_publications;
DROP TABLE IF EXISTS ingress_connector_nodes;
DROP TABLE IF EXISTS ingress_connectors;
"#,
            )
            .await?;
        Ok(())
    }
}
