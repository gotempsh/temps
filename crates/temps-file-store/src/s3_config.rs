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

/// Resolved connection details for the S3-compatible deployment-asset backend.
#[derive(Debug, Clone)]
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
    fn required(variable: &'static str) -> Result<String, StaticStorageConfigError> {
        std::env::var(variable)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or(StaticStorageConfigError::MissingS3Variable { variable })
    }

    let backend =
        std::env::var("TEMPS_STATIC_STORAGE_BACKEND").unwrap_or_else(|_| "filesystem".into());
    if backend != "s3" {
        return Ok(StaticStorageBackend::Filesystem);
    }

    Ok(StaticStorageBackend::S3(S3StorageConfig {
        bucket: required("TEMPS_LOG_S3_BUCKET")?,
        region: std::env::var("TEMPS_LOG_S3_REGION").unwrap_or_else(|_| "us-east-1".to_string()),
        endpoint: std::env::var("TEMPS_LOG_S3_ENDPOINT").ok(),
        access_key_id: required("TEMPS_LOG_S3_ACCESS_KEY_ID")?,
        secret_access_key: required("TEMPS_LOG_S3_SECRET_ACCESS_KEY")?,
        force_path_style: std::env::var("TEMPS_LOG_S3_FORCE_PATH_STYLE")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false),
        timeout: Duration::from_secs(DEFAULT_S3_TIMEOUT_SECS),
        prefix: Some(STATIC_ASSETS_KEY_PREFIX.to_string()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Environment variables are process-global, so every test touching them
    /// runs under this lock and clears its own variables afterward. Clears
    /// both this subsystem's own switch and the reused `TEMPS_LOG_S3_*`
    /// connection variables; using the same variable names as
    /// `temps-logs`'/`temps-log-aggregator`'s own env-var tests is safe
    /// since each crate's tests run in their own `cargo test` process.
    fn clear_env() {
        for variable in [
            "TEMPS_STATIC_STORAGE_BACKEND",
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
}
