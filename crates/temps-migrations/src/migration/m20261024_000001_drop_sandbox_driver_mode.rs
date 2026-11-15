// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-055 Phase 6: the sandbox driver facade is the only sandbox path.
//!
//! `m20261022_000006_nodes_sandbox_driver_mode` added a per-node migration
//! switch (`legacy` | `shadow` | `enforce`) between the old sandbox providers
//! and the driver facade. The facade now serves every sandbox on every node,
//! so the switch is gone and so are its columns:
//!
//! - `nodes.sandbox_driver_mode`;
//! - `network_config.control_plane_sandbox_driver_mode` (node 0's copy).
//!
//! Nothing reads either column any more, so dropping them changes no
//! behaviour. `down` re-adds both, with their CHECK constraints, at the
//! `legacy` default the original migration used.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub const UP_SQL: &str = r#"
ALTER TABLE network_config
    DROP CONSTRAINT IF EXISTS network_config_cp_sandbox_driver_mode_valid,
    DROP COLUMN IF EXISTS control_plane_sandbox_driver_mode;
ALTER TABLE nodes
    DROP CONSTRAINT IF EXISTS nodes_sandbox_driver_mode_valid,
    DROP COLUMN IF EXISTS sandbox_driver_mode;
"#;

pub const DOWN_SQL: &str = r#"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn up_drops_both_columns_and_their_constraints() {
        for fragment in [
            "DROP COLUMN IF EXISTS sandbox_driver_mode",
            "DROP COLUMN IF EXISTS control_plane_sandbox_driver_mode",
            "DROP CONSTRAINT IF EXISTS nodes_sandbox_driver_mode_valid",
            "DROP CONSTRAINT IF EXISTS network_config_cp_sandbox_driver_mode_valid",
        ] {
            assert!(UP_SQL.contains(fragment), "up is missing `{fragment}`");
        }
        assert!(!UP_SQL.contains("ADD COLUMN"), "up must only drop");
    }

    #[test]
    fn down_restores_both_columns_at_the_legacy_default() {
        for fragment in [
            "ADD COLUMN IF NOT EXISTS sandbox_driver_mode TEXT NOT NULL DEFAULT 'legacy'",
            "ADD COLUMN IF NOT EXISTS control_plane_sandbox_driver_mode TEXT NOT NULL DEFAULT 'legacy'",
            "ADD CONSTRAINT nodes_sandbox_driver_mode_valid",
            "ADD CONSTRAINT network_config_cp_sandbox_driver_mode_valid",
        ] {
            assert!(DOWN_SQL.contains(fragment), "down is missing `{fragment}`");
        }
    }

    #[test]
    fn is_registered_after_the_migration_that_added_the_columns() {
        let names: Vec<String> = crate::migration::Migrator::migrations()
            .into_iter()
            .map(|m| m.name().to_string())
            .collect();
        let added = names
            .iter()
            .position(|n| n == "m20261022_000006_nodes_sandbox_driver_mode");
        let dropped = names
            .iter()
            .position(|n| n == "m20261024_000001_drop_sandbox_driver_mode");
        match (added, dropped) {
            (Some(added), Some(dropped)) => assert!(added < dropped),
            other => panic!("both migrations must be registered, got {other:?}"),
        }
    }
}
