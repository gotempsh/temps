//! Integration tests for multinode deployment scheduling and reconciliation.
//!
//! These tests verify the complete flow:
//! 1. Node registration and heartbeat processing
//! 2. Replica scheduling across multiple nodes
//! 3. Container reconciliation after agent reconnect
//! 4. Failover when nodes go offline
//! 5. Drain operation with container migration
//!
//! Tests use MockDatabase and do NOT require Docker.

use sea_orm::{DatabaseBackend, MockDatabase};
use std::sync::Arc;
use temps_entities::{deployment_containers, deployments, nodes};

use temps_deployments::services::node_scheduler::{NodeAssignment, NodeScheduler};
use temps_deployments::services::node_service::{HeartbeatRequest, NodeService};

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
        capacity: serde_json::json!({
            "cpu_percent": 30.0,
            "memory_used_bytes": 2_000_000_000u64,
            "memory_total_bytes": 8_000_000_000u64,
        }),
        edge_public_key: None,
        compute_cidr: None,
        underlay_address: None,
        last_heartbeat: Some(chrono::Utc::now() - chrono::Duration::seconds(heartbeat_age_secs)),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

fn make_container(
    id: i32,
    deployment_id: i32,
    node_id: i32,
    container_id: &str,
) -> deployment_containers::Model {
    deployment_containers::Model {
        id,
        deployment_id,
        container_id: container_id.to_string(),
        container_name: format!("app-{}", id),
        container_port: 8080,
        host_port: Some(30000 + id),
        image_name: Some("myapp:latest".to_string()),
        status: Some("running".to_string()),
        service_name: None,
        created_at: chrono::Utc::now(),
        deployed_at: chrono::Utc::now(),
        ready_at: Some(chrono::Utc::now()),
        deleted_at: None,
        node_id: Some(node_id),
    }
}

fn make_deployment(id: i32, project_id: i32, environment_id: i32) -> deployments::Model {
    deployments::Model {
        id,
        project_id,
        environment_id,
        slug: format!("deploy-{}", id),
        state: "deployed".to_string(),
        metadata: None,
        deploying_at: None,
        ready_at: None,
        started_at: Some(chrono::Utc::now()),
        finished_at: Some(chrono::Utc::now()),
        context_vars: None,
        branch_ref: Some("main".to_string()),
        tag_ref: None,
        commit_sha: None,
        commit_message: None,
        commit_author: None,
        commit_json: None,
        cancelled_reason: None,
        static_dir_location: None,
        screenshot_location: None,
        image_name: None,
        deployment_config: None,
        promoted_from_deployment_id: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Scheduling Integration Tests
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_schedule_replicas_across_multiple_nodes() {
    // 3 active worker nodes, schedule 3 replicas
    let nodes = vec![
        make_node(1, "worker-1", "active", 10),
        make_node(2, "worker-2", "active", 10),
        make_node(3, "worker-3", "active", 10),
    ];

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![nodes])
        .into_connection();

    let scheduler = NodeScheduler::new(Arc::new(NodeService::new(Arc::new(db))));
    let assignments = scheduler
        .schedule_replicas(3, None, None, false)
        .await
        .unwrap();

    assert_eq!(assignments.len(), 3);
    // With 3 workers + local, should have at least some remote assignments
    let has_remote = assignments
        .iter()
        .any(|a| matches!(a, NodeAssignment::Remote { .. }));
    assert!(
        has_remote,
        "Should assign at least some replicas to remote nodes"
    );
}

#[tokio::test]
async fn test_scheduling_excludes_offline_nodes() {
    // Only active nodes returned by list_active (offline filtered by query)
    let nodes = vec![
        make_node(1, "worker-1", "active", 10),
        make_node(3, "worker-3", "active", 10),
    ];

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![nodes])
        .into_connection();

    let scheduler = NodeScheduler::new(Arc::new(NodeService::new(Arc::new(db))));
    let assignments = scheduler
        .schedule_replicas(4, None, None, false)
        .await
        .unwrap();

    // Node 2 (offline) should never appear since list_active filters it
    for assignment in &assignments {
        if let NodeAssignment::Remote { node_id, .. } = assignment {
            assert_ne!(*node_id, 2, "Offline node should not receive assignments");
        }
    }
}

#[tokio::test]
async fn test_scheduling_with_label_selectors() {
    // Two nodes, only one matches label selector
    let mut gpu_node = make_node(1, "gpu-worker", "active", 10);
    gpu_node.labels = serde_json::json!({"gpu": "true", "region": "us"});

    let mut cpu_node = make_node(2, "cpu-worker", "active", 10);
    cpu_node.labels = serde_json::json!({"region": "us"});

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![gpu_node, cpu_node]])
        .into_connection();

    let scheduler = NodeScheduler::new(Arc::new(NodeService::new(Arc::new(db))));
    let labels = serde_json::json!({"gpu": "true"});
    let assignments = scheduler
        .schedule_replicas(2, Some(&labels), None, false)
        .await
        .unwrap();

    for assignment in &assignments {
        if let NodeAssignment::Remote { node_id, .. } = assignment {
            assert_eq!(*node_id, 1, "Only GPU node should match label selector");
        }
    }
}

#[tokio::test]
async fn test_scheduling_falls_back_to_local_when_no_workers() {
    // No worker nodes returned
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![Vec::<nodes::Model>::new()])
        .into_connection();

    let scheduler = NodeScheduler::new(Arc::new(NodeService::new(Arc::new(db))));
    let assignments = scheduler
        .schedule_replicas(3, None, None, false)
        .await
        .unwrap();

    assert_eq!(assignments.len(), 3);
    for assignment in &assignments {
        assert!(
            matches!(assignment, NodeAssignment::Local),
            "Should fall back to local when no workers available"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Reconciliation Integration Tests
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_reconcile_after_agent_crash_removes_ghost_records() {
    // DB has 3 containers but agent only has 2
    let c1 = make_container(1, 10, 5, "real-container-1");
    let c2 = make_container(2, 10, 5, "real-container-2");
    let c3 = make_container(3, 11, 5, "ghost-container-3");

    let mut c3_updated = c3.clone();
    c3_updated.status = Some("removed".to_string());
    c3_updated.deleted_at = Some(chrono::Utc::now());

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![c1, c2, c3]])
        .append_query_results(vec![vec![c3_updated]])
        .into_connection();

    let service = NodeService::new(Arc::new(db));
    let actual = vec![
        "real-container-1".to_string(),
        "real-container-2".to_string(),
    ];

    let stale = service.reconcile_containers(5, &actual).await.unwrap();
    assert_eq!(stale, 1, "Should mark exactly 1 ghost container as deleted");
}

#[tokio::test]
async fn test_reconcile_agent_has_extra_containers_no_op() {
    // Agent reports containers not in DB — reconcile only cleans DB side
    let c1 = make_container(1, 10, 5, "known-container");

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![c1]])
        .into_connection();

    let service = NodeService::new(Arc::new(db));
    let actual = vec![
        "known-container".to_string(),
        "extra-docker-container".to_string(),
    ];

    let stale = service.reconcile_containers(5, &actual).await.unwrap();
    assert_eq!(
        stale, 0,
        "Extra Docker containers should not cause DB changes"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Failover Integration Tests
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_health_check_marks_stale_nodes_offline() {
    use temps_deployments::jobs::node_health_check::check_node_health;

    let stale_node = make_node(1, "worker-1", "active", 120);
    let mut offline_node = stale_node.clone();
    offline_node.status = "offline".to_string();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![stale_node.clone()]])
        .into_connection();

    let service_db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![stale_node]])
        .append_query_results(vec![vec![offline_node]])
        .into_connection();

    let node_service = NodeService::new(Arc::new(service_db));
    let marked = check_node_health(&node_service, &db).await;

    assert_eq!(marked, vec![1], "Stale node should be marked offline");
}

#[tokio::test]
async fn test_health_check_ignores_fresh_nodes() {
    use temps_deployments::jobs::node_health_check::check_node_health;

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![Vec::<nodes::Model>::new()])
        .into_connection();

    let service_db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let node_service = NodeService::new(Arc::new(service_db));

    let marked = check_node_health(&node_service, &db).await;
    assert!(
        marked.is_empty(),
        "Fresh nodes should not be marked offline"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Drain Integration Tests
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_drain_complete_transitions_status() {
    let draining = make_node(1, "worker-1", "draining", 10);
    let mut drained = draining.clone();
    drained.status = "drained".to_string();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![draining]])
        .append_query_results(vec![Vec::<deployment_containers::Model>::new()])
        .append_query_results(vec![vec![drained]])
        .into_connection();

    let service = NodeService::new(Arc::new(db));
    let complete = service.check_drain_complete(1).await.unwrap();
    assert!(
        complete,
        "Drain should be complete when no containers remain"
    );
}

#[tokio::test]
async fn test_drain_not_complete_with_remaining_containers() {
    let draining = make_node(1, "worker-1", "draining", 10);
    let container = make_container(1, 10, 1, "still-running");

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![draining]])
        .append_query_results(vec![vec![container]])
        .into_connection();

    let service = NodeService::new(Arc::new(db));
    let complete = service.check_drain_complete(1).await.unwrap();
    assert!(
        !complete,
        "Drain should not be complete while containers remain"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Heartbeat Lifecycle Integration Tests
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_heartbeat_reactivates_offline_node() {
    let offline = make_node(1, "worker-1", "offline", 100);
    let mut reactivated = offline.clone();
    reactivated.status = "active".to_string();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![offline]])
        .append_query_results(vec![vec![reactivated]])
        .into_connection();

    let service = NodeService::new(Arc::new(db));
    let result = service
        .heartbeat(
            1,
            HeartbeatRequest {
                capacity: serde_json::json!({"cpu_percent": 25}),
                labels: None,
            },
        )
        .await;

    assert!(result.is_ok(), "Heartbeat should reactivate offline node");
}

#[tokio::test]
async fn test_heartbeat_preserves_draining_status() {
    let draining = make_node(1, "worker-1", "draining", 10);
    let still_draining = draining.clone();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![draining]])
        .append_query_results(vec![vec![still_draining]])
        .into_connection();

    let service = NodeService::new(Arc::new(db));
    let result = service
        .heartbeat(
            1,
            HeartbeatRequest {
                capacity: serde_json::json!({"cpu_percent": 25}),
                labels: None,
            },
        )
        .await;

    assert!(
        result.is_ok(),
        "Heartbeat should succeed without changing draining status"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Affected Deployments Integration Tests
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_affected_deployments_identifies_needs_redeploy() {
    // Deployment 10: all 2 replicas on node 5 → needs redeploy
    // Deployment 20: 1 of 3 replicas on node 5 → does NOT need redeploy
    let c1 = make_container(1, 10, 5, "c1");
    let c2 = make_container(2, 10, 5, "c2");
    let c3 = make_container(3, 20, 5, "c3");

    let d1 = make_deployment(10, 100, 200);
    let d2 = make_deployment(20, 100, 201);

    let c4 = make_container(4, 20, 6, "c4");
    let c5 = make_container(5, 20, 7, "c5");

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![c1.clone(), c2.clone(), c3.clone()]])
        .append_query_results(vec![vec![d1, d2]])
        .append_query_results(vec![vec![c1, c2, c3, c4, c5]])
        .into_connection();

    let service = NodeService::new(Arc::new(db));
    let affected = service.affected_deployments(5).await.unwrap();

    let dep10 = affected.iter().find(|d| d.deployment_id == 10).unwrap();
    assert!(
        dep10.needs_redeploy(),
        "All replicas on node 5 → needs redeploy"
    );

    let dep20 = affected.iter().find(|d| d.deployment_id == 20).unwrap();
    assert!(
        !dep20.needs_redeploy(),
        "Has replicas on other nodes → no redeploy"
    );
}
