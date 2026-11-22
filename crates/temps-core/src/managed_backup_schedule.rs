// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The seam between the Cloud link and the backup scheduler (ADR-044).
//!
//! Enrolling an instance in Temps Cloud provisions a managed backup
//! destination: an `s3_sources` row the backup engines write to directly.
//! A destination with no schedule pointing at it never receives a backup, so
//! the offer's "nightly backups" is only true once a schedule exists. The
//! Cloud plugin (`temps-cloud`) knows when the destination appears; the
//! scheduler (`temps-backup`) knows how to create a schedule; neither crate
//! depends on the other. This trait is how the first asks the second, the
//! same shape as [`CloudTelemetryActivationTrigger`](crate::CloudTelemetryActivationTrigger).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use thiserror::Error;
use utoipa::ToSchema;

/// Retention the default schedule keeps when Cloud does not say otherwise:
/// the Starter offer's seven days. Cloud sends the plan's own figure on the
/// managed backup capability once it knows it.
pub const DEFAULT_MANAGED_BACKUP_RETENTION_DAYS: u16 = 7;

/// Name of the schedule the enrollment creates. It is an ordinary schedule
/// the operator may rename, retarget or delete; the name only makes its
/// origin recognisable in the list.
pub const MANAGED_BACKUP_SCHEDULE_NAME: &str = "Temps Cloud nightly";

/// Six-field cron: every day at 02:00 server time, when most databases are
/// quietest.
pub const MANAGED_BACKUP_SCHEDULE_EXPRESSION: &str = "0 0 2 * * *";

/// What the Cloud settings page shows about the schedule that targets the
/// managed destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ManagedBackupSchedule {
    pub id: i32,
    pub name: String,
    pub schedule_expression: String,
    /// Days each backup is kept before the schedule's retention deletes it.
    pub retention_period: i32,
    pub enabled: bool,
    #[schema(value_type = Option<String>)]
    pub next_run: Option<DateTime<Utc>>,
}

/// A service whose continuous archive (Postgres WAL-G, MariaDB binlogs) is
/// pinned to a source other than the managed destination. The nightly Cloud
/// schedule cannot back it up: archiving must not silently move between
/// sources, so every run of that service fails until the operator repoints
/// it to Cloud or points its own schedule at the pinned source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ManagedBackupArchiveConflict {
    pub service_id: i32,
    pub service_name: String,
    pub service_type: String,
    pub pinned_s3_source_id: i32,
    /// Name of the pinned source, or its id as text when the row is gone.
    pub pinned_s3_source_name: String,
}

/// What releasing a destination's schedules did (see
/// [`ManagedBackupScheduleProvisioner::release_schedules_for_source`]).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReleasedManagedBackupSchedules {
    /// Schedules enrollment created, now deleted.
    pub deleted: Vec<i32>,
    /// Schedules the operator created or claimed, now disabled.
    pub disabled: Vec<i32>,
}

/// Who is asking for a backup schedule whose reach is the whole instance.
///
/// A schedule that targets every external service or includes the control
/// plane dumps Temps's own database -- every project's configuration and
/// every encrypted secret -- so creating, retargeting or deleting one is an
/// instance-administrator decision, whatever permissions the caller's role or
/// API key otherwise holds. Handlers derive this from
/// `AuthContext::is_instance_admin()`; the service layers that own such
/// schedules take it as a parameter and refuse `Scoped`, so a future caller
/// that skips the handler check still cannot provision one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupScheduleAuthority {
    /// An Admin or PlatformAdmin principal, or the instance acting on its own
    /// behalf with no request principal (unattended first-boot enrollment,
    /// credential rotation, releasing schedules before a credential is
    /// revoked).
    InstanceAdmin,
    /// Any other principal, including a `Role::Custom` user or API key that
    /// holds `settings:write` and `backups:write`, and every deployment token.
    /// May manage project-scoped schedules only.
    Scoped,
}

impl BackupScheduleAuthority {
    /// The authority of a request principal, from whether it runs the
    /// instance (`AuthContext::is_instance_admin()`).
    pub fn from_instance_admin(is_instance_admin: bool) -> Self {
        if is_instance_admin {
            Self::InstanceAdmin
        } else {
            Self::Scoped
        }
    }

    pub fn is_instance_admin(self) -> bool {
        matches!(self, Self::InstanceAdmin)
    }
}

#[derive(Debug, Error)]
pub enum ManagedBackupScheduleError {
    #[error(
        "Only an instance administrator may create the managed backup schedule for S3 source \
         {s3_source_id}: it backs up every database and the control plane"
    )]
    NotAuthorized { s3_source_id: i32 },
    #[error("Could not read the backup schedules targeting S3 source {s3_source_id}: {reason}")]
    Lookup { s3_source_id: i32, reason: String },
    #[error("Could not create the default backup schedule for S3 source {s3_source_id}: {reason}")]
    Create { s3_source_id: i32, reason: String },
    #[error("Could not release the backup schedules targeting S3 source {s3_source_id}: {reason}")]
    Release { s3_source_id: i32, reason: String },
}

/// Registered by the backup plugin as `Arc<dyn ManagedBackupScheduleProvisioner>`
/// and resolved by the Cloud plugin once every service is registered. The
/// backup plugin is optional: the Cloud plugin degrades to "no schedule
/// information" when nothing registered an implementation.
#[async_trait]
pub trait ManagedBackupScheduleProvisioner: Send + Sync {
    /// The schedule that targets `s3_source_id`, when one exists. Several
    /// may; the oldest wins, because that is the one enrollment created or
    /// the operator set up first.
    async fn schedule_for_source(
        &self,
        s3_source_id: i32,
    ) -> Result<Option<ManagedBackupSchedule>, ManagedBackupScheduleError>;

    /// Return the schedule targeting `s3_source_id`, creating the default
    /// nightly one (every database plus the control plane, `retention_days`
    /// of retention) when none does. Idempotent: an existing schedule is
    /// returned untouched, whatever the operator has changed on it.
    ///
    /// The default schedule is instance-wide, so a `Scoped` caller is refused
    /// with [`ManagedBackupScheduleError::NotAuthorized`] before anything is
    /// looked up or created.
    async fn ensure_schedule_for_source(
        &self,
        s3_source_id: i32,
        retention_days: u16,
        authority: BackupScheduleAuthority,
    ) -> Result<ManagedBackupSchedule, ManagedBackupScheduleError>;

    /// Services whose continuous archive is pinned to a source other than
    /// `s3_source_id`. Empty when every archiving service already writes to
    /// it or has not been pinned yet (unpinned services default to the
    /// managed destination).
    async fn archive_conflicts_for_source(
        &self,
        s3_source_id: i32,
    ) -> Result<Vec<ManagedBackupArchiveConflict>, ManagedBackupScheduleError>;

    /// Stop every schedule targeting `s3_source_id`, for the moment the
    /// destination's credential is about to be revoked. The schedule
    /// enrollment created (still tagged `temps-cloud`) is deleted; any other
    /// is disabled, so the operator's configuration survives but nothing
    /// keeps running against a dead credential.
    async fn release_schedules_for_source(
        &self,
        s3_source_id: i32,
    ) -> Result<ReleasedManagedBackupSchedules, ManagedBackupScheduleError>;
}
