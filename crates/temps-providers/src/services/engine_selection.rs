// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which engine runs a standalone service, and the names and port it runs
//! under.
//!
//! This is the one place a service's type and stored backend become an
//! engine. Local paths build the engine from an [`EngineSelection`]; remote
//! paths only need its names and port. Building an engine needs a local
//! Docker handle, which a server started with `--profile control-plane` never
//! has, so a service on a worker node is addressed from the selection alone.

use super::ExternalServiceError;
use crate::externalsvc::garage::{garage_container_name, GARAGE_S3_PORT};
use crate::externalsvc::naming::{
    mariadb_container_name, mongodb_container_name, postgres_container_name, redis_container_name,
};
use crate::externalsvc::rustfs::{rustfs_container_name, DEFAULT_RUSTFS_API_PORT};
use crate::externalsvc::s3::minio_container_name;
use crate::externalsvc::{
    managed_instance_name, ManagedS3BackendKind, ManagedS3BackendSelection, ServiceType,
};

/// The engine implementation behind a standalone service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EngineKind {
    Mariadb,
    Mongodb,
    Postgres,
    Redis,
    Rustfs,
    Minio,
    Garage,
}

/// The engine a standalone service runs on and the instance name it runs
/// under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EngineSelection {
    pub(super) kind: EngineKind,
    /// What the engine's `get_name()` returns.
    pub(super) instance_name: String,
}

impl EngineSelection {
    /// The engine a service of `service_type` runs on with its type's default
    /// backend.
    #[allow(
        deprecated,
        reason = "existing MinIO services keep running on the MinIO engine"
    )]
    pub(super) fn for_type(name: &str, service_type: ServiceType) -> Self {
        let (kind, instance_name) = match service_type {
            ServiceType::Mariadb => (EngineKind::Mariadb, name.to_string()),
            ServiceType::Mongodb => (EngineKind::Mongodb, name.to_string()),
            ServiceType::Postgres => (EngineKind::Postgres, name.to_string()),
            ServiceType::Redis => (EngineKind::Redis, name.to_string()),
            // Temps KV and Blob take their instance name from
            // `managed_instance_name` so it agrees with the kv and blob
            // plugins: see the module docs on `externalsvc::naming` and
            // issue #495.
            ServiceType::Kv => (EngineKind::Redis, managed_instance_name(name, service_type)),
            ServiceType::Blob => (
                EngineKind::Rustfs,
                managed_instance_name(name, service_type),
            ),
            // S3 runs on RustFS unless its parameters pick another backend.
            ServiceType::S3 | ServiceType::Rustfs => (EngineKind::Rustfs, name.to_string()),
            ServiceType::Minio => (EngineKind::Minio, name.to_string()),
        };
        Self {
            kind,
            instance_name,
        }
    }

    /// The engine a stored service runs on: like [`Self::for_type`], but an
    /// S3 or Blob service follows the backend in its parameters.
    pub(super) fn for_parameters(
        service_id: i32,
        name: &str,
        service_type: ServiceType,
        parameters: &serde_json::Value,
    ) -> Result<Self, ExternalServiceError> {
        if !matches!(service_type, ServiceType::S3 | ServiceType::Blob) {
            return Ok(Self::for_type(name, service_type));
        }
        let backend = ManagedS3BackendSelection::from_parameters(parameters)
            .map_err(|e| ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: e.to_string(),
            })?
            .backend;
        match backend {
            ManagedS3BackendKind::Rustfs => Ok(Self::for_type(name, service_type)),
            ManagedS3BackendKind::Minio if service_type == ServiceType::S3 => Ok(Self {
                kind: EngineKind::Minio,
                instance_name: name.to_string(),
            }),
            ManagedS3BackendKind::Minio => Err(ExternalServiceError::ParameterValidationFailed {
                service_id,
                reason: "managed S3 backend 'minio' is only supported for S3 services; use the default 'rustfs' backend for Blob services"
                    .to_string(),
            }),
            ManagedS3BackendKind::Garage => Ok(Self {
                kind: EngineKind::Garage,
                instance_name: name.to_string(),
            }),
        }
    }

    /// [`Self::for_parameters`] for parameters held as a map, as they are
    /// once decrypted.
    pub(super) fn for_parameter_map(
        service_id: i32,
        name: &str,
        service_type: ServiceType,
        parameters: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<Self, ExternalServiceError> {
        let parameters = serde_json::Value::Object(
            parameters
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        );
        Self::for_parameters(service_id, name, service_type, &parameters)
    }

    /// What the engine's `get_docker_container_name()` returns.
    pub(super) fn container_name(&self) -> String {
        let name = &self.instance_name;
        match self.kind {
            EngineKind::Mariadb => mariadb_container_name(name),
            EngineKind::Mongodb => mongodb_container_name(name),
            EngineKind::Postgres => postgres_container_name(name),
            EngineKind::Redis => redis_container_name(name),
            EngineKind::Rustfs => rustfs_container_name(name),
            EngineKind::Minio => minio_container_name(name),
            EngineKind::Garage => garage_container_name(name),
        }
    }

    /// What the engine's `get_docker_internal_port()` returns: the port the
    /// service listens on inside its container.
    pub(super) fn internal_port(&self) -> String {
        match self.kind {
            EngineKind::Mariadb => "3306".to_string(),
            EngineKind::Mongodb => "27017".to_string(),
            EngineKind::Postgres => "5432".to_string(),
            EngineKind::Redis => "6379".to_string(),
            EngineKind::Rustfs => DEFAULT_RUSTFS_API_PORT.to_string(),
            EngineKind::Minio => "9000".to_string(),
            EngineKind::Garage => GARAGE_S3_PORT.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::ExternalServiceManager;
    use std::collections::HashMap;
    use std::sync::Arc;

    /// Every `ServiceType`. The match makes adding a variant a compile error
    /// here until it is listed, so the parity test below covers it.
    #[allow(deprecated, reason = "MinIO services still exist")]
    fn every_service_type() -> [ServiceType; 9] {
        let all = [
            ServiceType::Mariadb,
            ServiceType::Mongodb,
            ServiceType::Postgres,
            ServiceType::Redis,
            ServiceType::S3,
            ServiceType::Rustfs,
            ServiceType::Kv,
            ServiceType::Blob,
            ServiceType::Minio,
        ];
        for service_type in all {
            match service_type {
                ServiceType::Mariadb
                | ServiceType::Mongodb
                | ServiceType::Postgres
                | ServiceType::Redis
                | ServiceType::S3
                | ServiceType::Rustfs
                | ServiceType::Kv
                | ServiceType::Blob
                | ServiceType::Minio => {}
            }
        }
        all
    }

    /// A manager whose engines can be built but never reach a daemon: the
    /// HTTP Docker client only connects when a request is made, so this runs
    /// without Docker installed.
    fn manager_with_unreachable_docker() -> ExternalServiceManager {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let docker = bollard::Docker::connect_with_http(
            "http://127.0.0.1:1",
            1,
            bollard::API_DEFAULT_VERSION,
        )
        .expect("an HTTP Docker client is built without connecting");
        ExternalServiceManager::new(
            db.clone(),
            Arc::new(temps_core::EncryptionService::new_from_password(
                "engine-selection-test",
            )),
            Arc::new(docker),
            Arc::new(temps_dns::DnsRegistry::new(db)),
        )
    }

    /// Remote paths name and address containers from a selection, without
    /// building the engine, so the selection must agree with the engine for
    /// every service type, backend spelling and name. A disagreement would
    /// send start, stop or delete to the wrong container on a worker.
    #[test]
    fn selection_matches_the_engine_it_builds() {
        let manager = manager_with_unreachable_docker();
        let backends = [
            None,
            Some(serde_json::Value::Null),
            Some("rustfs".into()),
            Some("RUSTFS".into()),
            Some("minio".into()),
            Some(" minio ".into()),
            Some("garage".into()),
            Some("Garage".into()),
            Some("not-a-backend".into()),
            Some(serde_json::json!(5)),
        ];
        let mut engines_compared = 0;
        for name in ["orders", "temps-blob", "temps-kv", "a.b_C-1"] {
            for service_type in every_service_type() {
                for backend in &backends {
                    let parameters: HashMap<String, serde_json::Value> = backend
                        .clone()
                        .map(|backend| HashMap::from([("backend".to_string(), backend)]))
                        .unwrap_or_default();
                    let case = format!("{service_type} named {name:?} on backend {backend:?}");
                    let engine = manager.create_service_instance_for_parameters(
                        name.to_string(),
                        service_type,
                        &parameters,
                    );
                    let selection =
                        EngineSelection::for_parameter_map(0, name, service_type, &parameters);
                    match (engine, selection) {
                        (Ok(engine), Ok(selection)) => {
                            assert_eq!(selection.instance_name, engine.get_name(), "{case}");
                            assert_eq!(
                                selection.container_name(),
                                engine.get_docker_container_name(),
                                "{case}"
                            );
                            assert_eq!(
                                selection.internal_port(),
                                engine.get_docker_internal_port(),
                                "{case}"
                            );
                            engines_compared += 1;
                        }
                        (Err(engine_error), Err(selection_error)) => {
                            assert_eq!(
                                engine_error.to_string(),
                                selection_error.to_string(),
                                "{case}"
                            );
                        }
                        (engine, selection) => panic!(
                            "{case}: engine built = {}, selection = {selection:?}",
                            engine.is_ok()
                        ),
                    }
                }
            }
        }
        assert!(engines_compared > 0, "no engine was compared");

        for name in ["orders", "temps-blob"] {
            for service_type in every_service_type() {
                let engine = manager
                    .create_service_instance(name.to_string(), service_type)
                    .expect("an engine is built for every service type");
                let selection = EngineSelection::for_type(name, service_type);
                assert_eq!(
                    (selection.instance_name.clone(), selection.container_name()),
                    (engine.get_name(), engine.get_docker_container_name()),
                    "{service_type} named {name:?} on its default backend"
                );
            }
        }
    }

    #[test]
    fn a_rejected_backend_names_the_service() {
        let parameters = serde_json::json!({ "backend": "minio" });
        let error = EngineSelection::for_parameters(42, "assets", ServiceType::Blob, &parameters)
            .expect_err("Blob cannot run on MinIO");
        assert!(
            matches!(
                error,
                ExternalServiceError::ParameterValidationFailed { service_id: 42, .. }
            ),
            "{error:?}"
        );
    }
}
