// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The `BackupError` type and its conversions from database, S3 and anyhow
//! errors.

use super::*;

#[derive(Error, Debug)]
pub enum BackupError {
    #[error("Database error: {0}")]
    Database(sea_orm::DbErr),

    #[error("S3 error: {0}")]
    S3(String),

    /// An S3 destination could not be verified: it is unreachable, or it
    /// rejected the configured credentials or bucket. Returned by creating,
    /// updating and testing a destination; nothing is saved.
    #[error(
        "Could not verify S3 bucket '{bucket}' at {endpoint}: {}. {}. Cause: {cause}",
        .kind.summary(),
        .kind.hint()
    )]
    S3Unreachable {
        endpoint: String,
        bucket: String,
        kind: crate::services::s3_probe::S3ProbeFailureKind,
        cause: String,
    },

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

    #[error("Cannot delete backup {backup_id} while it is in state '{state}'; cancel it first and wait for it to become terminal")]
    BackupNotTerminal { backup_id: String, state: String },

    #[error("Cannot delete backup {backup_id}: it is referenced by {restore_count} restore history record(s)")]
    BackupHasRestoreHistory {
        backup_id: String,
        restore_count: u64,
    },

    #[error("Backup {backup_id} was only partially deleted: {deleted_objects} object(s) confirmed removed; {reason}")]
    PartialDeletion {
        backup_id: String,
        deleted_objects: u64,
        reason: String,
    },

    #[error("Cleanup preview is stale: {detail}. Run a new dry-run preview before deleting")]
    CleanupPreviewStale { detail: String },

    #[error("Access denied to {resource}: {detail}")]
    Forbidden { resource: String, detail: String },

    #[error("Failed to verify access to {resource}: {detail}")]
    Authorization { resource: String, detail: String },
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
