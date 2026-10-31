// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The backup service: control-plane and external-service backups, restores,
//! S3 sources, schedules, schedule runs and retention.
//!
//! [`BackupService`] is defined here; its behaviour is split by concern:
//!
//! - `error`: The `BackupError` type and its conversions from database, S3 and
//!   anyhow errors.
//! - `types`: Response and request types shared by the backup service and its
//!   handlers.
//! - `access_scope`: Project-scoped authorization, resolving which projects own
//!   a schedule, a backup or a backup collection.
//! - `retention`: Retention, expiry deadlines, previews and the cleanup of
//!   expired backups.
//! - `s3_sources`: S3 sources, CRUD, connection tests, client construction and
//!   bucket verification.
//! - `index`: The S3 backup index, backup metadata, orphan scans and listing
//!   the backups stored in a source.
//! - `deletion`: Deleting a backup, its S3 objects and its index entry.
//! - `walg_deletion`: Deleting WAL-G backups, target validation, repository
//!   identity and the helper container that runs `wal-g delete`.
//! - `walg`: WAL-G for the control-plane database, locating its container,
//!   enabling WAL archiving and taking base backups.
//! - `postgres_backup`: Control-plane backups (`create_backup`), the pg_dump
//!   sidecar and the upload to S3.
//! - `restore`: Restoring control-plane backups (SQLite, pg_dump and WAL-G).
//! - `queries`: Read-only backup lookups, listing, fetching, open alerts and
//!   live size.
//! - `recovery_set`: Recovery sets, publishing a completed fan-out run as one
//!   restorable set.
//! - `schedules`: Backup schedules, CRUD, service membership, default schedules
//!   and validation.
//! - `schedule_runs`: Schedule runs, enqueueing fan-out runs, pending backup
//!   rows, listing and cancelling runs.
//! - `scheduler`: The background loop that runs due backup schedules and
//!   enforces retention.
//! - `external_service`: Backups of external services (managed databases,
//!   caches and object stores).

use crate::handlers::backup_handler::{
    CreateBackupScheduleRequest, CreateS3SourceRequest, UpdateBackupScheduleRequest,
};
use anyhow::Result;
use aws_sdk_s3::error::ProvideErrorMetadata;
use aws_sdk_s3::Client as S3Client;
use chrono::{DateTime, Duration, Timelike, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseBackend, DatabaseConnection, EntityTrait,
    FromQueryResult, IntoActiveModel, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set,
    Statement, TransactionTrait, Value,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use tempfile::NamedTempFile;
use temps_entities::backups::Model as Backup;
use thiserror::Error;
use tokio::time;
use tracing::{debug, error, info, warn};
use urlencoding;
use uuid::Uuid;
use yaml_serde;

use cron::Schedule;
use temps_core::notifications::BackupFailureData;
use temps_core::secret_string::SecretString;
use temps_core::BackupScheduleAuthority;
use temps_entities::{backup_schedules::Model as BackupSchedule, s3_sources::Model as S3Source};
use temps_monitoring::alarm_service::{AlarmService, AlarmSeverity, AlarmType, FireAlarmRequest};
use temps_providers::ExternalServiceManager;
use tokio_stream::StreamExt;

mod access_scope;
mod deletion;
mod error;
mod external_service;
mod index;
mod postgres_backup;
mod queries;
mod recovery_set;
mod restore;
mod retention;
mod s3_sources;
mod schedule_runs;
mod scheduler;
mod schedules;
#[cfg(test)]
mod test_support;
mod types;
mod walg;
mod walg_deletion;

pub use access_scope::{
    BackupAccessScope, BackupCollectionAccessScope, BackupScheduleAccessScope,
    BackupWithAccessScope, ServiceProjectScope,
};
pub use error::BackupError;
pub use recovery_set::RecoverySetPublication;
use retention::{retention_cutoff, retention_expiry, validate_retention_period};
pub use retention::{
    RetentionCleanupFailure, RetentionCleanupReport, MAX_RETENTION_DAYS_FOR_SQL_ARITHMETIC,
};
use schedules::exclude_source_backing_services;
pub use types::{
    BackupAlertEntry, BackupTriggerParams, ChildBackupEntry, EnqueuedJob, ScheduleRunContext,
    ScheduleRunEntry, ScheduleRunJobEntry, ScheduleRunListResponse, ScheduleRunOutcome,
    ScheduleRunResponse, ScheduleRunSummary, ScheduleRunSummaryList, ServiceBackupEntry,
    TriggerSource,
};
use walg_deletion::{deletion_walg_service_identity, validated_walg_target, WalgDeletionEngine};

/// POSIX-safe shell escaping: wraps value in single quotes, escaping any
/// embedded single quotes. Safe for use in `sh -c` command strings.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn shell_export_assignment(line: &str) -> Option<String> {
    let (key, value) = line.split_once('=')?;
    let valid_key = key
        .chars()
        .all(|character| character == '_' || character.is_ascii_alphanumeric())
        && key
            .chars()
            .next()
            .is_some_and(|character| character == '_' || character.is_ascii_alphabetic());
    valid_key.then(|| format!("export {key}={}", shell_escape(value)))
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

fn s3_key_from_location(location: &str, expected_bucket: &str) -> Result<String, BackupError> {
    if let Some(rest) = location.strip_prefix("s3://") {
        let (bucket, key) = rest.split_once('/').ok_or_else(|| {
            BackupError::Validation(format!("Invalid S3 backup location: {}", location))
        })?;
        if bucket != expected_bucket {
            return Err(BackupError::Validation(format!(
                "Backup location bucket '{}' does not match configured bucket '{}'",
                bucket, expected_bucket
            )));
        }
        if key.is_empty() {
            return Err(BackupError::Validation(format!(
                "Backup location {} does not contain an object key",
                location
            )));
        }
        Ok(key.to_string())
    } else {
        let key = location.trim_start_matches('/');
        if key.is_empty() {
            return Err(BackupError::Validation(
                "Backup location does not contain an object key".to_string(),
            ));
        }
        Ok(key.to_string())
    }
}

fn json_contains_backup_identity(value: &serde_json::Value, backup_id: &str) -> bool {
    match value {
        serde_json::Value::Object(object) => {
            object
                .get("temps_backup_id")
                .and_then(serde_json::Value::as_str)
                == Some(backup_id)
                || object
                    .values()
                    .any(|value| json_contains_backup_identity(value, backup_id))
        }
        serde_json::Value::Array(values) => values
            .iter()
            .any(|value| json_contains_backup_identity(value, backup_id)),
        _ => false,
    }
}

#[derive(Clone)]
pub struct BackupService {
    db: Arc<DatabaseConnection>,
    external_service_manager: Arc<ExternalServiceManager>,
    alarm_service: Arc<AlarmService>,
    config_service: Arc<temps_config::ConfigService>,
    encryption_service: Arc<temps_core::EncryptionService>,
    /// Shared workspace `JobQueue` (typically backed by the in-memory
    /// broadcast service in `temps-queue`). Required at construction, so a
    /// plugin-ordering mistake fails at startup (`require_service`) instead
    /// of panicking on the first backup trigger. Triggers publish
    /// `Job::BackupRequested` here; the `BackupJobProcessor` subscribes and
    /// dispatches to the executor.
    queue: Arc<dyn temps_core::JobQueue>,
}

impl BackupService {
    /// Why running a backup of external service `service_id` with
    /// `engine_key` would restart its database now, or `None`. Only the
    /// WAL-G PostgreSQL engine can restart a service (to turn archiving on).
    pub async fn backup_restart_reason(
        &self,
        service_id: i32,
        engine_key: &str,
    ) -> Result<Option<String>, BackupError> {
        if engine_key != "postgres_walg" {
            return Ok(None);
        }
        self.external_service_manager
            .backup_restart_reason(service_id)
            .await
            .map_err(|e| {
                BackupError::ExternalService(format!(
                    "Failed to check whether backing up service {} restarts it: {}",
                    service_id, e
                ))
            })
    }

    pub fn new(
        db: Arc<DatabaseConnection>,
        external_service_manager: Arc<ExternalServiceManager>,
        alarm_service: Arc<AlarmService>,
        serve_config: Arc<temps_config::ConfigService>,
        encryption_service: Arc<temps_core::EncryptionService>,
        queue: Arc<dyn temps_core::JobQueue>,
    ) -> Self {
        Self {
            db,
            external_service_manager,
            alarm_service,
            config_service: serve_config,
            encryption_service,
            queue,
        }
    }

    /// Fire-and-forget S3 bucket lifecycle reconcile for the given source.
    /// Spawns a background task so the caller (schedule create/update/delete)
    /// is never blocked on S3, and lifecycle failures never bubble up as
    /// schedule operation failures — they only show up in logs.
    ///
    /// The reconcile rebuilds the bucket's lifecycle rules from current
    /// schedule state, so even concurrent schedule changes converge to a
    /// consistent rule set eventually. A failure here isn't a dead end: it's
    /// recorded on the source via `lifecycle_reconcile_failed_at` (see
    /// `S3LifecycleService::reconcile_bucket`), which keeps the source in
    /// the hourly sweep's scope — even with no enabled schedule left — until
    /// a later attempt actually succeeds.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sourced_internal_walg_values_are_shell_quoted() {
        assert_eq!(
            shell_export_assignment("WALG_S3_PREFIX=s3://bucket/ok;touch${IFS}/tmp/pwn;#"),
            Some("export WALG_S3_PREFIX='s3://bucket/ok;touch${IFS}/tmp/pwn;#'".to_string())
        );
        assert!(shell_export_assignment("BAD-KEY=value").is_none());
    }

    #[test]
    fn s3_key_from_uri_strips_matching_bucket() {
        let key = s3_key_from_location("s3://backups/path/to/archive.sql.gz", "backups")
            .expect("valid S3 URI should produce a key");
        assert_eq!(key, "path/to/archive.sql.gz");
    }

    #[test]
    fn s3_key_from_location_preserves_plain_key() {
        let key = s3_key_from_location("/path/to/archive.sql.gz", "backups")
            .expect("plain S3 key should be accepted");
        assert_eq!(key, "path/to/archive.sql.gz");
    }

    #[test]
    fn s3_key_from_uri_rejects_different_bucket() {
        let error = s3_key_from_location("s3://other/path/archive.sql.gz", "backups")
            .expect_err("mismatched bucket must be rejected");
        assert!(matches!(error, BackupError::Validation(_)));
        assert!(error
            .to_string()
            .contains("does not match configured bucket"));
    }

    #[test]
    fn walg_repository_identity_search_handles_nested_backup_list_detail() {
        let repository = serde_json::json!([
            {
                "backup_name": "base_0001",
                "user_data": {"temps_backup_id": "other"}
            },
            {
                "backup_name": "base_0002",
                "detail": {
                    "sentinel": {
                        "user_data": {"temps_backup_id": "selected-backup"}
                    }
                }
            }
        ]);
        assert!(json_contains_backup_identity(
            &repository,
            "selected-backup"
        ));
        assert!(!json_contains_backup_identity(&repository, "missing"));
    }
}
