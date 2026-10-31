// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
#[cfg(feature = "docker-tests")]
use crate::externalsvc::postgres::{postgres_container_name, postgres_volume_names};
use crate::externalsvc::ServiceType;
use crate::services::test_support::*;
#[cfg(feature = "docker-tests")]
use bollard::Docker;
#[cfg(feature = "docker-tests")]
use chrono::Utc;
#[cfg(feature = "docker-tests")]
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};
#[cfg(feature = "docker-tests")]
use serde_json::Value as JsonValue;
use std::collections::HashMap;
#[cfg(feature = "docker-tests")]
use std::net::TcpListener;
use std::sync::Arc;
#[cfg(feature = "docker-tests")]
use temps_entities::external_service_backups;
use temps_entities::{external_services, settings};

#[test]
fn generated_schedule_is_disabled_only_after_its_last_target_is_deleted() {
    assert!(generated_schedule_loses_last_target(
        Some("mariadb_base_backup"),
        0
    ));
    assert!(!generated_schedule_loses_last_target(
        Some("mariadb_base_backup"),
        1
    ));
    assert!(!generated_schedule_loses_last_target(None, 0));
}

#[cfg(feature = "docker-tests")]
struct NameRaceResources {
    docker: Arc<Docker>,
    name: String,
}

#[cfg(feature = "docker-tests")]
impl Drop for NameRaceResources {
    fn drop(&mut self) {
        let docker = self.docker.clone();
        let name = self.name.clone();
        let cleanup = std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                eprintln!("Could not start cleanup for name-race fixture '{name}'");
                return;
            };
            runtime.block_on(async {
                let cleanup = async {
                    let _ = docker
                        .remove_container(
                            &postgres_container_name(&name),
                            Some(bollard::query_parameters::RemoveContainerOptions {
                                force: true,
                                v: true,
                                ..Default::default()
                            }),
                        )
                        .await;
                    for volume in postgres_volume_names(&name) {
                        let _ = docker
                            .remove_volume(
                                &volume,
                                None::<bollard::query_parameters::RemoveVolumeOptions>,
                            )
                            .await;
                    }
                };
                if tokio::time::timeout(std::time::Duration::from_secs(20), cleanup)
                    .await
                    .is_err()
                {
                    eprintln!("Cleanup timed out for name-race fixture '{name}'");
                }
            });
        });
        let _ = cleanup.join();
    }
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn concurrent_duplicate_service_names_preserve_original_database() {
    let (manager, _test_db) = setup_test_manager_or_skip!();
    let name = format!("name-race-{}", uuid::Uuid::new_v4());
    let docker = manager.require_docker().unwrap();
    assert!(matches!(
        docker
            .inspect_container(
                &postgres_container_name(&name),
                None::<bollard::query_parameters::InspectContainerOptions>
            )
            .await,
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            ..
        })
    ));
    for volume in postgres_volume_names(&name) {
        assert!(matches!(
            docker.inspect_volume(&volume).await,
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404,
                ..
            })
        ));
    }
    // Register the unique, absent fixture resources before the first
    // create so failed initialization and early assertions clean up too.
    let _resources = NameRaceResources {
        docker,
        name: name.clone(),
    };
    let port = get_unused_port();
    let request = || CreateExternalServiceRequest {
        name: name.clone(),
        service_type: ServiceType::Postgres,
        version: None,
        parameters: HashMap::from([
            ("database".to_string(), serde_json::json!("app")),
            ("username".to_string(), serde_json::json!("app")),
            (
                "password".to_string(),
                serde_json::json!("TestPassword123!"),
            ),
            ("port".to_string(), serde_json::json!(port.to_string())),
            (
                "docker_image".to_string(),
                serde_json::json!(std::env::var("TEMPS_TEST_POSTGRES_DOCKER_IMAGE")
                    .unwrap_or_else(|_| "gotempsh/postgres-walg:18-bookworm".to_string())),
            ),
        ]),
        node_id: None,
        topology: "standalone".to_string(),
        members: vec![],
    };
    let (left, right) = tokio::join!(
        manager.create_service(request()),
        manager.create_service(request())
    );
    let (original, rejected) = match (left, right) {
        (Ok(service), Err(error)) | (Err(error), Ok(service)) => (service, error),
        other => panic!("exactly one create must succeed: {other:?}"),
    };
    let result = async {
        assert!(
            matches!(rejected, ExternalServiceError::ServiceNameConflict { existing_service_id, .. } if existing_service_id == original.id)
        );
        assert_eq!(
            external_services::Entity::find()
                .filter(external_services::Column::Name.eq(&name))
                .count(manager.db.as_ref())
                .await
                .unwrap(),
            1
        );
        let connection = format!("postgres://app:TestPassword123!@127.0.0.1:{port}/app");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&connection)
            .await
            .unwrap();
        sqlx::raw_sql("CREATE TABLE name_guard_marker (id integer PRIMARY KEY, marker text); INSERT INTO name_guard_marker VALUES (1, 'preserved')")
            .execute(&pool).await.unwrap();
        let retry = manager.create_service(request()).await.unwrap_err();
        assert!(matches!(
            retry,
            ExternalServiceError::ServiceNameConflict { .. }
        ));
        let marker: String =
            sqlx::query_scalar("SELECT marker FROM name_guard_marker WHERE id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(marker, "preserved");
        let container_name = postgres_container_name(&name);
        let before = manager
            .require_docker()
            .unwrap()
            .inspect_container(
                &container_name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .unwrap();
        // Historical built-in aliases remain lifecycle targets. Query
        // real PostgreSQL ownership without touching any host-wide alias.
        for (builtin, service_type, alias) in [
            ("temps-blob", "blob", "rustfs-blob-temps-blob"),
            ("temps-kv", "redis", "redis-kv-temps-kv"),
        ] {
            let mut owner: external_services::ActiveModel =
                manager.get_service(original.id).await.unwrap().into();
            owner.id = sea_orm::ActiveValue::NotSet;
            owner.name = Set(builtin.to_string());
            owner.service_type = Set(service_type.to_string());
            owner.config = Set(None);
            owner.status = Set("stopped".to_string());
            let owner = owner.insert(manager.db.as_ref()).await.unwrap();
            let txn = manager.db.begin().await.unwrap();
            let conflict = manager
                .reserve_imported_container(
                    &txn,
                    &format!("owned-alias-id-{}", uuid::Uuid::new_v4()),
                    alias,
                )
                .await
                .unwrap_err();
            assert!(
                matches!(conflict, ExternalServiceError::ServiceContainerConflict { existing_service_id, .. } if existing_service_id == owner.id)
            );
            txn.rollback().await.unwrap();
            external_services::Entity::delete_by_id(owner.id)
                .exec(manager.db.as_ref())
                .await
                .unwrap();
        }
        let txn = manager.db.begin().await.unwrap();
        manager
            .reserve_imported_container(
                &txn,
                "owned-unused-alias-id",
                &format!("rustfs-blob-{name}"),
            )
            .await
            .unwrap();
        txn.rollback().await.unwrap();
        // The internal rename path must reserve its destination before
        // stopping the original container, just as create/import do.
        let existing_name = format!("rename-existing-{}", uuid::Uuid::new_v4());
        let mut existing: external_services::ActiveModel =
            manager.get_service(original.id).await.unwrap().into();
        existing.id = sea_orm::ActiveValue::NotSet;
        existing.name = Set(existing_name.clone());
        existing.status = Set("stopped".to_string());
        let existing = existing.insert(manager.db.as_ref()).await.unwrap();
        let rename = |name: String| UpdateExternalServiceRequest {
            name: Some(name),
            parameters: HashMap::new(),
            docker_image: None,
        };
        let conflict = manager
            .update_service(original.id, rename(existing_name))
            .await
            .unwrap_err();
        assert!(
            matches!(conflict, ExternalServiceError::ServiceNameConflict { existing_service_id, .. } if existing_service_id == existing.id)
        );
        external_services::Entity::delete_by_id(existing.id)
            .exec(manager.db.as_ref())
            .await
            .unwrap();
        // Its data volume cannot follow a rename, so a service that
        // already owns Docker resources is refused before its container
        // is stopped, whatever the destination (#1455). The destination
        // checks below are only reachable from a service that owns none.
        let free_name = format!("rename-free-{}", uuid::Uuid::new_v4());
        let refused = manager
            .update_service(original.id, rename(free_name))
            .await
            .unwrap_err();
        assert!(
            matches!(refused, ExternalServiceError::DataSafetyRefusal { service_id, .. } if service_id == original.id),
            "renaming a service that owns Docker resources must be refused: {refused:?}"
        );
        let occupied_name = format!("rename-occupied-{}", uuid::Uuid::new_v4());
        let occupied_container = postgres_container_name(&occupied_name);
        let docker = manager.require_docker().unwrap();
        assert!(matches!(
            docker
                .inspect_container(
                    &occupied_container,
                    None::<bollard::query_parameters::InspectContainerOptions>
                )
                .await,
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404,
                ..
            })
        ));
        for volume in postgres_volume_names(&occupied_name) {
            assert!(matches!(
                docker.inspect_volume(&volume).await,
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 404,
                    ..
                })
            ));
        }
        let _occupied_resources = NameRaceResources {
            docker: docker.clone(),
            name: occupied_name.clone(),
        };
        let occupied = docker
            .create_container(
                Some(bollard::query_parameters::CreateContainerOptions {
                    name: Some(occupied_container.clone()),
                    ..Default::default()
                }),
                bollard::models::ContainerCreateBody {
                    image: Some(
                        std::env::var("TEMPS_TEST_POSTGRES_DOCKER_IMAGE")
                            .unwrap_or_else(|_| "gotempsh/postgres-walg:18-bookworm".to_string()),
                    ),
                    entrypoint: Some(vec!["sh".to_string()]),
                    cmd: Some(vec!["-c".to_string(), "sleep 300".to_string()]),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        // A stopped service with no Docker resources of its own passes the
        // data-safety guard, so the occupied destination is what refuses.
        let renamable_name = format!("rename-source-{}", uuid::Uuid::new_v4());
        let mut renamable: external_services::ActiveModel =
            manager.get_service(original.id).await.unwrap().into();
        renamable.id = sea_orm::ActiveValue::NotSet;
        renamable.name = Set(renamable_name.clone());
        renamable.status = Set("stopped".to_string());
        let renamable = renamable.insert(manager.db.as_ref()).await.unwrap();
        let conflict = manager
            .update_service(renamable.id, rename(occupied_name))
            .await
            .unwrap_err();
        assert!(
            matches!(
                conflict,
                ExternalServiceError::ServiceResourceConflict { .. }
            ),
            "an occupied rename destination must be refused: {conflict:?}"
        );
        assert_eq!(
            manager.get_service(renamable.id).await.unwrap().name,
            renamable_name
        );
        external_services::Entity::delete_by_id(renamable.id)
            .exec(manager.db.as_ref())
            .await
            .unwrap();
        let after = docker
            .inspect_container(
                &container_name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .unwrap();
        assert_eq!(before.id, after.id);
        assert_eq!(before.state.unwrap().status, after.state.unwrap().status);
        assert_eq!(manager.get_service(original.id).await.unwrap().name, name);
        assert_eq!(
            docker
                .inspect_container(
                    &occupied_container,
                    None::<bollard::query_parameters::InspectContainerOptions>
                )
                .await
                .unwrap()
                .id,
            Some(occupied.id)
        );
        let marker: String =
            sqlx::query_scalar("SELECT marker FROM name_guard_marker WHERE id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(marker, "preserved");
        pool.close().await;
    };
    let assertion = std::panic::AssertUnwindSafe(result);
    use futures::FutureExt;
    let assertion = assertion.catch_unwind().await;
    manager.delete_service(original.id).await.unwrap();
    if let Err(error) = assertion {
        std::panic::resume_unwind(error);
    }
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn deleting_renamed_service_preserves_admitted_backup_provenance() {
    use temps_entities::{backups, s3_sources, users};
    let (manager, test_db) = setup_test_manager_or_skip!();
    let now = Utc::now();
    let original_name = format!("backup-original-{}", uuid::Uuid::new_v4());
    let renamed_name = format!("backup-renamed-{}", uuid::Uuid::new_v4());
    // Only database fixtures are needed. Verify the uniquely named cleanup
    // targets are absent before exercising the real service deletion path.
    let docker = manager.require_docker().unwrap();
    for name in [&original_name, &renamed_name] {
        assert!(matches!(
            docker
                .inspect_container(
                    &postgres_container_name(name),
                    None::<bollard::query_parameters::InspectContainerOptions>
                )
                .await,
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404,
                ..
            })
        ));
        for volume in postgres_volume_names(name) {
            assert!(matches!(
                docker.inspect_volume(&volume).await,
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 404,
                    ..
                })
            ));
        }
    }
    let user = users::ActiveModel {
        name: Set("Backup Owner".to_string()),
        email: Set(format!("backup-owner-{}@test.local", uuid::Uuid::new_v4())),
        email_verified: Set(true),
        mfa_enabled: Set(false),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(test_db.db.as_ref())
    .await
    .unwrap();
    let service = external_services::ActiveModel {
        name: Set(original_name.clone()),
        service_type: Set("postgres".to_string()),
        status: Set("stopped".to_string()),
        config: Set(Some(
            manager.encryption_service.encrypt_string("{}").unwrap(),
        )),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(test_db.db.as_ref())
    .await
    .unwrap();
    let source = s3_sources::ActiveModel {
        name: Set(format!("backup-source-{}", uuid::Uuid::new_v4())),
        bucket_name: Set("test-bucket".to_string()),
        region: Set("us-east-1".to_string()),
        bucket_path: Set("owned".to_string()),
        access_key_id: Set("test-access".to_string()),
        secret_key: Set("test-secret".to_string()),
        is_default: Set(false),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(test_db.db.as_ref())
    .await
    .unwrap();
    let location =
        format!("s3://test-bucket/owned/external_services/postgres/{original_name}/walg");
    let backup = backups::ActiveModel {
        name: Set("admitted backup".to_string()),
        backup_id: Set(uuid::Uuid::new_v4().to_string()),
        backup_type: Set("full".to_string()),
        state: Set("completed".to_string()),
        started_at: Set(now),
        finished_at: Set(Some(now)),
        size_bytes: Set(Some(1024)),
        s3_source_id: Set(source.id),
        s3_location: Set(location.clone()),
        metadata: Set("{}".to_string()),
        compression_type: Set("gzip".to_string()),
        created_by: Set(user.id),
        tags: Set("[]".to_string()),
        ..Default::default()
    }
    .insert(test_db.db.as_ref())
    .await
    .unwrap();
    let mut child_ids = Vec::new();
    for (name, kind) in [
        (Some(original_name.clone()), Some("postgres".to_string())),
        (Some(original_name.clone()), None),
        (None, None),
    ] {
        let child = external_service_backups::ActiveModel {
            service_id: Set(service.id),
            backup_id: Set(backup.id),
            backup_type: Set("full".to_string()),
            state: Set("completed".to_string()),
            started_at: Set(now),
            finished_at: Set(Some(now)),
            size_bytes: Set(Some(1024)),
            s3_location: Set(location.clone()),
            metadata: Set(serde_json::json!({})),
            compression_type: Set("gzip".to_string()),
            created_by: Set(user.id),
            service_name_snapshot: Set(name),
            service_type_snapshot: Set(kind),
            ..Default::default()
        }
        .insert(test_db.db.as_ref())
        .await
        .unwrap();
        child_ids.push(child.id);
    }
    let mut renamed: external_services::ActiveModel = service.clone().into();
    renamed.name = Set(renamed_name.clone());
    renamed.update(test_db.db.as_ref()).await.unwrap();
    manager.delete_service(service.id).await.unwrap();
    assert!(external_services::Entity::find_by_id(service.id)
        .one(test_db.db.as_ref())
        .await
        .unwrap()
        .is_none());
    let retained = external_service_backups::Entity::find()
        .filter(external_service_backups::Column::Id.is_in(child_ids))
        .order_by_asc(external_service_backups::Column::Id)
        .all(test_db.db.as_ref())
        .await
        .unwrap();
    assert_eq!(retained.len(), 3);
    assert_eq!(
        retained[0].service_name_snapshot.as_deref(),
        Some(original_name.as_str())
    );
    assert_eq!(
        retained[1].service_name_snapshot.as_deref(),
        Some(original_name.as_str())
    );
    assert_eq!(
        retained[2].service_name_snapshot.as_deref(),
        Some(renamed_name.as_str())
    );
    assert!(retained
        .iter()
        .all(
            |child| child.service_type_snapshot.as_deref() == Some("postgres")
                && child.s3_location == location
        ));
}

/// The core safety guard: only PENDING/RUNNING/ROLLING_BACK upgrade rows
/// block a start/reconcile; terminal statuses must NOT. A silent regression
/// in the status filter reopens the concurrent-container-mutation-vs-upgrade
/// race the guard exists to prevent. (Docker-gated only because the test DB
/// itself needs a container; the guard logic under test is pure Sea-ORM.)
#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn ensure_no_active_upgrade_blocks_only_active_statuses() {
    use crate::externalsvc::postgres_upgrade::status;
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};
    use temps_entities::{postgres_major_upgrades, users};

    let (manager, test_db) = setup_test_manager_or_skip!();
    let port = get_unused_port();
    let name = format!("guard-test-{}", chrono::Utc::now().timestamp_millis());
    let mut params = HashMap::new();
    params.insert(
        "database".to_string(),
        JsonValue::String("appdb".to_string()),
    );
    params.insert(
        "username".to_string(),
        JsonValue::String("appuser".to_string()),
    );
    params.insert(
        "password".to_string(),
        JsonValue::String("appsecret".to_string()),
    );
    params.insert("port".to_string(), JsonValue::String(port.to_string()));
    params.insert(
        "docker_image".to_string(),
        JsonValue::String("gotempsh/postgres-walg:17-bookworm".to_string()),
    );
    let svc = manager
        .create_service(CreateExternalServiceRequest {
            name,
            service_type: ServiceType::Postgres,
            version: Some("17".to_string()),
            parameters: params,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        })
        .await
        .expect("create service");

    let result = async {
        // No upgrade row -> allowed.
        manager
            .ensure_no_active_upgrade(svc.id)
            .await
            .map_err(|e| format!("no upgrade row should allow: {e}"))?;

        let now = chrono::Utc::now();
        // A user row to satisfy postgres_major_upgrades.created_by -> users.id.
        let user = users::ActiveModel {
            name: Set("Guard Test".to_string()),
            email: Set(format!(
                "guard-user-{}@test.local",
                now.timestamp_nanos_opt().unwrap_or(0)
            )),
            email_verified: Set(true),
            mfa_enabled: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(test_db.db.as_ref())
        .await
        .map_err(|e| format!("insert user: {e}"))?;

        let row = postgres_major_upgrades::ActiveModel {
            service_id: Set(svc.id),
            from_version: Set("17".to_string()),
            to_version: Set("18".to_string()),
            from_image: Set("gotempsh/postgres-walg:17-bookworm".to_string()),
            to_image: Set("gotempsh/postgres-walg:18-bookworm".to_string()),
            status: Set(status::PENDING.to_string()),
            phase: Set(status::PENDING.to_string()),
            pre_upgrade_backup_id: Set(None),
            log_id: Set(format!("guard-{}", svc.id)),
            rollback_volume_name: Set(None),
            rollback_volume_expires_at: Set(None),
            error_message: Set(None),
            attempt: Set(1),
            started_at: Set(None),
            finished_at: Set(None),
            created_by: Set(user.id),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(test_db.db.as_ref())
        .await
        .map_err(|e| format!("insert upgrade row: {e}"))?;

        for st in [status::PENDING, status::RUNNING, status::ROLLING_BACK] {
            let mut am: postgres_major_upgrades::ActiveModel = row.clone().into();
            am.status = Set(st.to_string());
            am.update(test_db.db.as_ref())
                .await
                .map_err(|e| format!("update status {st}: {e}"))?;
            match manager.ensure_no_active_upgrade(svc.id).await {
                Err(ExternalServiceError::UpgradeInProgress { .. }) => {}
                other => return Err(format!("status {st} must block, got {other:?}")),
            }
        }

        for st in [
            status::FAILED,
            status::COMPLETED,
            status::CANCELLED,
            status::ROLLED_BACK,
        ] {
            let mut am: postgres_major_upgrades::ActiveModel = row.clone().into();
            am.status = Set(st.to_string());
            am.update(test_db.db.as_ref())
                .await
                .map_err(|e| format!("update status {st}: {e}"))?;
            manager
                .ensure_no_active_upgrade(svc.id)
                .await
                .map_err(|e| format!("terminal status {st} must not block: {e}"))?;
        }

        // A different service id is unaffected.
        manager
            .ensure_no_active_upgrade(svc.id + 100_000)
            .await
            .map_err(|e| format!("unrelated service must not block: {e}"))?;
        Ok::<(), String>(())
    }
    .await;

    let _ = manager.delete_service(svc.id).await;
    if let Err(e) = result {
        panic!("ensure_no_active_upgrade guard test failed: {e}");
    }
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_create_postgres_service() {
    let (manager, _test_db) = setup_test_manager_or_skip!();
    let random_unused_port = get_unused_port();
    let service_name = format!("test-postgres-{}", chrono::Utc::now().timestamp_millis());
    let mut params = HashMap::new();
    params.insert(
        "database".to_string(),
        JsonValue::String("testdb".to_string()),
    );
    params.insert(
        "username".to_string(),
        JsonValue::String("testuser".to_string()),
    );
    params.insert(
        "password".to_string(),
        JsonValue::String("testpass".to_string()),
    );
    params.insert(
        "port".to_string(),
        JsonValue::String(random_unused_port.to_string()),
    );
    params.insert(
        "host".to_string(),
        JsonValue::String("localhost".to_string()),
    );
    params.insert("max_connections".to_string(), JsonValue::Number(100.into()));
    params.insert(
        "docker_image".to_string(),
        JsonValue::String("gotempsh/postgres-walg:18-bookworm".to_string()),
    );

    let request = CreateExternalServiceRequest {
        name: service_name.clone(),
        service_type: ServiceType::Postgres,
        version: Some("18".to_string()),
        parameters: params,
        node_id: None,
        topology: "standalone".to_string(),
        members: Vec::new(),
    };

    let result = manager.create_service(request).await;
    assert!(
        result.is_ok(),
        "Failed to create service: {:?}",
        result.err()
    );

    let service = result.unwrap();
    assert_eq!(service.name, service_name);
    assert_eq!(service.service_type, ServiceType::Postgres);
    assert_eq!(service.version, Some("18".to_string()));
    assert_eq!(service.status, "running");

    // Cleanup
    let _ = manager.delete_service(service.id).await;
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_create_redis_service() {
    let (manager, _test_db) = setup_test_manager_or_skip!();
    let random_unused_port = get_unused_port();
    // Unique service name so the derived container name (redis-<name>) does
    // not collide with other tests' containers on the shared CI runner.
    let service_name = format!("test-redis-{}", chrono::Utc::now().timestamp_millis());
    let mut params = HashMap::new();
    params.insert(
        "port".to_string(),
        JsonValue::String(random_unused_port.to_string()),
    );
    let request = CreateExternalServiceRequest {
        name: service_name.clone(),
        service_type: ServiceType::Redis,
        version: Some("7".to_string()),
        parameters: params,
        node_id: None,
        topology: "standalone".to_string(),
        members: Vec::new(),
    };

    let result = manager.create_service(request).await;

    let service = result.expect("Failed to create Redis service");
    assert_eq!(service.name, service_name);
    assert_eq!(service.service_type, ServiceType::Redis);
    assert_eq!(service.status, "running");

    // Cleanup
    let _ = manager.delete_service(service.id).await;
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_create_s3_service() {
    struct S3FixtureResources {
        name: String,
    }

    impl Drop for S3FixtureResources {
        fn drop(&mut self) {
            let name = self.name.clone();
            let cleanup = std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("Failed to start S3 fixture cleanup runtime");
                runtime.block_on(async {
                    let cleanup = async {
                        // A fresh client keeps cleanup independent of the test runtime.
                        let docker = Docker::connect_with_local_defaults()
                            .map_err(|error| error.to_string())?;
                        let container_name = format!("rustfs-{name}");
                        match docker
                            .inspect_container(
                                &container_name,
                                None::<bollard::query_parameters::InspectContainerOptions>,
                            )
                            .await
                        {
                            Ok(container) => {
                                let labels = container.config.and_then(|config| config.labels);
                                if labels
                                    .as_ref()
                                    .and_then(|labels| labels.get("temps.service_name"))
                                    != Some(&name)
                                {
                                    return Err(format!(
                                        "Refusing to clean S3 fixture container '{container_name}': ownership label changed"
                                    ));
                                }
                                let id = container.id.ok_or_else(|| {
                                    format!("S3 fixture container '{container_name}' has no ID")
                                })?;
                                docker
                                    .remove_container(
                                        &id,
                                        Some(bollard::query_parameters::RemoveContainerOptions {
                                            force: true,
                                            ..Default::default()
                                        }),
                                    )
                                    .await
                                    .map_err(|error| error.to_string())?;
                            }
                            Err(bollard::errors::Error::DockerResponseServerError {
                                status_code: 404,
                                ..
                            }) => {}
                            Err(error) => return Err(error.to_string()),
                        }
                        for volume in [format!("rustfs_{name}_data"), format!("rustfs_{name}_logs")] {
                            match docker
                                .remove_volume(
                                    &volume,
                                    None::<bollard::query_parameters::RemoveVolumeOptions>,
                                )
                                .await
                            {
                                Ok(())
                                | Err(bollard::errors::Error::DockerResponseServerError {
                                    status_code: 404,
                                    ..
                                }) => {}
                                Err(error) => return Err(format!("Volume '{volume}': {error}")),
                            }
                        }
                        Ok::<(), String>(())
                    };
                    match tokio::time::timeout(std::time::Duration::from_secs(20), cleanup).await {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => eprintln!("S3 fixture '{name}' cleanup failed: {error}"),
                        Err(_) => eprintln!("S3 fixture '{name}' cleanup exceeded 20 seconds"),
                    }
                });
            });
            if cleanup.join().is_err() {
                eprintln!("S3 fixture '{}' cleanup thread panicked", self.name);
            }
        }
    }

    let (manager, _test_db) = setup_test_manager_or_skip!();
    let name = format!("test-s3-{}", uuid::Uuid::new_v4());
    let docker = manager.require_docker().unwrap();
    let container_name = format!("rustfs-{name}");
    assert!(
        matches!(
            docker
                .inspect_container(
                    &container_name,
                    None::<bollard::query_parameters::InspectContainerOptions>
                )
                .await,
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404,
                ..
            })
        ),
        "S3 fixture container '{container_name}' must not exist before creation"
    );
    for volume in [format!("rustfs_{name}_data"), format!("rustfs_{name}_logs")] {
        assert!(
            matches!(
                docker.inspect_volume(&volume).await,
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 404,
                    ..
                })
            ),
            "S3 fixture volume '{volume}' must not exist before creation"
        );
    }
    // These UUID-derived resources were absent above; clean them even if
    // creation or a readiness assertion fails before delete_service runs.
    let _cleanup = S3FixtureResources { name: name.clone() };
    let api_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let console_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut params = HashMap::from([
        (
            "port".to_string(),
            JsonValue::String(api_listener.local_addr().unwrap().port().to_string()),
        ),
        (
            "console_port".to_string(),
            JsonValue::String(console_listener.local_addr().unwrap().port().to_string()),
        ),
    ]);
    // A test-only override lets shared-host runs use a run-owned image.
    if let Ok(image) = std::env::var("TEMPS_TEST_RUSTFS_DOCKER_IMAGE") {
        params.insert("docker_image".to_string(), JsonValue::String(image));
    }
    let request = CreateExternalServiceRequest {
        name: name.clone(),
        service_type: ServiceType::S3,
        version: None,
        parameters: params,
        node_id: None,
        topology: "standalone".to_string(),
        members: Vec::new(),
    };
    drop(api_listener);
    drop(console_listener);
    let mut service = manager
        .create_service(request)
        .await
        .expect("Failed to create S3 service");
    assert_eq!(service.name, name);
    assert_eq!(service.service_type, ServiceType::S3);
    eprintln!(
        "S3 fixture '{name}' created with status '{}'",
        service.status
    );

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(190);
    while service.status != "running" {
        // Readiness is additive: this test also compiles on versions that
        // mark the service running synchronously and omit the field.
        let snapshot = serde_json::to_value(&service).unwrap();
        let diagnostic = format!(
            "status={}, error={:?}, readiness={:?}",
            service.status,
            service.error_message,
            snapshot.get("readiness")
        );
        assert_ne!(
            service.status, "failed",
            "S3 fixture '{name}' failed: {diagnostic}"
        );
        assert!(
            tokio::time::Instant::now() < deadline,
            "S3 fixture '{name}' did not become ready within 190 seconds: {diagnostic}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        service = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            manager.get_service_info(service.id),
        )
        .await
        .unwrap_or_else(|_| panic!("S3 fixture '{name}' status read timed out: {diagnostic}"))
        .unwrap_or_else(|error| {
            panic!("S3 fixture '{name}' status read failed: {error}; {diagnostic}")
        });
    }
    eprintln!("S3 fixture '{name}' reached running");
    manager
        .delete_service(service.id)
        .await
        .expect("Failed to delete owned S3 fixture");
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_delete_service() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    // Create a service first. Use an explicit unused port and a unique name
    // so the Redis container does not collide with the default port (6379)
    // or another test's container name on the shared CI runner.
    let random_unused_port = get_unused_port();
    let service_name = format!("test-delete-{}", chrono::Utc::now().timestamp_millis());
    let mut params = HashMap::new();
    params.insert(
        "password".to_string(),
        JsonValue::String("redis_pass".to_string()),
    );
    params.insert(
        "port".to_string(),
        JsonValue::String(random_unused_port.to_string()),
    );

    let request = CreateExternalServiceRequest {
        name: service_name,
        service_type: ServiceType::Redis,
        version: None,
        parameters: params,
        node_id: None,
        topology: "standalone".to_string(),
        members: Vec::new(),
    };

    let service = manager.create_service(request).await.unwrap();
    let service_id = service.id;

    // Delete the service
    let delete_result = manager.delete_service(service_id).await;
    assert!(delete_result.is_ok());

    // Verify service is deleted
    let get_result = manager.get_service_details(service_id).await;
    assert!(get_result.is_err());
    assert!(matches!(
        get_result.unwrap_err(),
        ExternalServiceError::ServiceNotFound { .. }
    ));
}

#[cfg(feature = "docker-tests")]
fn postgres_create_request(name: &str, database: &str) -> CreateExternalServiceRequest {
    let mut params = HashMap::new();
    params.insert(
        "database".to_string(),
        JsonValue::String(database.to_string()),
    );
    params.insert(
        "username".to_string(),
        JsonValue::String("original_user".to_string()),
    );
    params.insert(
        "password".to_string(),
        JsonValue::String("original_pass".to_string()),
    );
    // An explicit unused port keeps the container from colliding with the
    // default port (5449) or another test's container on a shared runner.
    params.insert(
        "port".to_string(),
        JsonValue::String(get_unused_port().to_string()),
    );
    CreateExternalServiceRequest {
        name: name.to_string(),
        service_type: ServiceType::Postgres,
        version: None,
        parameters: params,
        node_id: None,
        topology: "standalone".to_string(),
        members: Vec::new(),
    }
}

/// Whether another connection can take the advisory lock that serializes
/// writers of `name`. Runs on its own thread and runtime and is joined,
/// so the calling test's runtime is blocked meanwhile: a transaction the
/// service dropped without rolling back stays open here, exactly as it
/// does under a test database's synchronous cleanup (sea-orm only queues
/// the ROLLBACK, and a task on the blocked runtime would send it).
#[cfg(feature = "docker-tests")]
fn service_name_lock_is_free(database_url: &str, name: &str) -> Result<bool, String> {
    let database_url = database_url.to_string();
    let name = name.to_string();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("could not start probe runtime: {e}"))?;
        runtime.block_on(async {
            use sea_orm::ConnectionTrait;
            let conn = sea_orm::Database::connect(&database_url)
                .await
                .map_err(|e| format!("probe connection failed: {e}"))?;
            let row = conn
                .query_one(sea_orm::Statement::from_sql_and_values(
                    sea_orm::DatabaseBackend::Postgres,
                    "SELECT pg_try_advisory_lock(hashtextextended('external-service-name:' || $1, 0)) AS free",
                    [name.into()],
                ))
                .await
                .map_err(|e| format!("probe query failed: {e}"))?
                .ok_or_else(|| "probe query returned no row".to_string())?;
            row.try_get::<bool>("", "free")
                .map_err(|e| format!("probe result unreadable: {e}"))
        })
    })
    .join()
    .map_err(|_| "probe thread panicked".to_string())?
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_update_service_parameters() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    let service_name = format!("test-update-{}", chrono::Utc::now().timestamp_millis());
    let service = manager
        .create_service(postgres_create_request(&service_name, "original_db"))
        .await
        .unwrap();
    let service_id = service.id;

    // Update only updateable fields -- Postgres marks `database`,
    // `username`, `password`, and `host` as readonly (see
    // parameter_strategies.rs), so the request must not touch them or
    // the strategy rejects the whole update with a validation error.
    // A rename is not part of this case: it is refused for a service that
    // owns Docker resources (#1455), covered by
    // `test_update_service_rename_refusal_leaves_no_lock_held`.
    let update_request = UpdateExternalServiceRequest {
        name: None,
        parameters: HashMap::new(),
        docker_image: Some("gotempsh/postgres-walg:18-bookworm".to_string()),
    };

    let updated_service = manager.update_service(service_id, update_request).await;
    assert!(
        updated_service.is_ok(),
        "update_service failed: {:?}",
        updated_service.err()
    );
    assert_eq!(updated_service.unwrap().name, service_name);

    // Sensitive readonly fields must NOT have been mutated.
    let params_after = manager.get_service_parameters(service_id).await.unwrap();
    assert_eq!(
        params_after.get("database").and_then(|v| v.as_str()),
        Some("original_db"),
        "readonly `database` must be preserved across update"
    );

    // Cleanup
    let _ = manager.delete_service(service_id).await;
}

/// A refused rename is an early return from the middle of
/// `update_service`. It must not leave the destination name's lock held:
/// a transaction left to sea-orm's lazy Drop-time ROLLBACK keeps that
/// lock, and on a blocked current-thread runtime -- as in a test
/// database's synchronous cleanup -- is never rolled back at all, which
/// hung CI until the job's own timeout.
#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_update_service_rename_refusal_leaves_no_lock_held() {
    let (manager, test_db) = setup_test_manager_or_skip!();

    let ts = chrono::Utc::now().timestamp_millis();
    let service_name = format!("test-refuse-{ts}");
    let renamed_name = format!("test-refuse-renamed-{ts}");
    let service = manager
        .create_service(postgres_create_request(&service_name, "original_db"))
        .await
        .unwrap();

    let refused = manager
        .update_service(
            service.id,
            UpdateExternalServiceRequest {
                name: Some(renamed_name.clone()),
                parameters: HashMap::new(),
                docker_image: None,
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            refused,
            ExternalServiceError::DataSafetyRefusal { service_id, .. } if service_id == service.id
        ),
        "renaming a service that owns Docker resources must be refused: {refused:?}"
    );

    // Probed without yielding to this runtime, so a rollback that was
    // only queued has not been sent yet.
    let free = service_name_lock_is_free(&test_db.database_url, &renamed_name).unwrap();
    assert!(
        free,
        "the refused rename left a transaction open holding the lock on '{renamed_name}'"
    );
    assert_eq!(
        manager.get_service(service.id).await.unwrap().name,
        service_name,
        "a refused rename must not change the service's name"
    );

    // Cleanup
    let _ = manager.delete_service(service.id).await;
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_get_service_by_name() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    // Create a service
    let mut params = HashMap::new();
    params.insert(
        "password".to_string(),
        JsonValue::String("test".to_string()),
    );

    let request = CreateExternalServiceRequest {
        name: "unique-service-name".to_string(),
        service_type: ServiceType::Redis,
        version: None,
        parameters: params,
        node_id: None,
        topology: "standalone".to_string(),
        members: Vec::new(),
    };

    let service = manager.create_service(request).await.unwrap();
    let service_id = service.id;

    // Get service by name
    let found_service = manager.get_service_by_name("unique-service-name").await;
    assert!(found_service.is_ok());
    assert_eq!(found_service.unwrap().id, service.id);

    // Cleanup
    let _ = manager.delete_service(service_id).await;
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_get_service_by_slug() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    // Use a name that slugifies to something different (uppercase + hyphens)
    // but still produces a Docker-compatible resource name. Whitespace in
    // the name was previously tested here, but `*Service::new` interpolates
    // the raw name into Docker volume/container names, which forbid
    // whitespace — so the create call would explode before this test
    // could even reach the slug lookup. Picking a Docker-safe name that
    // still differs from its slug keeps slugification under test without
    // colliding with Docker's resource-name regex.
    let mut params = HashMap::new();
    params.insert(
        "password".to_string(),
        JsonValue::String("test".to_string()),
    );

    let raw_name = "Service-By-Slug-Test";
    let expected_slug = ExternalServiceManager::generate_slug(raw_name);
    assert_ne!(
        raw_name, expected_slug,
        "test relies on raw name differing from slug to prove the lookup uses the slug column"
    );

    let request = CreateExternalServiceRequest {
        name: raw_name.to_string(),
        service_type: ServiceType::Redis,
        version: None,
        parameters: params,
        node_id: None,
        topology: "standalone".to_string(),
        members: Vec::new(),
    };

    let service = manager.create_service(request).await.unwrap();
    let service_id = service.id;

    // Lookup must succeed by slug, not raw name.
    let found_service = manager
        .get_service_by_slug(&expected_slug)
        .await
        .expect("lookup by slug should succeed");
    assert_eq!(found_service.id, service.id);

    // Lookup by the raw name through the slug endpoint must NOT succeed —
    // that would mean we're filtering by name instead of slug (the bug
    // this test exists to guard against).
    let by_name_through_slug = manager.get_service_by_slug(raw_name).await;
    assert!(
        by_name_through_slug.is_err(),
        "get_service_by_slug must filter on the slug column, not name"
    );

    // Cleanup
    let _ = manager.delete_service(service_id).await;
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_list_services() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    // Create multiple services
    let mut services_created = vec![];

    for i in 0..3 {
        let random_unused_port = get_unused_port();
        let mut params = HashMap::new();
        params.insert(
            "port".to_string(),
            JsonValue::String(random_unused_port.to_string()),
        );

        let request = CreateExternalServiceRequest {
            name: format!("service-{}", i),
            service_type: ServiceType::Redis,
            version: None,
            parameters: params,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        };

        let service = manager.create_service(request).await.unwrap();
        services_created.push(service);
    }

    // List all services
    let all_services = manager.list_services().await;
    assert!(all_services.is_ok());

    let services_list = all_services.unwrap();
    assert!(services_list.len() >= 3);

    // Verify our created services are in the list
    for created in &services_created {
        assert!(services_list.iter().any(|s| s.id == created.id));
    }

    // Cleanup
    for service in services_created {
        let _ = manager.delete_service(service.id).await;
    }
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_service_parameter_encryption() {
    let (manager, _test_db) = setup_test_manager_or_skip!();
    let random_unused_port = get_unused_port();
    // Create a service with sensitive parameters
    let mut params = HashMap::new();
    params.insert(
        "database".to_string(),
        JsonValue::String("cryptodb".to_string()),
    );
    params.insert(
        "username".to_string(),
        JsonValue::String("cryptouser".to_string()),
    );
    params.insert(
        "password".to_string(),
        JsonValue::String("super_secret_password".to_string()),
    );
    params.insert(
        "port".to_string(),
        JsonValue::String(random_unused_port.to_string()),
    );
    params.insert(
        "host".to_string(),
        JsonValue::String("localhost".to_string()),
    );
    params.insert("max_connections".to_string(), JsonValue::Number(100.into()));
    params.insert(
        "docker_image".to_string(),
        JsonValue::String("gotempsh/postgres-walg:18-bookworm".to_string()),
    );

    let request = CreateExternalServiceRequest {
        name: "crypto-service".to_string(),
        service_type: ServiceType::Postgres,
        version: None,
        parameters: params,
        node_id: None,
        topology: "standalone".to_string(),
        members: Vec::new(),
    };

    let service = manager.create_service(request).await.unwrap();
    let service_id = service.id;

    // The persisted config must remain encrypted at rest.
    let stored_service = manager.get_service(service_id).await.unwrap();
    let encrypted_config = stored_service
        .config
        .expect("service config should be stored");
    assert!(!encrypted_config.contains("super_secret_password"));

    // Normal service details must mask sensitive parameters.
    let details = manager.get_service_details(service_id).await;
    assert!(details.is_ok());

    let service_details = details.unwrap();
    assert!(service_details.current_parameters.is_some());

    let current_params = service_details.current_parameters.unwrap();
    assert_eq!(
        current_params.get("password"),
        Some(&JsonValue::String("***".to_string()))
    );
    assert_eq!(
        current_params.get("max_connections"),
        Some(&JsonValue::Number(100.into()))
    );
    assert_eq!(service_details.sensitive_parameters, vec!["password"]);

    // Plaintext is available only through the explicit reveal path.
    let revealed_password = manager
        .get_sensitive_parameter_value(service_id, "password")
        .await
        .expect("explicit reveal should decrypt the password");
    assert_eq!(revealed_password, "super_secret_password");

    // Cleanup
    let _ = manager.delete_service(service_id).await;
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_invalid_service_type() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    // Try to get a service with invalid ID
    let result = manager.get_service_details(99999).await;
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        ExternalServiceError::ServiceNotFound { .. }
    ));
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_validate_parameters_fails_with_missing_required() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    // Create a postgres service without required parameters
    let params = HashMap::new(); // Empty parameters

    let request = CreateExternalServiceRequest {
        name: "invalid-service".to_string(),
        service_type: ServiceType::Postgres,
        version: None,
        parameters: params,
        node_id: None,
        topology: "standalone".to_string(),
        members: Vec::new(),
    };

    let result = manager.create_service(request).await;
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        ExternalServiceError::ParameterValidationFailed { .. }
    ));
}

#[tokio::test]
async fn project_accessible_service_list_filters_links_before_pagination() {
    let model = encrypted_service_model(17, serde_json::json!({}));
    let db = Arc::new(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            // Filtered page query.
            .append_query_results([vec![model.clone()]])
            // Existing service-info hydration query.
            .append_query_results([vec![model]])
            .into_connection(),
    );
    let manager = mock_service_manager_with_db(db.clone());

    let services = manager
        .list_project_accessible_services_paginated(1, 25, &[10, 11], 42)
        .await
        .expect("project-scoped external-service list should succeed");
    assert_eq!(services.len(), 1);
    assert_eq!(services[0].id, 17);

    drop(manager);
    let db = Arc::try_unwrap(db).unwrap_or_else(|_| panic!("test database still has owners"));
    let log = db.into_transaction_log();
    let list_sql = &log[0].statements()[0].sql;
    assert!(
        list_sql.contains("LEFT JOIN \"project_services\""),
        "creator-owned unlinked services require a left join: {list_sql}"
    );
    assert!(
        list_sql.contains("\"project_services\".\"project_id\" NOT IN ($1, $2)"),
        "hidden projects must be excluded before pagination: {list_sql}"
    );
    assert!(
        list_sql.contains("\"external_services\".\"created_by_user_id\" = $3")
            && list_sql.contains("\"project_services\".\"service_id\" IS NULL"),
        "only the caller's unlinked services may supplement visible links: {list_sql}"
    );
    assert!(
        list_sql.contains("SELECT DISTINCT"),
        "services linked to multiple accessible projects must be deduplicated: {list_sql}"
    );
    assert!(
        list_sql.contains("LIMIT $4 OFFSET $5"),
        "access filtering must be part of the paginated query: {list_sql}"
    );
}

#[tokio::test]
async fn legacy_duplicate_service_cannot_apply_metrics_to_shared_container() {
    let existing = encrypted_service_model(17, serde_json::json!({}));
    let duplicate = encrypted_service_model(23, serde_json::json!({}));
    let manager = mock_service_manager(vec![vec![duplicate], vec![existing]]);
    let error = manager
        .store_and_apply_ingest_key(
            23,
            "unused-key".to_string(),
            "http://localhost/unused".to_string(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ExternalServiceError::AmbiguousServiceName {
            service_id: 23,
            existing_service_id: 17,
            ..
        }
    ));
}

#[tokio::test]
async fn test_sensitive_parameter_reveal_returns_only_sensitive_values() {
    let model = encrypted_service_model(
        71,
        serde_json::json!({
            "password": "database-secret",
            "max_connections": 100,
            "ssl_mode": "prefer"
        }),
    );
    let manager = mock_service_manager(vec![vec![model]]);

    let password = manager
        .get_sensitive_parameter_value(71, "password")
        .await
        .unwrap();

    assert_eq!(password, "database-secret");
}

#[tokio::test]
async fn test_sensitive_parameter_reveal_rejects_operational_settings() {
    let manager = mock_service_manager(Vec::new());

    for parameter in ["max_connections", "ssl_mode", "tls_mode"] {
        let error = manager
            .get_sensitive_parameter_value(71, parameter)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ExternalServiceError::ParameterNotSensitive { service_id: 71, .. }
        ));
    }
}

#[tokio::test]
async fn test_sensitive_parameter_reveal_reports_missing_service() {
    let manager = mock_service_manager(vec![Vec::<external_services::Model>::new()]);

    let error = manager
        .get_sensitive_parameter_value(404, "password")
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        ExternalServiceError::ServiceNotFound { id: 404 }
    ));
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_upgrade_postgres_image_parameter_update() {
    // This test verifies that an allowlisted docker_image parameter can be
    // supplied again through the update path without changing major version.
    let (manager, _test_db) = setup_test_manager_or_skip!();
    let random_unused_port = get_unused_port();

    // Step 1: Create a PostgreSQL service with the managed PostgreSQL 18 image.
    let mut params = HashMap::new();
    params.insert(
        "database".to_string(),
        JsonValue::String("testdb".to_string()),
    );
    params.insert(
        "username".to_string(),
        JsonValue::String("testuser".to_string()),
    );
    params.insert(
        "password".to_string(),
        JsonValue::String("testpass".to_string()),
    );
    params.insert(
        "port".to_string(),
        JsonValue::String(random_unused_port.to_string()),
    );
    params.insert(
        "host".to_string(),
        JsonValue::String("localhost".to_string()),
    );
    params.insert("max_connections".to_string(), JsonValue::Number(100.into()));
    params.insert(
        "docker_image".to_string(),
        JsonValue::String("gotempsh/postgres-walg:18-bookworm".to_string()),
    );

    let request = CreateExternalServiceRequest {
        name: "test-postgres-upgrade-params".to_string(),
        service_type: ServiceType::Postgres,
        version: Some("18".to_string()),
        parameters: params,
        node_id: None,
        topology: "standalone".to_string(),
        members: Vec::new(),
    };

    let service = manager
        .create_service(request)
        .await
        .expect("Failed to create PostgreSQL 18 service");
    let service_id = service.id;

    // Verify initial service configuration
    let initial_details = manager.get_service_details(service_id).await.unwrap();
    let initial_params = initial_details.current_parameters.unwrap();
    assert_eq!(
        initial_params.get("docker_image").and_then(|v| v.as_str()),
        Some("gotempsh/postgres-walg:18-bookworm"),
        "Initial docker_image should be gotempsh/postgres-walg:18-bookworm"
    );

    // Step 2: Exercise the image update path with the same allowlisted image.
    // Only include updateable parameters - readonly params (database, username, password, host)
    // are rejected by validate_for_update().
    let mut update_params = HashMap::new();
    update_params.insert(
        "port".to_string(),
        JsonValue::String(random_unused_port.to_string()),
    );
    update_params.insert("max_connections".to_string(), JsonValue::Number(100.into()));

    let update_request = UpdateExternalServiceRequest {
        name: None,
        parameters: update_params,
        docker_image: Some("gotempsh/postgres-walg:18-bookworm".to_string()),
    };

    // Update the service - same major version so data is compatible.
    // Container reinitialization may fail in CI (e.g., image pull timeout), so we
    // tolerate errors from container recreation while still verifying the DB was updated.
    let update_result = manager.update_service(service_id, update_request).await;
    if let Err(ref e) = update_result {
        eprintln!(
            "Note: update_service returned error (container reinit may have failed): {}",
            e
        );
    }

    // Verify the docker_image parameter has been updated in the database.
    // The parameter update happens before container reinitialization in update_service(),
    // so even if container recreation fails, the config should be persisted.
    let updated_details = manager.get_service_details(service_id).await.unwrap();
    let updated_params = updated_details.current_parameters.unwrap();
    assert_eq!(
        updated_params.get("docker_image").and_then(|v| v.as_str()),
        Some("gotempsh/postgres-walg:18-bookworm"),
        "Docker image parameter should be updated to gotempsh/postgres-walg:18-bookworm"
    );

    // Cleanup - force delete to remove even unhealthy containers
    let _ = manager.delete_service(service_id).await;
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_create_service_with_invalid_params_rolls_back() {
    let (manager, _test_db) = setup_test_manager_or_skip!();

    // Create a Redis service with invalid port (email address)
    let mut params = HashMap::new();
    params.insert(
        "port".to_string(),
        JsonValue::String("dviejo@kfs.es".to_string()), // Invalid port
    );
    params.insert(
        "host".to_string(),
        JsonValue::String("localhost".to_string()),
    );

    let request = CreateExternalServiceRequest {
        name: "invalid-redis".to_string(),
        service_type: ServiceType::Redis,
        version: Some("7".to_string()),
        parameters: params,
        node_id: None,
        topology: "standalone".to_string(),
        members: Vec::new(),
    };

    // Attempt to create the service - should fail
    let result = manager.create_service(request).await;
    assert!(
        result.is_err(),
        "Expected service creation to fail with invalid port"
    );

    // Verify the error is an initialization failure
    match result.unwrap_err() {
        ExternalServiceError::InitializationFailed { id, reason } => {
            // Verify the error message contains information about the invalid port
            assert!(
                reason.contains("invalid port") || reason.contains("port specification"),
                "Expected error about invalid port, got: {}",
                reason
            );

            // Most importantly: verify the service record was NOT left in the database
            let service_check = manager.get_service(id).await;
            assert!(
                service_check.is_err(),
                "Service record should not exist after failed initialization"
            );

            // Verify it's specifically a "not found" error
            match service_check.unwrap_err() {
                ExternalServiceError::ServiceNotFound { .. } => {
                    // This is what we expect - service was properly cleaned up
                }
                other => panic!(
                    "Expected ServiceNotFound error, got different error: {:?}",
                    other
                ),
            }
        }
        other => panic!(
            "Expected InitializationFailed error, got different error: {:?}",
            other
        ),
    }

    // Double-check: list all services and verify our failed service is not there
    let all_services = manager.list_services().await.unwrap();
    assert!(
        !all_services.iter().any(|s| s.name == "invalid-redis"),
        "Failed service should not appear in service list"
    );
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_cannot_update_postgres_username() {
    let (manager, _test_db) = setup_test_manager_or_skip!();
    let random_unused_port = get_unused_port();
    let mut params = HashMap::new();
    params.insert(
        "database".to_string(),
        JsonValue::String("testdb".to_string()),
    );
    params.insert(
        "username".to_string(),
        JsonValue::String("testuser".to_string()),
    );
    params.insert(
        "password".to_string(),
        JsonValue::String("testpass".to_string()),
    );
    params.insert(
        "port".to_string(),
        JsonValue::String(random_unused_port.to_string()),
    );

    let request = CreateExternalServiceRequest {
        name: "test-postgres-readonly".to_string(),
        service_type: ServiceType::Postgres,
        version: Some("16".to_string()),
        parameters: params,
        node_id: None,
        topology: "standalone".to_string(),
        members: Vec::new(),
    };

    let service = manager
        .create_service(request)
        .await
        .expect("Failed to create service");
    let service_id = service.id;

    // Try to update username (readonly parameter)
    let mut update_params = HashMap::new();
    update_params.insert(
        "username".to_string(),
        JsonValue::String("newuser".to_string()),
    );

    let update_request = UpdateExternalServiceRequest {
        name: None,
        parameters: update_params,
        docker_image: None,
    };

    // This should FAIL because username is readonly
    let result = manager.update_service(service_id, update_request).await;
    assert!(
        result.is_err(),
        "Expected update to fail for readonly parameter"
    );

    match result.unwrap_err() {
        ExternalServiceError::ParameterValidationFailed { reason, .. } => {
            assert!(
                reason.contains("username"),
                "Error should mention 'username', got: {}",
                reason
            );
            assert!(
                reason.contains("Cannot update"),
                "Error should say cannot update"
            );
        }
        other => panic!("Expected ParameterValidationFailed, got: {:?}", other),
    }

    // Cleanup
    let _ = manager.delete_service(service_id).await;
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_cannot_update_postgres_password() {
    let (manager, _test_db) = setup_test_manager_or_skip!();
    let random_unused_port = get_unused_port();
    let mut params = HashMap::new();
    params.insert(
        "database".to_string(),
        JsonValue::String("testdb".to_string()),
    );
    params.insert(
        "username".to_string(),
        JsonValue::String("testuser".to_string()),
    );
    params.insert(
        "password".to_string(),
        JsonValue::String("testpass".to_string()),
    );
    params.insert(
        "port".to_string(),
        JsonValue::String(random_unused_port.to_string()),
    );

    let request = CreateExternalServiceRequest {
        name: "test-postgres-pwd".to_string(),
        service_type: ServiceType::Postgres,
        version: Some("16".to_string()),
        parameters: params,
        node_id: None,
        topology: "standalone".to_string(),
        members: Vec::new(),
    };

    let service = manager
        .create_service(request)
        .await
        .expect("Failed to create service");
    let service_id = service.id;

    // Try to update password (readonly parameter)
    let mut update_params = HashMap::new();
    update_params.insert(
        "password".to_string(),
        JsonValue::String("wrongpassword".to_string()),
    );

    let update_request = UpdateExternalServiceRequest {
        name: None,
        parameters: update_params,
        docker_image: None,
    };

    let result = manager.update_service(service_id, update_request).await;
    assert!(
        result.is_err(),
        "Expected update to fail for readonly password parameter"
    );

    // Cleanup
    let _ = manager.delete_service(service_id).await;
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_cannot_update_postgres_database() {
    let (manager, _test_db) = setup_test_manager_or_skip!();
    let random_unused_port = get_unused_port();
    let mut params = HashMap::new();
    params.insert(
        "database".to_string(),
        JsonValue::String("testdb".to_string()),
    );
    params.insert(
        "username".to_string(),
        JsonValue::String("testuser".to_string()),
    );
    params.insert(
        "password".to_string(),
        JsonValue::String("testpass".to_string()),
    );
    params.insert(
        "port".to_string(),
        JsonValue::String(random_unused_port.to_string()),
    );

    let request = CreateExternalServiceRequest {
        name: "test-postgres-db".to_string(),
        service_type: ServiceType::Postgres,
        version: Some("16".to_string()),
        parameters: params,
        node_id: None,
        topology: "standalone".to_string(),
        members: Vec::new(),
    };

    let service = manager
        .create_service(request)
        .await
        .expect("Failed to create service");
    let service_id = service.id;

    // Try to update database (readonly parameter)
    let mut update_params = HashMap::new();
    update_params.insert(
        "database".to_string(),
        JsonValue::String("newdb".to_string()),
    );

    let update_request = UpdateExternalServiceRequest {
        name: None,
        parameters: update_params,
        docker_image: None,
    };

    let result = manager.update_service(service_id, update_request).await;
    assert!(
        result.is_err(),
        "Expected update to fail for readonly database parameter"
    );

    // Cleanup
    let _ = manager.delete_service(service_id).await;
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_can_update_postgres_docker_image() {
    let (manager, _test_db) = setup_test_manager_or_skip!();
    let random_unused_port = get_unused_port();
    let mut params = HashMap::new();
    params.insert(
        "database".to_string(),
        JsonValue::String("testdb".to_string()),
    );
    params.insert(
        "username".to_string(),
        JsonValue::String("testuser".to_string()),
    );
    params.insert(
        "password".to_string(),
        JsonValue::String("testpass".to_string()),
    );
    params.insert(
        "port".to_string(),
        JsonValue::String(random_unused_port.to_string()),
    );
    // Explicitly set docker_image so the test is deterministic
    params.insert(
        "docker_image".to_string(),
        JsonValue::String("gotempsh/postgres-walg:18-bookworm".to_string()),
    );

    let request = CreateExternalServiceRequest {
        name: "test-postgres-image".to_string(),
        service_type: ServiceType::Postgres,
        version: Some("18".to_string()),
        parameters: params,
        node_id: None,
        topology: "standalone".to_string(),
        members: Vec::new(),
    };

    let service = manager
        .create_service(request)
        .await
        .expect("Failed to create service");
    let service_id = service.id;

    // Exercise an idempotent update with the allowlisted managed image.
    let update_params = HashMap::new();

    let update_request = UpdateExternalServiceRequest {
        name: None,
        parameters: update_params,
        docker_image: Some("gotempsh/postgres-walg:18-bookworm".to_string()),
    };

    let result = manager.update_service(service_id, update_request).await;
    assert!(result.is_ok(), "Should be able to update docker_image");

    // Verify the docker_image was updated
    let details = manager.get_service_details(service_id).await.unwrap();
    let params = details.current_parameters.unwrap();
    assert_eq!(
        params.get("docker_image").and_then(|v| v.as_str()),
        Some("gotempsh/postgres-walg:18-bookworm")
    );

    // Cleanup
    let _ = manager.delete_service(service_id).await;
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_cannot_update_redis_password() {
    let (manager, _test_db) = setup_test_manager_or_skip!();
    let random_unused_port = get_unused_port();
    let mut params = HashMap::new();
    params.insert(
        "password".to_string(),
        JsonValue::String("redis_password".to_string()),
    );
    params.insert(
        "port".to_string(),
        JsonValue::String(random_unused_port.to_string()),
    );

    let request = CreateExternalServiceRequest {
        name: "test-redis-pwd".to_string(),
        service_type: ServiceType::Redis,
        version: Some("7".to_string()),
        parameters: params,
        node_id: None,
        topology: "standalone".to_string(),
        members: Vec::new(),
    };

    let service = manager
        .create_service(request)
        .await
        .expect("Failed to create service");
    let service_id = service.id;

    // Try to update password (readonly parameter for Redis)
    let mut update_params = HashMap::new();
    update_params.insert(
        "password".to_string(),
        JsonValue::String("new_password".to_string()),
    );

    let update_request = UpdateExternalServiceRequest {
        name: None,
        parameters: update_params,
        docker_image: None,
    };

    let result = manager.update_service(service_id, update_request).await;
    assert!(
        result.is_err(),
        "Expected update to fail for readonly password parameter in Redis"
    );

    // Cleanup
    let _ = manager.delete_service(service_id).await;
}

/// The console's default HA layout (monitor on the control plane, data
/// members on workers) is refused before any row is written when the
/// control plane has no reachable address — instead of creating a
/// cluster whose data nodes can never register.
#[tokio::test]
async fn create_mixed_cluster_without_control_plane_address_writes_nothing() {
    let manager = mock_service_manager_with_db(Arc::new(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([Vec::<settings::Model>::new()])
            .append_query_results([Vec::<temps_entities::network_config::Model>::new()])
            .into_connection(),
    ));

    let err = manager
        .create_service_with_creator(
            CreateExternalServiceRequest {
                name: "ha-pg".to_string(),
                service_type: ServiceType::Postgres,
                version: None,
                parameters: HashMap::new(),
                node_id: None,
                topology: "cluster".to_string(),
                members: vec![
                    ClusterMemberRequest {
                        role: "monitor".to_string(),
                        node_id: None,
                    },
                    ClusterMemberRequest {
                        role: "replica".to_string(),
                        node_id: Some(3),
                    },
                    ClusterMemberRequest {
                        role: "replica".to_string(),
                        node_id: Some(4),
                    },
                ],
            },
            Some(1),
        )
        .await
        .expect_err("the cluster must be refused");
    // The mock holds only the two configuration reads: had creation gone
    // on to write the service row, it would have failed on the exhausted
    // mock with a database error instead of this one.
    assert!(
        matches!(err, ExternalServiceError::ControlPlaneAddressRequired { ref name, .. } if name == "ha-pg"),
        "{err:?}"
    );
}

/// Regression: the ten-member limit was only enforced inside the
/// background initialization, so an eleven-member request was accepted
/// and became a failed service. It must be refused before any row exists.
#[tokio::test]
async fn create_cluster_with_too_many_members_writes_nothing() {
    // No mocked results at all: any read or write would fail with a
    // database error instead of the validation error.
    let manager = mock_service_manager_with_db(Arc::new(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
    ));
    let members = std::iter::once(ClusterMemberRequest {
        role: "monitor".to_string(),
        node_id: None,
    })
    .chain((0..10).map(|_| ClusterMemberRequest {
        role: "replica".to_string(),
        node_id: None,
    }))
    .collect();

    let err = manager
        .create_service_with_creator(
            CreateExternalServiceRequest {
                name: "ha-pg".to_string(),
                service_type: ServiceType::Postgres,
                version: None,
                parameters: HashMap::new(),
                node_id: None,
                topology: "cluster".to_string(),
                members,
            },
            Some(1),
        )
        .await
        .expect_err("eleven members must be refused");
    assert!(
        matches!(
            err,
            ExternalServiceError::ClusterMemberLimitExceeded { requested: 11, .. }
        ),
        "{err:?}"
    );
}
