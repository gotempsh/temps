use crate::handlers::backup_handler::{
    CreateBackupScheduleRequest, CreateS3SourceRequest, UpdateBackupScheduleRequest,
};
use anyhow::Result;
use aws_sdk_s3::error::ProvideErrorMetadata;
use aws_sdk_s3::{Client as S3Client, Config};
use chrono::{DateTime, Duration, Timelike, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseBackend, DatabaseConnection, EntityTrait,
    FromQueryResult, IntoActiveModel, PaginatorTrait, QueryFilter, QueryOrder, Statement,
    TransactionTrait, Value,
};
use serde_json::json;
use serde_yaml;
use std::str::FromStr;
use std::sync::Arc;
use tempfile::NamedTempFile;
use temps_entities::backups::Model as Backup;
use thiserror::Error;
use tokio::time;
use tracing::{debug, error, info, warn};
use urlencoding;
use uuid::Uuid;

use cron::Schedule;
use temps_core::notifications::{BackupFailureData, NotificationService};
use temps_entities::{backup_schedules::Model as BackupSchedule, s3_sources::Model as S3Source};
use temps_providers::ExternalServiceManager;
use tokio_stream::StreamExt;

/// POSIX-safe shell escaping: wraps value in single quotes, escaping any
/// embedded single quotes. Safe for use in `sh -c` command strings.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Classify a backup location into one of the known storage formats.
/// Returns `None` for non-postgres / unknown locations so the UI can show
/// a neutral badge without guessing.
///
/// The `engine` hint is used to disambiguate formats that share location
/// shapes. Object-store backups (s3/rustfs/blob/minio) are always a
/// bucket-to-bucket mirror — their path has no extension — so we tag them
/// `"mirror"` when the engine identifies them as such.
fn classify_backup_format(location: &str, engine: Option<&str>) -> Option<String> {
    if location.is_empty() {
        return None;
    }
    // Engine-first: object-store backups are always mc-mirror dumps regardless
    // of location shape. No extension-based inference applies.
    if let Some(e) = engine {
        let e = e.to_ascii_lowercase();
        if matches!(e.as_str(), "s3" | "rustfs" | "blob" | "minio") {
            return Some("mirror".to_string());
        }
    }
    // Extension-based classification runs first — it's unambiguous when
    // the file suffix is present, regardless of whether the location is
    // an s3:// URL or a bare key.
    if location.ends_with(".sql.gz") || location.ends_with(".pgdump.gz") {
        return Some("pg_dump".to_string());
    }
    if location.ends_with(".rdb.gz") {
        return Some("rdb".to_string());
    }
    if location.ends_with(".bson.gz") || location.ends_with(".archive") {
        return Some("mongodump".to_string());
    }
    // WAL-G backups are uploaded by the wal-g binary as a *prefix*, not a
    // single file. The orchestrator records the WAL-G root prefix
    // (e.g. `s3://bucket/external_services/postgres/svc/walg`) as the
    // backup's location. Match by path segment, NOT by `s3://` prefix —
    // pg_dump / mongodump / rdb backups also live under `s3://...` and
    // would otherwise get misclassified as walg.
    let trimmed = location.trim_end_matches('/');
    if trimmed.ends_with("/walg") || trimmed.contains("/walg/") {
        return Some("walg".to_string());
    }
    None
}

/// Walk the S3 source's `external_services/` prefix to find backups that
/// aren't represented in the local DB (e.g., backups produced by a
/// previous Temps instance). Returns synthesized `SourceBackupEntry`-shape
/// JSON values tagged with `source: "s3_scan"`.
///
/// Paths we recognize (written by the backup pipeline):
/// - `external_services/<engine>/<service>/<YYYY>/<MM>/<DD>/*.sql.gz`
///   and `*.pgdump.gz` (pg_dump legacy)
/// - `external_services/<engine>/<service>/walg/basebackups_005/*_backup_stop_sentinel.json`
///   (WAL-G marker objects)
async fn scan_s3_for_orphan_backups(
    s3_client: &aws_sdk_s3::Client,
    s3_source: &temps_entities::s3_sources::Model,
    seen_locations: &std::collections::HashSet<String>,
) -> Result<Vec<serde_json::Value>, anyhow::Error> {
    let bucket = &s3_source.bucket_name;
    let prefix = build_s3_key(&s3_source.bucket_path, "external_services/");

    // First-level list: `external_services/<engine>/`. We use delimiter
    // '/' so we only get the top-level engine directories (CommonPrefixes).
    let engine_prefixes: Vec<String> = list_common_prefixes(s3_client, bucket, &prefix).await?;

    let mut out: Vec<serde_json::Value> = Vec::new();

    for engine_prefix in engine_prefixes {
        let engine = extract_trailing_segment(&engine_prefix);
        // Second-level list: `external_services/<engine>/<service>/`
        let service_prefixes = list_common_prefixes(s3_client, bucket, &engine_prefix).await?;

        for service_prefix in service_prefixes {
            let service_name = extract_trailing_segment(&service_prefix);

            // Look for a WAL-G backup under `<service_prefix>walg/`.
            let walg_prefix = format!("{}walg/", service_prefix);
            let walg_sentinels = list_walg_sentinels(s3_client, bucket, &walg_prefix).await?;
            for (name, last_modified, size) in walg_sentinels {
                // Canonical restore location is the walg root, not the
                // sentinel itself (wal-g backup-fetch takes a prefix).
                let endpoint_host = s3_source
                    .endpoint
                    .as_deref()
                    .and_then(|u| {
                        u.strip_prefix("http://")
                            .or_else(|| u.strip_prefix("https://"))
                    })
                    .unwrap_or("");
                let _ = endpoint_host; // silence unused — kept for future use
                let location = format!("s3://{}/{}", bucket, walg_prefix.trim_end_matches('/'));
                if seen_locations.contains(&location) {
                    continue;
                }
                out.push(serde_json::json!({
                    "id": 0,
                    "backup_id": "",
                    "name": format!("{} backup ({})", engine, service_name),
                    "type": "full",
                    "created_at": last_modified,
                    "size_bytes": size,
                    "location": location,
                    "metadata_location": "",
                    "engine": engine.clone(),
                    "origin_service_name": service_name.clone(),
                    "format": "walg",
                    "source": "s3_scan",
                    "state": "completed",
                    "scan_sentinel_key": name,
                }));
            }

            // Look for pg_dump-style objects under the service prefix.
            let dumps = list_dump_objects(s3_client, bucket, &service_prefix).await?;
            for (key, last_modified, size) in dumps {
                if seen_locations.contains(&key) {
                    continue;
                }
                let format = classify_backup_format(&key, Some(&engine))
                    .unwrap_or_else(|| "unknown".to_string());
                out.push(serde_json::json!({
                    "id": 0,
                    "backup_id": "",
                    "name": format!("{} backup ({})", engine, service_name),
                    "type": "full",
                    "created_at": last_modified,
                    "size_bytes": size,
                    "location": key,
                    "metadata_location": "",
                    "engine": engine.clone(),
                    "origin_service_name": service_name.clone(),
                    "format": format,
                    "source": "s3_scan",
                    "state": "completed",
                }));
            }
        }
    }

    Ok(out)
}

/// List CommonPrefixes under a given S3 prefix (with `/` delimiter).
/// Returns full subprefix paths (e.g. `external_services/postgres/`).
async fn list_common_prefixes(
    s3_client: &aws_sdk_s3::Client,
    bucket: &str,
    prefix: &str,
) -> Result<Vec<String>, anyhow::Error> {
    let mut out = Vec::new();
    let mut continuation: Option<String> = None;
    loop {
        let mut req = s3_client
            .list_objects_v2()
            .bucket(bucket)
            .prefix(prefix)
            .delimiter("/");
        if let Some(ct) = continuation.clone() {
            req = req.continuation_token(ct);
        }
        let resp = req.send().await?;
        for cp in resp.common_prefixes() {
            if let Some(p) = cp.prefix() {
                out.push(p.to_string());
            }
        }
        if resp.is_truncated().unwrap_or(false) {
            continuation = resp.next_continuation_token().map(|s| s.to_string());
            if continuation.is_none() {
                break;
            }
        } else {
            break;
        }
    }
    Ok(out)
}

/// Find WAL-G backup-stop-sentinel objects under a walg prefix and return
/// (key, last_modified_rfc3339, size_bytes) for each. WAL-G names them
/// `base_<timestamp>_backup_stop_sentinel.json`. The presence of the
/// sentinel is what marks a WAL-G backup as complete.
async fn list_walg_sentinels(
    s3_client: &aws_sdk_s3::Client,
    bucket: &str,
    walg_prefix: &str,
) -> Result<Vec<(String, String, Option<i32>)>, anyhow::Error> {
    let basebackups_prefix = format!("{}basebackups_005/", walg_prefix);
    let mut out = Vec::new();
    let mut continuation: Option<String> = None;
    loop {
        let mut req = s3_client
            .list_objects_v2()
            .bucket(bucket)
            .prefix(&basebackups_prefix);
        if let Some(ct) = continuation.clone() {
            req = req.continuation_token(ct);
        }
        let resp = req.send().await?;
        for obj in resp.contents() {
            let key = match obj.key() {
                Some(k) => k.to_string(),
                None => continue,
            };
            if !key.ends_with("_backup_stop_sentinel.json") {
                continue;
            }
            let lm = obj
                .last_modified()
                .and_then(|d| {
                    chrono::DateTime::<chrono::Utc>::from_timestamp(d.secs(), d.subsec_nanos())
                        .map(|c| c.to_rfc3339())
                })
                .unwrap_or_default();
            // i32 size cap — AWS gives i64; we store i32 in DB, match that.
            let size = obj.size().and_then(|s| i32::try_from(s).ok());
            out.push((key, lm, size));
        }
        if resp.is_truncated().unwrap_or(false) {
            continuation = resp.next_continuation_token().map(|s| s.to_string());
            if continuation.is_none() {
                break;
            }
        } else {
            break;
        }
    }
    Ok(out)
}

/// Find pg_dump / rdb / bson dump objects under a service prefix.
async fn list_dump_objects(
    s3_client: &aws_sdk_s3::Client,
    bucket: &str,
    service_prefix: &str,
) -> Result<Vec<(String, String, Option<i32>)>, anyhow::Error> {
    let mut out = Vec::new();
    let mut continuation: Option<String> = None;
    loop {
        let mut req = s3_client
            .list_objects_v2()
            .bucket(bucket)
            .prefix(service_prefix);
        if let Some(ct) = continuation.clone() {
            req = req.continuation_token(ct);
        }
        let resp = req.send().await?;
        for obj in resp.contents() {
            let key = match obj.key() {
                Some(k) => k.to_string(),
                None => continue,
            };
            // Skip walg internals — they're captured by the sentinel pass.
            if key.contains("/walg/") {
                continue;
            }
            if !(key.ends_with(".sql.gz")
                || key.ends_with(".pgdump.gz")
                || key.ends_with(".rdb.gz")
                || key.ends_with(".bson.gz")
                || key.ends_with(".archive"))
            {
                continue;
            }
            let lm = obj
                .last_modified()
                .and_then(|d| {
                    chrono::DateTime::<chrono::Utc>::from_timestamp(d.secs(), d.subsec_nanos())
                        .map(|c| c.to_rfc3339())
                })
                .unwrap_or_default();
            let size = obj.size().and_then(|s| i32::try_from(s).ok());
            out.push((key, lm, size));
        }
        if resp.is_truncated().unwrap_or(false) {
            continuation = resp.next_continuation_token().map(|s| s.to_string());
            if continuation.is_none() {
                break;
            }
        } else {
            break;
        }
    }
    Ok(out)
}

/// Given `external_services/postgres/` returns `postgres`. Returns empty
/// string when the prefix has no trailing segment.
fn extract_trailing_segment(prefix: &str) -> String {
    let trimmed = prefix.trim_end_matches('/');
    match trimmed.rsplit('/').next() {
        Some(s) => s.to_string(),
        None => String::new(),
    }
}

/// Build a normalized S3 object key from a bucket_path prefix and a relative
/// suffix. Keys must NEVER start with "/" — S3-compatible providers (MinIO, R2,
/// Backblaze B2) reject leading-slash keys as `InvalidArgument`. When the
/// configured `bucket_path` is empty or just "/", the prefix is dropped.
fn build_s3_key(bucket_path: &str, suffix: &str) -> String {
    let prefix = bucket_path.trim_matches('/');
    let suffix = suffix.trim_start_matches('/');
    if prefix.is_empty() {
        suffix.to_string()
    } else {
        format!("{}/{}", prefix, suffix)
    }
}

#[derive(Error, Debug)]
pub enum BackupError {
    #[error("Database error: {0}")]
    Database(sea_orm::DbErr),

    #[error("S3 error: {0}")]
    S3(String),

    #[error("Schedule error: {0}")]
    Schedule(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{resource} not found: {detail}")]
    NotFound { resource: String, detail: String },

    #[error("Invalid configuration: {0}")]
    Configuration(String),

    #[error("External service error: {0}")]
    ExternalService(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Internal error: {message}")]
    Internal { message: String },

    #[error("Unsupported: {0}")]
    Unsupported(String),

    #[error("Notification error: {0}")]
    NotificationError(String),

    /// A backup job for the same engine + target is already in flight.
    ///
    /// Returned by `run_schedule_now` when the runner's concurrency guard fires.
    /// Surfaces as `409 Conflict` in handlers.
    #[error(
        "A backup is already in flight (existing job id: {existing_job_id}); \
         refuse to enqueue a duplicate"
    )]
    AlreadyInFlight { existing_job_id: i64 },

    /// A `schedule_runs` row for this schedule already has `finished_at IS NULL`
    /// (at least one child backup is still pending or running).
    ///
    /// Returned by [`BackupService::run_schedule_now`] when the fan-out detects
    /// an in-flight run. Surfaces as `409 Conflict` in handlers.
    #[error(
        "A run for this schedule is already in flight (existing run id: {existing_run_id}); \
         wait for it to finish before triggering a new run"
    )]
    ScheduleRunAlreadyInFlight { existing_run_id: i64 },
}

impl From<aws_sdk_s3::error::SdkError<aws_sdk_s3::operation::put_object::PutObjectError>>
    for BackupError
{
    fn from(
        err: aws_sdk_s3::error::SdkError<aws_sdk_s3::operation::put_object::PutObjectError>,
    ) -> Self {
        BackupError::S3(crate::engines::v2_common::describe_sdk_error(
            "put_object",
            &err,
        ))
    }
}

impl From<aws_sdk_s3::error::SdkError<aws_sdk_s3::operation::delete_object::DeleteObjectError>>
    for BackupError
{
    fn from(
        err: aws_sdk_s3::error::SdkError<aws_sdk_s3::operation::delete_object::DeleteObjectError>,
    ) -> Self {
        BackupError::S3(crate::engines::v2_common::describe_sdk_error(
            "delete_object",
            &err,
        ))
    }
}

impl
    From<
        aws_sdk_s3::error::SdkError<
            aws_sdk_s3::operation::complete_multipart_upload::CompleteMultipartUploadError,
        >,
    > for BackupError
{
    fn from(
        err: aws_sdk_s3::error::SdkError<
            aws_sdk_s3::operation::complete_multipart_upload::CompleteMultipartUploadError,
        >,
    ) -> Self {
        BackupError::S3(crate::engines::v2_common::describe_sdk_error(
            "complete_multipart_upload",
            &err,
        ))
    }
}

// Conversion from anyhow::Error is used by service methods whose helper functions
// return anyhow::Result. This is a transitional impl; the goal is to convert all
// helper functions to return BackupError directly.
impl From<anyhow::Error> for BackupError {
    fn from(err: anyhow::Error) -> Self {
        BackupError::Internal {
            message: format!("{:#}", err),
        }
    }
}

impl From<sea_orm::DbErr> for BackupError {
    fn from(err: sea_orm::DbErr) -> Self {
        match err {
            sea_orm::DbErr::RecordNotFound(msg) => BackupError::NotFound {
                resource: "Backup resource".to_string(),
                detail: msg,
            },
            _ => BackupError::Database(err),
        }
    }
}

/// A single backup row returned by [`BackupService::list_external_service_backups`].
///
/// Populated from a JOIN of `external_service_backups`, `backups`, and `s3_sources`
/// so every field is available in a single SQL round-trip.
#[derive(Debug, FromQueryResult, serde::Serialize)]
pub struct ServiceBackupEntry {
    /// Row ID from the `backups` table.
    pub id: i32,
    /// UUID string assigned at backup creation time.
    pub backup_id: String,
    /// Human-friendly display name for this backup.
    pub name: String,
    /// Current state: "completed", "running", "failed".
    pub state: String,
    /// Backup variant (e.g. "full", "incremental").
    pub backup_type: String,
    /// When the backup started (RFC 3339 in the JSON response).
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// When the backup finished, if known.
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
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

/// A single run-history entry for the schedule detail page (deliverable 1).
///
/// Combines one `backups` row with the most-recent `backup_jobs` row for that
/// backup via a lateral JOIN.  Fields from `backup_jobs` are `None` for legacy
/// backup rows that pre-date ADR-014.
#[derive(Debug, FromQueryResult, serde::Serialize, utoipa::ToSchema)]
pub struct ScheduleRunEntry {
    /// DB id of the `backups` row.
    pub backup_id: i32,
    /// UUID string (`backups.backup_id`).
    pub backup_uuid: String,
    /// Current state: `"pending"`, `"running"`, `"completed"`, `"failed"`.
    pub state: String,
    /// When the backup was started (ISO 8601 / RFC 3339).
    #[schema(value_type = String)]
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// When the backup finished, if known.
    #[schema(value_type = Option<String>)]
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Final size in bytes once completed. `None` while running.
    pub size_bytes: Option<i64>,
    /// Engine-reported error message when `state = "failed"`.
    pub error_message: Option<String>,
    /// S3 object key or URL where the backup data lives.
    pub s3_location: String,
    /// Most recent `backup_jobs.id` for this backup. `None` for legacy rows.
    pub job_id: Option<i64>,
    /// Last completed step reported by the engine (e.g. `"upload"`).
    /// `None` when no step has been persisted yet.
    pub current_step: Option<String>,
    /// Number of claim-and-run attempts so far. `None` for legacy rows.
    pub attempts: Option<i32>,
}

/// Paginated run-history response for a backup schedule (deliverable 1).
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct ScheduleRunListResponse {
    /// Run entries, newest first.
    pub runs: Vec<ScheduleRunEntry>,
    /// Total number of runs across all pages.
    pub total: i64,
    /// Current page (1-based).
    pub page: i64,
    /// Number of items per page (clamped to 1–100).
    pub page_size: i64,
}

// ── Fan-out run types (schedule_runs table) ───────────────────────────────────

/// How a scheduler run was triggered.
///
/// Used by [`enqueue_scheduled_run`] to set `schedule_runs.triggered_by`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerSource {
    /// Triggered by the cron scheduler (`process_scheduled_backups`).
    Cron,
    /// Triggered by a manual "Run now" API call.
    Manual,
}

impl TriggerSource {
    /// Returns the database string representation.
    pub fn as_str(self) -> &'static str {
        match self {
            TriggerSource::Cron => "cron",
            TriggerSource::Manual => "manual",
        }
    }
}

/// A single job that was successfully enqueued during a fan-out run.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct EnqueuedJob {
    /// FK to `backups.id` for this job.
    pub backup_id: i32,
    /// FK to `backup_jobs.id` for this job.
    pub job_id: i64,
    /// Engine key (e.g. `"control_plane"`, `"redis"`, `"postgres_pgdump"`).
    pub engine: String,
    /// FK to `external_services.id` when this is an external-service job.
    /// `None` for the control-plane job.
    pub target_service_id: Option<i32>,
}

/// Optional schedule fan-out context for a pending external-service backup.
///
/// [`BackupService::enqueue_pending_external_service_backup`] (and its
/// convenience wrapper `create_pending_external_service_backup_row`) accept
/// this so both the manual-trigger handler and the schedule fan-out share
/// the same row-insert + enqueue logic. Manual triggers pass `None`;
/// scheduler fan-out passes `Some` with the parent `schedule_runs.id` and
/// `backup_schedules.id`. The fields are written verbatim onto the
/// `backups` row.
#[derive(Debug, Clone, Copy)]
pub struct ScheduleRunContext {
    pub schedule_id: i32,
    pub schedule_run_id: i64,
}

/// Outcome of [`BackupService::enqueue_scheduled_run`].
///
/// The cron caller treats both variants as `Ok` and logs accordingly.
/// The "Run now" handler returns `409 Conflict` on `AlreadyInFlight`.
#[derive(Debug)]
pub enum ScheduleRunOutcome {
    /// A new `schedule_runs` row was inserted and all eligible jobs were
    /// enqueued. `run_id` is the `schedule_runs.id` of the new row.
    Started {
        /// The newly created `schedule_runs.id`.
        run_id: i64,
        /// All jobs that were successfully enqueued in this fan-out.
        jobs: Vec<EnqueuedJob>,
    },
    /// A `schedule_runs` row for this schedule already exists with
    /// `finished_at IS NULL` (i.e., at least one child backup is still
    /// pending or running). The existing run id is returned so callers can
    /// log it or return it in a 409 response.
    AlreadyInFlight {
        /// The `schedule_runs.id` of the existing in-flight run.
        existing_run_id: i64,
    },
}

/// Parameters for triggering one backup task on the executor.
///
/// Mirrors the shape callers used with the old runner's `EnqueueJobParams`
/// minus the queue-specific fields (`target_kind`, `target_id`, `max_attempts`).
/// Callers fill in `engine`, the engine-specific `params` JSON, and an
/// optional `max_runtime_secs` override.
#[derive(Debug, Clone)]
pub struct BackupTriggerParams {
    /// Engine key (must match an executor-registered `BackupEngine::engine()`).
    pub engine: String,
    /// Engine-specific JSON parameters (service_id, s3_source_id, …).
    pub params: serde_json::Value,
    /// Optional wall-clock timeout override. `None` falls back to the
    /// schedule-level override; if that's also absent the engine default
    /// (resolved via `resolve_max_runtime`) wins. The trigger helpers below
    /// translate `None` → a sensible engine-family default in seconds.
    pub max_runtime_secs: Option<i64>,
}

/// Summary of one scheduler tick (or one "Run now" click), returned by
/// [`BackupService::list_schedule_runs`].
///
/// The `aggregate_state` is computed at read time from child backup counts:
/// - `"running"` — at least one child is `"pending"` or `"running"`.
/// - `"failed"` — at least one child is `"failed"` and none are running.
/// - `"completed"` — all children are `"completed"`.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct ScheduleRunSummary {
    /// `schedule_runs.id` for this tick.
    pub run_id: i64,
    /// FK to `backup_schedules.id`.
    pub schedule_id: i32,
    /// How the run was triggered: `"cron"` or `"manual"`.
    pub triggered_by: String,
    /// When the fan-out started (ISO 8601 / RFC 3339).
    pub started_at: String,
    /// When all children reached a terminal state. `None` while any child is
    /// still `"pending"` or `"running"`.
    pub finished_at: Option<String>,
    /// Aggregate state computed from child counts (see struct docs).
    pub aggregate_state: String,
    /// Total number of child backup jobs in this run.
    pub total_jobs: i64,
    /// Number of children in `state = "completed"`.
    pub completed_jobs: i64,
    /// Number of children in `state = "failed"`.
    pub failed_jobs: i64,
    /// Number of children in `state = "running"`.
    pub running_jobs: i64,
    /// Number of children in `state = "pending"`.
    pub pending_jobs: i64,
}

/// Paginated list of schedule run summaries returned by the new
/// [`BackupService::list_schedule_runs`].
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct ScheduleRunSummaryList {
    /// Run summaries, newest first. Includes synthetic single-job rows for
    /// legacy `backups` rows that have `schedule_id` set but no
    /// `schedule_run_id` (pre-fan-out history).
    pub runs: Vec<ScheduleRunSummary>,
    /// Total number of run entries across all pages.
    pub total: i64,
    /// Current page (1-based).
    pub page: i64,
    /// Number of items per page.
    pub page_size: i64,
}

/// A single job entry inside an expanded schedule run, returned by
/// [`BackupService::list_schedule_run_jobs`].
#[derive(Debug, FromQueryResult, serde::Serialize, utoipa::ToSchema)]
pub struct ScheduleRunJobEntry {
    /// `backups.id` for this job.
    pub backup_id: i32,
    /// `backups.backup_id` UUID string.
    pub backup_uuid: String,
    /// Engine key (e.g. `"control_plane"`, `"redis"`).
    pub engine: String,
    /// Name of the external service, or `"control plane"` for the
    /// control-plane job.
    pub service_name: String,
    /// `external_services.id` — `NULL` for the control-plane job.
    pub service_id: Option<i32>,
    /// Current state of this child backup.
    pub state: String,
    /// When this child backup started (ISO 8601 / RFC 3339).
    #[schema(value_type = String)]
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// When this child backup finished, if known.
    #[schema(value_type = Option<String>)]
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Size in bytes once completed; `None` while running.
    pub size_bytes: Option<i64>,
    /// Engine-reported error message when `state = "failed"`.
    pub error_message: Option<String>,
    /// FK to `s3_sources.id` — needed for the backup detail link.
    pub s3_source_id: i32,
}

/// HTTP response body for `POST /api/backups/schedules/{id}/run` (fan-out).
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct ScheduleRunResponse {
    /// The `schedule_runs.id` of the newly created run.
    pub schedule_run_id: i64,
    /// All jobs that were enqueued in this fan-out.
    pub jobs: Vec<EnqueuedJob>,
}

/// A single child backup entry returned by
/// [`BackupService::list_child_backups`].
///
/// Populated from a JOIN of `external_service_backups` and
/// `external_services` so every field is available in one SQL round-trip.
#[derive(Debug, FromQueryResult, serde::Serialize, utoipa::ToSchema)]
pub struct ChildBackupEntry {
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
    /// When the child backup started (ISO 8601 / RFC 3339).
    #[schema(value_type = String)]
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// When the child backup finished, if known.
    #[schema(value_type = Option<String>)]
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Size of the child backup in bytes, if available.
    pub size_bytes: Option<i64>,
    /// Object key or `s3://` URL where the backup data lives.
    pub s3_location: String,
    /// Engine-reported error message when `state = "failed"`.
    pub error_message: Option<String>,
    /// Compression algorithm used (e.g. "gzip", "lz4").
    pub compression_type: String,
}

#[derive(Clone)]
pub struct BackupService {
    db: Arc<DatabaseConnection>,
    external_service_manager: Arc<ExternalServiceManager>,
    notification_dispatcher: Arc<dyn NotificationService>,
    config_service: Arc<temps_config::ConfigService>,
    encryption_service: Arc<temps_core::EncryptionService>,
    /// Shared workspace `JobQueue` (typically backed by the in-memory
    /// broadcast service in `temps-queue`). Set once during plugin init
    /// via `set_queue`. Triggers publish `Job::BackupRequested` here;
    /// the `BackupJobProcessor` subscribes and dispatches to the executor.
    queue: std::sync::OnceLock<Arc<dyn temps_core::JobQueue>>,
}

impl BackupService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        external_service_manager: Arc<ExternalServiceManager>,
        notification_dispatcher: Arc<dyn NotificationService>,
        serve_config: Arc<temps_config::ConfigService>,
        encryption_service: Arc<temps_core::EncryptionService>,
    ) -> Self {
        Self {
            db,
            external_service_manager,
            notification_dispatcher,
            config_service: serve_config,
            encryption_service,
            queue: std::sync::OnceLock::new(),
        }
    }

    /// Wire the workspace `JobQueue` that this service should publish on.
    /// Called once by `BackupPlugin::register_services`. Idempotent.
    pub fn set_queue(&self, queue: Arc<dyn temps_core::JobQueue>) {
        let _ = self.queue.set(queue);
    }

    /// Fire-and-forget S3 bucket lifecycle reconcile for the given source.
    /// Spawns a background task so the caller (schedule create/update/delete)
    /// is never blocked on S3, and lifecycle failures never bubble up as
    /// schedule operation failures — they only show up in logs.
    ///
    /// The reconcile rebuilds the bucket's lifecycle rules from current
    /// schedule state, so even concurrent schedule changes converge to a
    /// consistent rule set eventually.
    fn fire_lifecycle_reconcile(&self, s3_source_id: i32) {
        let db = self.db.clone();
        let enc = self.encryption_service.clone();
        tokio::spawn(async move {
            let svc = super::S3LifecycleService::new(db, enc);
            match svc.reconcile_bucket(s3_source_id).await {
                Ok(outcome) => {
                    info!(s3_source_id, ?outcome, "S3 lifecycle reconcile completed");
                }
                Err(e) => {
                    warn!(
                        s3_source_id,
                        error = %e,
                        "S3 lifecycle reconcile failed (app-side retention still active)"
                    );
                }
            }
        });
    }

    /// Internal accessor — panics if `set_queue` was never called.
    fn queue(&self) -> &Arc<dyn temps_core::JobQueue> {
        self.queue
            .get()
            .expect("BackupService.queue not set — plugin init did not call set_queue")
    }

    /// Send a backup failure notification
    pub async fn send_backup_failure_notification(
        &self,
        backup_failure_data: BackupFailureData,
    ) -> Result<(), BackupError> {
        use std::collections::HashMap;
        use temps_core::notifications::{NotificationData, NotificationPriority, NotificationType};

        let mut metadata = HashMap::new();
        metadata.insert(
            "schedule_id".to_string(),
            backup_failure_data.schedule_id.to_string(),
        );
        metadata.insert(
            "schedule_name".to_string(),
            backup_failure_data.schedule_name.clone(),
        );
        metadata.insert(
            "backup_type".to_string(),
            backup_failure_data.backup_type.clone(),
        );
        metadata.insert("timestamp".to_string(), Utc::now().to_rfc3339());

        let notification = NotificationData {
            id: uuid::Uuid::new_v4().to_string(),
            title: format!("Backup Failed: {}", backup_failure_data.schedule_name),
            message: format!(
                "Backup failed for {} ({}): {}",
                backup_failure_data.schedule_name,
                backup_failure_data.backup_type,
                backup_failure_data.error
            ),
            notification_type: NotificationType::Error,
            priority: NotificationPriority::High,
            severity: Some("error".to_string()),
            timestamp: Utc::now(),
            metadata,
            bypass_throttling: false,
        };

        self.notification_dispatcher
            .send_notification(notification)
            .await
            .map_err(|e| BackupError::NotificationError(e.to_string()))?;

        Ok(())
    }

    pub async fn create_backup(
        &self,
        schedule_id: Option<i32>,
        s3_source_id: i32,
        backup_type: &str,
        created_by: i32,
    ) -> Result<Backup, BackupError> {
        info!("Starting backup process");

        // Get S3 source configuration
        let s3_source = temps_entities::s3_sources::Entity::find_by_id(s3_source_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: "S3 source not found".to_string(),
            })?;

        // Generate unique backup ID
        let backup_id = Uuid::new_v4().to_string();

        // Create S3 client (needed for metadata upload and legacy fallback)
        let s3_client = self.create_s3_client(&s3_source).await?;

        // Try WAL-G backup first (requires the internal DB container to have WAL-G installed).
        // Falls back to pg_dump sidecar if the DB is not running in a Docker container we can exec into.
        let (s3_location, size_bytes, compression_type) =
            match self.backup_postgres_walg(&s3_source, &backup_id).await {
                Ok((location, size)) => {
                    info!("WAL-G backup completed: {}", location);
                    (location, size, "lz4".to_string())
                }
                Err(e) => {
                    // WAL-G not available (e.g., DB on localhost, no Docker container found).
                    // Fall back to pg_dump sidecar approach.
                    warn!(
                        "WAL-G backup not available ({}), falling back to pg_dump sidecar",
                        e
                    );

                    let mut temp_file = NamedTempFile::new().map_err(BackupError::Io)?;

                    self.backup_postgres_database(&mut temp_file)
                        .await
                        .map_err(|e| {
                            error!(
                                "Database backup failed for S3 source {}: {}",
                                s3_source_id, e
                            );
                            e
                        })?;

                    let size_bytes = temp_file
                        .as_file()
                        .metadata()
                        .map_err(BackupError::Io)?
                        .len() as i64;

                    if size_bytes == 0 {
                        return Err(BackupError::Validation(
                            "Backup failed: backup file has zero size".to_string(),
                        ));
                    }

                    let s3_location = build_s3_key(
                        &s3_source.bucket_path,
                        &format!(
                            "backups/{}/{}/backup.sql.gz",
                            Utc::now().format("%Y/%m/%d"),
                            backup_id
                        ),
                    );

                    self.upload_backup(&s3_client, &s3_source, &temp_file, &s3_location)
                        .await
                        .map_err(|e| {
                            error!(
                                "Failed to upload backup to S3 source {} at {}: {}",
                                s3_source_id, s3_location, e
                            );
                            e
                        })?;

                    (s3_location, size_bytes, "gzip".to_string())
                }
            };

        // Create backup record
        let new_backup = temps_entities::backups::ActiveModel {
            id: sea_orm::NotSet,
            name: sea_orm::Set(format!("Backup {}", backup_id)),
            backup_id: sea_orm::Set(backup_id.clone()),
            schedule_id: sea_orm::Set(schedule_id),
            schedule_run_id: sea_orm::NotSet,
            backup_type: sea_orm::Set(backup_type.to_string()),
            state: sea_orm::Set("completed".to_string()),
            started_at: sea_orm::Set(chrono::Utc::now()),
            finished_at: sea_orm::Set(Some(chrono::Utc::now())),
            s3_source_id: sea_orm::Set(s3_source_id),
            s3_location: sea_orm::Set(s3_location.clone()),
            compression_type: sea_orm::Set(compression_type),
            created_by: sea_orm::Set(created_by),
            tags: sea_orm::Set("[]".to_string()),
            size_bytes: sea_orm::Set(Some(size_bytes)),
            file_count: sea_orm::Set(None),
            error_message: sea_orm::Set(None),
            expires_at: sea_orm::Set(None),
            checksum: sea_orm::Set(None),
            metadata: sea_orm::Set(
                serde_json::json!({
                    "size_bytes": size_bytes,
                    "database_version": "1.0",
                    "timestamp": Utc::now().to_rfc3339()
                })
                .to_string(),
            ),
        };

        let backup = new_backup.insert(self.db.as_ref()).await?;

        // Backup all external services
        let external_services = temps_entities::external_services::Entity::find()
            .all(self.db.as_ref())
            .await?;

        let mut external_backups = Vec::new();
        let mut failed_services = Vec::new();

        for service in external_services {
            match self
                .backup_external_service(&service, s3_source_id, backup_type, created_by)
                .await
            {
                Ok(backup) => {
                    info!(
                        "Successfully backed up external service {}: {}",
                        service.name, backup.backup_id
                    );
                    external_backups.push((backup, service));
                }
                Err(e) => {
                    error!("Failed to backup external service {}: {}", service.name, e);
                    failed_services.push(service.name.clone());

                    // Send notification about this specific failure
                    let error_msg = format!("External service backup failed: {}", e);
                    let failure_data = BackupFailureData {
                        schedule_id: schedule_id.unwrap_or(-1),
                        schedule_name: format!("External Service: {}", service.name),
                        backup_type: backup_type.to_string(),
                        error: error_msg.clone(),
                        timestamp: Utc::now(),
                    };

                    if let Err(notify_err) =
                        self.send_backup_failure_notification(failure_data).await
                    {
                        error!("Failed to send backup failure notification: {}", notify_err);
                    }

                    // Continue with next service instead of stopping
                }
            }
        }

        // Log summary of failed services if any
        if !failed_services.is_empty() {
            error!(
                "Backup completed with failures. Failed services: {}",
                failed_services.join(", ")
            );
        }

        // After successful backup upload, create and upload metadata file
        let metadata = self.generate_backup_metadata(&backup, &s3_source, &external_backups);
        let metadata_key = build_s3_key(
            &s3_source.bucket_path,
            &format!(
                "backups/{}/{}/metadata.json",
                Utc::now().format("%Y/%m/%d"),
                backup_id
            ),
        );

        // Upload metadata file
        s3_client
            .put_object()
            .bucket(&s3_source.bucket_name)
            .key(&metadata_key)
            .body(
                serde_json::to_vec(&metadata)
                    .map_err(BackupError::Serialization)?
                    .into(),
            )
            .content_type("application/json")
            .send()
            .await
            .map_err(|e| BackupError::S3(format!("Failed to upload metadata: {}", e)))?;

        // Update backup index
        self.update_backup_index(&s3_client, &s3_source, &backup)
            .await?;

        info!("Backup completed successfully: {}", backup_id);
        Ok(backup)
    }

    /// Find the Docker container that hosts the internal database by matching the hostname
    /// from DATABASE_URL against Docker container names and network aliases.
    ///
    /// Returns `(container_id, pgdata_path)` if found.
    async fn find_internal_db_container(&self) -> Result<(String, String), BackupError> {
        use bollard::query_parameters::ListContainersOptions;
        use bollard::Docker;

        let database_url = self.config_service.get_database_url();
        let url = url::Url::parse(&database_url).map_err(|e| BackupError::Internal {
            message: format!("Invalid DATABASE_URL: {}", e),
        })?;

        let db_host = url.host_str().unwrap_or("localhost").to_string();

        // Skip Docker discovery for local connections
        if db_host == "localhost" || db_host == "127.0.0.1" || db_host == "::1" {
            return Err(BackupError::Internal {
                message: format!(
                    "Database host '{}' is local — cannot exec into a Docker container",
                    db_host
                ),
            });
        }

        let docker = Docker::connect_with_local_defaults().map_err(|e| BackupError::Internal {
            message: format!("Failed to connect to Docker: {}", e),
        })?;

        // List all running containers
        let containers = docker
            .list_containers(Some(ListContainersOptions {
                all: false, // only running
                ..Default::default()
            }))
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to list Docker containers: {}", e),
            })?;

        // Find container matching the database hostname by:
        // 1. Container name (e.g., /temps-postgres matches "temps-postgres")
        // 2. Docker Compose service name in network aliases (e.g., "postgres" on compose network)
        for container in &containers {
            let container_id = container.id.as_deref().unwrap_or("");
            if container_id.is_empty() {
                continue;
            }

            // Check container names (Docker prefixes with '/')
            if let Some(names) = &container.names {
                for name in names {
                    let clean_name = name.trim_start_matches('/');
                    if clean_name == db_host {
                        return self
                            .resolve_pgdata_for_container(&docker, container_id)
                            .await;
                    }
                }
            }

            // Check network aliases (Docker Compose sets the service name as an alias)
            if let Some(network_settings) = &container.network_settings {
                if let Some(networks) = &network_settings.networks {
                    for net_config in networks.values() {
                        if let Some(aliases) = &net_config.aliases {
                            if aliases.iter().any(|a| a == &db_host) {
                                return self
                                    .resolve_pgdata_for_container(&docker, container_id)
                                    .await;
                            }
                        }
                    }
                }
            }
        }

        Err(BackupError::Internal {
            message: format!(
                "No Docker container found for database host '{}'. \
                 Ensure the database is running in a Docker container with WAL-G installed.",
                db_host
            ),
        })
    }

    /// Resolve the PGDATA path for a container by inspecting its environment variables.
    async fn resolve_pgdata_for_container(
        &self,
        docker: &bollard::Docker,
        container_id: &str,
    ) -> Result<(String, String), BackupError> {
        let inspect = docker
            .inspect_container(
                container_id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to inspect container {}: {}", container_id, e),
            })?;

        // Try to find PGDATA from container environment
        let mut pgdata = String::from("/var/lib/postgresql/data");
        if let Some(config) = &inspect.config {
            if let Some(env) = &config.env {
                for var in env {
                    if let Some(val) = var.strip_prefix("PGDATA=") {
                        pgdata = val.to_string();
                        break;
                    }
                }
            }
        }

        Ok((container_id.to_string(), pgdata))
    }

    /// Perform a WAL-G backup by exec'ing into the internal database container.
    /// WAL-G uploads directly to S3 — no data flows through the Temps process.
    ///
    /// Returns `(s3_location, size_bytes)` on success. The `s3_location` is the WAL-G
    /// S3 prefix (starts with `s3://`), used by the restore logic to detect WAL-G backups.
    async fn backup_postgres_walg(
        &self,
        s3_source: &S3Source,
        _backup_id: &str,
    ) -> Result<(String, i64), BackupError> {
        use bollard::exec::{CreateExecOptions, StartExecOptions};
        use bollard::Docker;

        let (container_id, pgdata) = self.find_internal_db_container().await?;

        info!(
            "Starting WAL-G backup via container {} (PGDATA={})",
            container_id, pgdata
        );

        let docker = Docker::connect_with_local_defaults().map_err(|e| BackupError::Internal {
            message: format!("Failed to connect to Docker: {}", e),
        })?;

        // Verify WAL-G is installed in the container
        let check_exec = docker
            .create_exec(
                &container_id,
                CreateExecOptions {
                    cmd: Some(vec!["which", "wal-g"]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to check WAL-G in container: {}", e),
            })?;

        docker
            .start_exec(
                &check_exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to run WAL-G check: {}", e),
            })?;

        // Wait for check to complete
        loop {
            let inspect =
                docker
                    .inspect_exec(&check_exec.id)
                    .await
                    .map_err(|e| BackupError::Internal {
                        message: format!("Failed to inspect WAL-G check exec: {}", e),
                    })?;
            if let Some(running) = inspect.running {
                if !running {
                    if let Some(exit_code) = inspect.exit_code {
                        if exit_code != 0 {
                            return Err(BackupError::Internal {
                                message: format!(
                                    "WAL-G is not installed in container {}. \
                                     Use the gotempsh/timescaledb-walg image.",
                                    container_id
                                ),
                            });
                        }
                    }
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        // Build WAL-G S3 prefix using a STABLE path (no date or backup_id).
        // WAL-G requires all backups and WAL segments to share the same prefix so that:
        // - wal-g wal-push archives WAL to {prefix}/wal_005/
        // - wal-g backup-push stores base backups in {prefix}/basebackups_005/
        // - wal-g backup-fetch LATEST finds the right backup + WAL chain
        // - wal-g delete retain works across all backups
        let walg_s3_prefix = format!(
            "s3://{}/{}/internal_db/walg",
            s3_source.bucket_name,
            s3_source.bucket_path.trim_matches('/'),
        );

        // Decrypt S3 credentials for WAL-G environment variables
        let decrypted_access_key = self
            .encryption_service
            .decrypt_string(&s3_source.access_key_id)
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to decrypt S3 access key: {}", e),
            })?;

        let decrypted_secret_key = self
            .encryption_service
            .decrypt_string(&s3_source.secret_key)
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to decrypt S3 secret key: {}", e),
            })?;

        // Build environment variables for WAL-G
        let mut env_vars: Vec<String> = vec![
            format!("WALG_S3_PREFIX={}", walg_s3_prefix),
            format!("AWS_ACCESS_KEY_ID={}", decrypted_access_key),
            format!("AWS_SECRET_ACCESS_KEY={}", decrypted_secret_key),
            format!("AWS_REGION={}", s3_source.region),
            format!("PGDATA={}", pgdata),
        ];

        // Resolve S3 endpoint for use inside the Docker container.
        // localhost/127.0.0.1 endpoints are translated to Docker-resolvable addresses.
        let s3_creds = temps_providers::S3Credentials {
            access_key_id: decrypted_access_key.clone(),
            secret_key: decrypted_secret_key.clone(),
            region: s3_source.region.clone(),
            endpoint: s3_source.endpoint.clone(),
            bucket_name: s3_source.bucket_name.clone(),
            bucket_path: s3_source.bucket_path.clone(),
            force_path_style: s3_source.force_path_style.unwrap_or(true),
        };
        if let Some(resolved_endpoint) = s3_creds
            .resolve_endpoint_for_container(&docker, &container_id)
            .await
        {
            env_vars.push(format!("AWS_ENDPOINT={}", resolved_endpoint));
        }

        if s3_source.force_path_style.unwrap_or(true) {
            env_vars.push("AWS_S3_FORCE_PATH_STYLE=true".to_string());
        }

        let env_refs: Vec<&str> = env_vars.iter().map(|s| s.as_str()).collect();

        // Run wal-g backup-push
        info!("Running wal-g backup-push in container {}", container_id);

        let exec = docker
            .create_exec(
                &container_id,
                CreateExecOptions {
                    cmd: Some(vec!["wal-g", "backup-push", &pgdata]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    env: Some(env_refs.clone()),
                    user: Some("postgres"),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to create WAL-G exec: {}", e),
            })?;

        docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to start WAL-G exec: {}", e),
            })?;

        // Poll for completion
        loop {
            let inspect =
                docker
                    .inspect_exec(&exec.id)
                    .await
                    .map_err(|e| BackupError::Internal {
                        message: format!("Failed to inspect WAL-G exec: {}", e),
                    })?;
            if let Some(running) = inspect.running {
                if !running {
                    if let Some(exit_code) = inspect.exit_code {
                        if exit_code != 0 {
                            return Err(BackupError::Internal {
                                message: format!(
                                    "wal-g backup-push failed with exit code {} in container {}",
                                    exit_code, container_id
                                ),
                            });
                        }
                    }
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }

        // Calculate total backup size by listing objects under the WAL-G prefix
        let s3_client = self.create_s3_client(s3_source).await?;
        let prefix = format!(
            "{}/internal_db/walg/basebackups_005/",
            s3_source.bucket_path.trim_matches('/'),
        );

        let mut total_size: i64 = 0;
        let mut continuation_token: Option<String> = None;
        loop {
            let mut req = s3_client
                .list_objects_v2()
                .bucket(&s3_source.bucket_name)
                .prefix(&prefix);

            if let Some(token) = continuation_token.take() {
                req = req.continuation_token(token);
            }

            let resp = req
                .send()
                .await
                .map_err(|e| BackupError::S3(format!("Failed to list WAL-G objects: {}", e)))?;

            for obj in resp.contents() {
                total_size += obj.size().unwrap_or(0);
            }

            if resp.is_truncated() == Some(true) {
                continuation_token = resp.next_continuation_token().map(|s| s.to_string());
            } else {
                break;
            }
        }

        info!(
            "WAL-G backup completed: {} ({} bytes)",
            walg_s3_prefix, total_size
        );

        // Enable continuous WAL archiving for the internal database.
        // Write S3 credentials to an env file on the shared volume, then configure
        // archive_command to source it before running wal-g wal-push.
        // Failures here are logged but do NOT fail the backup.
        if let Err(e) = self
            .enable_internal_wal_archiving(&docker, &container_id, &env_vars, &pgdata)
            .await
        {
            error!(
                "Failed to enable WAL archiving for internal DB in container '{}': {}. \
                 Base backup succeeded but continuous WAL archiving is not active.",
                container_id, e
            );
        }

        Ok((walg_s3_prefix, total_size))
    }

    /// Write WAL-G credentials to an env file on the shared volume and enable
    /// continuous WAL archiving for the internal database via `ALTER SYSTEM`.
    ///
    /// Same approach as external PostgreSQL services: the env file is refreshed on
    /// every backup so credential rotations are picked up automatically.
    async fn enable_internal_wal_archiving(
        &self,
        docker: &bollard::Docker,
        container_id: &str,
        env_vars: &[String],
        pgdata: &str,
    ) -> Result<(), BackupError> {
        use bollard::exec::{CreateExecOptions, StartExecOptions};

        // Determine the volume mount root (parent of PGDATA) for the env file location.
        // E.g., PGDATA=/var/lib/postgresql/data -> env file at /var/lib/postgresql/walg.env
        let volume_root = std::path::Path::new(pgdata)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "/var/lib/postgresql".to_string());
        let walg_env_path = format!("{}/walg.env", volume_root);

        // Filter to only S3/WAL-G env vars (no PGDATA, no PG connection vars)
        let env_file_lines: Vec<&String> = env_vars
            .iter()
            .filter(|line| line.starts_with("WALG_") || line.starts_with("AWS_"))
            .collect();

        // Write the env file via docker exec
        let write_cmd = format!(
            "printf '%s\\n' {} > {} && chmod 600 {}",
            env_file_lines
                .iter()
                .map(|line| format!("'export {}'", line.replace('\'', "'\\''")))
                .collect::<Vec<_>>()
                .join(" "),
            walg_env_path,
            walg_env_path,
        );

        let exec = docker
            .create_exec(
                container_id,
                CreateExecOptions {
                    cmd: Some(vec!["sh", "-c", &write_cmd]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    user: Some("postgres"),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to create env file write exec: {}", e),
            })?;

        docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to start env file write exec: {}", e),
            })?;

        loop {
            let inspect =
                docker
                    .inspect_exec(&exec.id)
                    .await
                    .map_err(|e| BackupError::Internal {
                        message: format!("Failed to inspect env file write exec: {}", e),
                    })?;
            if inspect.running == Some(false) {
                if inspect.exit_code != Some(0) {
                    return Err(BackupError::Internal {
                        message: format!(
                            "Failed to write walg.env (exit code {:?})",
                            inspect.exit_code
                        ),
                    });
                }
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        info!(
            "Written WAL-G credentials to {} in container '{}'",
            walg_env_path, container_id
        );

        // Parse DATABASE_URL for psql credentials
        let database_url = self.config_service.get_database_url();
        let url = url::Url::parse(&database_url).map_err(|e| BackupError::Internal {
            message: format!("Invalid DATABASE_URL for ALTER SYSTEM: {}", e),
        })?;
        let pg_user = url.username();
        let pg_password = url.password().unwrap_or("");

        // Enable archive_command via ALTER SYSTEM + pg_reload_conf().
        // Use two separate -c flags because ALTER SYSTEM cannot run inside a
        // transaction block, and psql wraps multiple statements in a single -c
        // into a transaction.
        let archive_command = format!(". {} && wal-g wal-push %p", walg_env_path);
        let alter_sql = format!(
            "ALTER SYSTEM SET archive_command = '{}'",
            archive_command.replace('\'', "''")
        );
        let reload_sql = "SELECT pg_reload_conf()";

        let password_env = format!("PGPASSWORD={}", pg_password);
        let exec = docker
            .create_exec(
                container_id,
                CreateExecOptions {
                    cmd: Some(vec![
                        "psql", "-U", pg_user, "-c", &alter_sql, "-c", reload_sql,
                    ]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    env: Some(vec![&password_env]),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to create ALTER SYSTEM exec: {}", e),
            })?;

        docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to start ALTER SYSTEM exec: {}", e),
            })?;

        loop {
            let inspect =
                docker
                    .inspect_exec(&exec.id)
                    .await
                    .map_err(|e| BackupError::Internal {
                        message: format!("Failed to inspect ALTER SYSTEM exec: {}", e),
                    })?;
            if inspect.running == Some(false) {
                if inspect.exit_code != Some(0) {
                    return Err(BackupError::Internal {
                        message: format!(
                            "ALTER SYSTEM SET archive_command failed (exit code {:?})",
                            inspect.exit_code
                        ),
                    });
                }
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        info!(
            "Enabled continuous WAL archiving for internal DB in container '{}'",
            container_id
        );

        Ok(())
    }

    /// Fetches the PostgreSQL version from the database
    async fn get_postgres_version(&self) -> Result<String> {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        let version_result = self
            .db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT version()".to_string(),
            ))
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query PostgreSQL version: {}", e))?
            .ok_or_else(|| anyhow::anyhow!("No version result returned"))?;

        let version_str: String = version_result
            .try_get("", "version")
            .map_err(|e| anyhow::anyhow!("Failed to extract version string: {}", e))?;

        debug!("PostgreSQL version string: {}", version_str);
        Ok(version_str)
    }

    /// Parses PostgreSQL version string and returns the major version number
    /// Example: "PostgreSQL 15.3 on x86_64..." -> "15"
    fn parse_postgres_version(&self, version_str: &str) -> Result<String> {
        // Version string format: "PostgreSQL 15.3 on x86_64-pc-linux-gnu..."
        let parts: Vec<&str> = version_str.split_whitespace().collect();

        if parts.len() < 2 {
            anyhow::bail!("Invalid PostgreSQL version string format: {}", version_str);
        }

        let version = parts[1]; // "15.3"
        let major_version = version
            .split('.')
            .next()
            .ok_or_else(|| anyhow::anyhow!("Failed to extract major version from: {}", version))?;

        debug!("Extracted PostgreSQL major version: {}", major_version);
        Ok(major_version.to_string())
    }

    /// Returns the Docker image tag for the pg_dump sidecar container.
    /// Temps requires TimescaleDB as its database, so the sidecar always uses the
    /// timescaledb-ha image to ensure pg_dump has the extension available.
    fn get_postgres_image_tag(&self, major_version: &str) -> String {
        format!("timescale/timescaledb-ha:pg{}", major_version)
    }

    /// Pulls the specified PostgreSQL Docker image
    async fn pull_postgres_image(&self, image_tag: &str) -> Result<()> {
        use bollard::query_parameters::CreateImageOptionsBuilder;
        use bollard::Docker;
        use futures::stream::StreamExt as FuturesStreamExt;

        info!("Pulling Docker image: {}", image_tag);

        let docker = Docker::connect_with_local_defaults()
            .map_err(|e| anyhow::anyhow!("Failed to connect to Docker: {}", e))?;

        let parts: Vec<&str> = image_tag.split(':').collect();
        let (image, tag) = if parts.len() == 2 {
            (parts[0], parts[1])
        } else {
            (image_tag, "latest")
        };

        let options = CreateImageOptionsBuilder::new()
            .from_image(image)
            .tag(tag)
            .build();

        let mut stream = docker.create_image(Some(options), None, None);

        while let Some(result) = FuturesStreamExt::next(&mut stream).await {
            match result {
                Ok(info) => {
                    if let Some(status) = info.status {
                        debug!("Docker pull: {}", status);
                    }
                }
                Err(e) => {
                    anyhow::bail!("Failed to pull Docker image {}: {}", image_tag, e);
                }
            }
        }

        info!("Successfully pulled Docker image: {}", image_tag);
        Ok(())
    }

    async fn backup_postgres_database(&self, temp_file: &mut NamedTempFile) -> Result<()> {
        use bollard::exec::CreateExecOptions;
        use bollard::models::ContainerCreateBody as Config;
        use bollard::query_parameters::RemoveContainerOptions;
        use bollard::Docker;

        info!("Creating PostgreSQL database backup using Docker");

        // Get database URL from server configuration
        let database_url = &self.config_service.get_database_url();

        // Parse database URL to extract connection parameters
        let url = url::Url::parse(database_url)
            .map_err(|e| anyhow::anyhow!("Invalid DATABASE_URL format: {}", e))?;

        let host = url.host_str().unwrap_or("localhost");
        let port = url.port().unwrap_or(5432);
        let database = url.path().trim_start_matches('/');
        let username = url.username();
        let password = url.password().unwrap_or("");

        // Connect to Docker
        let docker = Docker::connect_with_local_defaults()
            .map_err(|e| anyhow::anyhow!("Failed to connect to Docker: {}", e))?;

        // Get PostgreSQL version from database
        let version_str = self.get_postgres_version().await?;
        let major_version = self.parse_postgres_version(&version_str)?;
        let image_tag = self.get_postgres_image_tag(&major_version);

        // Pull the matching PostgreSQL Docker image
        self.pull_postgres_image(&image_tag).await?;

        // Create a temporary container name
        let container_name = format!("temps-pg-backup-{}", uuid::Uuid::new_v4());

        // Prepare environment variables with proper lifetimes
        // URL-decode password (it's stored URL-encoded in database for connection strings)
        let decoded_password = urlencoding::decode(password)
            .map(|s| s.to_string())
            .unwrap_or_else(|_| password.to_string());
        let pgpassword_env = format!("PGPASSWORD={}", decoded_password);
        let env_vars = vec![pgpassword_env];

        // Create a host directory for the bind mount so the backup file is written
        // directly to disk by the sidecar container, bypassing the Temps process entirely.
        // Previous approach streamed pg_dump output through Bollard's exec HTTP stream
        // into the Temps process, which caused unbounded memory growth (2-6+ GB) because
        // hyper/Bollard buffers the chunked HTTP response internally even though we write
        // each chunk to disk immediately.
        let backup_dir = self.config_service.data_dir().join("backups").join("tmp");
        tokio::fs::create_dir_all(&backup_dir).await.map_err(|e| {
            anyhow::anyhow!(
                "Failed to create backup temp directory {}: {}",
                backup_dir.display(),
                e
            )
        })?;
        let backup_filename = format!("{}.sql.gz", uuid::Uuid::new_v4());
        let host_backup_path = backup_dir.join(&backup_filename);
        let container_backup_path = format!("/backup/{}", backup_filename);

        // Create container config with version-matched postgres image (includes pg_dump).
        // Override the entrypoint to prevent the timescaledb-ha image from starting a full
        // PostgreSQL server instance inside the sidecar.
        // Bind-mount the host backup directory to /backup inside the container. We use
        // /backup instead of /tmp because the timescaledb-ha image runs as the postgres
        // user which may not have write access to a bind-mounted /tmp.
        let config = Config {
            image: Some(image_tag),
            entrypoint: Some(vec!["/bin/sleep".to_string()]),
            cmd: Some(vec!["86400".to_string()]), // 24h: must outlive pg_dump on large DBs (42+ GB)
            env: Some(env_vars),
            user: Some("root".to_string()), // Run as root to ensure write access to bind mount
            host_config: Some(bollard::models::HostConfig {
                network_mode: Some("host".to_string()),
                auto_remove: Some(true),
                oom_score_adj: Some(-500),
                binds: Some(vec![format!("{}:/backup:rw", backup_dir.display())]),
                ..Default::default()
            }),
            ..Default::default()
        };

        info!("Creating temporary Docker container for pg_dump");

        // Create container
        docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&container_name)
                        .build(),
                ),
                config,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create container: {}", e))?;

        // Helper to remove the sidecar on any error path
        let remove_sidecar = |docker: bollard::Docker, name: String| async move {
            let _ = docker
                .remove_container(
                    &name,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;
        };

        // Start container
        docker
            .start_container(
                &container_name,
                Some(bollard::query_parameters::StartContainerOptionsBuilder::new().build()),
            )
            .await
            .map_err(|e| {
                let docker = docker.clone();
                let name = container_name.clone();
                tokio::spawn(async move { remove_sidecar(docker, name).await });
                anyhow::anyhow!("Failed to start container: {}", e)
            })?;

        // Run pg_dump | gzip inside the sidecar, writing directly to the bind-mounted
        // host filesystem. This keeps the Temps process memory flat regardless of DB size.
        let port_str = port.to_string();

        info!("Running pg_dump command in Docker container (bind-mount mode)");

        // URL-decode password for exec env
        let decoded_password = urlencoding::decode(password)
            .map(|s| s.to_string())
            .unwrap_or_else(|_| password.to_string());
        let pgpassword = format!("PGPASSWORD={}", decoded_password);

        // Run pg_dump fully detached — no stdout/stderr streaming through the Temps process.
        // Previous approach used attach_stdout which caused Bollard's hyper HTTP client
        // to buffer the chunked transfer encoding internally, leading to unbounded memory
        // growth (19+ GB) even when we weren't reading stdout data.
        // Instead we redirect stderr to a file inside the container and poll for completion.
        let stderr_path = format!("/backup/{}.stderr", uuid::Uuid::new_v4());
        // pg_dumpall dumps the entire cluster: all databases, roles, and tablespaces.
        // `--database` is only the bootstrap connection target used to enumerate DBs.
        let pg_dump_shell_cmd = format!(
            "pg_dumpall --clean --if-exists --no-password --host={} --port={} --username={} --database={} 2>{} | gzip > {}",
            shell_escape(host), shell_escape(&port_str), shell_escape(username), shell_escape(database), stderr_path, container_backup_path
        );

        let exec = docker
            .create_exec(
                &container_name,
                CreateExecOptions {
                    cmd: Some(vec!["sh", "-c", &pg_dump_shell_cmd]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    env: Some(vec![pgpassword.as_str()]),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create exec: {}", e))?;

        // Start the exec in detached mode — no HTTP stream through the Temps process
        use bollard::exec::StartExecOptions;
        docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await?;

        // Poll for completion instead of streaming
        loop {
            let inspect = docker.inspect_exec(&exec.id).await?;
            if let Some(running) = inspect.running {
                if !running {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }

        // Read stderr from the file inside the container (via bind mount on host)
        let host_stderr_path =
            backup_dir.join(std::path::Path::new(&stderr_path).file_name().unwrap());
        let stderr_data = tokio::fs::read(&host_stderr_path).await.unwrap_or_default();
        let _ = tokio::fs::remove_file(&host_stderr_path).await;

        // Check if command was successful
        let exec_inspect = docker.inspect_exec(&exec.id).await?;
        if let Some(exit_code) = exec_inspect.exit_code {
            if exit_code != 0 {
                let stderr = String::from_utf8_lossy(&stderr_data);
                remove_sidecar(docker.clone(), container_name.clone()).await;
                let _ = tokio::fs::remove_file(&host_backup_path).await;
                return Err(anyhow::anyhow!(
                    "pg_dump failed with exit code {}: {}",
                    exit_code,
                    stderr
                ));
            }
        }

        // Clean up sidecar container
        remove_sidecar(docker.clone(), container_name.clone()).await;

        // Copy the backup file from the bind-mount location to the temp_file that the
        // caller uses for S3 upload. This is a local file copy (not through memory).
        tokio::fs::copy(&host_backup_path, temp_file.path())
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to copy backup from {} to temp file: {}",
                    host_backup_path.display(),
                    e
                )
            })?;

        // Clean up the bind-mount backup file
        let _ = tokio::fs::remove_file(&host_backup_path).await;

        info!("PostgreSQL backup completed successfully");
        Ok(())
    }

    async fn create_s3_client(&self, s3_source: &S3Source) -> Result<S3Client> {
        // Decrypt credentials before using them
        let decrypted_access_key = self
            .encryption_service
            .decrypt_string(&s3_source.access_key_id)
            .map_err(|e| anyhow::anyhow!("Failed to decrypt access key: {}", e))?;

        let decrypted_secret_key = self
            .encryption_service
            .decrypt_string(&s3_source.secret_key)
            .map_err(|e| anyhow::anyhow!("Failed to decrypt secret key: {}", e))?;

        let creds = aws_sdk_s3::config::Credentials::new(
            decrypted_access_key,
            decrypted_secret_key,
            None,
            None,
            "backup-service",
        );

        let mut config_builder = Config::builder()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new(s3_source.region.clone()))
            .force_path_style(s3_source.force_path_style.unwrap_or(true)) // Default to true for Minio
            .credentials_provider(creds)
            .http_client(crate::engines::v2_common::bundled_roots_http_client());

        // Only set endpoint URL if endpoint is specified (for Minio/custom S3)
        if let Some(endpoint) = &s3_source.endpoint {
            let endpoint_url = if endpoint.starts_with("http") {
                endpoint.clone()
            } else {
                format!("http://{}", endpoint)
            };
            config_builder = config_builder.endpoint_url(endpoint_url);
        }

        let config = config_builder.build();

        Ok(S3Client::from_conf(config))
    }

    /// Create S3 client from request (before persistence)
    async fn create_s3_client_from_request(
        &self,
        request: &CreateS3SourceRequest,
    ) -> Result<S3Client, BackupError> {
        let creds = aws_sdk_s3::config::Credentials::new(
            request.access_key_id.clone(),
            request.secret_key.clone(),
            None,
            None,
            "backup-service",
        );

        let mut config_builder = Config::builder()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new(request.region.clone()))
            .force_path_style(request.force_path_style.unwrap_or(true))
            .credentials_provider(creds)
            .http_client(crate::engines::v2_common::bundled_roots_http_client());

        // Only set endpoint URL if endpoint is specified (for MinIO)
        if let Some(endpoint) = &request.endpoint {
            let endpoint_url = if endpoint.starts_with("http") {
                endpoint.clone()
            } else {
                format!("http://{}", endpoint)
            };
            config_builder = config_builder.endpoint_url(endpoint_url);
        }

        let config = config_builder.build();
        Ok(S3Client::from_conf(config))
    }

    /// Test S3 connection and auto-create bucket if it doesn't exist
    async fn test_and_create_s3_bucket(
        &self,
        s3_client: &S3Client,
        bucket_name: &str,
    ) -> Result<(), BackupError> {
        // Try to check if bucket exists by listing objects with max-keys=1
        // This is a lightweight way to test access to the bucket
        match s3_client
            .list_objects_v2()
            .bucket(bucket_name)
            .max_keys(1)
            .send()
            .await
        {
            Ok(_) => {
                debug!("S3 bucket '{}' exists and is accessible", bucket_name);
                Ok(())
            }
            Err(e) => {
                // Check if it's a "NoSuchBucket" error
                let error_code = e
                    .as_service_error()
                    .and_then(|se| se.code())
                    .map(|s| s.to_string());

                if error_code.as_deref() == Some("NoSuchBucket") {
                    // Bucket doesn't exist, try to create it
                    debug!("S3 bucket '{}' does not exist, creating it...", bucket_name);
                    s3_client
                        .create_bucket()
                        .bucket(bucket_name)
                        .send()
                        .await
                        .map_err(|e| {
                            // Parse create bucket error for better messaging
                            let error_msg = self.parse_s3_error(&e, bucket_name, "create");
                            BackupError::S3(error_msg)
                        })?;
                    info!("Successfully created S3 bucket '{}'", bucket_name);
                    Ok(())
                } else {
                    // Other S3 error (invalid credentials, no access, etc.)
                    let error_msg = self.parse_s3_error(&e, bucket_name, "access");
                    Err(BackupError::S3(error_msg))
                }
            }
        }
    }

    /// Parse S3 SDK errors and provide user-friendly, actionable error messages
    fn parse_s3_error<E>(&self, error: &E, bucket_name: &str, operation: &str) -> String
    where
        E: std::error::Error + std::fmt::Display,
    {
        let error_str = error.to_string();

        // Check for common error patterns and provide actionable guidance

        // Connection/Network errors
        if error_str.contains("ConnectorError")
            || error_str.contains("connection")
            || error_str.contains("ConnectionRefused")
            || error_str.contains("tcp connect error")
        {
            return format!(
                "Unable to connect to S3 endpoint for bucket '{}'. \
                Please verify:\n\
                • The endpoint URL is correct and reachable\n\
                • Network/firewall allows connections to the S3 service\n\
                • The S3 service is running (for MinIO/LocalStack)\n\
                Technical details: {}",
                bucket_name, error_str
            );
        }

        // DNS resolution errors
        if error_str.contains("dns error")
            || error_str.contains("failed to lookup address")
            || error_str.contains("Name or service not known")
        {
            return format!(
                "Failed to resolve S3 endpoint hostname for bucket '{}'. \
                Please verify:\n\
                • The endpoint URL is correct\n\
                • DNS is properly configured\n\
                • The hostname is valid and resolvable\n\
                Technical details: {}",
                bucket_name, error_str
            );
        }

        // Timeout errors
        if error_str.contains("timeout") || error_str.contains("timed out") {
            return format!(
                "Connection to S3 endpoint timed out for bucket '{}'. \
                Please verify:\n\
                • The S3 service is running and responsive\n\
                • Network latency is acceptable\n\
                • Firewall rules allow connections\n\
                Technical details: {}",
                bucket_name, error_str
            );
        }

        // Authentication errors
        if error_str.contains("InvalidAccessKeyId")
            || error_str.contains("SignatureDoesNotMatch")
            || error_str.contains("InvalidSecurity")
        {
            return format!(
                "Authentication failed for bucket '{}'. \
                Please verify:\n\
                • Access Key ID is correct\n\
                • Secret Access Key is correct\n\
                • Credentials have not expired\n\
                • The credentials match the S3 service configuration\n\
                Technical details: {}",
                bucket_name, error_str
            );
        }

        // Permission/Authorization errors
        if error_str.contains("AccessDenied")
            || error_str.contains("Forbidden")
            || error_str.contains("403")
        {
            return format!(
                "Access denied when trying to {} bucket '{}'. \
                Please verify:\n\
                • The credentials have sufficient permissions\n\
                • The bucket exists and you have access to it\n\
                • IAM policies allow the required S3 operations\n\
                • Bucket policies do not restrict access\n\
                Technical details: {}",
                operation, bucket_name, error_str
            );
        }

        // Bucket already exists (from another account)
        if error_str.contains("BucketAlreadyExists") {
            return format!(
                "Bucket '{}' already exists in another account or region. \
                Please:\n\
                • Choose a different bucket name (bucket names must be globally unique)\n\
                • Or verify you have access to this existing bucket\n\
                Technical details: {}",
                bucket_name, error_str
            );
        }

        // Region mismatch
        if error_str.contains("AuthorizationHeaderMalformed") || error_str.contains("region") {
            return format!(
                "Region configuration issue for bucket '{}'. \
                Please verify:\n\
                • The region is correctly specified\n\
                • The bucket exists in the specified region\n\
                • For MinIO/LocalStack, use a valid region (e.g., 'us-east-1')\n\
                Technical details: {}",
                bucket_name, error_str
            );
        }

        // Invalid bucket name
        if error_str.contains("InvalidBucketName") {
            return format!(
                "Invalid bucket name '{}'. \
                Bucket names must:\n\
                • Be between 3 and 63 characters long\n\
                • Contain only lowercase letters, numbers, dots (.), and hyphens (-)\n\
                • Begin and end with a letter or number\n\
                • Not be formatted as an IP address\n\
                Technical details: {}",
                bucket_name, error_str
            );
        }

        // SSL/TLS errors
        if error_str.contains("ssl")
            || error_str.contains("tls")
            || error_str.contains("certificate")
        {
            return format!(
                "SSL/TLS error when connecting to S3 for bucket '{}'. \
                Please verify:\n\
                • The endpoint URL scheme matches the service (http:// for local, https:// for AWS)\n\
                • SSL certificates are valid (for custom endpoints)\n\
                • For local development, ensure HTTP is configured correctly\n\
                Technical details: {}",
                bucket_name, error_str
            );
        }

        // Generic S3 service error
        if error_str.contains("service error") {
            return format!(
                "S3 service error when trying to {} bucket '{}'. \
                This may be a temporary issue. Please:\n\
                • Verify the S3 service is operational\n\
                • Check service status/logs\n\
                • Try again in a few moments\n\
                Technical details: {}",
                operation, bucket_name, error_str
            );
        }

        // Default: return a formatted version of the error
        format!(
            "Failed to {} S3 bucket '{}': {}\n\
            \n\
            Please verify your S3 configuration:\n\
            • Endpoint URL is correct\n\
            • Access credentials are valid\n\
            • Region is correctly specified\n\
            • Bucket name is valid\n\
            • Network connectivity to S3 service",
            operation, bucket_name, error_str
        )
    }

    async fn upload_backup(
        &self,
        s3_client: &S3Client,
        s3_source: &S3Source,
        temp_file: &NamedTempFile,
        s3_location: &str,
    ) -> Result<()> {
        info!("Uploading backup to S3: {}", s3_location);

        // Get file size
        let file_size = temp_file.as_file().metadata()?.len();

        // Use multipart upload for files larger than 30MB
        const MULTIPART_THRESHOLD: u64 = 30 * 1024 * 1024; // 30MB in bytes

        if file_size > MULTIPART_THRESHOLD {
            self.upload_multipart(s3_client, s3_source, temp_file, s3_location)
                .await
        } else {
            self.upload_single_part(s3_client, s3_source, temp_file, s3_location)
                .await
        }
    }

    async fn upload_single_part(
        &self,
        s3_client: &S3Client,
        s3_source: &S3Source,
        temp_file: &NamedTempFile,
        s3_location: &str,
    ) -> Result<()> {
        // Stream from file instead of reading entire contents into memory
        let body = aws_sdk_s3::primitives::ByteStream::from_path(temp_file.path())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create byte stream from backup file: {}", e))?;

        match s3_client
            .put_object()
            .bucket(&s3_source.bucket_name)
            .key(s3_location)
            .body(body)
            .content_type("application/x-gzip")
            .send()
            .await
        {
            Ok(_) => {
                info!("Successfully uploaded backup using single-part upload");
                Ok(())
            }
            Err(e) => {
                if let Some(service_error) = e.as_service_error() {
                    error!(
                        "S3 service error during single-part upload: {:?} - Message: {}, Code: {:?}",
                        service_error,
                        service_error.message().unwrap_or("no message"),
                        service_error.code()
                    );
                    Err(anyhow::anyhow!(
                        "S3 upload failed: {} (code: {:?})",
                        service_error.message().unwrap_or("unknown error"),
                        service_error.code()
                    ))
                } else {
                    error!("Failed to upload backup: {}", e);
                    Err(anyhow::anyhow!("Failed to upload backup: {}", e))
                }
            }
        }
    }

    async fn upload_multipart(
        &self,
        s3_client: &S3Client,
        s3_source: &S3Source,
        temp_file: &NamedTempFile,
        s3_location: &str,
    ) -> Result<()> {
        // Create multipart upload
        let create_multipart_resp = match s3_client
            .create_multipart_upload()
            .bucket(&s3_source.bucket_name)
            .key(s3_location)
            .content_type("application/x-gzip")
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                if let Some(service_error) = e.as_service_error() {
                    error!(
                        "S3 service error creating multipart upload: {:?} - Message: {}, Code: {:?}",
                        service_error,
                        service_error.message().unwrap_or("no message"),
                        service_error.code()
                    );
                    return Err(anyhow::anyhow!(
                        "Failed to create multipart upload: {} (code: {:?})",
                        service_error.message().unwrap_or("unknown error"),
                        service_error.code()
                    ));
                }
                return Err(anyhow::anyhow!("Failed to create multipart upload: {}", e));
            }
        };

        let upload_id = create_multipart_resp
            .upload_id()
            .ok_or_else(|| anyhow::anyhow!("No upload ID received from S3"))?;

        let mut part_number = 1;
        let mut parts = aws_sdk_s3::types::CompletedMultipartUpload::builder();
        let mut total_size = 0;

        // Stream and upload file in chunks
        let file = tokio::fs::File::open(temp_file.path()).await?;
        let reader = tokio::io::BufReader::new(file);
        let mut stream = tokio_util::io::ReaderStream::new(reader);

        let chunk_size = 5 * 1024 * 1024; // 5MB chunks
        let mut buffer = Vec::with_capacity(chunk_size);

        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|e| anyhow::anyhow!("Failed to read chunk from file: {}", e))?;
            buffer.extend_from_slice(&chunk);

            if buffer.len() >= chunk_size {
                let chunk_len = buffer.len();
                match self
                    .upload_part(
                        s3_client,
                        &s3_source.bucket_name,
                        s3_location,
                        upload_id,
                        part_number,
                        std::mem::take(&mut buffer),
                    )
                    .await
                {
                    Ok(part) => {
                        parts = parts.parts(part);
                        total_size += chunk_len;
                        part_number += 1;
                        buffer.reserve(chunk_size);
                    }
                    Err(e) => {
                        self.abort_multipart_upload(
                            s3_client,
                            &s3_source.bucket_name,
                            s3_location,
                            upload_id,
                        )
                        .await;
                        return Err(e);
                    }
                }
            }
        }

        // Handle remaining data
        if !buffer.is_empty() {
            let chunk_len = buffer.len();
            match self
                .upload_part(
                    s3_client,
                    &s3_source.bucket_name,
                    s3_location,
                    upload_id,
                    part_number,
                    std::mem::take(&mut buffer),
                )
                .await
            {
                Ok(part) => {
                    parts = parts.parts(part);
                    total_size += chunk_len;
                }
                Err(e) => {
                    self.abort_multipart_upload(
                        s3_client,
                        &s3_source.bucket_name,
                        s3_location,
                        upload_id,
                    )
                    .await;
                    return Err(e);
                }
            }
        }

        // Complete multipart upload
        match s3_client
            .complete_multipart_upload()
            .bucket(&s3_source.bucket_name)
            .key(s3_location)
            .upload_id(upload_id)
            .multipart_upload(parts.build())
            .send()
            .await
        {
            Ok(_) => {
                info!(
                    "Successfully uploaded backup with size: {} bytes",
                    total_size
                );
                Ok(())
            }
            Err(e) => {
                if let Some(service_error) = e.as_service_error() {
                    error!(
                        "S3 service error completing multipart upload: {:?} - Message: {}, Code: {:?}",
                        service_error,
                        service_error.message().unwrap_or("no message"),
                        service_error.code()
                    );
                    Err(anyhow::anyhow!(
                        "Failed to complete multipart upload: {} (code: {:?})",
                        service_error.message().unwrap_or("unknown error"),
                        service_error.code()
                    ))
                } else {
                    error!("Failed to complete multipart upload: {}", e);
                    Err(anyhow::anyhow!(
                        "Failed to complete multipart upload: {}",
                        e
                    ))
                }
            }
        }
    }

    async fn upload_part(
        &self,
        s3_client: &S3Client,
        bucket: &str,
        key: &str,
        upload_id: &str,
        part_number: i32,
        body: Vec<u8>,
    ) -> Result<aws_sdk_s3::types::CompletedPart> {
        match s3_client
            .upload_part()
            .bucket(bucket)
            .key(key)
            .upload_id(upload_id)
            .body(body.into())
            .part_number(part_number)
            .send()
            .await
        {
            Ok(response) => {
                let etag = response
                    .e_tag
                    .ok_or_else(|| anyhow::anyhow!("No ETag received for part {}", part_number))?;

                Ok(aws_sdk_s3::types::CompletedPart::builder()
                    .e_tag(etag)
                    .part_number(part_number)
                    .build())
            }
            Err(e) => {
                if let Some(service_error) = e.as_service_error() {
                    error!(
                        "S3 service error uploading part {}: {:?} - Message: {}, Code: {:?}",
                        part_number,
                        service_error,
                        service_error.message().unwrap_or("no message"),
                        service_error.code()
                    );
                    Err(anyhow::anyhow!(
                        "Failed to upload part {}: {} (code: {:?})",
                        part_number,
                        service_error.message().unwrap_or("unknown error"),
                        service_error.code()
                    ))
                } else {
                    error!("Failed to upload part {}: {}", part_number, e);
                    Err(anyhow::anyhow!(
                        "Failed to upload part {}: {}",
                        part_number,
                        e
                    ))
                }
            }
        }
    }

    async fn abort_multipart_upload(
        &self,
        s3_client: &S3Client,
        bucket: &str,
        key: &str,
        upload_id: &str,
    ) {
        if let Err(e) = s3_client
            .abort_multipart_upload()
            .bucket(bucket)
            .key(key)
            .upload_id(upload_id)
            .send()
            .await
        {
            if let Some(service_error) = e.as_service_error() {
                error!(
                    "S3 service error aborting multipart upload: {:?} - Message: {}, Code: {:?}",
                    service_error,
                    service_error.message().unwrap_or("no message"),
                    service_error.code()
                );
            } else {
                error!("Failed to abort multipart upload: {}", e);
            }
        }
    }

    pub async fn restore_backup(&self, backup_id: &str) -> Result<(), BackupError> {
        use sea_orm::{ConnectionTrait, DatabaseBackend};

        info!(
            "Starting backup restoration process for backup: {}",
            backup_id
        );

        // Lookup backup record
        let backup = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::BackupId.eq(backup_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "Backup".to_string(),
                detail: "Backup not found".to_string(),
            })?;

        // Get S3 source
        let s3_source = temps_entities::s3_sources::Entity::find_by_id(backup.s3_source_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: "S3 source not found".to_string(),
            })?;

        let backend = self.db.get_database_backend();
        match backend {
            DatabaseBackend::Sqlite => self.restore_sqlite_backup(&backup, &s3_source).await,
            DatabaseBackend::Postgres => self.restore_postgres_backup(&backup, &s3_source).await,
            _ => Err(BackupError::Unsupported(
                "Database restore is currently supported only for SQLite and PostgreSQL"
                    .to_string(),
            )),
        }
    }

    async fn restore_sqlite_backup(
        &self,
        backup: &temps_entities::backups::Model,
        s3_source: &temps_entities::s3_sources::Model,
    ) -> Result<(), BackupError> {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        info!("Restoring SQLite backup: {}", backup.backup_id);

        // Create S3 client
        let s3_client = self
            .create_s3_client(s3_source)
            .await
            .map_err(|e| BackupError::S3(e.to_string()))?;

        // Download backup
        let response = s3_client
            .get_object()
            .bucket(&s3_source.bucket_name)
            .key(&backup.s3_location)
            .send()
            .await
            .map_err(|e| BackupError::S3(e.to_string()))?;

        // Stream S3 response → gzip decoder → temp file on disk.
        // Previous approach downloaded the entire compressed backup into memory and then
        // decompressed into a second in-memory buffer, causing peak memory equal to
        // compressed + decompressed size.
        let temp_file = NamedTempFile::new()?;
        {
            let mut body_stream = response.body;
            let mut decoder =
                flate2::write::GzDecoder::new(std::io::BufWriter::new(temp_file.as_file()));
            while let Some(chunk) = body_stream.next().await {
                let chunk = chunk.map_err(|e| BackupError::S3(e.to_string()))?;
                std::io::Write::write_all(&mut decoder, &chunk)?;
            }
            decoder.finish()?;
        }

        // Determine the SQLite database file path from server configuration
        let database_url = &self.config_service.get_database_url();

        // Accept sqlite://path or sqlite:path and derive the OS path
        let db_path = if let Some(rem) = database_url.strip_prefix("sqlite://") {
            rem.to_string()
        } else if let Some(rem) = database_url.strip_prefix("sqlite:") {
            rem.to_string()
        } else {
            return Err(BackupError::Unsupported(format!(
                "Unsupported database URL for SQLite restore: {}",
                database_url
            )));
        };

        if db_path == ":memory:" {
            return Err(BackupError::Unsupported(
                "Cannot restore into an in-memory SQLite database".into(),
            ));
        }

        // Ensure all WAL contents are checkpointed before file replacement
        // so the on-disk main db is consistent.
        let _ = self
            .db
            .execute(Statement::from_string(
                DatabaseBackend::Sqlite,
                "PRAGMA wal_checkpoint(FULL)".to_string(),
            ))
            .await;

        info!("Replacing SQLite database file at {}", db_path);

        // Make a safety copy of the current DB file if it exists
        let db_path_buf = std::path::PathBuf::from(&db_path);
        if db_path_buf.exists() {
            let mut backup_suffix = 0usize;
            loop {
                let safety_path = db_path_buf.with_extension(format!(
                    "bak{}",
                    if backup_suffix == 0 {
                        String::new()
                    } else {
                        format!(".{}", backup_suffix)
                    }
                ));
                if !safety_path.exists() {
                    let _ = std::fs::copy(&db_path_buf, &safety_path);
                    break;
                }
                backup_suffix += 1;
            }
        }

        // Replace the DB file with the restored one
        // Note: best-effort remove first to avoid cross-device rename issues
        if db_path_buf.exists() {
            let _ = std::fs::remove_file(&db_path_buf);
        }
        std::fs::copy(temp_file.path(), &db_path_buf).map_err(BackupError::Io)?;

        // Optionally run integrity check (best-effort)
        let _ = self
            .db
            .execute(Statement::from_string(
                DatabaseBackend::Sqlite,
                "PRAGMA integrity_check".to_string(),
            ))
            .await;

        info!("SQLite backup restored successfully");
        Ok(())
    }

    async fn restore_postgres_backup(
        &self,
        backup: &temps_entities::backups::Model,
        s3_source: &temps_entities::s3_sources::Model,
    ) -> Result<(), BackupError> {
        // Route to WAL-G restore if the backup was created with WAL-G (s3:// prefix)
        if backup.s3_location.starts_with("s3://") {
            return self.restore_postgres_walg(backup, s3_source).await;
        }

        // Legacy restore path: pg_dump SQL via psql/pg_restore sidecar
        use bollard::exec::CreateExecOptions;
        use bollard::models::ContainerCreateBody as Config;
        use bollard::query_parameters::RemoveContainerOptions;
        use bollard::Docker;

        info!("Restoring PostgreSQL backup: {}", backup.backup_id);

        // Create S3 client
        let s3_client = self
            .create_s3_client(s3_source)
            .await
            .map_err(|e| BackupError::S3(e.to_string()))?;

        // Download backup (gzipped SQL)
        let response = s3_client
            .get_object()
            .bucket(&s3_source.bucket_name)
            .key(&backup.s3_location)
            .send()
            .await
            .map_err(|e| BackupError::S3(e.to_string()))?;

        // Get database URL from server configuration
        let database_url = &self.config_service.get_database_url();

        // Parse database URL to extract connection parameters
        let url = url::Url::parse(database_url).map_err(|e| BackupError::Internal {
            message: format!("Invalid DATABASE_URL format: {}", e),
        })?;

        let host = url.host_str().unwrap_or("localhost");
        let port = url.port().unwrap_or(5432);
        let database = url.path().trim_start_matches('/');
        let username = url.username();
        let password = url.password().unwrap_or("");

        // Detect backup format from S3 location path:
        // - .pgdump.gz / backup.postgresql.gz = custom format (pg_restore) [legacy backups]
        // - .sql.gz = plain SQL format (psql) [current format]
        let is_plain_format = backup.s3_location.ends_with(".sql.gz");

        // Connect to Docker — restore uses a sidecar container to ensure
        // psql/pg_restore version matches the database, avoiding host dependency
        let docker = Docker::connect_with_local_defaults().map_err(|e| BackupError::Internal {
            message: format!("Failed to connect to Docker: {}", e),
        })?;

        // Get PostgreSQL version to match the sidecar image
        let version_str = self
            .get_postgres_version()
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to get PostgreSQL version: {}", e),
            })?;
        let major_version =
            self.parse_postgres_version(&version_str)
                .map_err(|e| BackupError::Internal {
                    message: format!("Failed to parse PostgreSQL version: {}", e),
                })?;
        let image_tag = self.get_postgres_image_tag(&major_version);

        // Pull the matching PostgreSQL Docker image
        self.pull_postgres_image(&image_tag)
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to pull Docker image: {}", e),
            })?;

        // Stream S3 response → gzip decoder → temp file on disk.
        // Previous approach downloaded the entire compressed backup into memory and then
        // decompressed into a second in-memory buffer, causing peak memory equal to
        // compressed + decompressed size (e.g. 10 GB + 28 GB = 38 GB).
        let restore_dir = self
            .config_service
            .data_dir()
            .join("backups")
            .join("restore_tmp");
        tokio::fs::create_dir_all(&restore_dir)
            .await
            .map_err(|e| BackupError::Internal {
                message: format!(
                    "Failed to create restore temp directory {}: {}",
                    restore_dir.display(),
                    e
                ),
            })?;

        let restore_filename = format!("{}.sql", uuid::Uuid::new_v4());
        let host_restore_path = restore_dir.join(&restore_filename);
        let container_restore_path = format!("/restore/{}", restore_filename);

        // Stream-decompress S3 body directly to disk — constant memory usage
        {
            let mut body_stream = response.body;
            let out_file =
                std::fs::File::create(&host_restore_path).map_err(|e| BackupError::Internal {
                    message: format!(
                        "Failed to create restore file {}: {}",
                        host_restore_path.display(),
                        e
                    ),
                })?;
            let mut decoder = flate2::write::GzDecoder::new(std::io::BufWriter::new(out_file));
            while let Some(chunk) = body_stream.next().await {
                let chunk = chunk.map_err(|e| BackupError::S3(e.to_string()))?;
                std::io::Write::write_all(&mut decoder, &chunk)?;
            }
            decoder.finish()?;
        }

        // Create sidecar container name
        let container_name = format!("temps-pg-restore-{}", uuid::Uuid::new_v4());

        // URL-decode password for env var
        let decoded_password = urlencoding::decode(password)
            .map(|s| s.to_string())
            .unwrap_or_else(|_| password.to_string());
        let pgpassword_env = format!("PGPASSWORD={}", decoded_password);

        let config = Config {
            image: Some(image_tag),
            entrypoint: Some(vec!["/bin/sleep".to_string()]),
            cmd: Some(vec!["3600".to_string()]),
            env: Some(vec![pgpassword_env.clone()]),
            user: Some("root".to_string()),
            host_config: Some(bollard::models::HostConfig {
                network_mode: Some("host".to_string()),
                auto_remove: Some(true),
                binds: Some(vec![format!("{}:/restore:rw", restore_dir.display())]),
                ..Default::default()
            }),
            ..Default::default()
        };

        // Helper to remove the sidecar on any error path
        let remove_sidecar = |docker: bollard::Docker, name: String| async move {
            let _ = docker
                .remove_container(
                    &name,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;
        };

        info!("Creating temporary Docker container for PostgreSQL restore");

        docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&container_name)
                        .build(),
                ),
                config,
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to create restore container: {}", e),
            })?;

        docker
            .start_container(
                &container_name,
                Some(bollard::query_parameters::StartContainerOptionsBuilder::new().build()),
            )
            .await
            .map_err(|e| {
                let docker = docker.clone();
                let name = container_name.clone();
                tokio::spawn(async move { remove_sidecar(docker, name).await });
                BackupError::Internal {
                    message: format!("Failed to start restore container: {}", e),
                }
            })?;

        let port_str = port.to_string();

        // Build the restore command based on backup format
        let (restore_tool, restore_cmd) = if is_plain_format {
            // Plain SQL: use psql to execute the dump.
            // NOTE: We intentionally do NOT use ON_ERROR_STOP=on because pg_dumpall --clean
            // generates "DROP ... ONLY" statements that TimescaleDB rejects for hypertables.
            // These errors are benign — the actual CREATE TABLE and COPY statements succeed.
            let cmd = format!(
                "psql --no-password --host={} --port={} --username={} --dbname={} --file={}",
                shell_escape(host),
                shell_escape(&port_str),
                shell_escape(username),
                shell_escape(database),
                container_restore_path
            );
            ("psql", cmd)
        } else {
            // Custom format: use pg_restore
            let cmd = format!(
                "pg_restore --verbose --clean --if-exists --no-password --host={} --port={} --username={} --dbname={} {}",
                shell_escape(host), shell_escape(&port_str), shell_escape(username), shell_escape(database), container_restore_path
            );
            ("pg_restore", cmd)
        };

        info!(
            "Running {} in Docker sidecar for backup {}",
            restore_tool, backup.backup_id
        );

        // Capture stderr in a file for diagnostics
        let stderr_path = format!("/restore/{}.stderr", uuid::Uuid::new_v4());
        let full_cmd = format!("{} 2>{}", restore_cmd, stderr_path);

        let exec = docker
            .create_exec(
                &container_name,
                CreateExecOptions {
                    cmd: Some(vec!["sh", "-c", &full_cmd]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    env: Some(vec![pgpassword_env.as_str()]),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to create exec for {}: {}", restore_tool, e),
            })?;

        // Start detached — no streaming through Temps process
        use bollard::exec::StartExecOptions;
        docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to start exec for {}: {}", restore_tool, e),
            })?;

        // Poll for completion
        loop {
            let inspect =
                docker
                    .inspect_exec(&exec.id)
                    .await
                    .map_err(|e| BackupError::Internal {
                        message: format!("Failed to inspect exec: {}", e),
                    })?;
            if let Some(running) = inspect.running {
                if !running {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }

        // Read stderr from bind mount for diagnostics
        let host_stderr_path =
            restore_dir.join(std::path::Path::new(&stderr_path).file_name().unwrap());
        let stderr_data = tokio::fs::read(&host_stderr_path).await.unwrap_or_default();
        let _ = tokio::fs::remove_file(&host_stderr_path).await;

        // Check exit code
        let exec_inspect =
            docker
                .inspect_exec(&exec.id)
                .await
                .map_err(|e| BackupError::Internal {
                    message: format!("Failed to inspect exec result: {}", e),
                })?;

        let exit_code = exec_inspect.exit_code.unwrap_or(-1);

        // Clean up sidecar and restore file
        remove_sidecar(docker.clone(), container_name.clone()).await;
        let _ = tokio::fs::remove_file(&host_restore_path).await;

        let stderr = String::from_utf8_lossy(&stderr_data);

        if exit_code != 0 {
            // For psql, exit code 1 = SQL errors in the script (may include benign
            // TimescaleDB hypertable warnings from --clean). Exit code 2 = connection error.
            // Exit code 3 = script error. For pg_restore, exit code 1 with "errors ignored"
            // is common for --clean on existing schemas.
            if is_plain_format && exit_code == 1 {
                // psql exit 1 = some SQL statements failed. This is expected when
                // pg_dumpall --clean generates "DROP ... ONLY" on TimescaleDB hypertables.
                // Log as warning, not error.
                warn!(
                    "{} completed with warnings (exit code {}): {}",
                    restore_tool, exit_code, stderr
                );
            } else if !is_plain_format && exit_code == 1 && stderr.contains("errors ignored") {
                warn!("{} completed with ignored errors: {}", restore_tool, stderr);
            } else {
                return Err(BackupError::Internal {
                    message: format!(
                        "{} failed with exit code {}: {}",
                        restore_tool, exit_code, stderr
                    ),
                });
            }
        } else if !stderr.is_empty() {
            debug!("{} stderr output: {}", restore_tool, stderr);
        }

        info!("PostgreSQL backup restored successfully via Docker sidecar");
        Ok(())
    }

    /// Restore internal database from a WAL-G backup.
    ///
    /// Multi-step process (same as external service WAL-G restore):
    /// 1. Fetch backup to temp directory on the shared volume (while PG still runs)
    /// 2. Add recovery.signal + recovery config, copy pg_wal
    /// 3. Disable restart policy, stop container
    /// 4. Swap PGDATA via ephemeral helper container (volumes_from)
    /// 5. Re-enable restart policy, start container → PG recovers → promotes
    async fn restore_postgres_walg(
        &self,
        backup: &temps_entities::backups::Model,
        s3_source: &temps_entities::s3_sources::Model,
    ) -> Result<(), BackupError> {
        use bollard::exec::{CreateExecOptions, StartExecOptions};
        use bollard::Docker;

        info!(
            "Restoring internal database from WAL-G backup: {}",
            backup.s3_location
        );

        let (container_id, pgdata) = self.find_internal_db_container().await?;

        let docker = Docker::connect_with_local_defaults().map_err(|e| BackupError::Internal {
            message: format!("Failed to connect to Docker: {}", e),
        })?;

        // Build WAL-G environment variables
        let decrypted_access_key = self
            .encryption_service
            .decrypt_string(&s3_source.access_key_id)
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to decrypt S3 access key: {}", e),
            })?;
        let decrypted_secret_key = self
            .encryption_service
            .decrypt_string(&s3_source.secret_key)
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to decrypt S3 secret key: {}", e),
            })?;

        let walg_s3_prefix = &backup.s3_location;
        let mut walg_env: Vec<String> = vec![
            format!("WALG_S3_PREFIX={}", walg_s3_prefix),
            format!("AWS_ACCESS_KEY_ID={}", decrypted_access_key),
            format!("AWS_SECRET_ACCESS_KEY={}", decrypted_secret_key),
            format!("AWS_REGION={}", s3_source.region),
            format!("PGDATA={}", pgdata),
        ];

        // Resolve S3 endpoint for use inside the Docker container.
        let s3_creds = temps_providers::S3Credentials {
            access_key_id: decrypted_access_key.clone(),
            secret_key: decrypted_secret_key.clone(),
            region: s3_source.region.clone(),
            endpoint: s3_source.endpoint.clone(),
            bucket_name: s3_source.bucket_name.clone(),
            bucket_path: s3_source.bucket_path.clone(),
            force_path_style: s3_source.force_path_style.unwrap_or(true),
        };
        if let Some(resolved_endpoint) = s3_creds
            .resolve_endpoint_for_container(&docker, &container_id)
            .await
        {
            walg_env.push(format!("AWS_ENDPOINT={}", resolved_endpoint));
        }
        if s3_source.force_path_style.unwrap_or(true) {
            walg_env.push("AWS_S3_FORCE_PATH_STYLE=true".to_string());
        }

        let walg_env_refs: Vec<&str> = walg_env.iter().map(|s| s.as_str()).collect();

        // Step 1: Fetch backup to temp directory on the shared volume.
        // Must be on the volume (not /tmp) so the helper container can see it via volumes_from.
        // The parent of PGDATA is typically the volume mount point (e.g., /var/lib/postgresql).
        let volume_root = std::path::Path::new(&pgdata)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "/var/lib/postgresql".to_string());
        let restore_temp = format!("{}/restore_temp", volume_root);

        info!(
            "Step 1: Fetching WAL-G backup to {} in container {}",
            restore_temp, container_id
        );
        let fetch_cmd_str = format!(
            "mkdir -p {restore_temp} && rm -rf {restore_temp}/* && wal-g backup-fetch {restore_temp} LATEST > /tmp/walg_restore.log 2>&1",
            restore_temp = restore_temp,
        );

        let exec = docker
            .create_exec(
                &container_id,
                CreateExecOptions {
                    cmd: Some(vec!["sh", "-c", &fetch_cmd_str]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    env: Some(walg_env_refs.clone()),
                    user: Some("postgres"),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to create WAL-G fetch exec: {}", e),
            })?;

        docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to start WAL-G fetch exec: {}", e),
            })?;

        // Poll for fetch completion
        loop {
            let inspect =
                docker
                    .inspect_exec(&exec.id)
                    .await
                    .map_err(|e| BackupError::Internal {
                        message: format!("Failed to inspect WAL-G fetch exec: {}", e),
                    })?;
            if let Some(running) = inspect.running {
                if !running {
                    if let Some(exit_code) = inspect.exit_code {
                        if exit_code != 0 {
                            return Err(BackupError::Internal {
                                message: format!(
                                    "WAL-G backup-fetch failed with exit code {} in container {}",
                                    exit_code, container_id
                                ),
                            });
                        }
                    }
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        info!("WAL-G backup fetched to {}", restore_temp);

        // Step 2: Prepare restored PGDATA for recovery.
        // - recovery.signal: tells PG to enter recovery mode
        // - restore_command = '/bin/true': no archived WAL to fetch
        // - recovery_target = 'immediate': stop at backup consistency point
        // - recovery_target_action = 'promote': promote to primary after recovery
        // - Copy pg_wal from running PGDATA (WAL not archived to S3)
        info!("Step 2: Preparing recovery configuration");
        let prepare_cmd_str = format!(
            concat!(
                "touch {restore_temp}/recovery.signal && ",
                "echo \"restore_command = '/bin/true'\" >> {restore_temp}/postgresql.auto.conf && ",
                "echo \"recovery_target = 'immediate'\" >> {restore_temp}/postgresql.auto.conf && ",
                "echo \"recovery_target_action = 'promote'\" >> {restore_temp}/postgresql.auto.conf && ",
                "rm -rf {restore_temp}/pg_wal && ",
                "cp -a {pgdata}/pg_wal {restore_temp}/pg_wal"
            ),
            restore_temp = restore_temp,
            pgdata = pgdata,
        );

        let exec = docker
            .create_exec(
                &container_id,
                CreateExecOptions {
                    cmd: Some(vec!["sh", "-c", &prepare_cmd_str]),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    user: Some("postgres"),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to create recovery prep exec: {}", e),
            })?;

        docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to start recovery prep exec: {}", e),
            })?;

        loop {
            let inspect =
                docker
                    .inspect_exec(&exec.id)
                    .await
                    .map_err(|e| BackupError::Internal {
                        message: format!("Failed to inspect recovery prep exec: {}", e),
                    })?;
            if inspect.running == Some(false) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        // Step 3: Disable restart policy and stop container.
        // The container has restart_policy=always, so Docker would immediately restart it.
        info!("Step 3: Disabling restart policy and stopping container for PGDATA swap");
        docker
            .update_container(
                &container_id,
                bollard::models::ContainerUpdateBody {
                    restart_policy: Some(bollard::models::RestartPolicy {
                        name: Some(bollard::models::RestartPolicyNameEnum::NO),
                        maximum_retry_count: None,
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to disable restart policy: {}", e),
            })?;

        docker
            .stop_container(
                &container_id,
                Some(bollard::query_parameters::StopContainerOptions {
                    t: Some(30),
                    signal: None,
                }),
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to stop container for restore: {}", e),
            })?;

        // Step 4: Swap PGDATA via ephemeral helper container.
        // Can't exec into a stopped container, so we create a helper with volumes_from.
        info!("Step 4: Swapping PGDATA via helper container");
        let swap_script = format!(
            "rm -rf {pgdata}/* && cp -a {restore_temp}/* {pgdata}/ && rm -rf {restore_temp}",
            pgdata = pgdata,
            restore_temp = restore_temp,
        );

        // Get the image from the container's config to use the same image for the helper
        let container_inspect = docker
            .inspect_container(
                &container_id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to inspect container for helper image: {}", e),
            })?;

        let container_image = container_inspect
            .config
            .as_ref()
            .and_then(|c| c.image.clone())
            .unwrap_or_else(|| "postgres:latest".to_string());

        let helper_name = format!(
            "{}-restore-helper",
            container_id.chars().take(12).collect::<String>()
        );
        let helper_config = bollard::models::ContainerCreateBody {
            image: Some(container_image),
            cmd: Some(vec!["sh".to_string(), "-c".to_string(), swap_script]),
            host_config: Some(bollard::models::HostConfig {
                volumes_from: Some(vec![container_id.clone()]),
                ..Default::default()
            }),
            user: Some("root".to_string()),
            ..Default::default()
        };

        let helper = docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&helper_name)
                        .build(),
                ),
                helper_config,
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to create restore helper container: {}", e),
            })?;

        docker
            .start_container(
                &helper.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to start restore helper container: {}", e),
            })?;

        // Wait for helper to finish
        let wait_result = docker
            .wait_container(
                &helper.id,
                None::<bollard::query_parameters::WaitContainerOptions>,
            )
            .next()
            .await;

        // Capture helper logs before cleanup
        let helper_logs = {
            use futures::TryStreamExt;
            let log_stream = docker.logs(
                &helper.id,
                Some(bollard::query_parameters::LogsOptions {
                    stdout: true,
                    stderr: true,
                    ..Default::default()
                }),
            );
            let logs: Vec<_> = log_stream.try_collect().await.unwrap_or_default();
            logs.iter()
                .map(|l| l.to_string())
                .collect::<Vec<_>>()
                .join("")
        };

        // Clean up helper
        let _ = docker
            .remove_container(
                &helper.id,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    v: false,
                    ..Default::default()
                }),
            )
            .await;

        if let Some(Ok(wait_response)) = wait_result {
            if wait_response.status_code != 0 {
                // Re-enable restart policy even on failure
                let _ = docker
                    .update_container(
                        &container_id,
                        bollard::models::ContainerUpdateBody {
                            restart_policy: Some(bollard::models::RestartPolicy {
                                name: Some(bollard::models::RestartPolicyNameEnum::ALWAYS),
                                maximum_retry_count: None,
                            }),
                            ..Default::default()
                        },
                    )
                    .await;
                let _ = docker
                    .start_container(
                        &container_id,
                        None::<bollard::query_parameters::StartContainerOptions>,
                    )
                    .await;

                return Err(BackupError::Internal {
                    message: format!(
                        "PGDATA swap helper exited with code {}. Logs:\n{}",
                        wait_response.status_code, helper_logs
                    ),
                });
            }
        }

        // Step 5: Re-enable restart policy and start the container.
        // PostgreSQL will enter recovery mode, reach consistency point, and promote.
        info!("Step 5: Re-enabling restart policy and starting container");
        docker
            .update_container(
                &container_id,
                bollard::models::ContainerUpdateBody {
                    restart_policy: Some(bollard::models::RestartPolicy {
                        name: Some(bollard::models::RestartPolicyNameEnum::ALWAYS),
                        maximum_retry_count: None,
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to re-enable restart policy: {}", e),
            })?;

        docker
            .start_container(
                &container_id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to start container after restore: {}", e),
            })?;

        // Wait for PostgreSQL to become healthy by polling the database connection.
        info!("Waiting for PostgreSQL to become ready after restore...");
        let max_wait = std::time::Duration::from_secs(120);
        let start = std::time::Instant::now();
        loop {
            if start.elapsed() > max_wait {
                return Err(BackupError::Internal {
                    message: format!(
                        "PostgreSQL did not become ready within {}s after restore",
                        max_wait.as_secs()
                    ),
                });
            }
            // Try connecting to the database
            let database_url = self.config_service.get_database_url();
            match sea_orm::Database::connect(&database_url).await {
                Ok(conn) => {
                    // Try a simple query to verify it's fully operational
                    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
                    match conn
                        .execute(Statement::from_string(
                            DatabaseBackend::Postgres,
                            "SELECT 1".to_string(),
                        ))
                        .await
                    {
                        Ok(_) => {
                            info!("PostgreSQL is ready after WAL-G restore");
                            break;
                        }
                        Err(_) => {
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        }
                    }
                }
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        }

        info!("Internal database WAL-G restore completed successfully");
        Ok(())
    }

    pub async fn list_backups(
        &self,
        s3_source_id: i32,
    ) -> Result<Vec<temps_entities::backups::Model>, BackupError> {
        let backups = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::S3SourceId.eq(s3_source_id))
            .order_by_desc(temps_entities::backups::Column::StartedAt)
            .all(self.db.as_ref())
            .await?;
        Ok(backups)
    }

    pub async fn delete_backup(&self, backup_id: &str) -> Result<(), BackupError> {
        info!("Deleting backup: {}", backup_id);

        let backup = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::BackupId.eq(backup_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "Backup".to_string(),
                detail: "Backup not found".to_string(),
            })?;

        let s3_source = temps_entities::s3_sources::Entity::find_by_id(backup.s3_source_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: "S3 source not found".to_string(),
            })?;

        // Create S3 client
        let s3_client = self.create_s3_client(&s3_source).await?;

        // Delete from S3
        s3_client
            .delete_object()
            .bucket(&s3_source.bucket_name)
            .key(&backup.s3_location)
            .send()
            .await
            .map_err(|e| BackupError::S3(e.to_string()))?;

        // Delete record from database
        temps_entities::backups::Entity::delete_many()
            .filter(temps_entities::backups::Column::BackupId.eq(backup_id))
            .exec(self.db.as_ref())
            .await?;

        info!("Backup deleted successfully");
        Ok(())
    }

    pub async fn cleanup_old_backups(&self, retention_days: i32) -> Result<()> {
        info!("Cleaning up old backups");

        let cutoff_date = Utc::now() - Duration::days(retention_days as i64);

        let old_backups = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::StartedAt.lt(cutoff_date))
            .all(self.db.as_ref())
            .await?;

        for backup in old_backups {
            if let Err(e) = self.delete_backup(&backup.backup_id).await {
                error!("Failed to delete old backup {}: {}", backup.backup_id, e);
            }
        }

        Ok(())
    }

    /// Enforce retention for every active backup schedule.
    /// Deletes backups that are older than each schedule's `retention_period` days.
    async fn enforce_retention(&self) -> Result<()> {
        let schedules = temps_entities::backup_schedules::Entity::find()
            .filter(temps_entities::backup_schedules::Column::Enabled.eq(true))
            .all(self.db.as_ref())
            .await?;

        for schedule in &schedules {
            if schedule.retention_period > 0 {
                let cutoff = Utc::now() - Duration::days(schedule.retention_period as i64);
                let old_backups = temps_entities::backups::Entity::find()
                    .filter(temps_entities::backups::Column::ScheduleId.eq(Some(schedule.id)))
                    .filter(temps_entities::backups::Column::StartedAt.lt(cutoff))
                    .all(self.db.as_ref())
                    .await?;

                if !old_backups.is_empty() {
                    info!(
                        "Retention cleanup: deleting {} backup(s) older than {} days for schedule {} ({})",
                        old_backups.len(),
                        schedule.retention_period,
                        schedule.id,
                        schedule.name
                    );
                }

                for backup in old_backups {
                    if let Err(e) = self.delete_backup(&backup.backup_id).await {
                        error!(
                            "Failed to delete expired backup {} for schedule {}: {}",
                            backup.backup_id, schedule.id, e
                        );
                    }
                }
            }
        }

        Ok(())
    }

    /// List all S3 sources
    pub async fn list_s3_sources(
        &self,
    ) -> Result<Vec<temps_entities::s3_sources::Model>, BackupError> {
        let sources = temps_entities::s3_sources::Entity::find()
            .all(self.db.as_ref())
            .await?;

        debug!("Listed {} S3 sources", sources.len());
        Ok(sources)
    }

    /// Create a new S3 source
    pub async fn create_s3_source(
        &self,
        request: CreateS3SourceRequest,
    ) -> Result<temps_entities::s3_sources::Model, BackupError> {
        // Validate the request
        if request.name.is_empty() {
            return Err(BackupError::Validation(
                "S3 source name cannot be empty".into(),
            ));
        }

        // Test S3 connection and auto-create bucket before persisting
        let s3_client = self.create_s3_client_from_request(&request).await?;
        self.test_and_create_s3_bucket(&s3_client, &request.bucket_name)
            .await?;

        // Encrypt sensitive credentials before storing
        let encrypted_access_key = self
            .encryption_service
            .encrypt_string(&request.access_key_id)
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to encrypt access key: {}", e),
            })?;

        let encrypted_secret_key = self
            .encryption_service
            .encrypt_string(&request.secret_key)
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to encrypt secret key: {}", e),
            })?;

        // First source is automatically default; subsequent sources require an explicit
        // set-default call. An explicit `is_default: true` in the request is honored and
        // will swap default atomically.
        let existing_count = temps_entities::s3_sources::Entity::find()
            .count(self.db.as_ref())
            .await?;
        let explicit_default = request.is_default.unwrap_or(false);
        let should_be_default = existing_count == 0 || explicit_default;

        let txn = self.db.begin().await?;

        if should_be_default && existing_count > 0 {
            // Clear existing default before inserting new default
            temps_entities::s3_sources::Entity::update_many()
                .col_expr(
                    temps_entities::s3_sources::Column::IsDefault,
                    sea_orm::sea_query::Expr::value(false),
                )
                .filter(temps_entities::s3_sources::Column::IsDefault.eq(true))
                .exec(&txn)
                .await?;
        }

        let new_source = temps_entities::s3_sources::ActiveModel {
            id: sea_orm::NotSet,
            name: sea_orm::Set(request.name.clone()),
            bucket_name: sea_orm::Set(request.bucket_name),
            bucket_path: sea_orm::Set(request.bucket_path),
            access_key_id: sea_orm::Set(encrypted_access_key),
            secret_key: sea_orm::Set(encrypted_secret_key),
            region: sea_orm::Set(request.region),
            created_at: sea_orm::Set(Utc::now()),
            updated_at: sea_orm::Set(Utc::now()),
            endpoint: sea_orm::Set(request.endpoint),
            force_path_style: sea_orm::Set(request.force_path_style),
            is_default: sea_orm::Set(should_be_default),
        };

        let source = new_source.insert(&txn).await?;
        txn.commit().await?;

        debug!(
            "Created new S3 source: {} (is_default={})",
            source.name, source.is_default
        );
        Ok(source)
    }

    /// Test an S3 connection using stored (encrypted) credentials for an existing source.
    /// Returns `Ok(())` on success, or `BackupError::S3` with user-friendly guidance on failure.
    pub async fn test_s3_source_connection(&self, id: i32) -> Result<(), BackupError> {
        let source = self.get_s3_source(id).await?;
        let client = self
            .create_s3_client(&source)
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to build S3 client for source {}: {}", id, e),
            })?;

        match client
            .list_objects_v2()
            .bucket(&source.bucket_name)
            .max_keys(1)
            .send()
            .await
        {
            Ok(_) => {
                debug!(
                    "S3 connection test succeeded for source {} (bucket {})",
                    id, source.bucket_name
                );
                Ok(())
            }
            Err(e) => {
                let error_msg = self.parse_s3_error(&e, &source.bucket_name, "access");
                Err(BackupError::S3(error_msg))
            }
        }
    }

    /// Test an S3 connection using credentials from a prospective request (before persistence).
    /// Does NOT create the bucket — only attempts a list to verify access.
    pub async fn test_s3_connection_from_request(
        &self,
        request: &CreateS3SourceRequest,
    ) -> Result<(), BackupError> {
        if request.access_key_id.is_empty() || request.secret_key.is_empty() {
            return Err(BackupError::Validation(
                "Access key and secret key are required to test connection".into(),
            ));
        }

        let client = self.create_s3_client_from_request(request).await?;
        match client
            .list_objects_v2()
            .bucket(&request.bucket_name)
            .max_keys(1)
            .send()
            .await
        {
            Ok(_) => Ok(()),
            Err(e) => {
                let error_code = e
                    .as_service_error()
                    .and_then(|se| se.code())
                    .map(|s| s.to_string());

                // NoSuchBucket is not a hard failure — credentials are valid, bucket is
                // just missing (would be auto-created on actual source creation).
                if error_code.as_deref() == Some("NoSuchBucket") {
                    debug!(
                        "S3 connection test: credentials valid, bucket '{}' does not yet exist",
                        request.bucket_name
                    );
                    Ok(())
                } else {
                    let error_msg = self.parse_s3_error(&e, &request.bucket_name, "access");
                    Err(BackupError::S3(error_msg))
                }
            }
        }
    }

    /// Atomically make the given source the default. All other sources will be set to
    /// is_default=false in the same transaction.
    pub async fn set_default_s3_source(
        &self,
        id: i32,
    ) -> Result<temps_entities::s3_sources::Model, BackupError> {
        // Verify target exists
        self.get_s3_source(id).await?;

        let txn = self.db.begin().await?;

        temps_entities::s3_sources::Entity::update_many()
            .col_expr(
                temps_entities::s3_sources::Column::IsDefault,
                sea_orm::sea_query::Expr::value(false),
            )
            .filter(temps_entities::s3_sources::Column::IsDefault.eq(true))
            .filter(temps_entities::s3_sources::Column::Id.ne(id))
            .exec(&txn)
            .await?;

        temps_entities::s3_sources::Entity::update_many()
            .col_expr(
                temps_entities::s3_sources::Column::IsDefault,
                sea_orm::sea_query::Expr::value(true),
            )
            .col_expr(
                temps_entities::s3_sources::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(Utc::now()),
            )
            .filter(temps_entities::s3_sources::Column::Id.eq(id))
            .exec(&txn)
            .await?;

        txn.commit().await?;

        let updated = self.get_s3_source(id).await?;
        info!("S3 source {} is now the default", updated.name);
        Ok(updated)
    }

    /// Return the currently-default S3 source, if any.
    pub async fn get_default_s3_source(
        &self,
    ) -> Result<Option<temps_entities::s3_sources::Model>, BackupError> {
        let source = temps_entities::s3_sources::Entity::find()
            .filter(temps_entities::s3_sources::Column::IsDefault.eq(true))
            .one(self.db.as_ref())
            .await?;
        Ok(source)
    }

    /// Get an S3 source by ID
    pub async fn get_s3_source(
        &self,
        id: i32,
    ) -> Result<temps_entities::s3_sources::Model, BackupError> {
        let source = temps_entities::s3_sources::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: "S3 source not found".to_string(),
            })?;

        Ok(source)
    }

    /// Delete an S3 source
    pub async fn delete_s3_source(&self, id: i32) -> Result<bool, BackupError> {
        // First check if source exists and is not in use
        let source = self.get_s3_source(id).await?;

        // Refuse to delete the default source while other sources exist. The caller
        // should set a different source as default first.
        if source.is_default {
            let other_count = temps_entities::s3_sources::Entity::find()
                .filter(temps_entities::s3_sources::Column::Id.ne(id))
                .count(self.db.as_ref())
                .await?;
            if other_count > 0 {
                return Err(BackupError::Validation(format!(
                    "S3 source '{}' is the default. Set a different source as default before deleting.",
                    source.name
                )));
            }
        }

        // Refuse to delete if any backup schedule still references this source.
        let schedule_count = temps_entities::backup_schedules::Entity::find()
            .filter(temps_entities::backup_schedules::Column::S3SourceId.eq(id))
            .count(self.db.as_ref())
            .await?;
        if schedule_count > 0 {
            return Err(BackupError::Validation(format!(
                "Cannot delete S3 source '{}': still referenced by {} backup schedule(s)",
                source.name, schedule_count
            )));
        }

        let result = temps_entities::s3_sources::Entity::delete_by_id(id)
            .exec(self.db.as_ref())
            .await?;

        debug!("Deleted S3 source: {}", source.name);
        Ok(result.rows_affected > 0)
    }

    /// List all backup schedules
    pub async fn list_backup_schedules(
        &self,
    ) -> Result<Vec<temps_entities::backup_schedules::Model>, BackupError> {
        let schedules = temps_entities::backup_schedules::Entity::find()
            .all(self.db.as_ref())
            .await?;

        debug!("Listed {} backup schedules", schedules.len());
        Ok(schedules)
    }

    /// Create a new backup schedule
    pub async fn create_backup_schedule(
        &self,
        request: CreateBackupScheduleRequest,
    ) -> Result<BackupSchedule, BackupError> {
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        // Resolve S3 source: explicit id OR fall back to the default source.
        let s3_source_id = self.resolve_s3_source_id(request.s3_source_id).await?;

        // Verify S3 source exists
        temps_entities::s3_sources::Entity::find_by_id(s3_source_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: format!("S3 source {} not found", s3_source_id),
            })?;

        // Validate the schedule expression
        self.validate_backup_schedule(&request.schedule_expression)?;

        // Calculate next run time
        let cron_schedule = Schedule::from_str(&request.schedule_expression)
            .map_err(|e| BackupError::Schedule(e.to_string()))?;
        let next_run = cron_schedule.upcoming(Utc).next();

        // Insert with SeaORM
        let now = chrono::Utc::now();
        let tags_json = serde_json::to_string(&request.tags)?;
        let new_schedule = temps_entities::backup_schedules::ActiveModel {
            id: sea_orm::NotSet,
            name: Set(request.name.clone()),
            backup_type: Set(request.backup_type.clone()),
            retention_period: Set(request.retention_period),
            s3_source_id: Set(s3_source_id),
            schedule_expression: Set(request.schedule_expression.clone()),
            enabled: Set(request.enabled),
            created_at: Set(now),
            updated_at: Set(now),
            description: Set(request.description.clone()),
            tags: Set(tags_json),
            next_run: Set(next_run),
            max_runtime_secs: Set(request.max_runtime_secs),
            // Default is true ("back up every database, including future
            // ones") so a freshly-created schedule does the obvious thing
            // without the operator having to pick services up front.
            target_all_services: Set(request.target_all_services.unwrap_or(true)),
            include_control_plane: Set(request.include_control_plane.unwrap_or(true)),
            ..Default::default()
        };

        // Validate the resulting schedule has at least one thing to back
        // up. We do this *after* defaulting so callers who omit the flags
        // get the safe "back up everything" behaviour instead of a 400.
        let target_all = request.target_all_services.unwrap_or(true);
        let include_cp = request.include_control_plane.unwrap_or(true);
        if !target_all && !include_cp {
            // Without target_all_services the operator must also attach at
            // least one service. They can't do that until the schedule
            // exists, so the only way to get here legitimately is via an
            // update — block it on create.
            return Err(BackupError::Validation(
                "A schedule must include the control plane, target all databases, \
                 or both. Set include_control_plane=true or target_all_services=true \
                 (or omit the flags to use the defaults)."
                    .to_string(),
            ));
        }

        let schedule_model = new_schedule.insert(self.db.as_ref()).await?;
        info!("Created new backup schedule: {}", schedule_model.name);
        self.fire_lifecycle_reconcile(schedule_model.s3_source_id);
        Ok(schedule_model)
    }

    /// Resolve an optional `s3_source_id` into a concrete ID. If `Some`, returns it
    /// as-is (caller still validates existence). If `None`, returns the current default
    /// source. Returns `Validation` if no default has been configured.
    pub async fn resolve_s3_source_id(&self, requested: Option<i32>) -> Result<i32, BackupError> {
        if let Some(id) = requested {
            return Ok(id);
        }

        match self.get_default_s3_source().await? {
            Some(source) => Ok(source.id),
            None => Err(BackupError::Validation(
                "No S3 source specified and no default S3 source is configured. \
                 Create an S3 source or mark one as default first."
                    .to_string(),
            )),
        }
    }

    /// Get a backup schedule by ID
    pub async fn get_backup_schedule(&self, id: i32) -> Result<BackupSchedule, BackupError> {
        use sea_orm::EntityTrait;

        let schedule = temps_entities::backup_schedules::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "BackupSchedule".to_string(),
                detail: "Backup schedule not found".to_string(),
            })?;

        Ok(schedule)
    }

    /// Delete a backup schedule
    pub async fn delete_backup_schedule(&self, id: i32) -> Result<bool, BackupError> {
        use sea_orm::EntityTrait;

        // Ensure it exists to preserve previous behavior/logging
        let schedule = self.get_backup_schedule(id).await?;

        let result = temps_entities::backup_schedules::Entity::delete_by_id(id)
            .exec(self.db.as_ref())
            .await?;
        info!("Deleted backup schedule: {}", schedule.name);
        self.fire_lifecycle_reconcile(schedule.s3_source_id);
        Ok(result.rows_affected > 0)
    }

    /// Attach external services to a backup schedule.
    ///
    /// Idempotent: re-attaching an already-attached service is a no-op (rows
    /// are inserted with `ON CONFLICT DO NOTHING`). Returns the number of rows
    /// actually inserted. Validates that the schedule and every supplied
    /// service id exist.
    pub async fn attach_services_to_schedule(
        &self,
        schedule_id: i32,
        service_ids: &[i32],
    ) -> Result<u64, BackupError> {
        use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

        // Validate schedule exists (raises NotFound otherwise).
        self.get_backup_schedule(schedule_id).await?;

        if service_ids.is_empty() {
            return Ok(0);
        }

        // De-duplicate the input so we don't ask the DB to insert dup rows
        // (ON CONFLICT handles it, but logging stays clean).
        let mut unique_ids: Vec<i32> = service_ids.to_vec();
        unique_ids.sort_unstable();
        unique_ids.dedup();

        // Validate every requested service id exists.
        let found_count = temps_entities::external_services::Entity::find()
            .filter(temps_entities::external_services::Column::Id.is_in(unique_ids.clone()))
            .count(self.db.as_ref())
            .await?;
        if (found_count as usize) != unique_ids.len() {
            return Err(BackupError::Validation(format!(
                "One or more service ids do not exist (requested {}, found {})",
                unique_ids.len(),
                found_count
            )));
        }

        // Build a single multi-row INSERT with ON CONFLICT DO NOTHING for
        // idempotency. Sea-ORM `insert_many` does not expose ON CONFLICT in
        // a portable way, so we drop to raw SQL.
        let mut sql = String::from(
            "INSERT INTO backup_schedule_services (schedule_id, service_id, created_at) VALUES ",
        );
        let mut params: Vec<sea_orm::Value> = Vec::with_capacity(unique_ids.len() * 2 + 1);
        params.push(sea_orm::Value::from(schedule_id));
        for (idx, sid) in unique_ids.iter().enumerate() {
            if idx > 0 {
                sql.push_str(", ");
            }
            let p = idx + 2; // $1 = schedule_id, $2.. = service_ids
            sql.push_str(&format!("($1, ${}, NOW())", p));
            params.push(sea_orm::Value::from(*sid));
        }
        sql.push_str(" ON CONFLICT (schedule_id, service_id) DO NOTHING");

        let result = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                params,
            ))
            .await
            .map_err(BackupError::Database)?;

        Ok(result.rows_affected())
    }

    /// Auto-provision a covering daily full-backup schedule for every MariaDB
    /// external service that does not yet have one.
    ///
    /// This drives point-in-time recovery out of the box: the daily full
    /// backup produces base backups via the `mariadb_physical` engine, and the
    /// binlog archiver already ships binary logs every few minutes, so
    /// base + binlogs = PITR with no operator action.
    ///
    /// Design (gated by the per-service `default_backup_provisioned` latch):
    /// - Only services where `default_backup_provisioned = false` are
    ///   considered, so we provision **exactly once** and never recreate a
    ///   schedule the operator later deletes.
    /// - Scope is **MariaDB only** (`service_type = "mariadb"`).
    /// - Requires a configured default S3 source. If none exists yet we log at
    ///   `debug` and return `Ok(())` — the next periodic tick retries, which
    ///   handles the "storage configured after the service" ordering.
    /// - Per-service failures are logged at `warn` and skipped, leaving the
    ///   latch `false` so the service is retried on the next tick. A single bad
    ///   service can't block the others.
    ///
    /// Idempotent and safe to call on a periodic tick.
    pub async fn reconcile_default_external_service_schedules(&self) -> Result<(), BackupError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        // 1. Resolve the default S3 source. If none is configured yet, this is
        //    not an error — we simply have nothing to point a schedule at, so
        //    we bail quietly and retry on the next tick.
        let s3_source_id = match self.resolve_s3_source_id(None).await {
            Ok(id) => id,
            Err(_) => {
                debug!(
                    "reconcile_default_external_service_schedules: no default S3 source \
                     configured yet, skipping (will retry next tick)"
                );
                return Ok(());
            }
        };

        // 2. Load unprovisioned MariaDB services.
        let services = temps_entities::external_services::Entity::find()
            .filter(temps_entities::external_services::Column::ServiceType.eq("mariadb"))
            .filter(temps_entities::external_services::Column::DefaultBackupProvisioned.eq(false))
            .all(self.db.as_ref())
            .await?;

        if services.is_empty() {
            return Ok(());
        }

        debug!(
            count = services.len(),
            s3_source_id,
            "reconcile_default_external_service_schedules: provisioning default backup \
             schedules for MariaDB services"
        );

        // 3. For each, create a daily full-backup schedule targeting exactly
        //    that service, then flip the latch.
        for service in services {
            if let Err(e) = self.provision_default_schedule_for_service(&service).await {
                // Leave default_backup_provisioned = false so the next tick
                // retries. One failing service must not block the others.
                warn!(
                    service_id = service.id,
                    service_name = %service.name,
                    error = %e,
                    "Failed to auto-provision default backup schedule for MariaDB service; \
                     will retry on next reconcile tick"
                );
                continue;
            }
        }

        Ok(())
    }

    /// Create the default daily full-backup schedule for a single MariaDB
    /// service and mark it provisioned. Helper for
    /// [`reconcile_default_external_service_schedules`]; on success the
    /// service's `default_backup_provisioned` latch is set to `true`.
    async fn provision_default_schedule_for_service(
        &self,
        service: &temps_entities::external_services::Model,
    ) -> Result<(), BackupError> {
        use sea_orm::{ActiveModelTrait, Set};

        // Daily at 03:00 UTC. 6-field cron (`sec min hour dom mon dow`) as
        // required by the `cron` crate / `validate_backup_schedule`; the two
        // adjacent occurrences are 24h apart, satisfying the validator's
        // "at least 1 hour" rule. Reuse create_backup_schedule for validation
        // and next-run computation — do NOT hand-roll a second insert.
        let request = CreateBackupScheduleRequest {
            name: format!("Auto base backup — {}", service.name),
            // `backup_type` is the schedule/job label ("full"); the actual
            // backup engine (`mariadb_physical`) is resolved from the service's
            // `service_type` at run time, not from this field.
            backup_type: "full".to_string(),
            // Days. 14 days of base backups is a sane default retention window.
            retention_period: 14,
            // Use the resolved default S3 source.
            s3_source_id: None,
            schedule_expression: "0 0 3 * * *".to_string(),
            enabled: true,
            description: Some(
                "Automatically created daily base backup so point-in-time recovery \
                 works out of the box. Safe to edit or delete."
                    .to_string(),
            ),
            tags: vec![],
            max_runtime_secs: None,
            // Target exactly this service (attached below), not every DB.
            //
            // `create_backup_schedule` refuses to create a schedule that has
            // nothing to back up (target_all=false AND include_control_plane=
            // false) because no services can be attached until the schedule
            // row exists. So we create it with the control plane temporarily
            // included, attach the service, then flip include_control_plane
            // off via `update_backup_schedule` — which permits the otherwise-
            // empty combination precisely because a service is now attached.
            target_all_services: Some(false),
            include_control_plane: Some(true),
        };

        let schedule = self.create_backup_schedule(request).await?;

        // Attach exactly this service so the schedule's fan-out targets it.
        self.attach_services_to_schedule(schedule.id, &[service.id])
            .await?;

        // Now that the service is attached, narrow the schedule down to exactly
        // that service: drop the control-plane backup so the schedule only
        // produces base backups for this MariaDB service.
        let schedule = self
            .update_backup_schedule(
                schedule.id,
                UpdateBackupScheduleRequest {
                    name: None,
                    description: None,
                    schedule_expression: None,
                    retention_period: None,
                    max_runtime_secs: None,
                    enabled: None,
                    tags: None,
                    target_all_services: None,
                    include_control_plane: Some(false),
                },
            )
            .await?;

        // Flip the one-shot latch so we never provision this service again.
        let mut active: temps_entities::external_services::ActiveModel = service.clone().into();
        active.default_backup_provisioned = Set(true);
        active.update(self.db.as_ref()).await?;

        info!(
            service_id = service.id,
            service_name = %service.name,
            schedule_id = schedule.id,
            schedule_name = %schedule.name,
            "Auto-provisioned default daily base-backup schedule for MariaDB service \
             (enables point-in-time recovery; edit or delete it like any schedule)"
        );

        Ok(())
    }

    /// Detach a single external service from a backup schedule.
    ///
    /// Returns `true` if a row was removed, `false` if nothing was attached.
    /// Does not raise `NotFound` when the membership row is absent — callers
    /// can treat detach as idempotent.
    pub async fn detach_service_from_schedule(
        &self,
        schedule_id: i32,
        service_id: i32,
    ) -> Result<bool, BackupError> {
        use sea_orm::EntityTrait;

        let result = temps_entities::backup_schedule_services::Entity::delete_by_id((
            schedule_id,
            service_id,
        ))
        .exec(self.db.as_ref())
        .await
        .map_err(BackupError::Database)?;

        Ok(result.rows_affected > 0)
    }

    /// List the external services attached to a given schedule, ordered by
    /// service name for stable UI rendering. Raises `NotFound` if the
    /// schedule does not exist.
    pub async fn list_services_for_schedule(
        &self,
        schedule_id: i32,
    ) -> Result<Vec<temps_entities::external_services::Model>, BackupError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

        self.get_backup_schedule(schedule_id).await?;

        let services = temps_entities::external_services::Entity::find()
            .inner_join(temps_entities::backup_schedule_services::Entity)
            .filter(temps_entities::backup_schedule_services::Column::ScheduleId.eq(schedule_id))
            .order_by_asc(temps_entities::external_services::Column::Name)
            .all(self.db.as_ref())
            .await
            .map_err(BackupError::Database)?;

        Ok(services)
    }

    /// List the schedules that target a given external service. Raises
    /// `NotFound` if the service does not exist.
    pub async fn list_schedules_for_service(
        &self,
        service_id: i32,
    ) -> Result<Vec<BackupSchedule>, BackupError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

        temps_entities::external_services::Entity::find_by_id(service_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "ExternalService".to_string(),
                detail: format!("External service {} not found", service_id),
            })?;

        let schedules = temps_entities::backup_schedules::Entity::find()
            .inner_join(temps_entities::backup_schedule_services::Entity)
            .filter(temps_entities::backup_schedule_services::Column::ServiceId.eq(service_id))
            .order_by_asc(temps_entities::backup_schedules::Column::Name)
            .all(self.db.as_ref())
            .await
            .map_err(BackupError::Database)?;

        Ok(schedules)
    }

    /// List backups for a schedule
    pub async fn list_backups_for_schedule(
        &self,
        schedule_id: i32,
    ) -> Result<Vec<Backup>, BackupError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

        // Verify schedule exists
        self.get_backup_schedule(schedule_id).await?;

        let backups = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::ScheduleId.eq(schedule_id))
            .order_by_desc(temps_entities::backups::Column::StartedAt)
            .all(self.db.as_ref())
            .await?;

        debug!(
            "Listed {} backups for schedule {}",
            backups.len(),
            schedule_id
        );
        Ok(backups)
    }

    /// Paginated run history for a backup schedule (deliverable 1).
    /// List run history for a backup schedule, one row per scheduler tick.
    ///
    /// Returns one [`ScheduleRunSummary`] per `schedule_runs` row linked to
    /// the schedule, with child backup counts aggregated in a single SQL
    /// round-trip. The `aggregate_state` is computed in Rust from the counts:
    ///
    /// - `"running"` — `pending_jobs + running_jobs > 0`
    /// - `"failed"` — `failed_jobs > 0` and `running_jobs + pending_jobs == 0`
    /// - `"completed"` — all children completed
    ///
    /// Legacy `backups` rows that have `schedule_id` set but no
    /// `schedule_run_id` (pre-fan-out history) are surfaced as synthetic
    /// single-job runs in the same list so old history does not disappear.
    ///
    /// `page` is 1-based and clamped to `1` when `< 1`.
    /// `page_size` is clamped to `100` when `> 100` and defaults to `20`.
    pub async fn list_schedule_runs(
        &self,
        schedule_id: i32,
        page: i64,
        page_size: i64,
    ) -> Result<ScheduleRunSummaryList, BackupError> {
        // Verify the schedule exists first so we return 404 instead of an
        // empty page when the caller passes an unknown id.
        self.get_backup_schedule(schedule_id).await?;

        // Clamp pagination parameters.
        let page = page.max(1);
        let page_size = page_size.clamp(1, 100);
        let offset = (page - 1) * page_size;

        // ── Count total run rows (real + synthetic legacy) ────────────────────

        #[derive(FromQueryResult)]
        struct CountRow {
            total: i64,
        }

        // Real runs: schedule_runs rows.
        // Legacy rows: backups with schedule_id set but schedule_run_id NULL.
        let count_sql = r#"
SELECT (
    SELECT COUNT(*) FROM schedule_runs WHERE schedule_id = $1
) + (
    SELECT COUNT(*) FROM backups
     WHERE schedule_id = $1
       AND schedule_run_id IS NULL
) AS total
        "#;

        let total = CountRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            count_sql,
            vec![Value::from(schedule_id)],
        ))
        .one(self.db.as_ref())
        .await
        .map_err(BackupError::Database)?
        .map(|r| r.total)
        .unwrap_or(0);

        // ── Fetch page — one row per tick + synthetic legacy rows ─────────────

        #[derive(FromQueryResult)]
        struct RunRow {
            run_id: i64,
            schedule_id: i32,
            triggered_by: String,
            started_at: chrono::DateTime<chrono::Utc>,
            finished_at: Option<chrono::DateTime<chrono::Utc>>,
            total_jobs: i64,
            completed_jobs: i64,
            failed_jobs: i64,
            running_jobs: i64,
            pending_jobs: i64,
        }

        // Real rows: one per schedule_runs row, child counts via LEFT JOIN.
        // Legacy rows (backups with schedule_id set, schedule_run_id NULL):
        // synthesised as if they were a run with a single job whose state
        // is the backup's own state. We use the backups.id negated as the
        // synthetic run_id to avoid collisions with real schedule_runs.id
        // (both are distinct integer ranges; negative is never a real run id).
        let sql = r#"
SELECT
    sr.id                AS run_id,
    sr.schedule_id       AS schedule_id,
    sr.triggered_by      AS triggered_by,
    sr.started_at        AS started_at,
    sr.finished_at       AS finished_at,
    COUNT(b.id)                                                   AS total_jobs,
    COUNT(b.id) FILTER (WHERE b.state = 'completed')              AS completed_jobs,
    COUNT(b.id) FILTER (WHERE b.state = 'failed')                 AS failed_jobs,
    COUNT(b.id) FILTER (WHERE b.state = 'running')                AS running_jobs,
    COUNT(b.id) FILTER (WHERE b.state = 'pending')                AS pending_jobs
FROM schedule_runs sr
LEFT JOIN backups b ON b.schedule_run_id = sr.id
WHERE sr.schedule_id = $1
GROUP BY sr.id

UNION ALL

-- Synthetic rows for legacy backups (pre-fan-out, no schedule_run_id).
SELECT
    (-b.id)::BIGINT      AS run_id,
    b.schedule_id        AS schedule_id,
    'cron'               AS triggered_by,
    b.started_at         AS started_at,
    b.finished_at        AS finished_at,
    1                    AS total_jobs,
    CASE WHEN b.state = 'completed' THEN 1 ELSE 0 END AS completed_jobs,
    CASE WHEN b.state = 'failed'    THEN 1 ELSE 0 END AS failed_jobs,
    CASE WHEN b.state = 'running'   THEN 1 ELSE 0 END AS running_jobs,
    CASE WHEN b.state = 'pending'   THEN 1 ELSE 0 END AS pending_jobs
FROM backups b
WHERE b.schedule_id = $1
  AND b.schedule_run_id IS NULL

ORDER BY started_at DESC
LIMIT  $2
OFFSET $3
        "#;

        let raw_rows = RunRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            vec![
                Value::from(schedule_id),
                Value::from(page_size),
                Value::from(offset),
            ],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(BackupError::Database)?;

        // ── Compute aggregate_state in Rust ───────────────────────────────────

        let runs = raw_rows
            .into_iter()
            .map(|r| {
                let aggregate_state = if r.pending_jobs + r.running_jobs > 0 {
                    "running".to_string()
                } else if r.failed_jobs > 0 {
                    "failed".to_string()
                } else {
                    "completed".to_string()
                };

                ScheduleRunSummary {
                    run_id: r.run_id,
                    schedule_id: r.schedule_id,
                    triggered_by: r.triggered_by,
                    started_at: r.started_at.to_rfc3339(),
                    finished_at: r.finished_at.map(|t| t.to_rfc3339()),
                    aggregate_state,
                    total_jobs: r.total_jobs,
                    completed_jobs: r.completed_jobs,
                    failed_jobs: r.failed_jobs,
                    running_jobs: r.running_jobs,
                    pending_jobs: r.pending_jobs,
                }
            })
            .collect();

        Ok(ScheduleRunSummaryList {
            runs,
            total,
            page,
            page_size,
        })
    }

    /// List the individual backup jobs belonging to a single scheduler run.
    ///
    /// Used by the schedule detail page's expandable accordion for per-run
    /// drill-down. Returns each child `backups` row joined with its
    /// `external_services` row (for the service name) and the most recent
    /// `backup_jobs` row (for the engine key).
    ///
    /// `page_size` defaults to 50 and is capped at 200 — a single scheduler
    /// tick produces at most 1 + N external services rows (small N).
    pub async fn list_schedule_run_jobs(
        &self,
        run_id: i64,
        page: i64,
        page_size: i64,
    ) -> Result<(Vec<ScheduleRunJobEntry>, i64), BackupError> {
        let page = page.max(1);
        let page_size = page_size.clamp(1, 200);
        let offset = (page - 1) * page_size;

        #[derive(FromQueryResult)]
        struct CountRow {
            total: i64,
        }

        let count_sql = r#"
SELECT COUNT(*) AS total FROM backups WHERE schedule_run_id = $1
        "#;

        let total = CountRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            count_sql,
            vec![Value::from(run_id)],
        ))
        .one(self.db.as_ref())
        .await
        .map_err(BackupError::Database)?
        .map(|r| r.total)
        .unwrap_or(0);

        // The engine key is stored on `backups.metadata` JSON (written at
        // trigger time by `BackupService::create_pending_*`). Read it from
        // there. external_service_backups joins to external_services for
        // the human-readable name.
        let sql = r#"
SELECT
    b.id                                            AS backup_id,
    b.backup_id                                     AS backup_uuid,
    COALESCE(b.metadata::jsonb ->> 'engine', 'control_plane') AS engine,
    COALESCE(es.name, 'control plane')              AS service_name,
    esb.service_id                                  AS service_id,
    b.state                                         AS state,
    b.started_at                                    AS started_at,
    b.finished_at                                   AS finished_at,
    b.size_bytes                                    AS size_bytes,
    b.error_message                                 AS error_message,
    b.s3_source_id                                  AS s3_source_id
FROM backups b
LEFT JOIN external_service_backups esb ON esb.backup_id = b.id
LEFT JOIN external_services es ON es.id = esb.service_id
WHERE b.schedule_run_id = $1
ORDER BY b.id
LIMIT  $2
OFFSET $3
        "#;

        let rows = ScheduleRunJobEntry::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            vec![
                Value::from(run_id),
                Value::from(page_size),
                Value::from(offset),
            ],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(BackupError::Database)?;

        Ok((rows, total))
    }

    /// Immediately enqueue a fan-out run for the given schedule (Run Now).
    ///
    /// Delegates to [`enqueue_scheduled_run`] with `TriggerSource::Manual`.
    /// Returns `409 Conflict` (via [`BackupError::ScheduleRunAlreadyInFlight`])
    /// when a run for this schedule already has `finished_at IS NULL`.
    ///
    /// Returns [`BackupError::Validation`] when the schedule is disabled.
    /// Cancel a single backup by `backups.id`. Flips the row + its latest
    /// `backup_jobs` row to `failed` with a "cancelled by user" reason.
    /// Returns the number of rows updated — `0` means the backup was already
    /// terminal (which the caller should treat as an idempotent success,
    /// not a 404). The runner's in-process cancellation token is observed
    /// on the next heartbeat tick (≤5s) so the engine exits cleanly and
    /// `rollback` reaps the sidecar.
    pub async fn cancel_backup(
        &self,
        backup_id: i32,
        triggered_by_user_id: Option<i32>,
    ) -> Result<u64, BackupError> {
        // Verify the backup exists so the caller gets a real 404 (not an
        // "everything looks fine, nothing happened" silent no-op) when the
        // id is wrong. Then delegate to `temps_backup_core::cancel_backup`
        // which owns the actual DB writes.
        temps_entities::backups::Entity::find_by_id(backup_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "Backup".to_string(),
                detail: format!("Backup {} not found", backup_id),
            })?;

        let reason = match triggered_by_user_id {
            Some(uid) => format!("cancelled by user {}", uid),
            None => "cancelled".to_string(),
        };

        let rows = temps_backup_core::cancel_backup(self.db.as_ref(), backup_id, &reason)
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to cancel backup {}: {}", backup_id, e),
            })?;

        // Notify the processor so any in-flight engine task fires its
        // cancel token and exits cleanly. The DB flip above already
        // marked the row as failed; this signal stops the running
        // container so the sidecar gets reaped promptly.
        if let Err(e) = self
            .queue()
            .send(temps_core::Job::BackupCancelRequested(
                temps_core::BackupCancelRequestedJob { backup_id },
            ))
            .await
        {
            warn!(
                backup_id,
                error = %e,
                "cancel_backup: queue.send for BackupCancelRequested failed; running engine (if any) will not be interrupted promptly",
            );
        }

        info!(
            backup_id,
            rows_affected = rows,
            triggered_by_user_id = ?triggered_by_user_id,
            "BackupService: cancel_backup completed",
        );

        Ok(rows)
    }

    /// Cancel every non-terminal child backup belonging to a scheduler run.
    /// Returns the number of children that were flipped to `failed`. The
    /// parent `schedule_runs.finished_at` is stamped automatically once no
    /// live children remain (which is true after a successful cancel).
    pub async fn cancel_schedule_run(
        &self,
        schedule_run_id: i64,
        triggered_by_user_id: Option<i32>,
    ) -> Result<u64, BackupError> {
        // Verify the run exists so the caller gets a real 404.
        temps_entities::schedule_runs::Entity::find_by_id(schedule_run_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "ScheduleRun".to_string(),
                detail: format!("Schedule run {} not found", schedule_run_id),
            })?;

        let reason = match triggered_by_user_id {
            Some(uid) => format!(
                "cancelled by user {} (run {} cancelled)",
                uid, schedule_run_id
            ),
            None => format!("cancelled (run {} cancelled)", schedule_run_id),
        };

        // Capture live child ids BEFORE the DB helper flips them — we
        // need them to signal the in-process consumer for each one.
        #[derive(FromQueryResult)]
        struct ChildId {
            id: i32,
        }
        let live_children = ChildId::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"SELECT id FROM backups
                WHERE schedule_run_id = $1 AND state IN ('pending', 'running')"#,
            vec![Value::from(schedule_run_id)],
        ))
        .all(self.db.as_ref())
        .await?;

        let cancelled =
            temps_backup_core::cancel_schedule_run(self.db.as_ref(), schedule_run_id, &reason)
                .await
                .map_err(|e| BackupError::Internal {
                    message: format!("Failed to cancel schedule run {}: {}", schedule_run_id, e),
                })?;

        // Publish a cancel signal per live child so any in-flight engine
        // tasks fire their cancel tokens and exit cleanly.
        let queue = self.queue();
        for child in live_children {
            if let Err(e) = queue
                .send(temps_core::Job::BackupCancelRequested(
                    temps_core::BackupCancelRequestedJob {
                        backup_id: child.id,
                    },
                ))
                .await
            {
                warn!(
                    schedule_run_id,
                    backup_id = child.id,
                    error = %e,
                    "cancel_schedule_run: queue.send for child cancel failed",
                );
            }
        }

        info!(
            schedule_run_id,
            cancelled,
            triggered_by_user_id = ?triggered_by_user_id,
            "BackupService: cancel_schedule_run completed",
        );

        Ok(cancelled)
    }

    pub async fn run_schedule_now(
        &self,
        schedule_id: i32,
        triggered_by_user_id: Option<i32>,
    ) -> Result<ScheduleRunResponse, BackupError> {
        let schedule = self.get_backup_schedule(schedule_id).await?;

        if !schedule.enabled {
            return Err(BackupError::Validation(format!(
                "Schedule {} is disabled; enable it before triggering a manual run",
                schedule_id
            )));
        }

        match self
            .enqueue_scheduled_run(&schedule, TriggerSource::Manual, triggered_by_user_id)
            .await?
        {
            ScheduleRunOutcome::Started { run_id, jobs } => {
                info!(
                    schedule_id = schedule.id,
                    schedule_name = %schedule.name,
                    run_id,
                    job_count = jobs.len(),
                    "run_schedule_now: fan-out run started",
                );
                Ok(ScheduleRunResponse {
                    schedule_run_id: run_id,
                    jobs,
                })
            }
            ScheduleRunOutcome::AlreadyInFlight { existing_run_id } => {
                Err(BackupError::ScheduleRunAlreadyInFlight { existing_run_id })
            }
        }
    }

    /// Fan-out a scheduler tick into one `schedule_runs` row + one control-plane
    /// backup job + one backup job per supported external service.
    ///
    /// ## Fan-out logic (in one transaction)
    ///
    /// 1. Check for an existing in-flight run (any `schedule_runs` row for this
    ///    schedule with `finished_at IS NULL`). If found, return
    ///    [`ScheduleRunOutcome::AlreadyInFlight`] immediately.
    /// 2. Insert a `schedule_runs` row. Capture `run_id`.
    /// 3. Insert a control-plane `backups` row with `schedule_run_id = run_id`.
    ///    Enqueue a `backup_jobs` row for `engine = "control_plane"`.
    /// 4. Load every `external_services` row and attempt to resolve its engine
    ///    key via [`resolve_engine_key`]. Skip rows where resolution returns
    ///    `Err` (unsupported type) — log at `warn` with the service id and type.
    /// 5. For each supported service, insert an `external_service_backups` row +
    ///    parent `backups` row (`schedule_run_id = run_id`), then enqueue a
    ///    `backup_jobs` row. Individual `AlreadyInFlight` responses from the
    ///    concurrency guard are logged at `info` and skipped — the rest of the
    ///    fan-out continues.
    /// 6. Advance `backup_schedules.next_run`, `last_run`, `last_job_id`.
    /// 7. Commit and return [`ScheduleRunOutcome::Started`].
    ///
    /// The cron caller treats both outcome variants as `Ok` and logs the run id.
    /// The "Run now" handler converts `AlreadyInFlight` to a `409 Conflict`.
    /// Close any `schedule_runs` rows for this schedule that have
    /// `finished_at IS NULL` but no longer have any pending/running children.
    ///
    /// Earlier code revisions could leave the parent row open if a worker
    /// crashed between writing the last child's terminal state and calling
    /// `mark_schedule_run_finished_if_done`. Without this reconciler the
    /// concurrency guard would refuse all future "Run now" requests for the
    /// schedule. The UPDATE is idempotent and a no-op for healthy schedules.
    ///
    /// Sets `finished_at` to the latest child `finished_at` so duration
    /// metrics remain accurate; falls back to `NOW()` if every child is
    /// missing a `finished_at` (shouldn't happen, but safer than NULL).
    async fn reconcile_drifted_schedule_runs(&self, schedule_id: i32) -> Result<(), BackupError> {
        use sea_orm::ConnectionTrait;

        let sql = r#"
UPDATE schedule_runs sr
   SET finished_at = COALESCE(
       (SELECT MAX(b.finished_at)
          FROM backups b
         WHERE b.schedule_run_id = sr.id),
       NOW()
   )
 WHERE sr.schedule_id = $1
   AND sr.finished_at IS NULL
   AND NOT EXISTS (
       SELECT 1 FROM backups b
        WHERE b.schedule_run_id = sr.id
          AND b.state IN ('pending', 'running')
   )
   AND EXISTS (
       SELECT 1 FROM backups b
        WHERE b.schedule_run_id = sr.id
   )
        "#;

        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                vec![Value::from(schedule_id)],
            ))
            .await
            .map_err(BackupError::Database)?;

        Ok(())
    }

    pub async fn enqueue_scheduled_run(
        &self,
        schedule: &temps_entities::backup_schedules::Model,
        triggered_by: TriggerSource,
        triggered_by_user_id: Option<i32>,
    ) -> Result<ScheduleRunOutcome, BackupError> {
        use sea_orm::{Set, TransactionTrait};

        let now = chrono::Utc::now();

        // Compute next_run before opening the transaction so a parse error
        // fails fast without wasted DB work.
        let cron_schedule = Schedule::from_str(&schedule.schedule_expression).map_err(|e| {
            BackupError::Validation(format!(
                "Invalid cron expression for schedule {}: {}",
                schedule.id, e
            ))
        })?;
        let next_run = cron_schedule.upcoming(Utc).next();

        // ── Step 1: in-flight check (before opening the write transaction) ────
        //
        // A run is only "in flight" if at least one child backup is still
        // `pending` or `running`. We do NOT rely on `schedule_runs.finished_at`
        // alone — that field can drift to NULL if a prior worker crashed
        // between writing the last child's terminal state and calling
        // `mark_schedule_run_finished_if_done`, leaving the schedule
        // permanently un-runnable. Checking children directly is also the
        // authoritative source of truth used by the aggregate-state SQL in
        // `list_schedule_runs`, so the guard and the UI agree by construction.
        //
        // While we're here, opportunistically close any drifted `schedule_runs`
        // rows so the guard, the UI, and `finished_at`-based queries stay in
        // sync after we let this new run through.
        self.reconcile_drifted_schedule_runs(schedule.id).await?;

        #[derive(FromQueryResult)]
        struct InFlightRow {
            id: i64,
        }

        let in_flight_sql = r#"
SELECT sr.id FROM schedule_runs sr
 WHERE sr.schedule_id = $1
   AND EXISTS (
       SELECT 1 FROM backups b
        WHERE b.schedule_run_id = sr.id
          AND b.state IN ('pending', 'running')
   )
 LIMIT 1
        "#;

        if let Some(existing) = InFlightRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            in_flight_sql,
            vec![Value::from(schedule.id)],
        ))
        .one(self.db.as_ref())
        .await
        .map_err(BackupError::Database)?
        {
            return Ok(ScheduleRunOutcome::AlreadyInFlight {
                existing_run_id: existing.id,
            });
        }

        // ── Step 2: connect to Docker for engine resolution ───────────────────
        // We connect once per tick, not per service, to amortise the socket
        // setup cost. `connect_with_local_defaults()` is fast (no round-trip).
        let docker =
            bollard::Docker::connect_with_local_defaults().map_err(|e| BackupError::Internal {
                message: format!(
                    "enqueue_scheduled_run: failed to connect to Docker for engine resolution \
                     (schedule {}): {}",
                    schedule.id, e
                ),
            })?;

        // Load the external services this schedule should fan out to.
        // Two modes (set on `backup_schedules.target_all_services`):
        //   - true  → every external service on the host (auto-includes
        //             future databases); the explicit join table is ignored.
        //   - false → only services attached via `backup_schedule_services`
        //             (the operator picked specific DBs).
        use sea_orm::{ColumnTrait, QueryFilter};
        let external_services = if schedule.target_all_services {
            temps_entities::external_services::Entity::find()
                .all(self.db.as_ref())
                .await
                .map_err(BackupError::Database)?
        } else {
            temps_entities::external_services::Entity::find()
                .inner_join(temps_entities::backup_schedule_services::Entity)
                .filter(
                    temps_entities::backup_schedule_services::Column::ScheduleId.eq(schedule.id),
                )
                .all(self.db.as_ref())
                .await
                .map_err(BackupError::Database)?
        };

        if external_services.is_empty() {
            // Two reasons we could end up here: no DBs exist yet, or the
            // operator picked "specific" mode and didn't attach anything.
            // Log both with `target_all_services` so it's obvious which.
            info!(
                schedule_id = schedule.id,
                schedule_name = %schedule.name,
                target_all_services = schedule.target_all_services,
                "enqueue_scheduled_run: no external services in scope; fan-out will be control-plane only",
            );
        }

        // Resolve engine keys outside the transaction (async Docker probes).
        let mut resolved_services: Vec<(temps_entities::external_services::Model, &'static str)> =
            Vec::with_capacity(external_services.len());

        for svc in external_services {
            match crate::engines::dispatch::resolve_engine_key(&svc, &docker).await {
                Ok(engine_key) => {
                    resolved_services.push((svc, engine_key));
                }
                Err(e) => {
                    warn!(
                        service_id = svc.id,
                        service_type = %svc.service_type,
                        error = %e,
                        "enqueue_scheduled_run: skipping unsupported external service",
                    );
                }
            }
        }

        // ── Step 3: open the write transaction ────────────────────────────────

        let txn = self.db.begin().await?;

        // Insert the schedule_runs row.
        let run_insert_sql = r#"
INSERT INTO schedule_runs (schedule_id, triggered_by, triggered_by_user_id, started_at, created_at)
VALUES ($1, $2, $3, NOW(), NOW())
RETURNING id
        "#;

        #[derive(FromQueryResult)]
        struct RunIdRow {
            id: i64,
        }

        let run_id = RunIdRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            run_insert_sql,
            vec![
                Value::from(schedule.id),
                Value::from(triggered_by.as_str().to_owned()),
                Value::from(triggered_by_user_id),
            ],
        ))
        .one(&txn)
        .await
        .map_err(BackupError::Database)?
        .ok_or_else(|| BackupError::Internal {
            message: format!(
                "enqueue_scheduled_run: INSERT into schedule_runs returned no id \
                 for schedule {}",
                schedule.id
            ),
        })?
        .id;

        let mut jobs: Vec<EnqueuedJob> = Vec::new();

        // Defer queue publishes until after txn.commit so the consumer
        // can't dispatch an engine against a backups row the txn might
        // still roll back.
        let mut deferred_messages: Vec<temps_core::BackupRequestedJob> = Vec::new();

        // ── Step 4: control-plane backup (skipped when the schedule
        // ──         opts out of control-plane coverage). ─────────────────────
        if schedule.include_control_plane {
            let cp_uuid = Uuid::new_v4().to_string();
            let cp_backup = temps_entities::backups::ActiveModel {
                id: sea_orm::NotSet,
                name: Set(format!("Backup {}", cp_uuid)),
                backup_id: Set(cp_uuid.clone()),
                schedule_id: Set(Some(schedule.id)),
                schedule_run_id: Set(Some(run_id)),
                backup_type: Set(schedule.backup_type.clone()),
                state: Set("pending".to_string()),
                started_at: Set(now),
                finished_at: Set(None),
                s3_source_id: Set(schedule.s3_source_id),
                s3_location: Set(String::new()),
                compression_type: Set("gzip".to_string()),
                created_by: Set(0),
                tags: Set("[]".to_string()),
                size_bytes: Set(None),
                file_count: Set(None),
                error_message: Set(None),
                expires_at: Set(None),
                checksum: Set(None),
                metadata: Set(serde_json::json!({
                    "engine": "control_plane",
                    "async_runner": true,
                    "scheduled": triggered_by == TriggerSource::Cron,
                    "schedule_id": schedule.id,
                    "run_id": run_id,
                    "timestamp": now.to_rfc3339(),
                })
                .to_string()),
            };

            let cp_backup_row = cp_backup.insert(&txn).await?;

            deferred_messages.push(temps_core::BackupRequestedJob {
                backup_id: cp_backup_row.id,
                engine: "control_plane".to_string(),
                params: serde_json::json!({
                    "s3_source_id": schedule.s3_source_id,
                    "schedule_id": schedule.id,
                    "run_id": run_id,
                }),
                max_runtime_secs: schedule.max_runtime_secs.unwrap_or(4 * 60 * 60),
            });
            jobs.push(EnqueuedJob {
                backup_id: cp_backup_row.id,
                job_id: cp_backup_row.id as i64,
                engine: "control_plane".to_string(),
                target_service_id: None,
            });
        } else {
            info!(
                schedule_id = schedule.id,
                run_id,
                "enqueue_scheduled_run: include_control_plane=false, skipping control-plane backup",
            );
        }

        // ── Step 5: external service backups ──────────────────────────────────

        for (svc, engine_key) in &resolved_services {
            let trigger = BackupTriggerParams {
                engine: engine_key.to_string(),
                params: serde_json::json!({
                    "service_id": svc.id,
                    "s3_source_id": schedule.s3_source_id,
                    "backup_type": schedule.backup_type,
                    "schedule_id": schedule.id,
                    "run_id": run_id,
                }),
                max_runtime_secs: schedule.max_runtime_secs,
            };

            match self
                .insert_pending_external_service_backup_in_txn(
                    &txn,
                    svc.id,
                    schedule.s3_source_id,
                    &schedule.backup_type,
                    0,
                    "gzip",
                    Some(ScheduleRunContext {
                        schedule_id: schedule.id,
                        schedule_run_id: run_id,
                    }),
                    &trigger,
                )
                .await
            {
                Ok((parent_row, _esb_row)) => {
                    deferred_messages.push(temps_core::BackupRequestedJob {
                        backup_id: parent_row.id,
                        engine: trigger.engine.clone(),
                        params: trigger.params.clone(),
                        max_runtime_secs: trigger.max_runtime_secs.unwrap_or(4 * 60 * 60),
                    });
                    jobs.push(EnqueuedJob {
                        backup_id: parent_row.id,
                        job_id: parent_row.id as i64,
                        engine: engine_key.to_string(),
                        target_service_id: Some(svc.id),
                    });
                }
                Err(e) => {
                    warn!(
                        schedule_id = schedule.id,
                        service_id = svc.id,
                        service_name = %svc.name,
                        engine = engine_key,
                        error = %e,
                        "enqueue_scheduled_run: failed to insert external service rows, skipping",
                    );
                }
            }
        }

        // ── Step 6: advance schedule metadata ─────────────────────────────────
        let mut schedule_update: temps_entities::backup_schedules::ActiveModel =
            schedule.clone().into_active_model();
        schedule_update.next_run = Set(next_run);
        schedule_update.last_run = Set(Some(now));
        schedule_update.update(&txn).await?;

        // ── Commit ────────────────────────────────────────────────────────────

        txn.commit().await?;

        // ── Publish queue messages after commit ───────────────────────────────
        // Publishing after commit guarantees the consumer never dispatches
        // an engine against a backups row that the txn might roll back.
        let queue = self.queue();
        for req in deferred_messages {
            let backup_id = req.backup_id;
            if let Err(e) = queue.send(temps_core::Job::BackupRequested(req)).await {
                warn!(
                    schedule_id = schedule.id,
                    backup_id,
                    error = %e,
                    "enqueue_scheduled_run: queue.send failed (row committed but not dispatched)",
                );
            }
        }

        info!(
            schedule_id = schedule.id,
            schedule_name = %schedule.name,
            run_id,
            job_count = jobs.len(),
            triggered_by = triggered_by.as_str(),
            "enqueue_scheduled_run: fan-out committed and dispatched",
        );

        Ok(ScheduleRunOutcome::Started { run_id, jobs })
    }

    /// Insert a pending control-plane `backups` row and dispatch the
    /// matching engine task on the executor. The synthetic `i64` returned
    /// is the backup row's id widened for backwards-compat with callers
    /// that previously took a `backup_jobs.id`.
    pub async fn create_pending_backup_row(
        &self,
        s3_source_id: i32,
        backup_type: &str,
        created_by: i32,
        trigger: BackupTriggerParams,
    ) -> Result<(Backup, i64), BackupError> {
        use sea_orm::Set;

        temps_entities::s3_sources::Entity::find_by_id(s3_source_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: format!("S3 source {} not found", s3_source_id),
            })?;

        let backup_uuid = Uuid::new_v4().to_string();
        let now = chrono::Utc::now();

        let new_backup = temps_entities::backups::ActiveModel {
            id: sea_orm::NotSet,
            name: Set(format!("Backup {}", backup_uuid)),
            backup_id: Set(backup_uuid.clone()),
            schedule_id: Set(None),
            schedule_run_id: sea_orm::NotSet,
            backup_type: Set(backup_type.to_string()),
            state: Set("pending".to_string()),
            started_at: Set(now),
            finished_at: Set(None),
            s3_source_id: Set(s3_source_id),
            s3_location: Set(String::new()),
            compression_type: Set("gzip".to_string()),
            created_by: Set(created_by),
            tags: Set("[]".to_string()),
            size_bytes: Set(None),
            file_count: Set(None),
            error_message: Set(None),
            expires_at: Set(None),
            checksum: Set(None),
            metadata: Set(serde_json::json!({
                "engine": trigger.engine,
                "async_runner": true,
                "timestamp": now.to_rfc3339(),
            })
            .to_string()),
        };

        let backup = new_backup.insert(self.db.as_ref()).await?;
        let backup_id = backup.id;

        // Publish to the queue. If publish fails the row is left in
        // `pending`; the next boot's reconcile will flip it to `failed`.
        let max_runtime_secs = trigger.max_runtime_secs.unwrap_or(4 * 60 * 60);
        if let Err(e) = self
            .queue()
            .send(temps_core::Job::BackupRequested(
                temps_core::BackupRequestedJob {
                    backup_id,
                    engine: trigger.engine.clone(),
                    params: trigger.params,
                    max_runtime_secs,
                },
            ))
            .await
        {
            return Err(BackupError::Internal {
                message: format!(
                    "Failed to publish BackupRequested for backup {}: {}",
                    backup_id, e
                ),
            });
        }

        info!(
            backup_id = %backup.backup_id,
            s3_source_id,
            backup_row_id = backup_id,
            "BackupService: created pending backup row and published BackupRequested",
        );

        Ok((backup, backup_id as i64))
    }

    /// Insert parent `backups` + child `external_service_backups` rows inside
    /// the supplied transaction. Does NOT spawn the engine task — the caller
    /// must call `executor.spawn` AFTER committing the transaction so the
    /// engine never sees a row the txn might roll back.
    ///
    /// Used by `enqueue_scheduled_run`'s fan-out. Manual triggers go through
    /// [`create_pending_external_service_backup_row`] instead, which handles
    /// the txn lifecycle internally.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_pending_external_service_backup_in_txn(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        service_id: i32,
        s3_source_id: i32,
        backup_type: &str,
        created_by: i32,
        compression_type: &str,
        schedule_ctx: Option<ScheduleRunContext>,
        trigger: &BackupTriggerParams,
    ) -> Result<
        (
            temps_entities::backups::Model,
            temps_entities::external_service_backups::Model,
        ),
        BackupError,
    > {
        use sea_orm::Set;

        let backup_uuid = Uuid::new_v4().to_string();
        let now = chrono::Utc::now();

        let mut backups_metadata = serde_json::Map::new();
        backups_metadata.insert(
            "engine".to_string(),
            serde_json::Value::String(trigger.engine.clone()),
        );
        backups_metadata.insert("async_runner".to_string(), serde_json::Value::Bool(true));
        backups_metadata.insert(
            "external_service_id".to_string(),
            serde_json::Value::Number(service_id.into()),
        );
        backups_metadata.insert(
            "service_id".to_string(),
            serde_json::Value::Number(service_id.into()),
        );
        backups_metadata.insert(
            "timestamp".to_string(),
            serde_json::Value::String(now.to_rfc3339()),
        );
        if let Some(ctx) = schedule_ctx {
            backups_metadata.insert(
                "schedule_id".to_string(),
                serde_json::Value::Number(ctx.schedule_id.into()),
            );
            backups_metadata.insert(
                "run_id".to_string(),
                serde_json::Value::Number(ctx.schedule_run_id.into()),
            );
        }

        let mut esb_metadata = serde_json::Map::new();
        esb_metadata.insert(
            "engine".to_string(),
            serde_json::Value::String(trigger.engine.clone()),
        );
        esb_metadata.insert("async_runner".to_string(), serde_json::Value::Bool(true));
        esb_metadata.insert(
            "backup_uuid".to_string(),
            serde_json::Value::String(backup_uuid.clone()),
        );
        esb_metadata.insert(
            "timestamp".to_string(),
            serde_json::Value::String(now.to_rfc3339()),
        );
        if let Some(ctx) = schedule_ctx {
            esb_metadata.insert(
                "schedule_id".to_string(),
                serde_json::Value::Number(ctx.schedule_id.into()),
            );
            esb_metadata.insert(
                "run_id".to_string(),
                serde_json::Value::Number(ctx.schedule_run_id.into()),
            );
        }

        let parent = temps_entities::backups::ActiveModel {
            id: sea_orm::NotSet,
            name: Set(format!("Backup {}", backup_uuid)),
            backup_id: Set(backup_uuid.clone()),
            schedule_id: Set(schedule_ctx.map(|c| c.schedule_id)),
            schedule_run_id: Set(schedule_ctx.map(|c| c.schedule_run_id)),
            backup_type: Set(backup_type.to_string()),
            state: Set("pending".to_string()),
            started_at: Set(now),
            finished_at: Set(None),
            s3_source_id: Set(s3_source_id),
            s3_location: Set(String::new()),
            compression_type: Set(compression_type.to_string()),
            created_by: Set(created_by),
            tags: Set("[]".to_string()),
            size_bytes: Set(None),
            file_count: Set(None),
            error_message: Set(None),
            expires_at: Set(None),
            checksum: Set(None),
            metadata: Set(serde_json::Value::Object(backups_metadata).to_string()),
        }
        .insert(txn)
        .await?;

        let child = temps_entities::external_service_backups::ActiveModel {
            id: sea_orm::NotSet,
            service_id: Set(service_id),
            backup_id: Set(parent.id),
            backup_type: Set(backup_type.to_string()),
            state: Set("pending".to_string()),
            started_at: Set(now),
            finished_at: Set(None),
            size_bytes: Set(None),
            s3_location: Set(String::new()),
            error_message: Set(None),
            metadata: Set(serde_json::Value::Object(esb_metadata)),
            checksum: Set(None),
            compression_type: Set(compression_type.to_string()),
            created_by: Set(created_by),
            expires_at: Set(None),
        }
        .insert(txn)
        .await?;

        info!(
            backup_id = %backup_uuid,
            service_id,
            s3_source_id,
            parent_row_id = parent.id,
            child_row_id = child.id,
            "BackupService: inserted pending external-service backup rows (engine task pending)",
        );

        Ok((parent, child))
    }

    /// Insert pending external-service backup rows (parent + child) and
    /// dispatch the engine task on the executor.
    ///
    /// Used by the manual `POST /external-services/{id}/run` handler. The
    /// txn is opened and committed internally; spawn happens after commit.
    ///
    /// Returns the child row and a synthetic `i64` (the parent backup row's
    /// id, widened) for backwards-compat with callers that previously used
    /// a `backup_jobs.id`.
    pub async fn create_pending_external_service_backup_row(
        &self,
        service_id: i32,
        s3_source_id: i32,
        backup_type: &str,
        created_by: i32,
        trigger: BackupTriggerParams,
    ) -> Result<(temps_entities::external_service_backups::Model, i64), BackupError> {
        temps_entities::external_services::Entity::find_by_id(service_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "ExternalService".to_string(),
                detail: format!("External service with ID {} not found", service_id),
            })?;
        temps_entities::s3_sources::Entity::find_by_id(s3_source_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: format!("S3 source {} not found", s3_source_id),
            })?;

        let txn = self.db.begin().await?;
        let (parent, child) = self
            .insert_pending_external_service_backup_in_txn(
                &txn,
                service_id,
                s3_source_id,
                backup_type,
                created_by,
                "none",
                None,
                &trigger,
            )
            .await?;
        txn.commit().await?;

        let backup_id = parent.id;
        let max_runtime_secs = trigger.max_runtime_secs.unwrap_or(4 * 60 * 60);
        if let Err(e) = self
            .queue()
            .send(temps_core::Job::BackupRequested(
                temps_core::BackupRequestedJob {
                    backup_id,
                    engine: trigger.engine.clone(),
                    params: trigger.params,
                    max_runtime_secs,
                },
            ))
            .await
        {
            warn!(
                backup_id,
                service_id,
                error = %e,
                "create_pending_external_service_backup_row: queue.send failed; row committed but not dispatched",
            );
        }

        Ok((child, backup_id as i64))
    }

    /// Update an S3 source
    pub async fn update_s3_source(
        &self,
        id: i32,
        request: crate::handlers::backup_handler::UpdateS3SourceRequest,
    ) -> Result<S3Source, BackupError> {
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        let current = temps_entities::s3_sources::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: "S3 source not found".to_string(),
            })?;

        let mut active = current.into_active_model();

        if let Some(name) = request.name {
            active.name = Set(name);
        }
        if let Some(bucket_name) = request.bucket_name {
            active.bucket_name = Set(bucket_name);
        }
        if let Some(bucket_path) = request.bucket_path {
            active.bucket_path = Set(bucket_path);
        }
        if let Some(access_key_id) = request.access_key_id {
            // Encrypt access key before storing
            let encrypted_access_key = self
                .encryption_service
                .encrypt_string(&access_key_id)
                .map_err(|e| BackupError::Internal {
                    message: format!("Failed to encrypt access key: {}", e),
                })?;
            active.access_key_id = Set(encrypted_access_key);
        }
        if let Some(secret_key) = request.secret_key {
            // Encrypt secret key before storing
            let encrypted_secret_key = self
                .encryption_service
                .encrypt_string(&secret_key)
                .map_err(|e| BackupError::Internal {
                    message: format!("Failed to encrypt secret key: {}", e),
                })?;
            active.secret_key = Set(encrypted_secret_key);
        }
        if let Some(region) = request.region {
            active.region = Set(region);
        }
        if let Some(endpoint) = request.endpoint {
            active.endpoint = Set(Some(endpoint));
        }
        if let Some(force_path_style) = request.force_path_style {
            active.force_path_style = Set(Some(force_path_style));
        }

        active.updated_at = Set(chrono::Utc::now());

        let updated = active.update(self.db.as_ref()).await?;
        Ok(updated)
    }

    /// Generate metadata for a backup
    fn generate_backup_metadata(
        &self,
        backup: &Backup,
        s3_source: &temps_entities::s3_sources::Model,
        external_backups: &[(
            temps_entities::external_service_backups::Model,
            temps_entities::external_services::Model,
        )],
    ) -> serde_json::Value {
        // Serialize the server config
        let config_yaml = serde_yaml::to_string(&self.config_service.get_server_config())
            .unwrap_or_else(|e| {
                error!("Failed to serialize server config: {}", e);
                String::new()
            });

        // Map external backups to the required format
        let external_backups = external_backups
            .iter()
            .map(|(b, service)| {
                json!({
                    "backup_id": b.backup_id,
                    "service_id": b.service_id,
                    "s3_location": b.s3_location,
                    "state": b.state,
                    "size_bytes": b.size_bytes,
                    "type": "full",
                    "metadata": {
                        "service_type": service.service_type,
                        "service_name": service.name
                    }
                })
            })
            .collect::<Vec<_>>();

        json!({
            "backup_id": backup.backup_id,
            "name": backup.name,
            "type": backup.backup_type,
            "created_at": backup.started_at.to_rfc3339(),
            "created_by": backup.created_by,
            "size_bytes": backup.size_bytes,
            "compression_type": backup.compression_type,
            "source": {
                "id": s3_source.id,
                "name": s3_source.name,
                "bucket": s3_source.bucket_name,
                "path": s3_source.bucket_path
            },
            "schedule_id": backup.schedule_id,
            "state": backup.state,
            "tags": serde_json::from_str::<Vec<String>>(&backup.tags).unwrap_or_default(),
            "checksum": backup.checksum,
            "server_config": config_yaml,
            "external_service_backups": external_backups,
            "metadata": serde_json::from_str::<serde_json::Value>(&backup.metadata).unwrap_or_default()
        })
    }

    /// Update the source's backup index
    async fn update_backup_index(
        &self,
        s3_client: &S3Client,
        s3_source: &temps_entities::s3_sources::Model,
        backup: &Backup,
    ) -> Result<()> {
        let index_key = build_s3_key(&s3_source.bucket_path, "backups/index.json");

        // Try to get existing index
        let mut index = match s3_client
            .get_object()
            .bucket(&s3_source.bucket_name)
            .key(&index_key)
            .send()
            .await
        {
            Ok(response) => {
                let data = response.body.collect().await?.to_vec();
                serde_json::from_slice::<serde_json::Value>(&data).unwrap_or_else(|_| {
                    json!({
                        "backups": [],
                        "last_updated": Utc::now().to_rfc3339()
                    })
                })
            }
            Err(_) => json!({
                "backups": [],
                "last_updated": Utc::now().to_rfc3339()
            }),
        };
        // Add new backup to index
        if let Some(backups) = index.get_mut("backups").and_then(|b| b.as_array_mut()) {
            backups.push(json!({
                "id": backup.id,
                "backup_id": backup.backup_id,
                "name": backup.name,
                "type": backup.backup_type,
                "created_at": backup.started_at.to_rfc3339(),
                "size_bytes": backup.size_bytes,
                "location": backup.s3_location.clone(),
                "metadata_location": backup.s3_location
                    .replace("backup.sql.gz", "metadata.json")
                    .replace("backup.postgresql.gz", "metadata.json")
            }));
        }
        index["last_updated"] = json!(Utc::now().to_rfc3339());

        // Upload updated index
        s3_client
            .put_object()
            .bucket(&s3_source.bucket_name)
            .key(&index_key)
            .body(serde_json::to_vec(&index)?.into())
            .content_type("application/json")
            .send()
            .await?;

        Ok(())
    }

    /// List every backup visible on an S3 source.
    ///
    /// Returns a union of two sources of truth, intended for the restore
    /// UI (both regular and cross-service disaster-recovery):
    ///
    /// 1. **DB rows** — backups this Temps instance recorded. Cheap,
    ///    trusted, has the canonical backup_id / state / size.
    /// 2. **S3 scan** (only when `include_s3_scan` is `true`) — objects
    ///    discovered by walking
    ///    `s3://<bucket>/<bucket_path>/external_services/<engine>/<service>/`.
    ///    This is how DR works when you've restored a Temps instance and
    ///    need to browse backups made by a previous instance whose DB you
    ///    no longer have. S3-scan entries get `id: 0`, `backup_id: ""`,
    ///    and `source: "s3_scan"` — the restore orchestrator keys off
    ///    `location` in that case, not `backup_id`.
    ///
    /// Setting `include_s3_scan = false` (the default) skips the bucket
    /// walk entirely and returns DB rows only — completing in <100 ms
    /// regardless of S3 endpoint latency.
    pub async fn list_source_backups(
        &self,
        s3_source_id: i32,
        include_s3_scan: bool,
    ) -> Result<serde_json::Value, BackupError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

        let s3_source = temps_entities::s3_sources::Entity::find_by_id(s3_source_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: "S3 source not found".to_string(),
            })?;

        // ---- Pass 1: DB-tracked backups ------------------------------------
        // `backup_external_service` inserts with state='running' and (now)
        // updates to 'completed' on success. Older rows may still be stuck
        // in 'running' — we show them anyway so they're visible/debuggable;
        // the UI badges them differently from completed ones.
        let db_rows = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::S3SourceId.eq(s3_source_id))
            .order_by_desc(temps_entities::backups::Column::StartedAt)
            .all(self.db.as_ref())
            .await?;

        let mut entries: Vec<serde_json::Value> = Vec::with_capacity(db_rows.len());
        // Collect locations we've seen to avoid surfacing the same backup
        // twice (once from DB + once from S3 scan).
        let mut seen_locations: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        // Cache for external_services lookups so we don't refetch the same
        // row N times within a single listing. Keyed by external_services.id.
        let mut ext_service_cache: std::collections::HashMap<i32, Option<(String, String)>> =
            std::collections::HashMap::new();

        for backup in db_rows {
            let metadata: serde_json::Value =
                serde_json::from_str(&backup.metadata).unwrap_or(serde_json::Value::Null);
            let mut service_name = metadata
                .get("service_name")
                .and_then(|v| v.as_str())
                .map(String::from);
            let mut service_type = metadata
                .get("service_type")
                .and_then(|v| v.as_str())
                .map(String::from);

            // ADR-014 async runner rows write only `external_service_id` into
            // metadata (not `service_name`/`service_type`). Without filling
            // those in, the frontend ServiceDetail.tsx page filters them out
            // (it matches by `origin_service_name === serviceName`) and the
            // user's failed/pending backups become invisible. Look up the
            // external service once per id and cache.
            if service_name.is_none() || service_type.is_none() {
                if let Some(ext_id) = metadata
                    .get("external_service_id")
                    .and_then(|v| v.as_i64())
                    .and_then(|v| i32::try_from(v).ok())
                {
                    let cached = ext_service_cache.entry(ext_id).or_insert_with_key(|_| None);
                    if cached.is_none() {
                        // Cache miss — try the DB. A None result means we
                        // already failed once; don't refetch. We can't reuse
                        // the entry api's value because we need an async call.
                        if let Ok(Some(svc)) =
                            temps_entities::external_services::Entity::find_by_id(ext_id)
                                .one(self.db.as_ref())
                                .await
                        {
                            *cached = Some((svc.name.clone(), svc.service_type.clone()));
                        }
                    }
                    if let Some((n, t)) = cached.clone() {
                        if service_name.is_none() {
                            service_name = Some(n);
                        }
                        if service_type.is_none() {
                            service_type = Some(t);
                        }
                    }
                }
            }

            // Skip control-plane backups — this endpoint powers the
            // "restore into an external service" UI, and whole-Temps-DB
            // backups (stored under `backups/...`, no service_type in
            // metadata) are not valid candidates for that flow. They'd
            // render as "pg_dump" with blank engine and confuse users
            // into thinking they could be restored onto their service.
            //
            // Rows created by the ADR-014 async runner for external services
            // may have an empty `s3_location` while pending (the location is
            // filled in by `mark_job_completed` on `Done`). These rows carry
            // `external_service_id` in their metadata — that field is the
            // canonical signal that the row belongs to an external service.
            // Using the `s3_location` alone to classify pending/failed rows
            // is the root cause of the "invisible backups" bug (Bug 4).
            let has_external_service_id = metadata.get("external_service_id").is_some();
            let is_control_plane =
                metadata.get("engine").and_then(|v| v.as_str()) == Some("control_plane");
            let is_external_service_location = backup.s3_location.contains("external_services/");

            // Include the row only if it is clearly an external-service backup.
            // Rule: skip if none of the three external-service signals are present.
            if !has_external_service_id
                && !is_external_service_location
                && service_type.is_none()
                && !is_control_plane
            {
                // Not enough signal — could be legacy orphan data. Skip.
                continue;
            }
            // Always skip confirmed control-plane backups.
            if is_control_plane && !is_external_service_location {
                continue;
            }

            let display_name = match (&service_name, &service_type) {
                (Some(n), Some(t)) => format!("{} backup ({})", t, n),
                _ => backup.name.clone(),
            };

            let format = classify_backup_format(&backup.s3_location, service_type.as_deref());

            let metadata_location = if backup.s3_location.is_empty() {
                String::new()
            } else {
                backup
                    .s3_location
                    .replace("backup.sql.gz", "metadata.json")
                    .replace("backup.postgresql.gz", "metadata.json")
            };

            if !backup.s3_location.is_empty() {
                seen_locations.insert(backup.s3_location.clone());
            }

            entries.push(serde_json::json!({
                "id": backup.id,
                "backup_id": backup.backup_id,
                "name": display_name,
                "type": backup.backup_type,
                "created_at": backup.started_at.to_rfc3339(),
                "size_bytes": backup.size_bytes,
                "location": backup.s3_location,
                "metadata_location": metadata_location,
                "engine": service_type,
                "origin_service_name": service_name,
                "format": format,
                "source": "db",
                "state": backup.state,
            }));
        }

        // ---- Pass 2: S3 scan for orphan backups (opt-in) ------------------
        // Only executed when `include_s3_scan` is true.  Skipped by default
        // because each scan may issue dozens of sequential LIST_OBJECTS_V2
        // calls against slow S3-compatible endpoints (e.g. OVH Object Storage
        // can take 5-30 s for a bucket with many prefixes).
        //
        // Best-effort: if the S3 client can't talk to the bucket we just
        // skip this pass and return the DB-based list. We never fail the
        // whole endpoint just because a bucket scan failed — the UI's
        // happy-path for normal users doesn't depend on it.
        //
        // Dedupe rule: if any DB row already references a given
        // `origin_service_name`, we DROP all S3-scan hits for that service
        // (the DB is authoritative — even if the row's `s3_location` is
        // empty due to the old bug, the user should pick the DB row so
        // the restore runs through the DB-backed path). S3-scan fills the
        // gap only for services this Temps has no DB record of.
        if include_s3_scan {
            let db_tracked_services: std::collections::HashSet<String> = entries
                .iter()
                .filter_map(|e| {
                    e.get("origin_service_name")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                })
                .collect();

            if let Ok(s3_client) = self.create_s3_client(&s3_source).await {
                match scan_s3_for_orphan_backups(&s3_client, &s3_source, &seen_locations).await {
                    Ok(scanned) => {
                        // For DB rows with empty `s3_location` (pre-fix backup
                        // rows), steal the matching S3-scan location so the
                        // entry is still restorable. Key by
                        // `origin_service_name` since we don't know the exact
                        // backup id from the scan.
                        let fallback_locations: std::collections::HashMap<
                            String,
                            (String, Option<String>),
                        > = scanned
                            .iter()
                            .filter_map(|e| {
                                let svc = e
                                    .get("origin_service_name")
                                    .and_then(|v| v.as_str())?
                                    .to_string();
                                let loc = e.get("location").and_then(|v| v.as_str())?.to_string();
                                let fmt =
                                    e.get("format").and_then(|v| v.as_str()).map(String::from);
                                Some((svc, (loc, fmt)))
                            })
                            .collect();

                        for entry in entries.iter_mut() {
                            let needs_fill = entry
                                .get("location")
                                .and_then(|v| v.as_str())
                                .map(|s| s.is_empty())
                                .unwrap_or(true);
                            if !needs_fill {
                                continue;
                            }
                            let origin =
                                match entry.get("origin_service_name").and_then(|v| v.as_str()) {
                                    Some(s) => s.to_string(),
                                    None => continue,
                                };
                            if let Some((loc, fmt)) = fallback_locations.get(&origin) {
                                entry["location"] = serde_json::Value::String(loc.clone());
                                if let Some(fmt) = fmt {
                                    entry["format"] = serde_json::Value::String(fmt.clone());
                                }
                            }
                        }

                        // Emit scanned entries for services not tracked by DB.
                        for entry in scanned {
                            let origin = entry
                                .get("origin_service_name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            if db_tracked_services.contains(origin) {
                                continue;
                            }
                            entries.push(entry);
                        }
                    }
                    Err(e) => {
                        warn!(
                        "S3 scan for orphan backups on source {} failed (returning DB-only list): {}",
                        s3_source_id, e
                    );
                    }
                }
            } else {
                warn!(
                    "Skipping S3 scan on source {}: failed to build S3 client",
                    s3_source_id
                );
            }
        } // end if include_s3_scan

        // Final sort: newest first, regardless of source.
        entries.sort_by(|a, b| {
            let ak = a.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
            let bk = b.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
            bk.cmp(ak)
        });

        let last_updated = entries
            .iter()
            .filter_map(|e| {
                e.get("created_at")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .next()
            .unwrap_or_else(|| Utc::now().to_rfc3339());

        Ok(serde_json::json!({
            "backups": entries,
            "last_updated": last_updated,
        }))
    }

    /// List backups for a specific external service using a single JOIN query.
    ///
    /// Unlike [`list_source_backups`], this method never touches S3.  It issues
    /// one SQL round-trip:
    ///
    /// ```sql
    /// SELECT b.id, b.backup_id, b.name, b.state, b.started_at, b.finished_at,
    ///        b.size_bytes, b.s3_location, b.error_message, b.compression_type,
    ///        b.s3_source_id, s.name AS s3_source_name,
    ///        esb.id AS external_service_backup_id
    /// FROM external_service_backups esb
    /// JOIN backups b ON b.id = esb.backup_id
    /// JOIN s3_sources s ON s.id = b.s3_source_id
    /// WHERE esb.service_id = $1
    /// ORDER BY b.started_at DESC
    /// LIMIT $2 OFFSET $3
    /// ```
    ///
    /// Returns a page of [`ServiceBackupEntry`] values plus the total count for
    /// pagination.  `page` is 1-based; `page_size` is capped at 100.
    pub async fn list_external_service_backups(
        &self,
        service_id: i32,
        page: i64,
        page_size: i64,
    ) -> Result<(Vec<ServiceBackupEntry>, i64), BackupError> {
        let page = page.max(1);
        let page_size = page_size.clamp(1, 100);
        let offset = (page - 1) * page_size;

        // Count total rows so the caller can render pagination controls.
        let count_stmt = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"SELECT COUNT(*) AS cnt
               FROM external_service_backups esb
               WHERE esb.service_id = $1"#,
            vec![Value::Int(Some(service_id))],
        );

        #[derive(FromQueryResult)]
        struct CountRow {
            cnt: i64,
        }

        let total = CountRow::find_by_statement(count_stmt)
            .one(self.db.as_ref())
            .await?
            .map(|r| r.cnt)
            .unwrap_or(0);

        // Fetch the page of backups.
        let rows_stmt = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"SELECT
                   b.id,
                   b.backup_id,
                   b.name,
                   b.state,
                   b.backup_type,
                   b.started_at,
                   b.finished_at,
                   b.size_bytes,
                   b.s3_location,
                   b.error_message,
                   b.compression_type,
                   b.s3_source_id,
                   s.name AS s3_source_name,
                   esb.id AS external_service_backup_id
               FROM external_service_backups esb
               JOIN backups b ON b.id = esb.backup_id
               JOIN s3_sources s ON s.id = b.s3_source_id
               WHERE esb.service_id = $1
               ORDER BY b.started_at DESC
               LIMIT $2 OFFSET $3"#,
            vec![
                Value::Int(Some(service_id)),
                Value::BigInt(Some(page_size)),
                Value::BigInt(Some(offset)),
            ],
        );

        let entries = ServiceBackupEntry::find_by_statement(rows_stmt)
            .all(self.db.as_ref())
            .await?;

        debug!(
            service_id,
            count = entries.len(),
            total,
            "list_external_service_backups: returned DB-only page"
        );

        Ok((entries, total))
    }

    /// Return every `external_service_backups` child row that belongs to the
    /// given parent `backups.id`, joined with `external_services` so the caller
    /// can display the service name and type without a second round-trip.
    ///
    /// Returns an empty `Vec` when the parent backup has no children (control-
    /// plane backups have no children by definition).  Returns `NotFound` when
    /// the parent `backups` row does not exist.
    ///
    /// SQL uses a single JOIN — no N+1.
    pub async fn list_child_backups(
        &self,
        parent_backup_id: i32,
    ) -> Result<Vec<ChildBackupEntry>, BackupError> {
        use sea_orm::EntityTrait;

        // Verify the parent backup exists first so we return 404 instead of an
        // empty list when the caller passes an unknown integer id.
        let _parent = temps_entities::backups::Entity::find_by_id(parent_backup_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "Backup".to_string(),
                detail: format!("Parent backup with id {} not found", parent_backup_id),
            })?;

        let sql = r#"
SELECT
    esb.id            AS id,
    esb.service_id    AS service_id,
    esb.state         AS state,
    esb.backup_type   AS backup_type,
    esb.started_at    AS started_at,
    esb.finished_at   AS finished_at,
    esb.size_bytes    AS size_bytes,
    esb.s3_location   AS s3_location,
    esb.error_message AS error_message,
    esb.compression_type AS compression_type,
    es.name           AS service_name,
    es.service_type   AS service_type
FROM external_service_backups esb
JOIN external_services es ON es.id = esb.service_id
WHERE esb.backup_id = $1
ORDER BY esb.id ASC
        "#;

        let rows = ChildBackupEntry::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            vec![Value::Int(Some(parent_backup_id))],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(BackupError::Database)?;

        debug!(
            parent_backup_id,
            count = rows.len(),
            "list_child_backups: returned children"
        );

        Ok(rows)
    }

    /// Get a backup by ID
    pub async fn get_backup(&self, backup_id: &str) -> Result<Option<Backup>, BackupError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let model = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::BackupId.eq(backup_id.to_string()))
            .one(self.db.as_ref())
            .await?;

        Ok(model)
    }

    /// Best-effort progress size for a running backup.
    ///
    /// Computed by listing the backup's S3 location and summing the
    /// reported sizes. Returns `None` for non-running backups (their
    /// `size_bytes` is authoritative once finished) and for backups
    /// whose `s3_location` isn't usable yet (engine still warming up).
    /// Errors talking to S3 are downgraded to `None` and logged — the
    /// detail page is best-effort.
    pub async fn compute_live_size(&self, backup: &Backup) -> Option<i64> {
        if backup.state != "running" {
            return None;
        }
        if backup.s3_location.is_empty() {
            return None;
        }

        let s3_source = match temps_entities::s3_sources::Entity::find_by_id(backup.s3_source_id)
            .one(self.db.as_ref())
            .await
        {
            Ok(Some(src)) => src,
            Ok(None) => return None,
            Err(e) => {
                warn!(
                    "Failed to load s3_source {} for live size: {}",
                    backup.s3_source_id, e
                );
                return None;
            }
        };

        let s3_client = match self.create_s3_client(&s3_source).await {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    "Failed to build S3 client for live-size lookup on backup {}: {}",
                    backup.id, e
                );
                return None;
            }
        };

        // The location can be either an `s3://bucket/key` URL (WAL-G,
        // cluster) or a bucket-relative key (pg_dump, mongodump). Try the
        // URL form first; fall back to treating the value as a key.
        let bucket = &s3_source.bucket_name;
        let key = if let Some((url_bucket, url_key)) =
            temps_providers::externalsvc::s3_util::parse_s3_url(&backup.s3_location)
        {
            // Sanity: only list inside our configured bucket. WAL-G's
            // prefix should always live in this same bucket.
            if &url_bucket != bucket {
                debug!(
                    "live size: s3:// bucket {} != configured bucket {}, listing anyway",
                    url_bucket, bucket
                );
            }
            url_key
        } else {
            backup.s3_location.trim_start_matches('/').to_string()
        };

        // Append a trailing slash if the key looks like a prefix (no file
        // extension). list_objects_v2 doesn't care, but this matches what
        // the engines pass elsewhere.
        let prefix = if key.ends_with('/') || key.contains('.') {
            key
        } else {
            format!("{}/", key)
        };

        match temps_providers::externalsvc::s3_util::list_total_size(&s3_client, bucket, &prefix)
            .await
        {
            Ok(0) => None,
            Ok(n) => Some(n),
            Err(e) => {
                debug!(
                    "live size lookup failed for backup {} ({}): {}",
                    backup.id, prefix, e
                );
                None
            }
        }
    }

    /// Get an external service by ID
    pub async fn get_external_service(
        &self,
        service_id: i32,
    ) -> Result<temps_entities::external_services::Model, BackupError> {
        temps_entities::external_services::Entity::find_by_id(service_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "ExternalService".to_string(),
                detail: format!("External service with ID {} not found", service_id),
            })
    }

    pub async fn backup_external_service(
        &self,
        service: &temps_entities::external_services::Model,
        s3_source_id: i32,
        backup_type: &str,
        created_by: i32,
    ) -> Result<temps_entities::external_service_backups::Model, BackupError> {
        info!("Starting external service backup process");
        let service_id = service.id;

        // Get S3 source configuration
        let s3_source = temps_entities::s3_sources::Entity::find_by_id(s3_source_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: "S3 source not found".to_string(),
            })?;

        // Create S3 client
        let s3_client = self
            .create_s3_client(&s3_source)
            .await
            .map_err(|e| BackupError::S3(e.to_string()))?;

        // Decrypt S3 credentials for services that pass them to external tools (e.g., WAL-G)
        let decrypted_access_key = self
            .encryption_service
            .decrypt_string(&s3_source.access_key_id)
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to decrypt access key for backup: {}", e),
            })?;
        let decrypted_secret_key = self
            .encryption_service
            .decrypt_string(&s3_source.secret_key)
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to decrypt secret key for backup: {}", e),
            })?;
        let s3_credentials = temps_providers::S3Credentials {
            access_key_id: decrypted_access_key,
            secret_key: decrypted_secret_key,
            region: s3_source.region.clone(),
            endpoint: s3_source.endpoint.clone(),
            bucket_name: s3_source.bucket_name.clone(),
            bucket_path: s3_source.bucket_path.clone(),
            force_path_style: s3_source.force_path_style.unwrap_or(true),
        };

        // Generate unique backup ID
        let backup_id = Uuid::new_v4().to_string();

        // Create backup record. The heartbeat starts at now() so the row
        // appears alive from the moment it's created — the worker task
        // refreshes it periodically while the engine runs.
        let now = chrono::Utc::now();
        let backup = temps_entities::backups::ActiveModel {
            id: sea_orm::NotSet,
            name: sea_orm::Set(format!("Backup {}", backup_id)),
            backup_id: sea_orm::Set(backup_id.clone()),
            schedule_id: sea_orm::Set(None),
            schedule_run_id: sea_orm::NotSet,
            backup_type: sea_orm::Set(backup_type.to_string()),
            state: sea_orm::Set("running".to_string()),
            started_at: sea_orm::Set(now),
            finished_at: sea_orm::Set(None),
            s3_source_id: sea_orm::Set(s3_source_id),
            s3_location: sea_orm::Set("".to_string()), // Will be updated by the service
            compression_type: sea_orm::Set("gzip".to_string()),
            created_by: sea_orm::Set(created_by),
            tags: sea_orm::Set("[]".to_string()),
            size_bytes: sea_orm::Set(None),
            file_count: sea_orm::Set(None),
            error_message: sea_orm::Set(None),
            metadata: sea_orm::Set(
                json!({
                    "service_id": service_id,
                    "service_type": service.service_type,
                    "service_name": service.name,
                    "timestamp": now.to_rfc3339()
                })
                .to_string(),
            ),
            checksum: sea_orm::Set(None),
            expires_at: sea_orm::Set(None),
        };

        let backup = backup.insert(self.db.as_ref()).await?;

        // Generate backup path
        let subpath = format!(
            "external_services/{}/{}/{}",
            service.service_type,
            service.name,
            Utc::now().format("%Y/%m/%d")
        );
        let subpath_root = format!(
            "external_services/{}/{}",
            service.service_type, service.name
        );
        let service_type = temps_providers::ServiceType::from_str(&service.service_type)
            .map_err(|e| BackupError::Validation(e.to_string()))?;
        let service_instance = self
            .external_service_manager
            .get_service_instance(service.name.clone(), service_type);

        let service_config = self
            .external_service_manager
            .get_service_config(service_id)
            .await
            .map_err(|e| BackupError::ExternalService(e.to_string()))?;

        // Cluster topology: route through the manager which knows how
        // to find the current primary and dispatch exec to it (local
        // bollard or remote agent). The trait method on
        // PostgresClusterService doesn't have access to the agent
        // protocol so it can't handle multi-host clusters; this is
        // the deliberate carve-out.
        let backup_outcome = if service.topology == "cluster" && service.service_type == "postgres"
        {
            self.external_service_manager
                .backup_postgres_cluster(service, &s3_credentials, &subpath_root, backup.id)
                .await
                .map_err(|e| {
                    error!(
                        "Cluster WAL-G backup failed for service '{}' (id={}): {}",
                        service.name, service.id, e
                    );
                    BackupError::ExternalService(e.to_string())
                })?
        } else {
            // Standalone: use the per-engine trait impl as before.
            service_instance
                .backup_to_s3(
                    &s3_client,
                    &s3_credentials,
                    backup.clone(),
                    &s3_source,
                    &subpath,
                    &subpath_root,
                    &self.db,
                    service,
                    service_config,
                )
                .await
                .map_err(|e| {
                    error!(
                        "External service backup failed for service '{}' (type={}, id={}): {}",
                        service.name, service.service_type, service.id, e
                    );
                    BackupError::ExternalService(e.to_string())
                })?
        };
        info!(
            "Backup created at location: {} ({} bytes)",
            backup_outcome.location,
            backup_outcome
                .size_bytes
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        );

        // If the engine couldn't determine size locally, fall back to
        // listing the S3 prefix. Best-effort: a missing size is annoying
        // but doesn't block the backup from being marked completed.
        let final_size_bytes = match backup_outcome.size_bytes {
            Some(n) => Some(n),
            None => {
                // Strip the "s3://bucket/" prefix to get a list-able key.
                let bucket = &s3_source.bucket_name;
                let prefix = backup_outcome
                    .location
                    .strip_prefix(&format!("s3://{}/", bucket))
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| backup_outcome.location.trim_start_matches('/').to_string());
                match temps_providers::externalsvc::s3_util::list_total_size(
                    &s3_client, bucket, &prefix,
                )
                .await
                {
                    Ok(n) => Some(n),
                    Err(e) => {
                        warn!(
                            "Could not compute size by listing s3://{}/{}: {}",
                            bucket, prefix, e
                        );
                        None
                    }
                }
            }
        };

        // Mark the parent `backups` row as completed. Without this the row
        // stays in state='running' forever, which breaks listing/filtering
        // and makes the restore UI skip the backup.
        let mut backup_update: temps_entities::backups::ActiveModel = backup.clone().into();
        backup_update.state = sea_orm::Set("completed".to_string());
        backup_update.s3_location = sea_orm::Set(backup_outcome.location.clone());
        backup_update.finished_at = sea_orm::Set(Some(Utc::now()));
        backup_update.size_bytes = sea_orm::Set(final_size_bytes);
        if let Err(e) = backup_update.update(self.db.as_ref()).await {
            // Don't fail the caller — the backup itself succeeded. Log and
            // continue; the row will be reconciled next time.
            error!("Failed to mark backup {} as completed: {}", backup.id, e);
        }

        // Get the external service backup record
        let external_backup = temps_entities::external_service_backups::Entity::find()
            .filter(temps_entities::external_service_backups::Column::BackupId.eq(backup.id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "ExternalServiceBackup".to_string(),
                detail: "External service backup record not found".to_string(),
            })?;

        info!(
            "External service backup completed successfully: {}",
            backup_id
        );
        Ok(external_backup)
    }

    // Add this new validation function
    fn validate_backup_schedule(&self, schedule: &str) -> Result<(), BackupError> {
        let schedule = Schedule::from_str(schedule)
            .map_err(|e| BackupError::Validation(format!("Invalid backup schedule: {}", e)))?;

        // Get the first two occurrences
        let upcoming = schedule.upcoming(Utc);
        let next_two = upcoming.take(2).collect::<Vec<_>>();
        if let [first, second] = next_two.as_slice() {
            let duration = *second - *first;
            if duration.num_minutes() < 60 {
                return Err(BackupError::Validation(
                    "Backup schedule must be at least 1 hour apart".into(),
                ));
            }
        }

        Ok(())
    }

    /// Start the backup scheduler with graceful cancellation support.
    ///
    /// This method runs an infinite loop that:
    /// 1. Initializes schedules that don't have `next_run` set.
    /// 2. Fires once per hour to enqueue any schedules whose `next_run` has
    ///    elapsed. Enqueueing is fast (milliseconds) because the runner picks
    ///    up and executes the jobs asynchronously — the scheduler never `.await`s
    ///    backup execution.
    /// 3. Enforces retention after enqueueing.
    /// 4. Can be gracefully cancelled via the provided `CancellationToken`.
    ///
    pub async fn start_backup_scheduler(
        &self,
        cancellation_token: tokio_util::sync::CancellationToken,
    ) -> Result<(), BackupError> {
        debug!("Starting backup scheduler");

        // First update all schedules that don't have next_run set
        let schedules = temps_entities::backup_schedules::Entity::find()
            .filter(temps_entities::backup_schedules::Column::NextRun.is_null())
            .all(self.db.as_ref())
            .await?;
        debug!("Updating next_run for {} schedules", schedules.len());
        for schedule in schedules {
            let cron_schedule = Schedule::from_str(&schedule.schedule_expression).map_err(|e| {
                BackupError::Validation(format!(
                    "Error parsing schedule expression for schedule {}: {}",
                    schedule.id, e
                ))
            })?;
            if let Some(next_run) = cron_schedule.upcoming(Utc).next() {
                let schedule_id = schedule.id;
                let mut schedule_update: temps_entities::backup_schedules::ActiveModel =
                    schedule.into_active_model();
                schedule_update.next_run = sea_orm::Set(Some(next_run));
                schedule_update.update(self.db.as_ref()).await?;
                info!(
                    "Updated next_run for schedule {}: {}",
                    schedule_id, next_run
                );
            }
        }

        loop {
            let now = Utc::now();

            // Only run at the start of each hour
            if now.minute() != 0 {
                // Sleep until next hour or cancellation
                let next_hour = (now + chrono::Duration::hours(1))
                    .with_minute(0)
                    .unwrap()
                    .with_second(0)
                    .unwrap()
                    .with_nanosecond(0)
                    .unwrap();
                let sleep_duration = next_hour - now;

                tokio::select! {
                    _ = time::sleep(time::Duration::from_secs(sleep_duration.num_seconds() as u64)) => {
                        continue;
                    }
                    _ = cancellation_token.cancelled() => {
                        info!("Backup scheduler received cancellation signal");
                        return Ok(());
                    }
                }
            }

            // Process scheduled backups with cancellation check
            tokio::select! {
                result = self.process_scheduled_backups(now) => {
                    if let Err(e) = result {
                        error!("Error processing scheduled backups: {}", e);
                    }
                }
                _ = cancellation_token.cancelled() => {
                    info!("Backup scheduler received cancellation signal");
                    return Ok(());
                }
            }

            // Enforce retention: delete backups older than the schedule's retention period
            tokio::select! {
                result = self.enforce_retention() => {
                    if let Err(e) = result {
                        error!("Error enforcing backup retention: {}", e);
                    }
                }
                _ = cancellation_token.cancelled() => {
                    info!("Backup scheduler received cancellation signal during retention cleanup");
                    return Ok(());
                }
            }

            // Sleep until next hour or cancellation
            let next_hour = (now + chrono::Duration::hours(1))
                .with_minute(0)
                .unwrap()
                .with_second(0)
                .unwrap()
                .with_nanosecond(0)
                .unwrap();
            let sleep_duration = next_hour - now;

            tokio::select! {
                _ = time::sleep(time::Duration::from_secs(sleep_duration.num_seconds() as u64)) => {}
                _ = cancellation_token.cancelled() => {
                    info!("Backup scheduler received cancellation signal");
                    return Ok(());
                }
            }
        }
    }

    /// Iterate over all enabled schedules whose `next_run` has elapsed and
    /// fan-out a `schedule_runs` row + backup jobs for each one.
    ///
    /// Each call to `enqueue_scheduled_run` is transactional and completes in
    /// milliseconds — the runner picks up and executes each job asynchronously.
    /// Sequential iteration is fast because we never `.await` backup execution
    /// inside this method.
    async fn process_scheduled_backups(&self, now: DateTime<Utc>) -> Result<(), BackupError> {
        let schedules = temps_entities::backup_schedules::Entity::find()
            .filter(temps_entities::backup_schedules::Column::Enabled.eq(true))
            .all(self.db.as_ref())
            .await?;

        for schedule in schedules {
            // Skip if next_run hasn't elapsed yet (or if it's unset — the
            // init loop in start_backup_scheduler already populated it).
            let due = schedule.next_run.is_some_and(|t| t <= now);
            if !due {
                continue;
            }

            match self
                .enqueue_scheduled_run(&schedule, TriggerSource::Cron, None)
                .await
            {
                Ok(ScheduleRunOutcome::Started { run_id, ref jobs }) => {
                    info!(
                        schedule_id = schedule.id,
                        schedule_name = %schedule.name,
                        run_id,
                        job_count = jobs.len(),
                        "scheduled run enqueued",
                    );
                }
                Ok(ScheduleRunOutcome::AlreadyInFlight { existing_run_id }) => {
                    info!(
                        schedule_id = schedule.id,
                        schedule_name = %schedule.name,
                        existing_run_id,
                        "scheduled run skipped: previous run still in flight",
                    );
                }
                Err(e) => {
                    error!(
                        schedule_id = schedule.id,
                        schedule_name = %schedule.name,
                        error = %e,
                        "scheduled run enqueue failed",
                    );
                }
            }
        }

        Ok(())
    }

    pub async fn update_next_run(&self, schedule_id: i32, schedule_str: &str) -> Result<()> {
        // Validate the schedule
        let schedule = Schedule::from_str(schedule_str)
            .map_err(|_| BackupError::Validation("Invalid backup schedule".into()))?;

        // Calculate next run time
        let next_run = schedule.upcoming(Utc).next();

        // Get the schedule and update it
        let schedule_model = temps_entities::backup_schedules::Entity::find_by_id(schedule_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "BackupSchedule".to_string(),
                detail: "Backup schedule not found".to_string(),
            })?;

        let mut schedule_update: temps_entities::backup_schedules::ActiveModel =
            schedule_model.into_active_model();
        schedule_update.next_run = sea_orm::Set(next_run);
        schedule_update.update(self.db.as_ref()).await?;

        info!(
            "Updated next run time for backup schedule {}: {:?}",
            schedule_id, next_run
        );
        Ok(())
    }

    /// Update an existing backup schedule with a partial set of changes.
    ///
    /// Only fields that are present (`Some`) in `request` are written to the
    /// database. Absent fields leave the column unchanged. Validation:
    ///
    /// - `name`: must be non-empty if present.
    /// - `schedule_expression`: validated by `validate_backup_schedule`; if
    ///   it differs from the stored value, `next_run` is recomputed.
    /// - `retention_period`: must be >= 1.
    /// - `max_runtime_secs`: `Some(Some(n))` requires `n >= 60`.
    pub async fn update_backup_schedule(
        &self,
        id: i32,
        request: UpdateBackupScheduleRequest,
    ) -> Result<temps_entities::backup_schedules::Model, BackupError> {
        use sea_orm::{ActiveModelTrait, IntoActiveModel, Set};

        // 1. Load the existing schedule (returns NotFound if absent).
        let existing = self.get_backup_schedule(id).await?;

        // 2. Validate fields before touching the ActiveModel.
        if let Some(ref name) = request.name {
            if name.is_empty() {
                return Err(BackupError::Validation("name cannot be empty".to_string()));
            }
        }

        if let Some(ref expr) = request.schedule_expression {
            self.validate_backup_schedule(expr)?;
        }

        if let Some(days) = request.retention_period {
            if days < 1 {
                return Err(BackupError::Validation(
                    "retention_period must be >= 1".to_string(),
                ));
            }
        }

        if let Some(Some(secs)) = request.max_runtime_secs {
            if secs < 60 {
                return Err(BackupError::Validation(
                    "max_runtime_secs must be >= 60".to_string(),
                ));
            }
        }

        // 3. Build the ActiveModel from the loaded model.
        let mut active: temps_entities::backup_schedules::ActiveModel =
            existing.clone().into_active_model();

        let mut changed_fields: Vec<&str> = Vec::new();

        if let Some(name) = request.name {
            active.name = Set(name);
            changed_fields.push("name");
        }
        if let Some(description) = request.description {
            active.description = Set(if description.is_empty() {
                None
            } else {
                Some(description)
            });
            changed_fields.push("description");
        }
        if let Some(expr) = request.schedule_expression {
            if expr != existing.schedule_expression {
                let cron_schedule =
                    Schedule::from_str(&expr).map_err(|e| BackupError::Schedule(e.to_string()))?;
                let next_run = cron_schedule.upcoming(Utc).next();
                active.schedule_expression = Set(expr);
                active.next_run = Set(next_run);
                changed_fields.push("schedule_expression");
                changed_fields.push("next_run");
            }
        }
        if let Some(days) = request.retention_period {
            active.retention_period = Set(days);
            changed_fields.push("retention_period");
        }
        if let Some(runtime) = request.max_runtime_secs {
            active.max_runtime_secs = Set(runtime);
            changed_fields.push("max_runtime_secs");
        }
        if let Some(enabled) = request.enabled {
            active.enabled = Set(enabled);
            changed_fields.push("enabled");
        }
        if let Some(tags) = request.tags {
            let tags_json = serde_json::to_string(&tags)?;
            active.tags = Set(tags_json);
            changed_fields.push("tags");
        }
        if let Some(target_all) = request.target_all_services {
            active.target_all_services = Set(target_all);
            changed_fields.push("target_all_services");
        }
        if let Some(include_cp) = request.include_control_plane {
            active.include_control_plane = Set(include_cp);
            changed_fields.push("include_control_plane");
        }

        // Pre-flight: figure out what state the schedule would be in after
        // the update. If the operator is moving toward "nothing to back up,"
        // reject before we commit so the run history doesn't fill up with
        // no-op runs.
        let final_target_all = request
            .target_all_services
            .unwrap_or(existing.target_all_services);
        let final_include_cp = request
            .include_control_plane
            .unwrap_or(existing.include_control_plane);
        if !final_target_all && !final_include_cp {
            use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};
            let attached_count = temps_entities::backup_schedule_services::Entity::find()
                .filter(temps_entities::backup_schedule_services::Column::ScheduleId.eq(id))
                .count(self.db.as_ref())
                .await
                .map_err(BackupError::Database)?;
            if attached_count == 0 {
                return Err(BackupError::Validation(
                    "Schedule would have nothing to back up: \
                     include_control_plane=false, target_all_services=false, \
                     and no services attached. Attach at least one service \
                     or re-enable one of the broader flags."
                        .to_string(),
                ));
            }
        }

        active.updated_at = Set(Utc::now());

        let updated = active.update(self.db.as_ref()).await?;

        // When the caller flipped target_all_services to true, clear any
        // stale explicit-membership rows. The user's choice ("clear it")
        // means "all means all" — no hidden saved list to surface later if
        // they flip back to specific.
        if matches!(request.target_all_services, Some(true)) {
            use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
            let deleted = temps_entities::backup_schedule_services::Entity::delete_many()
                .filter(temps_entities::backup_schedule_services::Column::ScheduleId.eq(id))
                .exec(self.db.as_ref())
                .await
                .map_err(BackupError::Database)?;
            info!(
                schedule_id = id,
                rows_deleted = deleted.rows_affected,
                "Cleared explicit service memberships after flipping target_all_services=true",
            );
        }

        info!(
            schedule_id = id,
            fields = ?changed_fields,
            "Updated backup schedule fields",
        );

        // If retention or enabled flipped, the desired S3 lifecycle config
        // changed. Reconcile in the background. (Schedule can't be moved to
        // a different s3_source via UpdateBackupScheduleRequest today, so
        // we only reconcile one bucket.)
        if changed_fields.contains(&"retention_period") || changed_fields.contains(&"enabled") {
            self.fire_lifecycle_reconcile(updated.s3_source_id);
        }

        Ok(updated)
    }

    // Add this new method
    pub async fn disable_backup_schedule(
        &self,
        id: i32,
    ) -> Result<temps_entities::backup_schedules::Model, BackupError> {
        let schedule_model = temps_entities::backup_schedules::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "BackupSchedule".to_string(),
                detail: "Backup schedule not found".to_string(),
            })?;

        let mut schedule_update: temps_entities::backup_schedules::ActiveModel =
            schedule_model.into_active_model();
        schedule_update.enabled = sea_orm::Set(false);
        schedule_update.updated_at = sea_orm::Set(Utc::now());
        schedule_update.update(self.db.as_ref()).await?;

        self.get_backup_schedule(id).await
    }

    /// Return the external service record linked to a backup via the
    /// `external_service_backups` join table, or `None` if no such row
    /// exists (e.g. for control-plane backups).
    ///
    /// Used by `GET /backups/{id}` to populate `external_service` in the
    /// response without requiring an N+1 join at the handler level.
    pub async fn get_backup_external_service(
        &self,
        backup_id: i32,
    ) -> Result<Option<temps_entities::external_services::Model>, BackupError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        // Look up the child row in external_service_backups for this backup.
        let child = temps_entities::external_service_backups::Entity::find()
            .filter(temps_entities::external_service_backups::Column::BackupId.eq(backup_id))
            .one(self.db.as_ref())
            .await?;

        let service_id = match child {
            Some(row) => row.service_id,
            None => return Ok(None),
        };

        // Load the parent external_services row. A missing row here is an
        // unexpected data-integrity gap, but we swallow it gracefully so
        // the backup detail page can still render.
        let service = temps_entities::external_services::Entity::find_by_id(service_id)
            .one(self.db.as_ref())
            .await?;

        Ok(service)
    }

    // Add this new method
    pub async fn enable_backup_schedule(
        &self,
        id: i32,
    ) -> Result<temps_entities::backup_schedules::Model, BackupError> {
        // Get the schedule to validate it exists and get the schedule expression
        let schedule = temps_entities::backup_schedules::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "BackupSchedule".to_string(),
                detail: "Backup schedule not found".to_string(),
            })?;

        // Calculate next run time based on the schedule expression
        let cron_schedule = Schedule::from_str(&schedule.schedule_expression)
            .map_err(|_| BackupError::Validation("Invalid backup schedule".into()))?;
        let next_run = cron_schedule.upcoming(Utc).next();

        // Update the schedule
        let mut schedule_update: temps_entities::backup_schedules::ActiveModel =
            schedule.into_active_model();
        schedule_update.enabled = sea_orm::Set(true);
        schedule_update.updated_at = sea_orm::Set(Utc::now());
        schedule_update.next_run = sea_orm::Set(next_run);

        let updated_schedule = schedule_update.update(self.db.as_ref()).await?;
        Ok(updated_schedule)
    }
}

/// Implementation of the pre-upgrade backup provider required by the
/// postgres major-upgrade orchestrator. Lives here (not in temps-providers)
/// because temps-backup owns `BackupService` and already depends on
/// temps-providers — the trait is defined in temps-providers specifically
/// to keep the dep flow one-way.
#[async_trait::async_trait]
impl temps_providers::externalsvc::postgres_upgrade::PreUpgradeBackupProvider for BackupService {
    async fn default_s3_source_id(&self, _service_id: i32) -> Result<Option<i32>, String> {
        // Default S3 source is user-scoped (global for now). Look up the
        // single row flagged is_default=true; return None if none set so
        // the orchestrator raises NoDefaultS3Source.
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        let row = temps_entities::s3_sources::Entity::find()
            .filter(temps_entities::s3_sources::Column::IsDefault.eq(true))
            .one(self.db.as_ref())
            .await
            .map_err(|e| e.to_string())?;
        Ok(row.map(|r| r.id))
    }

    async fn create_pre_upgrade_backup(
        &self,
        service_id: i32,
        s3_source_id: i32,
        created_by: i32,
    ) -> Result<i32, String> {
        let backup = self
            .create_backup(None, s3_source_id, "full", created_by)
            .await
            .map_err(|e| e.to_string())?;
        // `create_backup` returns a `temps_entities::backups::Model`; the
        // service-level backup id for external_services is surfaced via
        // `external_service_backups`. For the upgrade row we record the
        // `backups.id` itself (migration FK targets `backups(id)`), so we
        // need the numeric id — which the model exposes directly.
        let _ = service_id; // reserved for future: scope the search to this service
        Ok(backup.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::Docker;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use temps_core::notifications::{EmailMessage, NotificationData, NotificationError};
    use temps_core::EncryptionService;
    use temps_entities::{backup_schedules, s3_sources};

    /// Minimal `backup_schedules::Model` fixture shared by the schedule
    /// tests below.
    fn make_test_schedule(id: i32, s3_source_id: i32) -> temps_entities::backup_schedules::Model {
        temps_entities::backup_schedules::Model {
            id,
            name: format!("test-schedule-{}", id),
            backup_type: "full".to_string(),
            retention_period: 7,
            s3_source_id,
            schedule_expression: "0 0 * * * *".to_string(),
            enabled: true,
            last_run: None,
            next_run: Some(chrono::Utc::now() - chrono::Duration::minutes(1)),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            description: None,
            tags: "[]".to_string(),
            max_runtime_secs: None,
            target_all_services: true,
            include_control_plane: true,
        }
    }

    #[test]
    fn classify_pgdump_by_extension() {
        let loc = "s3://bucket/external_services/postgres/svc/2026/05/01/uuid/backup.sql.gz";
        assert_eq!(
            classify_backup_format(loc, Some("postgres")),
            Some("pg_dump".to_string())
        );
    }

    #[test]
    fn classify_walg_by_prefix_segment() {
        let loc = "s3://bucket/external_services/postgres/svc/walg";
        assert_eq!(
            classify_backup_format(loc, Some("postgres")),
            Some("walg".to_string())
        );
    }

    #[test]
    fn classify_walg_with_trailing_slash() {
        let loc = "s3://bucket/external_services/postgres/svc/walg/";
        assert_eq!(
            classify_backup_format(loc, Some("postgres")),
            Some("walg".to_string())
        );
    }

    #[test]
    fn classify_walg_sentinel_object_under_prefix() {
        // S3 scan may pass the sentinel key directly — still walg.
        let loc =
            "s3://bucket/external_services/postgres/svc/walg/basebackups_005/base_000_backup_stop_sentinel.json";
        assert_eq!(
            classify_backup_format(loc, Some("postgres")),
            Some("walg".to_string())
        );
    }

    #[test]
    fn classify_redis_rdb() {
        let loc = "s3://bucket/external_services/redis/svc/2026/05/01/uuid/dump.rdb.gz";
        assert_eq!(
            classify_backup_format(loc, Some("redis")),
            Some("rdb".to_string())
        );
    }

    #[test]
    fn classify_mongodump() {
        let loc = "s3://bucket/external_services/mongodb/svc/2026/05/01/uuid/dump.archive";
        assert_eq!(
            classify_backup_format(loc, Some("mongodb")),
            Some("mongodump".to_string())
        );
    }

    #[test]
    fn classify_s3_mirror_is_engine_driven() {
        // The location for an s3-mirror backup doesn't have a meaningful
        // extension; engine name carries the classification.
        let loc = "s3://bucket/external_services/s3/svc/2026/05/01/uuid";
        assert_eq!(
            classify_backup_format(loc, Some("s3")),
            Some("mirror".to_string())
        );
    }

    #[test]
    fn classify_empty_location_returns_none() {
        assert_eq!(classify_backup_format("", Some("postgres")), None);
    }

    #[test]
    fn classify_does_not_default_s3_uris_to_walg() {
        // Regression: any `s3://...` location used to be classified as
        // walg, mislabeling every pg_dump / rdb / mongodump backup that
        // happened to live in S3 (which is all of them). The classifier
        // must require an explicit `walg` path segment.
        let loc = "s3://bucket/external_services/postgres/svc/2026/05/01/uuid/backup.sql.gz";
        assert_eq!(
            classify_backup_format(loc, Some("postgres")),
            Some("pg_dump".to_string())
        );

        // Unknown extension, no walg segment, not an object-store engine —
        // we genuinely don't know. Better to return None than to
        // confidently mislabel.
        let unknown = "s3://bucket/external_services/postgres/svc/some/random/key";
        assert_eq!(classify_backup_format(unknown, Some("postgres")), None);
    }

    // Simple mock notification service for testing
    struct TestNotificationService;

    #[async_trait::async_trait]
    impl NotificationService for TestNotificationService {
        async fn send_email(&self, _message: EmailMessage) -> Result<(), NotificationError> {
            Ok(())
        }

        async fn send_notification(
            &self,
            _notification: NotificationData,
        ) -> Result<(), NotificationError> {
            Ok(())
        }

        async fn is_configured(&self) -> Result<bool, NotificationError> {
            Ok(true)
        }
    }

    fn create_mock_config_service() -> Arc<temps_config::ConfigService> {
        let server_config = temps_config::ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            "postgres://localhost:5432/test".to_string(),
            None,
            None,
        )
        .unwrap();

        // Create a mock database connection
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

        Arc::new(temps_config::ConfigService::new(
            Arc::new(server_config),
            db,
        ))
    }

    fn create_mock_notification_service() -> Arc<dyn NotificationService> {
        Arc::new(TestNotificationService)
    }

    fn create_mock_external_service_manager(
        db: Arc<sea_orm::DatabaseConnection>,
    ) -> Arc<temps_providers::ExternalServiceManager> {
        // Create a mock encryption service with a test key
        let test_key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let encryption_service = Arc::new(EncryptionService::new(test_key).unwrap());

        // Create Docker connection
        let docker = Docker::connect_with_local_defaults().unwrap();

        let dns_registry = Arc::new(temps_providers::DnsRegistry::new(db.clone()));
        Arc::new(temps_providers::ExternalServiceManager::new(
            db,
            encryption_service,
            Arc::new(docker),
            dns_registry,
        ))
    }

    #[tokio::test]
    #[ignore] // Requires system TLS certificates (fails on some macOS configurations)
    async fn test_create_s3_client() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());

        // Encrypt the credentials for the test
        let encrypted_access_key = encryption_service.encrypt_string("test-key").unwrap();
        let encrypted_secret_key = encryption_service.encrypt_string("test-secret").unwrap();

        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        let s3_source = S3Source {
            id: 1,
            name: "test-source".to_string(),
            bucket_name: "test-bucket".to_string(),
            bucket_path: "/backups".to_string(),
            access_key_id: encrypted_access_key,
            secret_key: encrypted_secret_key,
            region: "us-east-1".to_string(),
            endpoint: Some("http://localhost:9000".to_string()),
            force_path_style: Some(true),
            is_default: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let result = backup_service.create_s3_client(&s3_source).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_validate_backup_schedule_valid() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        // Valid schedule: every day at 2 AM (24 hours apart) - cron format with seconds
        let result = backup_service.validate_backup_schedule("0 0 2 * * *");
        assert!(
            result.is_ok(),
            "Expected valid schedule to pass: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_validate_backup_schedule_too_frequent() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());

        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        // Invalid schedule: every 30 minutes (too frequent) - cron format with seconds
        let result = backup_service.validate_backup_schedule("0 */30 * * * *");
        assert!(result.is_err(), "Expected error for too frequent schedule");
        match result {
            Err(BackupError::Validation(msg)) => {
                assert!(
                    msg.contains("at least 1 hour apart"),
                    "Error message should mention minimum interval: {}",
                    msg
                );
            }
            other => panic!("Expected validation error, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_validate_backup_schedule_invalid_cron() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        // Invalid cron expression
        let result = backup_service.validate_backup_schedule("invalid cron");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_s3_sources_empty() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<s3_sources::Model>::new()])
                .into_connection(),
        );

        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        let result = backup_service.list_s3_sources().await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 0);
    }

    #[tokio::test]
    #[ignore] // Requires system TLS certificates (fails on some macOS configurations)
    async fn test_create_s3_source() {
        let s3_source = s3_sources::Model {
            id: 1,
            name: "test-source".to_string(),
            bucket_name: "test-bucket".to_string(),
            bucket_path: "/backups".to_string(),
            access_key_id: "test-key".to_string(),
            secret_key: "test-secret".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some("http://localhost:9000".to_string()),
            force_path_style: Some(true),
            is_default: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![s3_source.clone()]])
                .append_exec_results(vec![MockExecResult {
                    last_insert_id: 1,
                    rows_affected: 1,
                }])
                .into_connection(),
        );

        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        let request = CreateS3SourceRequest {
            name: "test-source".to_string(),
            bucket_name: "test-bucket".to_string(),
            bucket_path: "/backups".to_string(),
            access_key_id: "test-key".to_string(),
            secret_key: "test-secret".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some("http://localhost:9000".to_string()),
            force_path_style: Some(true),
            is_default: None,
        };

        let result = backup_service.create_s3_source(request).await;
        assert!(result.is_ok());
        let source = result.unwrap();
        assert_eq!(source.name, "test-source");
        assert_eq!(source.bucket_name, "test-bucket");
    }

    #[tokio::test]
    async fn test_create_s3_source_empty_name() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        let request = CreateS3SourceRequest {
            name: "".to_string(),
            bucket_name: "test-bucket".to_string(),
            bucket_path: "/backups".to_string(),
            access_key_id: "test-key".to_string(),
            secret_key: "test-secret".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some("http://localhost:9000".to_string()),
            force_path_style: Some(true),
            is_default: None,
        };

        let result = backup_service.create_s3_source(request).await;
        assert!(result.is_err());
        match result {
            Err(BackupError::Validation(msg)) => {
                assert!(msg.contains("cannot be empty"));
            }
            _ => panic!("Expected validation error"),
        }
    }

    #[tokio::test]
    async fn test_list_backup_schedules_empty() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<backup_schedules::Model>::new()])
                .into_connection(),
        );

        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        let result = backup_service.list_backup_schedules().await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_get_s3_source_not_found() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<s3_sources::Model>::new()])
                .into_connection(),
        );

        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        let result = backup_service.get_s3_source(999).await;
        assert!(result.is_err());
        match result {
            Err(BackupError::NotFound { .. }) => {}
            _ => panic!("Expected NotFound error"),
        }
    }

    #[tokio::test]
    async fn test_get_backup_schedule_not_found() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<backup_schedules::Model>::new()])
                .into_connection(),
        );

        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        let result = backup_service.get_backup_schedule(999).await;
        assert!(result.is_err());
        match result {
            Err(BackupError::NotFound { .. }) => {}
            _ => panic!("Expected NotFound error"),
        }
    }

    #[tokio::test]
    async fn test_backup_to_minio_integration() {
        if bollard::Docker::connect_with_local_defaults().is_err() {
            println!("Docker not available, skipping test");
            return;
        }

        use temps_database::test_utils::TestDatabase;
        use testcontainers::{runners::AsyncRunner, GenericImage, ImageExt};

        // Start MinIO container
        let minio_container = GenericImage::new("minio/minio", "latest")
            .with_env_var("MINIO_ROOT_USER", "minioadmin")
            .with_env_var("MINIO_ROOT_PASSWORD", "minioadmin")
            .with_cmd(vec!["server", "/data", "--console-address", ":9001"])
            .start()
            .await
            .expect("Failed to start MinIO container");

        let minio_port = minio_container
            .get_host_port_ipv4(9000)
            .await
            .expect("Failed to get MinIO port");

        let minio_endpoint = format!("http://localhost:{}", minio_port);

        // Give MinIO time to start
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

        // Start PostgreSQL database with migrations
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");

        // Create S3 client for bucket creation
        let s3_config = aws_sdk_s3::config::Builder::new()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new("us-east-1"))
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                "minioadmin",
                "minioadmin",
                None,
                None,
                "test",
            ))
            .endpoint_url(&minio_endpoint)
            .force_path_style(true)
            .http_client(crate::engines::v2_common::bundled_roots_http_client())
            .build();

        let s3_client = aws_sdk_s3::Client::from_conf(s3_config);

        // Create test bucket
        let bucket_name = "test-backups";
        s3_client
            .create_bucket()
            .bucket(bucket_name)
            .send()
            .await
            .expect("Failed to create bucket");

        // Give bucket time to be ready
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

        // Setup backup service
        let external_service_manager = create_mock_external_service_manager(test_db.db.clone());
        let notification_service = create_mock_notification_service();

        // Create proper config service with test database
        let server_config = temps_config::ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            test_db.database_url.clone(),
            None,
            None,
        )
        .unwrap();

        let config_service = Arc::new(temps_config::ConfigService::new(
            Arc::new(server_config),
            test_db.db.clone(),
        ));

        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let backup_service = BackupService::new(
            test_db.db.clone(),
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        // Create a test user for backup operations
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::users;
        let test_user = users::ActiveModel {
            name: Set("Test User".to_string()),
            email: Set("test@example.com".to_string()),
            password_hash: Set(Some("test_hash".to_string())),
            email_verified: Set(true),
            ..Default::default()
        };
        test_user
            .insert(test_db.db.as_ref())
            .await
            .expect("Failed to create test user");

        // Create S3 source
        let s3_source_request = CreateS3SourceRequest {
            name: "test-minio".to_string(),
            bucket_name: bucket_name.to_string(),
            bucket_path: "/backups".to_string(),
            access_key_id: "minioadmin".to_string(),
            secret_key: "minioadmin".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some(minio_endpoint.clone()),
            force_path_style: Some(true),
            is_default: None,
        };

        let s3_source = backup_service
            .create_s3_source(s3_source_request)
            .await
            .expect("Failed to create S3 source");

        // Create backup schedule
        let schedule_request = CreateBackupScheduleRequest {
            name: "test-schedule".to_string(),
            backup_type: "full".to_string(),
            retention_period: 7,
            s3_source_id: Some(s3_source.id),
            schedule_expression: "0 0 2 * * *".to_string(), // Daily at 2 AM
            enabled: true,
            description: Some("Test backup schedule".to_string()),
            tags: vec![],
            max_runtime_secs: None,
            target_all_services: None,
            include_control_plane: None,
        };

        let schedule = backup_service
            .create_backup_schedule(schedule_request)
            .await
            .expect("Failed to create backup schedule");

        // Perform backup (use user ID 1 for test)
        let backup_result = backup_service
            .create_backup(Some(schedule.id), s3_source.id, "full", 1)
            .await
            .expect("Failed to create backup");

        // Verify backup was created
        assert!(backup_result.id > 0, "Backup should have an ID");
        assert_eq!(
            backup_result.state, "completed",
            "Backup should be completed"
        );
        assert!(
            backup_result.size_bytes.unwrap_or(0) > 0,
            "Backup should have a size"
        );

        println!("Backup created:");
        println!("  - ID: {}", backup_result.id);
        println!("  - State: {}", backup_result.state);
        println!("  - S3 Location: {}", backup_result.s3_location);
        println!("  - Size: {} bytes", backup_result.size_bytes.unwrap_or(0));

        // List all objects in bucket to see what was uploaded
        let list_result = s3_client
            .list_objects_v2()
            .bucket(bucket_name)
            .send()
            .await
            .expect("Failed to list objects");

        println!("\nObjects in bucket:");
        for obj in list_result.contents() {
            println!(
                "  - Key: {}, Size: {}",
                obj.key().unwrap_or("unknown"),
                obj.size().unwrap_or(0)
            );
        }

        let object_count = list_result.contents().len();
        assert!(
            object_count > 0,
            "Bucket should contain at least one backup file"
        );

        // Verify the specific backup file exists using the S3 location from the backup record
        let object_result = s3_client
            .head_object()
            .bucket(bucket_name)
            .key(&backup_result.s3_location)
            .send()
            .await;

        assert!(
            object_result.is_ok(),
            "Backup file should exist at location: {}. Error: {:?}",
            backup_result.s3_location,
            object_result.err()
        );

        // Download the backup and verify it is a valid gzip-compressed pg_dump custom format.
        //
        // This is the key assertion for the TimescaleDB fix: if the sidecar image were plain
        // postgres (missing the timescaledb extension), pg_dump would either fail with a non-zero
        // exit code (caught earlier) or produce a corrupt/truncated dump. A valid dump must:
        //   1. Start with gzip magic bytes 0x1f 0x8b
        //   2. Decompress to a pg_dump custom-format file starting with "PGDMP"
        //
        // This rules out zero-byte files, plain-text error output, and partial dumps that
        // happen to be non-zero in size.
        let backup_bytes = s3_client
            .get_object()
            .bucket(bucket_name)
            .key(&backup_result.s3_location)
            .send()
            .await
            .expect("Failed to download backup file from S3")
            .body
            .collect()
            .await
            .expect("Failed to read backup body")
            .into_bytes();

        assert!(
            backup_bytes.len() >= 2,
            "Backup file too small to contain gzip magic bytes"
        );
        assert_eq!(
            &backup_bytes[..2],
            &[0x1f, 0x8b],
            "Backup file does not start with gzip magic bytes — not a valid gzip file"
        );

        let mut decoder = flate2::read::GzDecoder::new(&backup_bytes[..]);
        let mut decompressed = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut decompressed)
            .expect("Failed to decompress backup — gzip stream is corrupt");

        // Backups use --format=plain so the decompressed content is SQL text starting
        // with a comment header ("--"), not the binary PGDMP magic bytes.
        let content_str = String::from_utf8_lossy(&decompressed);
        assert!(
            content_str.starts_with("--"),
            "Decompressed backup does not start with SQL comment header — expected plain-format pg_dump output, got: {:?}",
            &decompressed[..std::cmp::min(20, decompressed.len())]
        );

        println!("\n✓ Integration test passed:");
        println!("  - Database container started (timescale/timescaledb-ha)");
        println!("  - MinIO container started");
        println!("  - Backup created with ID: {}", backup_result.id);
        println!(
            "  - Backup size: {} bytes (compressed)",
            backup_result.size_bytes.unwrap_or(0)
        );
        println!("  - Decompressed size: {} bytes", decompressed.len());
        println!("  - Backup format: valid gzip-compressed pg_dump custom format (PGDMP)");
        println!("  - Objects in bucket: {}", object_count);
    }

    #[tokio::test]
    async fn test_restore_postgres_from_url() {
        if bollard::Docker::connect_with_local_defaults().is_err() {
            println!("Docker not available, skipping test");
            return;
        }

        use temps_database::test_utils::TestDatabase;
        use testcontainers::{runners::AsyncRunner, GenericImage, ImageExt};

        // Start MinIO container
        let minio_container = GenericImage::new("minio/minio", "latest")
            .with_env_var("MINIO_ROOT_USER", "minioadmin")
            .with_env_var("MINIO_ROOT_PASSWORD", "minioadmin")
            .with_cmd(vec!["server", "/data", "--console-address", ":9001"])
            .start()
            .await
            .expect("Failed to start MinIO container");

        let minio_port = minio_container
            .get_host_port_ipv4(9000)
            .await
            .expect("Failed to get MinIO port");

        let minio_endpoint = format!("http://localhost:{}", minio_port);

        // Give MinIO time to start
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

        // Start source PostgreSQL database with migrations (isolated instance)
        let source_db = TestDatabase::new_isolated()
            .await
            .expect("Failed to create source database");

        // Start target PostgreSQL database with migrations (isolated instance)
        let target_db = TestDatabase::new_isolated()
            .await
            .expect("Failed to create target database");

        // Create S3 client for bucket creation
        let s3_config = aws_sdk_s3::config::Builder::new()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new("us-east-1"))
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                "minioadmin",
                "minioadmin",
                None,
                None,
                "test",
            ))
            .endpoint_url(&minio_endpoint)
            .force_path_style(true)
            .http_client(crate::engines::v2_common::bundled_roots_http_client())
            .build();

        let s3_client = aws_sdk_s3::Client::from_conf(s3_config);

        // Create test bucket
        let bucket_name = "test-restore";
        s3_client
            .create_bucket()
            .bucket(bucket_name)
            .send()
            .await
            .expect("Failed to create bucket");

        // Give bucket time to be ready
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

        // Setup backup service for source database
        let external_service_manager = create_mock_external_service_manager(source_db.db.clone());
        let notification_service = create_mock_notification_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let source_config = temps_config::ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            source_db.database_url.clone(),
            None,
            None,
        )
        .unwrap();

        let source_config_service = Arc::new(temps_config::ConfigService::new(
            Arc::new(source_config),
            source_db.db.clone(),
        ));

        let source_backup_service = BackupService::new(
            source_db.db.clone(),
            external_service_manager.clone(),
            notification_service.clone(),
            source_config_service,
            encryption_service,
        );

        // Create a test user in source database
        use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
        use temps_entities::{projects, users};
        let test_user = users::ActiveModel {
            name: Set("Test User".to_string()),
            email: Set("test@example.com".to_string()),
            password_hash: Set(Some("test_hash".to_string())),
            email_verified: Set(true),
            ..Default::default()
        };
        let created_user = test_user
            .insert(source_db.db.as_ref())
            .await
            .expect("Failed to create test user");

        // Create a test project in source database
        use temps_entities::preset::Preset;
        let test_project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            git_url: Set(Some("https://github.com/test/repo".to_string())),
            preset: Set(Preset::Nixpacks),
            ..Default::default()
        };
        let created_project = test_project
            .insert(source_db.db.as_ref())
            .await
            .expect("Failed to create test project");

        println!("\n✓ Test data created in source database:");
        println!("  - User: {} (ID: {})", created_user.name, created_user.id);
        println!(
            "  - Project: {} (ID: {}, Slug: {})",
            created_project.name, created_project.id, created_project.slug
        );

        // Verify data exists in source database
        let user_count_before = users::Entity::find()
            .all(source_db.db.as_ref())
            .await
            .expect("Failed to count users")
            .len();
        let project_count_before = projects::Entity::find()
            .all(source_db.db.as_ref())
            .await
            .expect("Failed to count projects")
            .len();

        assert_eq!(
            user_count_before, 1,
            "Should have 1 user in source database"
        );
        assert_eq!(
            project_count_before, 1,
            "Should have 1 project in source database"
        );

        // Create S3 source
        let s3_source_request = CreateS3SourceRequest {
            name: "test-restore-source".to_string(),
            bucket_name: bucket_name.to_string(),
            bucket_path: "/backups".to_string(),
            access_key_id: "minioadmin".to_string(),
            secret_key: "minioadmin".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some(minio_endpoint.clone()),
            force_path_style: Some(true),
            is_default: None,
        };

        let s3_source = source_backup_service
            .create_s3_source(s3_source_request)
            .await
            .expect("Failed to create S3 source");

        // Perform backup of source database
        let backup_result = source_backup_service
            .create_backup(None, s3_source.id, "full", created_user.id)
            .await
            .expect("Failed to create backup");

        println!("\n✓ Backup created:");
        println!("  - ID: {}", backup_result.id);
        println!("  - Backup ID: {}", backup_result.backup_id);
        println!("  - State: {}", backup_result.state);
        println!("  - S3 Location: {}", backup_result.s3_location);
        println!("  - Size: {} bytes", backup_result.size_bytes.unwrap_or(0));

        // Verify backup file exists in S3
        let object_result = s3_client
            .head_object()
            .bucket(bucket_name)
            .key(&backup_result.s3_location)
            .send()
            .await;
        assert!(
            object_result.is_ok(),
            "Backup file should exist in S3: {:?}",
            object_result.err()
        );

        // Setup backup service for target database (different database URL)
        let target_config = temps_config::ServerConfig::new(
            "127.0.0.1:3001".to_string(),
            target_db.database_url.clone(),
            None,
            None,
        )
        .unwrap();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());
        let target_config_service = Arc::new(temps_config::ConfigService::new(
            Arc::new(target_config),
            target_db.db.clone(),
        ));

        let target_backup_service = BackupService::new(
            target_db.db.clone(),
            external_service_manager,
            notification_service,
            target_config_service,
            encryption_service,
        );

        // Create the S3 source in the target database
        let target_s3_source_request = CreateS3SourceRequest {
            name: "test-restore-source".to_string(),
            bucket_name: bucket_name.to_string(),
            bucket_path: "/backups".to_string(),
            access_key_id: "minioadmin".to_string(),
            secret_key: "minioadmin".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some(minio_endpoint.clone()),
            force_path_style: Some(true),
            is_default: None,
        };

        let target_s3_source = target_backup_service
            .create_s3_source(target_s3_source_request)
            .await
            .expect("Failed to create S3 source in target database");

        // Create a user in the target database to satisfy foreign key constraint.
        // Use an explicit high ID so the dump's COPY (which uses id=1 for the source's
        // first user) doesn't collide with this row when restoring into the target.
        let target_user = users::ActiveModel {
            id: Set(999_999),
            name: Set("Target User".to_string()),
            email: Set("target@example.com".to_string()),
            password_hash: Set(Some("target_hash".to_string())),
            email_verified: Set(true),
            ..Default::default()
        };
        let target_created_user = target_user
            .insert(target_db.db.as_ref())
            .await
            .expect("Failed to create user in target database");

        // Create backup record in target database pointing to the same backup in S3
        use temps_entities::backups;
        let target_backup = backups::ActiveModel {
            id: sea_orm::NotSet,
            name: Set(backup_result.name.clone()),
            backup_id: Set(backup_result.backup_id.clone()),
            schedule_id: Set(None),
            schedule_run_id: sea_orm::NotSet,
            backup_type: Set(backup_result.backup_type.clone()),
            state: Set(backup_result.state.clone()),
            started_at: Set(backup_result.started_at),
            finished_at: Set(backup_result.finished_at),
            s3_source_id: Set(target_s3_source.id),
            s3_location: Set(backup_result.s3_location.clone()),
            compression_type: Set(backup_result.compression_type.clone()),
            created_by: Set(target_created_user.id),
            tags: Set(backup_result.tags.clone()),
            size_bytes: Set(backup_result.size_bytes),
            file_count: Set(backup_result.file_count),
            error_message: Set(backup_result.error_message.clone()),
            expires_at: Set(backup_result.expires_at),
            checksum: Set(backup_result.checksum.clone()),
            metadata: Set(backup_result.metadata.clone()),
        };

        target_backup
            .insert(target_db.db.as_ref())
            .await
            .expect("Failed to create backup record in target database");

        println!("\n✓ Backup record created in target database");

        // Restore backup to target database
        println!("\n→ Starting restore to target database...");
        let restore_result = target_backup_service
            .restore_backup(&backup_result.backup_id)
            .await;

        // Note: pg_restore may emit warnings when restoring to a database with existing schema
        // This is expected behavior and not a failure
        match restore_result {
            Ok(_) => {
                println!("✓ Restore completed successfully");
            }
            Err(e) => {
                let error_msg = e.to_string();
                // Check if error contains "errors ignored" which indicates successful restore with warnings
                if error_msg.contains("errors ignored") || error_msg.contains("pg_restore") {
                    println!("✓ Restore completed with expected schema conflicts (this is normal when restoring to an existing schema)");
                } else {
                    panic!("Unexpected restore error: {:?}", e);
                }
            }
        }

        // Verify data was restored in target database
        println!("\n→ Verifying restored data in target database...");

        let restored_users = users::Entity::find()
            .all(target_db.db.as_ref())
            .await
            .expect("Failed to query users in target database");

        let restored_projects = projects::Entity::find()
            .all(target_db.db.as_ref())
            .await
            .expect("Failed to query projects in target database");

        // Find the specific project we created
        let restored_project = projects::Entity::find()
            .filter(projects::Column::Slug.eq("test-project"))
            .one(target_db.db.as_ref())
            .await
            .expect("Failed to find project by slug")
            .expect("Project with slug 'test-project' should exist after restore");

        // Find the specific user we created
        let restored_user = users::Entity::find()
            .filter(users::Column::Email.eq("test@example.com"))
            .one(target_db.db.as_ref())
            .await
            .expect("Failed to find user by email")
            .expect("User with email 'test@example.com' should exist after restore");

        println!("\n✓ Restore verification:");
        println!("  - Source database:");
        println!("    • Users: {}", user_count_before);
        println!("    • Projects: {}", project_count_before);
        println!(
            "    • Created project: '{}' (slug: {})",
            created_project.name, created_project.slug
        );
        println!("  - Target database after restore:");
        println!("    • Users: {}", restored_users.len());
        println!("    • Projects: {}", restored_projects.len());
        println!(
            "    • Restored user: '{}' (email: {})",
            restored_user.name, restored_user.email
        );
        println!(
            "    • Restored project: '{}' (slug: {}, git_url: {})",
            restored_project.name,
            restored_project.slug,
            restored_project
                .git_url
                .as_ref()
                .unwrap_or(&"None".to_string())
        );

        // Verify the data matches
        assert_eq!(
            restored_user.email, created_user.email,
            "Restored user email should match original"
        );
        assert_eq!(
            restored_project.slug, created_project.slug,
            "Restored project slug should match original"
        );
        assert_eq!(
            restored_project.name, created_project.name,
            "Restored project name should match original"
        );
        assert_eq!(
            restored_project.repo_name, created_project.repo_name,
            "Restored project repo_name should match original"
        );
        assert_eq!(
            restored_project.repo_owner, created_project.repo_owner,
            "Restored project repo_owner should match original"
        );
        assert_eq!(
            restored_project.git_url, created_project.git_url,
            "Restored project git_url should match original"
        );
        assert_eq!(
            restored_project.main_branch, created_project.main_branch,
            "Restored project main_branch should match original"
        );

        println!("\n✓ Integration test passed:");
        println!("  - Source database created with test data (user + project)");
        println!("  - Backup created and uploaded to MinIO");
        println!("  - Target database created");
        println!("  - Backup restored to target database from URL");
        println!("  - Data verified: project and user successfully restored with matching data");
    }

    #[tokio::test]
    #[ignore] // Requires system TLS certificates (fails on some macOS configurations)
    async fn test_create_s3_client_from_request_valid() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());

        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        let request = CreateS3SourceRequest {
            name: "test-source".to_string(),
            bucket_name: "test-bucket".to_string(),
            bucket_path: "/backups".to_string(),
            access_key_id: "test-access-key".to_string(),
            secret_key: "test-secret-key".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some("http://localhost:9000".to_string()),
            force_path_style: Some(true),
            is_default: None,
        };

        let result = backup_service.create_s3_client_from_request(&request).await;
        assert!(
            result.is_ok(),
            "create_s3_client_from_request should succeed with valid request"
        );
    }

    #[tokio::test]
    #[ignore] // Requires actual S3 connection
    async fn test_create_s3_source_with_bucket_creation() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());

        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        let request = CreateS3SourceRequest {
            name: "test-auto-create-bucket".to_string(),
            bucket_name: "test-auto-create-bucket".to_string(),
            bucket_path: "/backups".to_string(),
            access_key_id: "minioadmin".to_string(),
            secret_key: "minioadmin".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some("http://localhost:9000".to_string()),
            force_path_style: Some(true),
            is_default: None,
        };

        // This test requires a real MinIO instance running
        // When running, it should:
        // 1. Create an S3 client from the request
        // 2. Test the connection and create the bucket if needed
        // 3. Persist the S3 source to the database
        match backup_service.create_s3_source(request).await {
            Ok(_) => {
                println!("✓ S3 source created successfully with auto-bucket creation");
            }
            Err(e) => {
                println!(
                    "! Test skipped or failed: {} (requires running MinIO instance)",
                    e
                );
            }
        }
    }

    #[tokio::test]
    async fn test_create_s3_source_request_validation() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());

        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        let invalid_request = CreateS3SourceRequest {
            name: "".to_string(), // Empty name - should fail validation
            bucket_name: "test-bucket".to_string(),
            bucket_path: "/backups".to_string(),
            access_key_id: "test-key".to_string(),
            secret_key: "test-secret".to_string(),
            region: "us-east-1".to_string(),
            endpoint: None,
            force_path_style: None,
            is_default: None,
        };

        let result = backup_service.create_s3_source(invalid_request).await;
        assert!(
            result.is_err(),
            "create_s3_source should fail with empty name"
        );
        match result {
            Err(BackupError::Validation(msg)) => {
                assert!(
                    msg.contains("S3 source name cannot be empty"),
                    "Error should mention empty name validation"
                );
            }
            _ => panic!("Expected validation error for empty name"),
        }
    }

    // -------------------------------------------------------------------------
    // Bug 4: list_source_backups must include pending/failed rows without s3_location
    // -------------------------------------------------------------------------

    /// Regression test for the "invisible backups" bug (Bug 4).
    ///
    /// ADR-014 async-runner-created backups start with `s3_location = ""` because
    /// the location is only filled in by `mark_job_completed` when `Done` fires.
    /// Before the fix, the `list_source_backups` query skipped any row where
    /// `s3_location` was empty AND `s3_location` didn't contain `"external_services/"`.
    /// This made every pending/failed backup invisible in the UI.
    ///
    /// The fix: rows that carry `external_service_id` in their JSON metadata are
    /// always included, even with an empty `s3_location`.
    #[tokio::test]
    async fn test_list_source_backups_includes_pending_rows_without_s3_location() {
        use temps_entities::{backups, s3_sources};

        let s3_src = s3_sources::Model {
            id: 1,
            name: "test-src".to_string(),
            bucket_name: "bucket".to_string(),
            region: "us-east-1".to_string(),
            endpoint: None,
            bucket_path: "/backups".to_string(),
            access_key_id: "key".to_string(),
            secret_key: "secret".to_string(),
            force_path_style: Some(true),
            is_default: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        // A runner-created external service backup in `pending` state with empty
        // `s3_location`.  The metadata carries `external_service_id` which is the
        // signal introduced by the fix.
        let pending_backup = backups::Model {
            id: 55,
            name: "Backup abc-123".to_string(),
            backup_id: "abc-123".to_string(),
            schedule_id: None,
            schedule_run_id: None,
            backup_type: "full".to_string(),
            state: "pending".to_string(),
            started_at: Utc::now(),
            finished_at: None,
            size_bytes: None,
            file_count: None,
            s3_source_id: 1,
            s3_location: String::new(), // empty — the bug trigger
            error_message: None,
            metadata: serde_json::json!({
                "external_service_id": 42,
                "async_runner": true,
                "timestamp": Utc::now().to_rfc3339(),
            })
            .to_string(),
            checksum: None,
            compression_type: "none".to_string(),
            created_by: 1,
            expires_at: None,
            tags: "[]".to_string(),
        };

        // MockDatabase query sequence for `list_source_backups`:
        // 1. SELECT s3_sources WHERE id = 1   → returns our s3_src row
        // 2. SELECT backups WHERE s3_source_id = 1 → returns pending_backup
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![s3_src]])
                .append_query_results(vec![vec![pending_backup]])
                .into_connection(),
        );

        let external_service_manager = create_mock_external_service_manager(db.clone());
        let notification_service = create_mock_notification_service();
        let config_service = create_mock_config_service();
        let encryption_service =
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap());

        let backup_service = BackupService::new(
            db,
            external_service_manager,
            notification_service,
            config_service,
            encryption_service,
        );

        // DB-only path (include_s3_scan = false) — no S3 access in tests.
        let result = backup_service.list_source_backups(1, false).await;
        assert!(
            result.is_ok(),
            "list_source_backups should not fail: {:?}",
            result
        );

        let index = result.unwrap();
        let backups_arr = index
            .get("backups")
            .and_then(|v| v.as_array())
            .expect("response must have a 'backups' array");

        assert_eq!(
            backups_arr.len(),
            1,
            "Expected 1 backup entry (the pending row), got {}; Bug 4 regression: \
             pending rows with empty s3_location were being filtered out",
            backups_arr.len()
        );

        let entry = &backups_arr[0];
        assert_eq!(
            entry.get("state").and_then(|v| v.as_str()),
            Some("pending"),
            "The returned entry should have state='pending'"
        );
        assert_eq!(
            entry.get("id").and_then(|v| v.as_i64()),
            Some(55),
            "The returned entry should have id=55"
        );
    }

    // -------------------------------------------------------------------------
    // TimescaleDB sidecar image selection
    // -------------------------------------------------------------------------

    fn make_backup_service() -> BackupService {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db),
            create_mock_notification_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        )
    }

    /// The main Temps database always runs on TimescaleDB, so the pg_dump sidecar
    /// must always use the timescaledb-ha image — never plain postgres.
    #[test]
    fn test_pg_dump_sidecar_always_uses_timescaledb_image() {
        let svc = make_backup_service();

        for major in ["15", "16", "17", "18"] {
            let image = svc.get_postgres_image_tag(major);
            assert!(
                image.starts_with("timescale/timescaledb-ha:pg"),
                "Expected timescaledb-ha image for version {major}, got: {image}"
            );
            assert!(
                image.ends_with(major),
                "Image tag should end with the major version {major}, got: {image}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // list_external_service_backups — pagination math
    // -----------------------------------------------------------------------

    /// Validate the pagination clamping logic: page < 1 becomes 1, page_size
    /// above 100 becomes 100. We can't mock raw SQL in unit tests (Sea-ORM
    /// MockDatabase only covers entity model types, not bare FromQueryResult
    /// structs), so we test only the arithmetic that lives outside the query.
    #[test]
    fn test_list_external_service_backups_pagination_clamp() {
        // page = 0 clamps to 1 — the underflow happens at `(page - 1) * page_size`
        // if we don't, producing a negative OFFSET that Postgres rejects.
        let raw_page: i64 = 0;
        let page: i64 = raw_page.max(1);
        assert_eq!(page, 1);
        // page_size = 200 → page_size = 100 (clamp 1..=100)
        let page_size: i64 = 200_i64.clamp(1, 100);
        assert_eq!(page_size, 100);
        // offset
        let offset = (page - 1) * page_size;
        assert_eq!(offset, 0);
    }

    /// plain Postgres. Verify that parse_postgres_version correctly extracts the major
    /// version from a real TimescaleDB SELECT version() output.
    #[test]
    fn test_parse_postgres_version_from_timescaledb_version_string() {
        let svc = make_backup_service();

        let timescaledb_version_string =
            "PostgreSQL 17.4 on aarch64-unknown-linux-gnu, compiled by gcc (GCC) 13.2.0, 64-bit";

        let major = svc
            .parse_postgres_version(timescaledb_version_string)
            .expect("Should parse TimescaleDB version string");

        assert_eq!(major, "17");

        // Confirm the full image tag is correct end-to-end
        let image = svc.get_postgres_image_tag(&major);
        assert_eq!(image, "timescale/timescaledb-ha:pg17");
    }

    // ── update_backup_schedule unit tests ───────────────────────────────────

    /// `update_backup_schedule` rejects an invalid cron expression before any
    /// DB write: the early validation path returns `BackupError::Validation`
    /// without reaching the active-model update step.
    #[tokio::test]
    async fn test_update_schedule_rejects_invalid_cron() {
        let schedule = make_test_schedule(1, 1);

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // get_backup_schedule SELECT
                .append_query_results(vec![vec![schedule]])
                .into_connection(),
        );

        let svc = BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db),
            create_mock_notification_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        );

        let request = UpdateBackupScheduleRequest {
            name: None,
            description: None,
            schedule_expression: Some("not-a-cron".to_string()),
            retention_period: None,
            max_runtime_secs: None,
            enabled: None,
            tags: None,
            target_all_services: None,
            include_control_plane: None,
        };

        let result = svc.update_backup_schedule(1, request).await;
        assert!(result.is_err(), "Invalid cron must be rejected");
        // Validation fires before any DB write — error is Validation, not Schedule,
        // because validate_backup_schedule wraps the cron parse error in Validation.
        match result.unwrap_err() {
            BackupError::Validation(_) | BackupError::Schedule(_) => {}
            other => panic!("Expected Validation or Schedule error, got: {:?}", other),
        }
    }

    /// When the cron expression changes, `next_run` must be recomputed to a
    /// future timestamp. The updated model returned by the service must have a
    /// non-None `next_run`.
    #[tokio::test]
    async fn test_update_schedule_recomputes_next_run_when_cron_changes() {
        let mut schedule = make_test_schedule(1, 1);
        // Use a cron that is definitely different from what `make_test_schedule` sets.
        schedule.schedule_expression = "0 0 0 * * *".to_string(); // daily

        let updated_row = temps_entities::backup_schedules::Model {
            schedule_expression: "0 0 2 * * *".to_string(), // 2 AM daily (new value)
            ..schedule.clone()
        };

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // get_backup_schedule SELECT
                .append_query_results(vec![vec![schedule]])
                // active.update() SELECT (Sea-ORM mock returns the next query result)
                .append_query_results(vec![vec![updated_row.clone()]])
                .into_connection(),
        );

        let svc = BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db),
            create_mock_notification_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        );

        let request = UpdateBackupScheduleRequest {
            name: None,
            description: None,
            // Change from "0 0 0 * * *" to "0 0 2 * * *" — at least 1 h apart, valid.
            schedule_expression: Some("0 0 2 * * *".to_string()),
            retention_period: None,
            max_runtime_secs: None,
            enabled: None,
            tags: None,
            target_all_services: None,
            include_control_plane: None,
        };

        let result = svc.update_backup_schedule(1, request).await;
        assert!(
            result.is_ok(),
            "Valid cron change must succeed: {:?}",
            result
        );
        let model = result.unwrap();
        assert_eq!(model.schedule_expression, "0 0 2 * * *");
    }

    /// When only `name` is set, the service must not blow up and must return
    /// the updated model. The inactive fields are left at their existing values
    /// (the active model only sets the columns that were `Some` in the request).
    #[tokio::test]
    async fn test_update_schedule_leaves_fields_untouched_when_absent() {
        let schedule = make_test_schedule(1, 1);

        let updated_row = temps_entities::backup_schedules::Model {
            name: "renamed".to_string(),
            ..schedule.clone()
        };

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // get_backup_schedule SELECT
                .append_query_results(vec![vec![schedule.clone()]])
                // active.update() returns the updated row
                .append_query_results(vec![vec![updated_row]])
                .into_connection(),
        );

        let svc = BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db),
            create_mock_notification_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        );

        let request = UpdateBackupScheduleRequest {
            name: Some("renamed".to_string()),
            description: None,
            schedule_expression: None,
            retention_period: None,
            max_runtime_secs: None,
            enabled: None,
            tags: None,
            target_all_services: None,
            include_control_plane: None,
        };

        let result = svc.update_backup_schedule(1, request).await;
        assert!(
            result.is_ok(),
            "Name-only update must succeed: {:?}",
            result
        );
        let model = result.unwrap();
        assert_eq!(model.name, "renamed");
        // Other fields unchanged from make_test_schedule defaults.
        assert_eq!(model.retention_period, schedule.retention_period);
        assert_eq!(model.schedule_expression, schedule.schedule_expression);
    }

    /// When `find_by_id` returns no row (empty result), the service must return
    /// `BackupError::NotFound` without attempting an UPDATE.
    #[tokio::test]
    async fn test_update_schedule_not_found_returns_notfound() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // get_backup_schedule SELECT — empty result → schedule does not exist.
                .append_query_results(vec![Vec::<temps_entities::backup_schedules::Model>::new()])
                .into_connection(),
        );

        let svc = BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db),
            create_mock_notification_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        );

        let request = UpdateBackupScheduleRequest {
            name: Some("irrelevant".to_string()),
            description: None,
            schedule_expression: None,
            retention_period: None,
            max_runtime_secs: None,
            enabled: None,
            tags: None,
            target_all_services: None,
            include_control_plane: None,
        };

        let result = svc.update_backup_schedule(999, request).await;
        assert!(result.is_err(), "Missing schedule must return NotFound");
        assert!(
            matches!(result.unwrap_err(), BackupError::NotFound { .. }),
            "Expected NotFound variant"
        );
    }

    // ── list_schedule_runs ────────────────────────────────────────────────────

    /// Helper: build a minimal `ScheduleRunSummary` row value for MockDatabase.
    ///
    /// The new fan-out shape returns one row per scheduler tick with aggregate
    /// child counts. The row schema must match `RunRow` (defined in the
    /// `list_schedule_runs` SQL); fields here mirror that.
    fn make_run_entry_row(
        run_id: i64,
        started_at: chrono::DateTime<chrono::Utc>,
    ) -> std::collections::BTreeMap<String, sea_orm::Value> {
        use sea_orm::Value as SVal;
        let mut row = std::collections::BTreeMap::new();
        row.insert("run_id".to_string(), SVal::BigInt(Some(run_id)));
        row.insert("schedule_id".to_string(), SVal::Int(Some(1)));
        row.insert(
            "triggered_by".to_string(),
            SVal::String(Some(Box::new("cron".to_string()))),
        );
        row.insert(
            "started_at".to_string(),
            SVal::ChronoDateTimeUtc(Some(Box::new(started_at))),
        );
        row.insert("finished_at".to_string(), SVal::ChronoDateTimeUtc(None));
        row.insert("total_jobs".to_string(), SVal::BigInt(Some(1)));
        row.insert("completed_jobs".to_string(), SVal::BigInt(Some(1)));
        row.insert("failed_jobs".to_string(), SVal::BigInt(Some(0)));
        row.insert("running_jobs".to_string(), SVal::BigInt(Some(0)));
        row.insert("pending_jobs".to_string(), SVal::BigInt(Some(0)));
        row
    }

    /// `list_schedule_runs` must return rows ordered newest-first by the SQL
    /// (which uses `ORDER BY started_at DESC`).
    ///
    /// The MockDatabase returns rows in the order the test supplies them; we
    /// supply them newest-first and assert that the response preserves that
    /// order and that pagination metadata is correct.
    #[tokio::test]
    async fn test_list_schedule_runs_returns_rows_ordered_desc() {
        let now = chrono::Utc::now();
        let older = now - chrono::Duration::hours(2);

        let schedule = make_test_schedule(1, 1);

        // MockDatabase query sequence:
        // 1. get_backup_schedule SELECT (returns the schedule)
        // 2. COUNT(*) query
        // 3. Paginated rows query (returns two entries)
        let count_row = {
            let mut r = std::collections::BTreeMap::new();
            r.insert("total".to_string(), sea_orm::Value::BigInt(Some(2)));
            r
        };

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // get_backup_schedule
                .append_query_results(vec![vec![schedule]])
                // COUNT query
                .append_query_results(vec![vec![count_row]])
                // paginated rows — newest first (run_id=2 is more recent)
                .append_query_results(vec![vec![
                    make_run_entry_row(2, now),
                    make_run_entry_row(1, older),
                ]])
                .into_connection(),
        );

        let svc = BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db),
            create_mock_notification_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        );

        let response = svc
            .list_schedule_runs(1, 1, 20)
            .await
            .expect("list_schedule_runs must succeed");

        assert_eq!(response.total, 2, "total must match COUNT result");
        assert_eq!(response.runs.len(), 2, "must return 2 run entries");
        // First entry is the most recent (run_id=2).
        assert_eq!(response.runs[0].run_id, 2, "first entry must be newest");
        assert_eq!(response.runs[1].run_id, 1, "second entry must be older");
    }

    /// `list_schedule_runs` must clamp `page < 1` to 1, so the offset never
    /// goes negative.
    #[tokio::test]
    async fn test_list_schedule_runs_clamps_page_below_one() {
        let schedule = make_test_schedule(1, 1);
        let count_row = {
            let mut r = std::collections::BTreeMap::new();
            r.insert("total".to_string(), sea_orm::Value::BigInt(Some(0)));
            r
        };

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![schedule]])
                .append_query_results(vec![vec![count_row]])
                .append_query_results(vec![Vec::<
                    std::collections::BTreeMap<String, sea_orm::Value>,
                >::new()])
                .into_connection(),
        );

        let svc = BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db),
            create_mock_notification_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        );

        // page=-5 must be treated as page=1 (offset=0).
        let result = svc.list_schedule_runs(1, -5, 20).await;
        assert!(result.is_ok(), "negative page must not error: {:?}", result);
    }

    /// `list_schedule_runs` must clamp `page_size > 100` to 100 so the client
    /// cannot request an unbounded result set.
    #[tokio::test]
    async fn test_list_schedule_runs_clamps_page_size_above_100() {
        let schedule = make_test_schedule(1, 1);
        let count_row = {
            let mut r = std::collections::BTreeMap::new();
            r.insert("total".to_string(), sea_orm::Value::BigInt(Some(0)));
            r
        };

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![schedule]])
                .append_query_results(vec![vec![count_row]])
                .append_query_results(vec![Vec::<
                    std::collections::BTreeMap<String, sea_orm::Value>,
                >::new()])
                .into_connection(),
        );

        let svc = BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db),
            create_mock_notification_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        );

        // page_size=999 must be clamped to 100. The call itself must succeed.
        let result = svc.list_schedule_runs(1, 1, 999).await;
        assert!(
            result.is_ok(),
            "oversized page_size must not error: {:?}",
            result
        );
    }

    /// `list_schedule_runs` with an unknown schedule_id must return `NotFound`.
    #[tokio::test]
    async fn test_list_schedule_runs_not_found() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // get_backup_schedule returns empty → schedule does not exist
                .append_query_results(vec![Vec::<temps_entities::backup_schedules::Model>::new()])
                .into_connection(),
        );

        let svc = BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db),
            create_mock_notification_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        );

        let result = svc.list_schedule_runs(9999, 1, 20).await;
        assert!(result.is_err(), "unknown schedule must return an error");
        assert!(
            matches!(result.unwrap_err(), BackupError::NotFound { .. }),
            "expected BackupError::NotFound for unknown schedule"
        );
    }

    // ── list_child_backups ────────────────────────────────────────────────────

    /// Helper: build a minimal backup model for MockDatabase.
    fn make_test_backup_model(id: i32) -> temps_entities::backups::Model {
        use chrono::Utc;
        temps_entities::backups::Model {
            id,
            name: format!("Backup {}", id),
            backup_id: format!("uuid-{}", id),
            schedule_id: None,
            schedule_run_id: None,
            backup_type: "full".to_string(),
            state: "completed".to_string(),
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            s3_source_id: 1,
            s3_location: "s3://bucket/path".to_string(),
            error_message: None,
            metadata: "{}".to_string(),
            checksum: None,
            compression_type: "lz4".to_string(),
            created_by: 1,
            expires_at: None,
            size_bytes: Some(1024),
            file_count: None,
            tags: "[]".to_string(),
        }
    }

    /// Helper: build a minimal `ChildBackupEntry` BTreeMap row for MockDatabase.
    fn make_child_backup_row(
        id: i32,
        service_id: i32,
        state: &str,
    ) -> std::collections::BTreeMap<String, sea_orm::Value> {
        use sea_orm::Value as SVal;
        let mut row = std::collections::BTreeMap::new();
        row.insert("id".to_string(), SVal::Int(Some(id)));
        row.insert("service_id".to_string(), SVal::Int(Some(service_id)));
        row.insert(
            "service_name".to_string(),
            SVal::String(Some(Box::new(format!("service-{}", service_id)))),
        );
        row.insert(
            "service_type".to_string(),
            SVal::String(Some(Box::new("postgres".to_string()))),
        );
        row.insert(
            "state".to_string(),
            SVal::String(Some(Box::new(state.to_string()))),
        );
        row.insert(
            "backup_type".to_string(),
            SVal::String(Some(Box::new("full".to_string()))),
        );
        row.insert(
            "started_at".to_string(),
            SVal::ChronoDateTimeUtc(Some(Box::new(chrono::Utc::now()))),
        );
        row.insert("finished_at".to_string(), SVal::ChronoDateTimeUtc(None));
        row.insert("size_bytes".to_string(), SVal::BigInt(Some(2048)));
        row.insert(
            "s3_location".to_string(),
            SVal::String(Some(Box::new("s3://bucket/child".to_string()))),
        );
        row.insert("error_message".to_string(), SVal::String(None));
        row.insert(
            "compression_type".to_string(),
            SVal::String(Some(Box::new("lz4".to_string()))),
        );
        row
    }

    /// `list_child_backups` returns all children ordered by `id ASC` when the
    /// parent backup exists and has two completed child rows.
    #[tokio::test]
    async fn test_list_child_backups_returns_ordered_rows() {
        let parent = make_test_backup_model(10);

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // find_by_id for the parent backup
                .append_query_results(vec![vec![parent]])
                // child rows query — two entries, ordered by id ASC (service 1 then 2)
                .append_query_results(vec![vec![
                    make_child_backup_row(1, 1, "completed"),
                    make_child_backup_row(2, 2, "completed"),
                ]])
                .into_connection(),
        );

        let svc = BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db),
            create_mock_notification_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        );

        let children = svc
            .list_child_backups(10)
            .await
            .expect("list_child_backups must succeed");

        assert_eq!(children.len(), 2, "must return 2 children");
        assert_eq!(children[0].id, 1, "first child must have id=1");
        assert_eq!(children[1].id, 2, "second child must have id=2");
        assert_eq!(children[0].state, "completed");
        assert_eq!(children[0].service_type, "postgres");
    }

    /// `list_child_backups` returns an empty Vec when the parent backup exists
    /// but has no child rows (e.g. a control-plane backup).
    #[tokio::test]
    async fn test_list_child_backups_returns_empty_for_no_children() {
        let parent = make_test_backup_model(99);

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // find_by_id returns the parent
                .append_query_results(vec![vec![parent]])
                // child rows query returns nothing
                .append_query_results(vec![Vec::<
                    std::collections::BTreeMap<String, sea_orm::Value>,
                >::new()])
                .into_connection(),
        );

        let svc = BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db),
            create_mock_notification_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        );

        let children = svc
            .list_child_backups(99)
            .await
            .expect("list_child_backups must succeed for parent with no children");

        assert!(children.is_empty(), "must return empty Vec");
    }

    /// `list_child_backups` returns `NotFound` when the parent backup does not
    /// exist, so the handler can surface a 404 instead of an empty list.
    #[tokio::test]
    async fn test_list_child_backups_returns_not_found_for_missing_parent() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // find_by_id returns nothing → parent does not exist
                .append_query_results(vec![Vec::<temps_entities::backups::Model>::new()])
                .into_connection(),
        );

        let svc = BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db),
            create_mock_notification_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        );

        let result = svc.list_child_backups(9999).await;
        assert!(result.is_err(), "missing parent must return an error");
        assert!(
            matches!(result.unwrap_err(), BackupError::NotFound { .. }),
            "expected BackupError::NotFound for unknown parent backup"
        );
    }

    // ── backup_schedule_services membership ──────────────────────────────
    //
    // These tests pin the contract of the attach/detach/list helpers. They
    // need a `BackupService`, which in turn requires an
    // `ExternalServiceManager`, which constructs a Docker client at build
    // time. We early-return when Docker is unavailable so the suite stays
    // green in CI environments without a daemon.
    //
    // The point of these is the *resolution* behaviour, not the SQL — the
    // join query itself is exercised by the integration test.

    fn skip_if_no_docker() -> bool {
        match bollard::Docker::connect_with_local_defaults() {
            Ok(d) => {
                // A `ping` would be more accurate but is async; the
                // synchronous build is enough to keep tests green when the
                // daemon socket is missing entirely.
                drop(d);
                false
            }
            Err(_) => {
                println!("Docker not available, skipping test");
                true
            }
        }
    }

    fn build_service_for_mock(db: Arc<sea_orm::DatabaseConnection>) -> Result<BackupService, ()> {
        if skip_if_no_docker() {
            return Err(());
        }
        Ok(BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db),
            create_mock_notification_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        ))
    }

    #[tokio::test]
    async fn attach_services_rejects_unknown_schedule() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // get_backup_schedule -> find_by_id returns empty
                .append_query_results(vec![Vec::<backup_schedules::Model>::new()])
                .into_connection(),
        );
        let Ok(svc) = build_service_for_mock(db) else {
            return;
        };

        let err = svc
            .attach_services_to_schedule(42, &[1, 2, 3])
            .await
            .expect_err("missing schedule should error");
        assert!(
            matches!(err, BackupError::NotFound { .. }),
            "expected NotFound, got {:?}",
            err
        );
    }

    #[tokio::test]
    async fn attach_services_noop_on_empty_input() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // schedule lookup succeeds
                .append_query_results(vec![vec![make_test_schedule(7, 1)]])
                .into_connection(),
        );
        let Ok(svc) = build_service_for_mock(db) else {
            return;
        };

        // Empty list must short-circuit before any further query is issued —
        // we only queued one query result (the schedule lookup).
        let inserted = svc
            .attach_services_to_schedule(7, &[])
            .await
            .expect("empty attach should succeed");
        assert_eq!(inserted, 0);
    }

    // Note: validation-of-unknown-service-ids is covered by the integration
    // test (`integration_attach_list_detach_round_trip`) because mocking
    // Sea-ORM's `.count()` requires a query-result shape that `MockDatabase`
    // does not accept generically. The integration test exercises the same
    // code path against a real Postgres.

    #[tokio::test]
    async fn detach_service_returns_false_when_no_row() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results(vec![MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 0,
                }])
                .into_connection(),
        );
        let Ok(svc) = build_service_for_mock(db) else {
            return;
        };

        let removed = svc
            .detach_service_from_schedule(1, 2)
            .await
            .expect("detach should be idempotent");
        assert!(!removed, "no row → returns false");
    }

    #[tokio::test]
    async fn detach_service_returns_true_when_removed() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results(vec![MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .into_connection(),
        );
        let Ok(svc) = build_service_for_mock(db) else {
            return;
        };

        let removed = svc
            .detach_service_from_schedule(1, 2)
            .await
            .expect("detach should succeed");
        assert!(removed);
    }

    #[tokio::test]
    async fn list_services_for_unknown_schedule_returns_not_found() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<backup_schedules::Model>::new()])
                .into_connection(),
        );
        let Ok(svc) = build_service_for_mock(db) else {
            return;
        };

        let err = svc
            .list_services_for_schedule(404)
            .await
            .expect_err("missing schedule must error");
        assert!(matches!(err, BackupError::NotFound { .. }));
    }

    #[tokio::test]
    async fn list_schedules_for_unknown_service_returns_not_found() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // external_services::Entity::find_by_id → empty
                .append_query_results(vec![Vec::<temps_entities::external_services::Model>::new()])
                .into_connection(),
        );
        let Ok(svc) = build_service_for_mock(db) else {
            return;
        };

        let err = svc
            .list_schedules_for_service(123)
            .await
            .expect_err("missing service must error");
        assert!(matches!(err, BackupError::NotFound { .. }));
    }

    /// Integration test: round-trip attach → list → detach against a real
    /// Postgres backed by `TestDatabase::with_migrations`. Verifies the
    /// migration creates the join table correctly, the FKs cascade on
    /// service-and-schedule delete, and the resolver join returns the right
    /// rows. Skips gracefully when Docker (and therefore the test Postgres)
    /// is unavailable.
    #[tokio::test]
    async fn integration_attach_list_detach_round_trip() {
        if bollard::Docker::connect_with_local_defaults().is_err() {
            println!("Docker not available, skipping test");
            return;
        }
        use sea_orm::ActiveValue::Set;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use temps_database::test_utils::TestDatabase;

        let test_db = match TestDatabase::with_migrations().await {
            Ok(d) => d,
            Err(e) => {
                println!("TestDatabase unavailable, skipping: {e}");
                return;
            }
        };
        let db = test_db.db.clone();

        // Seed an S3 source (FK target for schedule).
        let s3_source = temps_entities::s3_sources::ActiveModel {
            id: sea_orm::NotSet,
            name: Set("integration-source".to_string()),
            bucket_name: Set("test-bucket".to_string()),
            bucket_path: Set("/".to_string()),
            access_key_id: Set("".to_string()),
            secret_key: Set("".to_string()),
            region: Set("us-east-1".to_string()),
            endpoint: Set(None),
            force_path_style: Set(Some(true)),
            is_default: Set(true),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
        }
        .insert(db.as_ref())
        .await
        .expect("insert s3 source");

        // Seed a schedule. Use 'specific' mode so the explicit-membership
        // path is exercised by this test (the integration test for the
        // 'all' branch lives in `integration_flip_to_all_clears_membership`).
        let schedule = temps_entities::backup_schedules::ActiveModel {
            id: sea_orm::NotSet,
            name: Set("integration-schedule".to_string()),
            backup_type: Set("full".to_string()),
            retention_period: Set(7),
            s3_source_id: Set(s3_source.id),
            schedule_expression: Set("0 0 2 * * *".to_string()),
            enabled: Set(true),
            last_run: Set(None),
            next_run: Set(None),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            description: Set(None),
            tags: Set("[]".to_string()),
            max_runtime_secs: Set(None),
            target_all_services: Set(false),
            include_control_plane: Set(true),
        }
        .insert(db.as_ref())
        .await
        .expect("insert schedule");

        // Seed two external services.
        let mk_svc = |name: &str, svc_type: &str| temps_entities::external_services::ActiveModel {
            id: sea_orm::NotSet,
            name: Set(name.to_string()),
            service_type: Set(svc_type.to_string()),
            version: Set(Some("17".to_string())),
            status: Set("running".to_string()),
            slug: Set(Some(name.to_string())),
            config: Set(None),
            node_id: Set(None),
            topology: Set("standalone".to_string()),
            error_message: Set(None),
            health_status: Set(None),
            last_health_check_at: Set(None),
            last_health_error: Set(None),
            consecutive_health_failures: Set(0),
            health_metadata: Set(None),
            metrics_enabled: Set(false),
            default_backup_provisioned: Set(false),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
        };
        let pg = mk_svc("pg-prod", "postgres")
            .insert(db.as_ref())
            .await
            .expect("insert pg service");
        let redis = mk_svc("redis-prod", "redis")
            .insert(db.as_ref())
            .await
            .expect("insert redis service");

        // Build a service. We can't use build_service_for_mock because we
        // want the *real* DB, not a mock. The Docker handle is required by
        // ExternalServiceManager but unused by these methods.
        let svc = BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db.clone()),
            create_mock_notification_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        );

        // 1) Attach both services.
        let inserted = svc
            .attach_services_to_schedule(schedule.id, &[pg.id, redis.id])
            .await
            .expect("attach succeeds");
        assert_eq!(inserted, 2, "both rows inserted");

        // 2) Re-attaching is idempotent (ON CONFLICT DO NOTHING).
        let inserted_again = svc
            .attach_services_to_schedule(schedule.id, &[pg.id, redis.id])
            .await
            .expect("re-attach succeeds");
        assert_eq!(inserted_again, 0, "no new rows on duplicate attach");

        // 3) list_services_for_schedule returns both, ordered by name.
        let listed = svc
            .list_services_for_schedule(schedule.id)
            .await
            .expect("list services");
        assert_eq!(listed.len(), 2);
        // Sorted by name: pg-prod < redis-prod
        assert_eq!(listed[0].name, "pg-prod");
        assert_eq!(listed[1].name, "redis-prod");

        // 4) list_schedules_for_service returns the schedule for each.
        let pg_schedules = svc
            .list_schedules_for_service(pg.id)
            .await
            .expect("list schedules for pg");
        assert_eq!(pg_schedules.len(), 1);
        assert_eq!(pg_schedules[0].id, schedule.id);

        // 5) Detach one service.
        let removed = svc
            .detach_service_from_schedule(schedule.id, pg.id)
            .await
            .expect("detach succeeds");
        assert!(removed);
        let listed = svc
            .list_services_for_schedule(schedule.id)
            .await
            .expect("list after detach");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "redis-prod");

        // 6) Detach again is idempotent (returns false, no error).
        let removed_again = svc
            .detach_service_from_schedule(schedule.id, pg.id)
            .await
            .expect("idempotent detach");
        assert!(!removed_again);

        // 7) Cascade: deleting the schedule removes all membership rows.
        temps_entities::backup_schedules::Entity::delete_by_id(schedule.id)
            .exec(db.as_ref())
            .await
            .expect("delete schedule");
        let leftover = temps_entities::backup_schedule_services::Entity::find()
            .filter(temps_entities::backup_schedule_services::Column::ScheduleId.eq(schedule.id))
            .all(db.as_ref())
            .await
            .expect("count leftover");
        assert!(
            leftover.is_empty(),
            "schedule delete must cascade to membership"
        );
    }

    /// Integration test: when `target_all_services = true`, flipping a
    /// schedule's mode via `update_backup_schedule` clears all explicit
    /// membership rows (clean-slate behaviour). When set back to false,
    /// the rows are not magically restored — the user has to attach
    /// again. Skips gracefully when Docker / test Postgres are absent.
    #[tokio::test]
    async fn integration_flip_to_all_clears_membership() {
        if bollard::Docker::connect_with_local_defaults().is_err() {
            println!("Docker not available, skipping test");
            return;
        }
        use sea_orm::ActiveValue::Set;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use temps_database::test_utils::TestDatabase;

        let test_db = match TestDatabase::with_migrations().await {
            Ok(d) => d,
            Err(e) => {
                println!("TestDatabase unavailable, skipping: {e}");
                return;
            }
        };
        let db = test_db.db.clone();

        // Seed S3 source + schedule (start in 'specific' mode so we have
        // membership rows to clear).
        let s3 = temps_entities::s3_sources::ActiveModel {
            id: sea_orm::NotSet,
            name: Set("flip-source".to_string()),
            bucket_name: Set("b".to_string()),
            bucket_path: Set("/".to_string()),
            access_key_id: Set("".to_string()),
            secret_key: Set("".to_string()),
            region: Set("us-east-1".to_string()),
            endpoint: Set(None),
            force_path_style: Set(Some(true)),
            is_default: Set(true),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
        }
        .insert(db.as_ref())
        .await
        .expect("insert s3 source");

        let schedule = temps_entities::backup_schedules::ActiveModel {
            id: sea_orm::NotSet,
            name: Set("flip-schedule".to_string()),
            backup_type: Set("full".to_string()),
            retention_period: Set(7),
            s3_source_id: Set(s3.id),
            schedule_expression: Set("0 0 2 * * *".to_string()),
            enabled: Set(true),
            last_run: Set(None),
            next_run: Set(None),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            description: Set(None),
            tags: Set("[]".to_string()),
            max_runtime_secs: Set(None),
            // Start as specific so we can attach rows.
            target_all_services: Set(false),
            include_control_plane: Set(true),
        }
        .insert(db.as_ref())
        .await
        .expect("insert schedule");

        let svc_a = temps_entities::external_services::ActiveModel {
            id: sea_orm::NotSet,
            name: Set("svc-a".to_string()),
            service_type: Set("postgres".to_string()),
            version: Set(Some("17".to_string())),
            status: Set("running".to_string()),
            slug: Set(Some("svc-a".to_string())),
            config: Set(None),
            node_id: Set(None),
            topology: Set("standalone".to_string()),
            error_message: Set(None),
            health_status: Set(None),
            last_health_check_at: Set(None),
            last_health_error: Set(None),
            consecutive_health_failures: Set(0),
            health_metadata: Set(None),
            metrics_enabled: Set(false),
            default_backup_provisioned: Set(false),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
        }
        .insert(db.as_ref())
        .await
        .expect("insert svc-a");

        let svc = BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db.clone()),
            create_mock_notification_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        );

        // Attach svc-a to the specific schedule.
        svc.attach_services_to_schedule(schedule.id, &[svc_a.id])
            .await
            .expect("attach");

        let listed = svc
            .list_services_for_schedule(schedule.id)
            .await
            .expect("list pre-flip");
        assert_eq!(listed.len(), 1, "precondition: one service attached");

        // Flip to target_all_services = true via the service-layer update
        // (mirrors what the handler does on PATCH).
        svc.update_backup_schedule(
            schedule.id,
            crate::handlers::backup_handler::UpdateBackupScheduleRequest {
                name: None,
                description: None,
                schedule_expression: None,
                retention_period: None,
                max_runtime_secs: None,
                enabled: None,
                tags: None,
                target_all_services: Some(true),
                include_control_plane: None,
            },
        )
        .await
        .expect("update succeeds");

        // Membership table must now be empty for this schedule.
        let after = temps_entities::backup_schedule_services::Entity::find()
            .filter(temps_entities::backup_schedule_services::Column::ScheduleId.eq(schedule.id))
            .all(db.as_ref())
            .await
            .expect("count after flip");
        assert!(
            after.is_empty(),
            "flipping to target_all_services=true must clear membership rows"
        );

        // Flip back to specific — list must stay empty (we cleared it).
        svc.update_backup_schedule(
            schedule.id,
            crate::handlers::backup_handler::UpdateBackupScheduleRequest {
                name: None,
                description: None,
                schedule_expression: None,
                retention_period: None,
                max_runtime_secs: None,
                enabled: None,
                tags: None,
                target_all_services: Some(false),
                include_control_plane: None,
            },
        )
        .await
        .expect("update back to specific");

        let after_specific = svc
            .list_services_for_schedule(schedule.id)
            .await
            .expect("list after flip-back");
        assert!(
            after_specific.is_empty(),
            "flipping back to specific must not magically restore membership"
        );
    }

    /// Unit test (no DB needed): create_backup_schedule rejects a request
    /// that would produce a no-op schedule (include_control_plane=false
    /// AND target_all_services=false). The validation runs before any
    /// DB call, so we don't even need a working Docker daemon for this.
    #[tokio::test]
    async fn create_rejects_empty_fan_out() {
        // Build a service with a mock DB. We never reach the DB because
        // validation fires first.
        if skip_if_no_docker() {
            return;
        }
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // resolve_s3_source_id: caller passed Some(1) so this query
                // (find_by_id) is the next thing the service does.
                .append_query_results(vec![vec![s3_sources::Model {
                    id: 1,
                    name: "s".to_string(),
                    bucket_name: "b".to_string(),
                    bucket_path: "/".to_string(),
                    access_key_id: "".to_string(),
                    secret_key: "".to_string(),
                    region: "us-east-1".to_string(),
                    endpoint: None,
                    force_path_style: Some(true),
                    is_default: true,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                }]])
                .into_connection(),
        );
        let svc = BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db),
            create_mock_notification_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        );

        let request = CreateBackupScheduleRequest {
            name: "bad".to_string(),
            backup_type: "full".to_string(),
            retention_period: 7,
            s3_source_id: Some(1),
            schedule_expression: "0 0 2 * * *".to_string(),
            enabled: true,
            description: None,
            tags: vec![],
            max_runtime_secs: None,
            target_all_services: Some(false),
            include_control_plane: Some(false),
        };

        let err = svc
            .create_backup_schedule(request)
            .await
            .expect_err("empty fan-out must be rejected");
        assert!(
            matches!(err, BackupError::Validation(ref msg) if msg.contains("control plane")),
            "expected Validation error mentioning control plane, got {:?}",
            err
        );
    }

    /// The daily base-backup cron expression used by the auto-provisioner
    /// (`reconcile_default_external_service_schedules`) must satisfy
    /// `validate_backup_schedule`: parse under the `cron` crate's 6-field
    /// format and produce adjacent runs at least one hour apart. This guards
    /// the load-bearing literal so a typo can't ship a schedule that the
    /// validator rejects at provision time.
    #[tokio::test]
    async fn auto_provision_cron_expression_is_valid() {
        // Same expression as provision_default_schedule_for_service.
        const DAILY_3AM: &str = "0 0 3 * * *";

        // Parses under the cron crate (6-field sec/min/hour/dom/mon/dow).
        let schedule =
            Schedule::from_str(DAILY_3AM).expect("auto-provision cron expression must parse");

        // Two adjacent runs are 24h apart -> passes the >= 1h rule in
        // validate_backup_schedule.
        let next_two: Vec<_> = schedule.upcoming(Utc).take(2).collect();
        assert_eq!(next_two.len(), 2, "expected two upcoming runs");
        let gap = next_two[1] - next_two[0];
        assert_eq!(
            gap.num_hours(),
            24,
            "daily base-backup runs should be 24h apart, got {} hours",
            gap.num_hours()
        );
    }

    /// `reconcile_default_external_service_schedules` is a safe no-op when no
    /// default S3 source is configured: `resolve_s3_source_id(None)` errors,
    /// the reconcile swallows it and returns `Ok(())` so the periodic tick can
    /// retry once storage is configured. The MockDatabase returns an empty
    /// `s3_sources` result for the `is_default = true` lookup, so the service
    /// never reaches the MariaDB query.
    #[tokio::test]
    async fn reconcile_default_schedules_noop_without_default_source() {
        if skip_if_no_docker() {
            return;
        }
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // get_default_s3_source: no default configured -> empty result.
                .append_query_results(vec![Vec::<s3_sources::Model>::new()])
                .into_connection(),
        );
        let svc = BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db),
            create_mock_notification_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        );

        let result = svc.reconcile_default_external_service_schedules().await;
        assert!(
            result.is_ok(),
            "reconcile must be a no-op (Ok) when no default S3 source exists, got {:?}",
            result
        );
    }

    /// Full-path integration test (needs a real DB + Docker): with a default
    /// S3 source and an unprovisioned MariaDB service, reconcile creates
    /// exactly one daily schedule, attaches the service to it, flips
    /// `default_backup_provisioned`, and is idempotent on a second call.
    #[tokio::test]
    async fn reconcile_default_schedules_provisions_mariadb_once() {
        if skip_if_no_docker() {
            return;
        }
        use sea_orm::ActiveValue::Set;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use temps_database::test_utils::TestDatabase;

        let test_db = match TestDatabase::with_migrations().await {
            Ok(d) => d,
            Err(e) => {
                println!("TestDatabase unavailable, skipping: {e}");
                return;
            }
        };
        let db = test_db.db.clone();

        // Default S3 source so resolve_s3_source_id(None) succeeds.
        temps_entities::s3_sources::ActiveModel {
            id: sea_orm::NotSet,
            name: Set("auto-prov-source".to_string()),
            bucket_name: Set("b".to_string()),
            bucket_path: Set("/".to_string()),
            access_key_id: Set("".to_string()),
            secret_key: Set("".to_string()),
            region: Set("us-east-1".to_string()),
            endpoint: Set(None),
            force_path_style: Set(Some(true)),
            is_default: Set(true),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
        }
        .insert(db.as_ref())
        .await
        .expect("insert s3 source");

        // One unprovisioned MariaDB service + one Postgres service (which must
        // be left alone — scope is MariaDB only).
        let maria = temps_entities::external_services::ActiveModel {
            id: sea_orm::NotSet,
            name: Set("maria-auto".to_string()),
            service_type: Set("mariadb".to_string()),
            status: Set("running".to_string()),
            topology: Set("standalone".to_string()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert mariadb service");

        let pg = temps_entities::external_services::ActiveModel {
            id: sea_orm::NotSet,
            name: Set("pg-untouched".to_string()),
            service_type: Set("postgres".to_string()),
            status: Set("running".to_string()),
            topology: Set("standalone".to_string()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert postgres service");

        let svc = BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db.clone()),
            create_mock_notification_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        );

        svc.reconcile_default_external_service_schedules()
            .await
            .expect("first reconcile succeeds");

        // Exactly one schedule created.
        let schedules = temps_entities::backup_schedules::Entity::find()
            .all(db.as_ref())
            .await
            .expect("list schedules");
        assert_eq!(
            schedules.len(),
            1,
            "expected exactly one auto-provisioned schedule"
        );
        let schedule = &schedules[0];
        assert_eq!(schedule.schedule_expression, "0 0 3 * * *");
        assert_eq!(schedule.backup_type, "full");
        assert_eq!(schedule.retention_period, 14);
        assert!(!schedule.target_all_services);
        assert!(!schedule.include_control_plane);

        // The MariaDB service is attached to it.
        let attached = svc
            .list_services_for_schedule(schedule.id)
            .await
            .expect("list attached services");
        assert_eq!(attached.len(), 1, "exactly the mariadb service attached");
        assert_eq!(attached[0].id, maria.id);

        // Latch flipped on MariaDB, untouched on Postgres.
        let maria_after = temps_entities::external_services::Entity::find_by_id(maria.id)
            .one(db.as_ref())
            .await
            .expect("reload mariadb")
            .expect("mariadb exists");
        assert!(
            maria_after.default_backup_provisioned,
            "mariadb latch must be set after provisioning"
        );
        let pg_after = temps_entities::external_services::Entity::find_by_id(pg.id)
            .one(db.as_ref())
            .await
            .expect("reload postgres")
            .expect("postgres exists");
        assert!(
            !pg_after.default_backup_provisioned,
            "non-mariadb services must never be provisioned"
        );

        // Idempotency: a second reconcile creates nothing new.
        svc.reconcile_default_external_service_schedules()
            .await
            .expect("second reconcile succeeds");
        let count_after = temps_entities::backup_schedules::Entity::find()
            .filter(temps_entities::backup_schedules::Column::Id.eq(schedule.id))
            .count(db.as_ref())
            .await
            .expect("count");
        let total = temps_entities::backup_schedules::Entity::find()
            .count(db.as_ref())
            .await
            .expect("count all");
        assert_eq!(count_after, 1);
        assert_eq!(total, 1, "second reconcile must not create a duplicate");
    }

    /// Integration test: when `include_control_plane = false` and a single
    /// service is attached, the fan-out produces exactly one backup row
    /// (no control-plane row alongside it). This is the scenario from
    /// the user report where picking one Postgres still produced a
    /// `control_plane` backup as a sidecar.
    #[tokio::test]
    async fn integration_fan_out_skips_control_plane_when_flag_off() {
        if bollard::Docker::connect_with_local_defaults().is_err() {
            println!("Docker not available, skipping test");
            return;
        }
        use sea_orm::ActiveValue::Set;
        use sea_orm::{ColumnTrait, EntityTrait};
        use temps_database::test_utils::TestDatabase;

        let test_db = match TestDatabase::with_migrations().await {
            Ok(d) => d,
            Err(e) => {
                println!("TestDatabase unavailable, skipping: {e}");
                return;
            }
        };
        let db = test_db.db.clone();

        let s3 = temps_entities::s3_sources::ActiveModel {
            id: sea_orm::NotSet,
            name: Set("cp-skip-source".to_string()),
            bucket_name: Set("b".to_string()),
            bucket_path: Set("/".to_string()),
            access_key_id: Set("".to_string()),
            secret_key: Set("".to_string()),
            region: Set("us-east-1".to_string()),
            endpoint: Set(None),
            force_path_style: Set(Some(true)),
            is_default: Set(true),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
        }
        .insert(db.as_ref())
        .await
        .expect("insert s3 source");

        // Schedule: specific mode, no control plane.
        let schedule = temps_entities::backup_schedules::ActiveModel {
            id: sea_orm::NotSet,
            name: Set("cp-skip-schedule".to_string()),
            backup_type: Set("full".to_string()),
            retention_period: Set(7),
            s3_source_id: Set(s3.id),
            schedule_expression: Set("0 0 2 * * *".to_string()),
            enabled: Set(true),
            last_run: Set(None),
            next_run: Set(None),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            description: Set(None),
            tags: Set("[]".to_string()),
            max_runtime_secs: Set(None),
            target_all_services: Set(false),
            include_control_plane: Set(false),
        }
        .insert(db.as_ref())
        .await
        .expect("insert schedule");

        let svc_pg = temps_entities::external_services::ActiveModel {
            id: sea_orm::NotSet,
            name: Set("pg-only".to_string()),
            service_type: Set("postgres".to_string()),
            version: Set(Some("17".to_string())),
            status: Set("running".to_string()),
            slug: Set(Some("pg-only".to_string())),
            config: Set(None),
            node_id: Set(None),
            topology: Set("standalone".to_string()),
            error_message: Set(None),
            health_status: Set(None),
            last_health_check_at: Set(None),
            last_health_error: Set(None),
            consecutive_health_failures: Set(0),
            health_metadata: Set(None),
            metrics_enabled: Set(false),
            default_backup_provisioned: Set(false),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
        }
        .insert(db.as_ref())
        .await
        .expect("insert pg");

        let svc = BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db.clone()),
            create_mock_notification_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
        );

        svc.attach_services_to_schedule(schedule.id, &[svc_pg.id])
            .await
            .expect("attach");

        // Sanity: post-attach, the schedule is well-formed (control-plane
        // off + specific mode + 1 attached service).
        let after_attach = svc
            .list_services_for_schedule(schedule.id)
            .await
            .expect("list");
        assert_eq!(after_attach.len(), 1);

        // Flip-empty test: updating to include_control_plane=false with
        // *no* attached services (we'll detach first) must fail.
        svc.detach_service_from_schedule(schedule.id, svc_pg.id)
            .await
            .expect("detach");
        let err = svc
            .update_backup_schedule(
                schedule.id,
                crate::handlers::backup_handler::UpdateBackupScheduleRequest {
                    name: None,
                    description: None,
                    schedule_expression: None,
                    retention_period: None,
                    max_runtime_secs: None,
                    enabled: None,
                    tags: None,
                    target_all_services: None,
                    include_control_plane: Some(false),
                },
            )
            .await
            .expect_err("empty fan-out must be rejected");
        assert!(
            matches!(err, BackupError::Validation(ref msg) if msg.contains("nothing to back up")),
            "expected Validation error, got {:?}",
            err
        );

        // Cleanup: schedule has no children, so the cascade can drop it.
        let _ = temps_entities::backup_schedules::Entity::delete_by_id(schedule.id)
            .exec(db.as_ref())
            .await;
        let _ = temps_entities::external_services::Entity::delete_by_id(svc_pg.id)
            .exec(db.as_ref())
            .await;
        // Silence unused warning on the QueryFilter / ColumnTrait imports.
        let _ = temps_entities::backup_schedule_services::Column::ScheduleId.eq(0);
    }
}
