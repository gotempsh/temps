// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

mod alerts;
mod backup;
mod notifier;
mod reconcile;
mod restore;
// `pub(crate)` so the upload path in `engines::v2_common::apply_object_tags`
// can reuse `is_unsupported_error` to decide whether a tagging failure is
// "this provider doesn't support tags" (warn + continue) vs a real error.
pub(crate) mod s3_lifecycle;
pub use alerts::{sweep_backup_alerts, SweepStats, OVERDUE_GRACE};
pub use backup::{
    BackupAccessScope, BackupAlertEntry, BackupCollectionAccessScope, BackupError,
    BackupScheduleAccessScope, BackupService, BackupTriggerParams, BackupWithAccessScope,
    ChildBackupEntry, EnqueuedJob, RecoverySetPublication, RetentionCleanupFailure,
    RetentionCleanupReport, ScheduleRunEntry, ScheduleRunJobEntry, ScheduleRunListResponse,
    ScheduleRunOutcome, ScheduleRunResponse, ScheduleRunSummary, ScheduleRunSummaryList,
    ServiceBackupEntry, ServiceProjectScope, TriggerSource,
};
pub use notifier::BackupNotificationAdapter;
pub use reconcile::reconcile_orphan_backups;
pub use restore::{
    BackupProducerServices, BackupSelector, PlanSourceBackup, PlanTarget, RestoreError,
    RestorePlan, RestoreRequestMode, RestoreRunView, RestoreService, RestoreServiceIdentity,
};
pub use s3_lifecycle::{ReconcileOutcome, S3LifecycleService};
