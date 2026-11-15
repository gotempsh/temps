// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Storage backend selection for deployment asset storage (static-site build
//! output and the content-addressed asset store).
//!
//! This reuses Temps' existing `TEMPS_LOG_S3_*` connection variables
//! (bucket/region/endpoint/credentials/path-style) instead of introducing a
//! second full credential surface. Those variables already answer "how does
//! this Temps install reach its S3-compatible bucket" -- they were
//! introduced for `temps-log-aggregator` (commit `cfb52a895`, ~7 months
//! before this file) and reused unchanged a second time for build/deploy job
//! log archival. Deployment assets are a third consumer of the same bucket
//! and credentials, not a reason to ask an operator to paste the same
//! access key in twice under a new name.
//!
//! **What is *not* reused: the backend on/off switch.** An earlier version
//! of this file also reused `TEMPS_LOG_STORAGE_BACKEND` itself to decide
//! whether static-site/CAS storage uses S3 -- i.e. the same flag that
//! already exists on every install that archives logs to S3 today. That is
//! unsafe across an upgrade: an operator who set `TEMPS_LOG_STORAGE_BACKEND=s3`
//! months ago to archive logs, with static-site deployments still sitting on
//! local disk, would have those deployments become instantly unreadable the
//! moment they upgrade to a build containing this file -- the proxy would
//! start looking for their already-deployed, never-migrated static assets in
//! an S3 bucket that has never seen them, purely because a flag they set for
//! an unrelated subsystem got silently reinterpreted as also applying here.
//! `TEMPS_STATIC_STORAGE_BACKEND` therefore stays its own variable: the one
//! part of this configuration that is genuinely a new, independent operator
//! decision ("should *this* subsystem, specifically, move to S3"), scoped so
//! it can never be flipped on by a decision made for a different subsystem.
//! It carries no bucket/region/credentials of its own -- once it is `s3`,
//! every connection detail comes from `TEMPS_LOG_S3_*`.
//!
//! Reusing the log side's bucket and credentials means static-site output
//! and CAS blobs land in the *same* bucket as log data once an operator
//! opts in, which is intentional (one bucket, one credential set for all of
//! Temps' own S3-compatible operational storage that has opted in). To keep
//! the data domains from colliding inside that shared bucket, every key this
//! backend writes is placed under a fixed `static-assets/` prefix that is
//! never operator-configurable -- unlike the log side's `TEMPS_LOG_S3_PREFIX`,
//! which selects between `logs/` (default) and an operator override, there is
//! deliberately no `TEMPS_STATIC_S3_PREFIX`: introducing one would either
//! reopen the "new env var" question or, if it reused `TEMPS_LOG_S3_PREFIX`,
//! could silently point static assets at the same key prefix as log data
//! whenever an operator customizes that variable for their own log setup.
//!
//! Both new S3-backed implementations (`temps_deployer::static_deployer::S3StaticDeployer`
//! and `temps_file_store::s3_store::S3FileStore`) share this one resolver and
//! the client-construction helper in `s3_client`, so the two backends can
//! never disagree about which bucket/region/credentials to use.

use std::time::Duration;

/// Fixed S3 request timeout for every deployment-asset S3 operation's
/// initial request/response-headers phase (PutObject, DeleteObject,
/// HeadObject, and the headers phase of GetObject); body streaming for large
/// objects is bounded separately by the idle-read timeout in `s3_store`
/// because a valid large static file must not be killed by its own size.
/// Not operator-configurable -- it has no equivalent on the log side, and an
/// internal request timeout is not something an operator needs to tune per
/// subsystem. See `s3_client::build_s3_client`.
pub const DEFAULT_S3_TIMEOUT_SECS: u64 = 10;

/// Fixed key prefix every deployment-asset object is stored under, so this
/// backend's `blobs/`/`paths/`/raw-key namespaces can never collide with
/// `TEMPS_LOG_S3_*`'s own `logs/`-prefixed (or operator-renamed) objects in
/// the same bucket. See the module doc comment.
pub const STATIC_ASSETS_KEY_PREFIX: &str = "static-assets/";

/// Root namespace used by every durable object-store consumer in stateless
/// mode. Keeping all objects for one installation below one stable prefix
/// gives each installation a stable namespace. Isolation still requires the
/// bucket policy to restrict credentials to this prefix.
pub const STATELESS_INSTANCES_KEY_PREFIX: &str = "instances";

/// Resolved connection details for the S3-compatible deployment-asset backend.
#[derive(Clone)]
pub struct S3StorageConfig {
    pub bucket: String,
    pub region: String,
    pub endpoint: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub force_path_style: bool,
    pub timeout: Duration,
    /// Always `Some(STATIC_ASSETS_KEY_PREFIX)` -- fixed, not
    /// operator-configurable. Kept as a field (rather than inlined at every
    /// key-building call site) so `s3_store`/`s3_static_deployer` share one
    /// value and one place that could change it.
    pub prefix: Option<String>,
}

impl std::fmt::Debug for S3StorageConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("S3StorageConfig")
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("endpoint", &self.endpoint)
            .field("access_key_id", &"[REDACTED]")
            .field("secret_access_key", &"[REDACTED]")
            .field("force_path_style", &self.force_path_style)
            .field("timeout", &self.timeout)
            .field("prefix", &self.prefix)
            .finish()
    }
}

/// Which backend deployment assets (static-site output + CAS blobs) are
/// stored on.
#[derive(Debug, Clone)]
pub enum StaticStorageBackend {
    /// Default: local disk under `TEMPS_DATA_DIR/static` and `TEMPS_DATA_DIR/cas`.
    Filesystem,
    /// S3-compatible object storage (AWS S3, MinIO, Tigris, R2, RustFS, ...).
    /// Enabled by `TEMPS_STATIC_STORAGE_BACKEND=s3`; bucket/region/endpoint/
    /// credentials come from the pre-existing `TEMPS_LOG_S3_*` variables.
    S3(S3StorageConfig),
}

#[derive(Debug, thiserror::Error)]
pub enum StaticStorageConfigError {
    #[error(
        "TEMPS_STATIC_STORAGE_BACKEND is set to 's3', but {variable} is not set. Set it -- \
         it is the same TEMPS_LOG_S3_* connection detail already used for S3-backed logs -- \
         or unset TEMPS_STATIC_STORAGE_BACKEND to keep storing static-site output and the CAS \
         asset store on local disk"
    )]
    MissingS3Variable { variable: &'static str },

    #[error("Invalid TEMPS_STATELESS value '{value}': expected exactly 'true' or 'false'")]
    InvalidStatelessFlag { value: String },

    #[error("TEMPS_STATELESS=true requires a non-empty TEMPS_INSTANCE_ID")]
    MissingInstanceId,

    #[error(
        "Invalid TEMPS_INSTANCE_ID '{value}': use 1-64 ASCII letters, digits, '.', '_', or '-'"
    )]
    InvalidInstanceId { value: String },
}

/// Validated process-wide stateless storage selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatelessStorage {
    Disabled,
    Enabled { instance_id: String },
}

impl StatelessStorage {
    /// Prefix shared by durable subsystems for this installation.
    pub fn instance_prefix(&self) -> Option<String> {
        match self {
            Self::Disabled => None,
            Self::Enabled { instance_id } => {
                Some(format!("{STATELESS_INSTANCES_KEY_PREFIX}/{instance_id}/"))
            }
        }
    }

    /// Place a subsystem below the validated installation namespace.
    pub fn subsystem_prefix(&self, subsystem: &str) -> Option<String> {
        self.instance_prefix()
            .map(|prefix| format!("{prefix}{}/", subsystem.trim_matches('/')))
    }
}

/// Bootstrap configuration for Temps' own S3-compatible operational storage:
/// the backend switches and the shared `TEMPS_LOG_S3_*` connection.
///
/// This is the **only** place these variables are read. Every consumer
/// (deployment assets here, the log aggregator and build-log archival via
/// [`resolve_log_storage_backend_for`], stateless startup) goes through
/// [`ObjectStorageEnvironment::from_process_env`], so the connection has one
/// home and the consumers cannot drift apart.
///
/// Why these stay in the environment rather than on the settings row: they
/// are bootstrap values. In stateless mode the bucket *is* the installation's
/// durable storage and is probed and bound to the database owner lock before
/// the control plane starts (`temps serve`'s stateless startup), and a
/// database-stored credential would have to be decrypted with a key that, in
/// that mode, itself lives behind this storage. Outside stateless mode they
/// still select where every process (console, standalone `temps proxy`,
/// deployer) reads deployment assets from at startup, and switching them at
/// runtime would strand data already written to the other backend.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ObjectStorageEnvironment {
    /// `TEMPS_LOG_STORAGE_BACKEND`: `s3` archives logs to the bucket.
    pub log_storage_backend: Option<String>,
    /// `TEMPS_STATIC_STORAGE_BACKEND`: `s3` stores deployment assets there.
    pub static_storage_backend: Option<String>,
    pub bucket: Option<String>,
    pub region: Option<String>,
    pub endpoint: Option<String>,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
    /// `TEMPS_LOG_S3_PREFIX`: log objects only; never applied to assets.
    pub log_prefix: Option<String>,
    pub force_path_style: Option<String>,
}

impl std::fmt::Debug for ObjectStorageEnvironment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ObjectStorageEnvironment")
            .field("log_storage_backend", &self.log_storage_backend)
            .field("static_storage_backend", &self.static_storage_backend)
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("endpoint", &self.endpoint)
            .field("access_key_id_set", &self.access_key_id.is_some())
            .field("secret_access_key_set", &self.secret_access_key.is_some())
            .field("log_prefix", &self.log_prefix)
            .field("force_path_style", &self.force_path_style)
            .finish()
    }
}

impl ObjectStorageEnvironment {
    /// Snapshot the variables. Called once per resolution at startup.
    #[allow(
        clippy::disallowed_methods,
        reason = "bootstrap: Temps' own S3 storage connection is needed before the control \
                  plane starts (stateless mode binds it to the database owner lock) and cannot \
                  live behind a key stored in that storage; this is its single reader"
    )]
    pub fn from_process_env() -> Self {
        let read = |name: &str| std::env::var(name).ok();
        Self {
            log_storage_backend: read("TEMPS_LOG_STORAGE_BACKEND"),
            static_storage_backend: read("TEMPS_STATIC_STORAGE_BACKEND"),
            bucket: read("TEMPS_LOG_S3_BUCKET"),
            region: read("TEMPS_LOG_S3_REGION"),
            endpoint: read("TEMPS_LOG_S3_ENDPOINT"),
            access_key_id: read("TEMPS_LOG_S3_ACCESS_KEY_ID"),
            secret_access_key: read("TEMPS_LOG_S3_SECRET_ACCESS_KEY"),
            log_prefix: read("TEMPS_LOG_S3_PREFIX"),
            force_path_style: read("TEMPS_LOG_S3_FORCE_PATH_STYLE"),
        }
    }

    /// The shared bucket connection, or the name of the first missing
    /// required variable. `prefix` is left `None` for the caller to set.
    fn s3_connection(&self) -> Result<S3StorageConfig, &'static str> {
        fn required(
            value: &Option<String>,
            variable: &'static str,
        ) -> Result<String, &'static str> {
            value
                .clone()
                .filter(|value| !value.trim().is_empty())
                .ok_or(variable)
        }
        Ok(S3StorageConfig {
            bucket: required(&self.bucket, "TEMPS_LOG_S3_BUCKET")?,
            region: self
                .region
                .clone()
                .unwrap_or_else(|| "us-east-1".to_string()),
            endpoint: self
                .endpoint
                .clone()
                .filter(|value| !value.trim().is_empty()),
            access_key_id: required(&self.access_key_id, "TEMPS_LOG_S3_ACCESS_KEY_ID")?,
            secret_access_key: required(&self.secret_access_key, "TEMPS_LOG_S3_SECRET_ACCESS_KEY")?,
            force_path_style: self
                .force_path_style
                .as_deref()
                .is_some_and(|v| v == "true" || v == "1"),
            timeout: Duration::from_secs(DEFAULT_S3_TIMEOUT_SECS),
            prefix: None,
        })
    }
}

/// Resolve and validate the installation-wide stateless mode opt-in.
///
/// `TEMPS_STATELESS` / `TEMPS_INSTANCE_ID` are one-shot first-boot inputs:
/// the persisted installation identity
/// (`temps_config::stateless_instance_id`) wins once it exists, and every
/// runtime consumer uses [`resolve_stateless_storage_for`] with that value.
#[allow(
    clippy::disallowed_methods,
    reason = "bootstrap: first-boot input deciding the installation's storage layout before the \
              database identity exists; afterwards the persisted identity is used"
)]
pub fn resolve_stateless_storage() -> Result<StatelessStorage, StaticStorageConfigError> {
    let enabled = match std::env::var("TEMPS_STATELESS") {
        Err(std::env::VarError::NotPresent) => false,
        Err(std::env::VarError::NotUnicode(value)) => {
            return Err(StaticStorageConfigError::InvalidStatelessFlag {
                value: value.to_string_lossy().into_owned(),
            });
        }
        Ok(value) => match value.as_str() {
            "true" => true,
            "false" => false,
            _ => return Err(StaticStorageConfigError::InvalidStatelessFlag { value }),
        },
    };
    let instance_id = enabled
        .then(|| {
            std::env::var("TEMPS_INSTANCE_ID")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .ok_or(StaticStorageConfigError::MissingInstanceId)
        })
        .transpose()?;
    resolve_stateless_storage_for(instance_id.as_deref())
}

/// Resolve stateless storage from the database-backed installation identity.
pub fn resolve_stateless_storage_for(
    instance_id: Option<&str>,
) -> Result<StatelessStorage, StaticStorageConfigError> {
    let Some(instance_id) = instance_id else {
        return Ok(StatelessStorage::Disabled);
    };
    let instance_id = instance_id.trim().to_string();
    if instance_id.is_empty() {
        return Err(StaticStorageConfigError::MissingInstanceId);
    }
    if matches!(instance_id.as_str(), "." | "..")
        || instance_id.len() > 64
        || !instance_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(StaticStorageConfigError::InvalidInstanceId { value: instance_id });
    }

    Ok(StatelessStorage::Enabled { instance_id })
}

/// Resolve the deployment-asset storage backend from the process environment.
///
/// `TEMPS_STATIC_STORAGE_BACKEND` is this subsystem's own, independent
/// on/off switch (see module doc comment for why it is not automatically
/// implied by `TEMPS_LOG_STORAGE_BACKEND`); when it selects `s3`, every
/// connection detail is read from the pre-existing `TEMPS_LOG_S3_*`
/// variables rather than a second credential surface.
///
/// Read once at process startup by every caller that needs a `StaticDeployer`
/// or `FileStore` (the proxy's read path, the deployer plugin's write path,
/// and the deployments plugin's CAS write path) so they always agree on which
/// backend and bucket are in use.
pub fn resolve_static_storage_backend() -> Result<StaticStorageBackend, StaticStorageConfigError> {
    let stateless = resolve_stateless_storage()?;
    resolve_static_storage_backend_for(&stateless)
}

pub fn resolve_static_storage_backend_for(
    stateless: &StatelessStorage,
) -> Result<StaticStorageBackend, StaticStorageConfigError> {
    static_storage_backend_from(stateless, &ObjectStorageEnvironment::from_process_env())
}

/// [`resolve_static_storage_backend_for`] over an explicit environment
/// snapshot, so the selection logic is testable without process env.
pub fn static_storage_backend_from(
    stateless: &StatelessStorage,
    environment: &ObjectStorageEnvironment,
) -> Result<StaticStorageBackend, StaticStorageConfigError> {
    let backend = environment
        .static_storage_backend
        .as_deref()
        .unwrap_or("filesystem");
    if backend != "s3" && *stateless == StatelessStorage::Disabled {
        return Ok(StaticStorageBackend::Filesystem);
    }

    let prefix = stateless
        .subsystem_prefix("static-assets")
        .unwrap_or_else(|| STATIC_ASSETS_KEY_PREFIX.to_string());

    let mut config = environment
        .s3_connection()
        .map_err(|variable| StaticStorageConfigError::MissingS3Variable { variable })?;
    config.prefix = Some(prefix);
    Ok(StaticStorageBackend::S3(config))
}

/// Default key prefix for log objects when `TEMPS_LOG_S3_PREFIX` is unset.
pub const DEFAULT_LOG_KEY_PREFIX: &str = "logs/";

/// Where log data (aggregated container logs and build/deploy job logs) is
/// stored. `S3` carries the shared connection with its log prefix applied.
#[derive(Debug, Clone)]
pub enum LogStorageBackend {
    Filesystem,
    S3(S3StorageConfig),
}

#[derive(Debug, thiserror::Error)]
pub enum LogStorageConfigError {
    #[error(transparent)]
    Stateless(#[from] StaticStorageConfigError),

    #[error(
        "TEMPS_LOG_STORAGE_BACKEND is set to 's3', but {variable} is not set. Set it (and the \
         other TEMPS_LOG_S3_* variables), or unset TEMPS_LOG_STORAGE_BACKEND to store aggregated \
         logs on local disk"
    )]
    MissingS3Variable { variable: &'static str },
}

/// Resolve the log storage backend from the process environment, through the
/// same single reader as the deployment-asset backend.
pub fn resolve_log_storage_backend_for(
    stateless: &StatelessStorage,
) -> Result<LogStorageBackend, LogStorageConfigError> {
    log_storage_backend_from(stateless, &ObjectStorageEnvironment::from_process_env())
}

/// [`resolve_log_storage_backend_for`] over an explicit environment snapshot.
///
/// In stateless mode logs always go to the installation's scoped `logs/`
/// namespace in the shared bucket. Otherwise `TEMPS_LOG_STORAGE_BACKEND=s3`
/// selects the bucket under `TEMPS_LOG_S3_PREFIX` (default `logs/`).
pub fn log_storage_backend_from(
    stateless: &StatelessStorage,
    environment: &ObjectStorageEnvironment,
) -> Result<LogStorageBackend, LogStorageConfigError> {
    if let Some(prefix) = stateless.subsystem_prefix("logs") {
        if let StaticStorageBackend::S3(mut storage) =
            static_storage_backend_from(stateless, environment)?
        {
            storage.prefix = Some(prefix);
            return Ok(LogStorageBackend::S3(storage));
        }
    }

    if environment.log_storage_backend.as_deref() != Some("s3") {
        return Ok(LogStorageBackend::Filesystem);
    }

    let mut storage = environment
        .s3_connection()
        .map_err(|variable| LogStorageConfigError::MissingS3Variable { variable })?;
    // A blank `TEMPS_LOG_S3_ENDPOINT` means AWS default resolution here too
    // (it used to be passed through as an empty endpoint for logs only).
    storage.prefix = Some(
        environment
            .log_prefix
            .clone()
            .unwrap_or_else(|| DEFAULT_LOG_KEY_PREFIX.to_string()),
    );
    Ok(LogStorageBackend::S3(storage))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Environment variables are process-global, so every test touching them
    /// runs under this lock and clears its own variables afterward. Clears
    /// both this subsystem's own switch and the reused `TEMPS_LOG_S3_*`
    /// connection variables; using the same variable names as
    /// `temps-deployment-logs`'/`temps-log-aggregator`'s own env-var tests is safe
    /// since each crate's tests run in their own `cargo test` process.
    fn clear_env() {
        for variable in [
            "TEMPS_STATIC_STORAGE_BACKEND",
            "TEMPS_STATELESS",
            "TEMPS_INSTANCE_ID",
            "TEMPS_LOG_STORAGE_BACKEND",
            "TEMPS_LOG_S3_BUCKET",
            "TEMPS_LOG_S3_REGION",
            "TEMPS_LOG_S3_ENDPOINT",
            "TEMPS_LOG_S3_ACCESS_KEY_ID",
            "TEMPS_LOG_S3_SECRET_ACCESS_KEY",
            "TEMPS_LOG_S3_FORCE_PATH_STYLE",
        ] {
            std::env::remove_var(variable);
        }
    }

    #[test]
    #[serial(temps_static_storage_env)]
    fn defaults_to_filesystem_when_unset() {
        clear_env();
        assert!(matches!(
            resolve_static_storage_backend().unwrap(),
            StaticStorageBackend::Filesystem
        ));
    }

    #[test]
    #[serial(temps_static_storage_env)]
    fn persisted_local_mode_ignores_true_bootstrap_environment() {
        clear_env();
        std::env::set_var("TEMPS_STATELESS", "true");
        std::env::set_var("TEMPS_INSTANCE_ID", "environment-instance");
        assert_eq!(
            resolve_stateless_storage_for(None).unwrap(),
            StatelessStorage::Disabled
        );
        clear_env();
    }

    #[test]
    #[serial(temps_static_storage_env)]
    fn persisted_stateless_identity_ignores_false_bootstrap_environment() {
        clear_env();
        std::env::set_var("TEMPS_STATELESS", "false");
        assert_eq!(
            resolve_stateless_storage_for(Some("durable-instance")).unwrap(),
            StatelessStorage::Enabled {
                instance_id: "durable-instance".to_string()
            }
        );
        clear_env();
    }

    #[test]
    #[serial(temps_static_storage_env)]
    fn defaults_to_filesystem_for_any_non_s3_value() {
        clear_env();
        std::env::set_var("TEMPS_STATIC_STORAGE_BACKEND", "filesystem");
        assert!(matches!(
            resolve_static_storage_backend().unwrap(),
            StaticStorageBackend::Filesystem
        ));
        clear_env();
    }

    /// Regression test for the exact upgrade hazard described in the module
    /// doc comment: an install that already archives logs to S3 (a
    /// perfectly ordinary, pre-existing configuration) must NOT have its
    /// static-site/CAS storage silently switched to S3 just because that
    /// flag is set -- `TEMPS_STATIC_STORAGE_BACKEND` is a separate decision.
    #[test]
    #[serial(temps_static_storage_env)]
    fn log_storage_backend_alone_does_not_enable_static_s3() {
        clear_env();
        std::env::set_var("TEMPS_LOG_STORAGE_BACKEND", "s3");
        std::env::set_var("TEMPS_LOG_S3_BUCKET", "temps-logs");
        std::env::set_var("TEMPS_LOG_S3_ACCESS_KEY_ID", "key");
        std::env::set_var("TEMPS_LOG_S3_SECRET_ACCESS_KEY", "secret");
        // TEMPS_STATIC_STORAGE_BACKEND deliberately unset -- this is exactly
        // the state of an existing install that opted logs into S3 before
        // this file existed and has not touched static-site config at all.

        assert!(matches!(
            resolve_static_storage_backend().unwrap(),
            StaticStorageBackend::Filesystem
        ));
        clear_env();
    }

    #[test]
    #[serial(temps_static_storage_env)]
    fn missing_required_s3_variable_names_it_in_the_error() {
        clear_env();
        std::env::set_var("TEMPS_STATIC_STORAGE_BACKEND", "s3");
        std::env::set_var("TEMPS_LOG_S3_ACCESS_KEY_ID", "key");
        std::env::set_var("TEMPS_LOG_S3_SECRET_ACCESS_KEY", "secret");
        // TEMPS_LOG_S3_BUCKET deliberately unset.

        let error = resolve_static_storage_backend().unwrap_err();

        assert!(error.to_string().contains("TEMPS_LOG_S3_BUCKET"));
        clear_env();
    }

    #[test]
    #[serial(temps_static_storage_env)]
    fn blank_required_variable_is_treated_as_missing() {
        clear_env();
        std::env::set_var("TEMPS_STATIC_STORAGE_BACKEND", "s3");
        std::env::set_var("TEMPS_LOG_S3_BUCKET", "   ");
        std::env::set_var("TEMPS_LOG_S3_ACCESS_KEY_ID", "key");
        std::env::set_var("TEMPS_LOG_S3_SECRET_ACCESS_KEY", "secret");

        let error = resolve_static_storage_backend().unwrap_err();

        assert!(error.to_string().contains("TEMPS_LOG_S3_BUCKET"));
        clear_env();
    }

    #[test]
    #[serial(temps_static_storage_env)]
    fn fully_configured_s3_backend_resolves_with_defaults_applied() {
        clear_env();
        std::env::set_var("TEMPS_STATIC_STORAGE_BACKEND", "s3");
        std::env::set_var("TEMPS_LOG_S3_BUCKET", "temps-static");
        std::env::set_var("TEMPS_LOG_S3_ACCESS_KEY_ID", "key");
        std::env::set_var("TEMPS_LOG_S3_SECRET_ACCESS_KEY", "secret");

        let backend = resolve_static_storage_backend().unwrap();
        let StaticStorageBackend::S3(config) = backend else {
            panic!("expected S3 backend");
        };

        assert_eq!(config.bucket, "temps-static");
        assert_eq!(config.region, "us-east-1");
        assert_eq!(config.endpoint, None);
        assert!(!config.force_path_style);
        assert_eq!(config.timeout, Duration::from_secs(DEFAULT_S3_TIMEOUT_SECS));
        assert_eq!(config.prefix.as_deref(), Some(STATIC_ASSETS_KEY_PREFIX));
        clear_env();
    }

    /// The common case: an operator who wants one shared bucket for logs and
    /// deployment assets sets both switches together.
    #[test]
    #[serial(temps_static_storage_env)]
    fn both_switches_set_together_share_bucket_and_credentials() {
        clear_env();
        std::env::set_var("TEMPS_LOG_STORAGE_BACKEND", "s3");
        std::env::set_var("TEMPS_STATIC_STORAGE_BACKEND", "s3");
        std::env::set_var("TEMPS_LOG_S3_BUCKET", "temps-shared");
        std::env::set_var("TEMPS_LOG_S3_ACCESS_KEY_ID", "key");
        std::env::set_var("TEMPS_LOG_S3_SECRET_ACCESS_KEY", "secret");

        let backend = resolve_static_storage_backend().unwrap();
        let StaticStorageBackend::S3(config) = backend else {
            panic!("expected S3 backend");
        };

        assert_eq!(config.bucket, "temps-shared");
        clear_env();
    }

    #[test]
    #[serial(temps_static_storage_env)]
    fn force_path_style_and_custom_endpoint_and_region_are_honored() {
        clear_env();
        std::env::set_var("TEMPS_STATIC_STORAGE_BACKEND", "s3");
        std::env::set_var("TEMPS_LOG_S3_BUCKET", "temps-static");
        std::env::set_var("TEMPS_LOG_S3_ACCESS_KEY_ID", "key");
        std::env::set_var("TEMPS_LOG_S3_SECRET_ACCESS_KEY", "secret");
        std::env::set_var("TEMPS_LOG_S3_ENDPOINT", "http://localhost:9000");
        std::env::set_var("TEMPS_LOG_S3_REGION", "eu-west-1");
        std::env::set_var("TEMPS_LOG_S3_FORCE_PATH_STYLE", "true");

        let backend = resolve_static_storage_backend().unwrap();
        let StaticStorageBackend::S3(config) = backend else {
            panic!("expected S3 backend");
        };

        assert_eq!(config.endpoint.as_deref(), Some("http://localhost:9000"));
        assert_eq!(config.region, "eu-west-1");
        assert!(config.force_path_style);
        clear_env();
    }

    #[test]
    #[serial(temps_static_storage_env)]
    fn blank_endpoint_uses_aws_default_resolution() {
        clear_env();
        std::env::set_var("TEMPS_STATIC_STORAGE_BACKEND", "s3");
        std::env::set_var("TEMPS_LOG_S3_BUCKET", "temps-static");
        std::env::set_var("TEMPS_LOG_S3_ACCESS_KEY_ID", "key");
        std::env::set_var("TEMPS_LOG_S3_SECRET_ACCESS_KEY", "secret");
        std::env::set_var("TEMPS_LOG_S3_ENDPOINT", "");

        let StaticStorageBackend::S3(config) = resolve_static_storage_backend().unwrap() else {
            panic!("expected S3 backend");
        };
        assert_eq!(config.endpoint, None);
        clear_env();
    }

    #[test]
    #[serial(temps_static_storage_env)]
    fn resolved_prefix_is_the_fixed_static_assets_namespace_regardless_of_input() {
        // There is deliberately no TEMPS_STATIC_S3_PREFIX / TEMPS_LOG_S3_PREFIX
        // read here (see module doc comment) -- setting the log side's own
        // prefix variable must NOT change where static assets land, or an
        // operator customizing their log prefix would silently redirect
        // static-site/CAS objects into whatever prefix they picked for logs.
        clear_env();
        std::env::set_var("TEMPS_STATIC_STORAGE_BACKEND", "s3");
        std::env::set_var("TEMPS_LOG_S3_BUCKET", "temps-static");
        std::env::set_var("TEMPS_LOG_S3_ACCESS_KEY_ID", "key");
        std::env::set_var("TEMPS_LOG_S3_SECRET_ACCESS_KEY", "secret");
        std::env::set_var("TEMPS_LOG_S3_PREFIX", "operator-custom-log-prefix/");

        let backend = resolve_static_storage_backend().unwrap();
        let StaticStorageBackend::S3(config) = backend else {
            panic!("expected S3 backend");
        };

        assert_eq!(config.prefix.as_deref(), Some(STATIC_ASSETS_KEY_PREFIX));
        std::env::remove_var("TEMPS_LOG_S3_PREFIX");
        clear_env();
    }

    #[test]
    #[serial(temps_static_storage_env)]
    fn stateless_mode_requires_instance_id() {
        clear_env();
        std::env::set_var("TEMPS_STATELESS", "true");

        assert!(matches!(
            resolve_stateless_storage(),
            Err(StaticStorageConfigError::MissingInstanceId)
        ));
        clear_env();
    }

    #[test]
    #[serial(temps_static_storage_env)]
    fn stateless_mode_rejects_unsafe_instance_id() {
        clear_env();
        std::env::set_var("TEMPS_STATELESS", "true");
        std::env::set_var("TEMPS_INSTANCE_ID", "../another-install");

        assert!(matches!(
            resolve_stateless_storage(),
            Err(StaticStorageConfigError::InvalidInstanceId { .. })
        ));
        clear_env();
    }

    #[test]
    #[serial(temps_static_storage_env)]
    fn stateless_mode_rejects_dot_instance_ids() {
        for unsafe_id in [".", ".."] {
            clear_env();
            std::env::set_var("TEMPS_STATELESS", "true");
            std::env::set_var("TEMPS_INSTANCE_ID", unsafe_id);
            assert!(matches!(
                resolve_stateless_storage(),
                Err(StaticStorageConfigError::InvalidInstanceId { .. })
            ));
        }
        clear_env();
    }

    fn complete_environment() -> ObjectStorageEnvironment {
        ObjectStorageEnvironment {
            bucket: Some("temps-shared".into()),
            access_key_id: Some("key-id".into()),
            secret_access_key: Some("secret-value".into()),
            ..Default::default()
        }
    }

    #[test]
    fn log_backend_defaults_to_filesystem() {
        let backend =
            log_storage_backend_from(&StatelessStorage::Disabled, &complete_environment())
                .expect("filesystem needs no configuration");
        assert!(matches!(backend, LogStorageBackend::Filesystem));
    }

    #[test]
    fn log_backend_uses_the_shared_connection_and_log_prefix() {
        let environment = ObjectStorageEnvironment {
            log_storage_backend: Some("s3".into()),
            region: Some("eu-west-1".into()),
            ..complete_environment()
        };
        let LogStorageBackend::S3(config) =
            log_storage_backend_from(&StatelessStorage::Disabled, &environment).expect("valid")
        else {
            panic!("expected S3 log storage");
        };
        assert_eq!(config.bucket, "temps-shared");
        assert_eq!(config.region, "eu-west-1");
        assert_eq!(config.prefix.as_deref(), Some(DEFAULT_LOG_KEY_PREFIX));

        let custom = ObjectStorageEnvironment {
            log_prefix: Some("custom/".into()),
            ..environment
        };
        let LogStorageBackend::S3(config) =
            log_storage_backend_from(&StatelessStorage::Disabled, &custom).expect("valid")
        else {
            panic!("expected S3 log storage");
        };
        assert_eq!(config.prefix.as_deref(), Some("custom/"));
    }

    #[test]
    fn log_backend_names_the_missing_variable() {
        let environment = ObjectStorageEnvironment {
            log_storage_backend: Some("s3".into()),
            access_key_id: None,
            ..complete_environment()
        };
        let error = log_storage_backend_from(&StatelessStorage::Disabled, &environment)
            .expect_err("incomplete configuration");
        let rendered = error.to_string();
        assert!(
            rendered.contains("TEMPS_LOG_S3_ACCESS_KEY_ID"),
            "{rendered}"
        );
        assert!(rendered.contains("TEMPS_LOG_STORAGE_BACKEND"), "{rendered}");
    }

    #[test]
    fn stateless_log_backend_is_scoped_to_the_instance() {
        let stateless = StatelessStorage::Enabled {
            instance_id: "cp-01".into(),
        };
        let LogStorageBackend::S3(config) =
            log_storage_backend_from(&stateless, &complete_environment()).expect("valid")
        else {
            panic!("stateless mode forces S3");
        };
        assert_eq!(config.prefix.as_deref(), Some("instances/cp-01/logs/"));
    }

    #[test]
    fn log_and_asset_backends_share_one_connection() {
        let environment = ObjectStorageEnvironment {
            log_storage_backend: Some("s3".into()),
            static_storage_backend: Some("s3".into()),
            endpoint: Some("http://minio:9000".into()),
            force_path_style: Some("1".into()),
            ..complete_environment()
        };
        let LogStorageBackend::S3(logs) =
            log_storage_backend_from(&StatelessStorage::Disabled, &environment).expect("valid")
        else {
            panic!("expected S3 logs");
        };
        let StaticStorageBackend::S3(assets) =
            static_storage_backend_from(&StatelessStorage::Disabled, &environment).expect("valid")
        else {
            panic!("expected S3 assets");
        };
        assert_eq!(logs.bucket, assets.bucket);
        assert_eq!(logs.endpoint, assets.endpoint);
        assert_eq!(logs.force_path_style, assets.force_path_style);
        assert_ne!(logs.prefix, assets.prefix);
    }

    #[test]
    fn environment_debug_redacts_credentials() {
        let debug = format!("{:?}", complete_environment());
        assert!(!debug.contains("key-id"));
        assert!(!debug.contains("secret-value"));
    }

    #[test]
    fn s3_config_debug_redacts_credentials() {
        let config = S3StorageConfig {
            bucket: "bucket".to_string(),
            region: "region".to_string(),
            endpoint: None,
            access_key_id: "visible-key-id".to_string(),
            secret_access_key: "visible-secret".to_string(),
            force_path_style: false,
            timeout: Duration::from_secs(1),
            prefix: None,
        };

        let debug = format!("{config:?}");
        assert!(!debug.contains("visible-key-id"));
        assert!(!debug.contains("visible-secret"));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    #[serial(temps_static_storage_env)]
    fn stateless_mode_forces_scoped_s3_storage() {
        clear_env();
        std::env::set_var("TEMPS_STATELESS", "true");
        std::env::set_var("TEMPS_INSTANCE_ID", "control-plane-01");
        std::env::set_var("TEMPS_LOG_S3_BUCKET", "temps-state");
        std::env::set_var("TEMPS_LOG_S3_ACCESS_KEY_ID", "key");
        std::env::set_var("TEMPS_LOG_S3_SECRET_ACCESS_KEY", "secret");

        let StaticStorageBackend::S3(config) = resolve_static_storage_backend().unwrap() else {
            panic!("stateless mode must force S3 storage");
        };
        assert_eq!(
            config.prefix.as_deref(),
            Some("instances/control-plane-01/static-assets/")
        );
        clear_env();
    }
}
