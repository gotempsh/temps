// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Import data from an external database into a database of a managed
//! service.
//!
//! The feature is split along the same line as backups (ADR-014): everything
//! that is the same for every database engine lives here, and each engine
//! contributes only what it alone knows through the narrow
//! [`DataImportEngine`] trait.
//!
//! Engine-agnostic (this module):
//! - [`source`]: parsing connection strings, the SSRF guard and DNS pinning,
//!   masking and secret scrubbing.
//! - [`runner`]: the short-lived helper container that streams the dump from
//!   the source into the target, its timeout, cancellation and fencing.
//! - [`service`]: run bookkeeping (`service_data_imports`), the
//!   one-import-per-database lock, conflicts with restores, and recovery of
//!   runs interrupted by a restart.
//!
//! Engine-specific (an `impl DataImportEngine` next to each engine):
//! what a source URL looks like, how to tell whether a target database is
//! empty, how to create or replace it the way provisioning does, and which
//! dump/restore commands move the data.
//!
//! ## Adding an engine
//!
//! 1. Implement [`DataImportEngine`] for the engine's `ExternalService` type.
//! 2. Override [`crate::externalsvc::ExternalService::data_import`] to return
//!    `Some(self)`.
//!
//! Nothing else changes: the API, the console and run tracking discover the
//! engine through that accessor. An engine that does not opt in is reported
//! as unsupported with a reason, never silently hidden.

pub mod runner;
pub mod service;
pub mod source;
#[cfg(test)]
pub(crate) mod test_support;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;

use crate::externalsvc::ServiceConfig;

pub use service::{
    active_import_run_ids, DataImportAvailability, DataImportRun, DataImportService, RunStarter,
    StartDataImport,
};
pub use source::{ImportSource, SourceEndpoint, SourceUrlRules};

/// What an engine accepts and how its imports behave. Returned to the console
/// so the import form can explain itself before the user types anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DataImportSpec {
    /// Human name of the engine, e.g. "PostgreSQL".
    pub engine_label: String,
    /// URL schemes the source connection string may use.
    pub source_schemes: Vec<String>,
    /// A complete example of a source connection string.
    pub source_url_example: String,
    /// Query-string options the source connection string may carry. Anything
    /// else is refused, because some options redirect the client elsewhere
    /// or read local files.
    pub allowed_source_options: Vec<String>,
    /// Whether a failed or interrupted import leaves no imported data behind.
    /// When false, a failure can leave the target partially written.
    pub atomic: bool,
    /// What the engine stores data in, singular ("table", "collection").
    pub object_noun: String,
}

/// State of the target database before an import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetInspection {
    /// Whether the database exists at all.
    pub exists: bool,
    /// Tables, collections, ... the database holds.
    pub object_count: i64,
    /// Size on disk, when the engine can measure it cheaply.
    pub size_bytes: Option<i64>,
}

/// What has to happen to the target database before data is copied in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetPreparation {
    /// The database does not exist: create it the way provisioning does.
    Create,
    /// The database exists and is empty: copy into it as is.
    UseExisting,
    /// The database exists and the caller asked to replace it: drop it and
    /// create it again.
    Recreate,
}

/// Decide how to prepare the target, refusing to copy into a database that
/// already holds data unless the caller explicitly asked to replace it.
pub fn plan_target_preparation(
    service_id: i32,
    database: &str,
    inspection: TargetInspection,
    replace: bool,
    object_noun: &str,
) -> Result<TargetPreparation, DataImportError> {
    match (inspection.exists, replace) {
        (false, _) => Ok(TargetPreparation::Create),
        (true, true) => Ok(TargetPreparation::Recreate),
        (true, false) if inspection.object_count == 0 => Ok(TargetPreparation::UseExisting),
        (true, false) => Err(DataImportError::TargetNotEmpty {
            service_id,
            database: database.to_string(),
            object_count: inspection.object_count,
            object_noun: object_noun.to_string(),
        }),
    }
}

/// Where the helper container reaches the target service.
#[derive(Debug, Clone, Copy)]
pub struct TransferTarget<'a> {
    /// Host name or address of the target container on the helper's network.
    pub host: &'a str,
    /// Port the target listens on inside its container.
    pub port: &'a str,
    /// Database receiving the data.
    pub database: &'a str,
}

/// One environment variable passed to the helper container.
#[derive(Clone)]
pub struct TransferEnv {
    pub name: String,
    pub value: String,
    /// Secret values are scrubbed from every message the run records.
    pub secret: bool,
}

impl TransferEnv {
    pub fn plain(name: &str, value: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            value: value.into(),
            secret: false,
        }
    }

    pub fn secret(name: &str, value: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            value: value.into(),
            secret: true,
        }
    }
}

impl std::fmt::Debug for TransferEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = if self.secret {
            "***"
        } else {
            self.value.as_str()
        };
        write!(f, "{}={}", self.name, value)
    }
}

/// How an engine moves the data: a producer that writes the dump of the
/// source to stdout, piped into a consumer that applies it to the target.
///
/// Both are POSIX shell fragments run by [`runner`] inside `image`. They must
/// read every credential and connection string from `env` — never inline
/// one — so secrets stay out of the command line. The runner records which
/// side failed, so a target error is never reported as a source error.
#[derive(Debug, Clone)]
pub struct TransferPlan {
    /// Image holding the engine's client tools.
    pub image: String,
    /// Writes the source dump to stdout; exits non-zero on any error.
    pub producer: String,
    /// Applies stdin to the target; exits non-zero on any error.
    pub consumer: String,
    pub env: Vec<TransferEnv>,
}

/// Engine-specific half of a data import. See the module documentation.
///
/// Every method takes the service's decrypted [`ServiceConfig`] explicitly,
/// so an engine instance freshly built by the service manager (which holds no
/// config of its own) can serve it.
#[async_trait]
pub trait DataImportEngine: Send + Sync {
    /// Static description of what this engine accepts.
    fn import_spec(&self) -> DataImportSpec;

    /// Parse and validate a source connection string. Pure: no network.
    fn parse_source(&self, raw: &str) -> Result<ImportSource, DataImportError>;

    /// Validate the name of the database that will receive the data,
    /// including refusing the server's own system databases.
    fn validate_target_database(&self, database: &str) -> Result<(), DataImportError>;

    /// Whether `database` exists on the target service and how much it holds.
    async fn inspect_target(
        &self,
        config: &ServiceConfig,
        database: &str,
    ) -> Result<TargetInspection, DataImportError>;

    /// Create, or drop and re-create, `database` exactly the way the engine's
    /// project provisioning creates it (same owner, same grants), so a
    /// deployment linked later finds it and uses it.
    async fn prepare_target(
        &self,
        config: &ServiceConfig,
        database: &str,
        preparation: TargetPreparation,
    ) -> Result<(), DataImportError>;

    /// Name of the container the target service runs in.
    fn target_container(&self, config: &ServiceConfig) -> Result<String, DataImportError>;

    /// Commands and environment that copy `source` into `target`.
    async fn transfer_plan(
        &self,
        config: &ServiceConfig,
        source: &ImportSource,
        target: &TransferTarget<'_>,
    ) -> Result<TransferPlan, DataImportError>;

    /// Turn a known client error in the helper's output into advice the
    /// operator can act on. `None` keeps the raw output.
    fn failure_hint(&self, _output: &str) -> Option<String> {
        None
    }
}

/// Errors of the data import feature. Every variant names what it was
/// operating on; none ever carries a secret.
#[derive(Debug, Error)]
pub enum DataImportError {
    #[error("Service {service_id} not found")]
    ServiceNotFound { service_id: i32 },

    #[error("Data import {run_id} not found for service {service_id}")]
    RunNotFound { service_id: i32, run_id: i32 },

    #[error("Service {service_id} ({service_type}) cannot receive imported data: {reason}")]
    Unsupported {
        service_id: i32,
        service_type: String,
        reason: String,
    },

    #[error("Service {service_id} is not ready to receive imported data: {reason}")]
    NotReady { service_id: i32, reason: String },

    #[error("Invalid source connection string: {reason}")]
    InvalidSource { reason: String },

    #[error("Invalid target database '{database}': {reason}")]
    InvalidTargetDatabase { database: String, reason: String },

    #[error(
        "Database '{database}' of service {service_id} is not empty ({object_count} \
         {object_noun}(s)); enable replace to drop it and import into a fresh database"
    )]
    TargetNotEmpty {
        service_id: i32,
        database: String,
        object_count: i64,
        object_noun: String,
    },

    #[error("Import {run_id} into database '{database}' of service {service_id} is still running")]
    AlreadyRunning {
        service_id: i32,
        database: String,
        run_id: i32,
    },

    #[error(
        "Service {service_id} is being restored (restore run {restore_run_id}); wait for it to \
         finish before importing data"
    )]
    RestoreInProgress {
        service_id: i32,
        restore_run_id: i32,
    },

    #[error("Data import {run_id} cannot be cancelled (status '{status}'): {reason}")]
    NotCancellable {
        run_id: i32,
        status: String,
        reason: String,
    },

    #[error("Validation error: {message}")]
    Validation { message: String },

    #[error("Failed to {operation} on service '{service}': {reason}")]
    Target {
        /// Service name (engines know the service by name, not id).
        service: String,
        operation: String,
        reason: String,
    },

    #[error("Data import helper failed for service {service_id}: {reason}")]
    Helper { service_id: i32, reason: String },

    #[error(transparent)]
    DockerUnavailable(#[from] temps_core::DockerUnavailable),

    #[error("Database error while handling data imports: {0}")]
    Database(#[from] sea_orm::DbErr),
}

impl DataImportError {
    /// A target-side failure, with secrets already removed from `reason`.
    pub fn target(
        service: impl Into<String>,
        operation: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self::Target {
            service: service.into(),
            operation: operation.into(),
            reason: reason.into(),
        }
    }

    pub fn invalid_source(reason: impl Into<String>) -> Self {
        Self::InvalidSource {
            reason: reason.into(),
        }
    }

    pub fn invalid_target_database(database: &str, reason: impl Into<String>) -> Self {
        Self::InvalidTargetDatabase {
            database: database.to_string(),
            reason: reason.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inspection(exists: bool, object_count: i64) -> TargetInspection {
        TargetInspection {
            exists,
            object_count,
            size_bytes: None,
        }
    }

    #[test]
    fn missing_database_is_created_whatever_replace_says() {
        for replace in [false, true] {
            assert_eq!(
                plan_target_preparation(1, "app", inspection(false, 0), replace, "table")
                    .expect("plan"),
                TargetPreparation::Create
            );
        }
    }

    #[test]
    fn empty_existing_database_is_used_as_is() {
        assert_eq!(
            plan_target_preparation(1, "app", inspection(true, 0), false, "table").expect("plan"),
            TargetPreparation::UseExisting
        );
    }

    #[test]
    fn existing_database_is_recreated_only_when_replace_is_requested() {
        assert_eq!(
            plan_target_preparation(1, "app", inspection(true, 4), true, "table").expect("plan"),
            TargetPreparation::Recreate
        );
        assert_eq!(
            plan_target_preparation(1, "app", inspection(true, 0), true, "table").expect("plan"),
            TargetPreparation::Recreate
        );
    }

    #[test]
    fn non_empty_database_is_refused_without_replace_and_says_how_much_it_holds() {
        let error = plan_target_preparation(7, "shop", inspection(true, 3), false, "collection")
            .expect_err("must refuse");
        assert!(matches!(
            error,
            DataImportError::TargetNotEmpty {
                service_id: 7,
                object_count: 3,
                ..
            }
        ));
        let message = error.to_string();
        assert!(message.contains("'shop'"), "{message}");
        assert!(message.contains("3 collection(s)"), "{message}");
        assert!(message.contains("replace"), "{message}");
    }

    #[test]
    fn secret_env_values_never_appear_in_debug_output() {
        let plan = TransferPlan {
            image: "postgres:18".to_string(),
            producer: "pg_dump \"$SRC\"".to_string(),
            consumer: "psql \"$DST\"".to_string(),
            env: vec![
                TransferEnv::secret("SRC", "postgres://u:hunter2@db.example.com/app"),
                TransferEnv::plain("DATABASE", "app"),
            ],
        };
        let debug = format!("{plan:?}");
        assert!(!debug.contains("hunter2"), "{debug}");
        assert!(debug.contains("SRC=***"), "{debug}");
        assert!(debug.contains("DATABASE=app"), "{debug}");
    }
}
