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
    BackupError, BackupService, BackupTriggerParams, ChildBackupEntry, EnqueuedJob,
    RetentionCleanupFailure, RetentionCleanupReport, ScheduleRunEntry, ScheduleRunJobEntry,
    ScheduleRunListResponse, ScheduleRunOutcome, ScheduleRunResponse, ScheduleRunSummary,
    ScheduleRunSummaryList, ServiceBackupEntry, TriggerSource,
};
pub use notifier::BackupNotificationAdapter;
pub use reconcile::reconcile_orphan_backups;
pub use restore::{
    BackupSelector, PlanSourceBackup, PlanTarget, RestoreError, RestorePlan, RestoreRequestMode,
    RestoreRunView, RestoreService,
};
pub use s3_lifecycle::{ReconcileOutcome, S3LifecycleService};
