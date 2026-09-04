// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Optional client linking a self-hosted Temps instance to a managed backend.
//!
//! # The rule this crate exists to keep
//!
//! **Local is primary. The managed backend is a mirror.** Nothing here may
//! block, slow, or fail the instance's own work. If the backend is down,
//! unreachable, unpaid or misconfigured, the instance keeps deploying, keeps
//! serving and keeps storing telemetry locally — it simply buffers what it
//! would have mirrored, and says so.
//!
//! Every operation therefore either succeeds, or degrades to a *reported*
//! state. There is no path where the instance is worse off than if it had
//! never connected.
//!
//! # What leaves the machine
//!
//! Only what is in [`temps_cloud_protocol`]: telemetry batches, heartbeats and
//! enrollment. No source, no environment variables, no secrets. An operator can
//! read the protocol crate and know exactly what is sent.

#![forbid(unsafe_code)]

pub mod flusher;
/// The liveness signal on the dedicated management channel -- see the module
/// docs for why this is separate from telemetry shipment and backup mirroring.
pub mod heartbeat;
/// ADR-043 §5a: the write-side sibling of [`query`]'s Cloud read proxy.
pub mod insert;
pub mod link;
pub mod outbox;
pub mod outbox_worker;
pub mod query;
pub mod spool;
pub mod state;
pub mod status;

pub use link::{
    CloudFallbackReason, CloudLink, CloudTelemetryFallback, EnrollmentKind, FlushOutcome,
    OutboxShipOutcome, SubmissionScope, SubmissionScopeBusy,
};
pub use outbox::{
    ClaimedSpan, ClaimedTelemetryRow, DeadLetterSummary, EnqueueOutcome, OutboxStats, SpanOutbox,
    SpanOutboxError, TelemetryOutbox, TelemetryOutboxError, DEAD_LETTER_PAYLOAD_RETENTION,
    OUTBOX_BATCH_SIZE, OUTBOX_MAX_ATTEMPTS,
};
pub use outbox_worker::{DrainObserver, DrainOutcome, OutboxCapSource};
pub use state::EnrollmentState;
pub use status::{LinkStatus, MirrorHealth, TelemetryDurability};

/// Operator-controlled export gates. Linking an account never enables data
/// export; persisted settings must be applied explicitly after startup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CloudFeatureSwitches {
    pub telemetry: bool,
    pub backups: bool,
    pub notifications: bool,
}

use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr},
    path::Path,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

use sha2::{Digest, Sha256};
use temps_cloud_protocol::{
    BackupArtifact, BackupCompleted, BackupTarget, BackupTargetRequest, EnrollRequest,
    EnrollResponse, IngestAck, ManagedAiAnalysisRequest, ManagedAiAnalysisResponse,
    ManagedAiCapability, ManagedAiChatRequest, ManagedAiChatResponse, ManagedBackupCapability,
    ManagedNotificationAccepted, ManagedNotificationRequest, NativeSnapshot, NativeSnapshotRequest,
    SpanRecord, TelemetryBatch, WalGObjectCompleted, WalGObjectTarget, WalGObjectTargetRequest,
    WalGSnapshot, WalGSnapshotCompleted, WalGSnapshotRequest,
};
use temps_core::url_validation::{validate_ipv4, validate_ipv6};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendUrl {
    url: url::Url,
    allow_loopback_development: bool,
}

impl BackendUrl {
    /// Parse a production managed-backend origin.
    ///
    /// The value comes from trusted host configuration, never an HTTP request.
    /// HTTPS is mandatory and credentials, query strings and fragments are
    /// rejected so bearer-token requests cannot be redirected or disguised.
    pub fn production(value: &str) -> Result<Self, CloudError> {
        Self::parse(value, false)
    }

    /// Explicit local-development escape hatch. Only loopback HTTP(S) origins
    /// are accepted; this must never become a general insecure-HTTP toggle.
    pub fn loopback_development(value: &str) -> Result<Self, CloudError> {
        Self::parse(value, true)
    }

    fn parse(value: &str, allow_loopback_http: bool) -> Result<Self, CloudError> {
        let parsed = url::Url::parse(value).map_err(|e| CloudError::InvalidBackendUrl {
            reason: e.to_string(),
        })?;

        if !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(CloudError::InvalidBackendUrl {
                reason: "credentials, query strings and fragments are not allowed".into(),
            });
        }
        if parsed.path() != "/" && !parsed.path().is_empty() {
            return Err(CloudError::InvalidBackendUrl {
                reason: "the backend URL must be an origin without a path".into(),
            });
        }

        let loopback = parsed
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("localhost"))
            || matches!(parsed.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
            || matches!(parsed.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback());

        match parsed.scheme() {
            "https" => {}
            "http" if allow_loopback_http && loopback => {}
            "http" => {
                return Err(CloudError::InvalidBackendUrl {
                    reason: "HTTP is allowed only for an explicit loopback development backend"
                        .into(),
                })
            }
            other => {
                return Err(CloudError::InvalidBackendUrl {
                    reason: format!("unsupported scheme {other:?}; HTTPS is required"),
                })
            }
        }

        let allow_loopback_development =
            allow_loopback_http && parsed.scheme() == "http" && loopback;
        Ok(Self {
            url: parsed,
            allow_loopback_development,
        })
    }

    /// `pub(crate)` so the ClickHouse read-proxy client in [`crate::query`]
    /// builds its URL the same way every other Cloud endpoint does, rather
    /// than string-concatenating a second one.
    pub(crate) fn endpoint(&self, path: &str) -> url::Url {
        let mut endpoint = self.url.clone();
        endpoint.set_path(path);
        endpoint
    }

    pub fn as_str(&self) -> &str {
        self.url.as_str()
    }

    pub fn allows_loopback_development(&self) -> bool {
        self.allow_loopback_development
    }

    fn permits_loopback_uploads(&self) -> bool {
        self.url.scheme() == "http" && is_loopback_host(&self.url)
    }
}

/// How long any single call to the backend may take.
///
/// Deliberately short. This runs alongside the instance's own work, and a slow
/// backend must never become the instance's latency.
///
/// `pub(crate)` so [`crate::query`] bounds reads against the Cloud telemetry
/// proxy with this same number instead of declaring a second one that can drift
/// away from it.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const AI_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const UPLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const UPLOAD_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Error)]
pub enum CloudError {
    #[error("Invalid managed backend URL: {reason}")]
    InvalidBackendUrl { reason: String },

    #[error("Failed to configure the managed-backend HTTP client: {reason}")]
    ClientConfiguration { reason: String },

    #[error("Managed backend returned an unsafe backup upload target: {reason}")]
    InvalidBackupTarget { reason: String },

    #[error("Not linked to an account. Paste an enrollment code to connect one.")]
    NotEnrolled,

    #[error(
        "Cloud link state at {path} is unreadable. Restore the original encryption key, or back up and remove the state file before reconnecting"
    )]
    LinkStateUnreadable { path: String },

    #[error("Managed Cloud outbound operations are blocked: {reason}")]
    ConfigurationBlocked { reason: String },

    #[error("Managed Cloud feature '{feature}' is disabled in local settings")]
    FeatureDisabled { feature: &'static str },

    #[error("Enrollment was refused: {detail}")]
    EnrollmentRefused { detail: String },

    #[error("Credential rejected by the backend — re-enroll this instance")]
    CredentialRejected,

    /// Transient. The caller keeps the batch spooled and tries again.
    #[error("Managed backend unreachable ({reason}); {spooled_bytes} bytes buffered locally")]
    Unreachable { reason: String, spooled_bytes: u64 },

    /// Transient. The destination accepted the connection but neither upload
    /// reads nor a response made progress within the bounded idle window.
    #[error(
        "Backup upload made no progress for {idle_timeout_ms}ms; {spooled_bytes} bytes remain buffered locally"
    )]
    BackupUploadIdleTimeout {
        idle_timeout_ms: u64,
        spooled_bytes: u64,
    },

    #[error("Backend rejected the payload: {detail}")]
    Rejected { detail: String },

    #[error("Backend acknowledgement did not match submission {submission_id}: {detail}")]
    InvalidAcknowledgement { submission_id: Uuid, detail: String },
}

impl CloudError {
    /// Whether retrying the same payload later could succeed.
    ///
    /// Drives the spool: retryable failures keep data, permanent ones must not
    /// buffer forever behind a problem no amount of waiting will fix.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            CloudError::Unreachable { .. }
                | CloudError::BackupUploadIdleTimeout { .. }
                | CloudError::CredentialRejected
                | CloudError::InvalidAcknowledgement { .. }
        )
    }
}

pub struct CloudClient {
    http: reqwest::Client,
    backend: BackendUrl,
    upload_idle_timeout: Duration,
}

impl CloudClient {
    pub fn new(backend: BackendUrl) -> Result<Self, CloudError> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| CloudError::ClientConfiguration {
                reason: e.to_string(),
            })?;
        Ok(Self {
            http,
            backend,
            upload_idle_timeout: UPLOAD_IDLE_TIMEOUT,
        })
    }

    /// Exchange an operator-pasted code for a long-lived instance token.
    pub async fn enroll(
        &self,
        code: &str,
        instance_id: Uuid,
        agent_version: &str,
    ) -> Result<EnrollResponse, CloudError> {
        let res = self
            .http
            .post(self.backend.endpoint("/v1/enroll"))
            .json(&EnrollRequest {
                enrollment_code: code.trim().to_uppercase(),
                instance_id,
                agent_version: agent_version.to_string(),
            })
            .send()
            .await
            .map_err(|e| CloudError::Unreachable {
                reason: e.to_string(),
                spooled_bytes: 0,
            })?;

        if res.status().is_success() {
            return res
                .json::<EnrollResponse>()
                .await
                .map_err(|e| CloudError::EnrollmentRefused {
                    detail: format!("unreadable response: {e}"),
                });
        }

        // Surface the backend's own wording — "this code has expired" is far
        // more useful to a lone operator than "enrollment failed".
        let detail = res
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v["detail"].as_str().map(String::from))
            .unwrap_or_else(|| "no detail provided".into());
        Err(CloudError::EnrollmentRefused { detail })
    }

    /// Revoke an instance credential before removing the local copy.
    pub async fn revoke(&self, token: &str) -> Result<(), CloudError> {
        let res = self
            .http
            .post(self.backend.endpoint("/v1/revoke"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| CloudError::Unreachable {
                reason: e.to_string(),
                spooled_bytes: 0,
            })?;

        let status = res.status();
        if status.is_success() {
            return Ok(());
        }
        match status.as_u16() {
            401 | 403 => Err(CloudError::CredentialRejected),
            429 | 500..=599 => Err(CloudError::Unreachable {
                reason: format!("backend returned {status}"),
                spooled_bytes: 0,
            }),
            _ => {
                let detail = res
                    .json::<serde_json::Value>()
                    .await
                    .ok()
                    .and_then(|value| value["detail"].as_str().map(String::from))
                    .unwrap_or_else(|| format!("backend returned {status}"));
                Err(CloudError::Rejected { detail })
            }
        }
    }

    /// Describe managed inference without reading or uploading local context.
    pub async fn managed_ai_capability(
        &self,
        token: &str,
    ) -> Result<ManagedAiCapability, CloudError> {
        let response = self
            .http
            .get(self.backend.endpoint("/v1/ai/capability"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|error| CloudError::Unreachable {
                reason: error.to_string(),
                spooled_bytes: 0,
            })?;
        decode_managed_response(response).await
    }

    /// Describe (or vend) a managed backup destination for this tenant, without
    /// uploading or reading any local backup bytes.
    ///
    /// The credential may be a *temporary* (STS-style) one, in which case
    /// [`ManagedBackupCapability::session_token`] is populated and must travel
    /// with the access key and secret through every signer and shell-out —
    /// SigV4 rejects the pair on its own. It stays `None` for a long-lived
    /// credential and for any backend that predates the field.
    pub async fn managed_backup_credentials(
        &self,
        token: &str,
    ) -> Result<ManagedBackupCapability, CloudError> {
        let response = self
            .http
            .get(self.backend.endpoint("/v1/backups/managed/capability"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|error| CloudError::Unreachable {
                reason: error.to_string(),
                spooled_bytes: 0,
            })?;
        decode_managed_response(response).await
    }

    /// Submit the exact manifest the operator approved in the OSS AI surface.
    /// Source telemetry is never fetched by Cloud and BYO credentials never use
    /// this path; local BYO continues through the OSS AI gateway directly.
    pub async fn managed_ai_analysis(
        &self,
        token: &str,
        request: &ManagedAiAnalysisRequest,
    ) -> Result<ManagedAiAnalysisResponse, CloudError> {
        let response = self
            .http
            .post(self.backend.endpoint("/v1/ai/analyses"))
            .bearer_auth(token)
            .timeout(AI_REQUEST_TIMEOUT)
            .json(request)
            .send()
            .await
            .map_err(|error| CloudError::Unreachable {
                reason: error.to_string(),
                spooled_bytes: 0,
            })?;
        decode_managed_response(response).await
    }

    /// Proxy one OpenAI-compatible completion. Retries reuse the request id so
    /// Cloud and the upstream provider cannot reserve or charge twice when a
    /// response is lost after either side has committed it.
    pub async fn managed_ai_chat(
        &self,
        token: &str,
        request: &ManagedAiChatRequest,
    ) -> Result<ManagedAiChatResponse, CloudError> {
        let mut last_failure = None;
        for attempt in 0..3 {
            match self
                .http
                .post(self.backend.endpoint("/v1/ai/chat/completions"))
                .bearer_auth(token)
                .timeout(AI_REQUEST_TIMEOUT)
                .json(request)
                .send()
                .await
            {
                Ok(response) if !matches!(response.status().as_u16(), 429 | 500..=599) => {
                    return decode_managed_response(response).await;
                }
                Ok(response) => {
                    last_failure = Some(describe_failed_response(response).await);
                }
                Err(error) => last_failure = Some(error.without_url().to_string()),
            }
            if attempt < 2 {
                tokio::time::sleep(Duration::from_millis(100 * (1 << attempt))).await;
            }
        }
        Err(CloudError::Unreachable {
            reason: last_failure.unwrap_or_else(|| "managed backend returned no response".into()),
            spooled_bytes: 0,
        })
    }

    /// Stream one completed local backup directly to Cloud-owned object
    /// storage. Every retry keeps the same client-generated backup id; target
    /// creation, PUT and completion are therefore safe when a response is lost.
    pub async fn upload_backup_file(
        &self,
        token: &str,
        instance_id: Uuid,
        backup_id: Uuid,
        source: String,
        artifact: BackupArtifact,
        path: &Path,
    ) -> Result<BackupTarget, CloudError> {
        let (bytes, checksum_sha256) = inspect_local_backup(path).await?;
        let request = BackupTargetRequest {
            backup_id,
            instance_id,
            source,
            estimated_bytes: bytes,
            checksum_sha256: checksum_sha256.clone(),
            artifact,
        };
        let target = self.backup_target(token, &request).await?;

        if !target.upload_required {
            return Ok(target);
        }

        let mut last_failure = None;
        for attempt in 0..3 {
            // Revalidate before every retry. A URL that expired during a
            // failed attempt must not be replayed merely because it was valid
            // when the retry loop began.
            let validated = self.validate_backup_upload_target(
                &target.upload_url,
                target.expires_at_millis,
                &target.headers,
            )?;
            let file = tokio::fs::File::open(path)
                .await
                .map_err(|error| CloudError::Rejected {
                    detail: format!("could not reopen backup artifact for upload: {error}"),
                })?;
            match self
                .send_backup_upload_reader(&validated, file, bytes)
                .await
            {
                Ok(response) if response.status().is_success() => {
                    last_failure = None;
                    break;
                }
                Ok(response)
                    if matches!(response.status().as_u16(), 408 | 425 | 429 | 500..=599) =>
                {
                    last_failure = Some(format!("object storage returned {}", response.status()));
                }
                Ok(response) => {
                    return Err(CloudError::Rejected {
                        detail: format!(
                            "object storage rejected the backup with {}",
                            response.status()
                        ),
                    });
                }
                Err(error) => last_failure = Some(error.to_string()),
            }
            if attempt < 2 {
                tokio::time::sleep(Duration::from_millis(100 * (1 << attempt))).await;
            }
        }
        if let Some(reason) = last_failure {
            return Err(CloudError::Unreachable {
                reason: format!("backup upload did not complete after bounded retries: {reason}"),
                spooled_bytes: bytes,
            });
        }

        self.complete_backup(
            token,
            &BackupCompleted {
                backup_id: target.backup_id,
                bytes,
                checksum_sha256,
            },
        )
        .await?;
        Ok(target)
    }

    /// Send one streaming object from a native/WAL-G repository to the
    /// short-lived destination issued by Cloud.
    ///
    /// This is deliberately shared with local-file uploads so repository
    /// mirroring cannot accidentally regain reqwest's redirect-following
    /// default or skip destination/header validation.
    pub async fn upload_backup_object_reader<R>(
        &self,
        target: &WalGObjectTarget,
        reader: R,
        spooled_bytes: u64,
    ) -> Result<reqwest::Response, CloudError>
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
    {
        if !target.upload_required {
            return Err(CloudError::InvalidBackupTarget {
                reason: format!(
                    "object {} in backup {} did not require an upload",
                    target.relative_key, target.backup_id
                ),
            });
        }
        let validated = self.validate_backup_upload_target(
            &target.upload_url,
            target.expires_at_millis,
            &target.headers,
        )?;
        self.send_backup_upload_reader(&validated, reader, spooled_bytes)
            .await
    }

    fn validate_backup_upload_target(
        &self,
        upload_url: &str,
        expires_at_millis: i64,
        headers: &BTreeMap<String, String>,
    ) -> Result<ValidatedBackupUpload, CloudError> {
        validate_backup_upload_target(
            upload_url,
            expires_at_millis,
            headers,
            self.backend.permits_loopback_uploads(),
        )
    }

    async fn send_backup_upload_reader<R>(
        &self,
        target: &ValidatedBackupUpload,
        reader: R,
        spooled_bytes: u64,
    ) -> Result<reqwest::Response, CloudError>
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
    {
        let upload_http = build_pinned_upload_client(target, spooled_bytes).await?;
        let (progress_tx, progress_rx) = tokio::sync::watch::channel(());
        let body =
            reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(UploadProgressReader {
                inner: reader,
                progress: progress_tx,
            }));
        let request = upload_http
            .put(target.url.clone())
            .headers(target.headers.clone())
            .body(body)
            .send();
        tokio::pin!(request);

        tokio::select! {
            response = &mut request => response.map_err(|error| CloudError::Unreachable {
                reason: format!("backup object upload failed: {}", error.without_url()),
                spooled_bytes,
            }),
            () = wait_for_upload_idle(progress_rx, self.upload_idle_timeout) => {
                Err(CloudError::BackupUploadIdleTimeout {
                    idle_timeout_ms: self.upload_idle_timeout.as_millis().try_into().unwrap_or(u64::MAX),
                    spooled_bytes,
                })
            }
        }
    }

    async fn backup_target(
        &self,
        token: &str,
        request: &BackupTargetRequest,
    ) -> Result<BackupTarget, CloudError> {
        self.retry_backup_json("/v1/backups/target", token, request)
            .await
    }

    pub async fn declare_walg_snapshot(
        &self,
        token: &str,
        request: &WalGSnapshotRequest,
    ) -> Result<WalGSnapshot, CloudError> {
        self.retry_backup_json("/v1/backups/walg/snapshots", token, request)
            .await
    }

    pub async fn declare_native_snapshot(
        &self,
        token: &str,
        request: &NativeSnapshotRequest,
    ) -> Result<NativeSnapshot, CloudError> {
        self.retry_backup_json("/v1/backups/native/snapshots", token, request)
            .await
    }

    pub async fn native_object_target(
        &self,
        token: &str,
        request: &WalGObjectTargetRequest,
    ) -> Result<WalGObjectTarget, CloudError> {
        self.retry_backup_json("/v1/backups/native/objects/target", token, request)
            .await
    }

    pub async fn complete_native_object(
        &self,
        token: &str,
        completion: &WalGObjectCompleted,
    ) -> Result<(), CloudError> {
        let _: serde_json::Value = self
            .retry_backup_json("/v1/backups/native/objects/complete", token, completion)
            .await?;
        Ok(())
    }

    pub async fn complete_native_snapshot(
        &self,
        token: &str,
        completion: &WalGSnapshotCompleted,
    ) -> Result<(), CloudError> {
        let _: serde_json::Value = self
            .retry_backup_json("/v1/backups/native/snapshots/complete", token, completion)
            .await?;
        Ok(())
    }

    pub async fn walg_object_target(
        &self,
        token: &str,
        request: &WalGObjectTargetRequest,
    ) -> Result<WalGObjectTarget, CloudError> {
        self.retry_backup_json("/v1/backups/walg/objects/target", token, request)
            .await
    }

    pub async fn complete_walg_object(
        &self,
        token: &str,
        completion: &WalGObjectCompleted,
    ) -> Result<(), CloudError> {
        let _: serde_json::Value = self
            .retry_backup_json("/v1/backups/walg/objects/complete", token, completion)
            .await?;
        Ok(())
    }

    pub async fn complete_walg_snapshot(
        &self,
        token: &str,
        completion: &WalGSnapshotCompleted,
    ) -> Result<(), CloudError> {
        let _: serde_json::Value = self
            .retry_backup_json("/v1/backups/walg/snapshots/complete", token, completion)
            .await?;
        Ok(())
    }

    async fn complete_backup(
        &self,
        token: &str,
        completion: &BackupCompleted,
    ) -> Result<(), CloudError> {
        let _: serde_json::Value = self
            .retry_backup_json("/v1/backups/complete", token, completion)
            .await?;
        Ok(())
    }

    async fn retry_backup_json<T, R>(
        &self,
        path: &str,
        token: &str,
        body: &T,
    ) -> Result<R, CloudError>
    where
        T: serde::Serialize + ?Sized,
        R: serde::de::DeserializeOwned,
    {
        let mut last_failure = None;
        for attempt in 0..3 {
            match self
                .http
                .post(self.backend.endpoint(path))
                .bearer_auth(token)
                .json(body)
                .send()
                .await
            {
                Ok(response)
                    if !matches!(response.status().as_u16(), 408 | 425 | 429 | 500..=599) =>
                {
                    return decode_managed_response(response).await;
                }
                Ok(response) => {
                    last_failure = Some(describe_failed_response(response).await);
                }
                Err(error) => last_failure = Some(error.without_url().to_string()),
            }
            if attempt < 2 {
                tokio::time::sleep(Duration::from_millis(100 * (1 << attempt))).await;
            }
        }
        Err(CloudError::Unreachable {
            reason: last_failure.unwrap_or_else(|| "managed backend returned no response".into()),
            spooled_bytes: 0,
        })
    }

    /// Queue one OSS alert for Cloud-owned fan-out. A retry with the same
    /// source id is idempotent at the backend.
    pub async fn send_notification(
        &self,
        token: &str,
        request: &ManagedNotificationRequest,
    ) -> Result<ManagedNotificationAccepted, CloudError> {
        self.retry_backup_json("/v1/notifications", token, request)
            .await
    }

    /// Mirror a batch of spans. Never called on a request path.
    pub async fn ship(
        &self,
        token: &str,
        submission_id: Uuid,
        spans: Vec<SpanRecord>,
    ) -> Result<IngestAck, CloudError> {
        let span_count = spans.len();
        let res = self
            .http
            .post(self.backend.endpoint("/v1/telemetry"))
            .bearer_auth(token)
            .json(&TelemetryBatch {
                submission_id,
                spans,
            })
            .send()
            .await
            .map_err(|e| CloudError::Unreachable {
                reason: e.to_string(),
                spooled_bytes: 0,
            })?;

        let status = res.status();
        if status.is_success() {
            let ack =
                res.json::<IngestAck>()
                    .await
                    .map_err(|e| CloudError::InvalidAcknowledgement {
                        submission_id,
                        detail: format!("unreadable ack: {e}"),
                    })?;
            if ack.submission_id != submission_id {
                return Err(CloudError::InvalidAcknowledgement {
                    submission_id,
                    detail: format!("response named submission {}", ack.submission_id),
                });
            }
            if ack.processed_spans != span_count {
                return Err(CloudError::InvalidAcknowledgement {
                    submission_id,
                    detail: format!("processed {} of {span_count} spans", ack.processed_spans),
                });
            }
            return Ok(ack);
        }

        match status.as_u16() {
            401 | 403 => Err(CloudError::CredentialRejected),
            // 5xx and 429 are the backend's problem, not the payload's: keep it.
            429 | 500..=599 => Err(CloudError::Unreachable {
                reason: format!("backend returned {status}"),
                spooled_bytes: 0,
            }),
            _ => {
                let detail = res
                    .json::<serde_json::Value>()
                    .await
                    .ok()
                    .and_then(|v| v["detail"].as_str().map(String::from))
                    .unwrap_or_else(|| format!("backend returned {status}"));
                Err(CloudError::Rejected { detail })
            }
        }
    }
}

struct UploadProgressReader<R> {
    inner: R,
    progress: tokio::sync::watch::Sender<()>,
}

impl<R> tokio::io::AsyncRead for UploadProgressReader<R>
where
    R: tokio::io::AsyncRead + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let filled_before = buffer.filled().len();
        match Pin::new(&mut self.inner).poll_read(context, buffer) {
            Poll::Ready(Ok(())) => {
                if buffer.filled().len() > filled_before {
                    self.progress.send_replace(());
                }
                Poll::Ready(Ok(()))
            }
            result => result,
        }
    }
}

async fn wait_for_upload_idle(
    mut progress: tokio::sync::watch::Receiver<()>,
    idle_timeout: Duration,
) {
    loop {
        let idle = tokio::time::sleep(idle_timeout);
        tokio::pin!(idle);
        tokio::select! {
            () = &mut idle => return,
            changed = progress.changed() => {
                if changed.is_err() {
                    tokio::time::sleep(idle_timeout).await;
                    return;
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
struct ValidatedBackupUpload {
    url: url::Url,
    headers: HeaderMap,
    allow_loopback: bool,
}

fn validate_backup_upload_target(
    upload_url: &str,
    expires_at_millis: i64,
    headers: &BTreeMap<String, String>,
    allow_loopback_http: bool,
) -> Result<ValidatedBackupUpload, CloudError> {
    if expires_at_millis <= chrono::Utc::now().timestamp_millis() {
        return Err(CloudError::InvalidBackupTarget {
            reason: "the presigned destination has expired".into(),
        });
    }

    let url = url::Url::parse(upload_url).map_err(|error| CloudError::InvalidBackupTarget {
        reason: format!("the destination URL is invalid: {error}"),
    })?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(CloudError::InvalidBackupTarget {
            reason: "credentials are not allowed in an upload URL".into(),
        });
    }
    if url.fragment().is_some() {
        return Err(CloudError::InvalidBackupTarget {
            reason: "fragments are not allowed in an upload URL".into(),
        });
    }

    let loopback = is_loopback_host(&url);
    match url.scheme() {
        "https" => {}
        "http" if allow_loopback_http && loopback => {}
        "http" => {
            return Err(CloudError::InvalidBackupTarget {
                reason: "HTTPS is required outside explicit loopback development".into(),
            })
        }
        scheme => {
            return Err(CloudError::InvalidBackupTarget {
                reason: format!("unsupported destination scheme {scheme:?}; HTTPS is required"),
            })
        }
    }

    let host = url.host().ok_or_else(|| CloudError::InvalidBackupTarget {
        reason: "the destination URL has no host".into(),
    })?;
    let unsafe_host = match host {
        url::Host::Domain(domain) => {
            domain.eq_ignore_ascii_case("localhost")
                || domain.to_ascii_lowercase().ends_with(".localhost")
        }
        url::Host::Ipv4(address) => is_unsafe_ip(IpAddr::V4(address)),
        url::Host::Ipv6(address) => is_unsafe_ip(IpAddr::V6(address)),
    };
    if unsafe_host && !(allow_loopback_http && loopback) {
        return Err(CloudError::InvalidBackupTarget {
            reason: format!("destination host {host} is not a public object-storage endpoint"),
        });
    }

    let mut validated_headers = HeaderMap::with_capacity(headers.len());
    for (name, value) in headers {
        let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
            CloudError::InvalidBackupTarget {
                reason: format!("upload header name {name:?} is invalid: {error}"),
            }
        })?;
        if !is_allowed_upload_header(&header_name) {
            return Err(CloudError::InvalidBackupTarget {
                reason: format!("upload header {header_name} is not allowed"),
            });
        }
        let header_value =
            HeaderValue::from_str(value).map_err(|error| CloudError::InvalidBackupTarget {
                reason: format!("upload header {header_name} has an invalid value: {error}"),
            })?;
        validated_headers.insert(header_name, header_value);
    }

    Ok(ValidatedBackupUpload {
        url,
        headers: validated_headers,
        allow_loopback: allow_loopback_http && loopback,
    })
}

async fn build_pinned_upload_client(
    target: &ValidatedBackupUpload,
    spooled_bytes: u64,
) -> Result<reqwest::Client, CloudError> {
    // Backup streams may legitimately run for hours. Bound connection
    // establishment, but never impose a whole-request timeout. Redirects are
    // disabled because a presigned destination is an authority boundary, not
    // a navigation hint.
    let mut builder = reqwest::Client::builder()
        .connect_timeout(UPLOAD_CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none());

    if let Some(url::Host::Domain(domain)) = target.url.host() {
        let port =
            target
                .url
                .port_or_known_default()
                .ok_or_else(|| CloudError::InvalidBackupTarget {
                    reason: "the destination URL has no known port".into(),
                })?;
        let lookup = tokio::time::timeout(
            UPLOAD_CONNECT_TIMEOUT,
            tokio::net::lookup_host((domain, port)),
        )
        .await
        .map_err(|_| CloudError::Unreachable {
            reason: format!(
                "backup destination {domain} DNS resolution timed out after {}ms",
                UPLOAD_CONNECT_TIMEOUT.as_millis()
            ),
            spooled_bytes,
        })?;
        let addresses = lookup
            .map_err(|error| CloudError::Unreachable {
                reason: format!("could not resolve backup destination {domain}: {error}"),
                spooled_bytes,
            })?
            .collect::<Vec<_>>();
        validate_resolved_upload_addresses(domain, &addresses, target.allow_loopback)?;
        // Pin the exact addresses that passed validation. reqwest retains the
        // original hostname for HTTP Host and TLS SNI/certificate checks, but
        // cannot perform a second DNS lookup and rebind the request elsewhere.
        builder = builder.resolve_to_addrs(domain, &addresses);
    }

    builder
        .build()
        .map_err(|error| CloudError::ClientConfiguration {
            reason: format!("could not configure backup upload client: {error}"),
        })
}

fn validate_resolved_upload_addresses(
    domain: &str,
    addresses: &[SocketAddr],
    allow_loopback: bool,
) -> Result<(), CloudError> {
    if addresses.is_empty() {
        return Err(CloudError::InvalidBackupTarget {
            reason: format!("destination host {domain} resolved to no addresses"),
        });
    }
    for address in addresses {
        let ip = address.ip();
        let permitted = if allow_loopback {
            // The only plaintext exception is an actual loopback socket. A
            // poisoned localhost answer containing even one public address
            // must fail closed rather than pinning an HTTP route off-host.
            ip.is_loopback()
        } else {
            !is_unsafe_ip(ip)
        };
        if !permitted {
            return Err(CloudError::InvalidBackupTarget {
                reason: format!("destination host {domain} resolved to unsafe address {ip}"),
            });
        }
    }
    Ok(())
}

fn is_allowed_upload_header(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "content-length" | "content-type" | "content-md5" | "if-none-match"
    ) || name.as_str().starts_with("x-amz-")
}

fn is_loopback_host(url: &url::Url) -> bool {
    url.host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("localhost"))
        || matches!(url.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
        || matches!(url.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback())
}

fn is_unsafe_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => validate_ipv4(&address).is_err(),
        IpAddr::V6(address) => validate_ipv6(&address).is_err(),
    }
}

async fn inspect_local_backup(path: &Path) -> Result<(u64, String), CloudError> {
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|error| CloudError::Rejected {
            detail: format!("could not open backup artifact: {error}"),
        })?;
    let mut checksum = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|error| CloudError::Rejected {
                detail: format!("could not read backup artifact: {error}"),
            })?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| CloudError::Rejected {
                detail: "backup artifact size exceeded the supported range".into(),
            })?;
        checksum.update(&buffer[..read]);
    }
    if bytes == 0 {
        return Err(CloudError::Rejected {
            detail: "backup artifact is empty".into(),
        });
    }
    let checksum_sha256 = checksum
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok((bytes, checksum_sha256))
}

/// Render a non-success response as an operator-readable reason, keeping the
/// Problem Details `detail` Cloud sent with it.
///
/// A retryable status is not automatically an opaque one: Cloud answers a 503
/// with a specific reason (which object storage call failed, for which key),
/// and dropping it left a self-hosted operator with nothing but
/// "returned 503 Service Unavailable" to debug a permanently stuck mirror.
async fn describe_failed_response(response: reqwest::Response) -> String {
    let status = response.status();
    match managed_detail(response).await {
        Some(detail) => format!("managed backend returned {status}: {detail}"),
        None => format!("managed backend returned {status}"),
    }
}

/// The subset of an RFC 7807 Problem Details body this client reads. Cloud's
/// error responses may carry `type`/`title`/`instance`/extension fields too,
/// but `detail` is the only one `managed_detail` surfaces — declaring just
/// it (rather than parsing into `serde_json::Value`) checks the response
/// actually has the expected shape instead of silently accepting anything.
#[derive(serde::Deserialize)]
struct ManagedProblemDetail {
    detail: Option<String>,
}

/// Extract the RFC 7807 `detail` from a managed response body, if it has one.
async fn managed_detail(response: reqwest::Response) -> Option<String> {
    let detail = response
        .json::<ManagedProblemDetail>()
        .await
        .ok()?
        .detail?
        .trim()
        .to_string();
    (!detail.is_empty()).then_some(detail)
}

async fn decode_managed_response<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, CloudError> {
    let status = response.status();
    if status.is_success() {
        return response
            .json::<T>()
            .await
            .map_err(|error| CloudError::Rejected {
                detail: format!("managed backend returned an unreadable response: {error}"),
            });
    }
    if matches!(status.as_u16(), 401 | 403) {
        return Err(CloudError::CredentialRejected);
    }
    if matches!(status.as_u16(), 429 | 500..=599) {
        return Err(CloudError::Unreachable {
            reason: describe_failed_response(response).await,
            spooled_bytes: 0,
        });
    }
    let detail = managed_detail(response)
        .await
        .unwrap_or_else(|| format!("managed backend returned {status}"));
    Err(CloudError::Rejected { detail })
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };

    use axum::{
        body::Body,
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::{post, put},
        Json, Router,
    };
    use chrono::Utc;
    use futures::StreamExt;
    use serde_json::json;
    use tokio::io::AsyncWriteExt;

    use super::*;

    async fn managed_chat_stub(
        State(calls): State<Arc<AtomicUsize>>,
        Json(request): Json<ManagedAiChatRequest>,
    ) -> (StatusCode, Json<serde_json::Value>) {
        let attempt = calls.fetch_add(1, Ordering::SeqCst);
        if attempt == 0 {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"detail": "temporary outage"})),
            );
        }
        (
            StatusCode::OK,
            Json(json!({
                "request_id": request.request_id,
                "settled_credits": 1,
                "provider": "test-provider",
                "model": "test-model",
                "body": {"id": "completion-1", "choices": []}
            })),
        )
    }

    #[tokio::test]
    async fn managed_chat_retries_transient_failure_with_the_same_request_id() {
        let calls = Arc::new(AtomicUsize::new(0));
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping managed-chat retry test: sandbox denied TCP bind");
                return;
            }
            Err(error) => panic!("bind loopback stub: {error}"),
        };
        let address = listener.local_addr().expect("stub address");
        let app = Router::new()
            .route("/v1/ai/chat/completions", post(managed_chat_stub))
            .with_state(calls.clone());
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve managed AI stub");
        });

        let client = CloudClient::new(
            BackendUrl::loopback_development(&format!("http://{address}"))
                .expect("loopback backend"),
        )
        .expect("cloud client");
        let request_id = Uuid::new_v4();
        let response = client
            .managed_ai_chat(
                "instance-token",
                &ManagedAiChatRequest {
                    request_id,
                    requested_at: Utc::now(),
                    body: json!({"messages": [{"role": "user", "content": "status?"}]}),
                },
            )
            .await
            .expect("transient outage recovers");

        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(response.request_id, request_id);
        assert_eq!(response.settled_credits, 1);
    }

    /// Spin up a loopback stub that serves `body` from the managed-backup
    /// capability route and return what `managed_backup_credentials` decoded,
    /// or `None` when the sandbox forbids binding a listener.
    async fn fetch_managed_backup_capability(
        body: serde_json::Value,
    ) -> Option<ManagedBackupCapability> {
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping managed-backup capability test: sandbox denied TCP bind");
                return None;
            }
            Err(error) => panic!("bind loopback stub: {error}"),
        };
        let address = listener.local_addr().expect("stub address");
        let app = Router::new().route(
            "/v1/backups/managed/capability",
            axum::routing::get(move || {
                let body = body.clone();
                async move { Json(body) }
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve managed backup stub");
        });

        let client = CloudClient::new(
            BackendUrl::loopback_development(&format!("http://{address}"))
                .expect("loopback backend"),
        )
        .expect("cloud client");
        Some(
            client
                .managed_backup_credentials("instance-token")
                .await
                .expect("capability decodes"),
        )
    }

    /// The whole point of the protocol change: a prefix-scoped R2 credential is
    /// STS-style, and SigV4 rejects it without the session token. If this ever
    /// stops arriving at the caller, every managed backup upload 403s.
    #[tokio::test]
    async fn managed_backup_credentials_carry_the_session_token_and_expiry() {
        let Some(capability) = fetch_managed_backup_capability(json!({
            "configured": true,
            "endpoint": "https://objects.example.com",
            "region": "auto",
            "bucket_name": "shared-bucket",
            "bucket_path": "tenants/1/instances/2/managed-backups/",
            "access_key_id": "temp-access-key",
            "secret_key": "temp-secret-key",
            "session_token": "temp-session-token",
            "expires_at": "2026-09-03T12:34:56Z",
            "reason": null,
        }))
        .await
        else {
            return;
        };

        assert!(capability.configured);
        assert_eq!(
            capability.session_token.as_deref(),
            Some("temp-session-token")
        );
        assert_eq!(
            capability.expires_at.map(|at| at.to_rfc3339()),
            Some("2026-09-03T12:34:56+00:00".to_string())
        );
        // And the token must not leak through the value's own Debug rendering,
        // which is what ends up in `tracing` output on the failure paths.
        assert!(!format!("{capability:?}").contains("temp-session-token"));
    }

    /// A backend that predates the temporary-credential work sends neither
    /// field. That must decode cleanly to `None`, so a long-lived credential
    /// keeps behaving exactly as it did.
    #[tokio::test]
    async fn managed_backup_credentials_default_to_no_session_token() {
        let Some(capability) = fetch_managed_backup_capability(json!({
            "configured": true,
            "endpoint": "https://objects.example.com",
            "region": "auto",
            "bucket_name": "shared-bucket",
            "bucket_path": "tenant-1",
            "access_key_id": "long-lived-access-key",
            "secret_key": "long-lived-secret-key",
            "reason": null,
        }))
        .await
        else {
            return;
        };

        assert!(capability.session_token.is_none());
        assert!(capability.expires_at.is_none());
    }

    struct NotificationStub {
        calls: AtomicUsize,
        source_ids: Mutex<Vec<String>>,
    }

    async fn notification_stub(
        State(state): State<Arc<NotificationStub>>,
        Json(request): Json<ManagedNotificationRequest>,
    ) -> (StatusCode, Json<serde_json::Value>) {
        state
            .source_ids
            .lock()
            .expect("notification source id lock")
            .push(request.source_notification_id);
        if state.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"detail": "temporary outage"})),
            );
        }
        (
            StatusCode::ACCEPTED,
            Json(json!({
                "event_id": Uuid::new_v4(),
                "queued_deliveries": 1,
                "duplicate": true
            })),
        )
    }

    #[tokio::test]
    async fn managed_notification_retries_503_with_the_same_source_id() {
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping notification retry test: sandbox denied TCP bind");
                return;
            }
            Err(error) => panic!("bind notification stub: {error}"),
        };
        let address = listener.local_addr().expect("notification stub address");
        let state = Arc::new(NotificationStub {
            calls: AtomicUsize::new(0),
            source_ids: Mutex::new(Vec::new()),
        });
        let app = Router::new()
            .route("/v1/notifications", post(notification_stub))
            .with_state(state.clone());
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve notification stub");
        });
        let client = CloudClient::new(
            BackendUrl::loopback_development(&format!("http://{address}"))
                .expect("loopback notification backend"),
        )
        .expect("cloud client");
        let source_id = "stable-private-source-id".to_string();
        let accepted = client
            .send_notification(
                "instance-token",
                &ManagedNotificationRequest {
                    source_notification_id: source_id.clone(),
                    title: "Deployment failed".into(),
                    message: "The deployment failed".into(),
                    severity: temps_cloud_protocol::ManagedNotificationSeverity::Error,
                    metadata: Default::default(),
                },
            )
            .await
            .expect("transient notification outage recovers");

        assert!(accepted.duplicate);
        assert_eq!(state.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            state
                .source_ids
                .lock()
                .expect("notification source ids lock")
                .as_slice(),
            [source_id, "stable-private-source-id".to_string()]
        );
    }

    /// A retryable status is where Cloud puts its most operationally useful
    /// reasons (which object-storage call failed, for which key). Dropping the
    /// body left an operator staring at a bare "returned 503 Service
    /// Unavailable" while a backup mirror retried for an hour.
    #[tokio::test]
    async fn an_exhausted_backup_retry_reports_the_reason_cloud_sent() {
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping backup detail test: sandbox denied TCP bind");
                return;
            }
            Err(error) => panic!("bind backup detail stub: {error}"),
        };
        let address = listener.local_addr().expect("backup detail stub address");
        let app = Router::new().route(
            "/v1/backups/walg/objects/complete",
            post(|| async {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({
                        "title": "Backups Unavailable",
                        "detail": "Backups are not configured: Object storage failed while \
                                   inspect uploaded object for repository/base/00: provider \
                                   returned HTTP 404 Not Found"
                    })),
                )
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve backup detail stub");
        });
        let client = CloudClient::new(
            BackendUrl::loopback_development(&format!("http://{address}"))
                .expect("loopback backup backend"),
        )
        .expect("cloud client");

        let error = client
            .complete_walg_object(
                "instance-token",
                &WalGObjectCompleted {
                    backup_id: Uuid::new_v4(),
                    relative_key: "repository/base/00".into(),
                    bytes: 1,
                    checksum_sha256: "0".repeat(64),
                },
            )
            .await
            .expect_err("a 503 on every attempt is a failure");

        assert!(error.is_retryable(), "a 503 stays retryable: {error}");
        let rendered = error.to_string();
        assert!(
            rendered.contains("provider returned HTTP 404 Not Found"),
            "the operator must see Cloud's own reason, got: {rendered}"
        );
        assert!(
            rendered.contains("503"),
            "the status still belongs in the message, got: {rendered}"
        );
    }

    #[tokio::test]
    async fn a_retryable_failure_without_a_body_still_names_the_status() {
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping bodyless retry test: sandbox denied TCP bind");
                return;
            }
            Err(error) => panic!("bind bodyless stub: {error}"),
        };
        let address = listener.local_addr().expect("bodyless stub address");
        let app = Router::new().route(
            "/v1/backups/walg/objects/complete",
            post(|| async { StatusCode::BAD_GATEWAY }),
        );
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve bodyless stub");
        });
        let client = CloudClient::new(
            BackendUrl::loopback_development(&format!("http://{address}"))
                .expect("loopback bodyless backend"),
        )
        .expect("cloud client");

        let error = client
            .complete_walg_object(
                "instance-token",
                &WalGObjectCompleted {
                    backup_id: Uuid::new_v4(),
                    relative_key: "repository/base/00".into(),
                    bytes: 1,
                    checksum_sha256: "0".repeat(64),
                },
            )
            .await
            .expect_err("a 502 on every attempt is a failure");

        assert!(
            error.to_string().contains("502"),
            "a body-less failure still names its status, got: {error}"
        );
    }

    struct BackupStub {
        origin: String,
        target_calls: AtomicUsize,
        upload_calls: AtomicUsize,
        complete_calls: AtomicUsize,
        first_upload_bytes: AtomicUsize,
        successful_upload_bytes: AtomicUsize,
        expected_upload_bytes: usize,
        backup_id: Mutex<Option<Uuid>>,
    }

    async fn backup_target_stub(
        State(state): State<Arc<BackupStub>>,
        Json(request): Json<BackupTargetRequest>,
    ) -> (StatusCode, Json<serde_json::Value>) {
        let attempt = state.target_calls.fetch_add(1, Ordering::SeqCst);
        let mut observed = state.backup_id.lock().expect("backup id lock");
        if let Some(id) = *observed {
            assert_eq!(id, request.backup_id, "target retry changed backup id");
        } else {
            *observed = Some(request.backup_id);
        }
        if attempt == 0 {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"detail": "target response lost"})),
            );
        }
        (
            StatusCode::CREATED,
            Json(json!({
                "backup_id": request.backup_id,
                "upload_url": format!("{}/upload", state.origin),
                "object_key": format!("backups/{}", request.backup_id),
                "expires_at_millis": Utc::now().timestamp_millis() + 60_000,
                "headers": {
                    "content-length": request.estimated_bytes.to_string(),
                    "x-amz-checksum-sha256": "provider-bound"
                }
            })),
        )
    }

    async fn backup_upload_stub(
        State(state): State<Arc<BackupStub>>,
        headers: HeaderMap,
        body: Body,
    ) -> StatusCode {
        assert_eq!(headers["x-amz-checksum-sha256"], "provider-bound");
        let attempt = state.upload_calls.fetch_add(1, Ordering::SeqCst);
        let mut stream = body.into_data_stream();
        if attempt == 0 {
            let bytes = match stream.next().await {
                Some(Ok(bytes)) => bytes.len(),
                _ => 0,
            };
            state.first_upload_bytes.store(bytes, Ordering::SeqCst);
            return StatusCode::SERVICE_UNAVAILABLE;
        }

        let mut received = 0_usize;
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(_) => return StatusCode::BAD_REQUEST,
            };
            received = match received.checked_add(chunk.len()) {
                Some(total) => total,
                None => return StatusCode::PAYLOAD_TOO_LARGE,
            };
        }
        state
            .successful_upload_bytes
            .store(received, Ordering::SeqCst);
        if received == state.expected_upload_bytes {
            StatusCode::OK
        } else {
            StatusCode::BAD_REQUEST
        }
    }

    async fn backup_complete_stub(
        State(state): State<Arc<BackupStub>>,
        Json(request): Json<BackupCompleted>,
    ) -> (StatusCode, Json<serde_json::Value>) {
        assert_eq!(
            Some(request.backup_id),
            *state.backup_id.lock().expect("backup id lock")
        );
        if state.complete_calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"detail": "completion response lost"})),
            );
        }
        (StatusCode::OK, Json(json!({"state": "complete"})))
    }

    async fn redirect_upload_stub() -> (StatusCode, [(axum::http::HeaderName, &'static str); 1]) {
        (
            StatusCode::TEMPORARY_REDIRECT,
            [(axum::http::header::LOCATION, "/redirect-target")],
        )
    }

    async fn redirect_target_stub(State(calls): State<Arc<AtomicUsize>>) -> StatusCode {
        calls.fetch_add(1, Ordering::SeqCst);
        StatusCode::NO_CONTENT
    }

    #[tokio::test]
    async fn backup_upload_recovers_each_network_boundary_without_changing_identity() {
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping backup retry test: sandbox denied TCP bind");
                return;
            }
            Err(error) => panic!("bind backup stub: {error}"),
        };
        let address = listener.local_addr().expect("backup stub address");
        let state = Arc::new(BackupStub {
            origin: format!("http://{address}"),
            target_calls: AtomicUsize::new(0),
            upload_calls: AtomicUsize::new(0),
            complete_calls: AtomicUsize::new(0),
            first_upload_bytes: AtomicUsize::new(0),
            successful_upload_bytes: AtomicUsize::new(0),
            expected_upload_bytes: 8 * 1024 * 1024,
            backup_id: Mutex::new(None),
        });
        let app = Router::new()
            .route("/v1/backups/target", post(backup_target_stub))
            .route("/upload", put(backup_upload_stub))
            .route("/v1/backups/complete", post(backup_complete_stub))
            .with_state(state.clone());
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve backup stub");
        });
        let temp = tempfile::tempdir().expect("backup tempdir");
        let artifact_path = temp.path().join("backup.sql.gz");
        tokio::fs::write(&artifact_path, vec![0x5a; state.expected_upload_bytes])
            .await
            .expect("write backup fixture");
        let client = CloudClient::new(
            BackendUrl::loopback_development(&format!("http://{address}"))
                .expect("loopback backup backend"),
        )
        .expect("cloud client");
        let backup_id = Uuid::new_v4();

        let target = client
            .upload_backup_file(
                "instance-token",
                Uuid::new_v4(),
                backup_id,
                "postgres/main".into(),
                BackupArtifact {
                    engine: temps_cloud_protocol::BackupEngine::Postgres,
                    format: temps_cloud_protocol::BackupFormat::PgDumpPlain,
                    compression: temps_cloud_protocol::BackupCompression::Gzip,
                    postgres_major: 18,
                },
                &artifact_path,
            )
            .await
            .expect("backup survives transient target, PUT and completion failures");

        assert_eq!(Some(target.backup_id), *state.backup_id.lock().unwrap());
        assert_eq!(target.backup_id, backup_id);
        assert_eq!(state.target_calls.load(Ordering::SeqCst), 2);
        assert_eq!(state.upload_calls.load(Ordering::SeqCst), 2);
        assert_eq!(state.complete_calls.load(Ordering::SeqCst), 2);
        let interrupted_bytes = state.first_upload_bytes.load(Ordering::SeqCst);
        assert!(
            interrupted_bytes > 0 && interrupted_bytes < state.expected_upload_bytes,
            "the injected outage must interrupt a live stream, not wait for the whole file"
        );
        assert_eq!(
            state.successful_upload_bytes.load(Ordering::SeqCst),
            state.expected_upload_bytes,
            "the retry must reopen and stream the complete artifact"
        );
    }

    #[tokio::test]
    async fn backup_upload_never_follows_provider_redirects() {
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping backup redirect test: sandbox denied TCP bind");
                return;
            }
            Err(error) => panic!("bind backup redirect stub: {error}"),
        };
        let address = listener.local_addr().expect("backup redirect stub address");
        let redirected_calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/redirect", put(redirect_upload_stub))
            .route("/redirect-target", put(redirect_target_stub))
            .with_state(redirected_calls.clone());
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve backup redirect stub");
        });

        let client = CloudClient::new(
            BackendUrl::loopback_development(&format!("http://{address}"))
                .expect("loopback redirect backend"),
        )
        .expect("cloud client");
        let target = WalGObjectTarget {
            backup_id: Uuid::new_v4(),
            relative_key: "basebackups_005/base.tar.lz4".into(),
            upload_required: true,
            upload_url: format!("http://localhost:{}/redirect", address.port()),
            expires_at_millis: Utc::now().timestamp_millis() + 60_000,
            headers: BTreeMap::new(),
        };

        let response = client
            .upload_backup_object_reader(
                &target,
                std::io::Cursor::new(b"backup bytes".to_vec()),
                12,
            )
            .await
            .expect("redirect response is returned without navigation");

        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            redirected_calls.load(Ordering::SeqCst),
            0,
            "backup bytes must never be replayed to a redirect destination"
        );
    }

    #[tokio::test]
    async fn backup_upload_times_out_when_target_stops_making_progress() {
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping stalled upload test: sandbox denied TCP bind");
                return;
            }
            Err(error) => panic!("bind stalled upload stub: {error}"),
        };
        let address = listener.local_addr().expect("stalled upload address");
        tokio::spawn(async move {
            if let Ok((_socket, _peer)) = listener.accept().await {
                // Keep the connection open without reading the request or
                // returning a response. The upload watchdog must abort it.
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });

        let mut client = CloudClient::new(
            BackendUrl::loopback_development(&format!("http://{address}"))
                .expect("loopback stalled-upload backend"),
        )
        .expect("cloud client");
        client.upload_idle_timeout = Duration::from_millis(100);
        let target = WalGObjectTarget {
            backup_id: Uuid::new_v4(),
            relative_key: "basebackups_005/base.tar.lz4".into(),
            upload_required: true,
            upload_url: format!("http://{address}/stalled"),
            expires_at_millis: Utc::now().timestamp_millis() + 60_000,
            headers: BTreeMap::new(),
        };
        let started = tokio::time::Instant::now();

        let error = client
            .upload_backup_object_reader(
                &target,
                std::io::Cursor::new(vec![0x5a; 64 * 1024]),
                64 * 1024,
            )
            .await
            .expect_err("a target that never consumes or responds must time out");

        assert!(matches!(
            &error,
            CloudError::BackupUploadIdleTimeout {
                idle_timeout_ms: 100,
                spooled_bytes: 65_536,
            }
        ));
        assert!(error.is_retryable());
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "idle upload cancellation must remain bounded"
        );
    }

    #[tokio::test]
    async fn backup_upload_allows_slow_continuous_reader_progress() {
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping progressing upload test: sandbox denied TCP bind");
                return;
            }
            Err(error) => panic!("bind progressing upload stub: {error}"),
        };
        let address = listener.local_addr().expect("progressing upload address");
        let app = Router::new().route(
            "/slow",
            put(|body: axum::body::Bytes| async move {
                assert_eq!(body.len(), 4 * 1024);
                StatusCode::OK
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve progressing upload stub");
        });

        let mut client = CloudClient::new(
            BackendUrl::loopback_development(&format!("http://{address}"))
                .expect("loopback progressing-upload backend"),
        )
        .expect("cloud client");
        client.upload_idle_timeout = Duration::from_millis(120);
        let target = WalGObjectTarget {
            backup_id: Uuid::new_v4(),
            relative_key: "wal_005/segment".into(),
            upload_required: true,
            upload_url: format!("http://{address}/slow"),
            expires_at_millis: Utc::now().timestamp_millis() + 60_000,
            headers: BTreeMap::new(),
        };
        let (reader, mut writer) = tokio::io::duplex(1024);
        tokio::spawn(async move {
            for _ in 0..4 {
                tokio::time::sleep(Duration::from_millis(50)).await;
                writer
                    .write_all(&vec![0x5a; 1024])
                    .await
                    .expect("write progressing upload chunk");
            }
        });
        let started = tokio::time::Instant::now();

        let response = client
            .upload_backup_object_reader(&target, reader, 4 * 1024)
            .await
            .expect("continuous progress must not hit the idle watchdog");

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            started.elapsed() > client.upload_idle_timeout,
            "test must run longer than one idle window"
        );
    }

    #[test]
    fn production_backup_targets_require_https_public_hosts_and_safe_headers() {
        let client = CloudClient::new(
            BackendUrl::production("https://cloud.example.com").expect("production backend"),
        )
        .expect("cloud client");
        let expires = Utc::now().timestamp_millis() + 60_000;
        let allowed_headers = BTreeMap::from([
            ("content-length".into(), "42".into()),
            ("x-amz-checksum-sha256".into(), "checksum".into()),
        ]);

        assert!(client
            .validate_backup_upload_target(
                "https://objects.example.com/backup?X-Amz-Signature=signed",
                expires,
                &allowed_headers,
            )
            .is_ok());

        for invalid in [
            "http://objects.example.com/backup",
            "https://user:secret@objects.example.com/backup",
            "https://objects.example.com/backup#fragment",
            "https://127.0.0.1/backup",
            "https://10.0.0.1/backup",
            "https://169.254.169.254/latest/meta-data",
            "https://localhost/backup",
            "file:///tmp/backup",
        ] {
            assert!(
                client
                    .validate_backup_upload_target(invalid, expires, &allowed_headers)
                    .is_err(),
                "accepted unsafe backup destination {invalid}"
            );
        }

        let unsafe_headers = BTreeMap::from([("authorization".into(), "Bearer secret".into())]);
        assert!(client
            .validate_backup_upload_target(
                "https://objects.example.com/backup",
                expires,
                &unsafe_headers,
            )
            .is_err());
        assert!(client
            .validate_backup_upload_target(
                "https://objects.example.com/backup",
                Utc::now().timestamp_millis() - 1,
                &BTreeMap::new(),
            )
            .is_err());
    }

    #[test]
    fn loopback_backup_targets_are_only_available_in_explicit_development() {
        let client = CloudClient::new(
            BackendUrl::loopback_development("http://127.0.0.1:19202").expect("loopback backend"),
        )
        .expect("cloud client");

        assert!(client
            .validate_backup_upload_target(
                "http://127.0.0.1:19000/temps-backups/object?signature=dev",
                Utc::now().timestamp_millis() + 60_000,
                &BTreeMap::new(),
            )
            .is_ok());
        assert!(client
            .validate_backup_upload_target(
                "http://192.168.1.20:9000/temps-backups/object",
                Utc::now().timestamp_millis() + 60_000,
                &BTreeMap::new(),
            )
            .is_err());
    }

    #[test]
    fn resolved_backup_hosts_reject_private_or_mixed_dns_answers() {
        let public = SocketAddr::from(([93, 184, 216, 34], 443));
        let private = SocketAddr::from(([10, 20, 30, 40], 443));
        let metadata = SocketAddr::from(([169, 254, 169, 254], 443));
        let loopback = SocketAddr::from(([127, 0, 0, 1], 19000));

        assert!(validate_resolved_upload_addresses("objects.example", &[public], false).is_ok());
        assert!(
            validate_resolved_upload_addresses("objects.example", &[public, private], false,)
                .is_err()
        );
        assert!(validate_resolved_upload_addresses("objects.example", &[metadata], false).is_err());
        assert!(validate_resolved_upload_addresses("localhost", &[loopback], false).is_err());
        assert!(validate_resolved_upload_addresses("localhost", &[loopback], true).is_ok());
        assert!(
            validate_resolved_upload_addresses("localhost", &[loopback, public], true,).is_err()
        );
        assert!(validate_resolved_upload_addresses("localhost", &[public], true).is_err());
        assert!(validate_resolved_upload_addresses("objects.example", &[], false).is_err());
    }

    #[test]
    fn only_transient_failures_are_retryable() {
        assert!(CloudError::Unreachable {
            reason: "timeout".into(),
            spooled_bytes: 0
        }
        .is_retryable());

        // These must NOT buffer forever: no amount of waiting fixes a revoked
        // credential or a payload the backend refuses.
        assert!(CloudError::CredentialRejected.is_retryable());
        assert!(!CloudError::NotEnrolled.is_retryable());
        assert!(!CloudError::Rejected {
            detail: "bad".into()
        }
        .is_retryable());
        assert!(!CloudError::InvalidBackupTarget {
            reason: "unsafe destination".into()
        }
        .is_retryable());
    }

    #[test]
    fn production_backends_require_a_clean_https_origin() {
        assert!(BackendUrl::production("https://cloud.test").is_ok());
        for invalid in [
            "http://cloud.test",
            "https://user@cloud.test",
            "https://cloud.test/path",
            "https://cloud.test?query=1",
            "https://cloud.test#fragment",
        ] {
            assert!(
                BackendUrl::production(invalid).is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[test]
    fn development_http_is_restricted_to_loopback() {
        assert!(BackendUrl::loopback_development("http://127.0.0.1:1234").is_ok());
        assert!(BackendUrl::loopback_development("http://localhost:1234").is_ok());
        assert!(BackendUrl::loopback_development("http://192.168.1.2:1234").is_err());
    }

    #[test]
    fn errors_tell_the_operator_what_to_do() {
        // These strings are the entire support channel for a self-hosted user.
        assert!(CloudError::NotEnrolled
            .to_string()
            .contains("enrollment code"));
        assert!(CloudError::CredentialRejected
            .to_string()
            .contains("re-enroll"));
    }
}
