// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The backup scheduler's side of the managed-destination seam (ADR-044):
//! how a Cloud-provisioned `s3_sources` row gets its nightly schedule, and
//! how it loses it again on disconnect.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{
    sea_query::Expr, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
};
use std::collections::HashSet;
use temps_core::{
    ManagedBackupArchiveConflict, ManagedBackupSchedule, ManagedBackupScheduleError,
    ManagedBackupScheduleProvisioner, ReleasedManagedBackupSchedules,
    MANAGED_BACKUP_SCHEDULE_EXPRESSION, MANAGED_BACKUP_SCHEDULE_NAME,
};

use temps_entities::{backup_schedule_services, backup_schedules, external_services, s3_sources};
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

/// Pair every service the Cloud schedule targets that is pinned somewhere
/// other than `managed_source_id` with the name of where it is pinned.
/// `targeted` is `None` when the schedule (existing, or the default one
/// "ensure" would create) covers every service. Pure so a test can pin
/// the rule.
pub fn archive_conflicts(
    services: &[external_services::Model],
    sources: &[s3_sources::Model],
    managed_source_id: i32,
    targeted: Option<&HashSet<i32>>,
) -> Vec<ManagedBackupArchiveConflict> {
    services
        .iter()
        .filter_map(|service| {
            let pinned = service.continuous_archive_s3_source_id?;
            if pinned == managed_source_id {
                return None;
            }
            if targeted.is_some_and(|targeted| !targeted.contains(&service.id)) {
                return None;
            }
            let pinned_s3_source_name = sources
                .iter()
                .find(|source| source.id == pinned)
                .map(|source| source.name.clone())
                .unwrap_or_else(|| format!("S3 source {pinned}"));
            Some(ManagedBackupArchiveConflict {
                service_id: service.id,
                service_name: service.name.clone(),
                service_type: service.service_type.clone(),
                pinned_s3_source_id: pinned,
                pinned_s3_source_name,
            })
        })
        .collect()
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

    async fn archive_conflicts_for_source(
        &self,
        s3_source_id: i32,
    ) -> Result<Vec<ManagedBackupArchiveConflict>, ManagedBackupScheduleError> {
        let lookup = |error: sea_orm::DbErr| ManagedBackupScheduleError::Lookup {
            s3_source_id,
            reason: error.to_string(),
        };
        let services = external_services::Entity::find()
            .filter(external_services::Column::ContinuousArchiveS3SourceId.is_not_null())
            .filter(external_services::Column::ContinuousArchiveS3SourceId.ne(s3_source_id))
            .order_by_asc(external_services::Column::Id)
            .all(self.db.as_ref())
            .await
            .map_err(lookup)?;
        if services.is_empty() {
            return Ok(Vec::new());
        }
        // Only a service the Cloud schedule targets can fail under it. With
        // no schedule yet, the default one "ensure" creates targets every
        // service, so every pinned-elsewhere service counts.
        let schedules = self.schedules_for_source(s3_source_id).await?;
        let targeted = if schedules.is_empty()
            || schedules
                .iter()
                .any(|schedule| schedule.target_all_services)
        {
            None
        } else {
            let ids: Vec<i32> = schedules.iter().map(|schedule| schedule.id).collect();
            Some(
                backup_schedule_services::Entity::find()
                    .filter(backup_schedule_services::Column::ScheduleId.is_in(ids))
                    .all(self.db.as_ref())
                    .await
                    .map_err(lookup)?
                    .into_iter()
                    .map(|row| row.service_id)
                    .collect::<HashSet<i32>>(),
            )
        };
        let sources = s3_sources::Entity::find()
            .all(self.db.as_ref())
            .await
            .map_err(lookup)?;
        Ok(archive_conflicts(
            &services,
            &sources,
            s3_source_id,
            targeted.as_ref(),
        ))
    }

    async fn release_schedules_for_source(
        &self,
        s3_source_id: i32,
    ) -> Result<ReleasedManagedBackupSchedules, ManagedBackupScheduleError> {
        let _serialised = self.ensure.lock().await;
        let schedules = self.schedules_for_source(s3_source_id).await?;
        let mut released = ReleasedManagedBackupSchedules {
            deleted: Vec::new(),
            disabled: schedules
                .iter()
                .filter(|schedule| schedule.enabled && !is_managed_schedule(&schedule.tags))
                .map(|schedule| schedule.id)
                .collect(),
        };

        // One statement stops everything first. Whatever fails after this
        // point, nothing targeting the destination fires again: the only
        // partial state a caller can observe is "disabled, not yet deleted",
        // which a retry completes.
        backup_schedules::Entity::update_many()
            .col_expr(backup_schedules::Column::Enabled, Expr::value(false))
            .filter(backup_schedules::Column::S3SourceId.eq(s3_source_id))
            .exec(self.db.as_ref())
            .await
            .map_err(|error| ManagedBackupScheduleError::Release {
                s3_source_id,
                reason: format!("disabling the schedules: {error}"),
            })?;

        for schedule in schedules
            .iter()
            .filter(|schedule| is_managed_schedule(&schedule.tags))
        {
            self.backups
                .delete_backup_schedule(schedule.id)
                .await
                .map_err(|error| ManagedBackupScheduleError::Release {
                    s3_source_id,
                    reason: format!(
                        "deleting schedule {} ({}): {error}",
                        schedule.id, schedule.name
                    ),
                })?;
            released.deleted.push(schedule.id);
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

    fn service(id: i32, name: &str, pinned: Option<i32>) -> external_services::Model {
        external_services::Model {
            id,
            name: name.to_string(),
            service_type: "postgres".to_string(),
            topology: "standalone".to_string(),
            status: "running".to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            node_id: None,
            version: None,
            slug: None,
            config: None,
            error_message: None,
            health_status: None,
            last_health_check_at: None,
            last_health_error: None,
            consecutive_health_failures: 0,
            health_metadata: None,
            metrics_enabled: false,
            default_backup_provisioned: false,
            ai_data_access: false,
            created_by_user_id: None,
            container_name: None,
            continuous_archive_s3_source_id: pinned,
            continuous_archive_pinned_at: None,
        }
    }

    fn source(id: i32, name: &str) -> s3_sources::Model {
        s3_sources::Model {
            id,
            name: name.to_string(),
            bucket_name: "bucket".to_string(),
            region: "auto".to_string(),
            endpoint: None,
            bucket_path: String::new(),
            access_key_id: "enc".to_string(),
            secret_key: "enc".to_string(),
            session_token: None,
            credentials_expire_at: None,
            force_path_style: None,
            is_default: false,
            managed_by_cloud: false,
            lifecycle_reconcile_failed_at: None,
            lifecycle_reconcile_generation: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            backing_service_id: None,
        }
    }

    /// Only a pin that points elsewhere is a conflict: unpinned services
    /// default to the managed destination, pinned-to-managed ones already
    /// write there. A pinned source whose row is gone is still named.
    #[test]
    fn only_services_pinned_elsewhere_conflict() {
        let sources = vec![source(3, "My own bucket"), source(9, "Temps Cloud")];
        let services = vec![
            service(1, "pg-main", None),
            service(2, "pg-cloud", Some(9)),
            service(3, "pg-own", Some(3)),
            service(4, "maria-gone", Some(42)),
        ];
        let conflicts = archive_conflicts(&services, &sources, 9, None);
        assert_eq!(
            conflicts
                .iter()
                .map(|c| (c.service_id, c.pinned_s3_source_name.as_str()))
                .collect::<Vec<_>>(),
            vec![(3, "My own bucket"), (4, "S3 source 42")]
        );
    }

    /// A Cloud schedule that targets selected services only conflicts with
    /// the pinned-elsewhere services among those; the rest are not its
    /// business.
    #[test]
    fn conflicts_are_scoped_to_the_schedule_targets() {
        let sources = vec![source(3, "My own bucket")];
        let services = vec![
            service(3, "pg-own", Some(3)),
            service(4, "maria-own", Some(3)),
        ];
        let targeted: HashSet<i32> = [4].into_iter().collect();
        let conflicts = archive_conflicts(&services, &sources, 9, Some(&targeted));
        assert_eq!(
            conflicts.iter().map(|c| c.service_id).collect::<Vec<_>>(),
            vec![4]
        );
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
