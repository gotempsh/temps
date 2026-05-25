use crate::engines::dispatch::{resolve_engine_key, ResolveEngineError};
use crate::handlers::audit::{
    AuditContext, BackupRunAudit, BackupScheduleStatusChangedAudit, BackupScheduleUpdatedAudit,
    ExternalServiceBackupRunAudit, S3SourceCreatedAudit, S3SourceDeletedAudit,
    S3SourceUpdatedAudit, ScheduleRunNowAudit, ScheduleServiceDetachedAudit,
    ScheduleServicesAttachedAudit,
};
use crate::handlers::types::BackupAppState;
use crate::services::BackupTriggerParams;
use crate::services::{
    BackupError, ChildBackupEntry, EnqueuedJob, ScheduleRunEntry, ScheduleRunJobEntry,
    ScheduleRunListResponse, ScheduleRunResponse, ScheduleRunSummary, ScheduleRunSummaryList,
};
use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, patch, post},
    Json, Router,
};
use sea_orm::{DatabaseBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use temps_auth::permission_guard;
use temps_auth::RequireAuth;
use temps_core::problemdetails;
use temps_core::problemdetails::{Problem, ProblemDetails};
use temps_core::RequestMetadata;
use tracing::error;
use utoipa::{OpenApi, ToSchema};

impl From<ResolveEngineError> for Problem {
    fn from(error: ResolveEngineError) -> Self {
        match error {
            ResolveEngineError::Unsupported { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Unsupported Service Type")
                .with_detail(error.to_string()),
            ResolveEngineError::WalgProbeFailed { .. } => {
                // Probe failure is non-fatal: caller should retry or fall back.
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Engine Detection Failed")
                    .with_detail(error.to_string())
            }
        }
    }
}

impl From<BackupError> for Problem {
    fn from(error: BackupError) -> Self {
        match error {
            BackupError::NotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Resource Not Found")
                .with_detail(error.to_string()),

            BackupError::Validation(ref msg) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Error")
                .with_detail(msg.clone()),

            BackupError::Schedule(ref msg) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Schedule Error")
                .with_detail(msg.clone()),

            BackupError::AlreadyInFlight { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Backup Already In Flight")
                .with_detail(error.to_string()),

            BackupError::ScheduleRunAlreadyInFlight { existing_run_id } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Schedule Run Already In Flight")
                    .with_detail(format!(
                        "A run for this schedule is already in flight \
                         (existing run id: {}). Wait for it to finish \
                         before triggering a new run.",
                        existing_run_id
                    ))
            }

            BackupError::Database(_)
            | BackupError::S3(_)
            | BackupError::Configuration(_)
            | BackupError::ExternalService(_)
            | BackupError::Internal { .. }
            | BackupError::NotificationError(_)
            | BackupError::Unsupported(_)
            | BackupError::Io(_)
            | BackupError::Serialization(_) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_s3_sources,
        create_s3_source,
        get_s3_source,
        update_s3_source,
        delete_s3_source,
        set_default_s3_source,
        test_s3_source_connection,
        test_s3_connection_preview,
        list_backup_schedules,
        create_backup_schedule,
        get_backup_schedule,
        delete_backup_schedule,
        list_backups_for_schedule,
        list_schedule_runs,
        list_schedule_run_jobs,
        run_backup_for_source,
        run_schedule_now,
        cancel_backup,
        cancel_schedule_run,
        list_source_backups,
        list_external_service_backups,
        get_backup,
        list_backup_children,
        disable_backup_schedule,
        enable_backup_schedule,
        update_backup_schedule,
        run_external_service_backup,
        list_backup_alerts,
        list_schedule_services,
        attach_schedule_services,
        detach_schedule_service,
        list_service_schedules
    ),
    components(
        schemas(
            CreateS3SourceRequest,
            UpdateS3SourceRequest,
            CreateBackupScheduleRequest,
            UpdateBackupScheduleRequest,
            RunBackupRequest,
            RunExternalServiceBackupRequest,
            S3SourceResponse,
            S3ConnectionTestResponse,
            BackupScheduleResponse,
            BackupResponse,
            ExternalServiceSummary,
            ExternalServiceBackupResponse,
            SourceBackupIndexResponse,
            SourceBackupEntry,
            ServiceBackupListResponse,
            ServiceBackupEntryResponse,
            BackupAlertResponse,
            BackupAlertListResponse,
            ScheduleRunEntry,
            ScheduleRunListResponse,
            ScheduleRunSummary,
            ScheduleRunSummaryList,
            ScheduleRunJobEntry,
            ScheduleRunResponse,
            EnqueuedJob,
            CancelBackupResponse,
            ChildBackupEntryResponse,
            ChildBackupListResponse,
            AttachScheduleServicesRequest,
            AttachScheduleServicesResponse,
        )
    ),
    info(
        title = "Backups API",
        description = "API endpoints for managing backup operations and schedules. \
        Handles S3 source configuration, backup scheduling, execution, and monitoring.",
        version = "1.0.0"
    ),
    tags(
        (name = "Backups", description = "Backup management endpoints")
    )
)]
pub struct BackupApiDoc;

#[derive(Deserialize, ToSchema, Clone)]
pub struct CreateS3SourceRequest {
    pub name: String,
    pub bucket_name: String,
    pub bucket_path: String,
    pub access_key_id: String,
    pub secret_key: String,
    pub region: String,
    /// Optional endpoint URL for S3-compatible services like MinIO
    #[schema(example = "http://minio.example.com:9000")]
    pub endpoint: Option<String>,
    /// Whether to use path-style addressing (default: true)
    #[schema(example = true)]
    pub force_path_style: Option<bool>,
    /// When true, make this the default source (will swap out any existing default).
    /// The very first S3 source is always created as default regardless of this flag.
    #[schema(example = false)]
    pub is_default: Option<bool>,
}

#[derive(Deserialize, ToSchema, Clone)]
pub struct UpdateS3SourceRequest {
    /// Optional new name for the source
    pub name: Option<String>,
    /// Optional new bucket name
    pub bucket_name: Option<String>,
    /// Optional new bucket path
    pub bucket_path: Option<String>,
    /// Optional new access key ID
    #[schema(example = "AKIAXXXXXXXXXXXXXXXX")]
    pub access_key_id: Option<String>,
    /// Optional new secret key
    pub secret_key: Option<String>,
    /// Optional new region
    pub region: Option<String>,
    /// Optional new endpoint URL for S3-compatible services
    #[schema(example = "http://minio.example.com:9000")]
    pub endpoint: Option<String>,
    /// Optional new path-style addressing setting
    #[schema(example = true)]
    pub force_path_style: Option<bool>,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateBackupScheduleRequest {
    pub name: String,
    pub backup_type: String,
    pub retention_period: i32,
    /// Optional S3 source. If omitted, the current default S3 source is used.
    pub s3_source_id: Option<i32>,
    pub schedule_expression: String,
    pub enabled: bool,
    pub description: Option<String>,
    pub tags: Vec<String>,
    /// Optional wall-clock timeout override for jobs created by this schedule
    /// (seconds). When set, overrides the engine-family default. `null` means
    /// "use engine default." The per-job `max_runtime_secs` in
    /// `EnqueueJobParams` can still override this for ad-hoc triggers.
    pub max_runtime_secs: Option<i64>,
    /// When `true` (default), the schedule backs up every external service
    /// on the host — including databases created in the future. When
    /// `false`, the schedule backs up only the services explicitly attached
    /// via `POST /backups/schedules/{id}/services`. Omit to use the default.
    #[serde(default)]
    pub target_all_services: Option<bool>,
    /// When `true` (default), every run also produces a `control_plane`
    /// backup of Temps's own database. Operators who use Temps purely as
    /// a backup orchestrator for external DBs can set this to `false` to
    /// keep the run history focused on those services.
    #[serde(default)]
    pub include_control_plane: Option<bool>,
}

/// Deserializer for `Option<Option<i64>>` that maps:
/// - field absent → `None` (leave the column unchanged)
/// - field present with JSON `null` → `Some(None)` (clear the column)
/// - field present with a number → `Some(Some(n))` (set to value)
fn deserialize_optional_optional_i64<'de, D>(
    deserializer: D,
) -> Result<Option<Option<i64>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // `Option<Option<i64>>` with serde's standard behaviour only handles
    // absent vs. `null` at the outermost level; nested `null` → `Some(None)`
    // requires a custom impl.
    let outer: Option<serde_json::Value> = serde::Deserialize::deserialize(deserializer)?;
    match outer {
        None => Ok(None),
        Some(serde_json::Value::Null) => Ok(Some(None)),
        Some(v) => {
            let n: i64 = serde_json::from_value(v).map_err(serde::de::Error::custom)?;
            Ok(Some(Some(n)))
        }
    }
}

/// Request body for updating an existing backup schedule via `PATCH /api/backups/schedules/{id}`.
///
/// All fields are optional; only present fields are updated. Absent fields
/// leave the corresponding column unchanged.
#[derive(Debug, Deserialize, ToSchema, Clone)]
pub struct UpdateBackupScheduleRequest {
    /// New schedule name. Skipped when `None`. Must not be empty if provided.
    pub name: Option<String>,
    /// New human-readable description. Pass an empty string `""` to clear.
    pub description: Option<String>,
    /// New cron expression. When changed, `next_run` is recomputed.
    pub schedule_expression: Option<String>,
    /// Days to retain backups produced by this schedule. Must be >= 1.
    pub retention_period: Option<i32>,
    /// Per-schedule wall-clock timeout override (seconds).
    ///
    /// - `None` (field absent) — leave current value unchanged
    /// - `Some(None)` (field present, JSON `null`) — clear override; fall back to engine default
    /// - `Some(Some(n))` — set to `n` seconds (must be >= 60)
    #[serde(default, deserialize_with = "deserialize_optional_optional_i64")]
    pub max_runtime_secs: Option<Option<i64>>,
    /// Enable or disable the schedule. Skipped when `None`.
    pub enabled: Option<bool>,
    /// Replace the full tag list. Skipped when `None`.
    pub tags: Option<Vec<String>>,
    /// Toggle between "back up every database" (`true`) and "back up only
    /// the explicit list" (`false`). When set to `true`, the server clears
    /// the explicit membership rows for this schedule.
    pub target_all_services: Option<bool>,
    /// Toggle whether the control-plane backup is produced on every run.
    pub include_control_plane: Option<bool>,
}

/// Returns the names of fields that are present (i.e., `Some`) in the patch
/// request, for inclusion in the audit log.
fn changed_fields_for_audit(request: &UpdateBackupScheduleRequest) -> Vec<String> {
    let mut fields = Vec::new();
    if request.name.is_some() {
        fields.push("name".to_string());
    }
    if request.description.is_some() {
        fields.push("description".to_string());
    }
    if request.schedule_expression.is_some() {
        fields.push("schedule_expression".to_string());
    }
    if request.retention_period.is_some() {
        fields.push("retention_period".to_string());
    }
    if request.max_runtime_secs.is_some() {
        fields.push("max_runtime_secs".to_string());
    }
    if request.enabled.is_some() {
        fields.push("enabled".to_string());
    }
    if request.tags.is_some() {
        fields.push("tags".to_string());
    }
    if request.target_all_services.is_some() {
        fields.push("target_all_services".to_string());
    }
    if request.include_control_plane.is_some() {
        fields.push("include_control_plane".to_string());
    }
    fields
}

#[derive(Deserialize, ToSchema, Clone)]
pub struct RunBackupRequest {
    /// Type of backup to perform
    #[schema(example = "full")]
    pub backup_type: String,
}

#[derive(Deserialize, ToSchema, Clone)]
pub struct RunExternalServiceBackupRequest {
    /// ID of the S3 source to store the backup. If omitted, the current default S3 source is used.
    #[schema(example = 1)]
    pub s3_source_id: Option<i32>,
    /// Type of backup to perform (e.g., "full", "incremental")
    #[schema(example = "full")]
    pub backup_type: Option<String>,
}

/// Response type for external service backup
#[derive(Debug, Serialize, ToSchema)]
pub struct ExternalServiceBackupResponse {
    pub id: i32,
    pub service_id: i32,
    pub backup_id: i32,
    pub backup_type: String,
    pub state: String,
    #[schema(example = "2025-01-15T14:30:00.123Z")]
    pub started_at: String,
    #[schema(example = "2025-01-15T14:35:00.456Z")]
    pub finished_at: Option<String>,
    pub size_bytes: Option<i64>,
    pub s3_location: String,
    pub error_message: Option<String>,
    pub metadata: serde_json::Value,
    pub checksum: Option<String>,
    pub compression_type: String,
    pub created_by: i32,
    #[schema(example = "2025-02-15T14:30:00.123Z")]
    pub expires_at: Option<String>,
}

impl From<temps_entities::external_service_backups::Model> for ExternalServiceBackupResponse {
    fn from(backup: temps_entities::external_service_backups::Model) -> Self {
        Self {
            id: backup.id,
            service_id: backup.service_id,
            backup_id: backup.backup_id,
            backup_type: backup.backup_type,
            state: backup.state,
            started_at: backup.started_at.to_rfc3339(),
            finished_at: backup.finished_at.map(|dt| dt.to_rfc3339()),
            size_bytes: backup.size_bytes,
            s3_location: backup.s3_location,
            error_message: backup.error_message,
            metadata: backup.metadata.clone(),
            checksum: backup.checksum,
            compression_type: backup.compression_type,
            created_by: backup.created_by,
            expires_at: backup.expires_at.map(|dt| dt.to_rfc3339()),
        }
    }
}

/// A single child backup entry in the `GET /backups/{id}/children` response.
///
/// Each entry corresponds to one `external_service_backups` row joined with
/// `external_services`, providing service metadata without a second request.
#[derive(Debug, Serialize, ToSchema)]
pub struct ChildBackupEntryResponse {
    /// Row ID from `external_service_backups`.
    pub id: i32,
    /// FK to `external_services.id`.
    pub service_id: i32,
    /// Human-readable name of the external service (e.g. "redis-prod").
    pub service_name: String,
    /// Service type string (e.g. "postgres", "redis", "mongodb", "s3").
    #[schema(example = "postgres")]
    pub service_type: String,
    /// Current state: "pending" | "running" | "completed" | "failed".
    pub state: String,
    /// Backup variant (e.g. "full", "incremental").
    pub backup_type: String,
    /// When the child backup started (RFC 3339).
    #[schema(example = "2025-01-15T14:30:00.123Z")]
    pub started_at: String,
    /// When the child backup finished, if known.
    #[schema(example = "2025-01-15T14:35:00.456Z")]
    pub finished_at: Option<String>,
    /// Size of the child backup in bytes, if available.
    pub size_bytes: Option<i64>,
    /// Object key or `s3://` URL where the backup data lives.
    pub s3_location: String,
    /// Engine-reported error message when `state = "failed"`.
    pub error_message: Option<String>,
    /// Compression algorithm used (e.g. "gzip", "lz4").
    pub compression_type: String,
}

impl From<ChildBackupEntry> for ChildBackupEntryResponse {
    fn from(entry: ChildBackupEntry) -> Self {
        Self {
            id: entry.id,
            service_id: entry.service_id,
            service_name: entry.service_name,
            service_type: entry.service_type,
            state: entry.state,
            backup_type: entry.backup_type,
            started_at: entry.started_at.to_rfc3339(),
            finished_at: entry.finished_at.map(|dt| dt.to_rfc3339()),
            size_bytes: entry.size_bytes,
            s3_location: entry.s3_location,
            error_message: entry.error_message,
            compression_type: entry.compression_type,
        }
    }
}

/// Response body for `GET /backups/{id}/children`.
///
/// Returns an empty `children` list (not 404) when the parent backup has no
/// child records (e.g. control-plane backups).
#[derive(Debug, Serialize, ToSchema)]
pub struct ChildBackupListResponse {
    /// Zero or more child backup entries ordered by `external_service_backups.id` ASC.
    pub children: Vec<ChildBackupEntryResponse>,
}

/// Response type for S3 source
#[derive(Serialize, ToSchema)]
pub struct S3SourceResponse {
    pub id: i32,
    pub name: String,
    pub bucket_name: String,
    pub bucket_path: String,
    #[schema(example = "AKIAXXXXXXXXXXXXXXXX")]
    pub access_key_id: String,
    #[schema(write_only)]
    pub secret_key: String,
    pub region: String,
    #[schema(example = "http://minio.example.com:9000")]
    pub endpoint: Option<String>,
    pub force_path_style: Option<bool>,
    pub is_default: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Response body for an S3 connection test.
#[derive(Serialize, ToSchema)]
pub struct S3ConnectionTestResponse {
    /// Whether the connection and credentials worked.
    pub ok: bool,
    /// Human-readable message (success confirmation or error detail).
    pub message: String,
}

/// Response type for backup schedule
#[derive(Serialize, ToSchema)]
pub struct BackupScheduleResponse {
    pub id: i32,
    pub name: String,
    pub backup_type: String,
    pub retention_period: i32,
    pub s3_source_id: i32,
    #[schema(example = "0 0 * * *")]
    pub schedule_expression: String,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub next_run: Option<i64>,
    pub last_run: Option<i64>,
    /// Per-schedule wall-clock timeout override for backup jobs (seconds).
    /// `null` means the engine-family default is used. See
    /// `temps_backup_core::timeouts::default_max_runtime_secs`.
    pub max_runtime_secs: Option<i64>,
    /// When `true`, the schedule auto-includes every external service on
    /// the host (and any future ones). When `false`, the schedule only
    /// targets services attached via `backup_schedule_services`.
    pub target_all_services: bool,
    /// When `true`, every run also produces a `control_plane` backup
    /// (Temps's own Postgres). When `false`, only the external service
    /// fan-out happens.
    pub include_control_plane: bool,
}

/// Body for `POST /api/backups/schedules/{id}/services` — attach external
/// services to a backup schedule. Idempotent.
#[derive(Debug, Deserialize, ToSchema)]
pub struct AttachScheduleServicesRequest {
    /// External service ids to attach. Duplicates are de-duplicated server-side.
    pub service_ids: Vec<i32>,
}

/// Response for `POST /api/backups/schedules/{id}/services`.
#[derive(Debug, Serialize, ToSchema)]
pub struct AttachScheduleServicesResponse {
    /// Number of rows actually inserted (excludes rows skipped by
    /// `ON CONFLICT DO NOTHING`).
    pub inserted: u64,
    /// Total number of services now attached to the schedule.
    pub total_attached: usize,
}

/// Summary of the external service that owns a backup. Only populated for
/// external-service backups (Redis, Postgres, etc.); absent for control-plane
/// backups.
#[derive(Debug, Serialize, ToSchema)]
pub struct ExternalServiceSummary {
    /// Database id of the external service.
    pub id: i32,
    /// Human-readable service name (e.g. "redis-prod").
    pub name: String,
    /// Service type string (e.g. "postgres", "redis", "mongodb").
    #[schema(example = "postgres")]
    pub service_type: String,
}

/// Response type for backup
#[derive(Debug, Serialize, ToSchema)]
pub struct BackupResponse {
    pub id: i32,
    pub name: String,
    pub backup_id: String,
    pub schedule_id: Option<i32>,
    pub backup_type: String,
    pub state: String,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    /// Final size of the backup once completed. Null while running.
    pub size_bytes: Option<i64>,
    /// Best-effort partial size while a backup is still running, computed
    /// by listing the S3 prefix. Null when the backup is finished
    /// (`size_bytes` is authoritative in that case).
    pub live_size_bytes: Option<i64>,
    pub file_count: Option<i32>,
    pub s3_source_id: i32,
    pub s3_location: String,
    pub error_message: Option<String>,
    pub metadata: serde_json::Value,
    pub checksum: Option<String>,
    pub compression_type: String,
    pub created_by: i32,
    pub expires_at: Option<i64>,
    pub tags: Vec<String>,
    /// External service that owns this backup (Redis, Postgres, etc.).
    /// `null` for control-plane backups (the Temps server's own database).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_service: Option<ExternalServiceSummary>,
    /// Name of the engine step currently executing (e.g., `"walg_push"`).
    /// `null` when no `backup_jobs` row exists for this backup (legacy rows
    /// pre-dating ADR-014), or when the job has not yet completed its first step.
    pub current_step: Option<String>,
    /// How many times this job has been claimed and run. `null` for legacy
    /// backups with no `backup_jobs` row.
    pub attempts: Option<i32>,
    /// Maximum attempts before the job is permanently failed. `null` for
    /// legacy backups.
    pub max_attempts: Option<i32>,
    /// Resolved wall-clock timeout for this backup job (seconds). `null` for
    /// legacy backups. Derived from the three-tier resolution order:
    /// caller override → schedule override → engine default.
    pub max_runtime_secs: Option<i64>,
}

/// Response type for source backup index
#[derive(Serialize, ToSchema)]
pub struct SourceBackupIndexResponse {
    /// List of backups in the source
    pub backups: Vec<SourceBackupEntry>,
    /// When the index was last updated
    #[schema(example = "2024-01-15T14:30:00.123Z")]
    pub last_updated: String,
}

/// Entry in the source backup index. Covers both DB-tracked backups
/// (have a row in `backups`) and S3-scan discoveries (raw S3 objects with
/// no DB row — used for disaster-recovery from another Temps instance).
#[derive(Serialize, ToSchema)]
pub struct SourceBackupEntry {
    /// DB row id. Zero for S3-scan entries that have no DB row.
    #[schema(example = 1)]
    pub id: i32,
    /// UUID identifier from the DB row. Empty for S3-scan entries.
    #[schema(example = "550e8400-e29b-41d4-a716-446655440000")]
    pub backup_id: String,
    /// Human-friendly display name ("postgres backup (svc-name)" for DB
    /// rows, or a synthesized label derived from the S3 path for scans).
    #[schema(example = "postgres backup (postgres-n4ea)")]
    pub name: String,
    /// Backup variant as recorded by the backup pipeline (e.g. "full").
    #[schema(example = "full")]
    pub backup_type: String,
    /// When the backup was created. For S3-scan entries this is the
    /// object's LastModified time.
    #[schema(example = "2024-01-15T14:30:00.123Z")]
    pub created_at: String,
    /// Size of the backup in bytes, if known.
    #[schema(example = 1024000)]
    pub size_bytes: Option<i64>,
    /// Raw S3 URL / key where the backup sits. For Postgres WAL-G backups
    /// this starts with `s3://`; for pg_dump-style backups it's the
    /// relative object key.
    #[schema(example = "s3://bucket/external_services/postgres/svc-name/walg")]
    pub location: String,
    /// Sidecar metadata.json location, if any. Empty when none.
    #[schema(example = "")]
    pub metadata_location: String,
    /// Engine that produced the backup ("postgres", "redis", "mongodb",
    /// "s3", "rustfs"). Used by the UI to mark engine-compat with the
    /// target service.
    #[schema(example = "postgres")]
    pub engine: Option<String>,
    /// Name of the service that produced the backup. For S3-scan entries
    /// this is parsed from the S3 path.
    #[schema(example = "postgres-n4ea")]
    pub origin_service_name: Option<String>,
    /// Storage format: "walg" for continuous-archive (PITR-capable),
    /// "pg_dump" for point-in-time dumps, "" for non-postgres.
    #[schema(example = "walg")]
    pub format: Option<String>,
    /// Provenance: "db" for rows in this Temps, "s3_scan" for objects
    /// discovered by the S3 bucket walk (e.g., backups made by another
    /// Temps instance).
    #[schema(example = "db")]
    pub source: String,
    /// Observed state ("completed", "running", "failed") — DB only.
    /// Empty string for S3-scan entries.
    #[schema(example = "completed")]
    pub state: String,
}

impl From<temps_entities::s3_sources::Model> for S3SourceResponse {
    fn from(source: temps_entities::s3_sources::Model) -> Self {
        Self {
            id: source.id,
            name: source.name,
            bucket_name: source.bucket_name,
            bucket_path: source.bucket_path,
            // Credentials are encrypted at rest — mask them in API responses
            access_key_id: "***".to_string(),
            secret_key: "***".to_string(),
            region: source.region,
            endpoint: source.endpoint,
            force_path_style: source.force_path_style,
            is_default: source.is_default,
            created_at: source.created_at.timestamp_millis(),
            updated_at: source.updated_at.timestamp_millis(),
        }
    }
}

impl From<temps_entities::backup_schedules::Model> for BackupScheduleResponse {
    fn from(schedule: temps_entities::backup_schedules::Model) -> Self {
        Self {
            id: schedule.id,
            name: schedule.name,
            backup_type: schedule.backup_type,
            retention_period: schedule.retention_period,
            s3_source_id: schedule.s3_source_id,
            schedule_expression: schedule.schedule_expression,
            enabled: schedule.enabled,
            created_at: schedule.created_at.timestamp_millis(),
            updated_at: schedule.updated_at.timestamp_millis(),
            description: schedule.description,
            tags: serde_json::from_str(&schedule.tags).unwrap_or_default(),
            next_run: schedule.next_run.map(|dt| dt.timestamp_millis()),
            last_run: schedule.last_run.map(|dt| dt.timestamp_millis()),
            max_runtime_secs: schedule.max_runtime_secs,
            target_all_services: schedule.target_all_services,
            include_control_plane: schedule.include_control_plane,
        }
    }
}

impl From<temps_entities::external_services::Model> for ExternalServiceSummary {
    fn from(svc: temps_entities::external_services::Model) -> Self {
        Self {
            id: svc.id,
            name: svc.name,
            service_type: svc.service_type,
        }
    }
}

impl From<temps_entities::backups::Model> for BackupResponse {
    fn from(backup: temps_entities::backups::Model) -> Self {
        Self {
            id: backup.id,
            name: backup.name,
            backup_id: backup.backup_id,
            schedule_id: backup.schedule_id,
            backup_type: backup.backup_type,
            state: backup.state,
            started_at: backup.started_at.timestamp_millis(),
            completed_at: backup.finished_at.map(|dt| dt.timestamp_millis()),
            size_bytes: backup.size_bytes,
            live_size_bytes: None,
            file_count: backup.file_count,
            s3_source_id: backup.s3_source_id,
            s3_location: backup.s3_location,
            error_message: backup.error_message,
            metadata: serde_json::from_str(&backup.metadata).unwrap_or_default(),
            checksum: backup.checksum,
            compression_type: backup.compression_type,
            created_by: backup.created_by,
            expires_at: backup.expires_at.map(|dt| dt.timestamp_millis()),
            tags: serde_json::from_str(&backup.tags).unwrap_or_default(),
            // Populated by the handler when the linked external service is available.
            external_service: None,
            // Populated by the handler via get_latest_job_for_backup.
            current_step: None,
            attempts: None,
            max_attempts: None,
            max_runtime_secs: None,
        }
    }
}

#[derive(Deserialize)]
struct S3BackupIndex {
    backups: Vec<S3BackupEntry>,
    last_updated: String,
}

#[derive(Deserialize)]
struct S3BackupEntry {
    #[serde(default)]
    id: i32,
    #[serde(default)]
    backup_id: String,
    name: String,
    #[serde(rename = "type")]
    backup_type: String,
    created_at: String,
    #[serde(default)]
    size_bytes: Option<i64>,
    #[serde(default)]
    location: String,
    #[serde(default)]
    metadata_location: String,
    // Newly surfaced fields — older cached JSON may not include them, so
    // `#[serde(default)]` keeps backwards compat with any legacy index.json.
    #[serde(default)]
    engine: Option<String>,
    #[serde(default)]
    origin_service_name: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default = "default_backup_source")]
    source: String,
    #[serde(default)]
    state: String,
}

fn default_backup_source() -> String {
    "db".to_string()
}

/// Query parameters for the S3-source backup listing.
#[derive(serde::Deserialize, utoipa::IntoParams)]
struct ListSourceBackupsParams {
    /// When `true`, scan the S3 bucket for backups not tracked in the
    /// local database (useful after disaster-recovery from another Temps
    /// instance).  Defaults to `false` — the fast DB-only path.
    #[serde(default)]
    include_s3_scan: bool,
}

/// List all backups in an S3 source
#[utoipa::path(
    tag = "Backups",
    get,
    path = "/backups/s3-sources/{id}/backups",
    params(ListSourceBackupsParams),
    responses(
        (status = 200, description = "List of all backups in the source", body = SourceBackupIndexResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 404, description = "S3 source not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn list_source_backups(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
    axum::extract::Query(params): axum::extract::Query<ListSourceBackupsParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);

    let index = app_state
        .backup_service
        .list_source_backups(id, params.include_s3_scan)
        .await
        .map_err(Problem::from)?;

    let s3_index: S3BackupIndex = serde_json::from_value(index).map_err(|e| {
        error!("Failed to parse backup index: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let response = SourceBackupIndexResponse {
        backups: s3_index
            .backups
            .into_iter()
            .map(|entry| SourceBackupEntry {
                id: entry.id,
                backup_id: entry.backup_id,
                name: entry.name,
                backup_type: entry.backup_type,
                created_at: entry.created_at,
                size_bytes: entry.size_bytes,
                location: entry.location,
                metadata_location: entry.metadata_location,
                engine: entry.engine,
                origin_service_name: entry.origin_service_name,
                format: entry.format,
                source: entry.source,
                state: entry.state,
            })
            .collect(),
        last_updated: s3_index.last_updated,
    };
    Ok(Json(response))
}

/// Paginated list of backups for a specific external service.
///
/// Returned by `GET /backups/external-services/{service_id}/backups`.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct ServiceBackupListResponse {
    /// Backups belonging to this service, newest first.
    pub backups: Vec<ServiceBackupEntryResponse>,
    /// Total number of backups for this service across all pages.
    pub total: i64,
    /// Current page (1-based).
    pub page: i64,
    /// Number of items per page.
    pub page_size: i64,
}

/// A single backup entry in the per-service backup list.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct ServiceBackupEntryResponse {
    /// Row ID from the `backups` table.
    pub id: i32,
    /// UUID string assigned at backup creation time.
    pub backup_id: String,
    /// Human-friendly display name.
    pub name: String,
    /// Current state: "completed", "running", "failed".
    pub state: String,
    /// Backup variant (e.g. "full", "incremental").
    pub backup_type: String,
    /// ISO 8601 timestamp when the backup started.
    #[schema(example = "2025-01-15T14:30:00Z")]
    pub started_at: String,
    /// ISO 8601 timestamp when the backup finished, if known.
    #[schema(example = "2025-01-15T14:35:00Z")]
    pub finished_at: Option<String>,
    /// Size of the backup in bytes, if available.
    pub size_bytes: Option<i64>,
    /// Object key or `s3://` URL for the backup data.
    pub s3_location: String,
    /// Engine-reported error message, populated when `state = "failed"`.
    pub error_message: Option<String>,
    /// Compression algorithm used (e.g. "gzip").
    pub compression_type: String,
    /// FK to `s3_sources.id`.
    pub s3_source_id: i32,
    /// Human-readable name of the S3 source.
    pub s3_source_name: String,
    /// Row ID from `external_service_backups`.
    pub external_service_backup_id: i32,
}

impl From<crate::services::ServiceBackupEntry> for ServiceBackupEntryResponse {
    fn from(e: crate::services::ServiceBackupEntry) -> Self {
        Self {
            id: e.id,
            backup_id: e.backup_id,
            name: e.name,
            state: e.state,
            backup_type: e.backup_type,
            started_at: e.started_at.to_rfc3339(),
            finished_at: e
                .finished_at
                .map(|dt: chrono::DateTime<chrono::Utc>| dt.to_rfc3339()),
            size_bytes: e.size_bytes,
            s3_location: e.s3_location,
            error_message: e.error_message,
            compression_type: e.compression_type,
            s3_source_id: e.s3_source_id,
            s3_source_name: e.s3_source_name,
            external_service_backup_id: e.external_service_backup_id,
        }
    }
}

/// Query parameters for the per-service backup listing.
#[derive(serde::Deserialize, utoipa::IntoParams)]
struct ListExternalServiceBackupsParams {
    /// Page number (1-based). Defaults to 1.
    #[serde(default = "default_page")]
    page: i64,
    /// Items per page. Defaults to 20, max 100.
    #[serde(default = "default_page_size")]
    page_size: i64,
}

fn default_page() -> i64 {
    1
}

fn default_page_size() -> i64 {
    20
}

/// List all backups for a specific external service (DB-only, no S3 scan).
///
/// Returns a paginated list of backups that belong to this service.
/// Completes in <100 ms regardless of S3 endpoint latency because it
/// never touches S3.
#[utoipa::path(
    tag = "Backups",
    get,
    path = "/backups/external-services/{service_id}/backups",
    params(
        ("service_id" = i32, Path, description = "External service ID"),
        ListExternalServiceBackupsParams
    ),
    responses(
        (status = 200, description = "Paginated list of backups for this service", body = ServiceBackupListResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn list_external_service_backups(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(service_id): Path<i32>,
    axum::extract::Query(params): axum::extract::Query<ListExternalServiceBackupsParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);

    let (entries, total) = app_state
        .backup_service
        .list_external_service_backups(service_id, params.page, params.page_size)
        .await
        .map_err(Problem::from)?;

    let response = ServiceBackupListResponse {
        backups: entries.into_iter().map(Into::into).collect(),
        total,
        page: params.page,
        page_size: params.page_size,
    };

    Ok(Json(response))
}

/// List the external services attached to a backup schedule.
#[utoipa::path(
    tag = "Backups",
    get,
    path = "/backups/schedules/{id}/services",
    params(("id" = i32, Path, description = "Schedule ID")),
    responses(
        (status = 200, description = "Services attached to this schedule", body = Vec<ExternalServiceSummary>),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Schedule not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn list_schedule_services(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);
    let services = app_state
        .backup_service
        .list_services_for_schedule(id)
        .await
        .map_err(Problem::from)?;
    let body: Vec<ExternalServiceSummary> = services.into_iter().map(Into::into).collect();
    Ok(Json(body))
}

/// Attach one or more external services to a backup schedule. Idempotent —
/// services that are already attached are silently skipped (`ON CONFLICT
/// DO NOTHING`). Returns the count of newly inserted rows + the total
/// membership after the operation.
#[utoipa::path(
    tag = "Backups",
    post,
    path = "/backups/schedules/{id}/services",
    params(("id" = i32, Path, description = "Schedule ID")),
    request_body = AttachScheduleServicesRequest,
    responses(
        (status = 200, description = "Services attached", body = AttachScheduleServicesResponse),
        (status = 400, description = "Validation error", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Schedule not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn attach_schedule_services(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
    Json(request): Json<AttachScheduleServicesRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsCreate);

    let inserted = app_state
        .backup_service
        .attach_services_to_schedule(id, &request.service_ids)
        .await
        .map_err(Problem::from)?;

    // Fetch the post-attach membership for the response so the UI doesn't
    // have to issue a follow-up GET.
    let total_attached = app_state
        .backup_service
        .list_services_for_schedule(id)
        .await
        .map_err(Problem::from)?
        .len();

    let audit = ScheduleServicesAttachedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        schedule_id: id,
        requested_service_ids: request.service_ids.clone(),
        inserted_count: inserted,
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for attach_schedule_services: {}",
            e
        );
    }

    Ok(Json(AttachScheduleServicesResponse {
        inserted,
        total_attached,
    }))
}

/// Detach a single external service from a backup schedule. Idempotent —
/// returns `204` whether or not a row was actually removed.
#[utoipa::path(
    tag = "Backups",
    delete,
    path = "/backups/schedules/{id}/services/{service_id}",
    params(
        ("id" = i32, Path, description = "Schedule ID"),
        ("service_id" = i32, Path, description = "External service ID"),
    ),
    responses(
        (status = 204, description = "Service detached (or was not attached)"),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn detach_schedule_service(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((id, service_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsDelete);

    let removed = app_state
        .backup_service
        .detach_service_from_schedule(id, service_id)
        .await
        .map_err(Problem::from)?;

    let audit = ScheduleServiceDetachedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        schedule_id: id,
        service_id,
        removed,
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for detach_schedule_service: {}",
            e
        );
    }

    Ok(StatusCode::NO_CONTENT)
}

/// List the schedules that target a specific external service. Useful for
/// the service detail page ("which schedules back this DB up?").
#[utoipa::path(
    tag = "Backups",
    get,
    path = "/backups/external-services/{service_id}/schedules",
    params(("service_id" = i32, Path, description = "External service ID")),
    responses(
        (status = 200, description = "Schedules backing up this service", body = Vec<BackupScheduleResponse>),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Service not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn list_service_schedules(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(service_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);
    let schedules = app_state
        .backup_service
        .list_schedules_for_service(service_id)
        .await
        .map_err(Problem::from)?;
    let body: Vec<BackupScheduleResponse> = schedules.into_iter().map(Into::into).collect();
    Ok(Json(body))
}

pub fn configure_routes() -> Router<Arc<BackupAppState>> {
    Router::new()
        .route(
            "/backups/s3-sources",
            get(list_s3_sources).post(create_s3_source),
        )
        .route(
            "/backups/s3-sources/{id}",
            get(get_s3_source)
                .patch(update_s3_source)
                .delete(delete_s3_source),
        )
        .route(
            "/backups/s3-sources/{id}/set-default",
            post(set_default_s3_source),
        )
        .route(
            "/backups/s3-sources/{id}/test",
            post(test_s3_source_connection),
        )
        .route("/backups/s3-sources/test", post(test_s3_connection_preview))
        .route("/backups/s3-sources/{id}/run", post(run_backup_for_source))
        .route(
            "/backups/schedules",
            get(list_backup_schedules).post(create_backup_schedule),
        )
        .route(
            "/backups/schedules/{id}",
            get(get_backup_schedule)
                .patch(update_backup_schedule)
                .delete(delete_backup_schedule),
        )
        .route(
            "/backups/schedules/{id}/backups",
            get(list_backups_for_schedule),
        )
        .route("/backups/schedules/{id}/runs", get(list_schedule_runs))
        .route("/backups/schedules/{id}/run", post(run_schedule_now))
        .route(
            "/backups/schedules/{id}/services",
            get(list_schedule_services).post(attach_schedule_services),
        )
        .route(
            "/backups/schedules/{id}/services/{service_id}",
            axum::routing::delete(detach_schedule_service),
        )
        .route(
            "/backups/external-services/{service_id}/schedules",
            get(list_service_schedules),
        )
        .route(
            "/backups/schedule-runs/{id}/jobs",
            get(list_schedule_run_jobs),
        )
        .route(
            "/backups/schedule-runs/{id}/cancel",
            post(cancel_schedule_run),
        )
        .route("/backups/{id}/cancel", post(cancel_backup))
        .route("/backups/s3-sources/{id}/backups", get(list_source_backups))
        .route(
            "/backups/external-services/{id}/backups",
            get(list_external_service_backups),
        )
        .route("/backups/{id}", get(get_backup))
        .route("/backups/{id}/children", get(list_backup_children))
        .route(
            "/backups/schedules/{id}/disable",
            patch(disable_backup_schedule),
        )
        .route(
            "/backups/schedules/{id}/enable",
            patch(enable_backup_schedule),
        )
        .route(
            "/backups/external-services/{id}/run",
            post(run_external_service_backup),
        )
        .route("/backups/alerts", get(list_backup_alerts))
}

/// List all S3 sources
#[utoipa::path(
    tag = "Backups",
    get,
    path = "/backups/s3-sources",
    responses(
        (status = 200, description = "List of S3 sources", body = Vec<S3SourceResponse>),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn list_s3_sources(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);
    let sources = app_state
        .backup_service
        .list_s3_sources()
        .await
        .map_err(Problem::from)?;

    let responses: Vec<S3SourceResponse> = sources.into_iter().map(Into::into).collect();
    Ok(Json(responses))
}

/// Create a new S3 source
#[utoipa::path(
    tag = "Backups",
    post,
    path = "/backups/s3-sources",
    request_body = CreateS3SourceRequest,
    responses(
        (status = 201, description = "S3 source created", body = S3SourceResponse),
        (status = 400, description = "Invalid request", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn create_s3_source(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateS3SourceRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsCreate);
    let source = app_state
        .backup_service
        .create_s3_source(request.clone())
        .await
        .map_err(Problem::from)?;

    // Create audit log
    let audit = S3SourceCreatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        source_id: source.id,
        name: source.name.clone(),
        bucket_name: source.bucket_name.clone(),
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok((StatusCode::CREATED, Json(S3SourceResponse::from(source))))
}

/// Get an S3 source by ID
#[utoipa::path(
    tag = "Backups",
    get,
    path = "/backups/s3-sources/{id}",
    responses(
        (status = 200, description = "S3 source details", body = S3SourceResponse),
        (status = 404, description = "S3 source not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn get_s3_source(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);
    let source = app_state.backup_service.get_s3_source(id).await?;

    Ok(Json(S3SourceResponse::from(source)))
}

/// Update an S3 source
#[utoipa::path(
    tag = "Backups",
    patch,
    path = "/backups/s3-sources/{id}",
    request_body = UpdateS3SourceRequest,
    responses(
        (status = 200, description = "S3 source updated", body = S3SourceResponse),
        (status = 404, description = "S3 source not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn update_s3_source(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<UpdateS3SourceRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsWrite);
    let source = app_state
        .backup_service
        .update_s3_source(id, request.clone())
        .await
        .map_err(Problem::from)?;

    // Create audit log with changed fields
    let mut updated_fields = HashMap::new();
    if request.name.is_some() {
        updated_fields.insert("name".to_string(), "updated".to_string());
    }
    if request.bucket_name.is_some() {
        updated_fields.insert("bucket_name".to_string(), "updated".to_string());
    }
    if request.bucket_path.is_some() {
        updated_fields.insert("bucket_path".to_string(), "updated".to_string());
    }
    if request.access_key_id.is_some() {
        updated_fields.insert("access_key_id".to_string(), "updated".to_string());
    }
    if request.secret_key.is_some() {
        updated_fields.insert("secret_key".to_string(), "updated".to_string());
    }
    if request.region.is_some() {
        updated_fields.insert("region".to_string(), "updated".to_string());
    }

    let audit = S3SourceUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        source_id: source.id,
        name: source.name.clone(),
        bucket_name: source.bucket_name.clone(),
        updated_fields,
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(Json(S3SourceResponse::from(source)))
}

/// Delete an S3 source
#[utoipa::path(
    tag = "Backups",
    delete,
    path = "/backups/s3-sources/{id}",
    responses(
        (status = 204, description = "S3 source deleted"),
        (status = 404, description = "S3 source not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn delete_s3_source(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsDelete);
    // Get source details before deletion for audit log
    let source = app_state.backup_service.get_s3_source(id).await?;

    app_state.backup_service.delete_s3_source(id).await?;

    let audit = S3SourceDeletedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        source_id: source.id,
        name: source.name,
        bucket_name: source.bucket_name,
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Mark an S3 source as the default. All new backups/schedules/services that do not
/// explicitly reference a source will use the default. Returns the updated source.
#[utoipa::path(
    tag = "Backups",
    post,
    path = "/backups/s3-sources/{id}/set-default",
    responses(
        (status = 200, description = "S3 source marked as default", body = S3SourceResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 404, description = "S3 source not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn set_default_s3_source(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsWrite);

    let source = app_state
        .backup_service
        .set_default_s3_source(id)
        .await
        .map_err(Problem::from)?;

    Ok(Json(S3SourceResponse::from(source)))
}

/// Test connectivity to an existing S3 source using its stored credentials.
#[utoipa::path(
    tag = "Backups",
    post,
    path = "/backups/s3-sources/{id}/test",
    responses(
        (status = 200, description = "Connection test result", body = S3ConnectionTestResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 404, description = "S3 source not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn test_s3_source_connection(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);

    match app_state.backup_service.test_s3_source_connection(id).await {
        Ok(()) => Ok(Json(S3ConnectionTestResponse {
            ok: true,
            message: "Connection successful".to_string(),
        })),
        Err(BackupError::NotFound { .. }) => Err(Problem::from(BackupError::NotFound {
            resource: "S3Source".to_string(),
            detail: format!("S3 source {} not found", id),
        })),
        Err(e) => Ok(Json(S3ConnectionTestResponse {
            ok: false,
            message: e.to_string(),
        })),
    }
}

/// Test S3 connectivity against a prospective source configuration (before creating it).
/// The credentials are NOT persisted. Useful for validating the form in the UI.
#[utoipa::path(
    tag = "Backups",
    post,
    path = "/backups/s3-sources/test",
    request_body = CreateS3SourceRequest,
    responses(
        (status = 200, description = "Connection test result", body = S3ConnectionTestResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 400, description = "Invalid request", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn test_s3_connection_preview(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Json(request): Json<CreateS3SourceRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsCreate);

    match app_state
        .backup_service
        .test_s3_connection_from_request(&request)
        .await
    {
        Ok(()) => Ok(Json(S3ConnectionTestResponse {
            ok: true,
            message: "Credentials valid and bucket reachable".to_string(),
        })),
        Err(BackupError::Validation(msg)) => Err(Problem::from(BackupError::Validation(msg))),
        Err(e) => Ok(Json(S3ConnectionTestResponse {
            ok: false,
            message: e.to_string(),
        })),
    }
}

/// List all backup schedules
#[utoipa::path(
    tag = "Backups",
    get,
    path = "/backups/schedules",
    responses(
        (status = 200, description = "List of backup schedules", body = Vec<BackupScheduleResponse>),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn list_backup_schedules(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);
    let schedules = app_state.backup_service.list_backup_schedules().await?;

    let responses: Vec<BackupScheduleResponse> = schedules.into_iter().map(Into::into).collect();
    Ok(Json(responses))
}

/// Create a new backup schedule
#[utoipa::path(
    tag = "Backups",
    post,
    path = "/backups/schedules",
    request_body = CreateBackupScheduleRequest,
    responses(
        (status = 201, description = "Backup schedule created", body = BackupScheduleResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn create_backup_schedule(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Json(request): Json<CreateBackupScheduleRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsCreate);

    match app_state
        .backup_service
        .create_backup_schedule(request)
        .await
    {
        Ok(schedule) => Ok((
            StatusCode::CREATED,
            Json(BackupScheduleResponse::from(schedule)),
        )),
        Err(e) => {
            error!("Failed to create backup schedule: {}", e);
            Err(Problem::from(e))
        }
    }
}

/// Get a backup schedule by ID
#[utoipa::path(
    tag = "Backups",
    get,
    path = "/backups/schedules/{id}",
    responses(
        (status = 200, description = "Backup schedule details", body = BackupScheduleResponse),
        (status = 404, description = "Backup schedule not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn get_backup_schedule(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);
    let schedule = app_state.backup_service.get_backup_schedule(id).await?;
    Ok(Json(BackupScheduleResponse::from(schedule)))
}

/// Delete a backup schedule
#[utoipa::path(
    tag = "Backups",
    delete,
    path = "/backups/schedules/{id}",
    responses(
        (status = 204, description = "Backup schedule deleted"),
        (status = 404, description = "Backup schedule not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn delete_backup_schedule(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, temps_core::problemdetails::Problem> {
    permission_guard!(auth, BackupsDelete);
    let result = app_state.backup_service.delete_backup_schedule(id).await?;
    if result {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(temps_core::error_builder::not_found()
            .title("Backup schedule not found")
            .build())
    }
}

/// List backups for a schedule
#[utoipa::path(
    tag = "Backups",
    get,
    path = "/backups/schedules/{id}/backups",
    responses(
        (status = 200, description = "List of backups for the schedule", body = Vec<BackupResponse>),
        (status = 404, description = "Backup schedule not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn list_backups_for_schedule(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);
    let backups = app_state
        .backup_service
        .list_backups_for_schedule(id)
        .await?;
    let responses: Vec<BackupResponse> = backups.into_iter().map(Into::into).collect();
    Ok(Json(responses))
}

/// Query parameters for `GET /api/backups/schedules/{id}/runs`.
#[derive(serde::Deserialize, utoipa::IntoParams)]
struct ListScheduleRunsParams {
    /// Page number (1-based, defaults to 1, clamped to 1 if < 1).
    #[serde(default = "default_page")]
    page: i64,
    /// Items per page (defaults to 20, clamped to 100 if > 100).
    #[serde(default = "default_page_size")]
    page_size: i64,
}

/// Query parameters for `GET /api/backups/schedule-runs/{id}/jobs`.
#[derive(serde::Deserialize, utoipa::IntoParams)]
struct ListScheduleRunJobsParams {
    /// Page number (1-based, defaults to 1).
    #[serde(default = "default_page")]
    page: i64,
    /// Items per page (defaults to 50, clamped to 200 if > 200).
    #[serde(default = "default_run_job_page_size")]
    page_size: i64,
}

fn default_run_job_page_size() -> i64 {
    50
}

/// Paginated run history for a backup schedule (one row per scheduler tick).
///
/// Returns one [`ScheduleRunSummary`] per scheduler tick, with child backup
/// counts aggregated in a single SQL round-trip. Legacy `backups` rows (pre-
/// fan-out) are surfaced as synthetic single-job runs so history does not
/// disappear. Ordered by `started_at DESC` (newest first).
///
/// Use `GET /backups/schedule-runs/{run_id}/jobs` to drill into a single run.
#[utoipa::path(
    tag = "Backups",
    get,
    path = "/backups/schedules/{id}/runs",
    params(ListScheduleRunsParams),
    responses(
        (status = 200, description = "Paginated run history for the schedule", body = ScheduleRunSummaryList),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Schedule not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn list_schedule_runs(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
    axum::extract::Query(params): axum::extract::Query<ListScheduleRunsParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);

    let result = app_state
        .backup_service
        .list_schedule_runs(id, params.page, params.page_size)
        .await
        .map_err(Problem::from)?;

    Ok(Json(result))
}

/// List the individual backup jobs for a single scheduler run.
///
/// Returns each child `backups` row joined with its external service name and
/// the most-recent `backup_jobs` engine key. Used by the schedule detail
/// accordion to show per-job detail on row expand.
///
/// `page_size` defaults to 50 and is capped at 200.
#[utoipa::path(
    tag = "Backups",
    get,
    path = "/backups/schedule-runs/{id}/jobs",
    responses(
        (status = 200, description = "Jobs for this scheduler run", body = Vec<ScheduleRunJobEntry>),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn list_schedule_run_jobs(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i64>,
    axum::extract::Query(params): axum::extract::Query<ListScheduleRunJobsParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);

    let (jobs, _total) = app_state
        .backup_service
        .list_schedule_run_jobs(id, params.page, params.page_size)
        .await
        .map_err(Problem::from)?;

    Ok(Json(jobs))
}

/// Immediately fan-out a run for the given schedule (Run Now).
///
/// Creates one `schedule_runs` row, one control-plane backup job, and one
/// backup job per supported external service — all in a single transaction.
/// Returns `202 Accepted` with a [`ScheduleRunResponse`] containing the new
/// `schedule_run_id` and the list of enqueued jobs. Returns `409 Conflict` if
/// a run for this schedule is already in flight or if the schedule is disabled.
#[utoipa::path(
    tag = "Backups",
    post,
    path = "/backups/schedules/{id}/run",
    responses(
        (status = 202, description = "Fan-out run enqueued for async execution", body = ScheduleRunResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Schedule not found", body = ProblemDetails),
        (status = 409, description = "Run already in flight or schedule disabled", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn run_schedule_now(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsCreate);

    let response = app_state
        .backup_service
        .run_schedule_now(id, Some(auth.user_id()))
        .await
        .map_err(Problem::from)?;

    // Audit the trigger. Use the first enqueued job's ids as representative
    // (the control-plane job is always first in the vec).
    let representative_backup_id = response.jobs.first().map(|j| j.backup_id).unwrap_or(0);
    let representative_job_id = response.jobs.first().map(|j| j.job_id).unwrap_or(0);

    let audit = ScheduleRunNowAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        schedule_id: id,
        // The schedule name is not returned in the fan-out response. Use the
        // run id as a unique identifier for the audit log.
        schedule_name: format!("run-{}", response.schedule_run_id),
        backup_id: representative_backup_id,
        job_id: representative_job_id,
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log for run_schedule_now: {}", e);
    }

    tracing::info!(
        schedule_id = id,
        schedule_run_id = response.schedule_run_id,
        job_count = response.jobs.len(),
        "run_schedule_now: fan-out run enqueued",
    );

    Ok((StatusCode::ACCEPTED, Json(response)).into_response())
}

/// Response body for cancel endpoints.
#[derive(Serialize, ToSchema)]
pub struct CancelBackupResponse {
    /// Number of rows that were actually flipped to `failed`. `0` is a valid
    /// success and means the backup was already terminal — the call is
    /// idempotent.
    pub cancelled: u64,
}

/// Cancel a single in-flight backup.
///
/// Flips the parent `backups` row + its latest `backup_jobs` row to
/// `failed` with reason `"cancelled by user <uid>"`. The in-process
/// `CancellationToken` is observed on the next heartbeat tick (≤5s), so the
/// engine exits cleanly and rollback reaps the sidecar. Idempotent: cancelling
/// an already-terminal backup is a 200 with `cancelled = 0`.
#[utoipa::path(
    tag = "Backups",
    post,
    path = "/backups/{id}/cancel",
    responses(
        (status = 200, description = "Cancel processed (idempotent)", body = CancelBackupResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Backup not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn cancel_backup(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsDelete);

    let cancelled = app_state
        .backup_service
        .cancel_backup(id, Some(auth.user_id()))
        .await
        .map_err(Problem::from)?;

    tracing::info!(
        backup_id = id,
        cancelled,
        user_id = auth.user_id(),
        "cancel_backup: completed",
    );

    Ok((StatusCode::OK, Json(CancelBackupResponse { cancelled })).into_response())
}

/// Cancel every non-terminal child backup belonging to a schedule run.
///
/// Loops over `state IN ('pending','running')` children and flips each via
/// the same path as the per-backup cancel endpoint. The parent
/// `schedule_runs.finished_at` is stamped automatically once no live
/// children remain. Idempotent: cancelling a run with no live children is
/// a 200 with `cancelled = 0`.
#[utoipa::path(
    tag = "Backups",
    post,
    path = "/backups/schedule-runs/{id}/cancel",
    responses(
        (status = 200, description = "Cancel processed (idempotent)", body = CancelBackupResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Schedule run not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn cancel_schedule_run(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsDelete);

    let cancelled = app_state
        .backup_service
        .cancel_schedule_run(id, Some(auth.user_id()))
        .await
        .map_err(Problem::from)?;

    tracing::info!(
        schedule_run_id = id,
        cancelled,
        user_id = auth.user_id(),
        "cancel_schedule_run: completed",
    );

    Ok((StatusCode::OK, Json(CancelBackupResponse { cancelled })).into_response())
}

/// Run a backup immediately for an S3 source.
///
/// Enqueues the backup for asynchronous execution via the `BackupRunner`
/// (ADR-014). Returns `202 Accepted` immediately: a `backups` row is inserted
/// with `state='pending'` and a `backup_jobs` row is enqueued for the
/// `ControlPlaneEngine`. Poll `GET /backups/{id}` to observe
/// `pending → running → completed`.
#[utoipa::path(
    tag = "Backups",
    post,
    path = "/backups/s3-sources/{id}/run",
    request_body = RunBackupRequest,
    responses(
        (status = 202, description = "Backup enqueued for async execution", body = BackupResponse),
        (status = 400, description = "Invalid request", body = ProblemDetails),
        (status = 404, description = "S3 source not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn run_backup_for_source(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<RunBackupRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsCreate);

    // Insert the `backups` row and the `backup_jobs` row atomically: if either
    // insert fails, both are rolled back. This prevents orphan `backups` rows
    // that sit in `state='pending'` indefinitely with no job to drive them
    // (ADR-014 lifecycle bug fix).
    let trigger = BackupTriggerParams {
        engine: "control_plane".to_string(),
        params: serde_json::json!({ "s3_source_id": id }),
        max_runtime_secs: None,
    };

    let (backup, job_id) = app_state
        .backup_service
        .create_pending_backup_row(id, &request.backup_type, auth.user_id(), trigger)
        .await
        .map_err(|e| {
            error!(
                s3_source_id = id,
                error = %e,
                "run_backup_for_source: failed to create pending backup row and enqueue job",
            );
            Problem::from(e)
        })?;

    let audit = BackupRunAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        source_id: backup.id,
        source_name: backup.name.clone(),
        backup_id: backup.backup_id.clone(),
        backup_type: request.backup_type,
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    tracing::info!(
        backup_id = backup.id,
        job_id,
        s3_source_id = id,
        "run_backup_for_source: job enqueued",
    );

    Ok((StatusCode::ACCEPTED, Json(BackupResponse::from(backup))).into_response())
}

/// Get a backup by ID
#[utoipa::path(
    tag = "Backups",
    get,
    path = "/backups/{id}",
    responses(
        (status = 200, description = "Backup details", body = BackupResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 404, description = "Backup not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn get_backup(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);

    let Some(backup) = app_state.backup_service.get_backup(&id).await? else {
        return Err(temps_core::error_builder::not_found()
            .title("Backup Not Found")
            .detail(format!("Backup with ID {} not found", id))
            .build());
    };

    let backup_id_int = backup.id;

    // Compute partial size while the backup is still running. Best-effort
    // and capped to one S3 list call per request — `compute_live_size`
    // returns None for finished or unresolvable backups.
    let live_size = app_state.backup_service.compute_live_size(&backup).await;
    let mut response = BackupResponse::from(backup);
    response.live_size_bytes = live_size;

    // Populate the linked external service if this is an external-service backup.
    // A `None` result means this is a control-plane backup — that's fine.
    // Errors are downgraded to None so a DB hiccup never breaks the detail page.
    response.external_service = app_state
        .backup_service
        .get_backup_external_service(backup_id_int)
        .await
        .unwrap_or(None)
        .map(|svc| ExternalServiceSummary {
            id: svc.id,
            name: svc.name,
            service_type: svc.service_type,
        });

    // current_step / attempts / max_attempts / max_runtime_secs used to
    // come from the per-backup `backup_jobs` row. That table is gone in
    // the queue-consumer architecture; these response fields remain in
    // the schema for backwards compat but are always None.

    Ok(Json(response))
}

/// List the external-service child backups that belong to a parent backup.
///
/// Each entry in `children` corresponds to one `external_service_backups` row,
/// joined with `external_services` so the caller receives the service name and
/// type without a second request.
///
/// Returns an empty `{ "children": [] }` — **not 404** — when the parent
/// backup exists but has no children (e.g. control-plane backups).
/// Returns 404 when the parent backup itself does not exist.
#[utoipa::path(
    tag = "Backups",
    get,
    path = "/backups/{id}/children",
    params(
        ("id" = i32, Path, description = "Integer row id of the parent backup")
    ),
    responses(
        (status = 200, description = "Child backup list (may be empty)", body = ChildBackupListResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Parent backup not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn list_backup_children(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);

    let children = app_state
        .backup_service
        .list_child_backups(id)
        .await
        .map_err(Problem::from)?;

    let response = ChildBackupListResponse {
        children: children.into_iter().map(Into::into).collect(),
    };

    Ok(Json(response))
}

/// Disable a backup schedule
#[utoipa::path(
    tag = "Backups",
    patch,
    path = "/backups/schedules/{id}/disable",
    responses(
        (status = 200, description = "Backup schedule disabled", body = BackupScheduleResponse),
        (status = 404, description = "Backup schedule not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn disable_backup_schedule(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsWrite);
    let schedule = app_state.backup_service.disable_backup_schedule(id).await?;

    let audit = BackupScheduleStatusChangedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        schedule_id: schedule.id,
        name: schedule.name.clone(),
        new_status: "disabled".to_string(),
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(Json(BackupScheduleResponse::from(schedule)))
}

/// Enable a backup schedule
#[utoipa::path(
    tag = "Backups",
    patch,
    path = "/backups/schedules/{id}/enable",
    responses(
        (status = 200, description = "Backup schedule enabled", body = BackupScheduleResponse),
        (status = 404, description = "Backup schedule not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn enable_backup_schedule(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsWrite);
    let schedule = app_state.backup_service.enable_backup_schedule(id).await?;
    let audit = BackupScheduleStatusChangedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        schedule_id: schedule.id,
        name: schedule.name.clone(),
        new_status: "enabled".to_string(),
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(Json(BackupScheduleResponse::from(schedule)))
}

/// Update a backup schedule (partial update).
///
/// All request fields are optional; only fields that are present in the
/// JSON body are updated. Absent fields leave the corresponding column
/// unchanged. If `schedule_expression` is changed, `next_run` is
/// recomputed automatically.
#[utoipa::path(
    tag = "Backups",
    patch,
    path = "/backups/schedules/{id}",
    request_body = UpdateBackupScheduleRequest,
    responses(
        (status = 200, description = "Schedule updated", body = BackupScheduleResponse),
        (status = 400, description = "Validation error", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 404, description = "Schedule not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn update_backup_schedule(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<UpdateBackupScheduleRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsWrite);

    let schedule = app_state
        .backup_service
        .update_backup_schedule(id, request.clone())
        .await?;

    let audit = BackupScheduleUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        schedule_id: schedule.id,
        schedule_name: schedule.name.clone(),
        fields_changed: changed_fields_for_audit(&request),
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for schedule update {}: {}",
            id, e
        );
    }

    Ok(Json(BackupScheduleResponse::from(schedule)))
}

/// Run a backup for an external service manually.
///
/// Enqueues the backup for asynchronous execution via the `BackupRunner`
/// (ADR-014). Returns `202 Accepted` immediately: pending parent and child
/// rows are inserted, and a `backup_jobs` row is enqueued for the resolved
/// engine. Poll `GET /backups/{id}` to observe `pending → running → completed`.
#[utoipa::path(
    tag = "Backups",
    post,
    path = "/backups/external-services/{id}/run",
    request_body = RunExternalServiceBackupRequest,
    responses(
        (status = 202, description = "Backup enqueued for async execution", body = ExternalServiceBackupResponse),
        (status = 400, description = "Invalid request", body = ProblemDetails),
        (status = 404, description = "External service or S3 source not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn run_external_service_backup(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<RunExternalServiceBackupRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsCreate);

    let service = app_state
        .backup_service
        .get_external_service(id)
        .await
        .map_err(|e| {
            error!("Failed to get external service {} for backup: {}", id, e);
            Problem::from(e)
        })?;

    let backup_type = request.backup_type.as_deref().unwrap_or("full");

    let s3_source_id = app_state
        .backup_service
        .resolve_s3_source_id(request.s3_source_id)
        .await
        .map_err(Problem::from)?;

    // 1. Resolve which engine handles this service type (may probe Docker).
    // 2. Insert pending parent (`backups`) + child (`external_service_backups`)
    //    + `backup_jobs` rows in a single transaction.
    // 3. Return 202 immediately — the runner executes the backup asynchronously.
    // All three inserts are atomic: if any fails, the entire operation is rolled
    // back and the handler returns an error. No orphan rows are created.
    let docker = bollard::Docker::connect_with_local_defaults().map_err(|e| {
        error!(service_id = id, error = %e, "run_external_service_backup: failed to connect to Docker for engine resolution");
        problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Docker Unavailable")
            .with_detail(format!("Could not connect to Docker to determine backup engine: {}", e))
    })?;

    let engine_key = resolve_engine_key(&service, &docker)
        .await
        .map_err(|e| {
            error!(service_id = id, error = %e, "run_external_service_backup: engine resolution failed");
            Problem::from(e)
        })?;

    let trigger = BackupTriggerParams {
        engine: engine_key.to_string(),
        params: serde_json::json!({
            "service_id": service.id,
            "s3_source_id": s3_source_id,
            "backup_type": backup_type,
        }),
        max_runtime_secs: None,
    };

    let (pending, job_id) = app_state
        .backup_service
        .create_pending_external_service_backup_row(
            service.id,
            s3_source_id,
            backup_type,
            auth.user_id(),
            trigger,
        )
        .await
        .map_err(|e| {
            error!(
                service_id = id,
                engine = engine_key,
                error = %e,
                "run_external_service_backup: failed to create pending rows and enqueue job",
            );
            Problem::from(e)
        })?;

    let audit = ExternalServiceBackupRunAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        service_id: service.id,
        service_name: service.name.clone(),
        service_type: service.service_type.clone(),
        backup_id: pending.id,
        backup_type: backup_type.to_string(),
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    tracing::info!(
        service_id = id,
        service_name = %service.name,
        engine = engine_key,
        job_id,
        "run_external_service_backup: job enqueued",
    );

    Ok((
        StatusCode::ACCEPTED,
        Json(ExternalServiceBackupResponse::from(pending)),
    )
        .into_response())
}

// ── Backup Alerts ──────────────────────────────────────────────────────────────

/// A single open backup alert surfaced in the UI banner.
///
/// Alerts are auto-opened by the watcher and auto-resolved when the triggering
/// condition clears. No manual dismiss is required or supported.
///
/// The optional `schedule_s3_source_id` field is included so the UI can
/// deep-link an `overdue_schedule` alert to the S3 source detail page that
/// hosts the schedule. `stalled_job` alerts no longer carry a deep-link
/// target — the alert message text contains the backup id for display.
#[derive(Debug, Serialize, ToSchema)]
pub struct BackupAlertResponse {
    /// Database id of the alert row.
    pub id: i64,
    /// `"overdue_schedule"` or `"stalled_job"`.
    pub kind: String,
    /// `"warning"` or `"critical"`.
    pub severity: String,
    /// FK to `backup_schedules.id`. Set for `overdue_schedule` alerts.
    pub schedule_id: Option<i32>,
    /// Human-readable name of the linked schedule, if applicable.
    pub schedule_name: Option<String>,
    /// FK to `backup_schedules.s3_source_id`. The UI uses this to deep-link
    /// the alert to the S3 source detail page that hosts the schedule.
    /// Set for `overdue_schedule` alerts.
    pub schedule_s3_source_id: Option<i32>,
    /// Human-readable description of the alert condition.
    pub message: String,
    /// RFC 3339 timestamp when the alert was opened.
    #[schema(example = "2026-05-15T10:00:00Z")]
    pub opened_at: String,
}

/// Response body for the list-backup-alerts endpoint.
#[derive(Debug, Serialize, ToSchema)]
pub struct BackupAlertListResponse {
    /// All currently open (unresolved) alerts, newest first.
    pub alerts: Vec<BackupAlertResponse>,
}

/// Internal row type for the alert JOIN query.
#[derive(Debug, FromQueryResult)]
struct AlertRow {
    pub id: i64,
    pub kind: String,
    pub severity: String,
    pub schedule_id: Option<i32>,
    pub schedule_name: Option<String>,
    pub schedule_s3_source_id: Option<i32>,
    pub message: String,
    pub opened_at: chrono::DateTime<chrono::Utc>,
}

/// List open backup alerts.
///
/// Returns all alerts that have not yet been resolved, ordered by `opened_at`
/// descending (newest first). The UI renders these as a banner above the
/// Backups page content. Alerts are auto-opened by the watcher and
/// auto-resolved when the triggering condition clears.
///
/// **Schedule overdue** — the backup scheduler did not enqueue a job within
/// the expected window (1 hour past `next_run`). Usually means the scheduler
/// task is dead or wedged.
///
/// **Job stalled** — a `backup_jobs` row has been in `state='pending'` for
/// more than 1 hour. The runner never claimed the job. Usually means the
/// runner task is dead or the runner concurrency cap is too low.
#[utoipa::path(
    tag = "Backups",
    get,
    path = "/backups/alerts",
    responses(
        (status = 200, description = "List of open backup alerts", body = BackupAlertListResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn list_backup_alerts(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);

    // LEFT JOIN backup_schedules so overdue_schedule alerts carry the
    // schedule's `s3_source_id` for UI deep-linking. stalled_job alerts
    // no longer reference a job row (the FK column was dropped); the
    // alert message text carries the backup id for display.
    let sql = r#"
SELECT
    a.id,
    a.kind,
    a.severity,
    a.schedule_id,
    s.name             AS schedule_name,
    s.s3_source_id     AS schedule_s3_source_id,
    a.message,
    a.opened_at
FROM backup_alerts a
LEFT JOIN backup_schedules s ON s.id = a.schedule_id
WHERE a.resolved_at IS NULL
ORDER BY a.opened_at DESC
"#;

    let rows = AlertRow::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        sql,
        vec![],
    ))
    .all(app_state.db.as_ref())
    .await
    .map_err(|e| {
        error!("Failed to query backup alerts: {}", e);
        problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Internal Server Error")
            .with_detail(format!("Failed to query backup alerts: {}", e))
    })?;

    let alerts = rows
        .into_iter()
        .map(|row| BackupAlertResponse {
            id: row.id,
            kind: row.kind,
            severity: row.severity,
            schedule_id: row.schedule_id,
            schedule_name: row.schedule_name,
            schedule_s3_source_id: row.schedule_s3_source_id,
            message: row.message,
            opened_at: row.opened_at.to_rfc3339(),
        })
        .collect();

    Ok(Json(BackupAlertListResponse { alerts }))
}
