//! Test utilities for external services backup and restore tests
//!
//! This module provides utilities to set up MinIO (S3-compatible storage) containers
//! and mock entities for testing backup and restore functionality across all external services.

use anyhow::Result;

// Docker-specific imports and types, only compiled with docker-tests feature
#[cfg(feature = "docker-tests")]
mod docker_utils {
    use anyhow::Result;
    use aws_sdk_s3::config::Region;
    use bollard::Docker;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    /// MinIO container configuration and client
    pub struct MinioTestContainer {
        pub container_id: String,
        pub port: u16,
        pub access_key: String,
        pub secret_key: String,
        pub bucket_name: String,
        pub s3_client: aws_sdk_s3::Client,
        pub s3_source: temps_entities::s3_sources::Model,
        docker: Arc<Docker>,
    }

    impl MinioTestContainer {
        /// Start a MinIO container and set up S3 client and bucket
        pub async fn start(docker: Arc<Docker>, bucket_name: &str) -> Result<Self> {
            use bollard::query_parameters::CreateImageOptions;
            use futures::StreamExt;

            // Sweep any `temps-test-minio-*` containers left behind by a
            // killed previous test run. Even though container names are
            // already unique-per-call (timestamp + random suffix), the
            // leaked instances hold ports + memory and pile up on dev
            // machines and CI runners. This sweep is best-effort —
            // anything we can't remove just stays for someone else.
            sweep_orphan_test_containers(&docker, "temps-test-minio-").await;

            // Find available port for MinIO - use random offset to avoid parallel test conflicts
            let random_offset = rand::random::<u16>() % 1000;
            let port = find_available_port(9000 + random_offset)?;
            let access_key = "minioadmin";
            let secret_key = "minioadmin";

            println!("Starting MinIO container on port {}...", port);

            // Join the same app network the real Postgres/Redis/S3/MongoDB
            // service containers use (see `ensure_network_exists`) so
            // `S3Credentials::resolve_endpoint_for_container` can find this
            // container by name and resolve a proper container-to-container
            // address, instead of falling back to `host.docker.internal`
            // (unreliable/unset on Linux without an explicit host-gateway
            // mapping, which is exactly what caused backup tests to hang).
            crate::utils::ensure_network_exists(&docker)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to ensure network exists: {:?}", e))?;

            // Pull MinIO image
            let mut pull_stream = docker.create_image(
                Some(CreateImageOptions {
                    from_image: Some("minio/minio:latest".to_string()),
                    ..Default::default()
                }),
                None,
                None,
            );
            while let Some(result) = pull_stream.next().await {
                result.map_err(|e| anyhow::anyhow!("Failed to pull MinIO image: {}", e))?;
            }

            // Create container
            let container_name = format!(
                "temps-test-minio-{}-{}",
                chrono::Utc::now().timestamp(),
                rand::random::<u32>()
            );
            let minio_config = bollard::models::ContainerCreateBody {
                image: Some("minio/minio:latest".to_string()),
                cmd: Some(vec!["server".to_string(), "/data".to_string()]),
                env: Some(vec![
                    format!("MINIO_ROOT_USER={}", access_key),
                    format!("MINIO_ROOT_PASSWORD={}", secret_key),
                ]),
                host_config: Some(bollard::models::HostConfig {
                    port_bindings: Some(HashMap::from([(
                        "9000/tcp".to_string(),
                        Some(vec![bollard::models::PortBinding {
                            host_ip: Some("127.0.0.1".to_string()),
                            host_port: Some(port.to_string()),
                        }]),
                    )])),
                    ..Default::default()
                }),
                networking_config: Some(bollard::models::NetworkingConfig {
                    endpoints_config: Some(HashMap::from([(
                        temps_core::NETWORK_NAME.to_string(),
                        bollard::models::EndpointSettings::default(),
                    )])),
                }),
                ..Default::default()
            };

            let container = docker
                .create_container(
                    Some(
                        bollard::query_parameters::CreateContainerOptionsBuilder::new()
                            .name(&container_name)
                            .build(),
                    ),
                    minio_config,
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create MinIO container: {}", e))?;

            docker
                .start_container(
                    &container.id,
                    None::<bollard::query_parameters::StartContainerOptions>,
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to start MinIO container: {}", e))?;

            // Wait for MinIO to be ready
            tokio::time::sleep(Duration::from_secs(3)).await;
            println!("✓ MinIO container started: {}", container.id);

            let s3_config = aws_sdk_s3::Config::builder()
                .endpoint_url(format!("http://localhost:{}", port))
                .region(Region::new("us-east-1"))
                .behavior_version_latest()
                .credentials_provider(aws_sdk_s3::config::Credentials::new(
                    access_key, secret_key, None, None, "minio",
                ))
                .force_path_style(true)
                .build();

            // Build the S3 client. The AWS SDK eagerly initialises its
            // rustls TrustStore at `Client::from_conf` time, and on hosts
            // without any system root CAs (some CI runners, minimal
            // containers, the GitHub macOS runner without keychain access)
            // it panics with "TrustStore configured to enable native roots
            // but no valid root certificates parsed!". The panic is raised
            // on the test thread, so we wrap construction in `catch_unwind`
            // and convert any panic into a skippable error whose message
            // contains "TrustStore". The sibling backup tests
            // (redis/mongodb/postgres) already check for that substring
            // and skip the test cleanly.
            let s3_client = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                aws_sdk_s3::Client::from_conf(s3_config)
            })) {
                Ok(c) => c,
                Err(panic_payload) => {
                    let panic_msg = panic_payload
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| {
                            panic_payload
                                .downcast_ref::<&'static str>()
                                .map(|s| s.to_string())
                        })
                        .unwrap_or_else(|| "(non-string panic payload)".to_string());
                    return Err(anyhow::anyhow!(
                        "Failed to construct S3 client (AWS SDK panicked): {}",
                        panic_msg
                    ));
                }
            };

            // Create bucket
            s3_client
                .create_bucket()
                .bucket(bucket_name)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create S3 bucket: {}", e))?;

            println!("✓ S3 bucket created: {}", bucket_name);

            // Create s3_source entity
            let s3_source = temps_entities::s3_sources::Model {
                id: 1,
                name: "test-source".to_string(),
                bucket_name: bucket_name.to_string(),
                region: "us-east-1".to_string(),
                endpoint: Some(format!("http://localhost:{}", port)),
                bucket_path: "".to_string(),
                access_key_id: access_key.to_string(),
                secret_key: secret_key.to_string(),
                force_path_style: Some(true),
                is_default: false,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };

            Ok(Self {
                container_id: container.id,
                port,
                access_key: access_key.to_string(),
                secret_key: secret_key.to_string(),
                bucket_name: bucket_name.to_string(),
                s3_client,
                s3_source,
                docker,
            })
        }

        /// Build S3Credentials from the test container's configuration.
        /// In tests, credentials are plaintext (not encrypted).
        ///
        /// Uses `localhost`, matching `s3_source.endpoint` above — these
        /// credentials are passed to WAL-G running *inside* a Docker
        /// container, and `run_walg_backup_push` resolves the endpoint via
        /// `S3Credentials::resolve_endpoint_for_container` before use. That
        /// resolver specifically detects `localhost`/`127.0.0.1` and looks up
        /// this MinIO container by name on the shared app network (see
        /// `ensure_network_exists` above), only falling back to
        /// `host.docker.internal` if it can't find it. Hardcoding
        /// `host.docker.internal` here bypassed that lookup entirely and
        /// relied on a hostname that isn't reliably resolvable on Linux
        /// without an explicit host-gateway mapping — the root cause of
        /// backup tests hanging indefinitely on CI.
        pub fn s3_credentials(&self) -> super::super::S3Credentials {
            super::super::S3Credentials {
                access_key_id: self.access_key.clone(),
                secret_key: self.secret_key.clone(),
                region: "us-east-1".to_string(),
                endpoint: Some(format!("http://localhost:{}", self.port)),
                bucket_name: self.bucket_name.clone(),
                bucket_path: "".to_string(),
                force_path_style: true,
            }
        }

        /// Stop and remove the MinIO container
        pub async fn cleanup(&self) -> Result<()> {
            use bollard::query_parameters::{RemoveContainerOptions, StopContainerOptions};

            println!("Cleaning up MinIO container...");

            let _ = self
                .docker
                .stop_container(
                    &self.container_id,
                    Some(StopContainerOptions {
                        t: Some(5),
                        signal: None,
                    }),
                )
                .await;

            let _ = self
                .docker
                .remove_container(
                    &self.container_id,
                    Some(RemoveContainerOptions {
                        force: true,
                        v: true,
                        ..Default::default()
                    }),
                )
                .await;

            println!("✓ MinIO container cleaned up");
            Ok(())
        }
    }

    impl Drop for MinioTestContainer {
        fn drop(&mut self) {
            // Synchronously clean up the MinIO container to prevent leaks on panic.
            // Uses the same block_in_place pattern as TestDatabase for reliability.
            let container_id = self.container_id.clone();
            let docker = Arc::clone(&self.docker);

            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    tokio::task::block_in_place(|| {
                        handle.block_on(async {
                            let _ = docker
                                .stop_container(
                                    &container_id,
                                    Some(bollard::query_parameters::StopContainerOptions {
                                        t: Some(5),
                                        signal: None,
                                    }),
                                )
                                .await;

                            let _ = docker
                                .remove_container(
                                    &container_id,
                                    Some(bollard::query_parameters::RemoveContainerOptions {
                                        force: true,
                                        v: true,
                                        ..Default::default()
                                    }),
                                )
                                .await;
                        });
                    });
                } else {
                    eprintln!(
                        "Warning: Cannot clean up MinIO container {} (no tokio runtime available)",
                        container_id
                    );
                }
            }));

            if result.is_err() {
                eprintln!(
                    "Warning: Cleanup panicked for MinIO container {} (runtime may be shutting down)",
                    self.container_id
                );
            }
        }
    }

    /// Find an available port starting from the given port
    fn find_available_port(start_port: u16) -> Result<u16> {
        use std::net::TcpListener;

        for port in start_port..start_port + 100 {
            if TcpListener::bind(("0.0.0.0", port)).is_ok() {
                return Ok(port);
            }
        }

        Err(anyhow::anyhow!(
            "No available port found in range {}-{}",
            start_port,
            start_port + 100
        ))
    }

    /// Best-effort removal of orphaned test containers whose names start
    /// with `name_prefix`. Used by integration helpers (e.g.
    /// `MinioTestContainer::start`) to keep dev machines and CI runners
    /// from accumulating leaked containers when a test panics before
    /// reaching its cleanup path.
    ///
    /// All errors are swallowed — this is a "make a best attempt"
    /// hygiene step, not a correctness gate.
    pub(super) async fn sweep_orphan_test_containers(docker: &Docker, name_prefix: &str) {
        use bollard::query_parameters::{ListContainersOptions, RemoveContainerOptions};

        let mut filters = HashMap::new();
        // Docker matches `name` filters as a substring, so the prefix
        // alone is enough; the per-test timestamp/random suffix keeps
        // it from sweeping concurrent siblings since each test waits
        // on its own future.
        filters.insert("name".to_string(), vec![name_prefix.to_string()]);

        let Ok(containers) = docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters: Some(filters),
                ..Default::default()
            }))
            .await
        else {
            return;
        };

        for c in containers {
            let id = match c.id {
                Some(id) => id,
                None => continue,
            };
            let _ = docker
                .remove_container(
                    &id,
                    Some(RemoveContainerOptions {
                        force: true,
                        v: true,
                        ..Default::default()
                    }),
                )
                .await;
        }
    }
}

#[cfg(feature = "docker-tests")]
pub use docker_utils::MinioTestContainer;

/// Create a mock backup record for testing
pub fn create_mock_backup(subpath: &str) -> temps_entities::backups::Model {
    temps_entities::backups::Model {
        id: 1,
        name: "test-backup".to_string(),
        backup_id: "test-backup-id".to_string(),
        schedule_id: None,
        schedule_run_id: None,
        backup_type: "external_service".to_string(),
        state: "completed".to_string(),
        started_at: chrono::Utc::now(),
        finished_at: None,
        size_bytes: None,
        file_count: None,
        s3_source_id: 1,
        s3_location: subpath.to_string(),
        error_message: None,
        metadata: "{}".to_string(),
        checksum: None,
        compression_type: "gzip".to_string(),
        created_by: 1,
        expires_at: None,
        tags: "".to_string(),
    }
}

/// Create a mock external service record for testing
pub fn create_mock_external_service(
    name: String,
    service_type: &str,
    version: &str,
) -> temps_entities::external_services::Model {
    temps_entities::external_services::Model {
        id: 1,
        name,
        service_type: service_type.to_string(),
        version: Some(version.to_string()),
        status: "running".to_string(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        slug: None,
        config: Some(serde_json::json!({}).to_string()),
        node_id: None,
        topology: "standalone".to_string(),
        error_message: None,
        health_status: None,
        last_health_check_at: None,
        last_health_error: None,
        consecutive_health_failures: 0,
        health_metadata: None,
        metrics_enabled: false,
    }
}

/// Create a mock database connection (in-memory SQLite) with required tables
/// for integration tests that exercise backup_to_s3 / restore_from_s3.
pub async fn create_mock_db() -> Result<sea_orm::DatabaseConnection> {
    use sea_orm::ConnectionTrait;

    let db_url = "sqlite::memory:";
    let db_conn = sea_orm::Database::connect(db_url)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create mock database: {}", e))?;

    // Create the external_service_backups table used by backup_to_s3 implementations.
    // This mirrors the PostgreSQL schema but uses SQLite-compatible types.
    db_conn
        .execute_unprepared(
            "CREATE TABLE IF NOT EXISTS external_service_backups (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                service_id INTEGER NOT NULL,
                backup_id INTEGER NOT NULL,
                backup_type TEXT NOT NULL,
                state TEXT NOT NULL,
                started_at TEXT NOT NULL,
                finished_at TEXT,
                size_bytes INTEGER,
                s3_location TEXT NOT NULL,
                error_message TEXT,
                metadata TEXT NOT NULL DEFAULT '{}',
                checksum TEXT,
                compression_type TEXT NOT NULL DEFAULT 'gzip',
                created_by INTEGER NOT NULL DEFAULT 0,
                expires_at TEXT
            )",
        )
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create external_service_backups table: {}", e))?;

    Ok(db_conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_minio_container_lifecycle() {
        use bollard::Docker;
        use std::sync::Arc;

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

        // Start MinIO container
        let minio = match MinioTestContainer::start(docker.clone(), "test-bucket").await {
            Ok(m) => m,
            Err(e) => {
                // If it's a certificate error, skip test gracefully (common on systems without configured root certificates)
                let error_msg = e.to_string();
                if error_msg.contains("certificate")
                    || error_msg.contains("TrustStore")
                    || error_msg.contains("panicked")
                {
                    println!("Skipping MinIO test: TLS certificate issue");
                    println!(
                        "   Reason: {}",
                        error_msg.lines().next().unwrap_or(&error_msg)
                    );
                    println!("   Solution: Install system root certificates (required by AWS SDK even for HTTP endpoints)");
                    println!("   On macOS: Use Keychain Access to manage certificates");
                    return;
                }
                panic!("Failed to start MinIO container: {}", e);
            }
        };

        // Verify bucket exists
        let buckets = match minio.s3_client.list_buckets().send().await {
            Ok(b) => b,
            Err(e) => {
                // Cleanup before returning
                let _ = minio.cleanup().await;
                panic!("Failed to list buckets: {}", e);
            }
        };

        let bucket_names: Vec<String> = buckets
            .buckets()
            .iter()
            .filter_map(|b| b.name().map(|n| n.to_string()))
            .collect();

        assert!(
            bucket_names.contains(&"test-bucket".to_string()),
            "Bucket should exist"
        );

        // Cleanup
        minio.cleanup().await.expect("Failed to cleanup");
    }

    #[test]
    fn test_create_mock_entities() {
        let backup = create_mock_backup("backups/test");
        assert_eq!(backup.backup_type, "external_service");
        assert_eq!(backup.s3_location, "backups/test");

        let service = create_mock_external_service("test-service".to_string(), "mongodb", "8.0");
        assert_eq!(service.name, "test-service");
        assert_eq!(service.service_type, "mongodb");
        assert_eq!(service.version, Some("8.0".to_string()));
    }

    #[tokio::test]
    async fn test_create_mock_db() {
        let db = create_mock_db().await.expect("Failed to create mock DB");

        // Verify we can ping the database
        assert!(
            db.ping().await.is_ok(),
            "Should be able to ping the database"
        );
    }
}
