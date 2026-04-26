//! Periodic job that checks node health, marks stale nodes as offline,
//! and triggers failover redeployment for affected environments.
//!
//! Runs on the control plane every 60 seconds. Nodes that haven't sent
//! a heartbeat in >90 seconds are marked offline. When a node transitions
//! to offline, its affected environments are automatically redeployed
//! to healthy nodes.

use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};

use temps_entities::nodes;

use crate::services::node_service::NodeService;
use crate::DeploymentService;

/// Threshold in seconds — nodes with older heartbeats are marked offline.
const HEARTBEAT_STALE_THRESHOLD_SECS: i64 = 90;

/// Runs a single health check pass across all active nodes.
///
/// This is designed to be called by a scheduler (e.g., every 60 seconds).
/// It does NOT run in a loop itself.
///
/// Returns the list of node IDs that were marked offline (for failover).
pub async fn check_node_health(node_service: &NodeService, db: &DatabaseConnection) -> Vec<i32> {
    let cutoff = chrono::Utc::now() - chrono::Duration::seconds(HEARTBEAT_STALE_THRESHOLD_SECS);

    // Find nodes that are still marked "active" but have a stale heartbeat
    let stale_nodes = match nodes::Entity::find()
        .filter(nodes::Column::Status.eq("active"))
        .filter(
            nodes::Column::LastHeartbeat
                .lt(cutoff)
                .or(nodes::Column::LastHeartbeat.is_null()),
        )
        .all(db)
        .await
    {
        Ok(nodes) => nodes,
        Err(e) => {
            tracing::error!("Failed to query nodes for health check: {}", e);
            return vec![];
        }
    };

    let mut marked_offline = Vec::new();

    for node in &stale_nodes {
        tracing::warn!(
            node_id = node.id,
            node_name = %node.name,
            last_heartbeat = ?node.last_heartbeat,
            "Node heartbeat stale, marking as offline"
        );

        if let Err(e) = node_service.mark_offline(node.id).await {
            tracing::error!(
                node_id = node.id,
                node_name = %node.name,
                "Failed to mark node as offline: {}",
                e
            );
        } else {
            marked_offline.push(node.id);
        }
    }

    if !marked_offline.is_empty() {
        tracing::info!(
            count = marked_offline.len(),
            "Node health check completed: marked {} node(s) offline",
            marked_offline.len()
        );
    }

    marked_offline
}

/// Check all draining nodes for drain completion and transition them
/// to "drained" status when all containers have been migrated.
///
/// This is designed to be called after `check_node_health` in the same
/// periodic job (every 60 seconds). Returns the node IDs that completed.
pub async fn check_drain_completion(node_service: &NodeService) -> Vec<i32> {
    match node_service.check_all_drains().await {
        Ok(completed) => completed,
        Err(e) => {
            tracing::error!("Failed to check drain completion: {}", e);
            vec![]
        }
    }
}

/// Handle failover for nodes that just went offline.
///
/// For each affected deployment:
/// - If other nodes still have healthy replicas, just retire the containers
///   on the offline node (proxy stops routing to them on next refresh).
/// - If ALL replicas were on the offline node, trigger a full redeploy so
///   the workload is rescheduled to a healthy node.
pub async fn failover_offline_nodes(
    offline_node_ids: &[i32],
    node_service: &NodeService,
    deployment_service: &DeploymentService,
) {
    if offline_node_ids.is_empty() {
        return;
    }

    for &node_id in offline_node_ids {
        let affected = match node_service.affected_deployments(node_id).await {
            Ok(deps) => deps,
            Err(e) => {
                tracing::error!(
                    node_id,
                    "Failed to query affected deployments for failover: {}",
                    e
                );
                continue;
            }
        };

        if affected.is_empty() {
            tracing::debug!(node_id, "No affected deployments for offline node");
            continue;
        }

        tracing::warn!(
            node_id,
            affected_count = affected.len(),
            "Failover: processing {} deployment(s) on offline node",
            affected.len()
        );

        for dep in &affected {
            if dep.needs_redeploy() {
                // All replicas were on this node — must redeploy
                match deployment_service
                    .redeploy_environment(dep.project_id, dep.environment_id)
                    .await
                {
                    Ok(_) => {
                        tracing::info!(
                            node_id,
                            project_id = dep.project_id,
                            environment_id = dep.environment_id,
                            "Failover: triggered full redeploy (no healthy replicas elsewhere)"
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            node_id,
                            project_id = dep.project_id,
                            environment_id = dep.environment_id,
                            "Failover: failed to trigger redeploy: {}",
                            e
                        );
                    }
                }
            } else {
                // Other nodes have healthy replicas — just retire stale containers
                match node_service
                    .retire_containers_on_node(node_id, dep.deployment_id)
                    .await
                {
                    Ok(count) => {
                        tracing::info!(
                            node_id,
                            deployment_id = dep.deployment_id,
                            retired = count,
                            remaining = dep.total_active_containers - dep.containers_on_node,
                            "Failover: retired containers, healthy replicas remain"
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            node_id,
                            deployment_id = dep.deployment_id,
                            "Failover: failed to retire containers: {}",
                            e
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn make_node(id: i32, name: &str, status: &str, heartbeat_age_secs: i64) -> nodes::Model {
        nodes::Model {
            id,
            name: name.to_string(),
            token_hash: "hash".to_string(),
            token_encrypted: None,
            address: format!("https://10.100.0.{}:3100", id),
            private_address: format!("10.100.0.{}", id),
            public_endpoint: None,
            wg_public_key: None,
            role: "worker".to_string(),
            status: status.to_string(),
            labels: serde_json::json!({}),
            capacity: serde_json::json!({}),
            last_heartbeat: Some(
                chrono::Utc::now() - chrono::Duration::seconds(heartbeat_age_secs),
            ),
            edge_public_key: None,
            compute_cidr: None,
            underlay_address: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn test_heartbeat_threshold_is_reasonable() {
        // Heartbeat threshold should be greater than the heartbeat interval (30s)
        // but not so long that stale nodes linger
        const { assert!(HEARTBEAT_STALE_THRESHOLD_SECS > 30) };
        const { assert!(HEARTBEAT_STALE_THRESHOLD_SECS <= 300) };
    }

    #[tokio::test]
    async fn test_check_node_health_no_stale_nodes() {
        // No stale nodes — query returns empty
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<nodes::Model>::new()])
            .into_connection();
        let node_service = NodeService::new(std::sync::Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
        ));

        let marked = check_node_health(&node_service, &db).await;
        assert!(marked.is_empty());
    }

    #[tokio::test]
    async fn test_check_node_health_marks_stale_nodes() {
        // Two stale nodes returned by query
        let stale_node_1 = make_node(1, "worker-1", "active", 120);
        let stale_node_2 = make_node(2, "worker-2", "active", 200);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![stale_node_1, stale_node_2]])
            .into_connection();

        // NodeService needs its own mock db for mark_offline calls:
        // - get_by_id for node 1, update for node 1
        // - get_by_id for node 2, update for node 2
        let node_1_for_service = make_node(1, "worker-1", "active", 120);
        let node_1_updated = make_node(1, "worker-1", "offline", 120);
        let node_2_for_service = make_node(2, "worker-2", "active", 200);
        let node_2_updated = make_node(2, "worker-2", "offline", 200);

        let service_db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_1_for_service]])
            .append_query_results(vec![vec![node_1_updated]])
            .append_query_results(vec![vec![node_2_for_service]])
            .append_query_results(vec![vec![node_2_updated]])
            .into_connection();
        let node_service = NodeService::new(std::sync::Arc::new(service_db));

        let marked = check_node_health(&node_service, &db).await;
        assert_eq!(marked.len(), 2);
        assert!(marked.contains(&1));
        assert!(marked.contains(&2));
    }

    #[tokio::test]
    async fn test_check_node_health_returns_offline_ids() {
        let stale_node = make_node(5, "worker-5", "active", 200);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![stale_node]])
            .into_connection();

        let node_for_service = make_node(5, "worker-5", "active", 200);
        let node_updated = make_node(5, "worker-5", "offline", 200);

        let service_db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![node_for_service]])
            .append_query_results(vec![vec![node_updated]])
            .into_connection();
        let node_service = NodeService::new(std::sync::Arc::new(service_db));

        let marked = check_node_health(&node_service, &db).await;
        assert_eq!(marked, vec![5]);
    }
}
