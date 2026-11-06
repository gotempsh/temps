// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runtime and stats of a service on a worker node, read from a
//! `--profile control-plane` server (no Docker handle) through a fake agent.

use super::*;
use crate::services::test_support::*;
use axum::{extract::Path, routing::get, Json, Router};
use std::sync::Arc;
use temps_entities::{external_services, nodes};

const NODE_ID: i32 = 3;

/// A fake worker agent serving `app`, and the node row that points at it,
/// its token encrypted with the test manager's key.
async fn worker_running(app: Router) -> nodes::Model {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let encryption =
        temps_core::EncryptionService::new_from_password("service-parameter-reveal-test");
    let mut node = nodes_test_model(NODE_ID);
    node.address = format!("http://{address}");
    node.token_encrypted = Some(encryption.encrypt_string("node-token").unwrap());
    node
}

fn worker_postgres() -> external_services::Model {
    let mut service = encrypted_service_model(
        11,
        serde_json::json!({
            "host": "localhost",
            "port": "5561",
            "username": "app",
            "password": "s3cret",
            "database": "orders",
        }),
    );
    service.name = "orders".to_string();
    service.node_id = Some(NODE_ID);
    service
}

/// A control-plane manager whose database answers the service row twice
/// (the request, then its parameters) and then the node.
fn manager_for(service: &external_services::Model, node: nodes::Model) -> ExternalServiceManager {
    let db = Arc::new(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![service.clone()], vec![service.clone()]])
            .append_query_results([vec![node]])
            .into_connection(),
    );
    let manager = control_plane_manager_with_db(db);
    assert!(
        manager.require_docker().is_err(),
        "the manager must have no Docker handle"
    );
    manager
}

fn stats_at(total: u64, system: u64) -> bollard::models::ContainerStatsResponse {
    bollard::models::ContainerStatsResponse {
        cpu_stats: Some(bollard::models::ContainerCpuStats {
            cpu_usage: Some(bollard::models::ContainerCpuUsage {
                total_usage: Some(total),
                ..Default::default()
            }),
            system_cpu_usage: Some(system),
            online_cpus: Some(2),
            ..Default::default()
        }),
        memory_stats: Some(bollard::models::ContainerMemoryStats {
            usage: Some(100 * 1024 * 1024),
            limit: Some(1024 * 1024 * 1024),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// The live failure: the runtime panel of a worker's database answered 409
/// `WORKER_NODE_REQUIRED`. It now reads the container through the agent.
#[tokio::test]
async fn a_worker_service_runtime_is_read_through_its_agent() {
    let app = Router::new().route(
        "/agent/services/{name}/runtime",
        get(|Path(name): Path<String>| async move {
            assert_eq!(name, "postgres-orders");
            Json(serde_json::json!({
                "success": true,
                "data": {
                    "found": true,
                    "container_id": "abc123",
                    "status": "running",
                    "restart_count": 2,
                    "oom_killed": false,
                    "exit_code": 0,
                    "image": "sha256:feed",
                    "memory_bytes": 512 * 1024 * 1024,
                },
                "error": null,
            }))
        }),
    );
    let service = worker_postgres();
    let manager = manager_for(&service, worker_running(app).await);

    let report = manager.get_service_runtime(11).await.expect("no 409");

    let member = &report.members[0];
    assert_eq!(member.container_name, "postgres-orders");
    assert_eq!(member.container_id.as_deref(), Some("abc123"));
    assert_eq!(member.status.as_deref(), Some("running"));
    assert_eq!(member.restart_count, Some(2));
    assert_eq!(member.resource_limits.memory_mb, Some(512));
    assert_eq!(member.unavailable_reason, None);
}

/// The stats panel of a worker's database: two samples from the agent, CPU
/// computed here with the control plane's own formula.
#[tokio::test]
async fn a_worker_service_stats_use_the_control_plane_formula() {
    let app = Router::new().route(
        "/agent/services/{name}/stats",
        get(|Path(_name): Path<String>| async move {
            Json(serde_json::json!({
                "success": true,
                "data": {
                    "found": true,
                    "samples": [stats_at(0, 0), stats_at(1_000_000_000, 4_000_000_000)],
                },
                "error": null,
            }))
        }),
    );
    let service = worker_postgres();
    let manager = manager_for(&service, worker_running(app).await);

    let report = manager.get_service_stats(11).await.expect("no 409");

    let member = &report.members[0];
    assert_eq!(member.cpu_percent, Some(50.0));
    assert_eq!(member.memory_usage_bytes, Some(100 * 1024 * 1024));
    assert_eq!(member.online_cpus, Some(2));
    assert_eq!(member.unavailable_reason, None);
}

/// An agent from before these routes answers 404: the page says to update
/// the agent on that node, instead of an error or a "container missing".
#[tokio::test]
async fn an_agent_without_the_routes_says_to_update_it() {
    let service = worker_postgres();

    let runtime_manager = manager_for(&service, worker_running(Router::new()).await);
    let runtime = runtime_manager
        .get_service_runtime(11)
        .await
        .expect("degrades instead of failing");
    let reason = runtime.members[0]
        .unavailable_reason
        .clone()
        .unwrap_or_default();
    assert!(
        reason.contains("update the agent on node worker-3"),
        "{reason}"
    );
    assert_eq!(runtime.members[0].container_id, None);

    let stats_manager = manager_for(&service, worker_running(Router::new()).await);
    let stats = stats_manager
        .get_service_stats(11)
        .await
        .expect("degrades instead of failing");
    let reason = stats.members[0]
        .unavailable_reason
        .clone()
        .unwrap_or_default();
    assert!(reason.starts_with("stats unavailable"), "{reason}");
    assert!(reason.contains("worker-3"), "{reason}");
}

/// A container the worker does not have is "missing", with no reason: the
/// agent answered, the container is simply not there.
#[tokio::test]
async fn a_missing_worker_container_is_reported_missing() {
    let app = Router::new().route(
        "/agent/services/{name}/runtime",
        get(|| async {
            Json(serde_json::json!({
                "success": true,
                "data": {"found": false},
                "error": null,
            }))
        }),
    );
    let service = worker_postgres();
    let manager = manager_for(&service, worker_running(app).await);

    let report = manager.get_service_runtime(11).await.expect("runtime");

    assert_eq!(report.members[0].container_id, None);
    assert_eq!(report.members[0].unavailable_reason, None);
}

/// A local service on a server without Docker is not "missing": the report
/// says this server cannot see it.
#[tokio::test]
async fn a_local_service_without_docker_says_why() {
    let mut service = worker_postgres();
    service.node_id = None;
    let db = Arc::new(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![service.clone()], vec![service.clone()]])
            .append_query_results([vec![service.clone()], vec![service]])
            .into_connection(),
    );
    let manager = control_plane_manager_with_db(db);

    let runtime = manager.get_service_runtime(11).await.expect("runtime");
    let reason = runtime.members[0]
        .unavailable_reason
        .clone()
        .unwrap_or_default();
    assert!(reason.contains("no local Docker daemon"), "{reason}");

    let stats = manager.get_service_stats(11).await.expect("stats");
    let reason = stats.members[0]
        .unavailable_reason
        .clone()
        .unwrap_or_default();
    assert!(reason.contains("no local Docker daemon"), "{reason}");
}

/// The health monitor's poller samples a worker's container once per tick
/// through the agent, keeping the sample as the next tick's baseline.
#[tokio::test]
async fn the_poller_samples_a_worker_container_once_per_tick() {
    let app = Router::new().route(
        "/agent/services/{name}/stats",
        get(
            |Path(_name): Path<String>,
             axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>| async move {
                assert_eq!(query.get("samples").map(String::as_str), Some("1"));
                Json(serde_json::json!({
                    "success": true,
                    "data": {"found": true, "samples": [stats_at(1_000_000_000, 4_000_000_000)]},
                    "error": null,
                }))
            },
        ),
    );
    let service = worker_postgres();
    let db = Arc::new(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![service.clone()]])
            .append_query_results([vec![worker_running(app).await]])
            .into_connection(),
    );
    let manager = control_plane_manager_with_db(db);
    let mut baselines = HashMap::from([("postgres-orders".to_string(), stats_at(0, 0))]);

    let report = manager
        .sample_service_stats(&service, &mut baselines)
        .await
        .expect("sample");

    assert_eq!(report.members[0].cpu_percent, Some(50.0));
    assert!(baselines.contains_key("postgres-orders"));
}
