// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `ingress_connectors.host_suffix`: name a tunnel's environment hosts
//! `<label>-<suffix>.<domain>` instead of `<label>.<domain>` (ADR 068 D3).
//!
//! A tunnel kept to part of a zone used to take a subdomain as its domain
//! (`lab.example.com`), so its hosts were two levels deep
//! (`shop-production.lab.example.com`). Cloudflare's free Universal SSL
//! certificate covers only one level below the zone, so those needed
//! Advanced Certificate Manager. With a suffix the domain is the zone and
//! the hosts stay one level deep (`shop-production-lab.example.com`).
//!
//! Nullable, no backfill: existing tunnels keep `<label>.<domain>`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE ingress_connectors ADD COLUMN IF NOT EXISTS host_suffix TEXT",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE ingress_connectors DROP COLUMN IF EXISTS host_suffix")
            .await?;
        Ok(())
    }
}
