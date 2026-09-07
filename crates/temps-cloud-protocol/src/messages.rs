// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Messages exchanged over the management channel and the ingest endpoint.
//!
//! # Forward compatibility
//!
//! Every envelope carries a `kind` string rather than an externally-tagged
//! enum, so a peer that receives a kind it does not know can log and drop the
//! frame instead of failing to deserialise the connection. That property is
//! load-bearing: a v1 instance will still be talking to the backend years after
//! v3 ships, and a single unknown frame must never take the channel down.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A framed message on the management channel.
///
/// `payload` stays as raw JSON until `kind` has been matched, so unknown kinds
/// cost nothing to skip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub kind: String,
    pub payload: serde_json::Value,
}

impl Envelope {
    pub fn new<T: Serialize>(kind: &str, payload: &T) -> Result<Self, serde_json::Error> {
        Ok(Self {
            kind: kind.to_string(),
            payload: serde_json::to_value(payload)?,
        })
    }

    /// Decode the payload, or `None` when this is not the expected kind.
    ///
    /// Returning `None` rather than an error for a kind mismatch is what lets
    /// a receive loop skip unknown frames without special-casing each one.
    pub fn decode<T: for<'de> Deserialize<'de>>(&self, kind: &str) -> Option<T> {
        if self.kind != kind {
            return None;
        }
        serde_json::from_value(self.payload.clone()).ok()
    }
}

// ---------------------------------------------------------------------------
// Enrollment
// ---------------------------------------------------------------------------

/// Sent once, over HTTPS, to exchange an operator-pasted enrollment code for
/// long-lived instance credentials.
#[derive(Clone, Serialize, Deserialize)]
pub struct EnrollRequest {
    /// Short-lived code the operator copied from the cloud console.
    pub enrollment_code: String,
    /// Stable identifier the instance generates once and persists.
    pub instance_id: Uuid,
    /// Reported for support and skew diagnostics only — never trusted for
    /// authorization decisions.
    pub agent_version: String,
}

impl std::fmt::Debug for EnrollRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnrollRequest")
            .field("enrollment_code", &"[REDACTED]")
            .field("instance_id", &self.instance_id)
            .field("agent_version", &self.agent_version)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct EnrollResponse {
    pub tenant_id: Uuid,
    /// Human-readable Cloud account identity for the local connection UI.
    /// Optional for compatibility with older managed backends.
    #[serde(default)]
    pub account_email: Option<String>,
    /// Bearer token for the management channel and the ingest endpoint.
    /// Scoped to this instance and this tenant, nothing else.
    pub instance_token: String,
    /// What the tenant's current plan permits. May shrink on downgrade.
    ///
    /// Defaults to empty when absent, per the additive-changes rule: a backend
    /// predating this field must still be understood, and an instance that
    /// cannot tell what it is allowed to do should assume nothing rather than
    /// everything.
    #[serde(default)]
    pub capabilities: Vec<crate::Capability>,
}

impl std::fmt::Debug for EnrollResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnrollResponse")
            .field("tenant_id", &self.tenant_id)
            .field("account_email", &self.account_email)
            .field("instance_token", &"[REDACTED]")
            .field("capabilities", &self.capabilities)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Telemetry
// ---------------------------------------------------------------------------

/// One span as shipped by an instance.
///
/// Deliberately flat and self-describing: the cloud must be able to accept a
/// batch from an instance several versions behind without a translation table.
///
/// # Two fidelity tiers on one struct (ADR-040 §1)
///
/// An instance decides, per project, how much of a span may leave it:
///
/// - **`Metered`** (the default, and what every instance shipped before
///   ADR-040): pseudonymised `trace_id`/`span_id`, the constant `name`
///   `"span"`, no attributes. Enough to meter and to prove liveness, and
///   nothing else.
/// - **`Queryable`** (opt-in, per project): the real span name, real trace and
///   span IDs, and the fields below that make a span renderable in a console.
///
/// Every field added for `Queryable` is `#[serde(default)]` **and** skipped on
/// serialization when empty. Both halves matter:
///
/// - `serde(default)` keeps a *newer gateway* able to read an *older
///   instance's* batch, which is the crate's stated compatibility rule.
/// - `skip_serializing_if` keeps a `Metered` record's bytes **identical** to
///   what instances shipped before these fields existed, so raising the
///   protocol version is not required and an older gateway sees no new keys.
///   `temps-otel` has a test pinning that byte-for-byte equivalence.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SpanRecord {
    pub trace_id: String,
    pub span_id: String,
    pub name: String,
    /// Milliseconds since the Unix epoch. Used for querying and retention.
    ///
    /// NEVER used for billing: the cloud meters on its own receive time, so a
    /// wrong clock on an instance cannot move money.
    pub ts_millis: i64,
    pub duration_ms: f64,
    #[serde(default)]
    pub attributes: std::collections::BTreeMap<String, String>,

    // ── Queryable-fidelity fields (ADR-040 §1) ──────────────────────────
    //
    // Absent at `Metered` fidelity. Never populated unless the owning project
    // opted in.
    /// Pseudonymous, stable per-link scoping key for the owning project —
    /// `pseudonymize_telemetry_id("project", project_id)`.
    ///
    /// A stable key the cloud can group and scope by without learning the
    /// project's **name**, and which no third party observing the payload can
    /// correlate back to a local project. It is deliberately *not* claimed to
    /// hide the project id from the cloud itself: the HMAC key is the
    /// instance's own bearer token, which the cloud issued and receives on
    /// every request, and the pseudonymised input is a small integer — so the
    /// key holder can enumerate `HMAC(token, "project\0" || i)` and invert
    /// every `project_ref` in the tenant. (The trace and span pseudonyms are
    /// different: their inputs are 128-bit random values, which are not
    /// enumerable.)
    ///
    /// Empty at `Metered` fidelity, where nothing is queryable and so nothing
    /// needs scoping.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub project_ref: String,
    /// OTel `service.name` of the emitting service. A traces view the user
    /// cannot filter by service is not a traces view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    /// OTel span kind (`SERVER`, `CLIENT`, …). Low cardinality, enum-like.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_kind: Option<String>,
    /// OTel status: `OK` / `ERROR` / `UNSET`. Required for error counts and
    /// the error filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<String>,
    /// Parent span id, required to render a trace as a tree rather than a
    /// flat list. Real (not pseudonymised) whenever `span_id` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
    /// OTel `deployment.environment` of the emitting service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
}

/// A batch posted to the ingest endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryBatch {
    /// Generated by the instance before the first attempt and reused for every
    /// retry of these exact bytes. The backend binds it to the authenticated
    /// instance and payload digest before treating a retry as idempotent.
    pub submission_id: Uuid,
    pub spans: Vec<SpanRecord>,
}

/// Ingest outcome. A client clears a submission only when the id matches and
/// `processed_spans` covers the entire attempted batch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IngestAck {
    pub submission_id: Uuid,
    /// Records fully handled by the gateway and safe to remove client-side.
    pub processed_spans: usize,
    /// Records retained after quota sampling. May be lower than `processed`.
    pub stored_spans: usize,
    /// Bytes the cloud will bill for — echoed back so the operator can
    /// reconcile their own figure against the invoice.
    pub metered_bytes: u64,
    /// Present when the batch was accepted but the tenant is degraded, e.g.
    /// over quota and now sampling. The instance must surface this, not hide it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<crate::Unavailable>,
}

// ---------------------------------------------------------------------------
// Backups
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupEngine {
    Postgres,
    TimescaleDb,
    MongoDb,
    Redis,
    MariaDb,
    RustFs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupFormat {
    /// Plain SQL produced by `pg_dump` or `pg_dumpall` and restored with psql.
    PgDumpPlain,
    /// A physical WAL-G repository snapshot: one completed base backup plus
    /// the WAL interval needed to make it consistent and PITR-capable.
    WalGRepository,
    /// A `mongodump --archive` stream stored as immutable repository objects.
    MongoDumpArchive,
    /// A Redis RDB snapshot produced with `redis-cli --rdb`.
    RedisRdb,
    /// A physical `mariadb-backup --stream=mbstream` snapshot.
    MariaDbPhysical,
    /// An immutable set of object-store objects and their checksums.
    ObjectSet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupCompression {
    None,
    Gzip,
    /// Compression is owned by WAL-G per repository object. The snapshot is
    /// not wrapped in another archive by Temps.
    WalGNative,
}

/// Engine-specific identity required to prove that a native snapshot can be
/// restored. This is tagged on the wire so Cloud never has to infer an engine
/// from a source label or file name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NativeSnapshotIdentity {
    PostgresWalG {
        postgres_major: u16,
        postgres_system_identifier: String,
        backup_name: String,
        timeline: u32,
        start_lsn: String,
        finish_lsn: String,
    },
    MongoDbStream {
        engine_version: String,
        backup_name: String,
    },
    RedisRdbStream {
        engine_version: String,
        backup_name: String,
    },
    MariaDbPhysical {
        engine_version: String,
        backup_name: String,
        /// Present when binary logging was enabled at base-backup time.
        #[serde(skip_serializing_if = "Option::is_none")]
        binlog_file: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        binlog_position: Option<u64>,
    },
    ObjectSet {
        snapshot_name: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeSnapshotObjectKind {
    BaseBackup,
    Sentinel,
    Wal,
    Metadata,
    Data,
    Binlog,
    Object,
}

/// One immutable object in a native snapshot. Objects are uploaded directly
/// from the self-hosted instance to Cloud object storage; the API only issues
/// targets and records independently verified checksums.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeSnapshotObjectDeclaration {
    pub relative_key: String,
    pub kind: NativeSnapshotObjectKind,
    pub bytes: u64,
    pub checksum_sha256: String,
}

/// Engine-neutral registration envelope for physical/native backups.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeSnapshotRequest {
    pub backup_id: Uuid,
    pub instance_id: Uuid,
    pub source: String,
    pub engine: BackupEngine,
    pub format: BackupFormat,
    pub compression: BackupCompression,
    pub identity: NativeSnapshotIdentity,
    pub objects: Vec<NativeSnapshotObjectDeclaration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeSnapshot {
    pub backup_id: Uuid,
    pub upload_required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WalGObjectKind {
    BaseBackup,
    Sentinel,
    Wal,
    Metadata,
}

/// One immutable object declared by a completed WAL-G snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalGObjectDeclaration {
    /// Path relative to the repository root. Absolute paths, parent
    /// traversal, empty segments and backslashes are rejected by Cloud.
    pub relative_key: String,
    pub kind: WalGObjectKind,
    pub bytes: u64,
    /// Hex SHA-256 computed by streaming the source object once. Cloud binds
    /// it into the presigned PUT; the subsequent upload is a second bounded-
    /// memory stream and never requires a local staging file.
    pub checksum_sha256: String,
}

/// Register a physical PostgreSQL snapshot before mirroring its objects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalGSnapshotRequest {
    pub backup_id: Uuid,
    pub instance_id: Uuid,
    pub source: String,
    pub engine: BackupEngine,
    pub postgres_major: u16,
    pub postgres_system_identifier: String,
    /// Exact name returned by `wal-g backup-list`, never `LATEST`.
    pub backup_name: String,
    pub timeline: u32,
    pub start_lsn: String,
    pub finish_lsn: String,
    pub objects: Vec<WalGObjectDeclaration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalGSnapshot {
    pub backup_id: Uuid,
    pub upload_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalGObjectTargetRequest {
    pub backup_id: Uuid,
    pub instance_id: Uuid,
    pub relative_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalGObjectTarget {
    pub backup_id: Uuid,
    pub relative_key: String,
    pub upload_required: bool,
    pub upload_url: String,
    pub expires_at_millis: i64,
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalGObjectCompleted {
    pub backup_id: Uuid,
    pub relative_key: String,
    pub bytes: u64,
    pub checksum_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalGSnapshotCompleted {
    pub backup_id: Uuid,
}

// ---------------------------------------------------------------------------
// Managed AI — context is assembled and approved on the OSS instance
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedAiTask {
    Standard,
    Deep,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedAiEvidence {
    /// Tenant-keyed opaque aliases. Raw application trace/span identifiers do
    /// not cross the managed boundary.
    pub trace_id: String,
    pub span_id: String,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    /// One fixed category from the protocol allow-list, never a raw span name.
    pub operation: String,
    pub duration_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedAiAnalysisRequest {
    pub analysis_id: Uuid,
    pub question: String,
    pub range_start: chrono::DateTime<chrono::Utc>,
    pub range_end: chrono::DateTime<chrono::Utc>,
    pub context_manifest_sha256: String,
    pub task: ManagedAiTask,
    pub evidence: Vec<ManagedAiEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedAiCitation {
    pub trace_id: String,
    pub span_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedAiAnalysisResponse {
    pub id: Uuid,
    pub state: String,
    pub task: ManagedAiTask,
    pub estimated_credits: u64,
    pub settled_credits: u64,
    pub provider: String,
    pub model: String,
    pub rate_card_version: String,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub answer: Option<String>,
    pub grounded: Option<bool>,
    pub citations: Vec<ManagedAiCitation>,
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedAiCapability {
    pub configured: bool,
    pub managed_provider: Option<String>,
    pub managed_model: Option<String>,
    pub destination_origin: Option<String>,
    pub inference_region: Option<String>,
    pub reason: Option<String>,
    pub setup_path: String,
}

// ---------------------------------------------------------------------------
// Managed backups — Cloud vends a ready-to-use S3-compatible destination
// ---------------------------------------------------------------------------

/// What Cloud can hand back for offsite backup storage on this tenant's plan.
///
/// All fields beyond `configured` and `reason` are only present once
/// `configured` is `true`; a backend that has not provisioned a destination
/// yet, or a tier that does not include managed backups, returns `configured:
/// false` with a human-readable `reason` so the instance can onboard the
/// operator instead of silently doing nothing.
#[derive(Clone, Serialize, Deserialize)]
pub struct ManagedBackupCapability {
    pub configured: bool,
    pub endpoint: Option<String>,
    pub region: Option<String>,
    pub bucket_name: Option<String>,
    pub bucket_path: Option<String>,
    pub access_key_id: Option<String>,
    /// Never logged; the [`Debug`] impl below redacts it the same way
    /// [`EnrollResponse`] redacts `instance_token`.
    pub secret_key: Option<String>,
    /// STS-style session token accompanying a *temporary* credential (e.g. one
    /// minted by Cloudflare R2's Temporary Access Credentials API so it is
    /// scoped to a single object prefix). SigV4 rejects such a credential
    /// unless the token travels with it as `X-Amz-Security-Token`, so every
    /// signer and shell-out on the instance side has to carry it through.
    ///
    /// `None` for a long-lived credential — the field is additive on the wire
    /// (serde defaults a missing `Option` to `None`), so an older backend that
    /// never sends it and an operator-configured source that never has one are
    /// indistinguishable and both keep working unchanged.
    ///
    /// Never logged; redacted by the [`Debug`] impl below exactly like
    /// `secret_key`.
    #[serde(default)]
    pub session_token: Option<String>,
    /// When the vended credential stops working, so the console can say so
    /// ahead of time instead of the operator discovering it through a failed
    /// upload. `None` means "does not expire" (a long-lived credential) or "the
    /// backend did not say".
    #[serde(default)]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// e.g. "not available on Starter" — surfaced verbatim to the operator.
    pub reason: Option<String>,
}

impl std::fmt::Debug for ManagedBackupCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedBackupCapability")
            .field("configured", &self.configured)
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket_name", &self.bucket_name)
            .field("bucket_path", &self.bucket_path)
            .field("access_key_id", &self.access_key_id)
            .field(
                "secret_key",
                &self.secret_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("expires_at", &self.expires_at)
            .field("reason", &self.reason)
            .finish()
    }
}

/// One OpenAI-compatible completion proxied by Cloud for a connected OSS
/// instance. The body remains opaque at the control-plane protocol layer so
/// OpenAI-compatible additive fields do not require a protocol-version bump.
/// Cloud still validates its encoded size, message shape, and streaming mode
/// before forwarding it to the deployment-approved provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedAiChatRequest {
    pub request_id: Uuid,
    pub requested_at: chrono::DateTime<chrono::Utc>,
    pub body: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedAiChatResponse {
    pub request_id: Uuid,
    pub settled_credits: u64,
    pub provider: String,
    pub model: String,
    pub body: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Managed notifications — Cloud fans one local provider out to many sinks
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedNotificationSeverity {
    Debug,
    Info,
    Warning,
    Error,
    Critical,
    Emergency,
}

/// A bounded notification produced by OSS and durably accepted by Cloud.
///
/// The stable source id makes retries safe. Cloud never trusts the timestamp
/// for ordering, billing, or retry decisions; it records server receive time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedNotificationRequest {
    pub source_notification_id: String,
    pub title: String,
    pub message: String,
    pub severity: ManagedNotificationSeverity,
    #[serde(default)]
    pub metadata: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedNotificationAccepted {
    pub event_id: Uuid,
    pub queued_deliveries: u32,
    pub duplicate: bool,
}

// ---------------------------------------------------------------------------
// Backup lifecycle — OSS pushes started/completed/failed as they happen, so
// Cloud can show a live "processing" state instead of waiting to notice the
// result on its next mirror-sweep poll.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupLifecycleStage {
    Started,
    Completed,
    Failed,
}

/// A backup lifecycle transition reported by an instance. `instance_id`
/// binds it to the reporting instance's bearer token the same way every
/// other backup-scoped call does; `backup_id` is that instance's local
/// `backups.id`, not globally unique, so Cloud keys its own state on
/// `(instance_id, backup_id)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupLifecycleEventRequest {
    pub instance_id: Uuid,
    pub backup_id: i32,
    pub engine: String,
    pub stage: BackupLifecycleStage,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s3_location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupLifecycleEventAccepted {
    pub event_id: Uuid,
}

// ---------------------------------------------------------------------------
// Liveness
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Heartbeat {
    pub instance_id: Uuid,
    /// Reported so the cloud can keep a DNS A record current for instances
    /// with dynamic addresses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_ip: Option<String>,
    /// Best-effort ISO 3166-1 alpha-2 country inferred for the public address.
    /// This is display metadata only and is never used for authorization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country_code: Option<String>,
    /// Best-effort region/state name associated with the public address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Best-effort city associated with the public address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    /// Local spool depth. A growing value is the signal that the instance is
    /// buffering because we are failing it.
    pub pending_spool_bytes: u64,
}

/// Cloud acknowledgement for a durably recorded heartbeat.
///
/// The instance may use the Cloud timestamp for skew diagnostics, but never
/// for a local authorization or billing decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatAck {
    pub received_at_millis: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_round_trips() {
        let hb = Heartbeat {
            instance_id: Uuid::nil(),
            public_ip: None,
            country_code: None,
            region: None,
            city: None,
            pending_spool_bytes: 42,
        };
        let env = Envelope::new("heartbeat", &hb).unwrap();
        let back: Heartbeat = env.decode("heartbeat").unwrap();
        assert_eq!(back.pending_spool_bytes, 42);
    }

    #[test]
    fn heartbeat_location_fields_are_additive() {
        let heartbeat: Heartbeat = serde_json::from_value(serde_json::json!({
            "instance_id": Uuid::nil(),
            "public_ip": "203.0.113.1",
            "pending_spool_bytes": 0
        }))
        .unwrap();

        assert_eq!(heartbeat.public_ip.as_deref(), Some("203.0.113.1"));
        assert!(heartbeat.country_code.is_none());
        assert!(heartbeat.region.is_none());
        assert!(heartbeat.city.is_none());
    }

    #[test]
    fn managed_notification_round_trips_without_provider_details() {
        let request = ManagedNotificationRequest {
            source_notification_id: "alert-42".into(),
            title: "Database unavailable".into(),
            message: "postgres did not answer its health check".into(),
            severity: ManagedNotificationSeverity::Critical,
            metadata: [("service".into(), "postgres".into())].into(),
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(!json.contains("slack"));
        assert!(!json.contains("email"));
        let decoded: ManagedNotificationRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.source_notification_id, "alert-42");
    }

    #[test]
    fn heartbeat_ack_is_additive_and_round_trips() {
        let ack = HeartbeatAck {
            received_at_millis: 1_700_000_000_000,
        };
        let env = Envelope::new("heartbeat_ack", &ack).unwrap();
        let decoded: HeartbeatAck = env.decode("heartbeat_ack").unwrap();
        assert_eq!(decoded.received_at_millis, ack.received_at_millis);
    }

    #[test]
    fn unknown_kind_decodes_to_none_rather_than_erroring() {
        // The forward-compatibility guarantee: a v1 peer receiving a v3 frame
        // must be able to skip it and keep the channel open.
        let env = Envelope::new("some_future_kind", &serde_json::json!({"x": 1})).unwrap();
        assert!(env.decode::<Heartbeat>("heartbeat").is_none());
    }

    #[test]
    fn span_attributes_default_when_absent() {
        // Older instances predate `attributes`; their batches must still parse.
        let json = r#"{
            "trace_id":"t","span_id":"s","name":"GET /",
            "ts_millis":1700000000000,"duration_ms":1.5
        }"#;
        let span: SpanRecord = serde_json::from_str(json).unwrap();
        assert!(span.attributes.is_empty());
    }

    #[test]
    fn queryable_fidelity_fields_default_when_absent() {
        // ADR-040 §1: a gateway several versions ahead must still be able to
        // read a batch from an instance that predates the fidelity tiers.
        let json = r#"{
            "trace_id":"t","span_id":"s","name":"span",
            "ts_millis":1700000000000,"duration_ms":1.5
        }"#;
        let span: SpanRecord = serde_json::from_str(json).expect("legacy record must parse");
        assert_eq!(span.project_ref, "");
        assert_eq!(span.service_name, None);
        assert_eq!(span.span_kind, None);
        assert_eq!(span.status_code, None);
        assert_eq!(span.parent_span_id, None);
        assert_eq!(span.environment, None);
    }

    #[test]
    fn a_metered_record_serializes_without_any_queryable_keys() {
        // The hard compatibility invariant: at `Metered` fidelity the bytes on
        // the wire are exactly what instances shipped before ADR-040, so no
        // protocol version bump is needed and an older gateway sees no new
        // keys. `temps-otel` pins the same property against its real
        // projection; this pins it at the protocol layer.
        let metered = SpanRecord {
            trace_id: "a".repeat(64),
            span_id: "b".repeat(64),
            name: "span".into(),
            ts_millis: 1_700_000_000_000,
            duration_ms: 1.5,
            attributes: Default::default(),
            ..Default::default()
        };

        let json = serde_json::to_string(&metered).expect("record must serialize");
        assert_eq!(
            json,
            format!(
                r#"{{"trace_id":"{}","span_id":"{}","name":"span","ts_millis":1700000000000,"duration_ms":1.5,"attributes":{{}}}}"#,
                "a".repeat(64),
                "b".repeat(64)
            )
        );
    }

    #[test]
    fn a_queryable_record_round_trips_every_added_field() {
        let queryable = SpanRecord {
            trace_id: "4bf92f3577b34da6a3ce929d0e0e4736".into(),
            span_id: "00f067aa0ba902b7".into(),
            name: "GET /orders".into(),
            ts_millis: 1_700_000_000_000,
            duration_ms: 12.5,
            attributes: [("http.route".to_string(), "/orders".to_string())]
                .into_iter()
                .collect(),
            project_ref: "c".repeat(64),
            service_name: Some("checkout".into()),
            span_kind: Some("SERVER".into()),
            status_code: Some("ERROR".into()),
            parent_span_id: Some("00f067aa0ba902b6".into()),
            environment: Some("production".into()),
        };

        let encoded = serde_json::to_string(&queryable).expect("record must serialize");
        let decoded: SpanRecord = serde_json::from_str(&encoded).expect("record must parse");
        assert_eq!(decoded, queryable);
    }

    #[test]
    fn ingest_ack_omits_warning_when_healthy() {
        let ack = IngestAck {
            submission_id: Uuid::new_v4(),
            processed_spans: 3,
            stored_spans: 3,
            metered_bytes: 100,
            warning: None,
        };
        let s = serde_json::to_string(&ack).unwrap();
        assert!(
            !s.contains("warning"),
            "healthy ack should not carry a warning key: {s}"
        );
    }

    #[test]
    fn enrollment_debug_output_redacts_credentials() {
        let request = EnrollRequest {
            enrollment_code: "secret-code".into(),
            instance_id: Uuid::new_v4(),
            agent_version: "test".into(),
        };
        let response = EnrollResponse {
            tenant_id: Uuid::new_v4(),
            account_email: Some("owner@example.com".into()),
            instance_token: "inst_secret".into(),
            capabilities: vec![],
        };

        assert!(!format!("{request:?}").contains("secret-code"));
        assert!(!format!("{response:?}").contains("inst_secret"));
    }

    #[test]
    fn managed_backup_capability_debug_output_redacts_the_secret_key() {
        let capability = ManagedBackupCapability {
            configured: true,
            endpoint: Some("https://objects.example.com".into()),
            region: Some("auto".into()),
            bucket_name: Some("tenant-bucket".into()),
            bucket_path: Some("tenant-42".into()),
            access_key_id: Some("AKIA-VISIBLE".into()),
            secret_key: Some("super-secret-value".into()),
            session_token: None,
            expires_at: None,
            reason: None,
        };

        let debug_output = format!("{capability:?}");
        assert!(!debug_output.contains("super-secret-value"));
        assert!(debug_output.contains("AKIA-VISIBLE"));
    }

    #[test]
    fn managed_backup_capability_debug_output_redacts_the_session_token() {
        let capability = ManagedBackupCapability {
            configured: true,
            endpoint: Some("https://objects.example.com".into()),
            region: Some("auto".into()),
            bucket_name: Some("tenant-bucket".into()),
            bucket_path: Some("tenant-42".into()),
            access_key_id: Some("AKIA-VISIBLE".into()),
            secret_key: Some("super-secret-value".into()),
            session_token: Some("super-secret-session-token".into()),
            expires_at: Some(
                chrono::DateTime::parse_from_rfc3339("2026-09-03T00:00:00Z")
                    .expect("valid timestamp")
                    .with_timezone(&chrono::Utc),
            ),
            reason: None,
        };

        let debug_output = format!("{capability:?}");
        assert!(!debug_output.contains("super-secret-session-token"));
        assert!(!debug_output.contains("super-secret-value"));
        assert!(debug_output.contains("session_token: Some(\"[REDACTED]\")"));
        // The expiry itself is not a secret — the console needs to show it.
        assert!(debug_output.contains("2026-09-03"));
    }

    #[test]
    fn managed_backup_capability_not_configured_round_trips_with_a_reason() {
        let capability = ManagedBackupCapability {
            configured: false,
            endpoint: None,
            region: None,
            bucket_name: None,
            bucket_path: None,
            access_key_id: None,
            secret_key: None,
            session_token: None,
            expires_at: None,
            reason: Some("not available on Starter".into()),
        };
        let json = serde_json::to_string(&capability).unwrap();
        let decoded: ManagedBackupCapability = serde_json::from_str(&json).unwrap();
        assert!(!decoded.configured);
        assert_eq!(decoded.reason.as_deref(), Some("not available on Starter"));
    }

    /// A backend that predates the temporary-credential work omits both new
    /// fields entirely. That must decode to `None`/`None` rather than fail —
    /// it is the wire half of the "an unconfigured source is untouched"
    /// guarantee, and it is what lets Cloud ship the endpoint before every
    /// instance has repinned.
    #[test]
    fn managed_backup_capability_defaults_the_temporary_credential_fields_when_absent() {
        let decoded: ManagedBackupCapability = serde_json::from_value(serde_json::json!({
            "configured": true,
            "endpoint": "https://objects.example.com",
            "region": "auto",
            "bucket_name": "tenant-bucket",
            "bucket_path": "tenant-42",
            "access_key_id": "AKIA-VISIBLE",
            "secret_key": "long-lived",
            "reason": null,
        }))
        .expect("a payload without the temporary-credential fields must still decode");

        assert!(decoded.session_token.is_none());
        assert!(decoded.expires_at.is_none());
    }

    #[test]
    fn managed_backup_capability_round_trips_a_temporary_credential() {
        let expires_at = chrono::DateTime::parse_from_rfc3339("2026-09-03T12:34:56Z")
            .expect("valid timestamp")
            .with_timezone(&chrono::Utc);
        let capability = ManagedBackupCapability {
            configured: true,
            endpoint: Some("https://objects.example.com".into()),
            region: Some("auto".into()),
            bucket_name: Some("tenant-bucket".into()),
            bucket_path: Some("tenants/42/instances/7/managed-backups/".into()),
            access_key_id: Some("AKIA-VISIBLE".into()),
            secret_key: Some("secret".into()),
            session_token: Some("session-token".into()),
            expires_at: Some(expires_at),
            reason: None,
        };

        let json = serde_json::to_string(&capability).expect("serialize");
        let decoded: ManagedBackupCapability = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(decoded.session_token.as_deref(), Some("session-token"));
        assert_eq!(decoded.expires_at, Some(expires_at));
    }

    #[test]
    fn enrollment_response_accepts_backends_without_an_account_email() {
        let tenant_id = Uuid::new_v4();
        let response: EnrollResponse = serde_json::from_value(serde_json::json!({
            "tenant_id": tenant_id,
            "instance_token": "inst_legacy"
        }))
        .unwrap();

        assert_eq!(response.tenant_id, tenant_id);
        assert!(response.account_email.is_none());
        assert!(response.capabilities.is_empty());
    }

    #[test]
    fn walg_snapshot_contract_preserves_repository_identity() {
        let request = WalGSnapshotRequest {
            backup_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            source: "postgres/main".into(),
            engine: BackupEngine::Postgres,
            postgres_major: 18,
            postgres_system_identifier: "758329011337".into(),
            backup_name: "base_00000001000000000000000A".into(),
            timeline: 1,
            start_lsn: "0/A000028".into(),
            finish_lsn: "0/B000090".into(),
            objects: vec![WalGObjectDeclaration {
                relative_key:
                    "basebackups_005/base_00000001000000000000000A_backup_stop_sentinel.json".into(),
                kind: WalGObjectKind::Sentinel,
                bytes: 512,
                checksum_sha256: "00".repeat(32),
            }],
        };
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["engine"], "postgres");
        assert_eq!(value["objects"][0]["kind"], "sentinel");
        assert_eq!(value["timeline"], 1);
    }

    #[test]
    fn native_mongodb_snapshot_has_a_tagged_restore_identity() {
        let request = NativeSnapshotRequest {
            backup_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            source: "mongodb/primary".into(),
            engine: BackupEngine::MongoDb,
            format: BackupFormat::MongoDumpArchive,
            compression: BackupCompression::WalGNative,
            identity: NativeSnapshotIdentity::MongoDbStream {
                engine_version: "8.0.12".into(),
                backup_name: "mongo-2026-08-08T10:00:00Z".into(),
            },
            objects: vec![NativeSnapshotObjectDeclaration {
                relative_key: "streams/mongodb.archive.lz4".into(),
                kind: NativeSnapshotObjectKind::Data,
                bytes: 1_024,
                checksum_sha256: "ab".repeat(32),
            }],
        };

        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["engine"], "mongo_db");
        assert_eq!(value["format"], "mongo_dump_archive");
        assert_eq!(value["identity"]["kind"], "mongo_db_stream");
        assert_eq!(value["objects"][0]["kind"], "data");
    }

    #[test]
    fn native_snapshot_wire_names_cover_all_new_engines() {
        let cases = [
            (BackupEngine::Redis, "redis"),
            (BackupEngine::MariaDb, "maria_db"),
            (BackupEngine::RustFs, "rust_fs"),
        ];

        for (engine, expected) in cases {
            assert_eq!(serde_json::to_value(engine).unwrap(), expected);
        }
        assert_eq!(
            serde_json::to_value(BackupFormat::ObjectSet).unwrap(),
            "object_set"
        );
    }
}
