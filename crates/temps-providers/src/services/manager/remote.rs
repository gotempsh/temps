// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Services hosted on worker nodes: the remote client and its create
//! parameters.

use crate::externalsvc::{
    mariadb::{validate_mariadb_image, MariaDbSizeProfile, MARIADB_DEFAULT_IMAGE},
    rustfs::DEFAULT_RUSTFS_IMAGE,
    ServiceType,
};
use crate::remote_service_client::{
    RemotePortMapping, RemoteServiceClient, RemoteServiceCreateParams,
};
use crate::services::*;
use sea_orm::EntityTrait;
use std::collections::HashMap;
use temps_entities::{nodes, settings};
use tracing::warn;

/// The container to address on a node that has no persisted name for it: the
/// canonical name, unless only the legacy one (the bare instance name an
/// earlier release used) exists there.
fn select_remote_container_name(
    canonical_name: &str,
    canonical_exists: bool,
    legacy_name: &str,
    legacy_exists: bool,
) -> String {
    if canonical_exists || canonical_name == legacy_name || !legacy_exists {
        canonical_name.to_string()
    } else {
        legacy_name.to_string()
    }
}

impl ExternalServiceManager {
    /// Look up a node by ID and return a `RemoteServiceClient` ready to call
    /// the agent's service endpoints.
    pub(in crate::services) async fn get_remote_client(
        &self,
        node_id: i32,
    ) -> Result<RemoteServiceClient, ExternalServiceError> {
        let node = nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::InternalError {
                reason: format!("Node {} not found", node_id),
            })?;

        let token = node
            .token_encrypted
            .as_deref()
            .ok_or(ExternalServiceError::InternalError {
                reason: format!(
                    "Node {} ({}) has no encrypted token — cannot authenticate",
                    node_id, node.name
                ),
            })
            .and_then(|encrypted| {
                self.encryption_service
                    .decrypt_string(encrypted)
                    .map_err(|e| ExternalServiceError::InternalError {
                        reason: format!(
                            "Failed to decrypt token for node {} ({}): {}",
                            node_id, node.name, e
                        ),
                    })
            })?;

        let sealing = node.request_sealing();
        if node.address.starts_with("https://") {
            let settings_row = settings::Entity::find_by_id(1)
                .one(self.db.as_ref())
                .await?
                .ok_or_else(|| ExternalServiceError::InternalError {
                    reason: format!(
                        "Cannot authenticate mTLS node {} ({}): application settings row is missing",
                        node_id, node.name
                    ),
                })?;
            let app_settings = temps_config::AppSettings::from_json(settings_row.data);
            let ca_cert = app_settings.multi_node.cluster_ca_cert_pem.ok_or_else(|| {
                ExternalServiceError::InternalError {
                    reason: format!(
                        "Cannot authenticate mTLS node {} ({}): cluster CA certificate is missing",
                        node_id, node.name
                    ),
                }
            })?;
            let encrypted_ca_key = app_settings
                .multi_node
                .cluster_ca_key_encrypted
                .ok_or_else(|| ExternalServiceError::InternalError {
                    reason: format!(
                        "Cannot authenticate mTLS node {} ({}): encrypted cluster CA key is missing",
                        node_id, node.name
                    ),
                })?;
            let ca_key = self
                .encryption_service
                .decrypt_string(&encrypted_ca_key)
                .map_err(|e| ExternalServiceError::InternalError {
                    reason: format!(
                        "Cannot authenticate mTLS node {} ({}): failed to decrypt cluster CA key: {}",
                        node_id, node.name, e
                    ),
                })?;
            let csr = temps_core::node_pki::generate_node_keypair_csr(
                "temps-control-plane",
                &[],
            )
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!(
                    "Cannot authenticate mTLS node {} ({}): failed to generate control-plane identity: {}",
                    node_id, node.name, e
                ),
            })?;
            let signed = temps_core::node_pki::sign_node_csr(
                &ca_cert,
                &ca_key,
                &csr.csr_pem,
                &[],
            )
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!(
                    "Cannot authenticate mTLS node {} ({}): failed to sign control-plane identity: {}",
                    node_id, node.name, e
                ),
            })?;
            let identity_pem = format!("{}\n{}", signed.cert_pem, csr.key_pem);
            // Over the WireGuard mesh for a node that joined with a public
            // address, verified for its registered host either way.
            let endpoint = node.agent_endpoint();
            if endpoint.via_mesh() {
                tracing::debug!(
                    node_id,
                    node = %node.name,
                    dial = %endpoint.dial_url,
                    "reaching the node agent over the WireGuard mesh"
                );
            }
            RemoteServiceClient::new_mtls_at(&endpoint, token, node.name, &identity_pem, &ca_cert)
                .map(|client| client.with_sealing(sealing))
        } else {
            RemoteServiceClient::new(node.address, token, node.name)
                .map(|client| client.with_sealing(sealing))
        }
    }

    /// The container of a standalone service on a worker node: the name the
    /// agent reported at create time, else the engine's canonical name, else
    /// the bare instance name an earlier release used, whichever the node
    /// has. Builds no engine, so it works without a local Docker handle.
    pub(super) async fn resolve_remote_container_name(
        &self,
        client: &RemoteServiceClient,
        service_id: i32,
        service_name: &str,
        service_type: ServiceType,
        parameters: &HashMap<String, serde_json::Value>,
    ) -> Result<String, ExternalServiceError> {
        let persisted_name = parameters
            .get("container_name")
            .and_then(serde_json::Value::as_str)
            .filter(|name| !name.is_empty());
        if let Some(container_name) = persisted_name {
            return Ok(container_name.to_string());
        }

        // The container is on the node, so the names are all that's needed.
        // No engine is built: that would need a local Docker handle.
        let selection =
            EngineSelection::for_parameter_map(service_id, service_name, service_type, parameters)?;
        let canonical_name = selection.container_name();
        let canonical_exists = client
            .service_status(&canonical_name)
            .await?
            .container_id
            .is_some();

        let legacy_name = selection.instance_name;
        let legacy_exists = if legacy_name != canonical_name && !canonical_exists {
            client
                .service_status(&legacy_name)
                .await?
                .container_id
                .is_some()
        } else {
            false
        };

        Ok(select_remote_container_name(
            &canonical_name,
            canonical_exists,
            &legacy_name,
            legacy_exists,
        ))
    }

    /// Server kind and image of a MinIO-engine service created on a remote
    /// node. The image lives on the node, so its metadata can't be read here:
    /// an explicit `server` wins; without one the config predates the field,
    /// i.e. it was created as MinIO — the MinIO image this path used to
    /// default to when none is stored, the stored image run as MinIO otherwise (logged, since a
    /// mislabelled RustFS image would then fail its healthcheck). New
    /// services always store `server` (see `MinioParameterStrategy`).
    fn remote_s3_server_kind_and_image(
        service_id: i32,
        parameters: &HashMap<String, String>,
    ) -> Result<(crate::externalsvc::s3::S3ServerKind, String), ExternalServiceError> {
        use crate::externalsvc::s3::S3ServerKind;

        let explicit = parameters
            .get("server")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(S3ServerKind::parse)
            .transpose()
            .map_err(|e| ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: e.to_string(),
            })?;
        let image = parameters
            .get("docker_image")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        Ok(match (explicit, image) {
            (Some(kind), Some(image)) => (kind, image),
            (Some(kind), None) => (kind, kind.default_image().to_string()),
            // The image this path defaulted to before the field existed, so
            // a node that still holds it keeps using it.
            (None, None) => (
                S3ServerKind::Minio,
                "quay.io/minio/minio:latest".to_string(),
            ),
            (None, Some(image)) => {
                warn!(
                    "Remote S3 service image '{}' has no stored server kind; running it as MinIO, \
                     as it was created. Set the service's `server` parameter to 'rustfs' or \
                     'minio' if that is wrong.",
                    image
                );
                (S3ServerKind::Minio, image)
            }
        })
    }

    /// Build the `RemoteServiceCreateParams` that the agent needs to create a
    /// Docker container for a given service type and parameters.
    pub(super) fn build_remote_create_params(
        &self,
        service_id: i32,
        service_name: &str,
        service_type: &ServiceType,
        parameters: &HashMap<String, String>,
    ) -> Result<RemoteServiceCreateParams, ExternalServiceError> {
        Self::remote_create_params(
            service_id,
            service_name,
            service_type,
            parameters,
            self.local_workloads_enabled,
        )
    }

    /// The create request a worker's agent receives for a service, built
    /// from the service's stored parameters without any Docker access.
    /// `local_workloads_enabled` only shapes the remedy of a refusal.
    pub fn remote_create_params(
        service_id: i32,
        service_name: &str,
        service_type: &ServiceType,
        parameters: &HashMap<String, String>,
        local_workloads_enabled: bool,
    ) -> Result<RemoteServiceCreateParams, ExternalServiceError> {
        #[allow(deprecated)]
        let backend_service_type = match (
            *service_type,
            parameters
                .get("backend")
                .map(|backend| backend.trim().to_ascii_lowercase()),
        ) {
            (ServiceType::S3, Some(backend)) if backend == "minio" => ServiceType::Minio,
            (ServiceType::Blob, Some(backend)) if backend == "minio" => {
                return Err(ExternalServiceError::ParameterValidationFailed {
                    service_id,
                    reason: "managed S3 backend 'minio' is only supported for S3 services; use the default 'rustfs' backend for Blob services".to_string(),
                });
            }
            (ServiceType::Blob, Some(backend)) if backend == "garage" => {
                return Err(ExternalServiceError::ParameterValidationFailed {
                    service_id,
                    reason: "managed S3 backend 'garage' is only supported for S3 services; the built-in Blob service runs on the default 'rustfs' backend".to_string(),
                });
            }
            (ServiceType::S3, Some(backend)) if backend == "garage" => {
                // Garage reads its configuration from a `garage.toml` that has
                // to be written into the container before it starts; the
                // agent's generic create request carries no files, so a worker
                // cannot create one yet. Creating it on this server is only a
                // remedy when this server runs local workloads.
                let remedy = if local_workloads_enabled {
                    "Create it on this server instead (leave node_id unset), or use the default 'rustfs' backend on the worker"
                } else {
                    "This server runs no local workloads (serve profile 'control-plane'), so use the default 'rustfs' backend on the worker"
                };
                return Err(ExternalServiceError::ParameterValidationFailed {
                    service_id,
                    reason: format!(
                        "managed S3 backend 'garage' cannot be created on a worker node for service '{service_name}': the worker agent cannot yet deliver garage.toml. {remedy}"
                    ),
                });
            }
            _ => *service_type,
        };

        let (image, container_port, env, volume_path, command) = match backend_service_type {
            ServiceType::Mariadb => {
                let image = parameters
                    .get("docker_image")
                    .cloned()
                    .unwrap_or_else(|| MARIADB_DEFAULT_IMAGE.to_string());
                validate_mariadb_image(&image).map_err(|reason| {
                    ExternalServiceError::ParameterValidationFailed { service_id, reason }
                })?;
                let size_profile = parameters
                    .get("size_profile")
                    .and_then(|value| MariaDbSizeProfile::parse(value))
                    .unwrap_or_default();
                let root_password = parameters.get("root_password").cloned().unwrap_or_default();
                let password = parameters.get("password").cloned().unwrap_or_default();
                let database = parameters
                    .get("database")
                    .cloned()
                    .unwrap_or_else(|| "app".to_string());
                let username = parameters
                    .get("username")
                    .cloned()
                    .unwrap_or_else(|| "app".to_string());

                let env = HashMap::from([
                    ("MARIADB_ROOT_PASSWORD".to_string(), root_password),
                    ("MARIADB_DATABASE".to_string(), database),
                    ("MARIADB_USER".to_string(), username),
                    ("MARIADB_PASSWORD".to_string(), password),
                    ("MARIADB_AUTO_UPGRADE".to_string(), "1".to_string()),
                ]);
                (
                    image,
                    3306u16,
                    env,
                    "/var/lib/mysql".to_string(),
                    Some(size_profile.server_args()),
                )
            }
            ServiceType::Postgres => {
                let image = parameters
                    .get("docker_image")
                    .cloned()
                    .unwrap_or_else(|| "gotempsh/postgres-walg:18-bookworm".to_string());
                let password = parameters.get("password").cloned().unwrap_or_default();
                let database = parameters
                    .get("database")
                    .cloned()
                    .unwrap_or_else(|| "postgres".to_string());
                let username = parameters
                    .get("username")
                    .cloned()
                    .unwrap_or_else(|| "postgres".to_string());
                let max_connections = parameters
                    .get("max_connections")
                    .cloned()
                    .unwrap_or_else(|| "100".to_string());

                let env = HashMap::from([
                    ("POSTGRES_USER".to_string(), username),
                    ("POSTGRES_PASSWORD".to_string(), password),
                    ("POSTGRES_DB".to_string(), database),
                    ("POSTGRES_HOST_AUTH_METHOD".to_string(), "md5".to_string()),
                ]);
                // archive_mode=off until enable_wal_archiving() flips it on
                // together with archive_command. See externalsvc/postgres.rs
                // for the same invariant on the create-container path.
                let cmd = vec![
                    "postgres".to_string(),
                    "-c".to_string(),
                    format!("max_connections={}", max_connections),
                    "-c".to_string(),
                    "wal_level=replica".to_string(),
                    "-c".to_string(),
                    "archive_mode=off".to_string(),
                    "-c".to_string(),
                    "archive_timeout=60".to_string(),
                ];
                (
                    image,
                    5432u16,
                    env,
                    "/var/lib/postgresql".to_string(),
                    Some(cmd),
                )
            }
            ServiceType::Redis => {
                let image = parameters
                    .get("docker_image")
                    .cloned()
                    .unwrap_or_else(|| "gotempsh/redis-walg:8-bookworm".to_string());
                let password = parameters.get("password").cloned().unwrap_or_default();
                let env = HashMap::new();
                let cmd =
                    crate::externalsvc::redis::redis_container_command(&image, &password, false);
                (image, 6379u16, env, "/data".to_string(), Some(cmd))
            }
            ServiceType::Mongodb => {
                let image = parameters
                    .get("docker_image")
                    .cloned()
                    .unwrap_or_else(|| "mongo:7".to_string());
                let username = parameters
                    .get("username")
                    .cloned()
                    .unwrap_or_else(|| "admin".to_string());
                let password = parameters.get("password").cloned().unwrap_or_default();
                let database = parameters
                    .get("database")
                    .cloned()
                    .unwrap_or_else(|| "admin".to_string());
                let env = HashMap::from([
                    ("MONGO_INITDB_ROOT_USERNAME".to_string(), username),
                    ("MONGO_INITDB_ROOT_PASSWORD".to_string(), password),
                    ("MONGO_INITDB_DATABASE".to_string(), database),
                ]);
                (image, 27017u16, env, "/data/db".to_string(), None)
            }
            ServiceType::S3 | ServiceType::Rustfs | ServiceType::Blob => {
                let image = parameters
                    .get("docker_image")
                    .cloned()
                    .unwrap_or_else(|| DEFAULT_RUSTFS_IMAGE.to_string());
                let access_key = parameters
                    .get("access_key")
                    .cloned()
                    .unwrap_or_else(|| "minioadmin".to_string());
                let secret_key = parameters.get("secret_key").cloned().unwrap_or_default();
                // RustFS reads RUSTFS_ACCESS_KEY/RUSTFS_SECRET_KEY. It ignores
                // RUSTFS_ROOT_USER/RUSTFS_ROOT_PASSWORD, which left remote
                // containers on RustFS's built-in credentials instead of the
                // ones stored for the service.
                let env = HashMap::from([
                    ("RUSTFS_ACCESS_KEY".to_string(), access_key),
                    ("RUSTFS_SECRET_KEY".to_string(), secret_key),
                ]);
                let cmd = vec![
                    "rustfs".to_string(),
                    "server".to_string(),
                    "/data".to_string(),
                ];
                (image, 9000u16, env, "/data".to_string(), Some(cmd))
            }
            ServiceType::Kv => {
                // KV is Redis-backed
                let image = parameters
                    .get("docker_image")
                    .cloned()
                    .unwrap_or_else(|| "gotempsh/redis-walg:8-bookworm".to_string());
                let password = parameters.get("password").cloned().unwrap_or_default();
                let env = HashMap::new();
                let cmd =
                    crate::externalsvc::redis::redis_container_command(&image, &password, false);
                (image, 6379u16, env, "/data".to_string(), Some(cmd))
            }
            #[allow(deprecated)]
            ServiceType::Minio => {
                let (kind, image) = Self::remote_s3_server_kind_and_image(service_id, parameters)?;
                let access_key = parameters
                    .get("access_key")
                    .cloned()
                    .unwrap_or_else(|| "minioadmin".to_string());
                let secret_key = parameters.get("secret_key").cloned().unwrap_or_default();
                let env = crate::externalsvc::s3::s3_server_credentials_env(
                    kind,
                    &access_key,
                    &secret_key,
                )
                .into_iter()
                .map(|(name, value)| (name.to_string(), value))
                .collect::<HashMap<_, _>>();
                let cmd = vec![
                    kind.as_str().to_string(),
                    "server".to_string(),
                    "/data".to_string(),
                ];
                (image, 9000u16, env, "/data".to_string(), Some(cmd))
            }
        };

        let host_port: u16 = parameters
            .get("port")
            .and_then(|p| p.parse().ok())
            .unwrap_or(container_port);

        // No engine is built here: that needs a local Docker handle, which a
        // control-plane-profile server does not have.
        let container_name =
            EngineSelection::for_type(service_name, backend_service_type).container_name();
        let container_name_for_volume = format!("{}-{}", backend_service_type, service_name);
        let volume_name = format!("{}_data", container_name_for_volume);

        // Resource limits may arrive as the modern nested `resources` block or
        // as legacy flat string keys (`memory_mb=512`, `nano_cpus=1000000000`,
        // etc.). Missing limits mean unlimited.
        let resource_limits = Self::remote_resource_limits_from_parameters(parameters);

        Ok(RemoteServiceCreateParams {
            name: container_name,
            service_type: service_type.to_string(),
            image,
            environment: env,
            port_mappings: vec![RemotePortMapping {
                host_port,
                container_port,
            }],
            volumes: HashMap::from([(volume_name, volume_path)]),
            network: Some(temps_core::NETWORK_NAME.to_string()),
            command,
            resource_limits,
        })
    }

    fn remote_resource_limits_from_parameters(
        parameters: &HashMap<String, String>,
    ) -> Option<crate::externalsvc::ServiceResourceLimits> {
        if let Some(resources) = parameters.get("resources") {
            if let Ok(limits) =
                serde_json::from_str::<crate::externalsvc::ServiceResourceLimits>(resources)
            {
                if !limits.is_unlimited() {
                    return Some(limits);
                }
            }
        }

        let parse_i64 = |key: &str| -> Option<i64> {
            parameters
                .get(key)
                .and_then(|s| s.trim().parse::<i64>().ok())
                .filter(|&n| n > 0)
        };
        let limits = crate::externalsvc::ServiceResourceLimits {
            memory_mb: parse_i64("memory_mb"),
            memory_swap_mb: parse_i64("memory_swap_mb"),
            nano_cpus: parse_i64("nano_cpus"),
            cpu_shares: parse_i64("cpu_shares"),
            shm_size_mb: parse_i64("shm_size_mb"),
        };
        if limits.is_unlimited() {
            None
        } else {
            Some(limits)
        }
    }

    /// Whether `container_name` runs on worker `node_id`, asked of that
    /// node's agent (the container is not on this host's Docker).
    pub async fn worker_service_running(
        &self,
        node_id: i32,
        container_name: &str,
    ) -> Result<bool, ExternalServiceError> {
        let client = self.get_remote_client(node_id).await?;
        let status = client.service_status(container_name).await?;
        Ok(status.running)
    }

    /// Run `command` inside `container_name` on worker `node_id` through its
    /// agent and return the exit code.
    pub async fn exec_in_worker_service(
        &self,
        node_id: i32,
        container_name: &str,
        command: Vec<String>,
    ) -> Result<i64, ExternalServiceError> {
        let client = self.get_remote_client(node_id).await?;
        let result = client
            .exec_in_service(crate::remote_service_client::RemoteExecParams {
                container_name: container_name.to_string(),
                command,
                environment: HashMap::new(),
                user: None,
                detach: false,
            })
            .await
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!(
                    "Remote exec failed in '{}' on node {}: {}",
                    container_name, node_id, e
                ),
            })?;
        Ok(result.exit_code)
    }

    /// The worker node service `service_id` runs on, refusing one on the
    /// control plane: its data operations take the local engines.
    async fn worker_service_target(
        &self,
        service_id: i32,
        operation: &str,
    ) -> Result<(i32, crate::externalsvc::ServiceConfig), ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        let ServiceExecutionRoute::Remote(node_id) = service_execution_route(service.node_id)
        else {
            return Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: format!(
                    "service {} ('{}') runs on the control plane; its {operation} uses this \
                     server's engines, not a worker's",
                    service.id, service.name
                ),
            });
        };
        let config = self.get_service_config(service_id).await?;
        Ok((node_id, config))
    }

    /// Back up service `service_id`, which runs on a worker node, on that
    /// node: the agent runs the engine's own dump (or bucket mirror) and
    /// writes it to `key` in `destination`. Credentials travel sealed.
    pub async fn backup_worker_service(
        &self,
        service_id: i32,
        destination: crate::remote_service_client::WorkerS3Destination,
        key: String,
    ) -> Result<crate::remote_service_client::RemoteServiceBackupResponse, ExternalServiceError>
    {
        let (node_id, service_config) = self.worker_service_target(service_id, "backup").await?;
        self.get_remote_client(node_id)
            .await?
            .backup_service(&crate::remote_service_client::RemoteServiceBackupRequest {
                service_config,
                destination,
                key,
            })
            .await
    }

    /// Restore service `service_id`, which runs on a worker node, in place on
    /// that node from the backup at `location` in `source`.
    pub async fn restore_worker_service(
        &self,
        service_id: i32,
        source: crate::remote_service_client::WorkerS3Destination,
        location: String,
    ) -> Result<crate::remote_service_client::RemoteServiceRestoreResponse, ExternalServiceError>
    {
        let (node_id, service_config) = self.worker_service_target(service_id, "restore").await?;
        self.get_remote_client(node_id)
            .await?
            .restore_service(&crate::remote_service_client::RemoteServiceRestoreRequest {
                service_config,
                source,
                location,
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::externalsvc::{rustfs::DEFAULT_RUSTFS_IMAGE, ServiceType};
    use crate::services::test_support::*;
    use std::collections::HashMap;

    #[test]
    fn remote_create_uses_provider_canonical_container_names() {
        let manager = mock_service_manager(vec![]);

        for (service_type, expected_name, expected_image) in [
            (ServiceType::Postgres, "postgres-orders", None),
            (ServiceType::Mariadb, "mariadb-orders", None),
            (ServiceType::Mongodb, "temps-mongodb-orders", None),
            (ServiceType::Redis, "redis-orders", None),
            (
                ServiceType::Rustfs,
                "rustfs-orders",
                Some(DEFAULT_RUSTFS_IMAGE),
            ),
            (ServiceType::S3, "rustfs-orders", Some(DEFAULT_RUSTFS_IMAGE)),
            (
                ServiceType::Blob,
                "rustfs-orders",
                Some(DEFAULT_RUSTFS_IMAGE),
            ),
        ] {
            let parameters = if service_type == ServiceType::Mariadb {
                HashMap::from([(
                    "docker_image".to_string(),
                    "ghcr.io/gotempsh/mariadb-walg@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                        .to_string(),
                )])
            } else {
                HashMap::new()
            };
            let params = manager
                .build_remote_create_params(7, "orders", &service_type, &parameters)
                .expect("default remote service parameters should be valid");
            assert_eq!(params.name, expected_name, "wrong name for {service_type}");
            if let Some(expected_image) = expected_image {
                assert_eq!(params.image, expected_image);
            }
        }
    }

    /// Regression test for issue #1780: a Dragonfly image has no `redis-server`
    /// binary, so the worker's container must start Dragonfly's own.
    #[test]
    fn remote_redis_command_follows_the_configured_image() {
        let manager = mock_service_manager(vec![]);

        for service_type in [ServiceType::Redis, ServiceType::Kv] {
            let dragonfly = HashMap::from([
                (
                    "docker_image".to_string(),
                    "docker.dragonflydb.io/dragonflydb/dragonfly:latest".to_string(),
                ),
                ("password".to_string(), "seeded-secret".to_string()),
            ]);
            let params = manager
                .build_remote_create_params(7, "orders", &service_type, &dragonfly)
                .expect("a Dragonfly image is a valid override");
            assert_eq!(
                params.command,
                Some(vec![
                    "dragonfly".to_string(),
                    "--logtostderr".to_string(),
                    "--requirepass".to_string(),
                    "seeded-secret".to_string(),
                ]),
                "wrong command for {service_type}"
            );

            let redis = HashMap::from([("password".to_string(), "seeded-secret".to_string())]);
            let params = manager
                .build_remote_create_params(7, "orders", &service_type, &redis)
                .expect("default parameters are valid");
            assert_eq!(
                params.command,
                Some(vec![
                    "redis-server".to_string(),
                    "--requirepass".to_string(),
                    "seeded-secret".to_string(),
                ]),
                "a real Redis keeps the command existing containers were created with"
            );
        }
    }

    /// A worker cannot be handed `garage.toml` by the generic agent create
    /// request, so a remote Garage service is refused up front, under the
    /// service's own ID and name, rather than started without its config.
    /// Creating it on this server is offered only when this server runs
    /// local workloads.
    #[test]
    fn remote_garage_creation_is_refused_with_an_actionable_error() {
        let parameters = HashMap::from([("backend".to_string(), "Garage".to_string())]);
        let refusal = |manager: ExternalServiceManager| match manager.build_remote_create_params(
            7,
            "orders",
            &ServiceType::S3,
            &parameters,
        ) {
            Err(ExternalServiceError::ParameterValidationFailed { service_id, reason }) => {
                assert_eq!(service_id, 7, "{reason}");
                assert!(
                    reason.contains("'orders'") && reason.contains("garage.toml"),
                    "{reason}"
                );
                reason
            }
            other => panic!("expected a validation error, got {other:?}"),
        };

        let full = refusal(unreachable_docker_manager());
        assert!(full.contains("leave node_id unset"), "{full}");
        assert!(!full.contains("control-plane"), "{full}");

        let control_plane = refusal(control_plane_manager());
        assert!(
            control_plane.contains("serve profile 'control-plane'")
                && control_plane.contains("'rustfs' backend"),
            "{control_plane}"
        );
        assert!(
            !control_plane.contains("this server instead"),
            "{control_plane}"
        );
    }

    /// The built-in Blob service has no Garage engine, on a worker or not.
    #[test]
    fn remote_blob_creation_rejects_the_garage_backend() {
        let manager = control_plane_manager();
        let parameters = HashMap::from([("backend".to_string(), "garage".to_string())]);
        let err = manager
            .build_remote_create_params(7, "assets", &ServiceType::Blob, &parameters)
            .unwrap_err();
        match err {
            ExternalServiceError::ParameterValidationFailed { service_id, reason } => {
                assert_eq!(service_id, 7, "{reason}");
                assert!(
                    reason.contains("only supported for S3 services"),
                    "{reason}"
                );
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// The remote MinIO-engine path configures the container from the
    /// stored `server` kind, never from the image name: a private mirror of
    /// RustFS with a neutral name runs RustFS, and a config stored before the
    /// field existed runs as the MinIO service it was created as.
    #[test]
    #[allow(deprecated)]
    fn remote_minio_engine_configures_the_container_from_the_server_kind() {
        let manager = mock_service_manager(vec![]);
        let build = |parameters: &[(&str, &str)]| {
            let parameters = parameters
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<_, _>>();
            manager.build_remote_create_params(7, "orders", &ServiceType::Minio, &parameters)
        };

        let mirror = "registry.internal:5000/mirror/object-store:1.0.0";
        let params = build(&[
            ("docker_image", mirror),
            ("server", "rustfs"),
            ("access_key", "AKIAEXAMPLE"),
            ("secret_key", "secret"),
        ])
        .unwrap();
        assert_eq!(params.image, mirror);
        assert_eq!(
            params
                .environment
                .get("RUSTFS_ACCESS_KEY")
                .map(String::as_str),
            Some("AKIAEXAMPLE")
        );
        assert!(!params.environment.contains_key("MINIO_ROOT_USER"));
        assert_eq!(params.command.as_ref().unwrap()[0], "rustfs");

        let params = build(&[("server", "rustfs")]).unwrap();
        assert_eq!(params.image, DEFAULT_RUSTFS_IMAGE);

        // Legacy rows (no `server`): MinIO, whatever the image is called.
        for parameters in [
            vec![("docker_image", "registry.internal:5000/mirror/rustfs:1.0.0")],
            vec![],
        ] {
            let params = build(&parameters).unwrap();
            assert!(
                params.environment.contains_key("MINIO_ROOT_USER"),
                "{parameters:?}"
            );
            assert_eq!(params.command.as_ref().unwrap()[0], "minio");
        }
        assert_eq!(build(&[]).unwrap().image, "quay.io/minio/minio:latest");

        assert!(matches!(
            build(&[("server", "ceph")]),
            Err(ExternalServiceError::ParameterValidationFailed { .. })
        ));
    }

    #[test]
    fn remote_container_resolution_prefers_canonical_then_legacy_names() {
        assert_eq!(
            select_remote_container_name("postgres-orders", true, "orders", false),
            "postgres-orders"
        );
        assert_eq!(
            select_remote_container_name("postgres-orders", true, "orders", true),
            "postgres-orders"
        );
        assert_eq!(
            select_remote_container_name("postgres-orders", false, "orders", true),
            "orders"
        );
        assert_eq!(
            select_remote_container_name("postgres-orders", false, "orders", false),
            "postgres-orders"
        );
    }

    /// A fake worker agent whose `status` route reports a container for each
    /// name in `existing` and none for any other.
    async fn agent_with_containers(existing: &'static [&'static str]) -> RemoteServiceClient {
        use axum::{extract::Path, routing::get, Json, Router};

        let app = Router::new().route(
            "/agent/services/{name}/status",
            get(move |Path(name): Path<String>| async move {
                let container_id = existing
                    .contains(&name.as_str())
                    .then(|| format!("id-{name}"));
                Json(serde_json::json!({
                    "success": true,
                    "data": {
                        "container_name": name,
                        "container_id": container_id,
                        "running": container_id.is_some(),
                        "health": null,
                    },
                    "error": null,
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        RemoteServiceClient::new(
            format!("http://{address}"),
            "node-token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap()
    }

    /// Start, stop and delete on a worker address its container by name. A
    /// `--profile control-plane` server has no Docker handle, so the name
    /// must be found without building an engine.
    #[tokio::test]
    async fn remote_container_resolution_needs_no_local_docker() {
        let manager = control_plane_manager();
        assert!(
            manager.require_docker().is_err(),
            "the control-plane manager must have no Docker handle"
        );
        let no_parameters = HashMap::new();

        let canonical = agent_with_containers(&["postgres-orders"]).await;
        assert_eq!(
            manager
                .resolve_remote_container_name(
                    &canonical,
                    7,
                    "orders",
                    ServiceType::Postgres,
                    &no_parameters,
                )
                .await
                .unwrap(),
            "postgres-orders"
        );

        // A container an earlier release created under the bare name.
        let legacy = agent_with_containers(&["orders"]).await;
        assert_eq!(
            manager
                .resolve_remote_container_name(
                    &legacy,
                    7,
                    "orders",
                    ServiceType::Postgres,
                    &no_parameters,
                )
                .await
                .unwrap(),
            "orders"
        );

        // The stored backend picks the engine, so its container name.
        let minio = agent_with_containers(&["minio-orders"]).await;
        let on_minio = HashMap::from([("backend".to_string(), serde_json::json!("minio"))]);
        assert_eq!(
            manager
                .resolve_remote_container_name(&minio, 7, "orders", ServiceType::S3, &on_minio)
                .await
                .unwrap(),
            "minio-orders"
        );

        // The name the agent reported at create time wins, without asking it.
        let persisted = HashMap::from([(
            "container_name".to_string(),
            serde_json::json!("orders-from-agent"),
        )]);
        assert_eq!(
            manager
                .resolve_remote_container_name(
                    &canonical,
                    7,
                    "orders",
                    ServiceType::Postgres,
                    &persisted,
                )
                .await
                .unwrap(),
            "orders-from-agent"
        );

        // A stored backend that cannot run the service is refused with the
        // service's ID, not a Docker error.
        let blob_on_minio = HashMap::from([("backend".to_string(), serde_json::json!("minio"))]);
        let error = manager
            .resolve_remote_container_name(
                &canonical,
                7,
                "assets",
                ServiceType::Blob,
                &blob_on_minio,
            )
            .await
            .expect_err("Blob cannot run on MinIO");
        assert!(
            matches!(
                error,
                ExternalServiceError::ParameterValidationFailed { service_id: 7, .. }
            ),
            "{error:?}"
        );
    }

    /// A `--profile control-plane` server has no Docker handle, but must
    /// still be able to create services on worker nodes.
    #[test]
    #[allow(deprecated, reason = "existing MinIO services can still be recreated")]
    fn remote_create_params_need_no_local_docker() {
        let manager = control_plane_manager();
        assert!(
            manager.require_docker().is_err(),
            "the control-plane manager must have no Docker handle"
        );

        let on_minio = HashMap::from([("backend".to_string(), "minio".to_string())]);
        for (service_type, parameters, expected_name) in [
            (ServiceType::Postgres, HashMap::new(), "postgres-orders"),
            (ServiceType::Mariadb, HashMap::new(), "mariadb-orders"),
            (ServiceType::Mongodb, HashMap::new(), "temps-mongodb-orders"),
            (ServiceType::Redis, HashMap::new(), "redis-orders"),
            (ServiceType::Kv, HashMap::new(), "redis-orders"),
            (ServiceType::S3, HashMap::new(), "rustfs-orders"),
            (ServiceType::S3, on_minio, "minio-orders"),
            (ServiceType::Rustfs, HashMap::new(), "rustfs-orders"),
            (ServiceType::Blob, HashMap::new(), "rustfs-orders"),
            (ServiceType::Minio, HashMap::new(), "minio-orders"),
        ] {
            let params = manager
                .build_remote_create_params(7, "orders", &service_type, &parameters)
                .unwrap_or_else(|e| panic!("{service_type} on a worker needs no Docker: {e}"));
            assert_eq!(
                params.name, expected_name,
                "wrong name for {service_type} with {parameters:?}"
            );
        }
    }

    /// Starting a service on a worker ends by asking for its readiness gate.
    /// There is none for a worker service, and asking must not need Docker.
    #[tokio::test]
    async fn remote_service_readiness_gate_needs_no_local_docker() {
        let manager = control_plane_manager();
        let mut service = encrypted_service_model(1, serde_json::json!({}));
        service.node_id = Some(2);

        let gate = manager
            .readiness_gate_for(&service)
            .await
            .expect("a worker service has no gate to build");
        assert!(gate.is_none());
    }

    // ── repoint_continuous_archive_source ───────────────────────────────────
}
