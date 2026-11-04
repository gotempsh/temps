// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Best-effort overlay setup while a node joins, and the mesh state a join
//! depends on. The overlay writes never fail the join: the agent's network
//! sync polls for the result, so a transient failure heals itself.

use super::*;
use temps_network::allocator::{AllocatorError, ComputeNetworkAllocator, PostgresAllocator};
use temps_network::mesh::{self, MeshError};
use tracing::{info, warn};

impl NodeService {
    /// Record the overlay network name a joining node chose because its host
    /// already had the cluster's (ADR 059). Best-effort like the underlay: if it
    /// is lost, the agent reports the name conflict with the fix.
    pub async fn record_overlay_network_name(&self, node_id: i32, name: &str) {
        let result = nodes::Entity::update_many()
            .col_expr(nodes::Column::OverlayNetworkName, Expr::value(name))
            .filter(nodes::Column::Id.eq(node_id))
            .exec(self.db.as_ref())
            .await;
        match result {
            Ok(_) => info!(
                node_id,
                overlay_network_name = name,
                "overlay network name set"
            ),
            Err(e) => warn!(
                node_id,
                "failed to set the overlay network name; the node's agent will report the name \
                 conflict: {}",
                e
            ),
        }
    }

    /// Set `nodes.underlay_address` to the value other nodes will use to reach
    /// this one over the overlay. Best-effort: failures are logged and
    /// swallowed because the operator can fix manually and the agent will
    /// pick up the change on its next poll.
    pub async fn record_underlay_address(&self, node_id: i32, underlay: &str) {
        // A node on the WireGuard mesh keeps its mesh address as underlay across
        // re-registration; the address it joined with is only its endpoint.
        // Decided in the UPDATE itself so a concurrent mesh registration can't be
        // overwritten from a stale read.
        let result = nodes::Entity::update_many()
            .col_expr(
                nodes::Column::UnderlayAddress,
                Expr::cust_with_values("COALESCE(mesh_wg_address, $1)", [underlay.to_string()]),
            )
            .filter(nodes::Column::Id.eq(node_id))
            .exec(self.db.as_ref())
            .await;
        match result {
            Ok(_) => info!(node_id, underlay, "underlay_address set"),
            Err(e) => warn!(
                node_id,
                "failed to set underlay_address (overlay may be delayed): {}", e
            ),
        }
    }

    /// Ask the allocator for a compute_cidr. Treat AlreadyAllocated as success
    /// (re-registration after a restart). Treat any other error as a
    /// non-fatal warning — the join flow stays successful so the operator
    /// isn't blocked from running deployments while overlay networking
    /// converges in the background.
    pub async fn allocate_overlay_cidr(&self, node_id: i32) {
        let allocator = PostgresAllocator::new(self.db.clone());
        match allocator.allocate_for_node(node_id).await {
            Ok(alloc) => info!(
                node_id,
                cidr = %alloc.compute_cidr,
                "compute_cidr auto-allocated on join"
            ),
            Err(AllocatorError::AlreadyAllocated { existing, .. }) => {
                info!(node_id, %existing, "compute_cidr already present (re-registration)");
            }
            Err(AllocatorError::UnderlayMissing { .. }) => {
                warn!(
                    node_id,
                    "underlay_address missing during allocation; agent sync will retry"
                );
            }
            Err(e) => warn!(
                node_id,
                "failed to allocate compute_cidr (overlay deferred to next agent poll): {}", e
            ),
        }
    }

    /// Whether the WireGuard mesh is on for this cluster.
    pub async fn mesh_is_enabled(&self) -> Result<bool, MeshError> {
        Ok(mesh::load_settings(self.db.as_ref()).await?.is_some())
    }

    /// The control plane's published mesh identity, once it has one.
    pub async fn published_mesh_control_plane(
        &self,
    ) -> Result<Option<mesh::PublishedControlPlane>, MeshError> {
        mesh::published_control_plane(self.db.as_ref()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, DbErr, MockDatabase, MockExecResult};

    fn exec_ok() -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }
    }

    fn statements(db: Arc<DatabaseConnection>) -> String {
        let db = Arc::try_unwrap(db).unwrap_or_else(|_| panic!("db still has owners"));
        format!("{:?}", db.into_transaction_log())
    }

    #[tokio::test]
    async fn the_underlay_write_keeps_a_mesh_address_when_one_is_set() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results([exec_ok()])
                .into_connection(),
        );
        let service = NodeService::new(db.clone());

        service.record_underlay_address(7, "10.0.0.7").await;

        drop(service);
        let log = statements(db);
        assert!(log.contains("COALESCE(mesh_wg_address"), "{log}");
        assert!(log.contains("10.0.0.7"), "{log}");
    }

    #[tokio::test]
    async fn the_overlay_name_write_targets_the_joining_node() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results([exec_ok()])
                .into_connection(),
        );
        let service = NodeService::new(db.clone());

        service
            .record_overlay_network_name(7, "temps-overlay-2")
            .await;

        drop(service);
        let log = statements(db);
        assert!(log.contains("overlay_network_name"), "{log}");
        assert!(log.contains("temps-overlay-2"), "{log}");
    }

    /// A join never fails on these writes: the agent's network sync heals them.
    #[tokio::test]
    async fn failed_overlay_writes_do_not_fail_the_join() {
        let service = NodeService::new(Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_errors([
                    DbErr::Custom("connection lost".to_string()),
                    DbErr::Custom("connection lost".to_string()),
                ])
                .into_connection(),
        ));

        service.record_underlay_address(7, "10.0.0.7").await;
        service
            .record_overlay_network_name(7, "temps-overlay-2")
            .await;
    }

    #[tokio::test]
    async fn a_failed_allocation_does_not_fail_the_join() {
        let service = NodeService::new(Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([DbErr::Custom("connection lost".to_string())])
                .into_connection(),
        ));

        service.allocate_overlay_cidr(7).await;
    }
}
