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
use temps_cloud_client::CloudFeatureSwitches;

use crate::service::CloudServiceError;

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
    Apply {
        path: PathBuf,
        reason: String,
        /// What the failed apply did to remote-console consent, for auditing.
        consent: ConsentTrace,
    },
}

/// Whether applying the bootstrap file changed remote-console consent
/// (`cloud.console_access_enabled`). ADR-045 Amendment 1: on a Cloud-hosted
/// plane the file's presence is the operator's consent, so applying it turns
/// the switch on -- and that change must be reported so it can be audited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleAccessConsent {
    /// The switch was already on; nothing about consent changed.
    AlreadyEnabled,
    /// The bootstrap turned the switch on. Carries the feature switches
    /// before and after so the audit record shows exactly what changed.
    GrantedByBootstrap {
        previous: CloudFeatureSwitches,
        new: CloudFeatureSwitches,
    },
}

impl ConsoleAccessConsent {
    pub fn was_granted(&self) -> bool {
        matches!(self, Self::GrantedByBootstrap { .. })
    }
}

/// What a *failed* apply did to remote-console consent. A failure that never
/// reached the consent write leaves it [`Self::Untouched`]; one that did must be
/// audited even though the bootstrap as a whole failed, because the
/// `cloud.console_access_enabled` row may have been written (and possibly
/// not reverted) before the error surfaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsentTrace {
    /// The consent write was never attempted.
    Untouched,
    /// Turning consent on was attempted and failed. The service then tried to
    /// put things back: `restored` says whether `console_access_enabled=false`
    /// was durably written again, `provider_revoked` whether the managed
    /// provider created moments earlier was removed again. Either being
    /// `false` means an operator has something to check.
    WriteFailed {
        previous: CloudFeatureSwitches,
        restored: bool,
        provider_revoked: bool,
    },
}

/// Why [`crate::service::CloudService::apply_bootstrap_console_oidc_config`]
/// failed, together with what it did to consent on the way.
#[derive(Debug, Error)]
#[error("{error}")]
pub struct ConsoleOidcApplyFailure {
    #[source]
    pub error: CloudServiceError,
    pub consent: ConsentTrace,
}

impl ConsoleOidcApplyFailure {
    /// A failure that happened before any consent write.
    pub fn untouched(error: CloudServiceError) -> Self {
        Self {
            error,
            consent: ConsentTrace::Untouched,
        }
    }
}

/// What the bootstrap file asked for, and did, about telemetry export
/// (`cloud.telemetry_enabled`). A Cloud-hosted plane exists to keep its logs,
/// traces and metrics in Cloud, so Temps Cloud writes `"telemetry_enabled":
/// true` into the file it drops on create and on every upgrade; like console
/// access (ADR-045 Amendment 1), the file's presence is the operator's consent.
/// Files without the field -- and every self-hosted instance, which never gets
/// a file -- leave telemetry exactly as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelemetryConsent {
    /// The file did not ask for telemetry export.
    NotRequested,
    /// The file asked for it; not yet applied. Only
    /// [`apply_console_oidc_bootstrap_file_with`] returns this --
    /// [`crate::service::CloudService::apply_console_oidc_bootstrap_file`]
    /// resolves it into one of the outcomes below.
    Requested,
    /// The switch was already on; nothing changed.
    AlreadyEnabled,
    /// An earlier boot already applied this consent. The file is re-dropped
    /// on every upgrade, so this is the normal case after the first one; the
    /// operator's current choice stands, even if it is "off".
    AlreadyDecided,
    /// The bootstrap turned telemetry export on.
    GrantedByBootstrap {
        previous: CloudFeatureSwitches,
        new: CloudFeatureSwitches,
    },
    /// Turning it on failed. The console-access part of the bootstrap still
    /// applied; telemetry stays off until an operator enables it.
    Failed { reason: String },
}

#[derive(Deserialize)]
struct RawTelemetryRequest {
    #[serde(default)]
    telemetry_enabled: bool,
}

/// Whether the bootstrap file's bytes ask for telemetry export. Anything
/// unreadable counts as "no": consent is never inferred from a malformed file.
pub fn bootstrap_requests_telemetry(bytes: &[u8]) -> bool {
    serde_json::from_slice::<RawTelemetryRequest>(bytes)
        .map(|raw| raw.telemetry_enabled)
        .unwrap_or(false)
}

/// Outcome of [`apply_console_oidc_bootstrap_file_with`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleOidcBootstrapOutcome {
    /// No bootstrap file was present at the given path. The normal case on
    /// every boot after the first, and on any non-Cloud-hosted instance.
    NotPresent,
    /// The file was parsed, applied, and removed. Carries the issuer and
    /// client id — never the secret — so the caller can audit-log it, and
    /// whether applying it also granted console-access consent.
    Applied {
        issuer: String,
        client_id: String,
        consent: ConsoleAccessConsent,
        telemetry: TelemetryConsent,
    },
}

/// Read, parse, apply, and delete the Cloud console-access OIDC bootstrap
/// file at `path`, if present.
///
/// `apply` is injected — rather than taking a concrete `CloudService` — so
/// this can be unit-tested without a real database connection.
/// [`crate::service::CloudService::apply_console_oidc_bootstrap_file`] is a
/// thin wrapper passing `CloudService::apply_bootstrap_console_oidc_config`,
/// which (unlike the per-connect `apply_console_oidc_config`) grants
/// console-access consent and does not require the link to be established --
/// see ADR-045 Amendment 1.
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
pub async fn apply_console_oidc_bootstrap_file_with<Apply, ApplyFut>(
    path: &Path,
    apply: Apply,
) -> Result<ConsoleOidcBootstrapOutcome, ConsoleOidcBootstrapError>
where
    Apply: FnOnce(ManagedCloudOidcConfig) -> ApplyFut,
    ApplyFut: std::future::Future<Output = Result<ConsoleAccessConsent, ConsoleOidcApplyFailure>>,
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
    let telemetry = if bootstrap_requests_telemetry(&bytes) {
        TelemetryConsent::Requested
    } else {
        TelemetryConsent::NotRequested
    };

    let consent = apply(config)
        .await
        .map_err(|failure| ConsoleOidcBootstrapError::Apply {
            path: path.to_path_buf(),
            reason: failure.error.to_string(),
            consent: failure.consent,
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

    Ok(ConsoleOidcBootstrapOutcome::Applied {
        issuer,
        client_id,
        consent,
        telemetry,
    })
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
    fn telemetry_request_is_read_only_from_an_explicit_true() {
        assert!(!bootstrap_requests_telemetry(valid_json().as_bytes()));
        assert!(bootstrap_requests_telemetry(
            br#"{"issuer":"https://c","client_id":"a","client_secret":"s","telemetry_enabled":true}"#
        ));
        assert!(!bootstrap_requests_telemetry(
            br#"{"issuer":"https://c","client_id":"a","client_secret":"s","telemetry_enabled":false}"#
        ));
        assert!(!bootstrap_requests_telemetry(b"not json"));
        assert!(!bootstrap_requests_telemetry(
            br#"{"telemetry_enabled":"yes"}"#
        ));
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

        let outcome = apply_console_oidc_bootstrap_file_with(&path, |_config| async {
            Ok::<_, ConsoleOidcApplyFailure>(ConsoleAccessConsent::AlreadyEnabled)
        })
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
                Ok::<_, ConsoleOidcApplyFailure>(ConsoleAccessConsent::AlreadyEnabled)
            }
        })
        .await
        .expect("a valid file must apply successfully");

        assert_eq!(
            outcome,
            ConsoleOidcBootstrapOutcome::Applied {
                issuer: "https://cloud.example.com".to_string(),
                client_id: "abc123".to_string(),
                consent: ConsoleAccessConsent::AlreadyEnabled,
                telemetry: TelemetryConsent::NotRequested,
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
            async { Ok::<_, ConsoleOidcApplyFailure>(ConsoleAccessConsent::AlreadyEnabled) }
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
            Err::<ConsoleAccessConsent, _>(ConsoleOidcApplyFailure {
                error: CloudServiceError::ConsoleOidcUnavailable,
                consent: ConsentTrace::WriteFailed {
                    previous: CloudFeatureSwitches::default(),
                    restored: false,
                    provider_revoked: true,
                },
            })
        })
        .await
        .expect_err("a failing apply must surface as an error");

        // The consent trace of the failed apply is carried through so the
        // caller can audit it.
        assert!(matches!(
            error,
            ConsoleOidcBootstrapError::Apply {
                consent: ConsentTrace::WriteFailed {
                    restored: false,
                    provider_revoked: true,
                    ..
                },
                ..
            }
        ));
        assert!(
            path.exists(),
            "the file must be left in place when apply fails"
        );
    }

    #[tokio::test]
    async fn consent_granted_by_apply_is_reported_in_the_outcome() {
        let dir = tempfile::tempdir().expect("tempdir must be creatable");
        let path = dir.path().join(CONSOLE_OIDC_BOOTSTRAP_FILENAME);
        tokio::fs::write(&path, valid_json())
            .await
            .expect("writing the fixture file must succeed");
        let granted = ConsoleAccessConsent::GrantedByBootstrap {
            previous: CloudFeatureSwitches::default(),
            new: CloudFeatureSwitches {
                console_access: true,
                ..CloudFeatureSwitches::default()
            },
        };
        let reported = granted.clone();

        let outcome = apply_console_oidc_bootstrap_file_with(&path, move |_config| async move {
            Ok::<_, ConsoleOidcApplyFailure>(reported)
        })
        .await
        .expect("a valid file must apply successfully");

        let ConsoleOidcBootstrapOutcome::Applied { consent, .. } = outcome else {
            panic!("expected Applied");
        };
        assert_eq!(consent, granted);
        assert!(consent.was_granted());
        assert!(!ConsoleAccessConsent::AlreadyEnabled.was_granted());
    }
}
