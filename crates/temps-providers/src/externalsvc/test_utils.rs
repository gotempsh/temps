//! Test utilities for external services backup and restore tests
//!
//! This module provides utilities to set up MinIO (S3-compatible storage) containers
//! and mock entities for testing backup and restore functionality across all external services.

use anyhow::Result;
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

        // Find available port for MinIO
        let port = find_available_port(9000)?;
        let access_key = "minioadmin";
        let secret_key = "minioadmin";

        println!("Starting MinIO container on port {}...", port);

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
        let container_name = format!("temps-test-minio-{}", chrono::Utc::now().timestamp());
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
                        host_ip: Some("0.0.0.0".to_string()),
                        host_port: Some(port.to_string()),
                    }]),
                )])),
                ..Default::default()
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

        // Configure S3 client for localhost testing
        // Using HTTP endpoint - SDK will automatically handle this without TLS
        let s3_config = aws_sdk_s3::config::Builder::new()
            .region(aws_sdk_s3::config::Region::new("us-east-1"))
            .endpoint_url(format!("http://localhost:{}", port))
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                access_key, secret_key, None, None, "test",
            ))
            .force_path_style(true)
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .build();

        let s3_client = aws_sdk_s3::Client::from_conf(s3_config);

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

/// Create a mock backup record for testing
pub fn create_mock_backup(subpath: &str) -> temps_entities::backups::Model {
    temps_entities::backups::Model {
        id: 1,
        name: "test-backup".to_string(),
        backup_id: "test-backup-id".to_string(),
        schedule_id: None,
        backup_type: "external_service".to_string(),
        state: "completed".to_string(),
        started_at: chrono::Utc::now().into(),
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
        created_at: chrono::Utc::now().into(),
        updated_at: chrono::Utc::now().into(),
        slug: None,
        config: Some(serde_json::json!({}).to_string()),
    }
}

/// Create a mock database connection (in-memory SQLite)
pub async fn create_mock_db() -> Result<sea_orm::DatabaseConnection> {
    let db_url = "sqlite::memory:";
    let db_conn = sea_orm::Database::connect(db_url)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create mock database: {}", e))?;
    Ok(db_conn)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_minio_container_lifecycle() {
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
        let minio = MinioTestContainer::start(docker.clone(), "test-bucket")
            .await
            .expect("Failed to start MinIO container");

        // Verify bucket exists
        let buckets = minio
            .s3_client
            .list_buckets()
            .send()
            .await
            .expect("Failed to list buckets");

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
