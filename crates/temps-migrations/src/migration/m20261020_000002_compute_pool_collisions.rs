// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A compute pool that avoids collisions and can be changed (ADR 056).
//!
//! - Moves the `network_config.compute_pool_cidr` column default to
//!   `10.202.0.0/16`. The singleton row is seeded when the table is created,
//!   so this never rewrites an existing cluster's pool: existing installs
//!   keep the pool they stored, and no row is touched here.
//! - Adds per-node overlay health and collision data to `nodes`, all nullable
//!   so an agent older than this change reads as "never reported":
//!   - `occupied_ranges`: IPv4 ranges the node already uses (host routes on
//!     non-Temps devices, other Docker networks), reported by the agent.
//!   - `overlay_status` (`ready` / `unavailable`) and `overlay_last_error`.
//!   - `overlay_ready_cidr`: the allocation the node last reported the
//!     overlay ready on. An allocation equal to it is "in use" and locks the
//!     pool; any other allocation may be re-assigned.
//!   - `overlay_remediation`: the last automatic re-allocation and why.

use sea_orm_migration::prelude::*;
use temps_entities::network_config::{DEFAULT_COMPUTE_POOL_CIDR, LEGACY_COMPUTE_POOL_CIDR};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(&format!(
            "ALTER TABLE network_config ALTER COLUMN compute_pool_cidr SET DEFAULT '{DEFAULT_COMPUTE_POOL_CIDR}'"
        ))
        .await?;
        conn.execute_unprepared(
            r#"
ALTER TABLE nodes
    ADD COLUMN IF NOT EXISTS occupied_ranges JSONB,
    ADD COLUMN IF NOT EXISTS overlay_status TEXT,
    ADD COLUMN IF NOT EXISTS overlay_last_error TEXT,
    ADD COLUMN IF NOT EXISTS overlay_ready_cidr TEXT,
    ADD COLUMN IF NOT EXISTS overlay_remediation TEXT;
"#,
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(
            r#"
ALTER TABLE nodes
    DROP COLUMN IF EXISTS occupied_ranges,
    DROP COLUMN IF EXISTS overlay_status,
    DROP COLUMN IF EXISTS overlay_last_error,
    DROP COLUMN IF EXISTS overlay_ready_cidr,
    DROP COLUMN IF EXISTS overlay_remediation;
"#,
        )
        .await?;
        conn.execute_unprepared(&format!(
            "ALTER TABLE network_config ALTER COLUMN compute_pool_cidr SET DEFAULT '{LEGACY_COMPUTE_POOL_CIDR}'"
        ))
        .await?;
        Ok(())
    }
}
