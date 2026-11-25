// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The node network policy mode `legacy` is now called `off`: let the
//! database hold either name.
//!
//! This is the expand half of the rename. Rows keep `legacy`, and this
//! release still writes `legacy` for `off` (`NodeMode::stored_str`): a
//! binary rolled back after an upgrade, which the schema guard allows,
//! reads only `legacy` and would fail on a row that says `off`. Widening
//! the constraints now lets a later release, once no supported binary
//! predates `off`, write `off` and rewrite the rows while this one can
//! still read them. Columns, defaults and rows are unchanged.
//!
//! `down` turns any `off` back into `legacy` and restores the original
//! constraints.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub const WIDENED: &str = "'off', 'legacy', 'shadow', 'enforce'";
const ORIGINAL: &str = "'legacy', 'shadow', 'enforce'";

pub fn up_sql() -> Vec<String> {
    vec![
        "ALTER TABLE nodes DROP CONSTRAINT IF EXISTS nodes_network_policy_mode_valid".to_string(),
        format!(
            "ALTER TABLE nodes ADD CONSTRAINT nodes_network_policy_mode_valid \
             CHECK (network_policy_mode IN ({WIDENED}))"
        ),
        "ALTER TABLE control_plane_node_settings \
         DROP CONSTRAINT IF EXISTS control_plane_node_settings_network_policy_mode_check"
            .to_string(),
        "ALTER TABLE control_plane_node_settings \
         DROP CONSTRAINT IF EXISTS control_plane_node_settings_network_policy_mode_valid"
            .to_string(),
        format!(
            "ALTER TABLE control_plane_node_settings \
             ADD CONSTRAINT control_plane_node_settings_network_policy_mode_valid \
             CHECK (network_policy_mode IN ({WIDENED}))"
        ),
    ]
}

pub fn down_sql() -> Vec<String> {
    vec![
        "UPDATE nodes SET network_policy_mode = 'legacy' WHERE network_policy_mode = 'off'"
            .to_string(),
        "UPDATE control_plane_node_settings SET network_policy_mode = 'legacy' \
         WHERE network_policy_mode = 'off'"
            .to_string(),
        "ALTER TABLE nodes DROP CONSTRAINT IF EXISTS nodes_network_policy_mode_valid".to_string(),
        format!(
            "ALTER TABLE nodes ADD CONSTRAINT nodes_network_policy_mode_valid \
             CHECK (network_policy_mode IN ({ORIGINAL}))"
        ),
        "ALTER TABLE control_plane_node_settings \
         DROP CONSTRAINT IF EXISTS control_plane_node_settings_network_policy_mode_valid"
            .to_string(),
        format!(
            "ALTER TABLE control_plane_node_settings \
             ADD CONSTRAINT control_plane_node_settings_network_policy_mode_check \
             CHECK (network_policy_mode IN ({ORIGINAL}))"
        ),
    ]
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for statement in up_sql() {
            db.execute_unprepared(&statement).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for statement in down_sql() {
            db.execute_unprepared(&statement).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn up_only_widens_and_rewrites_no_row() {
        let sql = up_sql().join("\n");
        assert!(sql.contains(WIDENED));
        assert!(!sql.contains("UPDATE"), "rows keep `legacy` in this release");
        assert!(!sql.contains("DEFAULT"));
    }

    #[test]
    fn is_registered() {
        let names: Vec<String> = crate::migration::Migrator::migrations()
            .into_iter()
            .map(|m| m.name().to_string())
            .collect();
        assert!(names
            .iter()
            .any(|n| n == "m20261125_000003_network_policy_mode_off"));
    }
}
