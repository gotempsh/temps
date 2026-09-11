// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The backup scheduler's side of the managed-destination seam (ADR-044):
//! how a Cloud-provisioned `s3_sources` row gets its nightly schedule.

use async_trait::async_trait;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use temps_core::{
    ManagedBackupSchedule, ManagedBackupScheduleError, ManagedBackupScheduleProvisioner,
    MANAGED_BACKUP_SCHEDULE_EXPRESSION, MANAGED_BACKUP_SCHEDULE_NAME,
};
use temps_entities::backup_schedules;

use crate::handlers::backup_handler::CreateBackupScheduleRequest;
use crate::services::BackupService;

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
        tags: vec!["temps-cloud".to_string()],
        max_runtime_secs: None,
        target_all_services: Some(true),
        include_control_plane: Some(true),
        service_ids: Vec::new(),
    }
}

#[async_trait]
impl ManagedBackupScheduleProvisioner for BackupService {
    async fn schedule_for_source(
        &self,
        s3_source_id: i32,
    ) -> Result<Option<ManagedBackupSchedule>, ManagedBackupScheduleError> {
        backup_schedules::Entity::find()
            .filter(backup_schedules::Column::S3SourceId.eq(s3_source_id))
            .order_by_asc(backup_schedules::Column::Id)
            .one(self.db.as_ref())
            .await
            .map(|model| model.map(summary))
            .map_err(|error| ManagedBackupScheduleError::Lookup {
                s3_source_id,
                reason: error.to_string(),
            })
    }

    async fn ensure_schedule_for_source(
        &self,
        s3_source_id: i32,
        retention_days: u16,
    ) -> Result<ManagedBackupSchedule, ManagedBackupScheduleError> {
        if let Some(existing) = self.schedule_for_source(s3_source_id).await? {
            return Ok(existing);
        }
        self.create_backup_schedule(default_managed_schedule_request(
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
    }

    /// `retention_period` must be at least one day; a backend that sends
    /// zero must not produce an unschedulable request.
    #[test]
    fn a_zero_retention_from_the_backend_is_floored_to_one_day() {
        assert_eq!(default_managed_schedule_request(1, 0).retention_period, 1);
    }
}
