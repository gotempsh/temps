// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::externalsvc::ClusterMemberSpec;
use crate::services::test_support::*;
use bollard::Docker;
#[cfg(feature = "docker-tests")]
use chrono::Utc;
#[cfg(feature = "docker-tests")]
use sea_orm::{ActiveModelTrait, DatabaseConnection};
use std::collections::HashMap;
use std::sync::Arc;
use temps_core::EncryptionService;
use temps_entities::nodes;
#[cfg(feature = "docker-tests")]
use temps_entities::service_members;

/// #1385: a replica removed while its provisioning task was still
/// running was deleted from the database, and the task then created its
/// container anyway -- untracked, crash-looping, and surviving even the
/// cluster's deletion. Removal is refused while the member is still
/// `creating`, before anything is deleted. (A control plane restart moves
/// an abandoned `creating` row to `failed`, so this cannot block forever.)
#[tokio::test]
async fn removing_a_member_that_is_still_provisioning_is_refused() {
    for step in [
        member_provisioning_step::INSERTING_ROW,
        member_provisioning_step::PROVISIONING_CONTAINER,
    ] {
        let mut cluster = encrypted_service_model(7, serde_json::json!({}));
        cluster.topology = "cluster".to_string();
        let mut member = service_member_model(8, 3, "replica");
        member.status = "creating".to_string();
        member.provisioning_step = Some(step.to_string());
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results([vec![cluster]])
                .append_query_results([vec![member]])
                .into_connection(),
        );
        let Ok(docker) = Docker::connect_with_local_defaults() else {
            println!("Docker client configuration unavailable, skipping");
            return;
        };
        let manager = ExternalServiceManager::new(
            db.clone(),
            Arc::new(EncryptionService::new_from_password(
                "service-parameter-reveal-test",
            )),
            Arc::new(docker),
            Arc::new(temps_dns::DnsRegistry::new(db.clone())),
        );

        let error = manager
            .remove_cluster_member(7, 8)
            .await
            .expect_err("a provisioning member must not be removed");
        assert!(
            matches!(
                &error,
                ExternalServiceError::ClusterMemberProvisioning {
                    service_id: 7,
                    container_name,
                    status,
                    ..
                } if container_name == "cluster-member-3" && status.contains(step)
            ),
            "{step}: {error:?}"
        );
        assert!(
            error.to_string().contains("still being provisioned"),
            "{error}"
        );
        drop(manager);
        let db = Arc::try_unwrap(db).expect("manager released the database");
        assert_eq!(
            db.into_transaction_log().len(),
            2,
            "{step}: only the service and member lookups may run"
        );
    }
}

fn local_member_plan(container_name: &str) -> AddMemberPlan {
    AddMemberPlan {
        service_id: 7,
        spec: ClusterMemberSpec {
            role: "replica".to_string(),
            node_id: None,
            ordinal: 3,
            hostname: None,
        },
        container_name: container_name.to_string(),
        member_fqdn: "orders-3.orders.temps.local".to_string(),
        member_port: 6073,
        member_params: crate::externalsvc::postgres_cluster::ClusterMemberCreateParams {
            container_name: container_name.to_string(),
            image: "temps-test-image-never-pulled:missing".to_string(),
            environment: HashMap::new(),
            command: None,
            container_port: 6073,
            volume_path: "/var/lib/postgres".to_string(),
            resource_limits: Default::default(),
        },
        control_plane_address: None,
    }
}

/// #1385: the provisioning task claims each irreversible step on the
/// member row. When the row is gone (its cluster was deleted), it stops
/// before creating anything -- no image pull, no volume, no container --
/// and writes nothing else.
#[tokio::test]
async fn provisioning_a_deleted_member_creates_nothing() {
    let db = Arc::new(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection(),
    );
    let Some(manager) = mock_manager(db.clone()) else {
        println!("Docker client configuration unavailable, skipping");
        return;
    };

    manager
        .clone()
        .complete_add_cluster_member(8, local_member_plan("temps-test-unused-member"))
        .await;

    drop(manager);
    let db = Arc::try_unwrap(db).expect("manager released the database");
    let log = db.into_transaction_log();
    assert_eq!(log.len(), 1, "only the step claim may run: {log:?}");
    assert!(
        format!("{log:?}").contains(member_provisioning_step::PROVISIONING_CONTAINER),
        "{log:?}"
    );
}

/// #1385: a container that provisioning created after its member was
/// deleted is removed together with its data volume, instead of being
/// left running (`unless-stopped`) and crash-looping. Skips without
/// Docker.
#[tokio::test]
async fn a_container_created_for_a_deleted_member_is_removed_with_its_volume() {
    const IMAGE: &str = "redis:7-alpine";
    let db =
        Arc::new(sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection());
    let Some(manager) = mock_manager(db) else {
        println!("Docker client configuration unavailable, skipping");
        return;
    };
    let Some(docker) = manager.docker.get() else {
        println!("Docker not available, skipping");
        return;
    };
    if docker.ping().await.is_err() || docker.inspect_image(IMAGE).await.is_err() {
        println!("Docker or {IMAGE} not available, skipping");
        return;
    }
    let name = format!(
        "temps-test-orphan-member-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..12]
    );
    let volume = format!("{name}_data");
    docker
        .create_volume(bollard::models::VolumeCreateRequest {
            name: Some(volume.clone()),
            ..Default::default()
        })
        .await
        .expect("create test volume");
    docker
        .create_container(
            Some(
                bollard::query_parameters::CreateContainerOptionsBuilder::new()
                    .name(&name)
                    .build(),
            ),
            bollard::models::ContainerCreateBody {
                image: Some(IMAGE.to_string()),
                host_config: Some(bollard::models::HostConfig {
                    binds: Some(vec![format!("{volume}:/data")]),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .expect("create test container");

    manager
        .discard_member_container(&local_member_plan(&name))
        .await;

    let container_left = docker
        .inspect_container(
            &name,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .is_ok();
    let volume_left = docker.inspect_volume(&volume).await.is_ok();
    // Clean up whatever the code under test left behind.
    let _ = docker
        .remove_container(
            &name,
            Some(bollard::query_parameters::RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;
    let _ = docker
        .remove_volume(
            &volume,
            None::<bollard::query_parameters::RemoveVolumeOptions>,
        )
        .await;
    assert!(!container_left, "the orphaned container must be removed");
    assert!(
        !volume_left,
        "the orphaned container's volume must be removed"
    );
}

/// Regression for `remove_cluster_member`'s delete-protection gate
/// (routed through `member_is_live_primary` -> `primary_member_from_health`):
/// a 2-node cluster's survivor lands in `wait_primary` after failover
/// (no third node left to attach as a standby) and stays there
/// indefinitely -- it is genuinely the writable primary, not a
/// transient state. Live evidence already proved a DELETE against a
/// `wait_primary` member returns 400; this pins the same behaviour at
/// the unit level so a future refactor back to a hand-rolled
/// `"primary" | "single"` match (which previously let an operator
/// delete the cluster's only writable node) fails the fast suite
/// immediately instead of only being caught live.
#[test]
fn primary_member_from_health_blocks_deletion_of_a_wait_primary_member() {
    let health = ClusterHealthReport {
        configured_standbys: 0,
        checked_at: chrono::Utc::now(),
        monitor_response_ms: 5,
        monitor_error: None,
        members: vec![cluster_member_health("orders-2", "wait_primary")],
    };

    assert!(
        ExternalServiceManager::primary_member_from_health(&health, "orders-2"),
        "a member reported as wait_primary must be treated as the live primary"
    );

    // Sanity: an unambiguous non-primary state must not be blocked,
    // and a name absent from the health report must never match.
    let secondary_health = ClusterHealthReport {
        configured_standbys: 0,
        checked_at: chrono::Utc::now(),
        monitor_response_ms: 5,
        monitor_error: None,
        members: vec![cluster_member_health("orders-3", "secondary")],
    };
    assert!(!ExternalServiceManager::primary_member_from_health(
        &secondary_health,
        "orders-3"
    ));
    assert!(!ExternalServiceManager::primary_member_from_health(
        &health,
        "orders-does-not-exist"
    ));
}

#[tokio::test]
async fn stored_member_endpoint_resolves_local_and_remote_members() {
    let remote_node = nodes::Model {
        id: 17,
        name: "worker-17".to_owned(),
        token_hash: "hash".to_owned(),
        token_encrypted: None,
        address: "https://worker-17:3100".to_owned(),
        private_address: "10.100.0.17".to_owned(),
        public_endpoint: None,
        wg_public_key: None,
        role: "worker".to_owned(),
        status: "active".to_owned(),
        labels: serde_json::json!({}),
        capacity: serde_json::json!({}),
        last_heartbeat: None,
        edge_public_key: None,
        compute_cidr: None,
        architecture: None,
        underlay_address: None,
        mesh_wg_public_key: None,
        mesh_wg_endpoint: None,
        mesh_wg_address: None,
        failover_at: None,
        dns_resolver_running: None,
        dns_resolver_tasks_alive: None,
        dns_resolver_last_sync_at: None,
        dns_resolver_consecutive_failures: 0,
        dns_resolver_last_error: None,
        dns_resolver_record_count: None,
        deployments_without_resolver: None,
        build_limits: None,
        public_ingress_enabled: false,
        public_ingress_running: None,
        public_ingress_last_error: None,
        public_ingress_certificate_count: None,
        public_ingress_route_count: None,
        public_ingress_unsupported_route_count: None,
        public_ingress_unsupported_reasons: serde_json::json!([]),
        occupied_ranges: None,
        overlay_status: None,
        overlay_last_error: None,
        overlay_ready_cidr: None,
        overlay_remediation: None,
        overlay_network_name: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    let manager = mock_service_manager_with_db(Arc::new(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![remote_node]])
            .into_connection(),
    ));

    let local = service_member_info(1, "cluster-node-1", "node", "running");
    assert_eq!(
        manager
            .stored_member_endpoint(41, &local)
            .await
            .expect("local persisted member should resolve"),
        (LOCAL_CLUSTER_HOST.to_owned(), 5432)
    );

    let mut remote = service_member_info(2, "cluster-node-2", "node", "running");
    remote.node_id = Some(17);
    remote.port = Some(6432);
    assert_eq!(
        manager
            .stored_member_endpoint(41, &remote)
            .await
            .expect("remote persisted member should resolve through its stored node"),
        ("10.100.0.17".to_owned(), 6432)
    );
}

#[tokio::test]
async fn stored_member_endpoint_rejects_missing_and_invalid_ports() {
    let manager = mock_service_manager(vec![]);
    let mut member = service_member_info(1, "cluster-node-1", "node", "running");

    member.port = None;
    let missing = manager
        .stored_member_endpoint(52, &member)
        .await
        .expect_err("member without a stored port must be rejected");
    assert!(matches!(
        missing,
        ExternalServiceError::ParameterValidationFailed { service_id: 52, .. }
    ));

    member.port = Some(70_000);
    let invalid = manager
        .stored_member_endpoint(52, &member)
        .await
        .expect_err("member with an invalid TCP port must be rejected");
    assert!(matches!(
        invalid,
        ExternalServiceError::ParameterValidationFailed { service_id: 52, .. }
    ));

    member.port = Some(0);
    let zero = manager
        .stored_member_endpoint(52, &member)
        .await
        .expect_err("TCP port zero must be rejected");
    assert!(matches!(
        zero,
        ExternalServiceError::ParameterValidationFailed { service_id: 52, .. }
    ));
}

#[tokio::test]
async fn stored_member_endpoint_reports_missing_node_with_context() {
    let manager = mock_service_manager_with_db(Arc::new(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([Vec::<nodes::Model>::new()])
            .into_connection(),
    ));
    let mut member = service_member_info(1, "cluster-node-1", "node", "running");
    member.node_id = Some(404);

    let error = manager
        .stored_member_endpoint(63, &member)
        .await
        .expect_err("missing persisted node must be reported");
    assert!(matches!(
        error,
        ExternalServiceError::InternalError { ref reason }
            if reason.contains("cluster-node-1")
                && reason.contains("service 63")
                && reason.contains("node 404")
    ));
}

#[tokio::test]
async fn stored_member_endpoint_preserves_database_failure_context() {
    let manager = mock_service_manager_with_db(Arc::new(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("connection lost".to_owned())])
            .into_connection(),
    ));
    let mut member = service_member_info(1, "cluster-node-1", "node", "running");
    member.node_id = Some(17);

    let error = manager
        .stored_member_endpoint(74, &member)
        .await
        .expect_err("database failure must retain endpoint lookup context");
    assert!(matches!(
        error,
        ExternalServiceError::DatabaseError { ref reason }
            if reason.contains("node 17")
                && reason.contains("cluster-node-1")
                && reason.contains("service 74")
                && reason.contains("connection lost")
    ));
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_add_cluster_member_not_found() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    let result = manager.add_cluster_member(99999, "replica", None).await;

    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        ExternalServiceError::ServiceNotFound { id: 99999 }
    ));
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_add_cluster_member_rejects_standalone() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    let service_id = insert_test_service(
        manager.db.as_ref(),
        "test-add-standalone",
        "postgres",
        "standalone",
        "running",
    )
    .await;

    let result = manager
        .add_cluster_member(service_id, "replica", None)
        .await;

    assert!(matches!(
        result.unwrap_err(),
        ExternalServiceError::ParameterValidationFailed { .. }
    ));
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_add_cluster_member_rejects_non_running_status() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    let service_id = insert_test_service(
        manager.db.as_ref(),
        "test-add-failed",
        "postgres",
        "cluster",
        "failed",
    )
    .await;

    let result = manager
        .add_cluster_member(service_id, "replica", None)
        .await;

    assert!(matches!(
        result.unwrap_err(),
        ExternalServiceError::ParameterValidationFailed { .. }
    ));
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_add_cluster_member_rejects_monitor_role() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    let service_id = insert_test_service(
        manager.db.as_ref(),
        "test-add-monitor",
        "postgres",
        "cluster",
        "running",
    )
    .await;

    let result = manager
        .add_cluster_member(service_id, "monitor", None)
        .await;

    let err = result.unwrap_err();
    assert!(
        matches!(err, ExternalServiceError::ParameterValidationFailed { .. }),
        "monitor must be rejected at runtime: {:?}",
        err
    );
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_add_cluster_member_rejects_primary_role() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    let service_id = insert_test_service(
        manager.db.as_ref(),
        "test-add-primary",
        "postgres",
        "cluster",
        "running",
    )
    .await;

    let result = manager
        .add_cluster_member(service_id, "primary", None)
        .await;

    let err = result.unwrap_err();
    assert!(
        matches!(err, ExternalServiceError::ParameterValidationFailed { .. }),
        "primary is elected, must be rejected at runtime: {:?}",
        err
    );
}

// ── remove_cluster_member validation ───────────────────────────────

#[cfg(feature = "docker-tests")]
async fn insert_test_member(
    db: &DatabaseConnection,
    service_id: i32,
    role: &str,
    ordinal: i32,
    container_name: &str,
) -> i32 {
    use sea_orm::ActiveValue::Set;
    let model = service_members::ActiveModel {
        service_id: Set(service_id),
        node_id: Set(None),
        role: Set(role.to_string()),
        container_id: Set(None),
        container_name: Set(container_name.to_string()),
        hostname: Set(None),
        port: Set(None),
        status: Set("running".to_string()),
        ordinal: Set(ordinal),
        config: Set(None),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    model.insert(db).await.unwrap().id
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_remove_cluster_member_rejects_standalone() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    let service_id = insert_test_service(
        manager.db.as_ref(),
        "test-rm-standalone",
        "postgres",
        "standalone",
        "running",
    )
    .await;

    let err = manager
        .remove_cluster_member(service_id, 12345)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ExternalServiceError::ParameterValidationFailed { .. }
    ));
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_remove_cluster_member_not_found() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    let service_id = insert_test_service(
        manager.db.as_ref(),
        "test-rm-missing",
        "postgres",
        "cluster",
        "running",
    )
    .await;

    // No service_members rows — member 99999 doesn't exist.
    let err = manager
        .remove_cluster_member(service_id, 99999)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ExternalServiceError::InitializationFailed { .. }
    ));
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_remove_cluster_member_rejects_monitor() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    let service_id = insert_test_service(
        manager.db.as_ref(),
        "test-rm-monitor",
        "postgres",
        "cluster",
        "running",
    )
    .await;
    let monitor_id = insert_test_member(
        manager.db.as_ref(),
        service_id,
        "monitor",
        0,
        "postgres-test-rm-monitor-monitor",
    )
    .await;
    // Quorum-satisfying data nodes so the monitor branch is the one we trip.
    insert_test_member(
        manager.db.as_ref(),
        service_id,
        "primary",
        1,
        "postgres-test-rm-monitor-1",
    )
    .await;
    insert_test_member(
        manager.db.as_ref(),
        service_id,
        "replica",
        2,
        "postgres-test-rm-monitor-2",
    )
    .await;
    insert_test_member(
        manager.db.as_ref(),
        service_id,
        "replica",
        3,
        "postgres-test-rm-monitor-3",
    )
    .await;

    let err = manager
        .remove_cluster_member(service_id, monitor_id)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        matches!(err, ExternalServiceError::ParameterValidationFailed { .. })
            && msg.contains("monitor"),
        "monitor must be rejected: {}",
        msg
    );
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_remove_cluster_member_rejects_primary() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    let service_id = insert_test_service(
        manager.db.as_ref(),
        "test-rm-primary",
        "postgres",
        "cluster",
        "running",
    )
    .await;
    insert_test_member(
        manager.db.as_ref(),
        service_id,
        "monitor",
        0,
        "postgres-test-rm-primary-monitor",
    )
    .await;
    let primary_id = insert_test_member(
        manager.db.as_ref(),
        service_id,
        "primary",
        1,
        "postgres-test-rm-primary-1",
    )
    .await;
    insert_test_member(
        manager.db.as_ref(),
        service_id,
        "replica",
        2,
        "postgres-test-rm-primary-2",
    )
    .await;
    insert_test_member(
        manager.db.as_ref(),
        service_id,
        "replica",
        3,
        "postgres-test-rm-primary-3",
    )
    .await;

    let err = manager
        .remove_cluster_member(service_id, primary_id)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        matches!(err, ExternalServiceError::ParameterValidationFailed { .. })
            && msg.contains("primary"),
        "primary must be rejected: {}",
        msg
    );
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_remove_cluster_member_rejects_quorum_drop() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    let service_id = insert_test_service(
        manager.db.as_ref(),
        "test-rm-quorum",
        "postgres",
        "cluster",
        "running",
    )
    .await;
    insert_test_member(
        manager.db.as_ref(),
        service_id,
        "monitor",
        0,
        "postgres-test-rm-quorum-monitor",
    )
    .await;
    insert_test_member(
        manager.db.as_ref(),
        service_id,
        "primary",
        1,
        "postgres-test-rm-quorum-1",
    )
    .await;
    // Only 2 data members total; removing one drops below quorum.
    let replica_id = insert_test_member(
        manager.db.as_ref(),
        service_id,
        "replica",
        2,
        "postgres-test-rm-quorum-2",
    )
    .await;

    let err = manager
        .remove_cluster_member(service_id, replica_id)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        matches!(err, ExternalServiceError::ParameterValidationFailed { .. })
            && msg.contains("quorum"),
        "quorum violation must be rejected: {}",
        msg
    );
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_remove_cluster_member_rejects_wrong_service() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    let service_a = insert_test_service(
        manager.db.as_ref(),
        "test-rm-svc-a",
        "postgres",
        "cluster",
        "running",
    )
    .await;
    let service_b = insert_test_service(
        manager.db.as_ref(),
        "test-rm-svc-b",
        "postgres",
        "cluster",
        "running",
    )
    .await;

    // Insert a member into service_b
    let stray_id = insert_test_member(
        manager.db.as_ref(),
        service_b,
        "replica",
        1,
        "postgres-test-rm-svc-b-1",
    )
    .await;

    // Try to remove it from service_a — must refuse.
    let err = manager
        .remove_cluster_member(service_a, stray_id)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        matches!(err, ExternalServiceError::ParameterValidationFailed { .. })
            && msg.contains("does not belong"),
        "cross-service removal must be rejected: {}",
        msg
    );
}

// ── promote_cluster_member validation ─────────────────────────────

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_promote_cluster_member_rejects_standalone() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    let service_id = insert_test_service(
        manager.db.as_ref(),
        "test-promote-standalone",
        "postgres",
        "standalone",
        "running",
    )
    .await;

    let err = manager
        .promote_cluster_member(service_id, 12345)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ExternalServiceError::ParameterValidationFailed { .. }
    ));
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_promote_cluster_member_rejects_non_postgres() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    let service_id = insert_test_service(
        manager.db.as_ref(),
        "test-promote-redis",
        "redis",
        "cluster",
        "running",
    )
    .await;

    let err = manager
        .promote_cluster_member(service_id, 12345)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ExternalServiceError::ParameterValidationFailed { .. }
    ));
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_promote_cluster_member_not_found() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    let service_id = insert_test_service(
        manager.db.as_ref(),
        "test-promote-missing",
        "postgres",
        "cluster",
        "running",
    )
    .await;

    let err = manager
        .promote_cluster_member(service_id, 99999)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ExternalServiceError::InitializationFailed { .. }
    ));
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_promote_cluster_member_rejects_monitor() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    let service_id = insert_test_service(
        manager.db.as_ref(),
        "test-promote-monitor",
        "postgres",
        "cluster",
        "running",
    )
    .await;
    let monitor_id = insert_test_member(
        manager.db.as_ref(),
        service_id,
        "monitor",
        0,
        "postgres-test-promote-monitor-monitor",
    )
    .await;

    let err = manager
        .promote_cluster_member(service_id, monitor_id)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        matches!(err, ExternalServiceError::ParameterValidationFailed { .. })
            && msg.contains("monitor"),
        "monitor must be rejected: {}",
        msg
    );
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_promote_cluster_member_rejects_already_primary() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    let service_id = insert_test_service(
        manager.db.as_ref(),
        "test-promote-already",
        "postgres",
        "cluster",
        "running",
    )
    .await;
    let primary_id = insert_test_member(
        manager.db.as_ref(),
        service_id,
        "primary",
        1,
        "postgres-test-promote-already-1",
    )
    .await;

    let err = manager
        .promote_cluster_member(service_id, primary_id)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        matches!(err, ExternalServiceError::ParameterValidationFailed { .. })
            && msg.contains("already the primary"),
        "already-primary must be rejected: {}",
        msg
    );
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_promote_cluster_member_rejects_wrong_service() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    let service_a = insert_test_service(
        manager.db.as_ref(),
        "test-promote-svc-a",
        "postgres",
        "cluster",
        "running",
    )
    .await;
    let service_b = insert_test_service(
        manager.db.as_ref(),
        "test-promote-svc-b",
        "postgres",
        "cluster",
        "running",
    )
    .await;
    let stray_id = insert_test_member(
        manager.db.as_ref(),
        service_b,
        "replica",
        1,
        "postgres-test-promote-svc-b-1",
    )
    .await;

    let err = manager
        .promote_cluster_member(service_a, stray_id)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        matches!(err, ExternalServiceError::ParameterValidationFailed { .. })
            && msg.contains("does not belong"),
        "cross-service promotion must be rejected: {}",
        msg
    );
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_promote_cluster_member_rejects_not_running() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    let service_id = insert_test_service(
        manager.db.as_ref(),
        "test-promote-stopped",
        "postgres",
        "cluster",
        "running",
    )
    .await;
    // Insert a stopped replica.
    use sea_orm::ActiveValue::Set;
    let stopped = service_members::ActiveModel {
        service_id: Set(service_id),
        node_id: Set(None),
        role: Set("replica".to_string()),
        container_id: Set(None),
        container_name: Set("postgres-test-promote-stopped-1".to_string()),
        hostname: Set(None),
        port: Set(None),
        status: Set("stopped".to_string()),
        ordinal: Set(1),
        config: Set(None),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let stopped_id = stopped.insert(manager.db.as_ref()).await.unwrap().id;

    let err = manager
        .promote_cluster_member(service_id, stopped_id)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        matches!(err, ExternalServiceError::ParameterValidationFailed { .. })
            && msg.contains("not running"),
        "stopped member must be rejected: {}",
        msg
    );
}
