// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Storage backend selection for deployment asset storage (static-site build
//! output and the content-addressed asset store).
//!
//! Mirrors the `TEMPS_LOG_STORAGE_BACKEND` / `TEMPS_LOG_S3_*` pattern used by
//! the log aggregator (`temps-cli/src/commands/serve/console.rs`), but under
//! its own `TEMPS_STATIC_*` prefix: these two subsystems are a different data
//! domain (deployment assets, read on every request) from aggregated logs,
//! and operators may want one backend without the other.
//!
//! Both new S3-backed implementations (`temps_deployer::static_deployer::S3StaticDeployer`
//! and `temps_file_store::s3_store::S3FileStore`) share this one resolver and
//! the client-construction helper in `s3_client`, so the two backends can
//! never disagree about which bucket/region/credentials to use.

use std::time::Duration;

/// Default S3 request timeout when `TEMPS_STATIC_S3_TIMEOUT_SECS` is unset.
/// Bounds the initial request/response-headers phase of every S3 operation
/// (PutObject, DeleteObject, HeadObject, and the headers phase of GetObject);
/// body streaming for large objects is bounded separately by the idle-read
/// timeout in `s3_store` because a valid large static file must not be killed
/// by its own size. See `s3_client::build_s3_client`.
pub const DEFAULT_S3_TIMEOUT_SECS: u64 = 10;

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
    /// Optional key prefix so one bucket can be shared across instances or
    /// environments (e.g. `prod/`). `None` stores directly at the bucket
    /// root under the `blobs/` and `paths/` namespaces.
    pub prefix: Option<String>,
}

/// Which backend deployment assets (static-site output + CAS blobs) are
/// stored on.
#[derive(Debug, Clone)]
pub enum StaticStorageBackend {
    /// Default: local disk under `TEMPS_DATA_DIR/static` and `TEMPS_DATA_DIR/cas`.
    Filesystem,
    /// S3-compatible object storage (AWS S3, MinIO, Tigris, R2, RustFS, ...).
    S3(S3StorageConfig),
}

#[derive(Debug, thiserror::Error)]
pub enum StaticStorageConfigError {
    #[error(
        "TEMPS_STATIC_STORAGE_BACKEND is set to 's3', but {variable} is not set. Set it (and \
         the other TEMPS_STATIC_S3_* variables), or unset TEMPS_STATIC_STORAGE_BACKEND to keep \
         storing static-site output and the CAS asset store on local disk"
    )]
    MissingS3Variable { variable: &'static str },

    #[error(
        "TEMPS_STATIC_S3_TIMEOUT_SECS is set to '{value}', which is not a positive integer \
         number of seconds"
    )]
    InvalidTimeout { value: String },
}

/// Resolve the deployment-asset storage backend from the process environment.
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

    let timeout_secs = match std::env::var("TEMPS_STATIC_S3_TIMEOUT_SECS") {
        Ok(value) => value
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|secs| *secs > 0)
            .ok_or(StaticStorageConfigError::InvalidTimeout { value })?,
        Err(_) => DEFAULT_S3_TIMEOUT_SECS,
    };

    Ok(StaticStorageBackend::S3(S3StorageConfig {
        bucket: required("TEMPS_STATIC_S3_BUCKET")?,
        region: std::env::var("TEMPS_STATIC_S3_REGION").unwrap_or_else(|_| "us-east-1".to_string()),
        endpoint: std::env::var("TEMPS_STATIC_S3_ENDPOINT").ok(),
        access_key_id: required("TEMPS_STATIC_S3_ACCESS_KEY_ID")?,
        secret_access_key: required("TEMPS_STATIC_S3_SECRET_ACCESS_KEY")?,
        force_path_style: std::env::var("TEMPS_STATIC_S3_FORCE_PATH_STYLE")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false),
        timeout: Duration::from_secs(timeout_secs),
        prefix: std::env::var("TEMPS_STATIC_S3_PREFIX")
            .ok()
            .filter(|value| !value.trim().is_empty()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Environment variables are process-global, so every test touching them
    /// runs under this lock and clears its own variables afterward — mirrors
    /// the pattern documented for `log_aggregator_storage_config`'s tests.
    fn clear_env() {
        for variable in [
            "TEMPS_STATIC_STORAGE_BACKEND",
            "TEMPS_STATIC_S3_BUCKET",
            "TEMPS_STATIC_S3_REGION",
            "TEMPS_STATIC_S3_ENDPOINT",
            "TEMPS_STATIC_S3_ACCESS_KEY_ID",
            "TEMPS_STATIC_S3_SECRET_ACCESS_KEY",
            "TEMPS_STATIC_S3_FORCE_PATH_STYLE",
            "TEMPS_STATIC_S3_TIMEOUT_SECS",
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

    #[test]
    #[serial(temps_static_storage_env)]
    fn missing_required_s3_variable_names_it_in_the_error() {
        clear_env();
        std::env::set_var("TEMPS_STATIC_STORAGE_BACKEND", "s3");
        std::env::set_var("TEMPS_STATIC_S3_ACCESS_KEY_ID", "key");
        std::env::set_var("TEMPS_STATIC_S3_SECRET_ACCESS_KEY", "secret");
        // TEMPS_STATIC_S3_BUCKET deliberately unset.

        let error = resolve_static_storage_backend().unwrap_err();

        assert!(error.to_string().contains("TEMPS_STATIC_S3_BUCKET"));
        clear_env();
    }

    #[test]
    #[serial(temps_static_storage_env)]
    fn blank_required_variable_is_treated_as_missing() {
        clear_env();
        std::env::set_var("TEMPS_STATIC_STORAGE_BACKEND", "s3");
        std::env::set_var("TEMPS_STATIC_S3_BUCKET", "   ");
        std::env::set_var("TEMPS_STATIC_S3_ACCESS_KEY_ID", "key");
        std::env::set_var("TEMPS_STATIC_S3_SECRET_ACCESS_KEY", "secret");

        let error = resolve_static_storage_backend().unwrap_err();

        assert!(error.to_string().contains("TEMPS_STATIC_S3_BUCKET"));
        clear_env();
    }

    #[test]
    #[serial(temps_static_storage_env)]
    fn fully_configured_s3_backend_resolves_with_defaults_applied() {
        clear_env();
        std::env::set_var("TEMPS_STATIC_STORAGE_BACKEND", "s3");
        std::env::set_var("TEMPS_STATIC_S3_BUCKET", "temps-static");
        std::env::set_var("TEMPS_STATIC_S3_ACCESS_KEY_ID", "key");
        std::env::set_var("TEMPS_STATIC_S3_SECRET_ACCESS_KEY", "secret");

        let backend = resolve_static_storage_backend().unwrap();
        let StaticStorageBackend::S3(config) = backend else {
            panic!("expected S3 backend");
        };

        assert_eq!(config.bucket, "temps-static");
        assert_eq!(config.region, "us-east-1");
        assert_eq!(config.endpoint, None);
        assert!(!config.force_path_style);
        assert_eq!(config.timeout, Duration::from_secs(DEFAULT_S3_TIMEOUT_SECS));
        clear_env();
    }

    #[test]
    #[serial(temps_static_storage_env)]
    fn invalid_timeout_is_rejected_with_a_typed_error() {
        clear_env();
        std::env::set_var("TEMPS_STATIC_STORAGE_BACKEND", "s3");
        std::env::set_var("TEMPS_STATIC_S3_BUCKET", "temps-static");
        std::env::set_var("TEMPS_STATIC_S3_ACCESS_KEY_ID", "key");
        std::env::set_var("TEMPS_STATIC_S3_SECRET_ACCESS_KEY", "secret");
        std::env::set_var("TEMPS_STATIC_S3_TIMEOUT_SECS", "not-a-number");

        let error = resolve_static_storage_backend().unwrap_err();

        assert!(matches!(
            error,
            StaticStorageConfigError::InvalidTimeout { .. }
        ));
        clear_env();
    }

    #[test]
    #[serial(temps_static_storage_env)]
    fn force_path_style_and_custom_endpoint_and_region_are_honored() {
        clear_env();
        std::env::set_var("TEMPS_STATIC_STORAGE_BACKEND", "s3");
        std::env::set_var("TEMPS_STATIC_S3_BUCKET", "temps-static");
        std::env::set_var("TEMPS_STATIC_S3_ACCESS_KEY_ID", "key");
        std::env::set_var("TEMPS_STATIC_S3_SECRET_ACCESS_KEY", "secret");
        std::env::set_var("TEMPS_STATIC_S3_ENDPOINT", "http://localhost:9000");
        std::env::set_var("TEMPS_STATIC_S3_REGION", "eu-west-1");
        std::env::set_var("TEMPS_STATIC_S3_FORCE_PATH_STYLE", "true");
        std::env::set_var("TEMPS_STATIC_S3_TIMEOUT_SECS", "30");

        let backend = resolve_static_storage_backend().unwrap();
        let StaticStorageBackend::S3(config) = backend else {
            panic!("expected S3 backend");
        };

        assert_eq!(config.endpoint.as_deref(), Some("http://localhost:9000"));
        assert_eq!(config.region, "eu-west-1");
        assert!(config.force_path_style);
        assert_eq!(config.timeout, Duration::from_secs(30));
        clear_env();
    }
}
