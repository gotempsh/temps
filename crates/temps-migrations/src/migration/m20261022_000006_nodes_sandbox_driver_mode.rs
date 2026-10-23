// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-055 Phase 6: sandboxes onto runtime drivers.
//!
//! - `nodes.sandbox_driver_mode` (`legacy` | `shadow` | `enforce`, default
//!   `legacy`): whether sandbox calls for sandboxes on that worker go to the
//!   old providers (`legacy`), go to them while the driver facade computes
//!   and logs what it would have done (`shadow`), or go through the driver
//!   facade (`enforce`), which also forwards sandbox-only operations
//!   (terminals, the retained agent runtime, disk resize, image identity,
//!   harness recovery) to the worker's driver instead of refusing them.
//! - `network_config.control_plane_sandbox_driver_mode`: the same setting for
//!   the control plane (node 0), which has no `nodes` row. It sits next to
//!   the control plane's other per-node settings on the singleton row.
//!
//! Every existing node starts in `legacy`, so nothing changes until an
//! administrator changes a mode through the API.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub const UP_SQL: &str = r#"
ALTER TABLE nodes
    ADD COLUMN IF NOT EXISTS sandbox_driver_mode TEXT NOT NULL DEFAULT 'legacy';
ALTER TABLE nodes DROP CONSTRAINT IF EXISTS nodes_sandbox_driver_mode_valid;
ALTER TABLE nodes
    ADD CONSTRAINT nodes_sandbox_driver_mode_valid
        CHECK (sandbox_driver_mode IN ('legacy', 'shadow', 'enforce'));

ALTER TABLE network_config
    ADD COLUMN IF NOT EXISTS control_plane_sandbox_driver_mode TEXT NOT NULL DEFAULT 'legacy';
ALTER TABLE network_config
    DROP CONSTRAINT IF EXISTS network_config_cp_sandbox_driver_mode_valid;
ALTER TABLE network_config
    ADD CONSTRAINT network_config_cp_sandbox_driver_mode_valid
        CHECK (control_plane_sandbox_driver_mode IN ('legacy', 'shadow', 'enforce'));
"#;

pub const DOWN_SQL: &str = r#"
ALTER TABLE network_config
    DROP CONSTRAINT IF EXISTS network_config_cp_sandbox_driver_mode_valid,
    DROP COLUMN IF EXISTS control_plane_sandbox_driver_mode;
ALTER TABLE nodes
    DROP CONSTRAINT IF EXISTS nodes_sandbox_driver_mode_valid,
    DROP COLUMN IF EXISTS sandbox_driver_mode;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(DOWN_SQL)
            .await?;
        Ok(())
    }
}
