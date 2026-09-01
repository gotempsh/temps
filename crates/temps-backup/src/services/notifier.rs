// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `BackupNotificationAdapter`: concrete impl of [`BackupFailureNotifier`] for
//! `temps-backup` (deliverable 3).
//!
//! Lives in `temps-backup` so it can reach:
//! - [`temps_monitoring::alarm_service::AlarmService`] (for persistence + dispatch)
//! - The `backups` entity (to look up schedule name)
//! - The `backup_schedules` entity (to look up schedule name for the notification)
//!
//! The adapter is wired into the `BackupRunner` via `runner.with_notifier(...)` in
//! `plugin.rs`.
//!
//! Backups are host-wide resources — a schedule can fan out across every
//! external service on the host and `backups`/`backup_schedules` carry no
//! `project_id` — so failures are fired as system alarms (`project_id: None`).

use std::sync::Arc;

use sea_orm::{DatabaseConnection, EntityTrait};
use temps_backup_core::{BackupFailureContext, BackupFailureNotifier};
use temps_monitoring::alarm_service::{AlarmService, AlarmSeverity, AlarmType, FireAlarmRequest};
use tracing::{debug, error, info};

/// Dispatches a [`FireAlarmRequest`] via [`AlarmService`] whenever a backup
/// job reaches the terminal `failed` state.
///
/// The adapter performs a DB lookup to enrich the alarm with the schedule
/// name (when available).  Any internal error is logged via `tracing::error!`
/// and swallowed — a notification failure must never surface to the caller.
pub struct BackupNotificationAdapter {
    alarm_service: Arc<AlarmService>,
    db: Arc<DatabaseConnection>,
}

impl BackupNotificationAdapter {
    /// Create a new adapter.
    ///
    /// Both `alarm_service` and `db` must be fully initialised before
    /// calling this constructor.
    pub fn new(alarm_service: Arc<AlarmService>, db: Arc<DatabaseConnection>) -> Self {
        Self { alarm_service, db }
    }
}

#[async_trait::async_trait]
impl BackupFailureNotifier for BackupNotificationAdapter {
    /// Dispatch a failure notification for `ctx`.
    ///
    /// Looks up the parent `backups` row to find a `schedule_id`; if present,
    /// looks up `backup_schedules` for a human-readable name.  Falls back to
    /// synthetic names gracefully — the notification is always sent even if
    /// lookups fail.
    async fn notify_failed(&self, ctx: BackupFailureContext) {
        // Look up the parent backups row to retrieve schedule_id.
        let schedule_name = match temps_entities::backups::Entity::find_by_id(ctx.backup_id)
            .one(self.db.as_ref())
            .await
        {
            Ok(Some(backup)) => {
                if let Some(sid) = backup.schedule_id {
                    match temps_entities::backup_schedules::Entity::find_by_id(sid)
                        .one(self.db.as_ref())
                        .await
                    {
                        Ok(Some(schedule)) => schedule.name,
                        Ok(None) => format!("schedule {}", sid),
                        Err(e) => {
                            error!(
                                backup_id = ctx.backup_id,
                                schedule_id = sid,
                                error = %e,
                                "BackupNotificationAdapter: failed to look up schedule name",
                            );
                            format!("schedule {}", sid)
                        }
                    }
                } else {
                    // Control-plane backup without a schedule (manual ad-hoc run).
                    format!("{} backup #{}", ctx.engine, ctx.backup_id)
                }
            }
            Ok(None) => {
                // Parent row disappeared — very unlikely; proceed with synthetic name.
                format!("{} backup #{}", ctx.engine, ctx.backup_id)
            }
            Err(e) => {
                error!(
                    backup_id = ctx.backup_id,
                    error = %e,
                    "BackupNotificationAdapter: failed to look up parent backup row",
                );
                format!("{} backup #{}", ctx.engine, ctx.backup_id)
            }
        };

        let title = format!("Backup Failed: {}", schedule_name);
        let message = format!(
            "Backup failed for {} (engine: {}, attempt {}/{}): {}",
            schedule_name, ctx.engine, ctx.attempts, ctx.max_attempts, ctx.error_message,
        );

        let request = FireAlarmRequest {
            project_id: None,
            environment_id: None,
            deployment_id: None,
            container_id: None,
            service_id: None,
            alarm_type: AlarmType::BackupFailed,
            severity: AlarmSeverity::Critical,
            title,
            message,
            metadata: Some(serde_json::json!({
                "backup_id": ctx.backup_id,
                "engine": ctx.engine,
                "attempts": ctx.attempts,
                "max_attempts": ctx.max_attempts,
                "failed_at": ctx.failed_at.to_rfc3339(),
            })),
        };

        match self.alarm_service.fire_alarm(request).await {
            Ok(Some(_)) => info!(
                backup_id = ctx.backup_id,
                engine = %ctx.engine,
                "BackupNotificationAdapter: fired backup-failed alarm",
            ),
            Ok(None) => debug!(
                backup_id = ctx.backup_id,
                "Backup-failed alarm suppressed by cooldown/silence",
            ),
            Err(e) => error!(
                backup_id = ctx.backup_id,
                engine = %ctx.engine,
                error = %e,
                "BackupNotificationAdapter: failed to fire failure alarm (non-fatal)",
            ),
        }
    }
}
