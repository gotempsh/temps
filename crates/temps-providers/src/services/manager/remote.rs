// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Services hosted on worker nodes: the remote client and its create
//! parameters.

use crate::externalsvc::{
    mariadb::{validate_mariadb_image, MariaDbSizeProfile, MARIADB_DEFAULT_IMAGE},
    rustfs::DEFAULT_RUSTFS_IMAGE,
    ExternalService, ServiceType,
};
use crate::remote_service_client::{
    RemotePortMapping, RemoteServiceClient, RemoteServiceCreateParams,
};
use crate::services::*;
use sea_orm::EntityTrait;
use std::collections::HashMap;
use temps_entities::{nodes, settings};
use tracing::warn;

fn select_remote_container_name(
    persisted_name: Option<&str>,
    canonical_name: &str,
    canonical_exists: bool,
    legacy_name: &str,
    legacy_exists: bool,
) -> String {
    if let Some(name) = persisted_name.filter(|name| !name.is_empty()) {
        return name.to_string();
    }
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
            let app_settings = temps_core::AppSettings::from_json(settings_row.data);
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
        } else {
            RemoteServiceClient::new(node.address, token, node.name)
        }
    }

    pub(super) async fn resolve_remote_container_name(
        &self,
        client: &RemoteServiceClient,
        service_instance: &dyn ExternalService,
        parameters: &HashMap<String, serde_json::Value>,
    ) -> Result<String, ExternalServiceError> {
        let persisted_name = parameters
            .get("container_name")
            .and_then(serde_json::Value::as_str)
            .filter(|name| !name.is_empty());
        if let Some(container_name) = persisted_name {
            return Ok(container_name.to_string());
        }

        let canonical_name = service_instance.get_docker_container_name();
        let canonical_exists = client
            .service_status(&canonical_name)
            .await?
            .container_id
            .is_some();

        let legacy_name = service_instance.get_name();
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
            persisted_name,
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
                service_id: 0,
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
        service_name: &str,
        service_type: &ServiceType,
        parameters: &HashMap<String, String>,
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
                    service_id: 0,
                    reason: "managed S3 backend 'minio' is only supported for S3 services; use the default 'rustfs' backend for Blob services".to_string(),
                });
            }
            (ServiceType::Blob, Some(backend)) if backend == "garage" => {
                return Err(ExternalServiceError::ParameterValidationFailed {
                    service_id: 0,
                    reason: "managed S3 backend 'garage' is only supported for S3 services; the built-in Blob service runs on the default 'rustfs' backend".to_string(),
                });
            }
            (ServiceType::S3, Some(backend)) if backend == "garage" => {
                // Garage reads its configuration from a `garage.toml` that has
                // to be written into the container before it starts; the
                // agent's generic create request carries no files, so a worker
                // cannot create one yet.
                return Err(ExternalServiceError::ParameterValidationFailed {
                    service_id: 0,
                    reason: format!(
                        "managed S3 backend 'garage' cannot be created on a remote node for service '{}': the agent cannot yet deliver garage.toml. Create it on the control-plane host, or use the default 'rustfs' backend on the worker",
                        service_name
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
                    ExternalServiceError::ParameterValidationFailed {
                        service_id: 0,
                        reason,
                    }
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
                let cmd = if password.is_empty() {
                    vec!["redis-server".to_string()]
                } else {
                    vec![
                        "redis-server".to_string(),
                        "--requirepass".to_string(),
                        password,
                    ]
                };
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
                let cmd = if password.is_empty() {
                    vec!["redis-server".to_string()]
                } else {
                    vec![
                        "redis-server".to_string(),
                        "--requirepass".to_string(),
                        password,
                    ]
                };
                (image, 6379u16, env, "/data".to_string(), Some(cmd))
            }
            #[allow(deprecated)]
            ServiceType::Minio => {
                let (kind, image) = Self::remote_s3_server_kind_and_image(parameters)?;
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

        let container_name = self
            .create_service_instance(service_name.to_string(), backend_service_type)?
            .get_docker_container_name();
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
                .build_remote_create_params("orders", &service_type, &parameters)
                .expect("default remote service parameters should be valid");
            assert_eq!(params.name, expected_name, "wrong name for {service_type}");
            if let Some(expected_image) = expected_image {
                assert_eq!(params.image, expected_image);
            }
        }
    }

    /// A worker cannot be handed `garage.toml` by the generic agent create
    /// request, so a remote Garage service is refused up front with the
    /// service name rather than started without its config.
    #[test]
    fn remote_garage_creation_is_refused_with_an_actionable_error() {
        let manager = mock_service_manager(vec![]);
        let parameters = HashMap::from([("backend".to_string(), "Garage".to_string())]);
        let err = manager
            .build_remote_create_params("orders", &ServiceType::S3, &parameters)
            .unwrap_err();
        match err {
            ExternalServiceError::ParameterValidationFailed { reason, .. } => {
                assert!(
                    reason.contains("garage") && reason.contains("orders"),
                    "{reason}"
                );
                assert!(reason.contains("control-plane"), "{reason}");
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// The built-in Blob service has no Garage engine, on a worker or not.
    #[test]
    fn remote_blob_creation_rejects_the_garage_backend() {
        let manager = mock_service_manager(vec![]);
        let parameters = HashMap::from([("backend".to_string(), "garage".to_string())]);
        let err = manager
            .build_remote_create_params("assets", &ServiceType::Blob, &parameters)
            .unwrap_err();
        match err {
            ExternalServiceError::ParameterValidationFailed { reason, .. } => {
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
            manager.build_remote_create_params("orders", &ServiceType::Minio, &parameters)
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
    fn remote_container_resolution_supports_persisted_canonical_and_legacy_names() {
        assert_eq!(
            select_remote_container_name(
                Some("persisted-container"),
                "postgres-orders",
                true,
                "orders",
                true,
            ),
            "persisted-container"
        );
        assert_eq!(
            select_remote_container_name(None, "postgres-orders", true, "orders", false),
            "postgres-orders"
        );
        assert_eq!(
            select_remote_container_name(None, "postgres-orders", false, "orders", true),
            "orders"
        );
        assert_eq!(
            select_remote_container_name(None, "postgres-orders", false, "orders", false),
            "postgres-orders"
        );
    }

    // ── repoint_continuous_archive_source ───────────────────────────────────
}
