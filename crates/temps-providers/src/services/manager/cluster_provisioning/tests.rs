// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::externalsvc::{ClusterMemberResult, ClusterMemberSpec};
use crate::services::test_support::*;
use chrono::Utc;
use sea_orm::{ActiveModelTrait, DatabaseConnection, EntityTrait, QueryOrder, Set};
use std::collections::BTreeSet;
use std::sync::Arc;
use temps_entities::{nodes, service_members, settings};

/// Regression for the FQDN-vs-container-name fix that determines every
/// local cluster's injected `POSTGRES_URL`: a cluster with any remote
/// member must use the `*.temps.local` FQDN (container names can't
/// cross a Docker-host boundary), while an all-local cluster must keep
/// the plain container name (the FQDN only resolves once the
/// experimental, off-by-default `cluster_dns.enabled` resolver wiring
/// is on, which broke every single-host cluster by default before this
/// fix).
/// The agent reports Docker's health with `{:?}` (`HEALTHY`), the local
/// inspect with `Display` (`healthy`); both must count as ready, and a
/// member without a health check is ready once it runs.
#[test]
fn member_container_ready_accepts_every_health_spelling() {
    assert!(member_container_ready(true, None));
    assert!(member_container_ready(true, Some("HEALTHY")));
    assert!(member_container_ready(true, Some("healthy")));
    assert!(member_container_ready(true, Some("\"HEALTHY\"")));
    assert!(member_container_ready(true, Some("NONE")));
    assert!(member_container_ready(true, Some("")));
}

#[test]
fn member_container_ready_waits_for_start_and_health() {
    assert!(!member_container_ready(false, None));
    assert!(!member_container_ready(false, Some("HEALTHY")));
    assert!(!member_container_ready(true, Some("STARTING")));
    assert!(!member_container_ready(true, Some("unhealthy")));
}

#[test]
fn resolve_member_hostname_prefers_fqdn_only_when_cluster_spans_hosts() {
    assert_eq!(
        ExternalServiceManager::resolve_member_hostname(
            true,
            "orders-1.orders.temps.local",
            "orders-postgres-1",
        ),
        "orders-1.orders.temps.local",
        "a cluster with any remote member must use the FQDN"
    );
    assert_eq!(
        ExternalServiceManager::resolve_member_hostname(
            false,
            "orders-1.orders.temps.local",
            "orders-postgres-1",
        ),
        "orders-postgres-1",
        "an all-local cluster must keep the plain container name"
    );
}

/// `add_cluster_member`'s monitor-reachability decision must be driven
/// by the actual topology of *this* add, not by a persisted string
/// (`monitor.hostname`) that only reflects the cluster's topology at
/// *creation* time and is never retroactively recomputed. In
/// particular: adding the cluster's first-ever remote member to a
/// previously all-local cluster must not hand that new member the
/// monitor's plain Docker container name (unreachable cross-host).
#[test]
fn monitor_reachability_for_add_derives_from_actual_add_topology() {
    assert_eq!(
        ExternalServiceManager::monitor_reachability_for_add(None, None),
        MonitorReachability::SameHost,
        "monitor and new member both local -> same Docker host"
    );
    assert_eq!(
        ExternalServiceManager::monitor_reachability_for_add(None, Some(7)),
        MonitorReachability::LocalControlPlane,
        "monitor local but the member being added is remote -> needs \
             the control plane's own private IP, not the monitor's \
             container name"
    );
    assert_eq!(
        ExternalServiceManager::monitor_reachability_for_add(Some(3), None),
        MonitorReachability::MonitorNode(3),
        "monitor itself is remote -> always its node's private address"
    );
    assert_eq!(
        ExternalServiceManager::monitor_reachability_for_add(Some(3), Some(7)),
        MonitorReachability::MonitorNode(3),
        "monitor remote and new member remote (possibly different \
             nodes) -> still the monitor's own node address"
    );
}

fn node_with_overlay(id: i32, status: Option<&str>, error: Option<&str>) -> nodes::Model {
    nodes::Model {
        overlay_status: status.map(str::to_owned),
        overlay_last_error: error.map(str::to_owned),
        ..nodes_test_model(id)
    }
}

/// ADR 056: a cluster that spans hosts is refused on a node whose
/// cross-node overlay is unavailable, naming it and its cause; a
/// single-host cluster and a node that never reported are not.
#[tokio::test]
async fn a_multi_host_cluster_is_refused_on_a_node_without_the_overlay() {
    let cause = "overlay subnet 10.202.0.0/24 overlaps Docker network 'shop_default'";
    let down = node_with_overlay(3, Some("unavailable"), Some(cause));

    let db = db_with_nodes(vec![down.clone()]);
    let err = ExternalServiceManager::resolve_member_placement(
        &db,
        1,
        &[member("monitor", None), member("replica", Some(3))],
    )
    .await
    .expect_err("a member across hosts needs the overlay");
    let message = err.to_string();
    assert!(message.contains("'worker-3'"), "{message}");
    assert!(message.contains("shop_default"), "{message}");

    // Every member on that one node: no cross-host traffic, allowed.
    let db = db_with_nodes(vec![down.clone()]);
    ExternalServiceManager::resolve_member_placement(
        &db,
        1,
        &[member("monitor", Some(3)), member("replica", Some(3))],
    )
    .await
    .expect("a single-host cluster does not need the overlay");

    // An older agent that never reported is not refused.
    let db = db_with_nodes(vec![node_with_overlay(4, None, None)]);
    ExternalServiceManager::resolve_member_placement(
        &db,
        1,
        &[member("monitor", None), member("replica", Some(4))],
    )
    .await
    .expect("an unreported overlay is not a refusal");

    // The add-member path applies the same rule.
    assert!(ExternalServiceManager::refuse_nodes_without_overlay(
        1,
        &[None, Some(3)],
        std::slice::from_ref(&down),
    )
    .is_err());
    assert!(ExternalServiceManager::refuse_nodes_without_overlay(
        1,
        &[None, Some(3)],
        &[node_with_overlay(3, Some("ready"), None)],
    )
    .is_ok());
}

fn db_with_nodes(rows: Vec<nodes::Model>) -> DatabaseConnection {
    sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
        .append_query_results(vec![rows])
        .into_connection()
}

/// The node list surfaces the control plane as id 0, but it has no `nodes`
/// row — local placement is `None` everywhere else. Without collapsing it,
/// creating a cluster member there failed with
/// `Internal error: Node 0 not found`.
#[tokio::test]
async fn test_control_plane_node_id_resolves_to_local() {
    let db = db_with_nodes(vec![]);
    let resolved = ExternalServiceManager::resolve_member_placement(
        &db,
        1,
        &[member("monitor", Some(0)), member("replica", None)],
    )
    .await
    .expect("node 0 is the control plane, not an unknown node");

    assert_eq!(resolved.len(), 2);
    assert!(
        resolved.iter().all(|m| m.node_id.is_none()),
        "both members should be local: {:?}",
        resolved.iter().map(|m| m.node_id).collect::<Vec<_>>()
    );
    // Roles must survive normalization untouched.
    assert_eq!(resolved[0].role, "monitor");
    assert_eq!(resolved[1].role, "replica");
}

/// An id that has no row must be rejected as a validation error *before*
/// any container is created — it used to surface as an internal error
/// partway through building the cluster.
#[tokio::test]
async fn test_unknown_node_id_is_a_validation_error() {
    let db = db_with_nodes(vec![]);
    let err =
        ExternalServiceManager::resolve_member_placement(&db, 7, &[member("replica", Some(42))])
            .await
            .expect_err("node 42 does not exist");

    match err {
        ExternalServiceError::ParameterValidationFailed { service_id, reason } => {
            assert_eq!(service_id, 7);
            assert!(reason.contains("42"), "must name the bad id: {reason}");
        }
        other => panic!("expected ParameterValidationFailed, got {other:?}"),
    }
}

/// A real worker id passes through so remote placement still works.
#[tokio::test]
async fn test_known_node_id_is_preserved() {
    let db = db_with_nodes(vec![nodes_test_model(3)]);

    let resolved =
        ExternalServiceManager::resolve_member_placement(&db, 1, &[member("replica", Some(3))])
            .await
            .expect("node 3 exists");

    assert_eq!(resolved[0].node_id, Some(3));
}

fn cluster_member_result(ordinal: i32, role: &str) -> ClusterMemberResult {
    ClusterMemberResult {
        ordinal,
        role: role.to_string(),
        container_id: String::new(),
        container_name: format!("cluster-member-{ordinal}"),
        port: None,
        status: "pending".to_string(),
    }
}

#[tokio::test]
async fn test_precreated_cluster_topology_is_one_transaction() {
    let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
        // pg_advisory_xact_lock
        .append_exec_results([sea_orm::MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        // No other cluster holds a port.
        .append_query_results([Vec::<std::collections::BTreeMap<&str, sea_orm::Value>>::new()])
        .append_query_results([
            vec![service_member_model(1, 0, "monitor")],
            vec![service_member_model(2, 1, "replica")],
        ])
        .into_connection();
    let results = [
        cluster_member_result(0, "monitor"),
        cluster_member_result(1, "replica"),
    ];
    let specs = [
        ClusterMemberSpec {
            role: "monitor".to_string(),
            node_id: None,
            ordinal: 0,
            hostname: None,
        },
        ClusterMemberSpec {
            role: "replica".to_string(),
            node_id: None,
            ordinal: 1,
            hostname: None,
        },
    ];

    let (created, base_port) = precreate_cluster_members(&db, 7, &results, &specs)
        .await
        .expect("the full topology should commit");
    assert_eq!(created.len(), 2);
    assert_eq!(base_port, 6070, "service 7 keeps its historical block");

    let log = db.into_transaction_log();
    assert_eq!(
        log.len(),
        1,
        "the port reservation and all member inserts must commit as one transaction"
    );
    let statements = log[0].statements();
    let lock = statements
        .iter()
        .position(|statement| statement.sql.contains("pg_advisory_xact_lock"))
        .expect("block selection must be serialized by an advisory lock");
    let first_insert = statements
        .iter()
        .position(|statement| statement.sql.starts_with("INSERT INTO \"service_members\""))
        .expect("members are inserted");
    assert!(
        lock < first_insert,
        "the lock must be held before any row is written"
    );
    let inserts: Vec<String> = statements
        .iter()
        .filter(|statement| statement.sql.starts_with("INSERT INTO \"service_members\""))
        .map(|statement| format!("{:?}", statement.values))
        .collect();
    assert_eq!(inserts.len(), 2);
    // The reserved ports are written with the rows, before commit, so a
    // concurrent creation sees the block as taken.
    assert!(inserts[0].contains("Int(Some(6070))"), "{}", inserts[0]);
    assert!(inserts[1].contains("Int(Some(6071))"), "{}", inserts[1]);
}

/// Regression: the port block was chosen without being reserved, and
/// ports were only saved after the members started, so two clusters
/// created at the same time could pick the same block and the second
/// failed binding it. Exercised against real PostgreSQL so the advisory
/// lock and row visibility are the real ones.
#[tokio::test]
async fn concurrent_cluster_creations_reserve_distinct_port_blocks() {
    use sea_orm::IntoActiveModel;
    use temps_database::test_utils::{is_container_runtime_unavailable, TestDatabase};

    let test_db = match TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
            println!("Skipping port reservation test: {error}");
            return;
        }
        Err(error) => panic!("port reservation test database setup failed: {error}"),
    };
    let db = test_db.connection_arc();

    // Past the historical formula, both ids fall back to "first free
    // block" — exactly where two creations could collide.
    for id in [6001, 6002] {
        let mut service = encrypted_service_model(id, serde_json::json!({}));
        service.name = format!("ha-pg-{id}");
        service.slug = Some(format!("ha-pg-{id}"));
        service.topology = "cluster".to_string();
        let mut active = service.into_active_model();
        active.id = Set(id);
        active
            .insert(db.as_ref())
            .await
            .expect("insert cluster service");
    }

    let topology = |prefix: &str| {
        let results: Vec<ClusterMemberResult> = ["monitor", "replica", "replica"]
            .iter()
            .enumerate()
            .map(|(i, role)| ClusterMemberResult {
                ordinal: i as i32,
                role: role.to_string(),
                container_id: String::new(),
                container_name: format!("{prefix}-{i}"),
                port: None,
                status: "pending".to_string(),
            })
            .collect();
        let specs: Vec<ClusterMemberSpec> = results
            .iter()
            .map(|r| ClusterMemberSpec {
                role: r.role.clone(),
                node_id: None,
                ordinal: r.ordinal,
                hostname: None,
            })
            .collect();
        (results, specs)
    };
    let (results_a, specs_a) = topology("postgres-ha-pg-6001");
    let (results_b, specs_b) = topology("postgres-ha-pg-6002");

    let (a, b) = tokio::join!(
        precreate_cluster_members(db.as_ref(), 6001, &results_a, &specs_a),
        precreate_cluster_members(db.as_ref(), 6002, &results_b, &specs_b),
    );
    let (_, base_a) = a.expect("first creation reserves a block");
    let (_, base_b) = b.expect("second creation reserves a block");

    assert_ne!(base_a, base_b, "concurrent clusters must not share a block");
    assert!(
        base_a.abs_diff(base_b) >= CLUSTER_PORT_BLOCK_SIZE,
        "blocks {base_a} and {base_b} overlap"
    );
    let stored: Vec<Option<i32>> = service_members::Entity::find()
        .order_by_asc(service_members::Column::Port)
        .all(db.as_ref())
        .await
        .expect("read members")
        .into_iter()
        .map(|m| m.port)
        .collect();
    assert_eq!(stored.len(), 6);
    assert!(
        stored.iter().all(Option::is_some),
        "ports are reserved with the rows: {stored:?}"
    );
    let unique: BTreeSet<Option<i32>> = stored.iter().copied().collect();
    assert_eq!(unique.len(), 6, "no two members share a port: {stored:?}");
}

/// A member list that does not start with the monitor would give a data
/// member the monitor's port; refuse before writing anything.
#[tokio::test]
async fn precreate_refuses_members_sharing_a_host_port() {
    let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
        .append_exec_results([sea_orm::MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .append_query_results([Vec::<std::collections::BTreeMap<&str, sea_orm::Value>>::new()])
        .append_query_results([vec![service_member_model(1, 0, "replica")]])
        .into_connection();
    let results = [
        cluster_member_result(0, "replica"),
        cluster_member_result(1, "monitor"),
    ];
    let specs = [
        ClusterMemberSpec {
            role: "replica".to_string(),
            node_id: None,
            ordinal: 0,
            hostname: None,
        },
        ClusterMemberSpec {
            role: "monitor".to_string(),
            node_id: None,
            ordinal: 1,
            hostname: None,
        },
    ];

    let err = precreate_cluster_members(&db, 7, &results, &specs)
        .await
        .expect_err("two members cannot share port 6070");
    assert!(
        matches!(&err, ExternalServiceError::ClusterPortsUnavailable { service_id: 7, reason } if reason.contains("6070")),
        "{err:?}"
    );
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_initialize_cluster_not_found() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    let result = manager
        .initialize_cluster(
            99999,
            &[ClusterMemberRequest {
                role: "primary".to_string(),
                node_id: None,
            }],
        )
        .await;

    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        ExternalServiceError::ServiceNotFound { id: 99999 }
    ));
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_initialize_cluster_unsupported_type() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    // S3 does not support cluster topology
    let service_id = insert_test_service(
        manager.db.as_ref(),
        "test-s3-cluster",
        "s3",
        "cluster",
        "creating",
    )
    .await;

    let result = manager
        .initialize_cluster(
            service_id,
            &[ClusterMemberRequest {
                role: "primary".to_string(),
                node_id: None,
            }],
        )
        .await;

    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        ExternalServiceError::InitializationFailed { .. }
    ));
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_initialize_cluster_invalid_role() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    let service_id = insert_test_service(
        manager.db.as_ref(),
        "test-pg-bad-role",
        "postgres",
        "cluster",
        "creating",
    )
    .await;

    let result = manager
        .initialize_cluster(
            service_id,
            &[ClusterMemberRequest {
                role: "invalid_role".to_string(),
                node_id: None,
            }],
        )
        .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, ExternalServiceError::ParameterValidationFailed { .. }),
        "Expected ParameterValidationFailed, got: {:?}",
        err
    );
}

fn multi_node_settings_row(private_address: Option<&str>) -> settings::Model {
    settings::Model {
        id: 1,
        data: serde_json::json!({ "multi_node": { "private_address": private_address } }),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn network_config_row(wireguard_enabled: bool) -> temps_entities::network_config::Model {
    temps_entities::network_config::Model {
        id: 1,
        compute_pool_cidr: "172.20.0.0/16".to_string(),
        subnet_prefix_len: 24,
        transport: "vxlan".to_string(),
        vxlan_vni: 4242,
        vxlan_port: 4789,
        underlay_mtu: 1500,
        control_plane_compute_cidr: None,
        control_plane_underlay_address: None,
        control_plane_overlay_ready: false,
        control_plane_setup_generation: 0,
        wireguard_enabled,
        wireguard_cidr: "10.201.0.0/16".to_string(),
        wireguard_port: 51820,
        control_plane_wg_public_key: None,
        control_plane_wg_endpoint: None,
        node_api_port: None,
        mesh_hub_node_id: None,
        mesh_hub_control_plane: false,
        compute_pool_confirmed_at: None,
        docker_network_name: "temps0".into(),
        updated_at: Utc::now(),
    }
}

#[tokio::test]
async fn control_plane_member_address_reads_operator_private_address() {
    let manager = mock_service_manager_with_db(Arc::new(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![multi_node_settings_row(Some("10.52.0.10"))]])
            .append_query_results([vec![network_config_row(false)]])
            .into_connection(),
    ));

    let ip = manager
        .control_plane_member_address("ha-pg")
        .await
        .expect("a configured private address must be used");
    assert_eq!(ip, "10.52.0.10".parse::<std::net::IpAddr>().unwrap());
}

#[tokio::test]
async fn control_plane_member_address_falls_back_to_mesh_address() {
    let manager = mock_service_manager_with_db(Arc::new(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![multi_node_settings_row(None)]])
            .append_query_results([vec![network_config_row(true)]])
            .into_connection(),
    ));

    let ip = manager
        .control_plane_member_address("ha-pg")
        .await
        .expect("the control plane's mesh address is reachable from workers");
    assert_eq!(ip, "10.201.0.1".parse::<std::net::IpAddr>().unwrap());
}

#[tokio::test]
async fn control_plane_member_address_unconfigured_is_a_typed_error() {
    let manager = mock_service_manager_with_db(Arc::new(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([Vec::<settings::Model>::new()])
            .append_query_results([vec![network_config_row(false)]])
            .into_connection(),
    ));

    let err = manager
        .control_plane_member_address("ha-pg")
        .await
        .expect_err("an unconfigured control plane must not guess an address");
    match &err {
        ExternalServiceError::ControlPlaneAddressRequired { name, reason } => {
            assert_eq!(name, "ha-pg");
            assert!(
                reason.contains("no control-plane private address"),
                "{reason}"
            );
        }
        other => panic!("expected ControlPlaneAddressRequired, got {other:?}"),
    }
    let message = err.to_string();
    assert!(message.contains("Cluster 'ha-pg'"), "{message}");
    assert!(message.contains("--private-address"), "{message}");
}

#[test]
fn only_mixed_placement_spans_control_plane_and_workers() {
    let member = |node_id: Option<i32>| ClusterMemberRequest {
        role: "replica".to_string(),
        node_id,
    };
    type M = ExternalServiceManager;

    assert!(!M::cluster_spans_control_plane_and_workers(&[
        member(None),
        member(None),
        member(None)
    ]));
    assert!(!M::cluster_spans_control_plane_and_workers(&[
        member(Some(3)),
        member(Some(4)),
        member(Some(5))
    ]));
    // The console's default: monitor on the control plane, one data
    // member per worker.
    assert!(M::cluster_spans_control_plane_and_workers(&[
        member(None),
        member(Some(3)),
        member(Some(4))
    ]));
    // The node list's control-plane pseudo-node is the control plane.
    assert!(!M::cluster_spans_control_plane_and_workers(&[
        member(Some(M::CONTROL_PLANE_NODE_ID)),
        member(None)
    ]));
    assert!(M::cluster_spans_control_plane_and_workers(&[
        member(Some(M::CONTROL_PLANE_NODE_ID)),
        member(Some(3))
    ]));
}
