// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Response and request types shared by the backup service and its handlers.

use super::*;

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
    /// The parent schedule's `retention_period`, in days. Used to stamp
    /// `backups.expires_at` (and the child `external_service_backups` row)
    /// with the retention deadline at insert time.
    pub retention_period: i32,
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

/// One unresolved backup alert, including optional schedule metadata used by
/// the API to build a safe response without issuing SQL from the handler.
#[derive(Debug, FromQueryResult)]
pub struct BackupAlertEntry {
    pub id: i64,
    pub kind: String,
    pub severity: String,
    pub schedule_id: Option<i32>,
    pub schedule_name: Option<String>,
    pub schedule_s3_source_id: Option<i32>,
    pub message: String,
    pub opened_at: chrono::DateTime<chrono::Utc>,
}
