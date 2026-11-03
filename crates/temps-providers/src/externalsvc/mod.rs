// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

pub use temps_providers_core::externalsvc::*;

use self::error::Result;
use std::collections::HashMap;
use std::sync::Arc;

pub mod garage;
pub mod mariadb;
pub mod mariadb_binlog_health;
pub mod mongodb;
pub mod postgres;
pub mod postgres_cluster;
pub mod postgres_role_reconciler;
pub mod postgres_upgrade;
pub mod postgres_wal_health;
pub mod rc_client;
pub mod redis;
pub mod rustfs;
pub mod s3;
pub(crate) mod s3_mirror;

#[cfg(test)]
mod runtime_provisioning_tests {
    use super::{
        exact_named_container, is_container_not_found, missing_required_probe_credential,
        sanitize_probe_result, set_runtime_container_name, validate_managed_probe_target,
        validate_runtime_target, HealthProbeResult, RuntimeProvisioningError, ServiceConfig,
        ServiceHealthProbeError, ServiceType,
    };

    #[test]
    fn exact_container_filter_does_not_match_a_sibling_or_regex_metacharacters() {
        let filter =
            regex::Regex::new(&super::exact_container_name_filter("/postgres-app.db")).unwrap();
        assert!(filter.is_match("/postgres-app.db"));
        assert!(!filter.is_match("/postgres-app.db-sibling"));
        assert!(!filter.is_match("/postgres-appXdb"));
    }

    #[test]
    fn docker_substring_name_match_is_not_treated_as_the_requested_container() {
        let sibling = bollard::models::ContainerSummary {
            id: Some("sibling-id".to_string()),
            names: Some(vec!["/redis-cache-replica".to_string()]),
            ..Default::default()
        };
        let exact = bollard::models::ContainerSummary {
            id: Some("exact-id".to_string()),
            names: Some(vec!["/redis-cache".to_string()]),
            ..Default::default()
        };

        assert!(exact_named_container(std::slice::from_ref(&sibling), "redis-cache").is_none());
        assert_eq!(
            exact_named_container(&[sibling, exact], "redis-cache")
                .and_then(|container| container.id.as_deref()),
            Some("exact-id")
        );
    }

    #[test]
    fn only_docker_404_means_container_absent() {
        let not_found = bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            message: "No such container".to_string(),
        };
        let daemon_failure = bollard::errors::Error::DockerResponseServerError {
            status_code: 500,
            message: "daemon unavailable".to_string(),
        };

        assert!(is_container_not_found(&not_found));
        assert!(!is_container_not_found(&daemon_failure));
        assert!(!is_container_not_found(
            &bollard::errors::Error::RequestTimeoutError
        ));
    }

    #[test]
    fn health_probe_rejects_non_loopback_managed_target() {
        let config = ServiceConfig {
            name: "orders-db".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({"host": "169.254.169.254", "port": "5432"}),
        };

        assert!(matches!(
            validate_managed_probe_target(&config),
            Err(ServiceHealthProbeError::UnsafeTarget { .. })
        ));
    }

    #[test]
    fn runtime_provisioning_rejects_non_loopback_managed_target() {
        let config = ServiceConfig {
            name: "orders-db".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({"host": "169.254.169.254", "port": "5432"}),
        };

        assert!(matches!(
            validate_runtime_target(&config),
            Err(RuntimeProvisioningError::UnsafeTarget { .. })
        ));
    }

    #[test]
    fn adopted_container_name_is_injected_before_provider_initialization() {
        let mut config = ServiceConfig {
            name: "orders-db".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({"host": "localhost", "port": "5432"}),
        };

        set_runtime_container_name(&mut config, "postgres-orders-db")
            .expect("object parameters should accept the canonical container name");

        assert_eq!(
            config.parameters["container_name"],
            serde_json::Value::String("postgres-orders-db".to_string())
        );
    }

    #[test]
    fn health_probe_redacts_secrets_and_bounds_errors() {
        let secret = "credential-that-must-not-leave-the-agent";
        let mut result =
            HealthProbeResult::down(format!("auth failed for {secret} {}", "x".repeat(3_000)));

        sanitize_probe_result(&mut result, &[secret.to_string()]);

        let message = result.error_message.as_deref().unwrap_or_default();
        assert!(!message.contains(secret));
        assert!(message.contains("[REDACTED]"));
        assert!(message.len() <= 2_051);
    }

    #[test]
    fn existing_service_probes_never_generate_missing_credentials() {
        let postgres = ServiceConfig {
            name: "orders-db".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({"host": "localhost", "port": "5432"}),
        };
        let mariadb = ServiceConfig {
            name: "orders-maria".to_string(),
            service_type: ServiceType::Mariadb,
            version: None,
            parameters: serde_json::json!({"host": "localhost", "port": "3306"}),
        };
        let redis = ServiceConfig {
            name: "orders-cache".to_string(),
            service_type: ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({"host": "localhost", "port": "6379"}),
        };

        assert_eq!(
            missing_required_probe_credential(&postgres),
            Some("password")
        );
        assert_eq!(
            missing_required_probe_credential(&mariadb),
            Some("root_password")
        );
        assert_eq!(missing_required_probe_credential(&redis), None);
    }
}

#[cfg(test)]
mod engine_naming_tests {
    use super::{mariadb, mongodb, postgres, redis, rustfs, s3};

    /// Pins the Docker names each engine derives from a service name. A
    /// restore into a new service checks these for leftovers before it
    /// provisions, and cancelling removes them: a silent rename here would
    /// make that check miss real data and the teardown remove the wrong
    /// objects.
    #[test]
    fn engine_docker_resource_names_are_stable() {
        assert_eq!(postgres::postgres_container_name("copy"), "postgres-copy");
        assert_eq!(
            postgres::postgres_volume_names("copy"),
            ["postgres-copy_data"]
        );
        assert_eq!(mariadb::mariadb_container_name("copy"), "mariadb-copy");
        assert_eq!(mariadb::mariadb_volume_names("copy"), ["mariadb_data_copy"]);
        assert_eq!(
            mongodb::mongodb_container_name("copy"),
            "temps-mongodb-copy"
        );
        assert_eq!(
            mongodb::mongodb_volume_names("copy"),
            ["temps-mongodb-copy-data"]
        );
        assert_eq!(s3::minio_container_name("copy"), "minio-copy");
        assert_eq!(s3::minio_volume_names("copy"), ["minio_copy_data"]);
        assert_eq!(redis::redis_container_name("copy"), "redis-copy");
        assert_eq!(redis::redis_volume_names("copy"), ["redis_data_copy"]);
        assert_eq!(rustfs::rustfs_container_name("copy"), "rustfs-copy");
        assert_eq!(
            rustfs::rustfs_volume_names("copy"),
            ["rustfs_copy_data", "rustfs_copy_logs"]
        );
    }
}

// Integration tests for service clusters
#[cfg(test)]
mod cluster_integration_tests;

// Re-export services for easier access
pub use garage::GarageService;
pub use mariadb::{BinlogManifest, MariaDbService};
pub use mongodb::MongodbService;
pub use postgres::PostgresService;
pub use postgres_cluster::PostgresClusterService;
pub use redis::RedisService;
pub use rustfs::RustfsService;
pub use s3::S3Service;

#[derive(Debug, thiserror::Error)]
pub enum RuntimeProvisioningError {
    #[error(
        "External-service runtime provisioning may only target the owning node loopback, got '{host}'"
    )]
    UnsafeTarget { host: String },

    #[error("External-service runtime parameters must be a JSON object")]
    InvalidParameters,

    #[error("Failed to inspect external-service container '{container}': {source}")]
    ContainerInspection {
        container: String,
        #[source]
        source: bollard::errors::Error,
    },

    #[error("Failed to rename legacy container '{legacy}' to '{canonical}': {source}")]
    LegacyContainerRename {
        legacy: String,
        canonical: String,
        #[source]
        source: bollard::errors::Error,
    },

    #[error("External-service provider provisioning failed: {0}")]
    Provider(#[source] EngineError),

    #[error("External-service provider provisioning failed: {0}")]
    Encryption(#[source] temps_core::EncryptionError),
}

#[derive(Debug, thiserror::Error)]
pub enum ServiceHealthProbeError {
    #[error("External-service health probe configuration is invalid: {0}")]
    Configuration(#[source] EngineError),

    #[error("External-service provider health probe failed: {0}")]
    Provider(#[source] EngineError),

    #[error(
        "External-service health probes may only target the owning node loopback, got '{host}'"
    )]
    UnsafeTarget { host: String },
}

impl OwnedContainerTargetError {
    fn into_engine_error(self, service: &str) -> EngineError {
        match self {
            Self::ContainerNotFound => EngineError::not_found(service, self.to_string()),
            Self::InvalidPort | Self::Inspection { .. } | Self::PortMismatch { .. } => {
                EngineError::failed(service, self.to_string())
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum OwnedContainerTargetError {
    #[error("managed service port is missing or is not a valid TCP port")]
    InvalidPort,
    #[error("managed service container was not found on the owning node")]
    ContainerNotFound,
    #[error("failed to inspect managed service container '{container}': {source}")]
    Inspection {
        container: String,
        #[source]
        source: bollard::errors::Error,
    },
    #[error(
        "stored port {stored_port} does not match container '{container}' published port for {internal_port}/tcp"
    )]
    PortMismatch {
        container: String,
        internal_port: String,
        stored_port: u16,
    },
}

/// Run a provider-authenticated health probe against a service owned by the
/// Docker daemon supplied by the caller.
///
/// This is deliberately a typed provider operation rather than a generic
/// host/port/command probe. The agent derives all network activity from the
/// validated provider configuration, which prevents this endpoint from
/// becoming an arbitrary TCP/HTTP/exec primitive.
#[allow(deprecated)]
pub async fn probe_service_health(
    config: ServiceConfig,
    docker: Arc<bollard::Docker>,
) -> std::result::Result<HealthProbeResult, ServiceHealthProbeError> {
    validate_managed_probe_target(&config)?;
    let secret_values = probe_secret_values(&config);
    if let Some(field) = missing_required_probe_credential(&config) {
        return Ok(HealthProbeResult::down(format!(
            "stored {} configuration is missing required credential field '{}'",
            config.service_type, field
        )));
    }
    let service_type = config.service_type;
    let name = match service_type {
        ServiceType::Kv | ServiceType::Blob => managed_instance_name(&config.name, service_type),
        _ => config.name.clone(),
    };

    let instance: Box<dyn ExternalService> = match service_type {
        ServiceType::Mariadb => Box::new(MariaDbService::new(name, docker.clone())),
        ServiceType::Mongodb => Box::new(MongodbService::new(name, docker.clone())),
        ServiceType::Postgres => Box::new(PostgresService::new(name, docker.clone())),
        ServiceType::Redis | ServiceType::Kv => Box::new(RedisService::new(name, docker.clone())),
        ServiceType::S3 | ServiceType::Blob => {
            let selection = ManagedS3BackendSelection::from_parameters(&config.parameters)
                .map_err(|e| ServiceHealthProbeError::Configuration(e.into()))?;
            let encryption = request_scoped_encryption_service()
                .map_err(ServiceHealthProbeError::Configuration)?;
            match selection.backend {
                ManagedS3BackendKind::Rustfs => {
                    Box::new(RustfsService::new(name, docker.clone(), encryption))
                }
                ManagedS3BackendKind::Minio if service_type == ServiceType::S3 => {
                    Box::new(S3Service::new(name, docker.clone(), encryption))
                }
                ManagedS3BackendKind::Minio => {
                    return Err(ServiceHealthProbeError::Configuration(
                        EngineError::invalid_value(
                            "service",
                            "managed S3 backend 'minio' is only supported for S3 services",
                        ),
                    ));
                }
                ManagedS3BackendKind::Garage => Box::new(GarageService::new(
                    name,
                    docker.clone(),
                    encryption,
                    service_type,
                )),
            }
        }
        ServiceType::Rustfs => Box::new(RustfsService::new(
            name,
            docker.clone(),
            request_scoped_encryption_service().map_err(ServiceHealthProbeError::Configuration)?,
        )),
        ServiceType::Minio => Box::new(S3Service::new(
            name,
            docker.clone(),
            request_scoped_encryption_service().map_err(ServiceHealthProbeError::Configuration)?,
        )),
    };

    validate_owned_container_port(&config, instance.as_ref(), &docker)
        .await
        .map_err(|error| {
            ServiceHealthProbeError::Configuration(error.into_engine_error(&config.name))
        })?;

    let mut result = instance
        .health_probe(config)
        .await
        .map_err(ServiceHealthProbeError::Provider)?;
    sanitize_probe_result(&mut result, &secret_values);
    Ok(result)
}

/// Creation-time config parsers may generate credentials for a new service.
/// A health probe must never do that: generated credentials cannot match an
/// already-running container. Redis is the exception because no password is
/// a valid, intentionally unauthenticated configuration.
fn missing_required_probe_credential(config: &ServiceConfig) -> Option<&'static str> {
    let present = |key: &str| {
        config
            .parameters
            .get(key)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.is_empty())
    };
    match config.service_type {
        ServiceType::Postgres | ServiceType::Mongodb if !present("password") => Some("password"),
        ServiceType::Mariadb if !present("root_password") => Some("root_password"),
        _ => None,
    }
}

fn validate_managed_probe_target(
    config: &ServiceConfig,
) -> std::result::Result<(), ServiceHealthProbeError> {
    let Some(host) = config
        .parameters
        .get("host")
        .and_then(|value| value.as_str())
    else {
        return Ok(());
    };
    if matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]") {
        return Ok(());
    }
    Err(ServiceHealthProbeError::UnsafeTarget {
        host: host.to_string(),
    })
}

fn probe_secret_values(config: &ServiceConfig) -> Vec<String> {
    let Some(parameters) = config.parameters.as_object() else {
        return Vec::new();
    };
    parameters
        .iter()
        .filter(|(key, _)| {
            let key = key.to_ascii_lowercase();
            key.contains("password")
                || key.contains("secret")
                || key.contains("token")
                || key.contains("access_key")
        })
        .filter_map(|(_, value)| value.as_str().map(str::to_string))
        .filter(|value| !value.is_empty())
        .collect()
}

fn sanitize_probe_result(result: &mut HealthProbeResult, secret_values: &[String]) {
    const MAX_ERROR_BYTES: usize = 2_048;
    let Some(message) = result.error_message.as_mut() else {
        return;
    };
    for secret in secret_values {
        *message = message.replace(secret, "[REDACTED]");
    }
    if message.len() > MAX_ERROR_BYTES {
        let mut boundary = MAX_ERROR_BYTES;
        while !message.is_char_boundary(boundary) {
            boundary -= 1;
        }
        message.truncate(boundary);
        message.push('…');
    }
}

fn request_scoped_encryption_service() -> Result<Arc<temps_core::EncryptionService>> {
    let key = temps_core::EncryptionService::generate_raw_key()?;
    Ok(Arc::new(temps_core::EncryptionService::new(&key)?))
}

fn is_container_not_found(error: &bollard::errors::Error) -> bool {
    matches!(
        error,
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}

async fn runtime_container_exists(
    docker: &bollard::Docker,
    container: &str,
) -> std::result::Result<bool, RuntimeProvisioningError> {
    match docker
        .inspect_container(
            container,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
    {
        Ok(_) => Ok(true),
        Err(error) if is_container_not_found(&error) => Ok(false),
        Err(source) => Err(RuntimeProvisioningError::ContainerInspection {
            container: container.to_string(),
            source,
        }),
    }
}

/// Provision one project's logical resource on the worker that owns the
/// service container.
///
/// Older control planes created remote containers using the raw service name,
/// while providers use a type-prefixed canonical name. Before initializing the
/// provider, adopt that legacy container by renaming it in place. Docker keeps
/// the container ID, volumes, network attachments, and published ports intact.
/// This avoids creating a second container during the first deployment after
/// upgrading while making subsequent provider operations use one canonical
/// identity.
#[allow(deprecated)]
pub async fn provision_runtime_environment(
    name: String,
    service_type: ServiceType,
    mut config: ServiceConfig,
    project_slug: &str,
    environment_slug: &str,
    docker: Arc<bollard::Docker>,
) -> std::result::Result<HashMap<String, String>, RuntimeProvisioningError> {
    validate_runtime_target(&config)?;
    let parameters = &config.parameters;
    let instance_name = match service_type {
        ServiceType::Kv | ServiceType::Blob => managed_instance_name(&name, service_type),
        _ => name.clone(),
    };

    let instance: Box<dyn ExternalService> = match service_type {
        ServiceType::Mariadb => Box::new(MariaDbService::new(instance_name, docker.clone())),
        ServiceType::Mongodb => Box::new(MongodbService::new(instance_name, docker.clone())),
        ServiceType::Postgres => Box::new(PostgresService::new(instance_name, docker.clone())),
        ServiceType::Redis | ServiceType::Kv => {
            Box::new(RedisService::new(instance_name, docker.clone()))
        }
        ServiceType::S3 | ServiceType::Blob => {
            let selection = ManagedS3BackendSelection::from_parameters(parameters)
                .map_err(|e| RuntimeProvisioningError::Provider(e.into()))?;

            // Runtime provisioning does not use the backup encryption APIs,
            // but these providers require the dependency at construction.
            // Use a fresh request-local key so this narrow function cannot
            // expose a provider carrying a fixed or reusable key.
            let encryption = Arc::new(
                temps_core::EncryptionService::new(
                    &temps_core::EncryptionService::generate_raw_key()
                        .map_err(RuntimeProvisioningError::Encryption)?,
                )
                .map_err(RuntimeProvisioningError::Encryption)?,
            );
            match selection.backend {
                ManagedS3BackendKind::Rustfs => Box::new(RustfsService::new(
                    instance_name,
                    docker.clone(),
                    encryption,
                )),
                ManagedS3BackendKind::Minio if service_type == ServiceType::S3 => {
                    Box::new(S3Service::new(instance_name, docker.clone(), encryption))
                }
                ManagedS3BackendKind::Minio => {
                    return Err(RuntimeProvisioningError::Provider(
                        EngineError::invalid_value(
                            "service",
                            "managed S3 backend 'minio' is only supported for S3 services",
                        ),
                    ));
                }
                ManagedS3BackendKind::Garage => Box::new(GarageService::new(
                    instance_name,
                    docker.clone(),
                    encryption,
                    service_type,
                )),
            }
        }
        ServiceType::Rustfs => {
            let encryption = Arc::new(
                temps_core::EncryptionService::new(
                    &temps_core::EncryptionService::generate_raw_key()
                        .map_err(RuntimeProvisioningError::Encryption)?,
                )
                .map_err(RuntimeProvisioningError::Encryption)?,
            );
            Box::new(RustfsService::new(
                instance_name,
                docker.clone(),
                encryption,
            ))
        }
        ServiceType::Minio => {
            let encryption = Arc::new(
                temps_core::EncryptionService::new(
                    &temps_core::EncryptionService::generate_raw_key()
                        .map_err(RuntimeProvisioningError::Encryption)?,
                )
                .map_err(RuntimeProvisioningError::Encryption)?,
            );
            Box::new(S3Service::new(instance_name, docker.clone(), encryption))
        }
    };

    let legacy_name = instance.get_name();
    let canonical_name = instance.get_docker_container_name();
    let canonical_exists = runtime_container_exists(&docker, &canonical_name).await?;
    let legacy_exists = if legacy_name == canonical_name {
        false
    } else {
        runtime_container_exists(&docker, &legacy_name).await?
    };
    if !canonical_exists && legacy_exists {
        tracing::info!(
            legacy_container = %legacy_name,
            canonical_container = %canonical_name,
            "Adopting legacy remote external-service container name"
        );
        docker
            .rename_container(
                &legacy_name,
                bollard::query_parameters::RenameContainerOptions {
                    name: canonical_name.clone(),
                },
            )
            .await
            .map_err(|source| RuntimeProvisioningError::LegacyContainerRename {
                legacy: legacy_name,
                canonical: canonical_name.clone(),
                source,
            })?;
    }

    if canonical_exists || legacy_exists {
        set_runtime_container_name(&mut config, &canonical_name)?;
    }

    validate_owned_container_port(&config, instance.as_ref(), &docker)
        .await
        .map_err(|error| {
            RuntimeProvisioningError::Provider(error.into_engine_error(&config.name))
        })?;

    instance
        .init(config.clone())
        .await
        .map_err(RuntimeProvisioningError::Provider)?;
    instance
        .get_runtime_env_vars(config, project_slug, environment_slug)
        .await
        .map_err(RuntimeProvisioningError::Provider)
}

/// Revoke the credentials [`provision_runtime_environment`] minted for one
/// project environment, on the Docker daemon supplied by the caller.
///
/// The control plane cannot reach a worker's storage engine directly, so a
/// remote service's unlink asks the owning agent to run this. Only managed
/// S3 engines (RustFS, Garage) have per-environment credentials; any other
/// service type is rejected rather than silently reported as revoked.
#[allow(deprecated)]
pub async fn revoke_runtime_environment(
    name: String,
    service_type: ServiceType,
    config: ServiceConfig,
    project_slug: &str,
    environment_slug: &str,
    docker: Arc<bollard::Docker>,
) -> std::result::Result<(), RuntimeProvisioningError> {
    validate_runtime_target(&config)?;
    let unsupported = |reason: &str| {
        RuntimeProvisioningError::Provider(EngineError::engine_failure(
            "service",
            format!(
                "service '{}' ({}) has no revocable managed S3 credentials: {}",
                name, service_type, reason
            ),
        ))
    };
    let encryption = Arc::new(
        temps_core::EncryptionService::new(
            &temps_core::EncryptionService::generate_raw_key()
                .map_err(RuntimeProvisioningError::Encryption)?,
        )
        .map_err(RuntimeProvisioningError::Encryption)?,
    );
    let backend: Box<dyn ManagedS3Backend> = match service_type {
        ServiceType::S3 | ServiceType::Blob => {
            let selection = ManagedS3BackendSelection::from_parameters(&config.parameters)
                .map_err(|e| RuntimeProvisioningError::Provider(e.into()))?;
            match selection.backend {
                ManagedS3BackendKind::Rustfs => {
                    Box::new(RustfsService::new(name.clone(), docker, encryption))
                }
                ManagedS3BackendKind::Garage => Box::new(GarageService::new(
                    name.clone(),
                    docker,
                    encryption,
                    service_type,
                )),
                ManagedS3BackendKind::Minio => {
                    return Err(unsupported("legacy MinIO services use one shared key"));
                }
            }
        }
        ServiceType::Rustfs => Box::new(RustfsService::new(name.clone(), docker, encryption)),
        ServiceType::Mariadb
        | ServiceType::Mongodb
        | ServiceType::Postgres
        | ServiceType::Redis
        | ServiceType::Kv
        | ServiceType::Minio => {
            return Err(unsupported("the service is not managed S3 storage"));
        }
    };
    backend
        .revoke_access_key(&config, project_slug, environment_slug)
        .await
        .map_err(|e| RuntimeProvisioningError::Provider(e.into()))
}

fn set_runtime_container_name(
    config: &mut ServiceConfig,
    container_name: &str,
) -> std::result::Result<(), RuntimeProvisioningError> {
    let parameters = config
        .parameters
        .as_object_mut()
        .ok_or(RuntimeProvisioningError::InvalidParameters)?;
    parameters.insert(
        "container_name".to_string(),
        serde_json::Value::String(container_name.to_string()),
    );
    Ok(())
}

fn validate_runtime_target(
    config: &ServiceConfig,
) -> std::result::Result<(), RuntimeProvisioningError> {
    let Some(host) = config
        .parameters
        .get("host")
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(());
    };
    if matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]") {
        return Ok(());
    }
    Err(RuntimeProvisioningError::UnsafeTarget {
        host: host.to_string(),
    })
}

async fn validate_owned_container_port(
    config: &ServiceConfig,
    instance: &dyn ExternalService,
    docker: &bollard::Docker,
) -> std::result::Result<(), OwnedContainerTargetError> {
    let stored_port = config
        .parameters
        .get("port")
        .and_then(|value| {
            value
                .as_u64()
                .and_then(|port| u16::try_from(port).ok())
                .or_else(|| value.as_str().and_then(|port| port.parse::<u16>().ok()))
        })
        .filter(|port| *port > 0)
        .ok_or(OwnedContainerTargetError::InvalidPort)?;

    let mut candidates = Vec::new();
    if let Some(container_name) = config
        .parameters
        .get("container_name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())
    {
        candidates.push(container_name.to_string());
    }
    for candidate in [instance.get_docker_container_name(), instance.get_name()] {
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }

    let mut inspected = None;
    for candidate in candidates {
        match docker
            .inspect_container(
                &candidate,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(container) => {
                inspected = Some((candidate, container));
                break;
            }
            Err(error) if is_container_not_found(&error) => {}
            Err(source) => {
                return Err(OwnedContainerTargetError::Inspection {
                    container: candidate,
                    source,
                })
            }
        }
    }
    let (container_name, container) =
        inspected.ok_or(OwnedContainerTargetError::ContainerNotFound)?;
    let internal_port = instance.get_docker_internal_port();
    let key = format!("{internal_port}/tcp");
    let matches_published_port = container
        .network_settings
        .and_then(|settings| settings.ports)
        .and_then(|ports| ports.get(&key).cloned())
        .flatten()
        .unwrap_or_default()
        .iter()
        .filter_map(|binding| binding.host_port.as_deref())
        .filter_map(|port| port.parse::<u16>().ok())
        .any(|port| port == stored_port);
    if !matches_published_port {
        return Err(OwnedContainerTargetError::PortMismatch {
            container: container_name,
            internal_port,
            stored_port,
        });
    }
    Ok(())
}

#[cfg(test)]
mod revoke_runtime_environment_tests {
    use super::*;

    /// A client that is never asked to talk to a daemon: every case below
    /// fails before the first Docker call.
    fn unused_docker() -> Arc<bollard::Docker> {
        Arc::new(
            bollard::Docker::connect_with_http(
                "http://127.0.0.1:1",
                1,
                bollard::API_DEFAULT_VERSION,
            )
            .expect("a lazily connecting client"),
        )
    }

    fn config(service_type: ServiceType, parameters: serde_json::Value) -> ServiceConfig {
        ServiceConfig {
            name: "media".to_string(),
            service_type,
            version: None,
            parameters,
        }
    }

    #[tokio::test]
    async fn only_managed_s3_services_have_credentials_to_revoke() {
        let error = revoke_runtime_environment(
            "media".to_string(),
            ServiceType::Postgres,
            config(ServiceType::Postgres, serde_json::json!({})),
            "shop",
            "prod",
            unused_docker(),
        )
        .await
        .expect_err("a database has no managed S3 credentials");

        assert!(
            error.to_string().contains("not managed S3 storage"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn legacy_minio_shares_one_key_and_cannot_be_revoked_per_environment() {
        let error = revoke_runtime_environment(
            "media".to_string(),
            ServiceType::S3,
            config(ServiceType::S3, serde_json::json!({ "backend": "minio" })),
            "shop",
            "prod",
            unused_docker(),
        )
        .await
        .expect_err("legacy MinIO has no per-environment keys");

        assert!(error.to_string().contains("legacy MinIO"), "{error}");
    }

    #[tokio::test]
    async fn revocation_refuses_a_target_that_is_not_the_node_loopback() {
        let error = revoke_runtime_environment(
            "media".to_string(),
            ServiceType::S3,
            config(
                ServiceType::S3,
                serde_json::json!({ "backend": "garage", "host": "10.0.0.5" }),
            ),
            "shop",
            "prod",
            unused_docker(),
        )
        .await
        .expect_err("only the owning node's loopback is a valid target");

        assert!(matches!(
            error,
            RuntimeProvisioningError::UnsafeTarget { .. }
        ));
    }
}
