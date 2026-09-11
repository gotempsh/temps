// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The backup scheduler's side of the managed-destination seam (ADR-044):
//! how a Cloud-provisioned `s3_sources` row gets its nightly schedule, and
//! how it loses it again on disconnect.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder};
use temps_core::{
    ManagedBackupSchedule, ManagedBackupScheduleError, ManagedBackupScheduleProvisioner,
    ReleasedManagedBackupSchedules, MANAGED_BACKUP_SCHEDULE_EXPRESSION,
    MANAGED_BACKUP_SCHEDULE_NAME,
};
use temps_entities::backup_schedules;
use tokio::sync::Mutex;

use crate::handlers::backup_handler::CreateBackupScheduleRequest;
use crate::services::BackupService;

/// Tag the default schedule carries so a disconnect can tell it apart from a
/// schedule the operator made. Removing the tag claims the schedule.
pub const MANAGED_BACKUP_SCHEDULE_TAG: &str = "temps-cloud";

/// The implementation the backup plugin registers for the Cloud plugin.
///
/// Reads through its own connection and writes through [`BackupService`],
/// so creating and deleting go through the same validation and lifecycle
/// hooks as the Backups page. `ensure` serialises its look-up-then-insert
/// behind a mutex: enrollment, the settings page and the CLI can all ask at
/// once, and `backup_schedules.s3_source_id` is not unique.
pub struct ManagedScheduleProvisioner {
    db: Arc<DatabaseConnection>,
    backups: Arc<BackupService>,
    ensure: Mutex<()>,
}

impl ManagedScheduleProvisioner {
    pub fn new(db: Arc<DatabaseConnection>, backups: Arc<BackupService>) -> Self {
        Self {
            db,
            backups,
            ensure: Mutex::new(()),
        }
    }

    async fn schedules_for_source(
        &self,
        s3_source_id: i32,
    ) -> Result<Vec<backup_schedules::Model>, ManagedBackupScheduleError> {
        backup_schedules::Entity::find()
            .filter(backup_schedules::Column::S3SourceId.eq(s3_source_id))
            .order_by_asc(backup_schedules::Column::Id)
            .all(self.db.as_ref())
            .await
            .map_err(|error| ManagedBackupScheduleError::Lookup {
                s3_source_id,
                reason: error.to_string(),
            })
    }
}

fn summary(model: backup_schedules::Model) -> ManagedBackupSchedule {
    ManagedBackupSchedule {
        id: model.id,
        name: model.name,
        schedule_expression: model.schedule_expression,
        retention_period: model.retention_period,
        enabled: model.enabled,
        next_run: model.next_run,
    }
}

/// Whether a schedule still carries the tag enrollment put on it.
pub fn is_managed_schedule(tags: &str) -> bool {
    serde_json::from_str::<Vec<String>>(tags)
        .map(|tags| tags.iter().any(|tag| tag == MANAGED_BACKUP_SCHEDULE_TAG))
        .unwrap_or(false)
}

/// The request enrollment creates: every database and the control plane,
/// nightly, kept for `retention_days`. Kept separate so a test can pin it.
pub fn default_managed_schedule_request(
    s3_source_id: i32,
    retention_days: u16,
) -> CreateBackupScheduleRequest {
    CreateBackupScheduleRequest {
        name: MANAGED_BACKUP_SCHEDULE_NAME.to_string(),
        backup_type: "full".to_string(),
        retention_period: i32::from(retention_days.max(1)),
        s3_source_id: Some(s3_source_id),
        schedule_expression: MANAGED_BACKUP_SCHEDULE_EXPRESSION.to_string(),
        enabled: true,
        description: Some(
            "Created when this instance connected to Temps Cloud. Edit it freely: the name, \
             time and retention are yours."
                .to_string(),
        ),
        tags: vec![MANAGED_BACKUP_SCHEDULE_TAG.to_string()],
        max_runtime_secs: None,
        target_all_services: Some(true),
        include_control_plane: Some(true),
        service_ids: Vec::new(),
    }
}

#[async_trait]
impl ManagedBackupScheduleProvisioner for ManagedScheduleProvisioner {
    async fn schedule_for_source(
        &self,
        s3_source_id: i32,
    ) -> Result<Option<ManagedBackupSchedule>, ManagedBackupScheduleError> {
        Ok(self
            .schedules_for_source(s3_source_id)
            .await?
            .into_iter()
            .next()
            .map(summary))
    }

    async fn ensure_schedule_for_source(
        &self,
        s3_source_id: i32,
        retention_days: u16,
    ) -> Result<ManagedBackupSchedule, ManagedBackupScheduleError> {
        let _serialised = self.ensure.lock().await;
        if let Some(existing) = self.schedule_for_source(s3_source_id).await? {
            return Ok(existing);
        }
        self.backups
            .create_backup_schedule(default_managed_schedule_request(
                s3_source_id,
                retention_days,
            ))
            .await
            .map(summary)
            .map_err(|error| ManagedBackupScheduleError::Create {
                s3_source_id,
                reason: error.to_string(),
            })
    }

    async fn release_schedules_for_source(
        &self,
        s3_source_id: i32,
    ) -> Result<ReleasedManagedBackupSchedules, ManagedBackupScheduleError> {
        let _serialised = self.ensure.lock().await;
        let mut released = ReleasedManagedBackupSchedules::default();
        for schedule in self.schedules_for_source(s3_source_id).await? {
            let release =
                |error: crate::services::BackupError| ManagedBackupScheduleError::Release {
                    s3_source_id,
                    reason: format!("schedule {} ({}): {error}", schedule.id, schedule.name),
                };
            if is_managed_schedule(&schedule.tags) {
                self.backups
                    .delete_backup_schedule(schedule.id)
                    .await
                    .map_err(release)?;
                released.deleted.push(schedule.id);
            } else if schedule.enabled {
                self.backups
                    .disable_backup_schedule(schedule.id)
                    .await
                    .map_err(release)?;
                released.disabled.push(schedule.id);
            }
        }
        Ok(released)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_schedule_covers_everything_nightly_with_the_plan_retention() {
        let request = default_managed_schedule_request(7, 30);
        assert_eq!(request.s3_source_id, Some(7));
        assert_eq!(request.retention_period, 30);
        assert_eq!(request.schedule_expression, "0 0 2 * * *");
        assert_eq!(request.target_all_services, Some(true));
        assert_eq!(request.include_control_plane, Some(true));
        assert!(request.service_ids.is_empty());
        assert!(request.enabled);
        assert!(is_managed_schedule(
            &serde_json::to_string(&request.tags).expect("tags serialise")
        ));
    }

    /// `retention_period` must be at least one day; a backend that sends
    /// zero must not produce an unschedulable request.
    #[test]
    fn a_zero_retention_from_the_backend_is_floored_to_one_day() {
        assert_eq!(default_managed_schedule_request(1, 0).retention_period, 1);
    }

    /// An operator who removes the tag has claimed the schedule: disconnect
    /// disables it instead of deleting it. Malformed tags count as claimed.
    #[test]
    fn only_a_schedule_still_tagged_temps_cloud_is_treated_as_managed() {
        assert!(is_managed_schedule(r#"["temps-cloud"]"#));
        assert!(is_managed_schedule(r#"["nightly","temps-cloud"]"#));
        assert!(!is_managed_schedule(r#"["nightly"]"#));
        assert!(!is_managed_schedule("[]"));
        assert!(!is_managed_schedule("not json"));
    }
}
