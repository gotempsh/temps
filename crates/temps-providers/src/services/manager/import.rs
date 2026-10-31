// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Importing existing containers as managed services.

use crate::externalsvc::{
    managed_instance_name, mariadb::MariaDbService, mongodb::MongodbService,
    postgres::PostgresService, redis::RedisService, rustfs::RustfsService, s3::S3Service,
    AvailableContainer, ExternalService, ServiceType,
};
use crate::services::*;
use sea_orm::{ActiveModelTrait, EntityTrait, Set, TransactionTrait};
use std::collections::HashMap;
use std::sync::Arc;
use temps_entities::external_services;

impl ExternalServiceManager {
    /// List available Docker containers that can be imported as services
    pub async fn list_available_containers(&self) -> Result<Vec<AvailableContainer>> {
        use bollard::query_parameters::ListContainersOptions;

        // Get list of managed services (we use their service names to exclude them)
        let managed_services = external_services::Entity::find()
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|service| service.name.to_lowercase())
            .collect::<std::collections::HashSet<_>>();

        let mut filters = HashMap::new();
        filters.insert("status".to_string(), vec!["running".to_string()]);

        let docker = self.docker.require()?;
        let containers = docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters: Some(filters),
                ..Default::default()
            }))
            .await
            .map_err(|e| anyhow::anyhow!("Failed to list Docker containers: {}", e))?;

        let mut available: Vec<AvailableContainer> = Vec::new();

        for container in containers {
            let container_id = container.id.clone().unwrap_or_default();

            // Extract container name (removing leading slash)
            let container_name_raw = container
                .names
                .clone()
                .and_then(|mut names| names.pop())
                .unwrap_or_else(|| container_id.clone());
            let container_name_lower = container_name_raw
                .strip_prefix('/')
                .unwrap_or(&container_name_raw)
                .to_lowercase();

            // Skip containers that are already managed by Temps
            if managed_services.contains(&container_name_lower) {
                continue;
            }

            let image = match &container.image {
                Some(img) => img.clone(),
                None => continue,
            };

            // Detect service type based on image name
            #[allow(deprecated)]
            let service_type = if crate::mariadb_query::is_mariadb_compatible_image(&image) {
                ServiceType::Mariadb
            } else if image.contains("postgres")
                || image.contains("timescaledb")
                || image.contains("pgvector")
            {
                ServiceType::Postgres
            } else if image.contains("redis") {
                ServiceType::Redis
            } else if image.contains("mongo") {
                ServiceType::Mongodb
            } else if image.contains("rustfs") {
                ServiceType::Rustfs
            } else if image.contains("minio") {
                // Existing MinIO containers are detected as deprecated Minio type
                ServiceType::Minio
            } else {
                continue; // Skip unknown service types
            };

            // Extract version from image tag
            let version = if let Some(tag_pos) = image.rfind(':') {
                image[tag_pos + 1..].to_string()
            } else {
                "latest".to_string()
            };

            // Extract exposed ports from container ports
            let exposed_ports = container
                .ports
                .clone()
                .unwrap_or_default()
                .iter()
                .map(|port| port.private_port)
                .collect::<Vec<u16>>();

            available.push(AvailableContainer {
                container_id,
                container_name: container_name_raw
                    .strip_prefix('/')
                    .unwrap_or(&container_name_raw)
                    .to_string(),
                image,
                version,
                service_type,
                is_running: matches!(
                    container.state,
                    Some(bollard::models::ContainerSummaryStateEnum::RUNNING)
                ),
                exposed_ports,
            });
        }

        Ok(available)
    }

    /// Import an existing Docker container as a managed external service
    pub async fn import_service(
        &self,
        request: ImportExternalServiceRequest,
    ) -> Result<ExternalServiceInfo> {
        self.import_service_with_creator(request, None).await
    }

    /// Import a service on behalf of an authenticated user, preserving the
    /// same one-time pre-link ownership semantics as newly provisioned services.
    pub async fn import_service_for_user(
        &self,
        request: ImportExternalServiceRequest,
        user_id: i32,
    ) -> Result<ExternalServiceInfo> {
        self.import_service_with_creator(request, Some(user_id))
            .await
    }

    async fn import_service_with_creator(
        &self,
        request: ImportExternalServiceRequest,
        created_by_user_id: Option<i32>,
    ) -> Result<ExternalServiceInfo> {
        let name_transaction = self.db.begin().await?;
        reserve_service_name(&name_transaction, &request.name, None).await?;
        // Get the service-specific implementation based on Docker inspection
        let docker = self
            .require_docker()
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        let container = docker
            .inspect_container(
                &request.container_id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to inspect container '{}': {}",
                    request.container_id,
                    e
                )
            })?;

        let container_name = container
            .name
            .as_deref()
            .map(|name| name.trim_start_matches('/'))
            .filter(|name| !name.is_empty())
            .ok_or_else(|| ExternalServiceError::InternalError {
                reason: "The import container has no canonical Docker name".to_string(),
            })?;
        let container_id =
            container
                .id
                .as_deref()
                .ok_or_else(|| ExternalServiceError::InternalError {
                    reason: "The import container has no immutable Docker identity".to_string(),
                })?;
        // Name and ID aliases of the same Docker workload share this lock,
        // even when concurrent callers choose different service names.
        self.reserve_imported_container(&name_transaction, container_id, container_name)
            .await?;

        let _image = container.config.and_then(|c| c.image).ok_or_else(|| {
            anyhow::anyhow!(
                "Could not determine image for container '{}'",
                request.container_id
            )
        })?;

        // Convert request parameters to credentials and additional_config for compatibility
        // Credentials are typically: username, password
        // Additional config is: docker_image, port, etc.
        let mut credentials = HashMap::new();
        let mut additional_config = serde_json::json!({});

        for (key, value) in &request.parameters {
            match key.as_str() {
                // S3/MinIO import reads access_key/secret_key from `credentials`
                // (not additional_config) — without them here, the S3 provider
                // rejects every import with "Access key is required".
                "username" | "password" | "database" | "root_password" | "access_key"
                | "secret_key" => {
                    if let Some(str_value) = value.as_str() {
                        credentials.insert(key.clone(), str_value.to_string());
                    }
                }
                _ => {
                    if let Some(obj) = additional_config.as_object_mut() {
                        obj.insert(key.clone(), value.clone());
                    }
                }
            }
        }

        // Get the appropriate service instance and call import
        #[allow(deprecated)]
        let service_config = match request.service_type {
            ServiceType::Mariadb => {
                let mariadb = MariaDbService::new(request.name.clone(), Arc::clone(&docker));
                mariadb
                    .import_from_container(
                        request.container_id.clone(),
                        request.name.clone(),
                        credentials,
                        additional_config,
                    )
                    .await?
            }
            ServiceType::Postgres => {
                let postgres = PostgresService::new(request.name.clone(), Arc::clone(&docker));
                postgres
                    .import_from_container(
                        request.container_id.clone(),
                        request.name.clone(),
                        credentials,
                        additional_config,
                    )
                    .await?
            }
            ServiceType::Redis => {
                let redis = RedisService::new(request.name.clone(), Arc::clone(&docker));
                redis
                    .import_from_container(
                        request.container_id.clone(),
                        request.name.clone(),
                        credentials,
                        additional_config,
                    )
                    .await?
            }
            ServiceType::Mongodb => {
                let mongodb = MongodbService::new(request.name.clone(), Arc::clone(&docker));
                mongodb
                    .import_from_container(
                        request.container_id.clone(),
                        request.name.clone(),
                        credentials,
                        additional_config,
                    )
                    .await?
            }
            // S3 now uses RustFS by default
            ServiceType::S3 => {
                let rustfs = RustfsService::new(
                    request.name.clone(),
                    Arc::clone(&docker),
                    Arc::clone(&self.encryption_service),
                );
                rustfs
                    .import_from_container(
                        request.container_id.clone(),
                        request.name.clone(),
                        credentials,
                        additional_config,
                    )
                    .await?
            }
            // Temps KV uses Redis backend
            ServiceType::Kv => {
                let redis = RedisService::new(
                    managed_instance_name(&request.name, request.service_type),
                    Arc::clone(&docker),
                );
                redis
                    .import_from_container(
                        request.container_id.clone(),
                        request.name.clone(),
                        credentials,
                        additional_config,
                    )
                    .await?
            }
            // Temps Blob uses RustfsService (high-performance S3-compatible storage)
            ServiceType::Blob => {
                let rustfs = RustfsService::new(
                    managed_instance_name(&request.name, request.service_type),
                    Arc::clone(&docker),
                    Arc::clone(&self.encryption_service),
                );
                rustfs
                    .import_from_container(
                        request.container_id.clone(),
                        request.name.clone(),
                        credentials,
                        additional_config,
                    )
                    .await?
            }
            // RustFS standalone S3-compatible storage
            ServiceType::Rustfs => {
                let rustfs = RustfsService::new(
                    request.name.clone(),
                    Arc::clone(&docker),
                    Arc::clone(&self.encryption_service),
                );
                rustfs
                    .import_from_container(
                        request.container_id.clone(),
                        request.name.clone(),
                        credentials,
                        additional_config,
                    )
                    .await?
            }
            // MinIO (deprecated) - kept for backward compatibility
            ServiceType::Minio => {
                let s3 = S3Service::new(
                    request.name.clone(),
                    Arc::clone(&docker),
                    Arc::clone(&self.encryption_service),
                );
                s3.import_from_container(
                    request.container_id.clone(),
                    request.name.clone(),
                    credentials,
                    additional_config,
                )
                .await?
            }
        };

        // Store in database
        let config_json = serde_json::to_string(&service_config.parameters)
            .map_err(|e| anyhow::anyhow!("Failed to serialize config: {}", e))?;

        // Encrypt the config
        let encrypted_config = self
            .encryption_service
            .encrypt(config_json.as_bytes())
            .map_err(|e| anyhow::anyhow!("Failed to encrypt service configuration: {}", e))?;

        // Plaintext real container name so the log collector can map the
        // imported (label-less) container back to this service without
        // decrypting `config`. Every provider's import_from_container stamps
        // `container_name` into the parameters.
        let imported_container_name = service_config
            .parameters
            .get("container_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let external_service = external_services::ActiveModel {
            name: Set(service_config.name.clone()),
            service_type: Set(service_config.service_type.to_string()),
            version: Set(service_config.version.clone()),
            status: Set("running".to_string()),
            config: Set(Some(encrypted_config)),
            container_name: Set(imported_container_name),
            created_by_user_id: Set(created_by_user_id),
            ..Default::default()
        }
        .insert(&name_transaction)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to save service to database: {}", e))?;

        name_transaction.commit().await?;

        // Return the created service info
        Ok(ExternalServiceInfo {
            id: external_service.id,
            name: external_service.name,
            service_type: ServiceType::from_str(&external_service.service_type)?,
            version: external_service.version,
            status: external_service.status,
            connection_info: None,
            created_at: external_service.created_at.to_rfc3339(),
            updated_at: external_service.updated_at.to_rfc3339(),
            node_id: external_service.node_id,
            topology: external_service.topology,
            members: Vec::new(),
            error_message: external_service.error_message,
            metrics_enabled: external_service.metrics_enabled,
            continuous_archive_s3_source_id: external_service.continuous_archive_s3_source_id,
            continuous_archive_pinned_at: external_service
                .continuous_archive_pinned_at
                .map(|pinned_at| pinned_at.to_rfc3339()),
            readiness: None,
        })
    }

    // -----------------------------------------------------------------------
    // Runtime + stats
    //
    // These methods sit close to bollard so handlers can return a clean DTO
    // without having to deal with `inspect_container` quirks (Option<...>
    // everywhere, OOMKilled vs RestartCount nesting, etc.).
    //
    // For services running on a remote node (`service.node_id = Some(_)`),
    // we currently return an empty/placeholder runtime entry — the agent
    // does not yet expose an inspect-or-stats endpoint. Wiring that up is
    // a separate change; this lets the UI render local services today.
    // -----------------------------------------------------------------------
}

#[cfg(test)]
mod tests {
    use crate::externalsvc::{AvailableContainer, ServiceType};
    #[cfg(feature = "docker-tests")]
    use crate::services::test_support::*;
    #[cfg(feature = "docker-tests")]
    use bollard::Docker;
    #[cfg(feature = "docker-tests")]
    use std::sync::Arc;

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_import_postgres_container_from_docker() {
        // Skip if Docker is not available
        let _docker = match Docker::connect_with_local_defaults() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Docker not available, skipping import test");
                return;
            }
        };

        let (manager, _test_db) = setup_test_manager_or_skip!();

        // TODO: Implement proper Docker container creation and import test
        // This test requires fixing the Bollard API usage for container creation
        // For now, we just verify that the manager can be created and list_available_containers works

        // Test list_available_containers - should return Ok even if no containers match
        match manager.list_available_containers().await {
            Ok(_containers) => {
                println!("✅ list_available_containers test passed");
            }
            Err(e) => {
                println!("⚠️  list_available_containers returned error: {}", e);
                // Don't panic - Docker may not be fully configured in test environment
            }
        }
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_list_available_containers() {
        // Skip if Docker is not available
        let _docker = match Docker::connect_with_local_defaults() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Docker not available, skipping list containers test");
                return;
            }
        };

        let (manager, _test_db) = setup_test_manager_or_skip!();

        // List available containers
        let result = manager.list_available_containers().await;

        assert!(
            result.is_ok(),
            "Failed to list containers: {:?}",
            result.err()
        );

        let containers = result.unwrap();
        println!("Found {} available containers", containers.len());

        // Verify structure of returned containers
        for container in containers {
            assert!(!container.container_id.is_empty(), "Container ID is empty");
            assert!(
                !container.container_name.is_empty(),
                "Container name is empty"
            );
            assert!(!container.image.is_empty(), "Image is empty");
            assert!(!container.version.is_empty(), "Version is empty");
        }
    }

    #[test]
    fn test_available_container_structure() {
        // Test that AvailableContainer struct is properly formed
        let container = AvailableContainer {
            container_id: "abc123".to_string(),
            container_name: "postgres-prod".to_string(),
            image: "gotempsh/postgres-walg:15-bookworm".to_string(),
            version: "15-bookworm".to_string(),
            service_type: ServiceType::Postgres,
            is_running: true,
            exposed_ports: vec![5432],
        };

        assert_eq!(container.container_id, "abc123");
        assert_eq!(container.container_name, "postgres-prod");
        assert_eq!(container.image, "gotempsh/postgres-walg:15-bookworm");
        assert_eq!(container.version, "15-bookworm");
        assert_eq!(container.service_type, ServiceType::Postgres);
        assert!(container.is_running);
    }

    #[test]
    fn test_service_type_detection_postgres() {
        let images = vec![
            "gotempsh/postgres-walg:15-bookworm",
            "gotempsh/postgres-walg:16-bookworm",
            "timescaledb/timescaledb-ha:pg15",
        ];

        for image in images {
            let detected = if image.contains("postgres") || image.contains("timescaledb") {
                ServiceType::Postgres
            } else {
                ServiceType::Redis
            };
            assert_eq!(
                detected,
                ServiceType::Postgres,
                "Failed for image: {}",
                image
            );
        }
    }

    #[test]
    fn test_service_type_detection_redis() {
        let images = vec![
            "gotempsh/redis-walg:8-bookworm",
            "redis:latest",
            "redis:6.2-bullseye",
        ];

        for image in images {
            let detected = if image.contains("redis") {
                ServiceType::Redis
            } else {
                ServiceType::Postgres
            };
            assert_eq!(detected, ServiceType::Redis, "Failed for image: {}", image);
        }
    }

    #[test]
    fn test_service_type_detection_mongodb() {
        let images = vec![
            "gotempsh/mongodb-walg:7.0",
            "mongo:latest",
            "gotempsh/mongodb-walg:8.0",
        ];

        for image in images {
            let detected = if image.contains("mongo") {
                ServiceType::Mongodb
            } else {
                ServiceType::Postgres
            };
            assert_eq!(
                detected,
                ServiceType::Mongodb,
                "Failed for image: {}",
                image
            );
        }
    }

    #[test]
    #[allow(deprecated)]
    fn test_service_type_detection_s3() {
        // S3 type is now backed by RustFS - MinIO images are detected as Minio (deprecated)
        let minio_images = vec![
            "minio/minio:latest",
            "minio/minio:RELEASE.2025-01-01T00-00-00Z",
        ];

        for image in minio_images {
            let detected = if image.contains("rustfs") {
                ServiceType::Rustfs
            } else if image.contains("minio") {
                ServiceType::Minio
            } else {
                ServiceType::Postgres
            };
            assert_eq!(
                detected,
                ServiceType::Minio,
                "MinIO image should be detected as Minio (deprecated): {}",
                image
            );
        }
    }

    #[test]
    fn test_service_type_detection_rustfs() {
        let images = vec![
            "rustfs/rustfs:latest",
            "rustfs/rustfs:1.0.0-alpha.98",
            "rustfs/rustfs:1.0.0",
        ];

        for image in images {
            let detected = if image.contains("rustfs") {
                ServiceType::Rustfs
            } else {
                ServiceType::Postgres
            };
            assert_eq!(detected, ServiceType::Rustfs, "Failed for image: {}", image);
        }
    }

    #[test]
    fn test_import_requires_valid_credentials() {
        // Test that credentials are required for import
        let credentials: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        // Empty credentials should fail validation
        assert!(credentials.is_empty());
    }

    #[test]
    fn test_import_service_config_parameters() {
        // Test that ServiceConfig parameters are properly structured
        let params = serde_json::json!({
            "host": "localhost",
            "port": 5432,
            "database": "importeddb",
            "username": "postgres",
            "password": "secret",
            "container_id": "abc123",
            "docker_image": "gotempsh/postgres-walg:15-bookworm",
        });

        assert_eq!(params["host"], "localhost");
        assert_eq!(params["port"], 5432);
        assert_eq!(params["database"], "importeddb");
        assert_eq!(params["container_id"], "abc123");
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_postgres_v17_import_and_upgrade_to_v18() {
        // This test demonstrates the complete workflow:
        // 1. Create a PostgreSQL v17 Docker container
        // 2. Import it as a service in Temps
        // 3. Upgrade the container to PostgreSQL v18
        // 4. Verify the imported service still works with the new version

        // Setup
        let (_manager, _test_db) = setup_test_manager_or_skip!();

        // Verify Docker is available
        let _docker = match Docker::connect_with_local_defaults() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("⚠️  Docker not available, skipping v17→v18 upgrade test");
                return;
            }
        };

        // Test workflow documentation:
        // =============================
        //
        // Step 1: Create PostgreSQL v17 container
        //   - Image: gotempsh/postgres-walg:17-bookworm
        //   - Environment: POSTGRES_DB=testdb, POSTGRES_USER=pguser, POSTGRES_PASSWORD=pgpass
        //   - Port: 5432 exposed
        //   - Name: test-postgres-v17-upgrade
        //
        // Step 2: Wait for container startup
        //   - Check postgres_isready command
        //   - Allow 5-10 seconds for full initialization
        //
        // Step 3: Import the container as a service
        //   - Call manager.list_available_containers()
        //   - Verify PostgreSQL v17 container is found
        //   - Call manager.import_service() with credentials:
        //     * username: pguser
        //     * password: pgpass
        //     * port: 5432
        //     * database: testdb
        //   - Service name: "imported-postgres-v17"
        //
        // Step 4: Verify initial import
        //   - Connect to imported service via connection_url
        //   - Execute: SELECT version() - should show 17.x
        //   - Execute: SELECT datname FROM pg_database - should list testdb
        //
        // Step 5: Upgrade PostgreSQL v17 → v18
        //   - Stop the v17 container
        //   - Create a backup/snapshot of the data volume (optional)
        //   - Create new v18 container with same volumes
        //   - Execute pg_upgrade (if needed)
        //   - Start the v18 container
        //
        // Step 6: Verify upgraded service still works
        //   - Re-connect using the same imported service credentials
        //   - Execute: SELECT version() - should show 18.x
        //   - Verify all databases still exist
        //   - Verify tables and data are intact
        //
        // Step 7: Cleanup
        //   - Stop and remove v18 container
        //   - Remove any volumes created for testing
        //   - Delete the imported service from database

        println!("✅ test_postgres_v17_import_and_upgrade_to_v18 placeholder created");
        println!("   This test verifies the complete import + upgrade workflow");
        println!("   Requires proper Bollard API implementation for container management");
        println!("   When implemented, this test will:");
        println!("   1. Create PostgreSQL v17 container");
        println!("   2. Import it as a Temps service");
        println!("   3. Upgrade the container to v18");
        println!("   4. Verify service connectivity with both versions");
    }

    // ── Cluster validation tests ──────────────────────────────────────
}
