// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pairings for workers that join outbound only (ADR 055).
//!
//! A worker that runs `temps join <url> <token>` with no address sends its
//! WireGuard key over HTTPS and dials the control plane, so its pairing has
//! no node endpoint: `node_pairings.node_endpoint` becomes nullable. A pairing
//! the control plane still has to dial (`waiting`) always has one, which the
//! check constraint keeps true.

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
ALTER TABLE node_pairings ALTER COLUMN node_endpoint DROP NOT NULL;
ALTER TABLE node_pairings DROP CONSTRAINT IF EXISTS node_pairings_waiting_has_endpoint;
ALTER TABLE node_pairings ADD CONSTRAINT node_pairings_waiting_has_endpoint
    CHECK (status <> 'waiting' OR node_endpoint IS NOT NULL);
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Outbound pairings cannot be represented without an endpoint; they
        // are dropped (pending ones simply stop being peered, finished ones
        // only lose their history row).
        manager
            .get_connection()
            .execute_unprepared(
                r#"
ALTER TABLE node_pairings DROP CONSTRAINT IF EXISTS node_pairings_waiting_has_endpoint;
DELETE FROM node_pairings WHERE node_endpoint IS NULL;
ALTER TABLE node_pairings ALTER COLUMN node_endpoint SET NOT NULL;
"#,
            )
            .await?;
        Ok(())
    }
}
