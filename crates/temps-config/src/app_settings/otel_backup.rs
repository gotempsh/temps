// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! OpenTelemetry ingest and backup executor settings.

use super::*;

// ── OpenTelemetry ingest settings ───────────────────────────────────

/// Default OTLP ingest requests allowed per project per rate-limit window.
pub const DEFAULT_OTEL_RATE_LIMIT_REQUESTS: u32 = 1000;
/// Default OTLP ingest rate-limit window, in seconds.
pub const DEFAULT_OTEL_RATE_LIMIT_WINDOW_SECS: u64 = 60;
/// Default key prefix for OTel logs archived to S3.
pub const DEFAULT_OTEL_S3_ARCHIVE_PREFIX: &str = "otel-logs";

/// What went wrong handling the encrypted OTel S3 archive credentials.
///
/// `reason` comes from `EncryptionService`, which reports cipher/encoding
/// problems and never echoes its input, so no variant can carry key material.
#[derive(Debug, thiserror::Error)]
pub enum OtelIngestSettingsError {
    #[error("Failed to encrypt the OTel S3 archive {field} before storing it: {reason}")]
    EncryptCredential { field: &'static str, reason: String },

    #[error(
        "Failed to decrypt the stored OTel S3 archive {field} (it may have been encrypted with a \
         different server encryption key; re-enter it in Settings): {reason}"
    )]
    DecryptCredential { field: &'static str, reason: String },
}

/// OTLP ingest behaviour: per-project rate limits and storage quota, the
/// background analysis jobs, and optional S3 archival of OTel logs.
///
/// Replaces the `TEMPS_OTEL_*` environment variables (except the host memory
/// knob `TEMPS_OTEL_MAX_CONCURRENT_INGEST_REQUESTS`). Read once when the OTel
/// plugin starts, so a change applies at the next restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct OtelIngestSettings {
    /// OTLP requests a project may send per window before ingest answers 429.
    #[schema(minimum = 1, example = 1000)]
    pub rate_limit_requests: u32,
    /// Length of the rate-limit window, in seconds.
    #[schema(minimum = 1, example = 60)]
    pub rate_limit_window_secs: u64,
    /// Per-project OTel storage quota in GiB. `None` (or `0`) disables quotas,
    /// which also skips the per-project usage estimate on every ingest.
    pub quota_gb_per_project: Option<u64>,
    /// Run the background service-health computation.
    pub enable_health_compute: bool,
    /// Run the background metric anomaly detector.
    pub enable_anomaly_detection: bool,
    /// Optional archival of OTel logs to an S3-compatible bucket.
    pub s3_archive: OtelS3ArchiveSettings,
}

impl Default for OtelIngestSettings {
    fn default() -> Self {
        Self {
            rate_limit_requests: DEFAULT_OTEL_RATE_LIMIT_REQUESTS,
            rate_limit_window_secs: DEFAULT_OTEL_RATE_LIMIT_WINDOW_SECS,
            quota_gb_per_project: None,
            enable_health_compute: true,
            enable_anomaly_detection: true,
            s3_archive: OtelS3ArchiveSettings::default(),
        }
    }
}

impl OtelIngestSettings {
    /// The per-project quota in bytes, `None` when quotas are disabled
    /// (unset or `0`). Saturates instead of overflowing on absurd values.
    pub fn quota_bytes_per_project(&self) -> Option<u64> {
        self.quota_gb_per_project
            .filter(|gb| *gb > 0)
            .map(|gb| gb.saturating_mul(1024 * 1024 * 1024))
    }
}

/// S3-compatible bucket OTel logs are archived to.
///
/// The access key and secret key are stored as ciphertext only. The plaintext
/// fields are write-only input from a settings save: never serialized, so
/// they can never be persisted or returned, and
/// [`OtelS3ArchiveSettings::apply_credential_update`] consumes them. They use
/// `skip_serializing_if` rather than `skip_serializing` so the OpenAPI schema
/// still lists them as write-only inputs; the stored ciphertext is left out
/// of it, since a client can neither read nor set it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct OtelS3ArchiveSettings {
    /// Bucket region. Archival is off unless region, bucket and both
    /// credentials are configured.
    pub region: Option<String>,
    /// Custom endpoint for S3-compatible stores (MinIO, R2, ...).
    pub endpoint: Option<String>,
    pub bucket: Option<String>,
    /// Key prefix archived objects are written under.
    pub prefix: String,

    /// Plaintext access key, accepted on a settings write only. Blank or
    /// absent preserves the stored key.
    #[serde(default, skip_serializing_if = "never_serialized")]
    #[schema(write_only)]
    pub access_key: Option<String>,
    /// Plaintext secret key, accepted on a settings write only. Blank or
    /// absent preserves the stored key.
    #[serde(default, skip_serializing_if = "never_serialized")]
    #[schema(write_only)]
    pub secret_key: Option<String>,
    /// Remove both stored credentials, which turns archival off.
    #[serde(default, skip_serializing_if = "never_serialized")]
    #[schema(write_only)]
    pub clear_credentials: bool,

    /// AES-256-GCM ciphertext of the access key. Never returned by the API.
    #[schema(ignore)]
    pub access_key_encrypted: Option<String>,
    /// AES-256-GCM ciphertext of the secret key. Never returned by the API.
    #[schema(ignore)]
    pub secret_key_encrypted: Option<String>,
}

/// `skip_serializing_if` predicate for write-only input fields.
fn never_serialized<T>(_: &T) -> bool {
    true
}

impl Default for OtelS3ArchiveSettings {
    fn default() -> Self {
        Self {
            region: None,
            endpoint: None,
            bucket: None,
            prefix: DEFAULT_OTEL_S3_ARCHIVE_PREFIX.to_string(),
            access_key: None,
            secret_key: None,
            clear_credentials: false,
            access_key_encrypted: None,
            secret_key_encrypted: None,
        }
    }
}

/// `Debug` reports whether credentials are stored, never ciphertext or
/// plaintext, so an accidental `{:?}` cannot put key material in a log line.
impl std::fmt::Debug for OtelS3ArchiveSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OtelS3ArchiveSettings")
            .field("region", &self.region)
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .field("access_key_configured", &self.access_key_configured())
            .field("secret_key_configured", &self.secret_key_configured())
            .finish()
    }
}

/// Decrypted OTel S3 archive credentials. Held in memory only for building
/// the S3 client; `Debug` is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct OtelS3Credentials {
    pub access_key: String,
    pub secret_key: String,
}

impl std::fmt::Debug for OtelS3Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OtelS3Credentials")
            .field("access_key", &"[REDACTED]")
            .field("secret_key", &"[REDACTED]")
            .finish()
    }
}

fn non_empty_ciphertext(value: &Option<String>) -> bool {
    value.as_deref().is_some_and(|c| !c.is_empty())
}

impl OtelS3ArchiveSettings {
    pub fn access_key_configured(&self) -> bool {
        non_empty_ciphertext(&self.access_key_encrypted)
    }

    pub fn secret_key_configured(&self) -> bool {
        non_empty_ciphertext(&self.secret_key_encrypted)
    }

    /// Whether archival has everything it needs: region, bucket and both
    /// credentials (the same rule the environment variables followed).
    pub fn is_configured(&self) -> bool {
        self.region.as_deref().is_some_and(|r| !r.trim().is_empty())
            && self.bucket.as_deref().is_some_and(|b| !b.trim().is_empty())
            && self.access_key_configured()
            && self.secret_key_configured()
    }

    /// Resolve the write-only credential fields against what is stored,
    /// encrypting newly submitted values.
    ///
    /// A non-empty plaintext replaces the stored value, blank or absent
    /// preserves it, and `clear_credentials` removes both. Client-supplied
    /// ciphertext is never trusted: it is always replaced by one of those
    /// three outcomes. The plaintext fields and the clear flag are consumed.
    pub fn apply_credential_update(
        &mut self,
        current: &OtelS3ArchiveSettings,
        encryption: &EncryptionService,
    ) -> Result<(), OtelIngestSettingsError> {
        let clear = std::mem::take(&mut self.clear_credentials);
        let access = take_submitted(&mut self.access_key);
        let secret = take_submitted(&mut self.secret_key);

        self.access_key_encrypted = resolve_credential(
            "access key",
            access,
            clear,
            &current.access_key_encrypted,
            encryption,
        )?;
        self.secret_key_encrypted = resolve_credential(
            "secret key",
            secret,
            clear,
            &current.secret_key_encrypted,
            encryption,
        )?;
        Ok(())
    }

    /// Decrypt both credentials for the one caller that needs them: the OTel
    /// plugin building its S3 client. `None` when either is not stored.
    pub fn decrypt_credentials(
        &self,
        encryption: &EncryptionService,
    ) -> Result<Option<OtelS3Credentials>, OtelIngestSettingsError> {
        let (Some(access), Some(secret)) = (
            self.access_key_encrypted
                .as_deref()
                .filter(|c| !c.is_empty()),
            self.secret_key_encrypted
                .as_deref()
                .filter(|c| !c.is_empty()),
        ) else {
            return Ok(None);
        };
        let decrypt = |field: &'static str, ciphertext: &str| {
            encryption.decrypt_string(ciphertext).map_err(|e| {
                OtelIngestSettingsError::DecryptCredential {
                    field,
                    reason: e.to_string(),
                }
            })
        };
        Ok(Some(OtelS3Credentials {
            access_key: decrypt("access key", access)?,
            secret_key: decrypt("secret key", secret)?,
        }))
    }
}

fn take_submitted(field: &mut Option<String>) -> Option<String> {
    field
        .take()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn resolve_credential(
    field: &'static str,
    submitted: Option<String>,
    clear: bool,
    stored: &Option<String>,
    encryption: &EncryptionService,
) -> Result<Option<String>, OtelIngestSettingsError> {
    match (submitted, clear) {
        // A real value wins over a stale clear flag from a form that sent both.
        (Some(plaintext), _) => encryption
            .encrypt_string(&plaintext)
            .map(Some)
            .map_err(|e| OtelIngestSettingsError::EncryptCredential {
                field,
                reason: e.to_string(),
            }),
        (None, true) => Ok(None),
        (None, false) => Ok(stored.clone()),
    }
}

// ── Backup executor settings ────────────────────────────────────────

/// Default number of backup jobs the executor runs at once.
pub const DEFAULT_BACKUP_EXECUTOR_MAX_CONCURRENT: u32 = 4;
/// Upper bound for [`BackupExecutorSettings::max_concurrent`]. Every running
/// backup holds a dump/upload stream; past this a small host thrashes.
pub const MAX_BACKUP_EXECUTOR_MAX_CONCURRENT: u32 = 64;

/// Backup executor tuning. Replaces `TEMPS_BACKUP_EXECUTOR_MAX_CONCURRENT`.
/// Read when the backup plugin starts, so a change applies at the next restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct BackupExecutorSettings {
    /// Backup jobs run concurrently on this control plane.
    #[schema(minimum = 1, maximum = 64, example = 4)]
    pub max_concurrent: u32,
}

impl Default for BackupExecutorSettings {
    fn default() -> Self {
        Self {
            max_concurrent: DEFAULT_BACKUP_EXECUTOR_MAX_CONCURRENT,
        }
    }
}

impl BackupExecutorSettings {
    /// Concurrency actually applied: a stored `0` (which would stall every
    /// backup forever) or an out-of-range value is clamped.
    pub fn effective_max_concurrent(&self) -> usize {
        // u32 -> usize is lossless on every supported (32/64-bit) target.
        self.max_concurrent
            .clamp(1, MAX_BACKUP_EXECUTOR_MAX_CONCURRENT) as usize
    }
}

#[cfg(test)]
mod otel_and_backup_settings_tests {
    use super::*;

    fn encryption() -> EncryptionService {
        EncryptionService::new("0000000000000000000000000000000000000000000000000000000000000000")
            .expect("test key")
    }

    #[test]
    fn defaults_match_the_former_environment_defaults() {
        let otel = OtelIngestSettings::default();
        assert_eq!(otel.rate_limit_requests, 1000);
        assert_eq!(otel.rate_limit_window_secs, 60);
        assert_eq!(otel.quota_bytes_per_project(), None);
        assert!(otel.enable_health_compute);
        assert!(otel.enable_anomaly_detection);
        assert_eq!(otel.s3_archive.prefix, "otel-logs");
        assert!(!otel.s3_archive.is_configured());
        assert_eq!(
            BackupExecutorSettings::default().effective_max_concurrent(),
            4
        );
    }

    #[test]
    fn a_settings_row_without_the_new_sections_reads_the_defaults() {
        let parsed = AppSettings::from_json(serde_json::json!({"preview_domain": "x.test"}));
        assert_eq!(parsed.otel_ingest, OtelIngestSettings::default());
        assert_eq!(parsed.backup_executor, BackupExecutorSettings::default());
    }

    #[test]
    fn new_sections_round_trip() {
        let mut settings = AppSettings::default();
        settings.otel_ingest.rate_limit_requests = 50;
        settings.otel_ingest.quota_gb_per_project = Some(2);
        settings.otel_ingest.enable_anomaly_detection = false;
        settings.otel_ingest.s3_archive.bucket = Some("archive".into());
        settings.backup_executor.max_concurrent = 9;
        let restored = AppSettings::from_json(settings.to_json());
        assert_eq!(restored.otel_ingest, settings.otel_ingest);
        assert_eq!(restored.backup_executor.max_concurrent, 9);
        assert_eq!(
            restored.otel_ingest.quota_bytes_per_project(),
            Some(2 * 1024 * 1024 * 1024)
        );
    }

    #[test]
    fn zero_quota_means_disabled() {
        let otel = OtelIngestSettings {
            quota_gb_per_project: Some(0),
            ..Default::default()
        };
        assert_eq!(otel.quota_bytes_per_project(), None);
    }

    #[test]
    fn backup_concurrency_is_clamped() {
        let zero = BackupExecutorSettings { max_concurrent: 0 };
        assert_eq!(zero.effective_max_concurrent(), 1);
        let huge = BackupExecutorSettings {
            max_concurrent: 10_000,
        };
        assert_eq!(huge.effective_max_concurrent(), 64);
    }

    #[test]
    fn plaintext_credentials_are_never_serialized() {
        let archive = OtelS3ArchiveSettings {
            access_key: Some("AKIAPLAINTEXT".into()),
            secret_key: Some("plain-secret".into()),
            ..Default::default()
        };
        let json = serde_json::to_string(&archive).expect("serializes");
        assert!(!json.contains("AKIAPLAINTEXT"));
        assert!(!json.contains("plain-secret"));
        assert!(!json.contains("\"access_key\""));
        assert!(!json.contains("\"secret_key\""));
        assert!(!json.contains("clear_credentials"));
        let debug = format!("{archive:?}");
        assert!(!debug.contains("AKIAPLAINTEXT"));
        assert!(!debug.contains("plain-secret"));
    }

    #[test]
    fn schema_lists_the_write_only_inputs_and_not_the_ciphertext() {
        let schema =
            serde_json::to_value(<OtelS3ArchiveSettings as utoipa::PartialSchema>::schema())
                .expect("schema serializes");
        let properties = schema["properties"].as_object().expect("properties");
        for input in ["access_key", "secret_key", "clear_credentials"] {
            assert_eq!(
                properties[input]["writeOnly"],
                serde_json::json!(true),
                "{input} must be a documented write-only input"
            );
        }
        assert!(!properties.contains_key("access_key_encrypted"));
        assert!(!properties.contains_key("secret_key_encrypted"));
    }

    #[test]
    fn submitted_credentials_are_encrypted_and_decrypt_back() {
        let encryption = encryption();
        let mut incoming = OtelS3ArchiveSettings {
            access_key: Some(" AKIA123 ".into()),
            secret_key: Some("secret-456".into()),
            ..Default::default()
        };
        incoming
            .apply_credential_update(&OtelS3ArchiveSettings::default(), &encryption)
            .expect("encrypts");
        assert_eq!(incoming.access_key, None);
        assert_eq!(incoming.secret_key, None);
        let stored = incoming.access_key_encrypted.clone().expect("ciphertext");
        assert!(!stored.contains("AKIA123"));
        let credentials = incoming
            .decrypt_credentials(&encryption)
            .expect("decrypts")
            .expect("both stored");
        assert_eq!(credentials.access_key, "AKIA123");
        assert_eq!(credentials.secret_key, "secret-456");
        assert!(!format!("{credentials:?}").contains("secret-456"));
    }

    #[test]
    fn blank_submission_preserves_and_clear_removes_stored_credentials() {
        let encryption = encryption();
        let current = OtelS3ArchiveSettings {
            access_key_encrypted: Some("stored-access".into()),
            secret_key_encrypted: Some("stored-secret".into()),
            ..Default::default()
        };

        // A client echoing (or forging) ciphertext never overrides storage.
        let mut round_trip = OtelS3ArchiveSettings {
            access_key: Some("   ".into()),
            access_key_encrypted: Some("forged".into()),
            ..Default::default()
        };
        round_trip
            .apply_credential_update(&current, &encryption)
            .expect("no encryption needed");
        assert_eq!(
            round_trip.access_key_encrypted.as_deref(),
            Some("stored-access")
        );
        assert_eq!(
            round_trip.secret_key_encrypted.as_deref(),
            Some("stored-secret")
        );

        let mut cleared = OtelS3ArchiveSettings {
            clear_credentials: true,
            ..Default::default()
        };
        cleared
            .apply_credential_update(&current, &encryption)
            .expect("no encryption needed");
        assert_eq!(cleared.access_key_encrypted, None);
        assert_eq!(cleared.secret_key_encrypted, None);
        assert!(!cleared.clear_credentials);
    }

    #[test]
    fn archive_needs_region_bucket_and_both_credentials() {
        let mut archive = OtelS3ArchiveSettings {
            region: Some("us-east-1".into()),
            bucket: Some("b".into()),
            access_key_encrypted: Some("a".into()),
            ..Default::default()
        };
        assert!(!archive.is_configured());
        archive.secret_key_encrypted = Some("s".into());
        assert!(archive.is_configured());
        archive.region = None;
        assert!(!archive.is_configured());
    }
}
