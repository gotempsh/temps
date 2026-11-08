// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a stored service is called and the connection variables it gives
//! out, without a Docker daemon.
//!
//! The engines own their naming and environment conventions, but an engine
//! is built around a Docker client. A server started with
//! `--profile control-plane` has none, and a service on a worker node is not
//! on this host's daemon anyway, so neither should need one to answer "what
//! is the container called" or "which variables does a linked app get".
//!
//! [`ServiceMetadata`] builds the engine and exposes only its pure methods:
//! naming and the synchronous environment-variable builders. Nothing it
//! exposes can reach a daemon, so it may hold an inert client when this
//! process has no Docker. Everything that does reach a daemon keeps going
//! through [`ExternalServiceManager::create_service_instance_for_parameters`]
//! and its typed [`ExternalServiceError::DockerUnavailable`].

use super::engine_selection::EngineSelection;
use super::{ExternalServiceError, ExternalServiceManager};
use crate::externalsvc::error::Result as EngineResult;
use crate::externalsvc::{ExternalService, ServiceType};
use bollard::Docker;
use std::collections::HashMap;
use std::sync::Arc;

/// The naming and environment-variable conventions of one stored service's
/// engine. Holds the engine privately so no daemon operation is reachable.
pub(crate) struct ServiceMetadata {
    engine: Box<dyn ExternalService>,
    /// The container name recorded in the service's parameters (an imported
    /// container, or the name a worker's agent reported at create time).
    /// Wins over the engine's derived name.
    container_name_override: Option<String>,
}

impl ServiceMetadata {
    /// The service's container: the recorded name, else the engine's own.
    pub(crate) fn container_name(&self) -> String {
        self.container_name_override
            .clone()
            .unwrap_or_else(|| self.engine.get_docker_container_name())
    }

    /// The engine's connection variables for `parameters`.
    pub(crate) fn environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> EngineResult<HashMap<String, String>> {
        self.engine.get_environment_variables(parameters)
    }

    /// The names of the variables the engine gives each linked environment
    /// on top of its connection variables (its runtime definitions).
    pub(crate) fn runtime_variable_names(&self) -> Vec<String> {
        self.engine
            .get_runtime_env_definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect()
    }

    /// The engine's container-to-container connection variables for
    /// `parameters`.
    pub(crate) fn docker_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> EngineResult<HashMap<String, String>> {
        self.engine.get_docker_environment_variables(parameters)
    }
}

/// A Docker client that is never used: engines take a client at
/// construction, and [`ServiceMetadata`] exposes none of the methods that
/// would use it. Building an HTTP client opens no connection. Should a
/// future method leak through, it fails against a closed local port rather
/// than reaching anything.
fn inert_docker_client(service_id: i32) -> Result<Arc<Docker>, ExternalServiceError> {
    Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION)
        .map(Arc::new)
        .map_err(|error| ExternalServiceError::InternalError {
            reason: format!(
                "Failed to prepare the naming conventions of service {service_id}: {error}"
            ),
        })
}

impl ExternalServiceManager {
    /// The naming and environment-variable conventions of a stored service,
    /// following the S3/Blob backend in its decrypted `parameters`. Needs no
    /// Docker daemon: works on a control-plane-profile server and for a
    /// service on a worker node.
    pub(crate) fn service_metadata(
        &self,
        service_id: i32,
        name: &str,
        service_type: ServiceType,
        parameters: &HashMap<String, serde_json::Value>,
    ) -> Result<ServiceMetadata, ExternalServiceError> {
        let selection =
            EngineSelection::for_parameter_map(service_id, name, service_type, parameters)?;
        let docker = match self.docker.get() {
            Some(docker) => docker.clone(),
            None => inert_docker_client(service_id)?,
        };
        let container_name_override = parameters
            .get("container_name")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        Ok(ServiceMetadata {
            engine: self.engine_with(selection, service_type, docker),
            container_name_override,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::test_support::{control_plane_manager, unreachable_docker_manager};

    fn postgres_parameters() -> HashMap<String, serde_json::Value> {
        HashMap::from([
            ("username".to_string(), serde_json::json!("app")),
            ("password".to_string(), serde_json::json!("s3cret")),
            ("database".to_string(), serde_json::json!("orders")),
            ("port".to_string(), serde_json::json!("5561")),
            ("host".to_string(), serde_json::json!("localhost")),
        ])
    }

    fn strings(parameters: &HashMap<String, serde_json::Value>) -> HashMap<String, String> {
        ExternalServiceManager::params_to_strings(parameters)
    }

    /// The live failure: a control-plane-profile server could not name a
    /// worker's container or list its variables, because both built an
    /// engine through `require_docker`.
    #[test]
    fn a_control_plane_server_names_a_service_and_lists_its_variables() {
        let manager = control_plane_manager();
        let parameters = postgres_parameters();

        let metadata = manager
            .service_metadata(7, "orders", ServiceType::Postgres, &parameters)
            .expect("metadata needs no Docker");
        let variables = metadata
            .environment_variables(&strings(&parameters))
            .expect("variables are computed from the parameters");

        assert_eq!(metadata.container_name(), "postgres-orders");
        assert_eq!(variables["POSTGRES_HOST"], "postgres-orders");
        assert!(variables.contains_key("POSTGRES_URL"), "{variables:?}");
    }

    /// The conventions do not depend on whether this process has Docker:
    /// a full-profile server answers exactly what a control-plane one does.
    #[allow(deprecated, reason = "MinIO services still exist")]
    #[test]
    fn metadata_agrees_with_and_without_a_local_daemon() {
        let without = control_plane_manager();
        let with = unreachable_docker_manager();
        let mut parameters = postgres_parameters();
        parameters.insert("access_key".to_string(), serde_json::json!("ak"));
        parameters.insert("secret_key".to_string(), serde_json::json!("sk"));
        for service_type in [
            ServiceType::Postgres,
            ServiceType::Mariadb,
            ServiceType::Mongodb,
            ServiceType::Redis,
            ServiceType::S3,
            ServiceType::Kv,
            ServiceType::Blob,
            ServiceType::Minio,
        ] {
            let a = without
                .service_metadata(1, "orders", service_type, &parameters)
                .expect("control plane");
            let b = with
                .service_metadata(1, "orders", service_type, &parameters)
                .expect("full profile");
            assert_eq!(a.container_name(), b.container_name(), "{service_type}");
            assert_eq!(
                a.environment_variables(&strings(&parameters)).ok(),
                b.environment_variables(&strings(&parameters)).ok(),
                "{service_type}"
            );
            assert_eq!(
                a.docker_environment_variables(&strings(&parameters)).ok(),
                b.docker_environment_variables(&strings(&parameters)).ok(),
                "{service_type}"
            );
        }
    }

    /// The container name a worker's agent reported, or an imported
    /// container's real name, wins over the derived one.
    #[test]
    fn a_recorded_container_name_wins() {
        let manager = control_plane_manager();
        let mut parameters = postgres_parameters();
        parameters.insert(
            "container_name".to_string(),
            serde_json::json!("legacy-orders-db"),
        );

        let metadata = manager
            .service_metadata(7, "orders", ServiceType::Postgres, &parameters)
            .expect("metadata");

        assert_eq!(metadata.container_name(), "legacy-orders-db");
        let variables = metadata
            .environment_variables(&strings(&parameters))
            .expect("variables");
        assert_eq!(variables["POSTGRES_HOST"], "legacy-orders-db");
    }

    /// A backend the service cannot run on is still refused under the
    /// service's own ID, as it is when building the engine.
    #[test]
    fn an_invalid_backend_is_refused_with_the_service_id() {
        let manager = control_plane_manager();
        let parameters = HashMap::from([("backend".to_string(), serde_json::json!("minio"))]);

        let error = manager
            .service_metadata(42, "assets", ServiceType::Blob, &parameters)
            .err()
            .expect("Blob cannot run on MinIO");

        assert!(
            matches!(
                error,
                ExternalServiceError::ParameterValidationFailed { service_id: 42, .. }
            ),
            "{error:?}"
        );
    }

    fn worker_service_row() -> temps_entities::external_services::Model {
        let mut service = crate::services::test_support::encrypted_service_model(
            11,
            serde_json::json!({
                "host": "localhost",
                "port": "5561",
                "username": "app",
                "password": "s3cret",
                "database": "orders",
            }),
        );
        service.name = "orders".to_string();
        service.node_id = Some(3);
        service
    }

    fn control_plane_answering(
        rows: usize,
    ) -> (
        ExternalServiceManager,
        temps_entities::external_services::Model,
    ) {
        let service = worker_service_row();
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results((0..rows).map(|_| vec![service.clone()]))
                .into_connection(),
        );
        (
            crate::services::test_support::control_plane_manager_with_db(db),
            service,
        )
    }

    /// The live failure: the variables panel of a worker's database answered
    /// 409 `WORKER_NODE_REQUIRED` for the masked and the names-only preview.
    #[tokio::test]
    async fn the_variable_previews_of_a_worker_service_need_no_docker() {
        use crate::services::EnvironmentVariableOptions;

        let (manager, _) = control_plane_answering(2);
        let masked = manager
            .get_environment_variables(
                11,
                None,
                None,
                EnvironmentVariableOptions {
                    mask_sensitive: true,
                    ..Default::default()
                },
            )
            .await
            .expect("masked preview needs no Docker");
        assert!(masked.masked);
        assert_eq!(
            masked.variables.get("POSTGRES_HOST"),
            Some(&"***".to_string())
        );
        assert!(!masked
            .variables
            .values()
            .any(|value| value.contains("s3cret")));

        let (manager, _) = control_plane_answering(2);
        let names = manager
            .get_environment_variables(
                11,
                None,
                None,
                EnvironmentVariableOptions {
                    names_only: true,
                    ..Default::default()
                },
            )
            .await
            .expect("names preview needs no Docker");
        assert!(names.variables.contains_key("POSTGRES_URL"), "{names:?}");

        let (manager, _) = control_plane_answering(2);
        let names = manager
            .get_service_preview_environment_variable_names(11)
            .await
            .expect("names need no Docker");
        assert!(
            names.contains(&"POSTGRES_PASSWORD".to_string()),
            "{names:?}"
        );

        let (manager, _) = control_plane_answering(2);
        let masked = manager
            .get_service_preview_environment_variables_masked(11)
            .await
            .expect("masked values need no Docker");
        assert!(masked.values().all(|value| value == "***"), "{masked:?}");
    }

    /// The variable-name preview lists what a linked app receives, including
    /// the per-environment names: a storage service's `S3_BUCKET` and a
    /// database's per-environment database. On a worker node and on the
    /// control plane alike, with no Docker.
    #[tokio::test]
    async fn the_name_preview_lists_the_per_environment_variables() {
        let mut storage = crate::services::test_support::encrypted_service_model(
            12,
            serde_json::json!({
                "host": "localhost",
                "port": "9010",
                "access_key": "ak",
                "secret_key": "sk",
                "region": "us-east-1",
            }),
        );
        storage.name = "files".to_string();
        storage.service_type = "s3".to_string();
        for node_id in [Some(3), None] {
            storage.node_id = node_id;
            let db = Arc::new(
                sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                    .append_query_results((0..2).map(|_| vec![storage.clone()]))
                    .into_connection(),
            );
            let manager = crate::services::test_support::control_plane_manager_with_db(db);
            let names = manager
                .get_service_preview_environment_variable_names(12)
                .await
                .expect("names need no Docker");
            for name in ["S3_BUCKET", "S3_ENDPOINT", "AWS_ACCESS_KEY_ID"] {
                assert!(names.contains(&name.to_string()), "{node_id:?}: {names:?}");
            }
            let mut sorted = names.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(names, sorted, "sorted, each name once");
        }

        let (manager, _) = control_plane_answering(2);
        let names = manager
            .get_service_preview_environment_variable_names(11)
            .await
            .expect("names need no Docker");
        assert!(
            names.contains(&"POSTGRES_DATABASE".to_string()),
            "{names:?}"
        );
        assert!(names.contains(&"POSTGRES_URL".to_string()), "{names:?}");
    }

    /// The project's linked-service variables (what the environments page
    /// lists when no environment is picked) include a worker's database.
    #[tokio::test]
    async fn a_worker_service_lists_its_connection_variables() {
        let (manager, _) = control_plane_answering(2);
        let variables = manager
            .get_service_environment_variables(11, 1)
            .await
            .expect("connection variables need no Docker");
        assert_eq!(variables["POSTGRES_HOST"], "postgres-orders");
        assert_eq!(variables["POSTGRES_PORT"], "5432");
    }

    /// Building an engine for an operation still needs Docker, and still
    /// says so with the typed error.
    #[test]
    fn engines_for_daemon_operations_still_need_docker() {
        let manager = control_plane_manager();
        let error = manager
            .create_service_instance_for_parameters(
                "orders".to_string(),
                ServiceType::Postgres,
                &postgres_parameters(),
            )
            .err()
            .expect("no Docker on a control-plane server");
        assert!(
            matches!(error, ExternalServiceError::DockerUnavailable(_)),
            "{error:?}"
        );
    }
}
