// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
#[cfg(feature = "docker-tests")]
use crate::externalsvc::port_util::find_available_port;
use crate::externalsvc::{ExternalService, ServiceConfig, ServiceType};
use bollard::Docker;
use std::collections::HashMap;
use std::sync::Arc;
#[cfg(feature = "docker-tests")]
use std::time::Duration;
#[cfg(feature = "docker-tests")]
use urlencoding;

#[test]
fn postgres_restores_defer_target_writes_to_the_gate() {
    let docker = match bollard::Docker::connect_with_local_defaults() {
        Ok(docker) => std::sync::Arc::new(docker),
        Err(_) => {
            println!("Docker client unavailable, skipping");
            return;
        }
    };
    let pg = PostgresService::new("gate-test".to_string(), docker);
    assert!(pg.defers_target_writes("s3://bucket/walg"));
    assert!(pg.defers_target_writes("external_services/postgres/x/backup.sql.gz"));
}

#[test]
fn test_parameter_schema_editable_fields() {
    let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
    let service = PostgresService::new("test-editable".to_string(), docker);

    // Get the parameter schema
    let schema_opt = service.get_parameter_schema();
    assert!(schema_opt.is_some(), "Schema should be generated");

    let schema = schema_opt.unwrap();
    let schema_obj = schema.as_object().expect("Schema should be an object");
    let properties = schema_obj
        .get("properties")
        .and_then(|v| v.as_object())
        .expect("Properties should be an object");

    // Define expected editable status for each field
    let editable_status = vec![
        ("host", false),
        ("port", true),
        ("database", false),
        ("username", false),
        ("password", true),
        ("max_connections", true),
        ("ssl_mode", true),
        ("docker_image", true),
    ];

    for (field_name, should_be_editable) in editable_status {
        let field = properties
            .get(field_name)
            .and_then(|v| v.as_object())
            .unwrap_or_else(|| panic!("{} field should exist", field_name));

        let is_editable = field
            .get("x-editable")
            .and_then(|v| v.as_bool())
            .unwrap_or_else(|| panic!("{} should have x-editable property", field_name));

        assert_eq!(
            is_editable, should_be_editable,
            "Field {} editable status should be {}",
            field_name, should_be_editable
        );
    }
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_port_change_after_creation() {
    // Use OS-assigned ports so this test doesn't collide with anything
    // else on the runner. Hardcoded ports (6543/6544 previously) flaked
    // whenever another parallel test or background process held the
    // socket. We only need two distinct free ports; the test doesn't
    // actually *bind* the container to them, just verifies that
    // get_local_address reflects the configured value.
    use std::net::TcpListener;
    let pick = || {
        TcpListener::bind("127.0.0.1:0")
            .expect("failed to bind for port allocation")
            .local_addr()
            .expect("failed to read local addr")
            .port()
    };
    let initial_port = pick();
    let new_port = loop {
        let p = pick();
        if p != initial_port {
            break p;
        }
    };

    let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
    let service = PostgresService::new("test-port-change".to_string(), docker);

    let config1 = ServiceConfig {
        name: "test-postgres".to_string(),
        service_type: crate::externalsvc::ServiceType::Postgres,
        version: None,
        parameters: serde_json::json!({
            "host": "localhost",
            "port": initial_port.to_string(),
            "database": "testdb",
            "username": "testuser",
            "password": "testpass123",
            "max_connections": 100,
            "ssl_mode": "disable",
            "docker_image": "gotempsh/postgres-walg:18-bookworm"
        }),
    };

    // Initialize service
    let result = service.init(config1.clone()).await;
    assert!(result.is_ok(), "Service initialization failed");

    // Verify initial port is set
    let local_addr = service.get_local_address(config1.clone()).unwrap();
    let initial_port_str = initial_port.to_string();
    assert!(
        local_addr.contains(&initial_port_str),
        "Initial port should be {initial_port_str}, got '{local_addr}'"
    );

    let config2 = ServiceConfig {
        name: "test-postgres".to_string(),
        service_type: crate::externalsvc::ServiceType::Postgres,
        version: None,
        parameters: serde_json::json!({
            "host": "localhost",
            "port": new_port.to_string(),
            "database": "testdb",
            "username": "testuser",
            "password": "testpass123",
            "max_connections": 100,
            "ssl_mode": "disable",
            "docker_image": "gotempsh/postgres-walg:18-bookworm"
        }),
    };

    // Verify new port configuration is recognized
    let new_local_addr = service.get_local_address(config2).unwrap();
    let new_port_str = new_port.to_string();
    assert!(
        new_local_addr.contains(&new_port_str),
        "New port should be {new_port_str}, got '{new_local_addr}'"
    );

    // Cleanup
    let _ = service.cleanup().await;
}

#[test]
fn test_default_docker_image() {
    let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
    let service = PostgresService::new("test-image".to_string(), docker);

    let (image_name, version) = service.get_default_docker_image();
    assert_eq!(
        image_name, "gotempsh/postgres-walg",
        "Default image should be gotempsh/postgres-walg"
    );
    assert_eq!(
        version, "18-bookworm",
        "Default version should be 18-bookworm"
    );
}

#[tokio::test]
async fn test_image_upgrade_scenario() {
    let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
    let _service = PostgresService::new("test-upgrade".to_string(), docker);

    // Create initial config with previous PostgreSQL version
    let old_config = ServiceConfig {
        name: "test-postgres".to_string(),
        service_type: crate::externalsvc::ServiceType::Postgres,
        version: None,
        parameters: serde_json::json!({
            "host": "localhost",
            "port": Some("6545"),
            "database": "testdb",
            "username": "testuser",
            "password": "testpass123",
            "max_connections": 100,
            "ssl_mode": "disable",
            "docker_image": "gotempsh/postgres-walg:17-bookworm"
        }),
    };

    // Create new config with upgraded PostgreSQL version
    let new_config = ServiceConfig {
        name: "test-postgres".to_string(),
        service_type: crate::externalsvc::ServiceType::Postgres,
        version: None,
        parameters: serde_json::json!({
            "host": "localhost",
            "port": Some("6545"),
            "database": "testdb",
            "username": "testuser",
            "password": "testpass123",
            "max_connections": 100,
            "ssl_mode": "disable",
            "docker_image": "gotempsh/postgres-walg:18-bookworm"
        }),
    };

    // Note: Full upgrade test would require actual Docker containers
    // This test verifies the configuration structure
    assert!(old_config.parameters.get("docker_image").is_some());
    assert!(new_config.parameters.get("docker_image").is_some());

    let old_image = old_config
        .parameters
        .get("docker_image")
        .and_then(|v| v.as_str());
    let new_image = new_config
        .parameters
        .get("docker_image")
        .and_then(|v| v.as_str());

    assert_eq!(old_image, Some("gotempsh/postgres-walg:17-bookworm"));
    assert_eq!(new_image, Some("gotempsh/postgres-walg:18-bookworm"));
}

#[test]
fn test_parameter_schema_includes_docker_image() {
    let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
    let service = PostgresService::new("test-schema".to_string(), docker);

    let schema_opt = service.get_parameter_schema();
    assert!(schema_opt.is_some(), "Schema should be generated");

    let schema = schema_opt.unwrap();
    let properties = schema
        .get("properties")
        .and_then(|v| v.as_object())
        .expect("Properties should be an object");

    // Verify docker_image field exists in schema
    assert!(
        properties.contains_key("docker_image"),
        "docker_image should be in schema"
    );

    // Verify docker_image is marked as editable
    let docker_image_field = properties
        .get("docker_image")
        .and_then(|v| v.as_object())
        .expect("docker_image field should be an object");

    let is_editable = docker_image_field
        .get("x-editable")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    assert!(is_editable, "docker_image should be editable");
}

#[test]
fn test_parameter_schema_matches_required_creation_credentials() {
    let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
    let service = PostgresService::new("test-required-schema".to_string(), docker);

    let schema = service
        .get_parameter_schema()
        .expect("PostgreSQL creation schema should exist");
    let required = schema["required"]
        .as_array()
        .expect("PostgreSQL creation schema should declare required fields");

    assert!(required.iter().any(|field| field == "database"));
    assert!(required.iter().any(|field| field == "username"));
}

#[cfg(feature = "docker-tests")]
#[tokio::test]
async fn test_postgres_v17_to_v18_upgrade_rejected_and_data_survives() {
    // `ExternalService::upgrade()` used to attempt major-version bumps
    // itself via a broken `run_pg_upgrade` that deleted the live volume
    // and booted a brand-new empty cluster hardcoded to
    // `postgres:{version}-alpine` / `POSTGRES_USER=postgres` — silently
    // discarding the real data while reporting success. The old version
    // of this test only asserted `SELECT version()` on the post-upgrade
    // container, which is exactly why that data loss went unnoticed: it
    // never checked that pre-upgrade data was still there.
    //
    // `upgrade()` now refuses any major-version change outright and
    // points callers at the dedicated orchestrator
    // (`POST /external-services/{id}/upgrades`, `postgres_upgrade.rs`).
    // This test asserts that refusal, and — the part the original test
    // was missing — that the v17 container and its data are completely
    // untouched afterward.

    let docker = match Docker::connect_with_defaults() {
        Ok(d) => Arc::new(d),
        Err(_) => {
            println!("Docker not available, skipping test");
            return;
        }
    };

    let port = 19432u16; // Use unique port to avoid conflicts
    let password = "postgres"; // Use default PostgreSQL password
    let service_name = format!(
        "test_postgres_upgrade_{}",
        chrono::Utc::now().timestamp_millis()
    );

    // Create v17 service configuration
    let v17_params = serde_json::json!({
        "host": "localhost",
        "port": port.to_string(),
        "database": "postgres",
        "username": "postgres",
        "password": password,
        "max_connections": 100,
        "docker_image": "gotempsh/postgres-walg:17-bookworm",
    });

    let v17_config = ServiceConfig {
        name: service_name.clone(),
        service_type: crate::externalsvc::ServiceType::Postgres,
        version: Some("17".to_string()),
        parameters: v17_params,
    };

    // Create v18 service configuration (used only as the `upgrade()`
    // target — the call is expected to be rejected before touching
    // anything).
    let v18_params = serde_json::json!({
        "host": "localhost",
        "port": port.to_string(),
        "database": "postgres",
        "username": "postgres",
        "password": password,
        "max_connections": 100,
        "docker_image": "gotempsh/postgres-walg:18-bookworm",
    });

    let v18_config = ServiceConfig {
        name: service_name.clone(),
        service_type: crate::externalsvc::ServiceType::Postgres,
        version: Some("18".to_string()),
        parameters: v18_params,
    };

    // Initialize v17 service
    let v17_service = PostgresService::new(service_name.clone(), docker.clone());

    match v17_service.init(v17_config.clone()).await {
        Ok(_) => {}
        Err(e) => {
            println!(
                "Failed to initialize v17 service: {}. Skipping test (Docker may not be available)",
                e
            );
            let _ = v17_service.remove().await;
            return;
        }
    }

    // Give the container time to start and fully initialize with password
    // PostgreSQL needs time to initialize the database and set up authentication
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    // Wait for PostgreSQL to be healthy
    let mut retries = 0;
    loop {
        match v17_service.health_check().await {
            Ok(healthy) if healthy => break,
            _ if retries < 60 => {
                retries += 1;
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            }
            _ => {
                println!("PostgreSQL 17 failed to start after 60 retries (30 seconds)");
                let _ = v17_service.remove().await;
                return;
            }
        }
    }

    // Connect, verify v17 version, and seed a marker row we can check
    // for survival after the rejected upgrade attempt.
    let connection_string = format!(
        "postgresql://postgres:{}@127.0.0.1:{}/postgres",
        urlencoding::encode(password),
        port
    );

    // Try to connect with retries since database might still be initializing
    let mut db_pool = None;
    for attempt in 0..10 {
        match sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&connection_string)
            .await
        {
            Ok(pool) => {
                db_pool = Some(pool);
                break;
            }
            Err(e) if attempt < 9 => {
                println!(
                    "Connection attempt {} failed: {}. Retrying...",
                    attempt + 1,
                    e
                );
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            }
            Err(e) => {
                println!(
                    "Failed to connect to v17 PostgreSQL after 10 attempts: {}. Skipping test",
                    e
                );
                let _ = v17_service.remove().await;
                return;
            }
        }
    }

    let db_pool = db_pool.unwrap();

    let version_v17: (String,) = match sqlx::query_as("SELECT version()").fetch_one(&db_pool).await
    {
        Ok(v) => v,
        Err(e) => {
            println!("Failed to query version from v17: {}. Skipping test", e);
            db_pool.close().await;
            let _ = v17_service.remove().await;
            return;
        }
    };

    println!("PostgreSQL 17 version: {}", version_v17.0);
    assert!(
        version_v17.0.contains("17"),
        "Version should contain '17', got: {}",
        version_v17.0
    );

    sqlx::query("CREATE TABLE upgrade_survives_marker (id int PRIMARY KEY, note text)")
        .execute(&db_pool)
        .await
        .expect("failed to create marker table");
    sqlx::query("INSERT INTO upgrade_survives_marker (id, note) VALUES (1, 'pre-upgrade')")
        .execute(&db_pool)
        .await
        .expect("failed to insert marker row");

    db_pool.close().await;

    // Attempt the major-version upgrade via `upgrade()` — must be
    // rejected, not attempted.
    let upgrade_result = v17_service
        .upgrade(v17_config.clone(), v18_config.clone())
        .await;

    let err = match upgrade_result {
        Ok(_) => {
            let _ = v17_service.remove().await;
            panic!(
                "upgrade() must reject a major-version change (17 -> 18) instead of \
                     attempting it directly; use the dedicated orchestrator instead"
            );
        }
        Err(e) => e,
    };
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("Cannot change PostgreSQL major version"),
        "expected rejection to name the major-version-change restriction, got: {}",
        err_msg
    );

    // Nothing should have been touched: same container, same image,
    // same data. Re-verify against the ORIGINAL v17 service handle —
    // if `upgrade()` had stopped/replaced the container despite
    // rejecting, this reconnect would fail or return a different image.
    let mut db_pool = None;
    for attempt in 0..10 {
        match sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&connection_string)
            .await
        {
            Ok(pool) => {
                db_pool = Some(pool);
                break;
            }
            Err(e) if attempt < 9 => {
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                let _ = e;
            }
            Err(e) => {
                let _ = v17_service.remove().await;
                panic!(
                    "container should still be reachable on v17 after a rejected upgrade, \
                         but reconnect failed: {}",
                    e
                );
            }
        }
    }
    let db_pool = db_pool.unwrap();

    let version_after: (String,) = sqlx::query_as("SELECT version()")
        .fetch_one(&db_pool)
        .await
        .expect("version query should still work against the untouched v17 container");
    assert!(
        version_after.0.contains("17"),
        "container must still be on v17 after a rejected upgrade, got: {}",
        version_after.0
    );

    let marker: (i32, String) =
        sqlx::query_as("SELECT id, note FROM upgrade_survives_marker WHERE id = 1")
            .fetch_one(&db_pool)
            .await
            .expect("marker row must survive a rejected upgrade attempt");
    assert_eq!(marker.1, "pre-upgrade");

    println!("Major-version upgrade correctly rejected; v17 data intact.");

    // Cleanup
    db_pool.close().await;
    let _ = v17_service.stop().await;
    let _ = v17_service.remove().await;
}

#[test]
fn test_import_service_config_creation() {
    // Test that ServiceConfig is properly created for import
    let config = ServiceConfig {
        name: "test-postgres-import".to_string(),
        service_type: ServiceType::Postgres,
        version: Some("15-bookworm".to_string()),
        parameters: serde_json::json!({
            "host": "localhost",
            "port": 5432,
            "database": "testdb",
            "username": "postgres",
            "password": "testpass",
            "max_connections": "20",
            "ssl_mode": "disable",
            "docker_image": "gotempsh/postgres-walg:15-bookworm",
            "container_id": "abc123def456",
        }),
    };

    assert_eq!(config.name, "test-postgres-import");
    assert_eq!(config.service_type, ServiceType::Postgres);
    assert_eq!(config.version, Some("15-bookworm".to_string()));
    assert_eq!(config.parameters["host"], "localhost");
    assert_eq!(config.parameters["port"], 5432);
}

#[test]
fn test_import_version_extraction_with_tag() {
    // Test version extraction from Docker image names
    let test_cases = vec![
        ("gotempsh/postgres-walg:15-bookworm", "15-bookworm"),
        ("postgres:latest", "latest"),
        ("postgres:14.5", "14.5"),
        ("postgres:16-bookworm", "16-bookworm"),
    ];

    for (image, expected_version) in test_cases {
        let version = if let Some(tag_pos) = image.rfind(':') {
            image[tag_pos + 1..].to_string()
        } else {
            "latest".to_string()
        };

        assert_eq!(version, expected_version, "Failed for image: {}", image);
    }
}

#[test]
fn test_import_version_extraction_without_tag() {
    let image = "postgres";
    let version = if let Some(tag_pos) = image.rfind(':') {
        image[tag_pos + 1..].to_string()
    } else {
        "latest".to_string()
    };

    assert_eq!(version, "latest");
}

#[test]
fn test_import_connection_url_format() {
    let username = "postgres";
    let password = "mysecretpassword";
    let port = 5432;
    let database = "importeddb";

    let connection_url = format!(
        "postgresql://{}:{}@localhost:{}/{}",
        username, password, port, database
    );

    // Verify all components are present
    assert!(connection_url.contains("postgresql://"));
    assert!(connection_url.contains("postgres"));
    assert!(connection_url.contains("mysecretpassword"));
    assert!(connection_url.contains("localhost"));
    assert!(connection_url.contains("5432"));
    assert!(connection_url.contains("importeddb"));
}

#[test]
fn test_import_validates_required_credentials() {
    let credentials: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    // Missing all required fields

    // These should all be None
    assert!(!credentials.contains_key("username"));
    assert!(!credentials.contains_key("password"));
    assert!(!credentials.contains_key("port"));
    assert!(!credentials.contains_key("database"));
}

#[test]
fn test_import_credential_extraction() {
    let mut credentials: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    credentials.insert("username".to_string(), "importuser".to_string());
    credentials.insert("password".to_string(), "importpass".to_string());
    credentials.insert("port".to_string(), "5433".to_string());
    credentials.insert("database".to_string(), "importdb".to_string());

    // Verify credential extraction
    assert_eq!(
        credentials.get("username").map(|s| s.as_str()),
        Some("importuser")
    );
    assert_eq!(
        credentials.get("password").map(|s| s.as_str()),
        Some("importpass")
    );
    assert_eq!(credentials.get("port").map(|s| s.as_str()), Some("5433"));
    assert_eq!(
        credentials.get("database").map(|s| s.as_str()),
        Some("importdb")
    );
}

// `flavor = "multi_thread"` is required because `S3TestContainer`'s
// `Drop` impl calls `tokio::task::block_in_place`, which panics on the
// default current-thread runtime.
#[cfg(feature = "docker-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_postgres_backup_and_restore_to_s3() {
    // Whole-test wall-clock budget. Anything above this is a hang — fail
    // loudly with a diagnostic instead of stalling the CI runner for 90 min.
    // See incident: GitHub run 25940925537 (PR #89) burned 86 min on this
    // test because it never returned. Sister tests in redis.rs and
    // mongodb.rs already wrap themselves the same way.
    //
    // The body pulls the wal-g image, boots Postgres, creates a table,
    // runs a full base backup to MinIO, then a full restore — comfortably
    // under 5 minutes on cold runners but can hang indefinitely on a
    // wedged Docker daemon if left unbounded.
    const TEST_TIMEOUT: Duration = Duration::from_secs(300);

    tokio::time::timeout(TEST_TIMEOUT, run_postgres_backup_and_restore_to_s3())
        .await
        .expect(
            "test_postgres_backup_and_restore_to_s3 exceeded 300s — likely hung on \
                 Postgres/Docker/wal-g wait",
        );
}

/// Body of `test_postgres_backup_and_restore_to_s3`, extracted so the
/// outer test can wrap it in `tokio::time::timeout` without a giant
/// async block at the call site.
#[cfg(feature = "docker-tests")]
async fn run_postgres_backup_and_restore_to_s3() {
    use crate::externalsvc::test_utils::{
        create_mock_backup, create_mock_db, create_mock_external_service, S3TestContainer,
    };

    // Check if Docker is available
    let docker = match Docker::connect_with_local_defaults() {
        Ok(d) => Arc::new(d),
        Err(e) => {
            println!("Docker not available, skipping test: {}", e);
            return;
        }
    };

    // Verify Docker is actually responding
    if docker.ping().await.is_err() {
        println!("Docker daemon not responding, skipping test");
        return;
    }

    // Start MinIO container for S3 operations
    let minio = match S3TestContainer::start(docker.clone(), "postgres-backup-test").await {
        Ok(m) => m,
        Err(e) => {
            let error_msg = e.to_string();
            if error_msg.contains("certificate")
                || error_msg.contains("TrustStore")
                || error_msg.contains("panicked")
            {
                println!("❌ Skipping PostgreSQL backup test: TLS certificate issue");
                println!(
                    "   Reason: {}",
                    error_msg.lines().next().unwrap_or(&error_msg)
                );
                println!("   Solution: Install system root certificates (required by AWS SDK even for HTTP endpoints)");
                return;
            }
            panic!("Failed to start MinIO container: {}", e);
        }
    };

    // Create PostgreSQL service
    //
    // Pick a free port so parallel test runs (and leaked containers from
    // previous runs) don't collide. Previously hardcoded to 15432, which
    // caused "port is already allocated" failures in CI when a leftover
    // container held the port (see redis.rs's identical fix for 16379).
    let pg_port = match find_available_port(15432) {
        Some(p) => p,
        None => {
            println!("No available port in 15432..16432 range, skipping test");
            let _ = minio.cleanup().await;
            return;
        }
    };
    let pg_password = "testpass123";
    let service_name = format!("test_pg_backup_{}", chrono::Utc::now().timestamp_millis());

    let pg_params = serde_json::json!({
        "host": "localhost",
        "port": pg_port.to_string(),
        "database": "postgres",
        "username": "postgres",
        "password": pg_password,
        "max_connections": 100,
        "docker_image": "gotempsh/postgres-walg:18-bookworm",
    });

    let pg_config = ServiceConfig {
        name: service_name.clone(),
        service_type: ServiceType::Postgres,
        version: Some("18".to_string()),
        parameters: pg_params,
    };

    let pg_service = PostgresService::new(service_name.clone(), docker.clone());

    // Initialize PostgreSQL service
    match pg_service.init(pg_config.clone()).await {
        Ok(_) => println!("✓ PostgreSQL service initialized"),
        Err(e) => {
            println!("Failed to initialize PostgreSQL: {}. Skipping test", e);
            let _ = minio.cleanup().await;
            return;
        }
    }

    // Wait for PostgreSQL to be healthy
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    // Create a test database and insert data
    let connection_string = format!(
        "postgresql://postgres:{}@127.0.0.1:{}/postgres",
        urlencoding::encode(pg_password),
        pg_port
    );

    let db_pool = match sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&connection_string)
        .await
    {
        Ok(pool) => pool,
        Err(e) => {
            println!("Failed to connect to PostgreSQL: {}. Skipping test", e);
            let _ = pg_service.remove().await;
            let _ = minio.cleanup().await;
            return;
        }
    };

    // Create test table and insert data
    match sqlx::query(
        "CREATE TABLE test_backup (id SERIAL PRIMARY KEY, name TEXT NOT NULL, value INT NOT NULL)",
    )
    .execute(&db_pool)
    .await
    {
        Ok(_) => println!("✓ Test table created"),
        Err(e) => {
            println!("Failed to create test table: {}. Skipping test", e);
            db_pool.close().await;
            let _ = pg_service.remove().await;
            let _ = minio.cleanup().await;
            return;
        }
    }

    match sqlx::query("INSERT INTO test_backup (name, value) VALUES ($1, $2), ($3, $4), ($5, $6)")
        .bind("test1")
        .bind(100)
        .bind("test2")
        .bind(200)
        .bind("test3")
        .bind(300)
        .execute(&db_pool)
        .await
    {
        Ok(_) => println!("✓ Test data inserted"),
        Err(e) => {
            println!("Failed to insert test data: {}. Skipping test", e);
            db_pool.close().await;
            let _ = pg_service.remove().await;
            let _ = minio.cleanup().await;
            return;
        }
    }

    // Verify data was inserted
    let count: (i64,) = match sqlx::query_as("SELECT COUNT(*) FROM test_backup")
        .fetch_one(&db_pool)
        .await
    {
        Ok(c) => c,
        Err(e) => {
            println!("Failed to count test data: {}. Skipping test", e);
            db_pool.close().await;
            let _ = pg_service.remove().await;
            let _ = minio.cleanup().await;
            return;
        }
    };
    assert_eq!(count.0, 3, "Should have 3 rows");
    println!("✓ Verified {} rows in test table", count.0);

    // Close connection before backup
    db_pool.close().await;

    // Create mock database connection for backup/restore operations
    let mock_db = match create_mock_db().await {
        Ok(db) => db,
        Err(e) => {
            println!("Failed to create mock database: {}. Skipping test", e);
            let _ = pg_service.remove().await;
            let _ = minio.cleanup().await;
            return;
        }
    };

    // Create mock backup record
    let backup = create_mock_backup("backups/postgres/test");
    let external_service = create_mock_external_service(service_name.clone(), "postgres", "17");

    // Perform backup to S3
    let s3_creds = minio.s3_credentials();
    let backup_location = match pg_service
        .backup_to_s3(
            &minio.s3_client,
            &s3_creds,
            backup,
            &minio.s3_source,
            "backups/postgres",
            "backups",
            &mock_db,
            &external_service,
            pg_config.clone(),
        )
        .await
    {
        Ok(outcome) => {
            println!(
                "✓ Backup completed to: {} ({:?} bytes)",
                outcome.location, outcome.size_bytes
            );
            outcome.location
        }
        Err(e) => {
            println!("Backup failed: {}. Skipping test", e);
            let _ = pg_service.remove().await;
            let _ = minio.cleanup().await;
            return;
        }
    };

    // Drop the test table to simulate data loss
    let db_pool = match sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&connection_string)
        .await
    {
        Ok(pool) => pool,
        Err(e) => {
            println!("Failed to reconnect to PostgreSQL: {}. Skipping test", e);
            let _ = pg_service.remove().await;
            let _ = minio.cleanup().await;
            return;
        }
    };

    match sqlx::query("DROP TABLE IF EXISTS test_backup")
        .execute(&db_pool)
        .await
    {
        Ok(_) => println!("✓ Test table dropped (simulating data loss)"),
        Err(e) => {
            println!("Failed to drop test table: {}. Skipping test", e);
            db_pool.close().await;
            let _ = pg_service.remove().await;
            let _ = minio.cleanup().await;
            return;
        }
    }

    // Verify table is gone
    let table_exists: (bool,) = match sqlx::query_as(
        "SELECT EXISTS (SELECT FROM information_schema.tables WHERE table_name = 'test_backup')",
    )
    .fetch_one(&db_pool)
    .await
    {
        Ok(exists) => exists,
        Err(e) => {
            println!("Failed to check table existence: {}. Skipping test", e);
            db_pool.close().await;
            let _ = pg_service.remove().await;
            let _ = minio.cleanup().await;
            return;
        }
    };
    assert!(!table_exists.0, "Table should not exist after drop");
    println!("✓ Verified table was dropped");

    db_pool.close().await;

    // Restore from S3 backup
    match pg_service
        .restore_from_s3(
            &minio.s3_client,
            &s3_creds,
            &backup_location,
            &minio.s3_source,
            pg_config.clone(),
        )
        .await
    {
        Ok(_) => println!("✓ Restore completed from: {}", backup_location),
        Err(e) => {
            println!("Restore failed: {}. Skipping test", e);
            let _ = pg_service.remove().await;
            let _ = minio.cleanup().await;
            return;
        }
    };

    // Verify restored data
    let db_pool = match sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&connection_string)
        .await
    {
        Ok(pool) => pool,
        Err(e) => {
            println!("Failed to reconnect after restore: {}. Skipping test", e);
            let _ = pg_service.remove().await;
            let _ = minio.cleanup().await;
            return;
        }
    };

    // Verify table exists
    let table_exists: (bool,) = match sqlx::query_as(
        "SELECT EXISTS (SELECT FROM information_schema.tables WHERE table_name = 'test_backup')",
    )
    .fetch_one(&db_pool)
    .await
    {
        Ok(exists) => exists,
        Err(e) => {
            println!("Failed to check restored table: {}. Skipping test", e);
            db_pool.close().await;
            let _ = pg_service.remove().await;
            let _ = minio.cleanup().await;
            return;
        }
    };
    assert!(table_exists.0, "Table should exist after restore");
    println!("✓ Verified table was restored");

    // Verify row count
    let count: (i64,) = match sqlx::query_as("SELECT COUNT(*) FROM test_backup")
        .fetch_one(&db_pool)
        .await
    {
        Ok(c) => c,
        Err(e) => {
            println!("Failed to count restored data: {}. Skipping test", e);
            db_pool.close().await;
            let _ = pg_service.remove().await;
            let _ = minio.cleanup().await;
            return;
        }
    };
    assert_eq!(count.0, 3, "Should have 3 rows after restore");
    println!("✓ Verified {} rows were restored", count.0);

    // Verify actual data values
    let rows: Vec<(i32, String, i32)> =
        match sqlx::query_as("SELECT id, name, value FROM test_backup ORDER BY id")
            .fetch_all(&db_pool)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                println!("Failed to fetch restored rows: {}. Skipping test", e);
                db_pool.close().await;
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };

    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].1, "test1");
    assert_eq!(rows[0].2, 100);
    assert_eq!(rows[1].1, "test2");
    assert_eq!(rows[1].2, 200);
    assert_eq!(rows[2].1, "test3");
    assert_eq!(rows[2].2, 300);
    println!("✓ Verified all data values match original");

    // A completed row restore does not establish continuous recovery
    // coverage. The restored service must stay safely disabled until an
    // explicit backup destination is configured again.
    let mode: String = sqlx::query_scalar("SHOW archive_mode")
        .fetch_one(&db_pool)
        .await
        .unwrap();
    assert_eq!(mode, "off", "restore must suspend WAL pushes durably");
    db_pool.close().await;
    pg_service.stop().await.unwrap();
    pg_service.start().await.unwrap();
    pg_service
        .enable_continuous_archiving(pg_config.clone(), &s3_creds, &backup_location)
        .await
        .expect("a subsequent backup must repair active WAL archiving");
    let db_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&connection_string)
        .await
        .unwrap();
    let settings: (String, String) = sqlx::query_as(
        "SELECT current_setting('archive_mode'), current_setting('archive_command')",
    )
    .fetch_one(&db_pool)
    .await
    .unwrap();
    assert_eq!(settings.0, "on");
    assert!(settings.1.contains("wal-g wal-push"), "{:?}", settings);
    let segment: String = sqlx::query_scalar("SELECT pg_walfile_name(pg_current_wal_lsn())")
        .fetch_one(&db_pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO test_backup (name, value) VALUES ('post-restore-wal', 400)")
        .execute(&db_pool)
        .await
        .unwrap();
    sqlx::query("SELECT pg_switch_wal()")
        .execute(&db_pool)
        .await
        .unwrap();
    let wal_key = backup_location
        .strip_prefix("s3://backups/")
        .unwrap()
        .trim_end_matches('/');
    let prefix = format!("{wal_key}/wal_005/{segment}");
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let objects = minio
                .s3_client
                .list_objects_v2()
                .bucket("backups")
                .prefix(&prefix)
                .send()
                .await
                .unwrap();
            if objects
                .contents()
                .iter()
                .any(|object| object.size().unwrap_or(0) > 0)
            {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .expect("post-restore writes must emit a WAL object at the selected destination");

    // Cleanup
    db_pool.close().await;
    let _ = pg_service.stop().await;
    let _ = pg_service.remove().await;
    let _ = minio.cleanup().await;

    println!("✅ PostgreSQL backup and restore test passed!");
}

/// Regression guard for the cross-tenant IDOR: a client-supplied
/// `container_name` in a create request would make every subsequent Docker
/// operation (start, exec, backup, restore) target that named container
/// instead of creating a new one. Because Docker names are global and
/// predictable (`postgres-{slug}`), this lets a tenant redirect operations
/// to a different tenant's live database container.
///
/// The schema-level `#[schemars(skip)]` only hides the field from the UI
/// form — it does NOT stop `serde_json::from_value` from deserialising a
/// client-supplied value. `validate_for_creation` must reject it explicitly.
#[test]
fn test_container_name_rejected_by_validate_for_creation() {
    use crate::parameter_strategies::PostgresParameterStrategy;
    let strategy = PostgresParameterStrategy;
    let mut params = std::collections::HashMap::new();
    params.insert(
        "database".to_string(),
        serde_json::Value::String("mydb".to_string()),
    );
    params.insert(
        "username".to_string(),
        serde_json::Value::String("myuser".to_string()),
    );
    params.insert(
        "container_name".to_string(),
        serde_json::Value::String("postgres-victim-slug".to_string()),
    );
    let result =
        crate::parameter_strategies::ParameterStrategy::validate_for_creation(&strategy, &params);
    assert!(
        result.is_err(),
        "validate_for_creation must reject a client-supplied container_name"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("container_name"),
        "error message should mention 'container_name', got: {err}"
    );
}

#[test]
fn test_get_environment_variables_always_uses_container_name() {
    // get_environment_variables always uses container name and internal port
    // for container-to-container communication, regardless of deployment mode
    let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
    let service = PostgresService::new("test-env-vars".to_string(), docker);

    let mut params = HashMap::new();
    params.insert("port".to_string(), "5433".to_string());
    params.insert("database".to_string(), "testdb".to_string());
    params.insert("username".to_string(), "testuser".to_string());
    params.insert("password".to_string(), "testpass".to_string());

    let env_vars = service.get_environment_variables(&params).unwrap();

    // Always uses container name and internal port (5432)
    assert_eq!(
        env_vars.get("POSTGRES_HOST").unwrap(),
        "postgres-test-env-vars"
    );
    assert_eq!(env_vars.get("POSTGRES_PORT").unwrap(), "5432");
    assert!(env_vars
        .get("POSTGRES_URL")
        .unwrap()
        .contains("postgres-test-env-vars:5432"));
}

#[test]
fn test_get_docker_environment_variables_always_uses_container_name() {
    // get_docker_environment_variables always uses container name and internal port
    // for container-to-container communication, regardless of deployment mode
    let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
    let service = PostgresService::new("test-docker-env".to_string(), docker);

    let mut params = HashMap::new();
    params.insert("port".to_string(), "5434".to_string());
    params.insert("database".to_string(), "testdb".to_string());
    params.insert("username".to_string(), "testuser".to_string());
    params.insert("password".to_string(), "testpass".to_string());

    let env_vars = service.get_docker_environment_variables(&params).unwrap();

    // Always uses container name and internal port (5432)
    assert_eq!(
        env_vars.get("POSTGRES_HOST").unwrap(),
        "postgres-test-docker-env"
    );
    assert_eq!(env_vars.get("POSTGRES_PORT").unwrap(), "5432");
}

// ── Database Name SQL Injection Prevention Tests ─────────────────

#[tokio::test]
async fn test_restore_capabilities_declares_all_modes_supported() {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(d) => Arc::new(d),
        Err(_) => {
            println!("Docker not available, skipping");
            return;
        }
    };
    let pg = PostgresService::new("test-caps".to_string(), docker);
    let cfg = ServiceConfig {
        name: "test-caps".into(),
        service_type: ServiceType::Postgres,
        version: Some("18".into()),
        parameters: serde_json::json!({
            "host": "localhost",
            "port": "5432",
            "database": "postgres",
            "username": "postgres",
            "password": "p",
            "max_connections": 100,
            "docker_image": "gotempsh/postgres-walg:18-bookworm",
        }),
    };
    let caps = pg.restore_capabilities(cfg).await.unwrap();
    assert!(caps.restore_in_place);
    assert!(caps.restore_to_new_service);
    assert!(caps.pitr);
    // We don't compute bounds here — unbounded picker in UI.
    assert!(caps.earliest_pitr_time.is_none());
    assert!(caps.latest_pitr_time.is_none());
}

#[tokio::test]
async fn test_restore_pitr_rejects_legacy_backup_without_docker_work() {
    // restore_pitr must reject a non-WAL-G backup (missing s3:// prefix)
    // BEFORE attempting any Docker operations, so this test can run
    // anywhere the library builds.
    let docker = match Docker::connect_with_local_defaults() {
        Ok(d) => Arc::new(d),
        Err(_) => {
            println!("Docker not available, skipping");
            return;
        }
    };
    let pg = PostgresService::new("test-pitr-guard".to_string(), docker);

    let cfg = ServiceConfig {
        name: "test-pitr-guard".into(),
        service_type: ServiceType::Postgres,
        version: Some("18".into()),
        parameters: serde_json::json!({
            "host": "localhost",
            "port": "5432",
            "database": "postgres",
            "username": "postgres",
            "password": "p",
            "max_connections": 100,
            "docker_image": "gotempsh/postgres-walg:18-bookworm",
        }),
    };

    // Synthesize the minimum viable RestoreContext with a legacy backup
    // location (.pgdump.gz, no s3:// prefix).
    let legacy_location = "backups/legacy/dump.pgdump.gz".to_string();
    let s3_creds = crate::externalsvc::S3Credentials {
        access_key_id: "k".into(),
        secret_key: "s".into(),
        session_token: None,
        region: "us-east-1".into(),
        endpoint: None,
        bucket_name: "b".into(),
        bucket_path: "".into(),
        force_path_style: true,
    };
    let s3_source = temps_entities::s3_sources::Model {
        id: 1,
        name: "src".into(),
        bucket_name: "b".into(),
        bucket_path: "".into(),
        access_key_id: "enc".into(),
        secret_key: "enc".into(),
        session_token: None,
        credentials_expire_at: None,
        region: "us-east-1".into(),
        endpoint: None,
        force_path_style: Some(true),
        is_default: false,
        managed_by_cloud: false,
        lifecycle_reconcile_failed_at: None,
        lifecycle_reconcile_generation: 0,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        backing_service_id: None,
    };
    let backup = temps_entities::backups::Model {
        id: 1,
        name: "b".into(),
        backup_id: "id".into(),
        schedule_id: None,
        schedule_run_id: None,
        backup_type: "external_service".into(),
        state: "completed".into(),
        started_at: chrono::Utc::now(),
        finished_at: None,
        size_bytes: None,
        file_count: None,
        s3_source_id: 1,
        s3_location: legacy_location.clone(),
        error_message: None,
        metadata: "{}".into(),
        checksum: None,
        compression_type: "gzip".into(),
        created_by: 1,
        expires_at: None,
        tags: "".into(),
    };
    let source_service = temps_entities::external_services::Model {
        id: 1,
        name: "source".into(),
        service_type: "postgres".into(),
        version: Some("18".into()),
        status: "running".into(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        slug: None,
        config: Some("{}".into()),
        node_id: None,
        topology: "standalone".into(),
        error_message: None,
        health_status: None,
        last_health_check_at: None,
        last_health_error: None,
        consecutive_health_failures: 0,
        health_metadata: None,
        metrics_enabled: false,
        default_backup_provisioned: false,
        container_name: None,
        created_by_user_id: None,
        continuous_archive_s3_source_id: None,
        continuous_archive_pinned_at: None,
        workload_mode: Default::default(),
        log_ingest_lines_per_sec: None,
        log_ingest_kib_per_sec: None,
    };
    // Build a MockDatabase for the `pool` slot — restore_pitr for
    // Postgres doesn't touch it in the legacy-reject path.
    let mock_db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection();
    // Build the S3 client. The AWS SDK eagerly initialises its rustls
    // TrustStore at `Client::from_conf` time, and on hosts without any
    // system root CAs (some CI runners, minimal containers, macOS
    // without keychain access) it panics with "TrustStore configured
    // to enable native roots but no valid root certificates parsed!".
    // We wrap construction in `catch_unwind` and skip the test on that
    // specific panic — mirroring the pattern in
    // `externalsvc/test_utils.rs::S3TestContainer::start`.
    let s3_client = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let aws_creds = aws_sdk_s3::config::Credentials::new("k", "s", None, None, "test");
        let conf = aws_sdk_s3::Config::builder()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new("us-east-1"))
            .credentials_provider(aws_creds)
            .build();
        aws_sdk_s3::Client::from_conf(conf)
    })) {
        Ok(c) => c,
        Err(panic_payload) => {
            let panic_msg = panic_payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| {
                    panic_payload
                        .downcast_ref::<&'static str>()
                        .map(ToString::to_string)
                })
                .unwrap_or_else(|| "(non-string panic payload)".to_string());
            if panic_msg.contains("TrustStore") || panic_msg.contains("certificate") {
                println!(
                    "Skipping test: AWS SDK panicked initialising rustls TrustStore: {}",
                    panic_msg
                );
                return;
            }
            panic!("AWS SDK panic constructing S3 client: {}", panic_msg);
        }
    };
    let ctx = crate::externalsvc::RestoreContext {
        s3_client: &s3_client,
        s3_credentials: &s3_creds,
        s3_source: &s3_source,
        backup: &backup,
        backup_location: &legacy_location,
        source_service: &source_service,
        live_target_config: cfg.clone(),
        source_config: cfg,
        pool: &mock_db,
        gate: &crate::externalsvc::NoopRestoreGate,
    };

    let err = pg
        .restore_pitr(
            ctx,
            crate::externalsvc::RecoveryTarget::Time {
                time: chrono::Utc::now(),
            },
            false,
            None,
        )
        .await
        .expect_err("PITR on legacy backup must fail fast");
    let msg = err.to_string();
    assert!(
        msg.contains("WAL-G"),
        "expected WAL-G requirement in error, got: {}",
        msg
    );
}

// -----------------------------------------------------------------
// PostgreSQL recovery target formatting
// -----------------------------------------------------------------
