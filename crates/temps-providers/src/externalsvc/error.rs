// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The error every [`super::ExternalService`] engine returns.
//!
//! Variants say what kind of failure this is, so the HTTP layer can answer
//! with its own status instead of a blanket 500: bad input is a 400, a missing
//! resource a 404, a refused change a 409. Engines know the name of the
//! service they run; the numeric service ID is added by the manager in
//! `ExternalServiceError::Engine`.
//!
//! Driver and exec output can quote a connection string or a `password=` pair,
//! so every constructor below passes its text through
//! [`temps_core::console_startup::redact_secrets`]. Build variants through
//! them rather than with a struct literal.

use axum::http::StatusCode;
use temps_core::console_startup::redact_secrets;
use temps_core::problemdetails::{self, Problem};
use temps_docker_util::DockerUtilError;

use super::managed_s3::ManagedS3Error;
use super::postgres::{PostgresRestartNotConfirmed, PostgresUpgradeRejected};
use super::recreate_guard::RecreateGuardError;
use super::restore_staging::StagingError;
use super::{RestoreCancelled, ServiceType, StagedRestoreStillRunning};
use crate::sandbox_engines::SandboxNamespaceConflict;

/// `Result` over [`EngineError`], the return type of the engine code.
pub type Result<T, E = EngineError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The caller's configuration or request cannot be used.
    #[error("Invalid request for service '{service}': {reason}")]
    InvalidInput { service: String, reason: String },

    /// A value rejected by a helper that runs without a service at hand.
    #[error("Invalid {engine} value: {reason}")]
    InvalidValue {
        engine: &'static str,
        reason: String,
    },

    /// This engine does not implement the operation.
    #[error("{operation} not supported for service type {service_type} (service '{service}')")]
    Unsupported {
        service: String,
        service_type: ServiceType,
        operation: &'static str,
    },

    /// Something the operation needs does not exist.
    #[error("Service '{service}': {resource}")]
    NotFound { service: String, resource: String },

    /// The current state of the service forbids the change. Nothing was changed.
    #[error("Service '{service}': {reason}")]
    Conflict { service: String, reason: String },

    /// The engine instance has no configuration yet: a caller skipped `init`.
    #[error("Service '{service}' is not initialized: {reason}")]
    NotInitialized { service: String, reason: String },

    /// The service could not be reached. `reason` never holds credentials.
    #[error("Cannot connect to service '{service}': {reason}")]
    ConnectionFailed { service: String, reason: String },

    /// An operation on a known service failed.
    #[error("Service '{service}': {reason}")]
    OperationFailed { service: String, reason: String },

    /// A failure in a helper that runs without a service at hand.
    #[error("{engine}: {reason}")]
    EngineFailure {
        engine: &'static str,
        reason: String,
    },

    /// A command run inside a container failed.
    #[error("Container '{container}': {reason}")]
    ContainerExec { container: String, reason: String },

    #[error("Docker operation failed: {0}")]
    Docker(#[from] bollard::errors::Error),

    #[error("I/O operation failed: {0}")]
    Io(#[from] std::io::Error),

    #[error("Could not serialize or parse JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Database operation failed: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("Could not encrypt or decrypt a credential: {0}")]
    Encryption(#[from] temps_core::EncryptionError),

    #[error(transparent)]
    ManagedS3(#[from] ManagedS3Error),

    #[error(transparent)]
    RestoreStaging(#[from] StagingError),

    #[error(transparent)]
    DockerUtil(#[from] DockerUtilError),

    /// Boxed: it carries up to seven strings, and every engine call returns it.
    #[error(transparent)]
    RecreateGuard(Box<RecreateGuardError>),

    #[error(transparent)]
    RestoreCancelled(#[from] RestoreCancelled),

    #[error(transparent)]
    StagedRestoreStillRunning(#[from] StagedRestoreStillRunning),

    #[error(transparent)]
    UpgradeRejected(#[from] PostgresUpgradeRejected),

    #[error(transparent)]
    RestartNotConfirmed(#[from] PostgresRestartNotConfirmed),

    #[error(transparent)]
    SandboxNamespace(#[from] SandboxNamespaceConflict),
}

impl From<RecreateGuardError> for EngineError {
    fn from(error: RecreateGuardError) -> Self {
        Self::RecreateGuard(Box::new(error))
    }
}

impl EngineError {
    pub fn invalid_input(service: impl Into<String>, reason: impl AsRef<str>) -> Self {
        Self::InvalidInput {
            service: service.into(),
            reason: redact_secrets(reason.as_ref()),
        }
    }

    pub fn invalid_value(engine: &'static str, reason: impl AsRef<str>) -> Self {
        Self::InvalidValue {
            engine,
            reason: redact_secrets(reason.as_ref()),
        }
    }

    pub fn unsupported(
        service: impl Into<String>,
        service_type: ServiceType,
        operation: &'static str,
    ) -> Self {
        Self::Unsupported {
            service: service.into(),
            service_type,
            operation,
        }
    }

    pub fn not_found(service: impl Into<String>, resource: impl AsRef<str>) -> Self {
        Self::NotFound {
            service: service.into(),
            resource: redact_secrets(resource.as_ref()),
        }
    }

    pub fn conflict(service: impl Into<String>, reason: impl AsRef<str>) -> Self {
        Self::Conflict {
            service: service.into(),
            reason: redact_secrets(reason.as_ref()),
        }
    }

    pub fn not_initialized(service: impl Into<String>, reason: impl AsRef<str>) -> Self {
        Self::NotInitialized {
            service: service.into(),
            reason: redact_secrets(reason.as_ref()),
        }
    }

    pub fn connection(service: impl Into<String>, reason: impl AsRef<str>) -> Self {
        Self::ConnectionFailed {
            service: service.into(),
            reason: redact_secrets(reason.as_ref()),
        }
    }

    pub fn failed(service: impl Into<String>, reason: impl AsRef<str>) -> Self {
        Self::OperationFailed {
            service: service.into(),
            reason: redact_secrets(reason.as_ref()),
        }
    }

    pub fn engine_failure(engine: &'static str, reason: impl AsRef<str>) -> Self {
        Self::EngineFailure {
            engine,
            reason: redact_secrets(reason.as_ref()),
        }
    }

    pub fn container_exec(container: impl Into<String>, reason: impl AsRef<str>) -> Self {
        Self::ContainerExec {
            container: container.into(),
            reason: redact_secrets(reason.as_ref()),
        }
    }

    /// Whether the caller can act on this error (the request, not the server,
    /// is what has to change). The manager keeps these typed so they reach the
    /// client with their own status; every other error stays a 500.
    pub fn is_client_error(&self) -> bool {
        Problem::from(self).status_code.is_client_error()
    }
}

impl From<&EngineError> for Problem {
    fn from(error: &EngineError) -> Self {
        let (status, title) = match error {
            EngineError::InvalidInput { .. } | EngineError::InvalidValue { .. } => {
                (StatusCode::BAD_REQUEST, "Invalid Service Request")
            }
            EngineError::Unsupported { .. } => (StatusCode::BAD_REQUEST, "Operation Not Supported"),
            EngineError::UpgradeRejected(_) => (StatusCode::BAD_REQUEST, "Upgrade Rejected"),
            EngineError::NotFound { .. } => (StatusCode::NOT_FOUND, "Service Resource Not Found"),
            EngineError::Conflict { .. } => (StatusCode::CONFLICT, "Service Conflict"),
            EngineError::RestoreCancelled(_) => (StatusCode::CONFLICT, "Restore Cancelled"),
            EngineError::RestartNotConfirmed(_) => (StatusCode::CONFLICT, "Restart Not Confirmed"),
            EngineError::SandboxNamespace(_) => {
                (StatusCode::CONFLICT, "Database Belongs To Something Else")
            }
            EngineError::RecreateGuard(guard) if guard.is_refusal() => {
                (StatusCode::CONFLICT, "Change Would Lose Data")
            }
            EngineError::RecreateGuard(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Container Replacement Failed",
            ),
            EngineError::StagedRestoreStillRunning(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Staged Restore Still Running",
            ),
            EngineError::NotInitialized { .. } => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Service Not Initialized")
            }
            EngineError::ConnectionFailed { .. } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Service Connection Failed",
            ),
            EngineError::OperationFailed { .. }
            | EngineError::EngineFailure { .. }
            | EngineError::ContainerExec { .. } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Service Operation Failed",
            ),
            EngineError::ManagedS3(
                ManagedS3Error::UnsupportedBackend { .. }
                | ManagedS3Error::BackendNotAString { .. }
                | ManagedS3Error::DeprecatedBackend
                | ManagedS3Error::BucketsNotAList { .. }
                | ManagedS3Error::InvalidBucketName { .. }
                | ManagedS3Error::TooManyBuckets { .. }
                | ManagedS3Error::InvalidConfig { .. }
                | ManagedS3Error::Unsupported { .. },
            ) => (StatusCode::BAD_REQUEST, "Invalid Managed Storage Request"),
            EngineError::ManagedS3(
                ManagedS3Error::Admin { .. }
                | ManagedS3Error::NotReady { .. }
                | ManagedS3Error::Docker { .. }
                | ManagedS3Error::Mirror { .. },
            ) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Managed Storage Operation Failed",
            ),
            EngineError::RestoreStaging(_) | EngineError::DockerUtil(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Restore Transfer Failed")
            }
            EngineError::Docker(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Docker Operation Failed")
            }
            EngineError::Io(_) => (StatusCode::INTERNAL_SERVER_ERROR, "I/O Operation Failed"),
            EngineError::Json(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Invalid Stored Data"),
            EngineError::Database(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Database Operation Failed",
            ),
            EngineError::Encryption(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Credential Operation Failed",
            ),
        };
        problemdetails::new(status)
            .with_title(title)
            .with_detail(redact_secrets(&error.to_string()))
    }
}

impl From<EngineError> for Problem {
    fn from(error: EngineError) -> Self {
        Problem::from(&error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const SECRET: &str = "s3cr3t-pw";

    fn status(error: EngineError) -> StatusCode {
        Problem::from(error).status_code
    }

    #[test]
    fn invalid_input_is_a_400_naming_the_service() {
        let error = EngineError::invalid_input("orders", "Missing port parameter");
        assert!(error.to_string().contains("orders"), "{error}");
        assert_eq!(status(error), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn invalid_value_is_a_400() {
        let error = EngineError::invalid_value("MariaDB", "password too long");
        assert_eq!(status(error), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn unsupported_is_a_400_naming_the_operation_and_service() {
        let error = EngineError::unsupported("orders", ServiceType::Redis, "Cluster mode");
        let message = error.to_string();
        assert!(
            message.contains("Cluster mode") && message.contains("orders"),
            "{message}"
        );
        assert_eq!(status(error), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn upgrade_rejected_is_a_400() {
        let error = EngineError::from(PostgresUpgradeRejected::Downgrade { from: 17, to: 16 });
        assert_eq!(status(error), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn not_found_is_a_404_naming_the_resource() {
        let error = EngineError::not_found("orders", "Imported container 'pg' not found");
        assert!(error.to_string().contains("'pg'"), "{error}");
        assert_eq!(status(error), StatusCode::NOT_FOUND);
    }

    #[test]
    fn conflict_is_a_409() {
        let error = EngineError::conflict("orders", "a service with that name already exists");
        assert_eq!(status(error), StatusCode::CONFLICT);
    }

    #[test]
    fn restore_cancelled_is_a_409() {
        let error = EngineError::from(RestoreCancelled {
            target: "postgres-orders".into(),
        });
        assert_eq!(status(error), StatusCode::CONFLICT);
    }

    #[test]
    fn restart_not_confirmed_is_a_409() {
        let error = EngineError::from(PostgresRestartNotConfirmed {
            service_name: "orders".into(),
        });
        assert_eq!(status(error), StatusCode::CONFLICT);
    }

    #[test]
    fn sandbox_namespace_conflict_is_a_409() {
        let conflict = crate::sandbox_engines::check_deployment_database(
            "MariaDB",
            "orders",
            Some("the sandbox user exists".to_string()),
        )
        .unwrap_err();
        assert_eq!(status(EngineError::from(conflict)), StatusCode::CONFLICT);
    }

    #[test]
    fn a_recreate_refusal_is_a_409_and_a_failed_recreate_a_500() {
        let engine = super::super::recreate_guard::ManagedEngine::Postgres;
        let refusal = RecreateGuardError::DataInContainerLayer {
            engine,
            service: "orders".into(),
            container: "postgres-orders".into(),
            operation: "a test".into(),
            data_dir: "/data".into(),
        };
        assert_eq!(status(EngineError::from(refusal)), StatusCode::CONFLICT);
        let failure = RecreateGuardError::DataNotPreserved {
            engine,
            service: "orders".into(),
            container: "postgres-orders".into(),
            operation: "a test".into(),
            reason: "identity changed".into(),
            rollback: "rolled back".into(),
            original_in_place: true,
        };
        assert_eq!(
            status(EngineError::from(failure)),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn managed_storage_validation_is_a_400_and_its_failures_a_500() {
        assert_eq!(
            status(EngineError::from(ManagedS3Error::DeprecatedBackend)),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status(EngineError::from(ManagedS3Error::Mirror {
                service: "orders".into(),
                operation: "list".into(),
                reason: "timed out".into(),
            })),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn server_side_failures_stay_500s() {
        let failures = vec![
            EngineError::not_initialized("orders", "Redis not configured"),
            EngineError::connection("orders", "connection refused"),
            EngineError::failed("orders", "Failed to pull image"),
            EngineError::engine_failure("MariaDB", "decompression failed"),
            EngineError::container_exec("postgres-orders", "exited with code 1"),
            EngineError::from(bollard::errors::Error::RequestTimeoutError),
            EngineError::from(std::io::Error::other("disk full")),
            EngineError::from(serde_json::from_str::<u8>("x").unwrap_err()),
            EngineError::from(sea_orm::DbErr::Custom("gone".into())),
            EngineError::from(StagedRestoreStillRunning {
                target: "postgres-orders".into(),
                reason: "docker unreachable".into(),
            }),
        ];
        for error in failures {
            let shown = error.to_string();
            assert_eq!(status(error), StatusCode::INTERNAL_SERVER_ERROR, "{shown}");
        }
    }

    #[test]
    fn only_errors_the_caller_can_act_on_count_as_client_errors() {
        assert!(EngineError::invalid_input("orders", "bad").is_client_error());
        assert!(EngineError::not_found("orders", "gone").is_client_error());
        assert!(EngineError::conflict("orders", "busy").is_client_error());
        assert!(!EngineError::failed("orders", "broke").is_client_error());
        assert!(!EngineError::connection("orders", "refused").is_client_error());
    }

    #[test]
    fn a_problem_carries_the_message_as_its_detail() {
        let problem = Problem::from(EngineError::not_found("orders", "backup 7"));
        assert_eq!(
            problem.body.get("detail").and_then(|v| v.as_str()),
            Some("Service 'orders': backup 7")
        );
    }

    fn assert_no_secret(error: &EngineError) {
        let display = error.to_string();
        let debug = format!("{error:?}");
        assert!(!display.contains(SECRET), "Display leaked: {display}");
        assert!(!debug.contains(SECRET), "Debug leaked: {debug}");
        let body = serde_json::to_string(&Problem::from(error).body).unwrap();
        assert!(!body.contains(SECRET), "Problem leaked: {body}");
    }

    /// Text a driver may build from the URL it was given, through every
    /// constructor that takes free-form text.
    #[test]
    fn no_constructor_lets_a_connection_string_or_password_pair_through() {
        for text in [
            format!("connect redis://default:{SECRET}@10.0.0.1:6379/0 failed"),
            format!("connect postgres://app:{SECRET}@db:5432/app: refused"),
            format!("connect mongodb://root:{SECRET}@db:27017/?authSource=admin failed"),
            format!("connect mysql://root:{SECRET}@db:3306/app failed"),
            format!("connect failed: host=db user=app password={SECRET}"),
            format!("connect failed: password: {SECRET}"),
        ] {
            for error in [
                EngineError::connection("orders", &text),
                EngineError::invalid_input("orders", &text),
                EngineError::invalid_value("Redis", &text),
                EngineError::not_found("orders", &text),
                EngineError::conflict("orders", &text),
                EngineError::not_initialized("orders", &text),
                EngineError::failed("orders", &text),
                EngineError::engine_failure("Redis", &text),
                EngineError::container_exec("redis-orders", &text),
            ] {
                assert_no_secret(&error);
            }
        }
    }

    /// The real drivers, refused on a closed loopback port with a password in
    /// the URL: what they report must not bring the password along.
    #[tokio::test]
    async fn real_driver_connection_failures_carry_no_secret() {
        let redis_error = redis::Client::open(format!("redis://:{SECRET}@127.0.0.1:1/0"))
            .unwrap()
            .get_multiplexed_async_connection()
            .await
            .map(|_| ())
            .unwrap_err();
        assert_no_secret(&EngineError::connection("orders", redis_error.to_string()));

        let postgres_error = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(2))
            .connect(&format!("postgres://app:{SECRET}@127.0.0.1:1/app"))
            .await
            .map(|_| ())
            .unwrap_err();
        assert_no_secret(&EngineError::connection(
            "orders",
            postgres_error.to_string(),
        ));

        let mysql_error = sqlx::mysql::MySqlPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(2))
            .connect(&format!("mysql://root:{SECRET}@127.0.0.1:1/app"))
            .await
            .map(|_| ())
            .unwrap_err();
        assert_no_secret(&EngineError::connection("orders", mysql_error.to_string()));

        let mongo_error = mongodb::options::ClientOptions::parse(format!(
            "mongodb://root:{SECRET}@127.0.0.1:1/?authMechanism=NOT-A-MECHANISM"
        ))
        .await
        .map(|_| ())
        .unwrap_err();
        assert_no_secret(&EngineError::connection("orders", mongo_error.to_string()));
    }

    /// A method the engine does not implement answers 400, not a 500 that
    /// reads like a server fault.
    #[tokio::test]
    async fn a_trait_default_reports_unsupported_with_the_service_name() {
        let docker = bollard::Docker::connect_with_http(
            "http://127.0.0.1:1",
            1,
            bollard::API_DEFAULT_VERSION,
        )
        .unwrap();
        let service = super::super::redis::RedisService::new("orders".into(), Arc::new(docker));
        let config = super::super::ServiceConfig {
            name: "orders".into(),
            service_type: ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({}),
        };
        let error = super::super::ExternalService::init_cluster(&service, config, Vec::new())
            .await
            .unwrap_err();
        assert!(
            matches!(
                &error,
                EngineError::Unsupported { service, service_type: ServiceType::Redis, .. }
                    if service == "orders"
            ),
            "{error:?}"
        );
        assert_eq!(status(error), StatusCode::BAD_REQUEST);
    }
}
