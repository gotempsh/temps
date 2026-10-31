// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
#[cfg(feature = "docker-tests")]
use crate::externalsvc::port_util::find_available_port;
use crate::externalsvc::port_util::find_available_port_async;
use crate::externalsvc::{ExternalService, ServiceConfig, ServiceType};
#[cfg(feature = "docker-tests")]
use bollard::query_parameters::InspectContainerOptions;
use bollard::query_parameters::StopContainerOptions;
use bollard::Docker;
use std::sync::Arc;
#[cfg(feature = "docker-tests")]
use std::time::Duration;

#[test]
fn a_long_accented_name_is_cut_on_a_character_boundary() {
    let name = format!("{}é-staging", "a".repeat(62));
    let normalized = PostgresService::normalize_database_name(&name);
    assert_eq!(normalized, "a".repeat(62));
    assert!(normalized.len() <= 63);
    // Names that already fit are unchanged, and ASCII still cuts at 63.
    assert_eq!(
        PostgresService::normalize_database_name("Shop-Prod"),
        "shop_prod"
    );
    assert_eq!(
        PostgresService::normalize_database_name(&"b".repeat(80)),
        "b".repeat(63)
    );
    assert_eq!(
        PostgresService::normalize_database_name("7days"),
        "db_7days"
    );
    assert_eq!(PostgresService::normalize_database_name(""), "");
}

#[test]
fn imported_restore_clone_uses_only_its_new_managed_container() {
    let source = PostgresConfig {
        host: "localhost".to_string(),
        port: "5432".to_string(),
        database: "app".to_string(),
        username: "app".to_string(),
        password: "test-password".to_string(),
        max_connections: 42,
        ssl_mode: Some("disable".to_string()),
        docker_image: "gotempsh/postgres-walg:18-bookworm".to_string(),
        container_name: Some("operator-owned-imported-workload".to_string()),
    };
    let clone = source.clone().for_managed_clone();
    let service = PostgresService::new(
        "new-managed".to_string(),
        Arc::new(
            Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION)
                .unwrap(),
        ),
    );
    assert_eq!(
        service.get_live_container_name(&source),
        "operator-owned-imported-workload"
    );
    assert_eq!(
        service.get_live_container_name(&clone),
        "postgres-new-managed"
    );
    assert_eq!(
        source.container_name.as_deref(),
        Some("operator-owned-imported-workload")
    );
    assert_eq!(clone.password, source.password);
    assert_eq!(clone.max_connections, source.max_connections);
    assert_eq!(clone.database, source.database);
    assert!(serde_json::to_value(clone)
        .unwrap()
        .get("container_name")
        .is_none());
}

#[test]
fn healthcheck_cmd_pins_database_when_it_differs_from_username() {
    let cmd = postgres_healthcheck_cmd("appuser", "appdb");
    assert_eq!(
        cmd,
        "exec pg_isready -h 127.0.0.1 -p 5432 -U 'appuser' -d 'appdb'"
    );
}

#[test]
fn healthcheck_cmd_escapes_single_quotes_in_username_and_database() {
    let cmd = postgres_healthcheck_cmd("a'user", "a'db");
    assert_eq!(
        cmd,
        "exec pg_isready -h 127.0.0.1 -p 5432 -U 'a'\\''user' -d 'a'\\''db'"
    );
}

#[test]
fn host_config_without_init_is_reported_as_missing_init() {
    // Created before the init process was required: Docker reports
    // `Init: null`.
    assert!(host_config_missing_init(
        &bollard::models::HostConfig::default()
    ));
    assert!(host_config_missing_init(&bollard::models::HostConfig {
        init: Some(false),
        ..Default::default()
    }));
}

#[test]
fn host_config_with_init_is_not_reported_as_missing_init() {
    assert!(!host_config_missing_init(&bollard::models::HostConfig {
        init: POSTGRES_CONTAINER_INIT,
        ..Default::default()
    }));
}

/// pgvector needs no preload library, but the WAL-G TimescaleDB variant is
/// still a TimescaleDB image and must keep `timescaledb` preloaded —
/// dropping it silently disables hypertable background workers.
#[test]
fn shared_preload_libraries_cover_new_allowlisted_images() {
    assert_eq!(
        PostgresService::shared_preload_libraries_for_image("pgvector/pgvector:pg18"),
        "pg_stat_statements"
    );
    assert_eq!(
        PostgresService::shared_preload_libraries_for_image("gotempsh/pgvector-walg:pg18"),
        "pg_stat_statements"
    );
    assert_eq!(
        PostgresService::shared_preload_libraries_for_image("gotempsh/timescaledb-walg:pg18"),
        "timescaledb,pg_stat_statements"
    );
}

/// PGDATA is derived from the image tag, so every allowlisted image must
/// yield a parseable major version — otherwise the container is created
/// with a broken data directory path.
#[test]
fn every_allowlisted_image_yields_a_pgdata_path() {
    for image in ALLOWED_POSTGRES_DOCKER_IMAGES {
        let version = PostgresService::extract_postgres_version(image)
            .unwrap_or_else(|e| panic!("no version for allowlisted image {image}: {e}"));
        assert!(
            (15..=18).contains(&version),
            "unexpected major version {version} for {image}"
        );
    }
}

/// Regression test for a bug where `init()` persisted the *requested*
/// host port even when `create_container`'s retry-on-conflict logic
/// bound the container to a *different* port. Reported symptom: the
/// service's stored config (and DB-persisted connection info) showed
/// port 5441, but `docker ps` showed the container published on 5446 —
/// any client using the stored port couldn't connect.
///
/// Reproduced deterministically by pre-occupying the requested port with
/// a plain `TcpListener` before calling `init()`, forcing Docker's own
/// bind to fail with "port is already allocated" and triggering the
/// exact retry path that used to lose track of the real port.
#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_init_persists_actual_port_after_conflict_retry() {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(d) => d,
        Err(_) => {
            println!("Docker not available, skipping");
            return;
        }
    };
    if docker.ping().await.is_err() {
        println!("Docker not available, skipping");
        return;
    }

    let requested_port = find_available_port(5432).expect("an OS-bindable port should exist");

    // Hold the port so Docker's own bind fails with "port is already
    // allocated" — the exact race `create_container`'s retry handles.
    let _blocker = std::net::TcpListener::bind(("0.0.0.0", requested_port))
        .expect("failed to occupy the port for the test");

    let docker = Arc::new(docker);
    let service = PostgresService::new("test-port-conflict-retry".to_string(), docker);

    let config = ServiceConfig {
        name: "test-postgres-conflict".to_string(),
        service_type: crate::externalsvc::ServiceType::Postgres,
        version: None,
        parameters: serde_json::json!({
            "host": "localhost",
            "port": requested_port.to_string(),
            "database": "testdb",
            "username": "testuser",
            "password": "testpass123",
            "max_connections": 100,
            "ssl_mode": "disable",
            "docker_image": "gotempsh/postgres-walg:18-bookworm"
        }),
    };

    let inferred_params = service
        .init(config)
        .await
        .expect("init should succeed by retrying on a fresh port");

    let persisted_port: u16 = inferred_params
        .get("port")
        .expect("port should be present in inferred params")
        .parse()
        .expect("port should be numeric");

    assert_ne!(
        persisted_port, requested_port,
        "persisted port must differ from the occupied port that forced a retry"
    );

    // The in-memory config used by start()/health checks must agree too.
    let cached_port: u16 = service
        .config
        .read()
        .await
        .as_ref()
        .expect("config should be set after init")
        .port
        .parse()
        .expect("cached port should be numeric");
    assert_eq!(
        cached_port, persisted_port,
        "cached config port must match what was persisted"
    );

    // And it must match the port the container is actually bound to.
    let container_name = service.get_container_name();
    let inspect = service
        .docker
        .inspect_container(
            &container_name,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .expect("container should exist");
    let bound_ports: Vec<u16> = inspect
        .network_settings
        .and_then(|ns| ns.ports)
        .into_iter()
        .flatten()
        .filter_map(|(_, bindings)| bindings)
        .flatten()
        .filter_map(|b| b.host_port.and_then(|p| p.parse().ok()))
        .collect();
    assert!(
        bound_ports.contains(&persisted_port),
        "container's actual bound ports {:?} should include the persisted port {}",
        bound_ports,
        persisted_port
    );

    let _ = service.cleanup().await;
}

/// Regression test for a standalone service that reset every client
/// connection every few minutes. The postmaster ran as PID 1, so any
/// process orphaned inside the container was reparented to it; when one
/// exited with code 2 PostgreSQL 18 logged `untracked child process ...
/// exited with exit code 2` and ran crash recovery. The usual source was
/// the healthcheck itself: Docker kills only the `sh -c` wrapper of a
/// probe that exceeds its timeout, leaving `pg_isready` behind.
///
/// Covers both halves: a new container gets an init process that absorbs
/// such an orphan, and `start()` recreates a container that predates it.
#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_orphaned_process_does_not_crash_restart_postgres() {
    use futures::StreamExt;

    let docker = match Docker::connect_with_local_defaults() {
        Ok(d) => d,
        Err(_) => {
            println!("Docker not available, skipping");
            return;
        }
    };
    if docker.ping().await.is_err() {
        println!("Docker not available, skipping");
        return;
    }

    let port = find_available_port(5432).expect("an OS-bindable port should exist");
    let docker = Arc::new(docker);
    let service = PostgresService::new("test-orphan-reaping".to_string(), docker.clone());
    let config = ServiceConfig {
        name: "test-postgres-orphan".to_string(),
        service_type: crate::externalsvc::ServiceType::Postgres,
        version: None,
        parameters: serde_json::json!({
            "host": "localhost",
            "port": port.to_string(),
            "database": "testdb",
            "username": "testuser",
            "password": "testpass123",
            "max_connections": 100,
            "ssl_mode": "disable",
            "docker_image": "gotempsh/postgres-walg:18-bookworm"
        }),
    };
    service.init(config).await.expect("init should succeed");
    let container_name = service.get_container_name();

    let inspect = docker
        .inspect_container(&container_name, None::<InspectContainerOptions>)
        .await
        .expect("container should exist");
    assert!(
        inspect
            .host_config
            .as_ref()
            .is_some_and(|host_config| !host_config_missing_init(host_config)),
        "a new PostgreSQL container must run under an init process"
    );
    let healthcheck = inspect
        .config
        .and_then(|c| c.healthcheck)
        .and_then(|h| h.test)
        .unwrap_or_default();
    assert_eq!(
        healthcheck.last().map(String::as_str),
        Some("exec pg_isready -h 127.0.0.1 -p 5432 -U 'testuser' -d 'testdb'")
    );

    // The exec'd shell returns at once; its child outlives it and then
    // exits with the code that made the postmaster treat it as a crash.
    let exec = docker
        .create_exec(
            &container_name,
            bollard::exec::CreateExecOptions {
                cmd: Some(vec!["sh", "-c", "(sleep 1; exit 2) >/dev/null 2>&1 &"]),
                ..Default::default()
            },
        )
        .await
        .expect("exec should be created");
    docker
        .start_exec(&exec.id, None::<bollard::exec::StartExecOptions>)
        .await
        .expect("exec should start");
    tokio::time::sleep(Duration::from_secs(4)).await;

    let mut logs = String::new();
    let mut stream = docker.logs(
        &container_name,
        Some(bollard::query_parameters::LogsOptions {
            stdout: true,
            stderr: true,
            ..Default::default()
        }),
    );
    while let Some(Ok(chunk)) = stream.next().await {
        logs.push_str(&chunk.to_string());
    }
    assert!(
        !logs.contains("untracked child process")
            && !logs.contains("terminating any other active server processes"),
        "an orphaned process must be reaped by init, not by the postmaster:\n{logs}"
    );

    // Swap in a container shaped like one created before the init
    // process was required, then let `start()` reconcile it.
    docker
        .remove_container(
            &container_name,
            Some(bollard::query_parameters::RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await
        .expect("container should be removable");
    docker
        .create_container(
            Some(
                bollard::query_parameters::CreateContainerOptionsBuilder::new()
                    .name(&container_name)
                    .build(),
            ),
            bollard::models::ContainerCreateBody {
                image: Some("gotempsh/postgres-walg:18-bookworm".to_string()),
                cmd: Some(vec![
                    "postgres".to_string(),
                    "-c".to_string(),
                    "archive_mode=off".to_string(),
                    "-c".to_string(),
                    format!(
                        "shared_preload_libraries={}",
                        PostgresService::shared_preload_libraries_for_image(
                            "gotempsh/postgres-walg:18-bookworm"
                        )
                    ),
                ]),
                // Same data volume and PGDATA as the real container: the
                // recreate guard refuses to replace a container whose
                // data lives anywhere else (e.g. an anonymous volume),
                // and then `start()` would keep it as it is.
                env: Some(vec![format!(
                    "PGDATA={}",
                    PostgresService::get_pgdata_path("gotempsh/postgres-walg:18-bookworm")
                        .expect("PGDATA path for the test image")
                )]),
                host_config: Some(bollard::models::HostConfig {
                    mounts: Some(vec![bollard::models::Mount {
                        target: Some("/var/lib/postgresql".to_string()),
                        source: Some(format!("{container_name}_data")),
                        typ: Some(bollard::models::MountTypeEnum::VOLUME),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .expect("legacy-shaped container should be created");

    service.start().await.expect("start should reconcile");

    let inspect = docker
        .inspect_container(&container_name, None::<InspectContainerOptions>)
        .await
        .expect("container should exist after reconcile");
    assert!(
        inspect
            .host_config
            .as_ref()
            .is_some_and(|host_config| !host_config_missing_init(host_config)),
        "start() must recreate a container that was created without an init process"
    );

    // `cleanup()` is a no-op, so remove what this test created.
    let _ = docker
        .remove_container(
            &container_name,
            Some(bollard::query_parameters::RemoveContainerOptions {
                force: true,
                v: true,
                ..Default::default()
            }),
        )
        .await;
    let _ = docker
        .remove_volume(
            &format!("{container_name}_data"),
            None::<bollard::query_parameters::RemoveVolumeOptions>,
        )
        .await;
}

#[test]
fn test_extract_postgres_version() {
    // Test various PostgreSQL image formats
    let test_cases = vec![
        ("gotempsh/postgres-walg:16-bookworm", 16),
        ("gotempsh/postgres-walg:18-bookworm", 18),
        ("postgres:16.0-alpine", 16),
        ("postgres:17.2-alpine", 17),
        ("timescale/timescaledb-ha:pg16", 16),
        ("timescale/timescaledb-ha:pg18", 18),
        ("postgres:15", 15),
        ("postgres:14.5", 14),
    ];

    for (image, expected_version) in test_cases {
        let result = PostgresService::extract_postgres_version(image);
        assert!(
            result.is_ok(),
            "Failed to extract version from image: {}",
            image
        );
        assert_eq!(
            result.unwrap(),
            expected_version,
            "Image {} should extract version {}",
            image,
            expected_version
        );
    }
}

#[test]
fn test_version_extraction_invalid_formats() {
    // Test invalid image formats
    let invalid_cases = vec![
        "postgres",            // No tag
        "postgres:latest",     // Non-numeric version
        "postgres:abc-alpine", // Non-numeric version
        "postgres:alpha",      // Non-numeric version
    ];

    for image in invalid_cases {
        let result = PostgresService::extract_postgres_version(image);
        assert!(
            result.is_err(),
            "Image {} should fail to extract version",
            image
        );
    }
}

#[test]
fn test_upgrade_version_check() {
    // Test that downgrade is prevented
    let old_config = PostgresInputConfig {
        host: "localhost".to_string(),
        port: Some("5432".to_string()),
        database: "testdb".to_string(),
        username: "testuser".to_string(),
        password: Some("testpass".to_string()),
        max_connections: 100,
        ssl_mode: Some("disable".to_string()),
        docker_image: Some("gotempsh/postgres-walg:18-bookworm".to_string()),
        container_name: None,
    };

    let downgrade_config = PostgresInputConfig {
        host: "localhost".to_string(),
        port: Some("5432".to_string()),
        database: "testdb".to_string(),
        username: "testuser".to_string(),
        password: Some("testpass".to_string()),
        max_connections: 100,
        ssl_mode: Some("disable".to_string()),
        docker_image: Some("gotempsh/postgres-walg:17-bookworm".to_string()),
        container_name: None,
    };

    let old_version =
        PostgresService::extract_postgres_version(&old_config.docker_image.clone().unwrap())
            .unwrap();
    let downgrade_version =
        PostgresService::extract_postgres_version(&downgrade_config.docker_image.clone().unwrap())
            .unwrap();

    // Verify that downgrade is detected (old >= new means no upgrade)
    assert!(
        old_version >= downgrade_version,
        "Downgrade should be detected: {} >= {}",
        old_version,
        downgrade_version
    );
}

#[test]
fn test_postgres_v17_to_v18_upgrade_config() {
    // Test the configuration for upgrading from PostgreSQL 17 to 18
    let v17_config = PostgresInputConfig {
        host: "localhost".to_string(),
        port: Some("5432".to_string()),
        database: "mydb".to_string(),
        username: "postgres".to_string(),
        password: Some("mysecretpass".to_string()),
        max_connections: 100,
        ssl_mode: Some("disable".to_string()),
        docker_image: Some("gotempsh/postgres-walg:17-bookworm".to_string()),
        container_name: None,
    };

    let v18_config = PostgresInputConfig {
        host: "localhost".to_string(),
        port: Some("5432".to_string()),
        database: "mydb".to_string(),
        username: "postgres".to_string(),
        password: Some("mysecretpass".to_string()),
        max_connections: 100,
        ssl_mode: Some("disable".to_string()),
        docker_image: Some("gotempsh/postgres-walg:18-bookworm".to_string()),
        container_name: None,
    };

    // Convert to runtime configs
    let v17_runtime: PostgresConfig = v17_config.into();
    let v18_runtime: PostgresConfig = v18_config.into();

    // Verify both configs are valid
    assert_eq!(
        v17_runtime.docker_image,
        "gotempsh/postgres-walg:17-bookworm"
    );
    assert_eq!(
        v18_runtime.docker_image,
        "gotempsh/postgres-walg:18-bookworm"
    );

    // Verify other parameters are preserved
    assert_eq!(v17_runtime.database, v18_runtime.database);
    assert_eq!(v17_runtime.username, v18_runtime.username);
    assert_eq!(v17_runtime.password, v18_runtime.password);
    assert_eq!(v17_runtime.max_connections, v18_runtime.max_connections);

    // Extract versions
    let v17_version = PostgresService::extract_postgres_version(&v17_runtime.docker_image)
        .expect("Should extract v17");
    let v18_version = PostgresService::extract_postgres_version(&v18_runtime.docker_image)
        .expect("Should extract v18");

    // Verify upgrade path is valid
    assert_eq!(v17_version, 17);
    assert_eq!(v18_version, 18);
    assert!(v18_version > v17_version, "v18 should be greater than v17");
}

#[test]
fn test_get_effective_address_baremetal_mode() {
    let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
    let service = PostgresService::new("test-effective-addr".to_string(), docker);

    let config = ServiceConfig {
        name: "test-postgres".to_string(),
        service_type: crate::externalsvc::ServiceType::Postgres,
        version: None,
        parameters: serde_json::json!({
            "host": "localhost",
            "port": "5432",
            "database": "testdb",
            "username": "postgres",
            "password": "testpass",
            "max_connections": 100,
        }),
    };

    let (host, port) = service
        .get_effective_address_for_environment(config, temps_core::ExecutionEnvironment::Host)
        .unwrap();

    // In baremetal mode, should return localhost with exposed port
    assert_eq!(host, "localhost");
    assert_eq!(port, "5432");
}

#[test]
fn test_get_effective_address_docker_mode() {
    let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
    let service = PostgresService::new("test-effective-addr-docker".to_string(), docker);

    let config = ServiceConfig {
        name: "test-postgres".to_string(),
        service_type: crate::externalsvc::ServiceType::Postgres,
        version: None,
        parameters: serde_json::json!({
            "host": "localhost",
            "port": "5432",
            "database": "testdb",
            "username": "postgres",
            "password": "testpass",
            "max_connections": 100,
        }),
    };

    let (host, port) = service
        .get_effective_address_for_environment(config, temps_core::ExecutionEnvironment::Docker)
        .unwrap();

    // In Docker mode, should return container name with internal port
    assert_eq!(host, "postgres-test-effective-addr-docker");
    assert_eq!(port, "5432"); // Internal port
}

#[test]
fn test_get_effective_address_docker_mode_uses_imported_container_name() {
    let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
    let service = PostgresService::new("imported-svc".to_string(), docker);

    let config = ServiceConfig {
        name: "imported-svc".to_string(),
        service_type: crate::externalsvc::ServiceType::Postgres,
        version: None,
        parameters: serde_json::json!({
            "host": "localhost",
            "port": "5432",
            "database": "testdb",
            "username": "postgres",
            "password": "testpass",
            "max_connections": 100,
            "container_name": "legacy-postgres",
        }),
    };

    let (host, port) = service
        .get_effective_address_for_environment(config, temps_core::ExecutionEnvironment::Docker)
        .unwrap();
    // The imported container name wins over the derived `postgres-{name}`.
    assert_eq!(host, "legacy-postgres");
    assert_eq!(port, "5432");
}

#[tokio::test]
async fn identical_image_upgrade_rejected_before_contacting_docker() {
    // An unreachable daemon makes any pull or lifecycle mutation fail;
    // the typed rejection must precede all of them.
    let docker = Arc::new(
        Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION).unwrap(),
    );
    let service = PostgresService::new("same-image-test".to_string(), docker);
    let config = ServiceConfig {
        name: "same-image-test".to_string(),
        service_type: ServiceType::Postgres,
        version: None,
        parameters: serde_json::json!({
            "docker_image": "gotempsh/postgres-walg:18-bookworm",
            "database": "app",
            "username": "app",
            "password": "test-password",
            "port": "5432",
        }),
    };
    let error = service.upgrade(config.clone(), config).await.unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<PostgresUpgradeRejected>(),
            Some(PostgresUpgradeRejected::AlreadySelected { image })
                if image == "gotempsh/postgres-walg:18-bookworm"
        ),
        "{error}"
    );
    assert!(error.to_string().contains("already uses the selected"));
}

#[test]
fn postgres_upgrade_rejected_roundtrips_through_anyhow() {
    // `ExternalServiceManager::upgrade_service` downcasts the anyhow error
    // back to this typed variant to map it to HTTP 400. Guard that the type
    // stays downcastable (a 'static std::error::Error) through the
    // `?`/`.into()` path — if it stopped being downcastable the response
    // would silently degrade to 500.
    let mv: anyhow::Error = PostgresUpgradeRejected::MajorVersionChange { from: 17, to: 18 }.into();
    assert!(matches!(
        mv.downcast_ref::<PostgresUpgradeRejected>(),
        Some(PostgresUpgradeRejected::MajorVersionChange { from: 17, to: 18 })
    ));
    assert!(mv
        .to_string()
        .contains("Cannot change PostgreSQL major version"));

    let dg: anyhow::Error = PostgresUpgradeRejected::Downgrade { from: 18, to: 17 }.into();
    assert!(matches!(
        dg.downcast_ref::<PostgresUpgradeRejected>(),
        Some(PostgresUpgradeRejected::Downgrade { .. })
    ));
    assert!(dg.to_string().contains("Cannot downgrade PostgreSQL"));
}

#[test]
fn test_validate_database_name_valid_names() {
    assert!(PostgresService::validate_database_name("mydb").is_ok());
    assert!(PostgresService::validate_database_name("project_1_production").is_ok());
    assert!(PostgresService::validate_database_name("db_test_env").is_ok());
    assert!(PostgresService::validate_database_name("a").is_ok());
    assert!(PostgresService::validate_database_name("_private").is_ok());
}

#[test]
fn test_validate_database_name_rejects_empty() {
    assert!(PostgresService::validate_database_name("").is_err());
}

#[test]
fn test_validate_database_name_rejects_sql_injection_single_quote() {
    // Classic SQL injection: ' OR 1=1 --
    assert!(PostgresService::validate_database_name("test'; DROP TABLE users--").is_err());
}

#[test]
fn test_validate_database_name_rejects_sql_injection_semicolon() {
    assert!(PostgresService::validate_database_name("mydb; DROP DATABASE production").is_err());
}

#[test]
fn test_validate_database_name_rejects_spaces() {
    assert!(PostgresService::validate_database_name("my database").is_err());
}

#[test]
fn test_validate_database_name_rejects_special_chars() {
    assert!(PostgresService::validate_database_name("db-name").is_err());
    assert!(PostgresService::validate_database_name("db.name").is_err());
    assert!(PostgresService::validate_database_name("db/name").is_err());
    assert!(PostgresService::validate_database_name("db\\name").is_err());
    assert!(PostgresService::validate_database_name("db\"name").is_err());
    assert!(PostgresService::validate_database_name("db`name").is_err());
}

#[test]
fn test_validate_database_name_rejects_uppercase() {
    // Uppercase is rejected to enforce consistency (normalize_database_name lowercases)
    assert!(PostgresService::validate_database_name("MyDatabase").is_err());
}

#[test]
fn test_validate_database_name_rejects_leading_digit() {
    assert!(PostgresService::validate_database_name("1database").is_err());
    assert!(PostgresService::validate_database_name("123").is_err());
}

#[test]
fn test_validate_database_name_rejects_too_long() {
    let long_name = "a".repeat(64);
    assert!(PostgresService::validate_database_name(&long_name).is_err());
}

#[test]
fn test_validate_database_name_accepts_max_length() {
    let max_name = "a".repeat(63);
    assert!(PostgresService::validate_database_name(&max_name).is_ok());
}

#[test]
fn test_normalize_then_validate_is_always_safe() {
    // Any input passed through normalize_database_name should pass validation
    let dangerous_inputs = vec![
        "'; DROP TABLE users--",
        "test; DELETE FROM sessions",
        "../../etc/passwd",
        "admin\x00hidden",
        "Robert'); DROP TABLE Students;--",
        "name WITH spaces AND STUFF",
        "UPPERCASE_NAME",
        "123_starts_with_number",
    ];

    for input in dangerous_inputs {
        let normalized = PostgresService::normalize_database_name(input);
        assert!(
            PostgresService::validate_database_name(&normalized).is_ok(),
            "normalize_database_name('{}') produced '{}' which failed validation",
            input,
            normalized
        );
    }
}

#[test]
fn shared_preload_libraries_default_image_is_pg_stat_statements_only() {
    let libs =
        PostgresService::shared_preload_libraries_for_image("gotempsh/postgres-walg:18-bookworm");
    assert_eq!(libs, "pg_stat_statements");
}

#[test]
fn shared_preload_libraries_timescale_image_merges_both() {
    let libs = PostgresService::shared_preload_libraries_for_image("timescale/timescaledb-ha:pg18");
    assert_eq!(
        libs, "timescaledb,pg_stat_statements",
        "timescaledb must be preloaded alongside pg_stat_statements, never dropped"
    );
}

#[test]
fn shared_preload_libraries_timescale_image_any_tag() {
    // Detection is substring-based on the image name, not tag-specific —
    // confirm a different tag still matches.
    let libs =
        PostgresService::shared_preload_libraries_for_image("timescale/timescaledb-ha:pg16-ts2.15");
    assert!(libs.contains("timescaledb"));
    assert!(libs.contains("pg_stat_statements"));
}

mod recreate_guard_docker {
    use super::*;
    use crate::externalsvc::recreate_guard::test_support::*;
    use crate::externalsvc::recreate_guard::{ManagedEngine, RecreateGuardError};

    const WALG_18: &str = "gotempsh/postgres-walg:18-bookworm";
    const WALG_17: &str = "gotempsh/postgres-walg:17-bookworm";
    const PASSWORD: &str = "apppass123";

    fn env(extra: &[&str]) -> Vec<String> {
        let mut env = vec![
            "POSTGRES_USER=app".to_string(),
            format!("POSTGRES_PASSWORD={PASSWORD}"),
            "POSTGRES_DB=app".to_string(),
        ];
        env.extend(extra.iter().map(|s| String::from(*s)));
        env
    }

    fn config(port: u16, image: &str) -> PostgresConfig {
        PostgresConfig {
            host: "localhost".into(),
            port: port.to_string(),
            database: "app".into(),
            username: "app".into(),
            password: PASSWORD.into(),
            max_connections: 50,
            ssl_mode: Some("disable".into()),
            docker_image: image.into(),
            container_name: None,
        }
    }

    fn walg_env() -> Vec<String> {
        // Never reached: archiving is refused, or verified by settings
        // only. A closed port keeps any wal-push from leaving the host.
        vec![
            "WALG_S3_PREFIX=s3://recreate-guard-test/walg".into(),
            "AWS_ACCESS_KEY_ID=test".into(),
            "AWS_SECRET_ACCESS_KEY=test".into(),
            "AWS_REGION=us-east-1".into(),
            "AWS_ENDPOINT=http://127.0.0.1:9".into(),
        ]
    }

    async fn seed(docker: &Docker, container: &str) {
        psql(
            docker,
            container,
            "CREATE TABLE accounts(id int); \
                 INSERT INTO accounts SELECT generate_series(1,7); \
                 CREATE ROLE reporting LOGIN",
        )
        .await;
    }

    async fn assert_untouched(docker: &Docker, container: &str, id: &str) {
        assert_eq!(
            container_id(docker, container).await.as_deref(),
            Some(id),
            "same container"
        );
        assert!(is_running(docker, container).await, "still running");
        assert_eq!(
            psql(docker, container, "SELECT count(*) FROM accounts").await,
            "7"
        );
        assert_eq!(psql(docker, container, "SHOW archive_mode").await, "off");
        let walg = sh_eventually(
            docker,
            container,
            "test -e /var/lib/postgresql/walg.env && echo present || echo absent",
        )
        .await;
        assert_eq!(
            walg.trim(),
            "absent",
            "a refusal must not leave walg.env behind"
        );
        assert!(set_aside_containers(docker, container).await.is_empty());
    }

    fn assert_refusal(err: &anyhow::Error) {
        let guard = err
            .downcast_ref::<RecreateGuardError>()
            .unwrap_or_else(|| panic!("expected a recreate refusal, got: {err:#}"));
        assert!(guard.is_refusal(), "{guard}");
        assert!(guard.to_string().contains("Nothing was changed"), "{guard}");
    }

    /// One mismatched layout: run it, seed it, then "Run backup"'s
    /// archiving step must refuse and leave everything as it was.
    async fn refuses(
        test: &str,
        image: &str,
        config_image: &str,
        extra_env: &[&str],
        mounts: impl FnOnce(&RunScope, &str) -> Vec<bollard::models::Mount>,
    ) {
        let Some(docker) = docker_or_skip(test).await else {
            return;
        };
        let scope = RunScope::new(&docker, test);
        let service = scope.name("svc");
        let container = postgres_container_name(&service);
        scope.track_volume(postgres_volume_names(&service)[0].clone());
        let port = find_available_port_async(&docker, 25432)
            .await
            .expect("free port");
        let mounts = mounts(&scope, &service);
        let id = run_container(
            &docker,
            &container,
            image,
            &env(extra_env),
            None,
            mounts,
            Some(("5432/tcp", port)),
        )
        .await;
        seed(&docker, &container).await;

        let provider = PostgresService::new(service.clone(), Arc::new(docker.clone()));
        let err = provider
            .enable_wal_archiving(&container, &walg_env(), &config(port, config_image), true)
            .await
            .expect_err("archiving must be refused for this layout");
        assert_refusal(&err);
        assert_untouched(&docker, &container, &id).await;
        scope.cleanup().await;
    }

    /// The #1415 root cause: a container Temps did not create, named
    /// `postgres-<service>`, keeping its cluster on the image's anonymous
    /// volume. On the reported build "Run backup" removed it and started
    /// an empty cluster on `postgres-<service>_data`.
    #[tokio::test]
    async fn issue_1415_container_on_an_anonymous_volume_is_never_recreated() {
        refuses("anon", WALG_18, WALG_18, &[], |_, _| vec![]).await;
    }

    #[tokio::test]
    async fn issue_1415_container_on_a_compose_volume_is_never_recreated() {
        refuses("compose", WALG_18, WALG_18, &[], |scope, _| {
            let v = scope.name("stack_pgdata");
            scope.track_volume(&v);
            vec![volume_mount(&v, "/var/lib/postgresql")]
        })
        .await;
    }

    #[tokio::test]
    async fn container_on_a_bind_mount_is_never_recreated() {
        refuses("bind", WALG_18, WALG_18, &[], |scope, _| {
            vec![bind_mount(&scope.bind_dir("bind"), "/var/lib/postgresql")]
        })
        .await;
    }

    /// Releases before Nov 2025 mounted the volume at
    /// `/var/lib/postgresql/data` and let the image pick PGDATA.
    #[tokio::test]
    async fn legacy_layout_is_never_recreated() {
        refuses("legacy", WALG_17, WALG_17, &[], |_, service| {
            vec![volume_mount(
                &postgres_volume_names(service)[0],
                "/var/lib/postgresql/data",
            )]
        })
        .await;
    }

    /// The service's image says 18 while the container still runs 17.
    #[tokio::test]
    async fn changed_major_version_is_never_recreated() {
        refuses(
            "major",
            WALG_17,
            WALG_18,
            &["PGDATA=/var/lib/postgresql/17/docker"],
            |_, service| {
                vec![volume_mount(
                    &postgres_volume_names(service)[0],
                    "/var/lib/postgresql",
                )]
            },
        )
        .await;
    }

    /// `start()`'s drift reconcile on a legacy container (no
    /// shared_preload_libraries flag, so always "drifted"): it must start
    /// the container unchanged instead of replacing it.
    #[tokio::test]
    async fn start_drift_on_a_legacy_container_starts_it_unchanged() {
        let Some(docker) = docker_or_skip("start_drift_legacy").await else {
            return;
        };
        let scope = RunScope::new(&docker, "drift");
        let service = scope.name("svc");
        let container = postgres_container_name(&service);
        let volume = postgres_volume_names(&service)[0].clone();
        scope.track_volume(&volume);
        let port = find_available_port_async(&docker, 25432)
            .await
            .expect("free port");
        let id = run_container(
            &docker,
            &container,
            WALG_17,
            &env(&[]),
            None,
            vec![volume_mount(&volume, "/var/lib/postgresql/data")],
            Some(("5432/tcp", port)),
        )
        .await;
        seed(&docker, &container).await;
        docker
            .stop_container(&container, None::<StopContainerOptions>)
            .await
            .expect("stop");

        let provider = PostgresService::new(service.clone(), Arc::new(docker.clone()));
        let params = serde_json::to_value(config(port, WALG_17)).unwrap();
        provider
            .force_recreate(ServiceConfig {
                name: service.clone(),
                service_type: ServiceType::Postgres,
                version: None,
                parameters: params,
            })
            .await
            .expect("start must fall back to the unchanged container");
        assert_untouched(&docker, &container, &id).await;
        scope.cleanup().await;
    }

    /// A normal start of a managed service whose volume says archiving is
    /// configured (walg.env present, e.g. a first backup interrupted
    /// before its recreate) but whose container still runs with
    /// archive_mode=off. The manager starts it through `force_recreate`
    /// with a provider it just built, so the drift is repaired, through
    /// the guard, on the same cluster.
    #[tokio::test]
    async fn start_repairs_archive_drift_on_managed_storage() {
        let Some(docker) = docker_or_skip("start_repairs_archive_drift").await else {
            return;
        };
        let scope = RunScope::new(&docker, "drift2");
        let service = scope.name("svc");
        let container = postgres_container_name(&service);
        scope.track_volume(postgres_volume_names(&service)[0].clone());
        let port = find_available_port_async(&docker, 25432)
            .await
            .expect("free port");
        let cfg = config(port, WALG_18);
        let params = serde_json::to_value(&cfg).unwrap();
        PostgresService::new(service.clone(), Arc::new(docker.clone()))
            .init(ServiceConfig {
                name: service.clone(),
                service_type: ServiceType::Postgres,
                version: None,
                parameters: params.clone(),
            })
            .await
            .expect("init");
        seed(&docker, &container).await;
        let identity = system_id_of(&docker, &container).await;
        sh_eventually(
            &docker,
            &container,
            "echo 'WALG_S3_PREFIX=s3://recreate-guard-test/walg' > /var/lib/postgresql/walg.env",
        )
        .await;
        assert_eq!(psql(&docker, &container, "SHOW archive_mode").await, "off");

        // As start_service does it: a fresh provider, then force_recreate.
        PostgresService::new(service.clone(), Arc::new(docker.clone()))
            .force_recreate(ServiceConfig {
                name: service.clone(),
                service_type: ServiceType::Postgres,
                version: None,
                parameters: params,
            })
            .await
            .expect("start with drift repair");
        assert_eq!(psql(&docker, &container, "SHOW archive_mode").await, "on");
        assert_eq!(system_id_of(&docker, &container).await, identity);
        assert_eq!(
            psql(&docker, &container, "SELECT count(*) FROM accounts").await,
            "7"
        );
        assert!(set_aside_containers(&docker, &container).await.is_empty());
        scope.cleanup().await;
    }

    async fn system_id_of(docker: &Docker, container: &str) -> Option<String> {
        crate::externalsvc::recreate_guard::fingerprint(
            docker,
            ManagedEngine::Postgres,
            container,
            "/var/lib/postgresql/18/docker",
            false,
            false,
        )
        .await
        .identity
    }

    /// A manual backup run without confirming a restart never restarts
    /// PostgreSQL: with archive_mode off, enabling archiving is refused
    /// at the point the restart would be decided, and nothing changes.
    #[tokio::test]
    async fn archiving_without_restart_consent_changes_nothing() {
        let Some(docker) = docker_or_skip("archiving_without_restart_consent").await else {
            return;
        };
        let scope = RunScope::new(&docker, "noconsent");
        let service = scope.name("svc");
        let container = postgres_container_name(&service);
        scope.track_volume(postgres_volume_names(&service)[0].clone());
        let port = find_available_port_async(&docker, 25432)
            .await
            .expect("free port");
        let provider = PostgresService::new(service.clone(), Arc::new(docker.clone()));
        let cfg = config(port, WALG_18);
        provider
            .init(ServiceConfig {
                name: service.clone(),
                service_type: ServiceType::Postgres,
                version: None,
                parameters: serde_json::to_value(&cfg).unwrap(),
            })
            .await
            .expect("init");
        seed(&docker, &container).await;
        let id = container_id(&docker, &container).await.unwrap();

        let err = provider
            .enable_wal_archiving(&container, &walg_env(), &cfg, false)
            .await
            .expect_err("a restart without consent");
        assert!(
            err.downcast_ref::<PostgresRestartNotConfirmed>().is_some(),
            "{err:#}"
        );
        assert_untouched(&docker, &container, &id).await;
        scope.cleanup().await;
    }

    /// The exact #1415 flow on a service Temps created: archive_mode off,
    /// then "Run backup"'s archiving step. It restarts PostgreSQL in a new
    /// container, and the data, roles and cluster identity are the same.
    #[tokio::test]
    async fn issue_1415_managed_service_keeps_its_data_when_archiving_is_enabled() {
        let Some(docker) = docker_or_skip("issue_1415_managed").await else {
            return;
        };
        let scope = RunScope::new(&docker, "managed");
        let service = scope.name("svc");
        let container = postgres_container_name(&service);
        scope.track_volume(postgres_volume_names(&service)[0].clone());
        let port = find_available_port_async(&docker, 25432)
            .await
            .expect("free port");
        let provider = PostgresService::new(service.clone(), Arc::new(docker.clone()));
        let cfg = config(port, WALG_18);
        provider
            .init(ServiceConfig {
                name: service.clone(),
                service_type: ServiceType::Postgres,
                version: None,
                parameters: serde_json::to_value(&cfg).unwrap(),
            })
            .await
            .expect("init");
        seed(&docker, &container).await;
        let before = crate::externalsvc::recreate_guard::fingerprint(
            &docker,
            ManagedEngine::Postgres,
            &container,
            "/var/lib/postgresql/18/docker",
            true,
            false,
        )
        .await;
        let old_id = container_id(&docker, &container).await.unwrap();

        let outcome = provider
            .enable_wal_archiving(&container, &walg_env(), &cfg, true)
            .await
            .expect("archiving on a managed service");
        assert!(outcome.restarted);
        assert_ne!(container_id(&docker, &container).await.unwrap(), old_id);
        let after = crate::externalsvc::recreate_guard::fingerprint(
            &docker,
            ManagedEngine::Postgres,
            &container,
            "/var/lib/postgresql/18/docker",
            true,
            false,
        )
        .await;
        assert_eq!(after.identity, before.identity, "same cluster");
        assert_eq!(
            psql(&docker, &container, "SELECT count(*) FROM accounts").await,
            "7"
        );
        assert_eq!(
            psql(
                &docker,
                &container,
                "SELECT count(*) FROM pg_roles WHERE rolname = 'reporting'"
            )
            .await,
            "1"
        );
        assert_eq!(psql(&docker, &container, "SHOW archive_mode").await, "on");
        assert!(set_aside_containers(&docker, &container).await.is_empty());

        // A second backup must not restart it again.
        let again = provider
            .enable_wal_archiving(&container, &walg_env(), &cfg, true)
            .await
            .expect("already archiving");
        assert!(!again.restarted);
        scope.cleanup().await;
    }
}
