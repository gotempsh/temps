// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Retention, expiry deadlines, previews and the cleanup of expired backups.

use super::*;

pub(super) fn validate_retention_period(days: i32) -> Result<(), BackupError> {
    if days < 1 {
        return Err(BackupError::Validation(
            "retention_period must be >= 1".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn retention_cutoff(days: i32) -> Result<DateTime<Utc>, BackupError> {
    validate_retention_period(days)?;
    Utc::now()
        .checked_sub_signed(Duration::days(i64::from(days)))
        .ok_or_else(|| {
            BackupError::Validation(format!(
                "retention_period {} days is outside the supported date range",
                days
            ))
        })
}

/// Retention deadline of a backup created by a schedule.
///
/// `run_retention_cleanup` deletes a schedule's backups once
/// `started_at < now - retention_period days`, so the moment a given backup
/// becomes eligible for deletion is exactly `started_at + retention_period`.
/// Storing that on the row (`backups.expires_at`) is what lets the API and
/// console answer "until when is this backup kept?" without re-deriving the
/// schedule's retention every time.
///
/// Returns `None` when the retention period is not a positive number of days
/// (retention is disabled, so the backup is kept until deleted) or when the
/// resulting timestamp would overflow the supported date range.
///
/// Raw-SQL callers that add a retention period to a timestamp directly
/// (instead of going through this function) cannot express "return None on
/// overflow" — `timestamp + interval` in Postgres raises an error and aborts
/// the statement instead. They must clamp the number of days to
/// [`MAX_RETENTION_DAYS_FOR_SQL_ARITHMETIC`] first so the addition can never
/// leave the range Postgres/chrono support.
pub const MAX_RETENTION_DAYS_FOR_SQL_ARITHMETIC: i64 = 36_500_000; // ~100,000 years

pub(super) fn retention_expiry(
    started_at: DateTime<Utc>,
    retention_period_days: i32,
) -> Option<DateTime<Utc>> {
    if retention_period_days < 1 {
        return None;
    }
    started_at.checked_add_signed(Duration::days(i64::from(retention_period_days)))
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct RetentionCleanupFailure {
    pub backup_id: String,
    pub reason: String,
    pub partial: bool,
    pub deleted_objects: u64,
}

#[derive(Debug, Clone, Default, serde::Serialize, utoipa::ToSchema)]
pub struct RetentionCleanupReport {
    /// True when this report is a non-destructive preview.
    pub dry_run: bool,
    /// Schedule scope, or `None` when every schedule was considered.
    pub schedule_id: Option<i32>,
    pub expired: u64,
    pub deleted: u64,
    pub failed: u64,
    /// Capped diagnostic sample; `failed` remains the authoritative total.
    pub failures: Vec<RetentionCleanupFailure>,
    /// Capped sample of deleted backup UUIDs for audit attribution.
    pub deleted_backup_ids: Vec<String>,
    pub deleted_backup_ids_truncated: bool,
    pub partially_deleted_backup_ids: Vec<String>,
    pub partially_deleted_backup_ids_truncated: bool,
    /// Capped sample of backups selected by the retention policy.
    pub candidate_backup_ids: Vec<String>,
    pub candidate_backup_ids_truncated: bool,
}

impl BackupService {
    pub async fn cleanup_old_backups(&self, retention_days: i32) -> Result<()> {
        info!("Cleaning up old backups");

        let cutoff_date = retention_cutoff(retention_days)?;

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

    /// Enforce retention for every backup schedule, including disabled ones.
    /// Deletes backups that are older than each schedule's `retention_period` days.
    pub async fn enforce_retention(
        &self,
        schedule_id: Option<i32>,
        expected_backup_ids: Option<&[String]>,
    ) -> Result<RetentionCleanupReport, BackupError> {
        if let Some(expected) = expected_backup_ids {
            return self
                .enforce_previewed_retention(schedule_id, expected)
                .await;
        }
        self.run_retention_cleanup(schedule_id, false).await
    }

    /// Preview retention candidates without deleting S3 objects or database rows.
    pub async fn preview_retention(
        &self,
        schedule_id: Option<i32>,
    ) -> Result<RetentionCleanupReport, BackupError> {
        self.run_retention_cleanup(schedule_id, true).await
    }

    async fn run_retention_cleanup(
        &self,
        schedule_id: Option<i32>,
        dry_run: bool,
    ) -> Result<RetentionCleanupReport, BackupError> {
        let mut query = temps_entities::backup_schedules::Entity::find();
        if let Some(id) = schedule_id {
            query = query.filter(temps_entities::backup_schedules::Column::Id.eq(id));
        }
        let schedules = query.all(self.db.as_ref()).await?;
        if let Some(id) = schedule_id {
            if schedules.is_empty() {
                return Err(BackupError::NotFound {
                    resource: "BackupSchedule".to_string(),
                    detail: format!("schedule id {}", id),
                });
            }
        }

        let mut report = RetentionCleanupReport {
            dry_run,
            schedule_id,
            ..RetentionCleanupReport::default()
        };

        for schedule in &schedules {
            if schedule.retention_period > 0 {
                let cutoff = retention_cutoff(schedule.retention_period)?;
                let mut last_id = 0;
                loop {
                    use sea_orm::QuerySelect;
                    let old_backups = match temps_entities::backups::Entity::find()
                        .filter(temps_entities::backups::Column::ScheduleId.eq(Some(schedule.id)))
                        .filter(temps_entities::backups::Column::StartedAt.lt(cutoff))
                        .filter(temps_entities::backups::Column::Id.gt(last_id))
                        .order_by_asc(temps_entities::backups::Column::Id)
                        .limit(100)
                        .all(self.db.as_ref())
                        .await
                    {
                        Ok(backups) => backups,
                        Err(e) => {
                            if dry_run {
                                return Err(BackupError::Database(e));
                            }
                            report.failed += 1;
                            if report.failures.len() < 100 {
                                report.failures.push(RetentionCleanupFailure {
                                    backup_id: format!("schedule:{}", schedule.id),
                                    reason: format!("Failed to query expired backups: {}", e),
                                    partial: false,
                                    deleted_objects: 0,
                                });
                            }
                            break;
                        }
                    };
                    if old_backups.is_empty() {
                        break;
                    }
                    for backup in old_backups {
                        last_id = backup.id;
                        report.expired += 1;
                        let backup_uuid = backup.backup_id.clone();
                        if report.candidate_backup_ids.len() < 100 {
                            report.candidate_backup_ids.push(backup_uuid.clone());
                        } else {
                            report.candidate_backup_ids_truncated = true;
                        }
                        if dry_run {
                            continue;
                        }
                        match self.delete_backup_model(backup).await {
                            Ok(_) => {
                                report.deleted += 1;
                                if report.deleted_backup_ids.len() < 100 {
                                    report.deleted_backup_ids.push(backup_uuid);
                                } else {
                                    report.deleted_backup_ids_truncated = true;
                                }
                            }
                            Err(e) => {
                                report.failed += 1;
                                let (partial, deleted_objects) = match &e {
                                    BackupError::PartialDeletion {
                                        deleted_objects, ..
                                    } => (true, *deleted_objects),
                                    _ => (false, 0),
                                };
                                error!(
                                    backup_id = %backup_uuid,
                                    schedule_id = schedule.id,
                                    error = %e,
                                    "Failed to delete expired backup"
                                );
                                if report.failures.len() < 100 {
                                    report.failures.push(RetentionCleanupFailure {
                                        backup_id: backup_uuid.clone(),
                                        reason: e.to_string(),
                                        partial,
                                        deleted_objects,
                                    });
                                }
                                if partial {
                                    if report.partially_deleted_backup_ids.len() < 100 {
                                        report.partially_deleted_backup_ids.push(backup_uuid);
                                    } else {
                                        report.partially_deleted_backup_ids_truncated = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(report)
    }

    /// Delete exactly the candidates approved by a prior dry-run preview.
    /// The candidate query is completed before any deletion begins, so policy
    /// or cutoff drift cannot expand the destructive set after confirmation.
    async fn enforce_previewed_retention(
        &self,
        schedule_id: Option<i32>,
        expected_backup_ids: &[String],
    ) -> Result<RetentionCleanupReport, BackupError> {
        use std::collections::{HashMap, HashSet};

        if expected_backup_ids.len() > 100 {
            return Err(BackupError::Validation(
                "A preview-bound cleanup accepts at most 100 backup IDs".to_string(),
            ));
        }
        let expected: HashSet<&str> = expected_backup_ids.iter().map(String::as_str).collect();
        if expected.len() != expected_backup_ids.len() {
            return Err(BackupError::Validation(
                "Cleanup preview contains duplicate backup IDs".to_string(),
            ));
        }

        let mut schedule_query = temps_entities::backup_schedules::Entity::find();
        if let Some(id) = schedule_id {
            schedule_query =
                schedule_query.filter(temps_entities::backup_schedules::Column::Id.eq(id));
        }
        let schedules = schedule_query.all(self.db.as_ref()).await?;
        if let Some(id) = schedule_id {
            if schedules.is_empty() {
                return Err(BackupError::NotFound {
                    resource: "BackupSchedule".to_string(),
                    detail: format!("schedule id {}", id),
                });
            }
        }

        let schedules_by_id: HashMap<i32, &temps_entities::backup_schedules::Model> = schedules
            .iter()
            .map(|schedule| (schedule.id, schedule))
            .collect();
        let candidates = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::BackupId.is_in(expected_backup_ids.to_vec()))
            .all(self.db.as_ref())
            .await?;

        let actual: HashSet<&str> = candidates
            .iter()
            .map(|backup| backup.backup_id.as_str())
            .collect();
        if actual != expected {
            return Err(BackupError::CleanupPreviewStale {
                detail: format!(
                    "preview approved {} backup(s), but {} still exist",
                    expected.len(),
                    actual.len(),
                ),
            });
        }
        for backup in &candidates {
            let Some(candidate_schedule_id) = backup.schedule_id else {
                return Err(BackupError::CleanupPreviewStale {
                    detail: format!(
                        "backup {} no longer belongs to a schedule",
                        backup.backup_id
                    ),
                });
            };
            let Some(schedule) = schedules_by_id.get(&candidate_schedule_id) else {
                return Err(BackupError::CleanupPreviewStale {
                    detail: format!(
                        "backup {} is no longer in the selected schedule scope",
                        backup.backup_id
                    ),
                });
            };
            let cutoff = retention_cutoff(schedule.retention_period)?;
            if backup.started_at >= cutoff {
                return Err(BackupError::CleanupPreviewStale {
                    detail: format!(
                        "backup {} is no longer expired by its retention policy",
                        backup.backup_id
                    ),
                });
            }
        }

        let mut report = RetentionCleanupReport {
            schedule_id,
            expired: candidates.len() as u64,
            candidate_backup_ids: candidates
                .iter()
                .map(|backup| backup.backup_id.clone())
                .collect(),
            ..RetentionCleanupReport::default()
        };
        for backup in candidates {
            let backup_uuid = backup.backup_id.clone();
            match self.delete_backup_model(backup).await {
                Ok(_) => {
                    report.deleted += 1;
                    report.deleted_backup_ids.push(backup_uuid);
                }
                Err(error) => {
                    report.failed += 1;
                    let (partial, deleted_objects) = match &error {
                        BackupError::PartialDeletion {
                            deleted_objects, ..
                        } => (true, *deleted_objects),
                        _ => (false, 0),
                    };
                    if partial {
                        report
                            .partially_deleted_backup_ids
                            .push(backup_uuid.clone());
                    }
                    report.failures.push(RetentionCleanupFailure {
                        backup_id: backup_uuid,
                        reason: error.to_string(),
                        partial,
                        deleted_objects,
                    });
                }
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_core::EncryptionService;
    use temps_entities::backup_schedules;

    #[test]
    fn retention_validation_rejects_non_positive_days() {
        let error = validate_retention_period(0).expect_err("zero-day retention must be rejected");
        assert!(matches!(error, BackupError::Validation(_)));
        assert!(error.to_string().contains("retention_period must be >= 1"));
        assert!(validate_retention_period(-1).is_err());
        assert!(validate_retention_period(1).is_ok());
        assert!(validate_retention_period(3650).is_ok());
    }

    #[tokio::test]
    async fn preview_retention_is_schedule_scoped_and_non_destructive() {
        let schedule = make_test_schedule(7, 1);
        let mut expired_backup = make_test_backup_model(41);
        expired_backup.schedule_id = Some(schedule.id);
        expired_backup.started_at = chrono::Utc::now() - chrono::Duration::days(8);

        // Query sequence: selected schedule, one candidate batch, empty batch.
        // No transaction, S3-source lookup, or DELETE result is provided; an
        // accidental destructive call therefore makes this test fail.
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![schedule]])
                .append_query_results(vec![vec![expired_backup], vec![]])
                .into_connection(),
        );
        let service = BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db.clone()),
            create_mock_alarm_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
            Arc::new(NoopJobQueue),
        );

        let report = service
            .preview_retention(Some(7))
            .await
            .expect("dry run should return retention candidates");

        assert!(report.dry_run);
        assert_eq!(report.schedule_id, Some(7));
        assert_eq!(report.expired, 1);
        assert_eq!(report.deleted, 0);
        assert_eq!(report.failed, 0);
        assert_eq!(report.candidate_backup_ids, vec!["uuid-41"]);

        drop(service);
        let statements = Arc::try_unwrap(db)
            .expect("service dropped, leaving one database reference")
            .into_transaction_log();
        assert_eq!(statements.len(), 3, "preview must only issue SELECTs");
        let sql = format!("{statements:?}");
        assert!(
            sql.contains("backup_schedules") && sql.contains("schedule_id"),
            "preview queries must retain the requested schedule scope: {sql}"
        );
    }

    #[tokio::test]
    async fn preview_retention_returns_not_found_for_unknown_schedule() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<backup_schedules::Model>::new()])
                .into_connection(),
        );
        let service = BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db),
            create_mock_alarm_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
            Arc::new(NoopJobQueue),
        );

        let error = service
            .preview_retention(Some(999))
            .await
            .expect_err("an unknown cleanup scope must be rejected");
        assert!(matches!(error, BackupError::NotFound { .. }));
    }

    #[tokio::test]
    async fn preview_bound_cleanup_rejects_candidate_that_is_no_longer_expired() {
        let schedule = make_test_schedule(7, 1);
        let mut recent_backup = make_test_backup_model(42);
        recent_backup.schedule_id = Some(schedule.id);
        recent_backup.started_at = chrono::Utc::now();

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![schedule]])
                .append_query_results(vec![vec![recent_backup]])
                .into_connection(),
        );
        let service = BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db),
            create_mock_alarm_service(),
            create_mock_config_service(),
            Arc::new(EncryptionService::new("test_encryption_key_1234567890ab").unwrap()),
            Arc::new(NoopJobQueue),
        );
        let expected = vec!["uuid-42".to_string()];

        let error = service
            .enforce_retention(Some(7), Some(&expected))
            .await
            .expect_err("retention drift must force a new preview");

        assert!(matches!(error, BackupError::CleanupPreviewStale { .. }));
    }

    /// A one-day retention moves the deadline exactly 24 hours past
    /// `started_at` — the same instant `run_retention_cleanup` starts
    /// treating the backup as expired.
    #[test]
    fn retention_expiry_adds_one_day() {
        let started_at = DateTime::parse_from_rfc3339("2026-01-15T14:30:00Z")
            .expect("valid fixture timestamp")
            .with_timezone(&Utc);

        assert_eq!(
            retention_expiry(started_at, 1),
            Some(
                DateTime::parse_from_rfc3339("2026-01-16T14:30:00Z")
                    .expect("valid fixture timestamp")
                    .with_timezone(&Utc)
            )
        );
    }

    /// A long retention (10 years) is still representable and is not
    /// silently clamped.
    #[test]
    fn retention_expiry_handles_large_retention_periods() {
        let started_at = DateTime::parse_from_rfc3339("2026-01-15T14:30:00Z")
            .expect("valid fixture timestamp")
            .with_timezone(&Utc);

        let expiry = retention_expiry(started_at, 3650).expect("10 years is in range");
        assert_eq!(expiry, started_at + Duration::days(3650));
    }

    /// Zero and negative retention mean "no schedule-driven expiry": the
    /// backup is kept until someone deletes it, so there is no deadline to
    /// show.
    #[test]
    fn retention_expiry_is_none_for_non_positive_periods() {
        let started_at = Utc::now();

        assert_eq!(retention_expiry(started_at, 0), None);
        assert_eq!(retention_expiry(started_at, -1), None);
        assert_eq!(retention_expiry(started_at, i32::MIN), None);
    }

    /// An absurd retention period would overflow the supported date range;
    /// report no deadline rather than panicking or wrapping.
    #[test]
    fn retention_expiry_is_none_on_overflow() {
        let started_at = Utc::now();

        assert_eq!(retention_expiry(started_at, i32::MAX), None);
    }

    /// Integration test: changing a schedule's `retention_period` rewrites
    /// the stored deadline of every backup that schedule owns, and leaves
    /// backups that belong to no schedule alone (they are kept until
    /// deleted). Skips gracefully when Docker / test Postgres are absent.
    #[tokio::test]
    async fn integration_retention_change_recomputes_backup_expiry() {
        if bollard::Docker::connect_with_local_defaults().is_err() {
            println!("Docker not available, skipping test");
            return;
        }
        use sea_orm::ActiveValue::Set;
        use sea_orm::EntityTrait;
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
            name: Set("retention-source".to_string()),
            bucket_name: Set("b".to_string()),
            bucket_path: Set("/".to_string()),
            access_key_id: Set(String::new()),
            secret_key: Set(String::new()),
            session_token: Set(None),
            credentials_expire_at: Set(None),
            region: Set("us-east-1".to_string()),
            endpoint: Set(None),
            force_path_style: Set(Some(true)),
            is_default: Set(true),
            managed_by_cloud: Set(false),
            lifecycle_reconcile_failed_at: Set(None),
            lifecycle_reconcile_generation: Set(0),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            backing_service_id: Set(None),
        }
        .insert(db.as_ref())
        .await
        .expect("insert s3 source");

        let schedule = temps_entities::backup_schedules::ActiveModel {
            id: sea_orm::NotSet,
            name: Set("retention-schedule".to_string()),
            backup_type: Set("full".to_string()),
            retention_period: Set(7),
            s3_source_id: Set(s3.id),
            schedule_expression: Set("0 0 2 * * *".to_string()),
            enabled: Set(true),
            last_run: Set(None),
            next_run: Set(None),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            description: Set(None),
            tags: Set("[]".to_string()),
            max_runtime_secs: Set(None),
            target_all_services: Set(true),
            include_control_plane: Set(true),
            generated_kind: Set(None),
        }
        .insert(db.as_ref())
        .await
        .expect("insert schedule");

        // `backups.created_by` is a FK to `users`.
        let owner = temps_entities::users::ActiveModel {
            name: Set("Backup Owner".to_string()),
            email: Set("retention-owner@example.com".to_string()),
            password_hash: Set(Some("test_hash".to_string())),
            email_verified: Set(true),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert owner user");

        let started_at = DateTime::parse_from_rfc3339("2026-01-15T14:30:00Z")
            .expect("valid fixture timestamp")
            .with_timezone(&Utc);

        let make_backup =
            |name: &str, schedule_id: Option<i32>| temps_entities::backups::ActiveModel {
                id: sea_orm::NotSet,
                name: Set(name.to_string()),
                backup_id: Set(Uuid::new_v4().to_string()),
                schedule_id: Set(schedule_id),
                schedule_run_id: Set(None),
                backup_type: Set("full".to_string()),
                state: Set("completed".to_string()),
                started_at: Set(started_at),
                finished_at: Set(Some(started_at)),
                size_bytes: Set(None),
                file_count: Set(None),
                s3_source_id: Set(s3.id),
                s3_location: Set(String::new()),
                error_message: Set(None),
                metadata: Set("{}".to_string()),
                checksum: Set(None),
                compression_type: Set("gzip".to_string()),
                created_by: Set(owner.id),
                expires_at: Set(retention_expiry(started_at, 7)),
                tags: Set("[]".to_string()),
            };

        let scheduled = make_backup("scheduled-backup", Some(schedule.id))
            .insert(db.as_ref())
            .await
            .expect("insert scheduled backup");

        let mut manual = make_backup("manual-backup", None);
        manual.expires_at = Set(None);
        let manual = manual
            .insert(db.as_ref())
            .await
            .expect("insert manual backup");

        assert_eq!(
            scheduled.expires_at,
            Some(started_at + Duration::days(7)),
            "precondition: the scheduled backup starts with the 7-day deadline"
        );

        let svc = BackupService::new(
            db.clone(),
            create_mock_external_service_manager(db.clone()),
            create_mock_alarm_service(),
            create_mock_config_service(),
            Arc::new(
                EncryptionService::new("test_encryption_key_1234567890ab")
                    .expect("test encryption key"),
            ),
            Arc::new(NoopJobQueue),
        );

        svc.update_backup_schedule(
            schedule.id,
            crate::handlers::backup_handler::UpdateBackupScheduleRequest {
                name: None,
                description: None,
                schedule_expression: None,
                retention_period: Some(30),
                max_runtime_secs: None,
                enabled: None,
                tags: None,
                target_all_services: None,
                include_control_plane: None,
                service_ids: None,
            },
            temps_core::BackupScheduleAuthority::InstanceAdmin,
        )
        .await
        .expect("retention update succeeds");

        let scheduled_after = temps_entities::backups::Entity::find_by_id(scheduled.id)
            .one(db.as_ref())
            .await
            .expect("reload scheduled backup")
            .expect("scheduled backup still exists");
        assert_eq!(
            scheduled_after.expires_at,
            Some(started_at + Duration::days(30)),
            "extending retention must move the stored deadline forward"
        );

        let manual_after = temps_entities::backups::Entity::find_by_id(manual.id)
            .one(db.as_ref())
            .await
            .expect("reload manual backup")
            .expect("manual backup still exists");
        assert_eq!(
            manual_after.expires_at, None,
            "a backup with no schedule is kept until deleted"
        );
    }
}
