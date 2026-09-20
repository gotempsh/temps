// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! First-boot bootstrap of the Cloud-managed console-access OIDC provider
//! from a one-shot file Temps Cloud drops at
//! `<TEMPS_DATA_DIR>/cloud-oidc.json` before a hosted control plane's first
//! boot (ADR-045 §4).
//!
//! This is the OIDC analogue of `TEMPS_CLOUD_ENROLLMENT_CODE`: a first-boot
//! bootstrap *input*, not runtime configuration. It is read at most once per
//! boot, its result (the `oidc_providers` row) is what actually persists,
//! and the file is deleted immediately after a successful apply so the
//! plaintext client secret never lingers on disk.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

use temps_auth::oidc_service::ManagedCloudOidcConfig;

/// Filename Temps Cloud writes under the instance's `TEMPS_DATA_DIR`.
pub const CONSOLE_OIDC_BOOTSTRAP_FILENAME: &str = "cloud-oidc.json";

/// The on-disk shape of the bootstrap file. A dedicated struct — rather than
/// deriving `Deserialize` directly on `ManagedCloudOidcConfig` — so this
/// module controls the wire format independently of that struct's field set
/// in `temps-auth`, and so it can carry a redacting `Debug` impl: the
/// plaintext secret must never end up in a log line via an incidental
/// `{:?}` somewhere in the call chain. Unknown fields are ignored (no
/// `deny_unknown_fields`), so Cloud can add fields this instance doesn't
/// understand yet without breaking older instances.
#[derive(Deserialize)]
struct RawConsoleOidcBootstrapFile {
    issuer: String,
    client_id: String,
    client_secret: String,
}

impl std::fmt::Debug for RawConsoleOidcBootstrapFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawConsoleOidcBootstrapFile")
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .finish()
    }
}

impl From<RawConsoleOidcBootstrapFile> for ManagedCloudOidcConfig {
    fn from(raw: RawConsoleOidcBootstrapFile) -> Self {
        Self {
            issuer: raw.issuer,
            client_id: raw.client_id,
            client_secret: raw.client_secret,
        }
    }
}

/// Errors from parsing the bootstrap file's *contents*, independent of where
/// those bytes came from. Deliberately does not re-validate the issuer
/// (https-only unless loopback) — that check already lives in
/// `temps_auth::oidc_service::OidcService::upsert_managed_cloud_provider`
/// (`validate_issuer_url`), and duplicating it here would only let the two
/// drift apart. An invalid issuer therefore parses fine and fails later, at
/// apply time, as `ConsoleOidcBootstrapError::Apply`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConsoleOidcBootstrapParseError {
    #[error("not valid JSON: {reason}")]
    InvalidJson { reason: String },
    #[error("field '{field}' cannot be empty")]
    EmptyField { field: &'static str },
}

/// Parse the bytes of a `cloud-oidc.json` bootstrap file into a
/// [`ManagedCloudOidcConfig`]. Pure — no filesystem access — so it is
/// trivially unit-testable and reusable by callers other than
/// [`apply_console_oidc_bootstrap_file_with`].
pub fn parse_console_oidc_bootstrap_file(
    bytes: &[u8],
) -> Result<ManagedCloudOidcConfig, ConsoleOidcBootstrapParseError> {
    let raw: RawConsoleOidcBootstrapFile = serde_json::from_slice(bytes).map_err(|error| {
        ConsoleOidcBootstrapParseError::InvalidJson {
            reason: error.to_string(),
        }
    })?;

    for (value, field) in [
        (&raw.issuer, "issuer"),
        (&raw.client_id, "client_id"),
        (&raw.client_secret, "client_secret"),
    ] {
        if value.trim().is_empty() {
            return Err(ConsoleOidcBootstrapParseError::EmptyField { field });
        }
    }

    Ok(raw.into())
}

/// Errors from applying the bootstrap file at a given path: reading it,
/// parsing its contents, or persisting the parsed configuration.
#[derive(Debug, Error)]
pub enum ConsoleOidcBootstrapError {
    #[error("could not read Cloud console-access OIDC bootstrap file {path}: {reason}")]
    Read { path: PathBuf, reason: String },
    #[error("Cloud console-access OIDC bootstrap file {path} is invalid: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: ConsoleOidcBootstrapParseError,
    },
    #[error("could not apply Cloud console-access OIDC bootstrap file {path}: {reason}")]
    Apply { path: PathBuf, reason: String },
}

/// Outcome of [`apply_console_oidc_bootstrap_file_with`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleOidcBootstrapOutcome {
    /// No bootstrap file was present at the given path. The normal case on
    /// every boot after the first, and on any non-Cloud-hosted instance.
    NotPresent,
    /// The file was parsed, applied, and removed. Carries the issuer and
    /// client id — never the secret — so the caller can audit-log it.
    Applied { issuer: String, client_id: String },
}

/// Read, parse, apply, and delete the Cloud console-access OIDC bootstrap
/// file at `path`, if present.
///
/// `apply` is injected — rather than taking a concrete `CloudService` — so
/// this can be unit-tested without a real database connection.
/// [`crate::service::CloudService::apply_console_oidc_bootstrap_file`] is a
/// thin wrapper passing `CloudService::apply_console_oidc_config`.
///
/// A missing file is not an error: it degrades to
/// [`ConsoleOidcBootstrapOutcome::NotPresent`] at debug-log noise, since that
/// is the expected state on every boot after the first. A file that exists
/// but fails to parse is left in place (never deleted) so an operator can
/// inspect it; a file that parses but fails to apply is also left in place,
/// for the same reason. Only a successful apply deletes the file. If the
/// delete itself then fails, that is logged at error level (the secret is
/// already persisted, encrypted, in the database — but the plaintext file
/// must still be removed by the operator) and the outcome is still
/// `Applied`, since the configuration *was* applied.
pub async fn apply_console_oidc_bootstrap_file_with<Apply, ApplyFut, ApplyErr>(
    path: &Path,
    apply: Apply,
) -> Result<ConsoleOidcBootstrapOutcome, ConsoleOidcBootstrapError>
where
    Apply: FnOnce(ManagedCloudOidcConfig) -> ApplyFut,
    ApplyFut: std::future::Future<Output = Result<(), ApplyErr>>,
    ApplyErr: std::fmt::Display,
{
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(
                path = %path.display(),
                "no Cloud console-access OIDC bootstrap file present"
            );
            return Ok(ConsoleOidcBootstrapOutcome::NotPresent);
        }
        Err(error) => {
            return Err(ConsoleOidcBootstrapError::Read {
                path: path.to_path_buf(),
                reason: error.to_string(),
            });
        }
    };

    let config = parse_console_oidc_bootstrap_file(&bytes).map_err(|source| {
        ConsoleOidcBootstrapError::Parse {
            path: path.to_path_buf(),
            source,
        }
    })?;

    let issuer = config.issuer.clone();
    let client_id = config.client_id.clone();

    apply(config)
        .await
        .map_err(|error| ConsoleOidcBootstrapError::Apply {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;

    if let Err(error) = tokio::fs::remove_file(path).await {
        tracing::error!(
            path = %path.display(),
            %error,
            "applied the Cloud console-access OIDC bootstrap file but could not remove it \
             afterwards; the configuration is now stored, encrypted, in the database, but the \
             plaintext client secret is still on disk at this path -- delete it manually"
        );
    }

    Ok(ConsoleOidcBootstrapOutcome::Applied { issuer, client_id })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn valid_json() -> &'static str {
        r#"{"issuer":"https://cloud.example.com","client_id":"abc123","client_secret":"shh"}"#
    }

    #[test]
    fn parse_valid_file_succeeds() {
        let config = parse_console_oidc_bootstrap_file(valid_json().as_bytes())
            .expect("valid bootstrap file must parse");
        assert_eq!(config.issuer, "https://cloud.example.com");
        assert_eq!(config.client_id, "abc123");
        assert_eq!(config.client_secret, "shh");
    }

    #[test]
    fn parse_ignores_unknown_fields() {
        let json = r#"{
            "issuer":"https://cloud.example.com",
            "client_id":"abc123",
            "client_secret":"shh",
            "future_field":"unused"
        }"#;
        let config = parse_console_oidc_bootstrap_file(json.as_bytes())
            .expect("unknown fields must be ignored, not rejected");
        assert_eq!(config.client_id, "abc123");
    }

    /// `ManagedCloudOidcConfig` deliberately has no `Debug`/`Clone` (it holds
    /// a plaintext client secret), so these tests can't use
    /// `Result::expect_err`/`unwrap_err`, which require `T: Debug`. Assert on
    /// the `Result` directly instead.
    #[test]
    fn parse_missing_field_fails() {
        let json = r#"{"issuer":"https://cloud.example.com","client_id":"abc123"}"#;
        let Err(error) = parse_console_oidc_bootstrap_file(json.as_bytes()) else {
            panic!("missing client_secret must fail to parse");
        };
        assert!(matches!(
            error,
            ConsoleOidcBootstrapParseError::InvalidJson { .. }
        ));
    }

    #[test]
    fn parse_empty_field_fails() {
        let json = r#"{"issuer":"","client_id":"abc123","client_secret":"shh"}"#;
        let Err(error) = parse_console_oidc_bootstrap_file(json.as_bytes()) else {
            panic!("blank issuer must be rejected");
        };
        assert_eq!(
            error,
            ConsoleOidcBootstrapParseError::EmptyField { field: "issuer" }
        );
    }

    #[test]
    fn parse_bad_json_fails() {
        let Err(error) = parse_console_oidc_bootstrap_file(b"not json") else {
            panic!("malformed JSON must be rejected");
        };
        assert!(matches!(
            error,
            ConsoleOidcBootstrapParseError::InvalidJson { .. }
        ));
    }

    #[tokio::test]
    async fn file_not_present_returns_not_present_without_error() {
        let dir = tempfile::tempdir().expect("tempdir must be creatable");
        let path = dir.path().join(CONSOLE_OIDC_BOOTSTRAP_FILENAME);

        let outcome =
            apply_console_oidc_bootstrap_file_with(&path, |_config| async { Ok::<_, String>(()) })
                .await
                .expect("a missing file must not be an error");

        assert_eq!(outcome, ConsoleOidcBootstrapOutcome::NotPresent);
    }

    #[tokio::test]
    async fn file_present_is_applied_and_removed() {
        let dir = tempfile::tempdir().expect("tempdir must be creatable");
        let path = dir.path().join(CONSOLE_OIDC_BOOTSTRAP_FILENAME);
        tokio::fs::write(&path, valid_json())
            .await
            .expect("writing the fixture file must succeed");

        let applied_with: Arc<std::sync::Mutex<Option<ManagedCloudOidcConfig>>> =
            Arc::new(std::sync::Mutex::new(None));
        let recorder = applied_with.clone();

        let outcome = apply_console_oidc_bootstrap_file_with(&path, move |config| {
            let recorder = recorder.clone();
            async move {
                *recorder.lock().expect("mutex must not be poisoned") = Some(config);
                Ok::<_, String>(())
            }
        })
        .await
        .expect("a valid file must apply successfully");

        assert_eq!(
            outcome,
            ConsoleOidcBootstrapOutcome::Applied {
                issuer: "https://cloud.example.com".to_string(),
                client_id: "abc123".to_string(),
            }
        );
        assert!(
            !path.exists(),
            "the file must be removed after a successful apply"
        );
        let applied = applied_with
            .lock()
            .expect("mutex must not be poisoned")
            .take()
            .expect("apply must have been called");
        assert_eq!(applied.client_secret, "shh");
    }

    #[tokio::test]
    async fn unparsable_file_is_left_in_place_with_a_clear_error() {
        let dir = tempfile::tempdir().expect("tempdir must be creatable");
        let path = dir.path().join(CONSOLE_OIDC_BOOTSTRAP_FILENAME);
        tokio::fs::write(&path, b"not json")
            .await
            .expect("writing the fixture file must succeed");

        let calls = Arc::new(AtomicUsize::new(0));
        let call_counter = calls.clone();
        let error = apply_console_oidc_bootstrap_file_with(&path, move |_config| {
            call_counter.fetch_add(1, Ordering::SeqCst);
            async { Ok::<_, String>(()) }
        })
        .await
        .expect_err("malformed JSON must return an error");

        assert!(matches!(error, ConsoleOidcBootstrapError::Parse { .. }));
        assert!(error.to_string().contains(&path.display().to_string()));
        assert!(
            path.exists(),
            "an unparsable file must be left in place for the operator to inspect"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "apply must never be called when parsing fails"
        );
    }

    #[tokio::test]
    async fn apply_failure_leaves_file_in_place() {
        let dir = tempfile::tempdir().expect("tempdir must be creatable");
        let path = dir.path().join(CONSOLE_OIDC_BOOTSTRAP_FILENAME);
        tokio::fs::write(&path, valid_json())
            .await
            .expect("writing the fixture file must succeed");

        let error = apply_console_oidc_bootstrap_file_with(&path, |_config| async {
            Err::<(), _>("upstream rejected the issuer".to_string())
        })
        .await
        .expect_err("a failing apply must surface as an error");

        assert!(matches!(error, ConsoleOidcBootstrapError::Apply { .. }));
        assert!(
            path.exists(),
            "the file must be left in place when apply fails"
        );
    }
}
