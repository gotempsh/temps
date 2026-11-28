// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bump `network_policy_state.revision` on every change a node's policy
//! snapshot depends on.
//!
//! m20261022_000007 bumps it on rules, profiles, sandboxes created or
//! deleted, and services renamed. A snapshot also reads which sandboxes are
//! live and recorded, service members and their name records, and the
//! nodes' addresses and compute subnets. Nodes in `legacy` now receive a
//! snapshot every pass, served from a cache keyed on this revision, so it
//! must move whenever any of those does.
//!
//! The triggers fire per statement and only on the columns a snapshot
//! reads, so a heartbeat (`nodes.last_heartbeat`) or a sandbox touch
//! (`sandboxes.last_activity_at`) does not invalidate every node's cache.
//! Additive; `down` drops the triggers.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// `(trigger, table, events)`; each runs `network_policy_bump_revision()`.
pub const TRIGGERS: &[(&str, &str, &str)] = &[
    (
        "sandboxes_network_policy_revision_update",
        "sandboxes",
        "UPDATE OF status, egress_recording, node_id, public_id",
    ),
    (
        "service_members_network_policy_revision",
        "service_members",
        "INSERT OR DELETE OR UPDATE OF service_id, container_name, compute_ip, status, node_id",
    ),
    (
        "service_endpoints_network_policy_revision",
        "service_endpoints",
        "INSERT OR DELETE OR UPDATE OF fqdn, record_type, target_ip, owner_kind, owner_id, node_id",
    ),
    (
        "nodes_network_policy_revision",
        "nodes",
        "INSERT OR DELETE OR UPDATE OF private_address, underlay_address, mesh_wg_address, \
         compute_cidr, status",
    ),
    (
        "network_config_network_policy_revision",
        "network_config",
        "INSERT OR DELETE OR UPDATE OF control_plane_compute_cidr",
    ),
];

pub fn up_sql() -> Vec<String> {
    TRIGGERS
        .iter()
        .flat_map(|(name, table, events)| {
            [
                format!("DROP TRIGGER IF EXISTS {name} ON {table}"),
                format!(
                    "CREATE TRIGGER {name} AFTER {events} ON {table} \
                     FOR EACH STATEMENT EXECUTE FUNCTION network_policy_bump_revision()"
                ),
            ]
        })
        .collect()
}

pub fn down_sql() -> Vec<String> {
    TRIGGERS
        .iter()
        .map(|(name, table, _)| format!("DROP TRIGGER IF EXISTS {name} ON {table}"))
        .collect()
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
    fn heartbeats_and_activity_do_not_bump_the_revision() {
        let sql = up_sql().join("\n");
        assert!(!sql.contains("last_heartbeat"));
        assert!(!sql.contains("last_activity_at"));
        assert!(sql.contains("UPDATE OF status, egress_recording"));
        assert_eq!(down_sql().len(), TRIGGERS.len());
    }

    #[test]
    fn is_registered() {
        let names: Vec<String> = crate::migration::Migrator::migrations()
            .into_iter()
            .map(|m| m.name().to_string())
            .collect();
        assert!(names
            .iter()
            .any(|n| n == "m20261125_000004_network_policy_revision_inputs"));
    }
}
