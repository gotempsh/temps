// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Generic restore orchestrator.
//!
//! Takes a restore request (backup id + mode), writes a `restore_runs` row,
//! and drives the per-engine `ExternalService` trait methods
//! (`restore_from_s3`, `restore_to_new_service`, `restore_pitr`) to
//! completion in a spawned task. Handlers poll `restore_runs` for progress.

use aws_sdk_s3::Client as S3Client;
use chrono::Utc;
use sea_orm::ActiveValue::{NotSet, Set};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use temps_providers::externalsvc::{RecoveryTarget, RestoreContext, ServiceType};
use temps_providers::{ExternalServiceManager, S3Credentials};
use thiserror::Error;
use tracing::{error, info, warn};
use utoipa::ToSchema;

#[derive(Error, Debug)]
pub enum RestoreError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("Backup {backup_id} not found")]
    BackupNotFound { backup_id: i32 },

    #[error("Backup {backup_id} is being deleted and cannot be restored")]
    BackupDeleting { backup_id: i32 },

    #[error(
        "Backup {backup_id} is in state '{state}' and cannot be restored: only a completed \
         backup holds a full snapshot. Wait for it to finish, or pick a completed backup."
    )]
    BackupNotRestorable { backup_id: i32, state: String },

    #[error("External service {service_id} not found")]
    ServiceNotFound { service_id: i32 },

    #[error("S3 source {s3_source_id} not found")]
    S3SourceNotFound { s3_source_id: i32 },

    #[error("Restore run {restore_run_id} not found")]
    RestoreRunNotFound { restore_run_id: i32 },

    #[error("Backup {backup_id} is not linked to a service — cannot restore")]
    BackupHasNoService { backup_id: i32 },

    #[error("Validation error: {message}")]
    Validation { message: String },

    #[error(
        "Cannot read the restore options of service {service_id} ('{service_name}'): {reason}"
    )]
    CapabilitiesUnavailable {
        service_id: i32,
        service_name: String,
        reason: String,
    },

    #[error("Cannot reserve restored service name '{name}': {reason}")]
    ServiceNameConflict { name: String, reason: String },

    #[error("Restore mode '{mode}' not supported by service type '{service_type}'")]
    UnsupportedMode { mode: String, service_type: String },

    #[error(
        "Restoring {backup} onto service {target_service_id} would overwrite it with data \
         from a different service ({origin}). Re-submit with `confirm_cross_service: true` \
         (CLI: `--confirm-cross-service`) to confirm this cross-service restore."
    )]
    CrossServiceRestoreNotConfirmed {
        target_service_id: i32,
        backup: String,
        origin: String,
    },

    #[error(
        "Service {service_id} already has an active restore (run {restore_run_id}). \
         Wait for it to finish, or open that run to check its status, before starting \
         another restore onto this service."
    )]
    RestoreAlreadyActive {
        service_id: i32,
        restore_run_id: i32,
    },

    #[error(
        "Service {service_id} is receiving imported data (data import {data_import_run_id}). \
         Wait for the import to finish, or cancel it, before restoring onto this service."
    )]
    DataImportActive {
        service_id: i32,
        data_import_run_id: i32,
    },

    #[error("Restore run {restore_run_id} is already {status}, so there is nothing to cancel")]
    RestoreNotActive { restore_run_id: i32, status: String },

    #[error(
        "Restore run {restore_run_id} worker stopped unexpectedly during phase '{phase}': {reason}"
    )]
    WorkerCrashed {
        restore_run_id: i32,
        phase: String,
        reason: String,
    },

    /// Signal from a guarded phase move: a cancellation was recorded before
    /// the worker could leave `phase`. The worker turns it into
    /// [`RestoreError::Cancelled`] once it knows what, if anything, it has to
    /// clean up.
    #[error("Restore run {restore_run_id} was cancelled during phase '{phase}'")]
    RestoreCancelled { restore_run_id: i32, phase: String },

    /// Terminal outcome of a cancelled run; `message` says what was left
    /// behind and is stored on the run.
    #[error("{message}")]
    Cancelled {
        restore_run_id: i32,
        phase: String,
        message: String,
    },

    /// A cancelled run staged its backup with a process that could not be
    /// confirmed stopped. The run is kept active, holding the service, so no
    /// other restore reuses the scratch data while it may still be written.
    #[error(
        "Restore run {restore_run_id} was cancelled, but its staged download in {target} could \
         not be confirmed stopped ({reason}). The run stays active so no other restore uses the \
         service until the download is confirmed gone; the next server start retries the check."
    )]
    StagedRestoreStillRunning {
        restore_run_id: i32,
        target: String,
        reason: String,
    },

    #[error("Restore run {restore_run_id} cannot be cancelled in phase '{phase}': {reason}")]
    RestoreNotCancellable {
        restore_run_id: i32,
        phase: String,
        reason: String,
    },

    #[error("Encryption error: {reason}")]
    Encryption { reason: String },

    #[error("External service error: {reason}")]
    ExternalService { reason: String },

    #[error("Internal error: {reason}")]
    Internal { reason: String },
}

mod worker_mariadb;
mod worker_mongodb;
mod worker_redis;
mod worker_walg;

fn selected_walg_target_user_data(
    backup: &temps_entities::backups::Model,
) -> Result<Option<String>, RestoreError> {
    let metadata: serde_json::Value =
        serde_json::from_str(&backup.metadata).map_err(|error| RestoreError::Validation {
            message: format!(
                "Backup {} has invalid metadata JSON: {}",
                backup.backup_id, error
            ),
        })?;
    let Some(version) = metadata.get("walg_identity_version") else {
        return Ok(None);
    };
    if version.as_u64() != Some(1) {
        return Err(RestoreError::Validation {
            message: format!(
                "Backup {} uses unsupported WAL-G identity version {}",
                backup.backup_id, version
            ),
        });
    }
    let value = metadata
        .get("walg_target_user_data")
        .ok_or_else(|| RestoreError::Validation {
            message: format!(
                "Backup {} is missing its WAL-G target user data",
                backup.backup_id
            ),
        })?;
    if value
        .get("temps_backup_id")
        .and_then(serde_json::Value::as_str)
        != Some(backup.backup_id.as_str())
    {
        return Err(RestoreError::Validation {
            message: format!(
                "Backup {} has WAL-G target user data for a different backup",
                backup.backup_id
            ),
        });
    }
    serde_json::to_string(value)
        .map(Some)
        .map_err(|error| RestoreError::Validation {
            message: format!(
                "Backup {} has invalid WAL-G target user data: {}",
                backup.backup_id, error
            ),
        })
}

/// How the caller identifies which backup to restore.
///
/// Two flavors, because disaster-recovery means we can't always assume a
/// backup has a row in our DB:
/// - `Id` — the backup exists in `backups` (normal case, produced by
///   this Temps instance).
/// - `Location` — caller hands us a raw S3 URL / key discovered via
///   bucket scan. Used when restoring from another Temps instance's
///   backups that we've never ingested.
#[derive(Debug, Clone)]
pub enum BackupSelector {
    Id(i32),
    Location {
        location: String,
        engine: String,
        s3_source_id: i32,
    },
}

/// What the caller wants to do. Mirrors `externalsvc::RestoreMode` but
/// flattened for JSON over the wire.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RestoreRequestMode {
    /// Restore the backup onto the existing service (destructive).
    InPlace,
    /// Provision a new service and restore into it.
    NewService {
        /// Name for the new service. Orchestrator auto-suggests
        /// `{source}-restore-{yyyymmdd-hhmm}` if caller omits, but we require
        /// an explicit value at the API boundary.
        name: String,
        /// Optional parameter overrides (port, docker_image, database).
        #[serde(default)]
        parameter_overrides: serde_json::Value,
    },
    /// Point-in-time recovery. Only valid on a backup that anchors a
    /// continuous change stream: a WAL-G base (Postgres) or a physical
    /// `mariadb-backup` base with archived binary logs (MariaDB).
    Pitr {
        /// Whether PITR restores in place or creates a new service.
        to_new_service: bool,
        /// Required when `to_new_service` is true.
        new_service_name: Option<String>,
        /// Recovery target kind + value.
        target: RecoveryTarget,
    },
}

/// How a backup relates to the service it is being restored onto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreOriginBinding {
    /// Services recorded as having produced the backup. Empty when the origin
    /// is unknown: a raw-location (S3 scan) backup, or a backup with no
    /// owning service.
    pub origin_service_ids: Vec<i32>,
    /// True when the target is not one of the backup's origin services,
    /// including when the origin is unknown.
    pub cross_service: bool,
}

impl RestoreOriginBinding {
    /// Classify `origin_service_ids` against `target_service_id`.
    pub fn new(target_service_id: i32, origin_service_ids: Vec<i32>) -> Self {
        let cross_service = !origin_service_ids.contains(&target_service_id);
        Self {
            origin_service_ids,
            cross_service,
        }
    }

    /// Enforce the identity binding for a restore request.
    ///
    /// A destructive restore (in place, or PITR in place) onto a service that
    /// did not produce the backup is refused unless the caller explicitly
    /// confirmed it. Non-destructive modes provision a fresh service and are
    /// never blocked: there is no existing data to overwrite.
    pub fn require_confirmation(
        &self,
        target_service_id: i32,
        selector: &BackupSelector,
        mode: &RestoreRequestMode,
        confirmed: bool,
    ) -> Result<(), RestoreError> {
        if !self.cross_service || !mode.is_destructive() || confirmed {
            return Ok(());
        }
        let backup = match selector {
            BackupSelector::Id(id) => format!("backup {id}"),
            BackupSelector::Location { s3_source_id, .. } => {
                format!("a raw backup location on S3 source {s3_source_id}")
            }
        };
        let origin = if self.origin_service_ids.is_empty() {
            "origin service unknown".to_string()
        } else {
            format!(
                "produced by service(s) {}",
                self.origin_service_ids
                    .iter()
                    .map(i32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        Err(RestoreError::CrossServiceRestoreNotConfirmed {
            target_service_id,
            backup,
            origin,
        })
    }
}

impl RestoreRequestMode {
    /// Whether this mode overwrites the target service's existing data.
    pub fn is_destructive(&self) -> bool {
        matches!(
            self,
            RestoreRequestMode::InPlace
                | RestoreRequestMode::Pitr {
                    to_new_service: false,
                    ..
                }
        )
    }

    /// Name of the service this run creates, for modes that create one.
    pub fn new_service_name(&self) -> Option<&str> {
        match self {
            RestoreRequestMode::InPlace => None,
            RestoreRequestMode::NewService { name, .. } => Some(name),
            RestoreRequestMode::Pitr {
                to_new_service: true,
                new_service_name,
                ..
            } => new_service_name.as_deref(),
            RestoreRequestMode::Pitr { .. } => None,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            RestoreRequestMode::InPlace => "in_place",
            RestoreRequestMode::NewService { .. } => "new_service",
            RestoreRequestMode::Pitr { .. } => "pitr",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RestoreRunView {
    pub id: i32,
    pub source_backup_id: i32,
    pub source_service_id: i32,
    pub target_service_id: Option<i32>,
    pub target_service_name: Option<String>,
    pub mode: String,
    pub status: String,
    pub phase: String,
    pub recovery_target: Option<serde_json::Value>,
    pub error_message: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub created_at: String,
    /// When cancellation was requested, if it was.
    pub cancel_requested_at: Option<String>,
    /// Whether `POST /restore-runs/{id}/cancel` would be accepted now.
    pub cancellable: bool,
    /// Why the run cannot be cancelled now; `None` when `cancellable`.
    pub not_cancellable_reason: Option<String>,
    /// The backup this run restores from, so a client can show and link to
    /// it. Always present; its fields are `None` where they are unknown.
    pub source_backup: RestoreRunSourceBackup,
}

/// The backup a restore run reads.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct RestoreRunSourceBackup {
    /// `backups.id` of a tracked backup. `None` for a restore from a raw S3
    /// location (a backup this instance did not record).
    pub id: Option<i32>,
    /// The backup's UUID, which addresses it in the console and the
    /// `/backups/{id}` API. `None` for a raw-location restore, or when the
    /// tracked backup has since been deleted.
    pub backup_id: Option<String>,
    /// S3 source holding the backup.
    pub s3_source_id: Option<i32>,
    /// Object key or `s3://` URL of the backup.
    pub location: Option<String>,
    /// When the backup was taken (ISO 8601). `None` when unknown.
    pub taken_at: Option<String>,
}

impl RestoreRunSourceBackup {
    /// What the run row alone records: the tracked id, or the raw location
    /// and S3 source stored in its resume token.
    fn from_run(run: &temps_entities::restore_runs::Model) -> Self {
        let token = run.resume_token.as_ref();
        Self {
            id: (run.source_backup_id > 0).then_some(run.source_backup_id),
            backup_id: None,
            s3_source_id: token
                .and_then(|t| t.get("s3_source_id"))
                .and_then(|v| v.as_i64())
                .and_then(|v| i32::try_from(v).ok()),
            location: token
                .and_then(|t| t.get("backup_location"))
                .and_then(|v| v.as_str())
                .filter(|l| !l.is_empty())
                .map(String::from),
            taken_at: None,
        }
    }

    fn from_backup(backup: &temps_entities::backups::Model) -> Self {
        Self {
            id: Some(backup.id),
            backup_id: Some(backup.backup_id.clone()),
            s3_source_id: Some(backup.s3_source_id),
            location: Some(backup.s3_location.clone()).filter(|l| !l.is_empty()),
            taken_at: Some(
                backup
                    .started_at
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            ),
        }
    }
}

/// Non-sensitive service identity used by restore handlers for response
/// labels and audit records. Loading stays in the service layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreServiceIdentity {
    pub id: i32,
    pub name: String,
    pub service_type: String,
}

/// External services that produced one backup, resolved in a batched service
/// query for authorization. An empty `service_ids` list is authoritative and
/// identifies a control-plane/ownerless backup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupProducerServices {
    pub backup_id: i32,
    pub service_ids: Vec<i32>,
}

impl From<temps_entities::restore_runs::Model> for RestoreRunView {
    fn from(m: temps_entities::restore_runs::Model) -> Self {
        let not_cancellable_reason = super::restore_cancel::not_cancellable_reason(&m);
        let source_backup = RestoreRunSourceBackup::from_run(&m);
        Self {
            source_backup,
            cancel_requested_at: m.cancel_requested_at.map(|d| d.to_rfc3339()),
            cancellable: not_cancellable_reason.is_none(),
            not_cancellable_reason,
            id: m.id,
            source_backup_id: m.source_backup_id,
            source_service_id: m.source_service_id,
            target_service_id: m.target_service_id,
            target_service_name: m.target_service_name,
            mode: m.mode,
            status: m.status,
            phase: m.phase,
            recovery_target: m.recovery_target,
            error_message: m.error_message,
            started_at: m.started_at.map(|d| d.to_rfc3339()),
            finished_at: m.finished_at.map(|d| d.to_rfc3339()),
            created_at: m.created_at.to_rfc3339(),
        }
    }
}

/// Preview of a restore operation. Answers "what will happen if I click
/// start?" with engine-level specificity so the user can confirm before
/// committing to a destructive action.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RestorePlan {
    /// Target engine ("postgres", etc.).
    pub engine: String,
    /// Service we'll operate on (or provision a sibling of).
    pub target_service: PlanTarget,
    /// Backup we'll read from.
    pub source_backup: PlanSourceBackup,
    /// How the restore will be performed: "walg_restore", "pg_dump_restore",
    /// "mariadb_physical_restore", "mariadb_dump_restore",
    /// "mongodump_restore", "redis_rdb_restore", "s3_mirror_restore", or
    /// "unsupported".
    pub strategy: String,
    /// Ordered list of human-readable actions the orchestrator will take.
    pub steps: Vec<String>,
    /// Non-blocking caveats the user should see (cross-service, empty
    /// location that will be auto-resolved, missing engine metadata, ...).
    pub warnings: Vec<String>,
    /// Blocking problems. The UI disables the Start button when non-empty.
    pub errors: Vec<String>,
    /// Whether any step overwrites existing data on the target service.
    pub destructive: bool,
    /// Whether the backup was produced by a service other than the target
    /// (or its origin is unknown). A destructive cross-service restore must
    /// be confirmed explicitly with `confirm_cross_service: true`.
    pub cross_service: bool,
    /// Echo of the requested mode for the UI.
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlanTarget {
    pub id: i32,
    pub name: String,
    /// Expected Docker container name.
    pub container: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlanSourceBackup {
    /// DB id, absent for orphan (S3-scan) backups.
    pub id: Option<i32>,
    /// Service that originally produced the backup, if known.
    pub origin_service_name: Option<String>,
    /// Resolved S3 location the orchestrator will actually use.
    pub location: String,
    /// True when the original row's `s3_location` was empty and we resolved
    /// a location by probing S3. The UI shows this as a warning.
    pub location_was_resolved: bool,
    /// "walg", "pg_dump", "mariadb_physical", "mariadb_dump", "mongodump",
    /// "redis_rdb", "s3_mirror", "unknown".
    pub format: String,
    pub size_bytes: Option<i64>,
    pub created_at: Option<String>,
}

/// Orchestrates cross-engine restore operations by dispatching to the
/// `ExternalService` trait methods and persisting progress to
/// `restore_runs`.
pub struct RestoreService {
    db: Arc<DatabaseConnection>,
    external_service_manager: Arc<ExternalServiceManager>,
    encryption_service: Arc<temps_core::EncryptionService>,
    /// Anonymous product telemetry. No-op unless wired with
    /// [`Self::with_telemetry`].
    telemetry: Arc<dyn temps_core::telemetry::TelemetryReporter>,
    /// Cancellation signals for the restore workers this service spawned.
    cancellations: super::restore_cancel::RestoreCancellations,
    /// The workload plane, once `temps serve` starts it. In-place restores
    /// of a database on a worker node run as a job through it (ADR-055
    /// Phase 9); an empty slot refuses them with the reason.
    workload_plane: Arc<temps_workload_plane::PlaneSlot>,
    /// Derives the S3 credential a restore on a worker node receives
    /// (ADR-093).
    worker_credentials: Arc<temps_providers::worker_s3_scope::WorkerCredentialScoper>,
}

impl RestoreService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        external_service_manager: Arc<ExternalServiceManager>,
        encryption_service: Arc<temps_core::EncryptionService>,
    ) -> Self {
        // Without the shared scoper a worker restore still never receives a
        // source's own credential: this one scopes through STS and refuses
        // a managed source (it has no Cloud minter).
        let worker_credentials = Arc::new(
            temps_providers::worker_s3_scope::WorkerCredentialScoper::new(
                encryption_service.clone(),
            ),
        );
        Self {
            db,
            external_service_manager,
            encryption_service,
            telemetry: Arc::new(temps_core::telemetry::NoopTelemetryReporter),
            cancellations: super::restore_cancel::RestoreCancellations::default(),
            workload_plane: Arc::new(temps_workload_plane::PlaneSlot::new()),
            worker_credentials,
        }
    }

    /// Derive worker restores' S3 credentials with `scoper`, the process's
    /// shared one (it holds the Temps Cloud minter).
    pub fn with_worker_credentials(
        mut self,
        scoper: Arc<temps_providers::worker_s3_scope::WorkerCredentialScoper>,
    ) -> Self {
        self.worker_credentials = scoper;
        self
    }

    /// Restore databases on worker nodes through `plane` (ADR-055 Phase 9).
    pub fn with_workload_plane(mut self, plane: Arc<temps_workload_plane::PlaneSlot>) -> Self {
        self.workload_plane = plane;
        self
    }

    /// Report restore outcomes as anonymous `restore_succeeded` /
    /// `restore_failed` telemetry.
    pub fn with_telemetry(
        mut self,
        telemetry: Arc<dyn temps_core::telemetry::TelemetryReporter>,
    ) -> Self {
        self.telemetry = telemetry;
        self
    }

    /// Build an auto-suggested new-service name from a source name.
    pub fn suggest_new_service_name(source_name: &str) -> String {
        format!(
            "{}-restore-{}",
            source_name,
            Utc::now().format("%Y%m%d-%H%M")
        )
    }

    /// Read the trait-declared capabilities for a service without touching
    /// the backup itself. The handler layer uses this to decide which UI
    /// options to show.
    /// Describe what a restore would do, without executing it.
    ///
    /// The UI shows this as a preview before the user commits to running
    /// a destructive operation. The plan covers:
    ///
    /// - Which backup we'd restore from (location, format, origin).
    /// - Which target container we'd touch, and whether it's same-service or
    ///   cross-service (disaster recovery).
    /// - The concrete strategy (WAL-G base backup replay vs pg_dump import)
    ///   and the resulting container actions (stop, swap PGDATA, restart).
    /// - Warnings (non-blocking) and errors (blocking) so the UI can disable
    ///   the "Start restore" button when preconditions aren't met.
    pub async fn plan_restore(
        &self,
        target_service_id: i32,
        selector: BackupSelector,
        mode: RestoreRequestMode,
    ) -> Result<RestorePlan, RestoreError> {
        let target = self.load_service(target_service_id).await?;

        // Resolve the backup identity. We don't touch the DB beyond reads.
        let (backup_row, backup_location, backup_engine_hint, s3_source_id, backup_id_opt) =
            match &selector {
                BackupSelector::Id(id) => {
                    let backup = temps_entities::backups::Entity::find_by_id(*id)
                        .one(self.db.as_ref())
                        .await?
                        .ok_or(RestoreError::BackupNotFound { backup_id: *id })?;
                    let engine = self.resolve_backup_engine(&backup).await?;
                    (
                        Some(backup.clone()),
                        backup.s3_location.clone(),
                        engine,
                        backup.s3_source_id,
                        Some(backup.id),
                    )
                }
                BackupSelector::Location {
                    location,
                    engine,
                    s3_source_id,
                } => (
                    None,
                    location.clone(),
                    ResolvedBackupEngine::Known(engine.clone()),
                    *s3_source_id,
                    None,
                ),
            };

        let mut warnings: Vec<String> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        if let Some(backup) = &backup_row {
            if backup.state == "deleting" {
                errors.push(format!(
                    "Backup {} is being deleted and cannot be restored.",
                    backup.id
                ));
            }
        }

        // Engine compat — the same resolution `start_restore` enforces.
        match &backup_engine_hint {
            ResolvedBackupEngine::Known(engine) => {
                if !engines_compatible(engine, &target.service_type) {
                    errors.push(format!(
                        "Engine mismatch: backup is '{}' but target '{}' is '{}'.",
                        engine, target.name, target.service_type
                    ));
                }
            }
            ResolvedBackupEngine::Unknown { has_producer: true } => errors.push(
                "Cannot determine which engine produced this backup (no service_type in its \
                 metadata and its producing service is gone), so it cannot be restored safely."
                    .into(),
            ),
            ResolvedBackupEngine::Unknown {
                has_producer: false,
            } => warnings.push(
                "Could not determine the backup's engine from metadata — proceeding as if it matches the target."
                    .into(),
            ),
        }

        // Classify backup format. For missing locations (pre-fix DB rows),
        // we peek at what the orchestrator's S3 resolver would find so the
        // plan matches reality.
        let mut resolved_location = backup_location.clone();
        let mut location_was_resolved = false;
        if resolved_location.is_empty() {
            let origin_and_uuid = backup_row.as_ref().and_then(|b| {
                serde_json::from_str::<serde_json::Value>(&b.metadata)
                    .ok()
                    .and_then(|v| {
                        v.get("service_name")
                            .and_then(|s| s.as_str())
                            .map(String::from)
                    })
                    .map(|origin| (origin, b.backup_id.clone()))
            });
            if let Some((origin, backup_uuid)) = origin_and_uuid {
                if let Ok(s3_source) = temps_entities::s3_sources::Entity::find_by_id(s3_source_id)
                    .one(self.db.as_ref())
                    .await
                    .map(|o| o.ok_or(()))
                    .unwrap_or(Err(()))
                {
                    // Build an S3 client from the source and probe.
                    let decrypted_access_key = self
                        .encryption_service
                        .decrypt_string(&s3_source.access_key_id)
                        .ok();
                    let decrypted_secret_key = self
                        .encryption_service
                        .decrypt_string(&s3_source.secret_key)
                        .ok();
                    // A source without a session token yields `None` here; a
                    // decrypt failure is also treated as "no token" on this
                    // best-effort probe path, exactly like the two keys above.
                    let decrypted_session_token =
                        temps_entities::s3_sources::decrypt_session_token(
                            self.encryption_service.as_ref(),
                            &s3_source,
                        )
                        .ok()
                        .flatten();
                    if let (Some(a), Some(s)) = (decrypted_access_key, decrypted_secret_key) {
                        let creds = S3Credentials {
                            access_key_id: a,
                            secret_key: s,
                            session_token: decrypted_session_token,
                            region: s3_source.region.clone(),
                            endpoint: s3_source.endpoint.clone(),
                            bucket_name: s3_source.bucket_name.clone(),
                            bucket_path: s3_source.bucket_path.clone(),
                            force_path_style: s3_source.force_path_style.unwrap_or(true),
                        };
                        let client = build_s3_client(&creds);
                        if let Ok(Some(loc)) = resolve_backup_location_from_s3(
                            &client,
                            &s3_source,
                            &target.service_type,
                            &origin,
                            &backup_uuid,
                        )
                        .await
                        {
                            resolved_location = loc;
                            location_was_resolved = true;
                        }
                    }
                }
            }
            if resolved_location.is_empty() {
                errors.push(
                    "This backup has no s3_location and no matching object was found on the S3 source."
                        .into(),
                );
            } else {
                warnings.push(format!(
                    "Backup row has an empty s3_location; orchestrator will auto-resolve to: {}",
                    resolved_location
                ));
            }
        }

        // Strategy classification: see `classify_restore_strategy`.
        let target_is_mariadb = target.service_type.eq_ignore_ascii_case("mariadb");
        let engine_lower = target.service_type.to_ascii_lowercase();
        let strategy = classify_restore_strategy(&engine_lower, &resolved_location);

        // PITR needs a continuous change stream anchored to the base backup:
        // WAL-G's WAL archive for Postgres, archived binary logs for MariaDB's
        // physical base. A logical dump anchors nothing in either engine.
        let is_pitr = matches!(mode, RestoreRequestMode::Pitr { .. });
        if is_pitr && !matches!(strategy, "walg_restore" | "mariadb_physical_restore") {
            if target_is_mariadb {
                errors.push(
                    "PITR requires a physical (mariadb-backup) base backup. The selected backup is a logical dump; choose a physical backup or a different mode."
                        .into(),
                );
            } else {
                errors.push(format!(
                    "PITR requires a WAL-G backup. The selected backup is {}; choose a WAL-G backup or a different mode.",
                    restore_format(strategy).replace('_', " ")
                ));
            }
        }

        // Cross-service warning. The binding is decided by service identity
        // (the backup's recorded producer ids), not by name: a service that
        // was deleted and re-created under the same name is a different
        // service.
        let origin_service_name = match backup_row.as_ref() {
            Some(backup) => backup_origin_service_name(self.db.as_ref(), backup).await,
            None => None,
        };
        let origin_binding = self
            .resolve_origin_binding(target_service_id, &selector)
            .await?;
        if origin_binding.cross_service {
            let origin = origin_service_name
                .as_deref()
                .map(|name| format!("'{name}'"))
                .unwrap_or_else(|| "an unknown service".to_string());
            warnings.push(format!(
                "Cross-service restore: backup produced by {}, target is '{}'. Data will be overwritten by foreign data.",
                origin, target.name
            ));
            if mode.is_destructive() {
                warnings.push(
                    "This destructive cross-service restore must be confirmed explicitly \
                     (confirm_cross_service) before it can start."
                        .into(),
                );
            }
        }

        // Credential-propagation warning. Fires for engines whose restored
        // data carries the source's authentication state:
        //
        //   Postgres — pg_authid (system catalog inside the base backup).
        //   MongoDB  — admin.system.users is inside the mongodump archive,
        //              and we run `mongorestore --archive --drop` without
        //              excluding the admin database, so post-restore the
        //              target authenticates with the source's root password.
        //   MariaDB  — only for a PHYSICAL base. `mariadb-backup --copy-back`
        //              replaces the entire datadir, `mysql` system schema
        //              included, so `mysql.user` (the password-hash table)
        //              becomes the source's. The logical `mariadb_dump`
        //              backup explicitly excludes the `mysql` schema
        //              (`SCHEMA_NAME NOT IN (... 'mysql' ...)` in
        //              temps-backup/src/engines/mariadb_dump.rs), so a dump
        //              restore leaves the target's credentials untouched —
        //              which is why `mariadb_dump_restore` is absent from
        //              the strategy list below.
        //
        // Redis RDB and S3 object copies don't carry auth, so the warning
        // would be misleading for those.
        let engine_preserves_source_credentials = matches!(
            target.service_type.to_ascii_lowercase().as_str(),
            "postgres" | "mongodb" | "mariadb"
        );

        if mode.is_destructive()
            && engine_preserves_source_credentials
            && matches!(
                strategy,
                "walg_restore"
                    | "pg_dump_restore"
                    | "mariadb_physical_restore"
                    | "mongodump_restore"
            )
        {
            let origin_still_known = backup_row
                .as_ref()
                .and_then(|b| serde_json::from_str::<serde_json::Value>(&b.metadata).ok())
                .and_then(|v| v.get("service_id").and_then(|id| id.as_i64()))
                .is_some()
                || temps_entities::external_service_backups::Entity::find()
                    .filter(
                        temps_entities::external_service_backups::Column::BackupId
                            .eq(backup_row.as_ref().map(|b| b.id).unwrap_or(0)),
                    )
                    .one(self.db.as_ref())
                    .await
                    .ok()
                    .flatten()
                    .is_some();

            if origin_still_known {
                if let Some(origin) = origin_service_name.as_ref() {
                    if origin != &target.name {
                        warnings.push(format!(
                            "Password change: after restore, '{}' will authenticate with '{}'s password. The target service's stored config will be updated automatically so UI/env/CLI stay accurate.",
                            target.name, origin
                        ));
                    }
                }
            } else {
                warnings.push(
                    "Password change (origin unknown): after restore, the database will expect the password from the original backup's service, which this Temps no longer tracks. You may need to reset the password manually via `ALTER USER` after the restore completes."
                        .into(),
                );
            }
        }

        // Build step list. Engine-first, because each engine has its own
        // container naming, data format, and recovery mechanics. Within an
        // engine we further branch on strategy + mode.
        let container_name = engine_container_name(&engine_lower, &target.name);
        let mut steps: Vec<String> = Vec::new();
        let mut destructive = false;

        // A service on a worker node is restored there, by a job or its
        // agent: describe what actually runs, and refuse what it cannot do,
        // as `start_restore` will.
        if let Some(worker) = worker_restore_plan(
            &target,
            &resolved_location,
            &mode,
            &container_name,
            origin_binding.cross_service,
        ) {
            steps = worker.steps;
            destructive = true;
            errors.extend(worker.refusal);
        } else {
            match engine_lower.as_str() {
                "postgres" => {
                    build_postgres_steps(
                        strategy,
                        &mode,
                        &container_name,
                        &resolved_location,
                        &mut steps,
                        &mut destructive,
                        &mut errors,
                    );
                }
                "redis" => {
                    build_redis_steps(
                        strategy,
                        &mode,
                        &container_name,
                        &resolved_location,
                        &mut steps,
                        &mut destructive,
                        &mut errors,
                    );
                    if origin_binding.cross_service && matches!(mode, RestoreRequestMode::InPlace) {
                        steps.push(redis_bookkeeping_step());
                    }
                }
                "mongodb" => {
                    if let Some(warning) = build_mongodb_steps(
                        strategy,
                        &mode,
                        &container_name,
                        &resolved_location,
                        &mut steps,
                        &mut destructive,
                        &mut errors,
                    ) {
                        warnings.push(warning.into());
                    }
                }
                "mariadb" => {
                    build_mariadb_steps(
                        strategy,
                        &mode,
                        &container_name,
                        &resolved_location,
                        &mut steps,
                        &mut destructive,
                        &mut errors,
                    );
                }
                "s3" | "rustfs" | "blob" | "minio" => {
                    build_object_store_steps(
                        strategy,
                        &mode,
                        &target.name,
                        &resolved_location,
                        &mut steps,
                        &mut destructive,
                        &mut errors,
                    );
                }
                other => {
                    errors.push(format!(
                    "Restore plan preview is not implemented for engine '{}'. The orchestrator may still run, but the step-by-step preview below will be generic.",
                    other
                ));
                    steps.push(format!("Target: {}", container_name));
                    steps.push(format!(
                        "Strategy: {} (details depend on the '{}' engine)",
                        strategy, other
                    ));
                    steps.push("Dispatch restore via the engine's trait implementation".into());
                    if matches!(mode, RestoreRequestMode::InPlace) {
                        destructive = true;
                    }
                }
            }
        }

        // MinIO / RustFS reachability sanity check is done implicitly when
        // the orchestrator fires — we don't duplicate it here.

        Ok(RestorePlan {
            engine: target.service_type.clone(),
            target_service: PlanTarget {
                id: target.id,
                name: target.name.clone(),
                container: container_name,
            },
            source_backup: PlanSourceBackup {
                id: backup_id_opt,
                origin_service_name,
                location: resolved_location,
                location_was_resolved,
                format: restore_format(strategy).into(),
                size_bytes: backup_row.as_ref().and_then(|b| b.size_bytes),
                created_at: backup_row.as_ref().map(|b| b.started_at.to_rfc3339()),
            },
            strategy: strategy.to_string(),
            steps,
            warnings,
            errors,
            destructive,
            cross_service: origin_binding.cross_service,
            mode: match mode {
                RestoreRequestMode::InPlace => "in_place".into(),
                RestoreRequestMode::NewService { .. } => "new_service".into(),
                RestoreRequestMode::Pitr { .. } => "pitr".into(),
            },
        })
    }

    pub async fn get_capabilities(
        &self,
        service_id: i32,
    ) -> Result<temps_providers::externalsvc::RestoreCapabilities, RestoreError> {
        self.get_capabilities_with_identity(service_id)
            .await
            .map(|(capabilities, _)| capabilities)
    }

    /// Read restore capabilities and the already-loaded service identity in
    /// one service-layer operation so the handler does not repeat the query.
    pub async fn get_capabilities_with_identity(
        &self,
        service_id: i32,
    ) -> Result<
        (
            temps_providers::externalsvc::RestoreCapabilities,
            RestoreServiceIdentity,
        ),
        RestoreError,
    > {
        self.capabilities_of(service_id, true).await
    }

    /// The restore modes of `service_id`, and with `read_window` the PITR
    /// window its archive covers where that is read from the archive
    /// (admission needs the modes only).
    async fn capabilities_of(
        &self,
        service_id: i32,
        read_window: bool,
    ) -> Result<
        (
            temps_providers::externalsvc::RestoreCapabilities,
            RestoreServiceIdentity,
        ),
        RestoreError,
    > {
        let service = self.load_service(service_id).await?;
        let identity = RestoreServiceIdentity {
            id: service.id,
            name: service.name.clone(),
            service_type: service.service_type.clone(),
        };
        // A PostgreSQL on a worker node restores WAL-G backups on its node
        // in place and to a point in time, through its agent, with the
        // window its archive covers (see `worker_walg`).
        if crate::engines::workload_job::takes_job_path(&service) {
            let capabilities = worker_walg::capabilities(
                &self.db,
                &self.encryption_service,
                &service,
                read_window,
            )
            .await;
            return Ok((capabilities, identity));
        }
        // A MongoDB or a Redis on a worker node restores with the control
        // plane's own restore on its node, in place (see `worker_mongodb`,
        // `worker_redis`).
        if worker_mongodb::takes_path(&service) {
            return Ok((worker_mongodb::capabilities(), identity));
        }
        // A MariaDB on a worker node is restored by the control plane's own
        // MariaDB restore on its node (see `worker_mariadb`).
        if worker_mariadb::takes_mariadb_path(&service) {
            return Ok((worker_mariadb::capabilities(), identity));
        }
        // The other services on a worker node are restored on that node, in
        // place only (see `agent_restore_supported`). Their engine is not on
        // this host, which may have no Docker at all.
        if worker_redis::takes_redis_path(&service)
            || crate::engines::worker_agent::takes_agent_path(&service)
        {
            return Ok((worker_restore_capabilities(), identity));
        }
        let service_type =
            ServiceType::from_str(&service.service_type).map_err(|e| RestoreError::Validation {
                message: format!("Invalid service type '{}': {}", service.service_type, e),
            })?;
        let unavailable = |reason: String| RestoreError::CapabilitiesUnavailable {
            service_id,
            service_name: service.name.clone(),
            reason,
        };
        let service_config = self
            .external_service_manager
            .get_service_config(service_id)
            .await
            .map_err(|e| unavailable(format!("loading its configuration failed: {e}")))?;
        let instance = self
            .external_service_manager
            .resolve_service_instance(&service.name, service_type)
            .await
            .map_err(|e| unavailable(format!("its engine is not available here: {e}")))?;
        let capabilities = instance
            .restore_capabilities(service_config)
            .await
            .map_err(|e| unavailable(format!("its engine could not answer: {e}")))?;
        Ok((capabilities, identity))
    }

    /// Validate a restore request, insert a `restore_runs` row, spawn the
    /// worker, and return the run record so the caller can poll.
    ///
    /// `target_service_id` is the service that will end up holding the
    /// restored data (or act as the template for a new service). It is
    /// independent of whichever service originally produced the backup —
    /// important for disaster-recovery flows where the origin service may
    /// not exist on this Temps anymore.
    ///
    /// `selector` identifies the backup either by DB id (normal case) or
    /// by raw S3 location (orphan backup discovered via S3 scan).
    pub async fn start_restore(
        &self,
        target_service_id: i32,
        selector: BackupSelector,
        mode: RestoreRequestMode,
        user_id: i32,
    ) -> Result<RestoreRunView, RestoreError> {
        // Resolve the backup: either via the DB row, or synthesize one
        // from a raw S3 location (orphan from another Temps instance).
        let (
            resolved_backup_id,
            backup_location,
            backup_engine_hint,
            s3_source_id,
            backup_started_at,
        ) = match &selector {
            BackupSelector::Id(id) => {
                let backup = temps_entities::backups::Entity::find_by_id(*id)
                    .one(self.db.as_ref())
                    .await?
                    .ok_or(RestoreError::BackupNotFound { backup_id: *id })?;
                // Reject before any engine or capability work: a pending,
                // running or failed backup points at a partial (or empty)
                // snapshot, and an in-place restore from it would wipe the
                // target. Admission re-checks this under the row lock.
                ensure_backup_restorable(&backup)?;

                // Fail closed: a backup some service produced must have a
                // known engine, otherwise the compatibility check below
                // would be skipped. Only producer-less (control-plane)
                // backups, which are administrators-only, may proceed
                // without one.
                let engine = match self.resolve_backup_engine(&backup).await? {
                    ResolvedBackupEngine::Known(engine) => Some(engine),
                    ResolvedBackupEngine::Unknown {
                        has_producer: false,
                    } => None,
                    ResolvedBackupEngine::Unknown { has_producer: true } => {
                        return Err(RestoreError::Validation {
                            message: format!(
                                "Cannot determine which engine produced backup {}: its metadata \
                                 has no service_type and its producing service is gone. \
                                 Refusing to restore it without an engine compatibility check.",
                                backup.id
                            ),
                        });
                    }
                };
                (
                    Some(backup.id),
                    backup.s3_location.clone(),
                    engine,
                    backup.s3_source_id,
                    Some(backup.started_at),
                )
            }
            BackupSelector::Location {
                location,
                engine,
                s3_source_id,
            } => {
                if location.trim().is_empty() {
                    return Err(RestoreError::Validation {
                        message: "backup_location cannot be empty".into(),
                    });
                }
                // Orphan restores (backup discovered by S3 scan, produced
                // by another Temps instance) have no DB row and thus no
                // known `started_at` to validate a PITR target against —
                // we can't range-check what we don't know. The engine
                // (Postgres, via `restore_pitr`) still protects against a
                // target the WAL can't reach: it FATALs out of recovery
                // rather than promoting, which the container health
                // check surfaces as a failed restore run.
                (
                    None,
                    location.clone(),
                    Some(engine.clone()),
                    *s3_source_id,
                    None,
                )
            }
        };

        // Target service: where the restored data goes.
        let target = self.load_service(target_service_id).await?;

        // ADR-055 Phase 9: a database on a worker node is restored by a job
        // on its node, which applies a dump in place. Refuse anything else
        // now, not after the run started.
        if worker_walg::takes_walg_path(&target, &backup_location) {
            worker_walg::walg_restore_supported(&target, &mode)?;
        } else if worker_mariadb::takes_mariadb_path(&target) {
            worker_mariadb::restore_supported(&target, &mode)?;
        } else if crate::engines::workload_job::takes_job_path(&target) {
            job_restore_supported(&target, &backup_location, &mode)?;
        } else if worker_mongodb::takes_path(&target) {
            worker_mongodb::supported(&target, &mode)?;
        } else if worker_redis::takes_redis_path(&target) {
            worker_redis::restore_supported(&target, &backup_location, &mode)?;
        } else if crate::engines::worker_agent::takes_agent_path(&target) {
            agent_restore_supported(&target, &backup_location, &mode)?;
        }

        // Engine-compat guard: refuse to restore a postgres backup onto a
        // redis target, etc.
        if let Some(engine) = &backup_engine_hint {
            if !engines_compatible(engine, &target.service_type) {
                return Err(RestoreError::Validation {
                    message: format!(
                        "Engine mismatch: backup is '{}', target service '{}' is '{}'. Restore requires matching engines.",
                        engine, target.name, target.service_type
                    ),
                });
            }
        }

        // Capabilities come from the TARGET — we're going to run the
        // restore logic on its service instance.
        let (caps, _) = self.capabilities_of(target_service_id, false).await?;
        match &mode {
            RestoreRequestMode::InPlace => {
                if !caps.restore_in_place {
                    return Err(RestoreError::UnsupportedMode {
                        mode: "in_place".into(),
                        service_type: target.service_type.clone(),
                    });
                }
            }
            RestoreRequestMode::NewService { name, .. } => {
                if !caps.restore_to_new_service {
                    return Err(RestoreError::UnsupportedMode {
                        mode: "new_service".into(),
                        service_type: target.service_type.clone(),
                    });
                }
                if name.trim().is_empty() {
                    return Err(RestoreError::Validation {
                        message: "new service name cannot be empty".into(),
                    });
                }
            }
            RestoreRequestMode::Pitr {
                to_new_service,
                new_service_name,
                target: recovery_target,
            } => {
                if !caps.pitr {
                    return Err(RestoreError::UnsupportedMode {
                        mode: "pitr".into(),
                        service_type: target.service_type.clone(),
                    });
                }
                validate_pitr_backup_location(&target.service_type, &backup_location)?;
                if *to_new_service
                    && new_service_name
                        .as_ref()
                        .map(|n| n.trim().is_empty())
                        .unwrap_or(true)
                {
                    return Err(RestoreError::Validation {
                        message: "new_service_name is required when to_new_service=true".into(),
                    });
                }
                validate_pitr_recovery_target(recovery_target, backup_started_at)?;
            }
        }

        let (target_name, recovery_target_json) = match &mode {
            RestoreRequestMode::InPlace => (None, None),
            RestoreRequestMode::NewService { name, .. } => (Some(name.clone()), None),
            RestoreRequestMode::Pitr {
                to_new_service,
                new_service_name,
                target,
            } => {
                let tgt_json =
                    serde_json::to_value(target).map_err(|e| RestoreError::Internal {
                        reason: format!("Failed to serialize recovery target: {}", e),
                    })?;
                let name = if *to_new_service {
                    new_service_name.clone()
                } else {
                    None
                };
                (name, Some(tgt_json))
            }
        };

        let parameter_overrides = match &mode {
            RestoreRequestMode::NewService {
                parameter_overrides,
                ..
            } => parameter_overrides.clone(),
            _ => serde_json::json!({}),
        };

        // Persist the backup selector + target service. For orphan backups
        // (no DB id) we use id=0 as a sentinel since the column is
        // non-null; the worker keys off resume_token to find the location.
        let resume_token = serde_json::json!({
            "backup_location": backup_location,
            "engine_hint": backup_engine_hint,
            "s3_source_id": s3_source_id,
        });

        // Insert the run while holding the source row lock. Deletion uses the
        // same lock before checking restore history, so exactly one operation
        // wins and remote data can never be removed underneath a new restore.
        let log_id = uuid::Uuid::new_v4().to_string();
        let run_active = temps_entities::restore_runs::ActiveModel {
            id: NotSet,
            source_backup_id: Set(resolved_backup_id.unwrap_or(0)),
            // `source_service_id` historically meant "service we're
            // operating on". Orchestration now always points at the
            // TARGET — the worker re-discovers the backup from the
            // resume_token.
            source_service_id: Set(target_service_id),
            target_service_id: Set(None),
            target_service_name: Set(target_name),
            mode: Set(mode.as_str().to_string()),
            status: Set("running".to_string()),
            phase: Set("prepare".to_string()),
            recovery_target: Set(recovery_target_json),
            parameter_overrides: Set(parameter_overrides),
            resume_token: Set(Some(resume_token)),
            log_id: Set(log_id),
            error_message: Set(None),
            attempt: Set(1),
            started_at: Set(Some(Utc::now())),
            finished_at: Set(None),
            created_by: Set(user_id),
            created_at: NotSet,
            updated_at: NotSet,
            cancel_requested_at: Set(None),
            cancel_requested_by: Set(None),
        };
        let run = insert_restore_run(
            self.db.as_ref(),
            run_active,
            resolved_backup_id,
            target_service_id,
            mode.is_destructive(),
        )
        .await?;

        // Spawn the worker — it owns Arc clones and updates the row as it goes.
        let run_id = run.id;
        let db = self.db.clone();
        let mgr = self.external_service_manager.clone();
        let enc = self.encryption_service.clone();
        let telemetry = self.telemetry.clone();
        let plane = self.workload_plane.clone();
        let scoper = self.worker_credentials.clone();
        // Registered before the worker starts so a cancellation that arrives
        // immediately still reaches it.
        let registration = self.cancellations.register(run_id);
        tokio::spawn(async move {
            if let Err(e) = run_restore_worker(
                db,
                mgr,
                enc,
                plane,
                scoper,
                telemetry,
                run_id,
                mode,
                registration,
            )
            .await
            {
                error!("Restore run {} failed: {}", run_id, e);
            }
        });

        Ok(run.into())
    }

    /// Ask a running restore to stop.
    ///
    /// Accepted only while stopping is safe (see `restore_cancel`): the
    /// cancellation is recorded with a conditional update, and the worker is
    /// then woken. Repeating a request that was already recorded is not an
    /// error — the current run is returned. Anything else is
    /// [`RestoreError::RestoreNotCancellable`] with the reason the console
    /// shows the operator.
    pub async fn cancel_restore_run(
        &self,
        restore_run_id: i32,
        user_id: i32,
    ) -> Result<RestoreRunView, RestoreError> {
        let run = load_run_for_update(&self.db, restore_run_id).await?;
        if super::restore_cancel::record_cancellation(self.db.as_ref(), &run, user_id).await? {
            let signalled = self.cancellations.signal(restore_run_id);
            info!(
                "Cancellation of restore run {} recorded by user {} in phase '{}' (worker signalled: {})",
                restore_run_id, user_id, run.phase, signalled
            );
            return self.get_restore_run(restore_run_id).await;
        }

        // The conditional update lost: re-read to explain why with the
        // state that actually won.
        let current = load_run_for_update(&self.db, restore_run_id).await?;
        if current.cancel_requested_at.is_some()
            && ACTIVE_RESTORE_STATUSES.contains(&current.status.as_str())
        {
            return Ok(current.into());
        }
        if !ACTIVE_RESTORE_STATUSES.contains(&current.status.as_str()) {
            return Err(RestoreError::RestoreNotActive {
                restore_run_id,
                status: current.status,
            });
        }
        let reason = super::restore_cancel::not_cancellable_reason(&current).unwrap_or_else(|| {
            "The restore changed state while the request was processed; try again.".to_string()
        });
        Err(RestoreError::RestoreNotCancellable {
            restore_run_id,
            phase: current.phase,
            reason,
        })
    }

    /// Fetch a single restore run by id.
    pub async fn get_restore_run(&self, id: i32) -> Result<RestoreRunView, RestoreError> {
        let run = temps_entities::restore_runs::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or(RestoreError::RestoreRunNotFound { restore_run_id: id })?;
        views_with_source_backups(self.db.as_ref(), vec![run])
            .await?
            .into_iter()
            .next()
            .ok_or(RestoreError::RestoreRunNotFound { restore_run_id: id })
    }

    /// List restore runs for a given source service, newest first.
    pub async fn list_restore_runs_for_service(
        &self,
        service_id: i32,
    ) -> Result<Vec<RestoreRunView>, RestoreError> {
        let runs = temps_entities::restore_runs::Entity::find()
            .filter(temps_entities::restore_runs::Column::SourceServiceId.eq(service_id))
            .order_by_desc(temps_entities::restore_runs::Column::CreatedAt)
            .limit(50)
            .all(self.db.as_ref())
            .await?;
        views_with_source_backups(self.db.as_ref(), runs).await
    }

    /// Resolve every external service that produced a backup. An empty result
    /// identifies a control-plane/ownerless backup and must remain
    /// administrators-only at the authorization layer.
    pub async fn backup_source_service_ids(
        &self,
        backup_id: i32,
    ) -> Result<Vec<i32>, RestoreError> {
        Ok(self
            .backup_source_services_for_backups(&[backup_id])
            .await?
            .into_iter()
            .next()
            .map(|mapping| mapping.service_ids)
            .unwrap_or_default())
    }

    /// Resolve producer services for many backup ids in one database query.
    /// The result contains one deterministic entry for every requested id,
    /// including backups with no producer-service rows.
    pub async fn backup_source_services_for_backups(
        &self,
        backup_ids: &[i32],
    ) -> Result<Vec<BackupProducerServices>, RestoreError> {
        let unique_backup_ids: BTreeSet<i32> = backup_ids.iter().copied().collect();
        if unique_backup_ids.is_empty() {
            return Ok(Vec::new());
        }

        let rows = temps_entities::external_service_backups::Entity::find()
            .filter(
                temps_entities::external_service_backups::Column::BackupId
                    .is_in(unique_backup_ids.iter().copied()),
            )
            .all(self.db.as_ref())
            .await?;

        let mut services_by_backup: BTreeMap<i32, BTreeSet<i32>> = unique_backup_ids
            .into_iter()
            .map(|backup_id| (backup_id, BTreeSet::new()))
            .collect();
        for row in rows {
            if let Some(service_ids) = services_by_backup.get_mut(&row.backup_id) {
                service_ids.insert(row.service_id);
            }
        }

        Ok(services_by_backup
            .into_iter()
            .map(|(backup_id, service_ids)| BackupProducerServices {
                backup_id,
                service_ids: service_ids.into_iter().collect(),
            })
            .collect())
    }

    /// Resolve which services produced the selected backup and whether
    /// restoring it onto `target_service_id` crosses service identity.
    pub async fn resolve_origin_binding(
        &self,
        target_service_id: i32,
        selector: &BackupSelector,
    ) -> Result<RestoreOriginBinding, RestoreError> {
        let origin_service_ids = match selector {
            BackupSelector::Id(backup_id) => self.backup_source_service_ids(*backup_id).await?,
            // A raw S3 location carries no trustworthy origin identity.
            BackupSelector::Location { .. } => Vec::new(),
        };
        Ok(RestoreOriginBinding::new(
            target_service_id,
            origin_service_ids,
        ))
    }

    pub async fn get_service_identity(
        &self,
        service_id: i32,
    ) -> Result<RestoreServiceIdentity, RestoreError> {
        let service = self.load_service(service_id).await?;
        Ok(RestoreServiceIdentity {
            id: service.id,
            name: service.name,
            service_type: service.service_type,
        })
    }

    /// Which engine produced a recorded backup, from (in order) its metadata,
    /// the engine snapshot on its producer link, or the producing service's
    /// current type. Shared by `plan_restore` and `start_restore` so the
    /// preview and the enforcement can never disagree.
    async fn resolve_backup_engine(
        &self,
        backup: &temps_entities::backups::Model,
    ) -> Result<ResolvedBackupEngine, RestoreError> {
        use temps_entities::external_service_backups::{Column, Entity};
        let producers = Entity::find()
            .filter(Column::BackupId.eq(backup.id))
            .find_also_related(temps_entities::external_services::Entity)
            .all(self.db.as_ref())
            .await?;
        let producer_engines: Vec<(Option<String>, Option<String>)> = producers
            .into_iter()
            .map(|(link, service)| (link.service_type_snapshot, service.map(|s| s.service_type)))
            .collect();
        Ok(pick_backup_engine(&backup.metadata, &producer_engines))
    }

    async fn load_service(
        &self,
        id: i32,
    ) -> Result<temps_entities::external_services::Model, RestoreError> {
        temps_entities::external_services::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or(RestoreError::ServiceNotFound { service_id: id })
    }
}

/// Engine of a backup as far as Temps can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ResolvedBackupEngine {
    Known(String),
    /// No engine recorded anywhere. `has_producer` is whether any service is
    /// recorded as having produced the backup: when one is, the engine must
    /// be known before restoring it.
    Unknown {
        has_producer: bool,
    },
}

/// Pick a backup's engine from its metadata JSON, else from its producer
/// links as `(engine snapshot on the link, producing service's current type)`.
fn pick_backup_engine(
    metadata: &str,
    producers: &[(Option<String>, Option<String>)],
) -> ResolvedBackupEngine {
    let non_empty = |value: &Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let from_metadata = serde_json::from_str::<serde_json::Value>(metadata)
        .ok()
        .and_then(|v| {
            v.get("service_type")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .filter(|v| !v.trim().is_empty());
    from_metadata
        .or_else(|| {
            producers
                .iter()
                .find_map(|(snapshot, _)| non_empty(snapshot))
        })
        .or_else(|| producers.iter().find_map(|(_, current)| non_empty(current)))
        .map(ResolvedBackupEngine::Known)
        .unwrap_or(ResolvedBackupEngine::Unknown {
            has_producer: !producers.is_empty(),
        })
}

/// Views of `runs` with their source backups resolved in one query.
async fn views_with_source_backups(
    db: &DatabaseConnection,
    runs: Vec<temps_entities::restore_runs::Model>,
) -> Result<Vec<RestoreRunView>, RestoreError> {
    let backup_ids: BTreeSet<i32> = runs
        .iter()
        .map(|run| run.source_backup_id)
        .filter(|id| *id > 0)
        .collect();
    let backups: BTreeMap<i32, temps_entities::backups::Model> = if backup_ids.is_empty() {
        BTreeMap::new()
    } else {
        temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::Id.is_in(backup_ids))
            .all(db)
            .await?
            .into_iter()
            .map(|backup| (backup.id, backup))
            .collect()
    };
    Ok(runs
        .into_iter()
        .map(|run| {
            let source_backup = backups
                .get(&run.source_backup_id)
                .map(RestoreRunSourceBackup::from_backup);
            let mut view = RestoreRunView::from(run);
            if let Some(source_backup) = source_backup {
                view.source_backup = source_backup;
            }
            view
        })
        .collect())
}

/// Engine-family check: two engines are restore-compatible if they
/// speak the same wire protocol and share the same backup/restore path.
///
/// Today there's exactly one family: the S3-compatible object stores.
/// A RustFS backup can be restored onto a MinIO/S3 target (and back)
/// because both are just buckets of opaque objects mirrored via `mc`.
/// Postgres/Redis/Mongo each form their own single-engine family.
fn engines_compatible(a: &str, b: &str) -> bool {
    let a = a.to_ascii_lowercase();
    let b = b.to_ascii_lowercase();
    if a == b {
        return true;
    }
    let object_store = ["s3", "rustfs", "minio", "blob"];
    object_store.contains(&a.as_str()) && object_store.contains(&b.as_str())
}

/// Reject a PITR recovery target that's provably out of range BEFORE we
/// ever touch the target container: nothing in a backup's WAL can predate
/// the base backup itself starting, so a target before that can never be a
/// real, honored recovery point.
///
/// Root-cause context: without this check, Postgres itself does NOT error
/// on a too-early `recovery_target_time` — it silently stops recovery at
/// the earliest point it CAN reach (immediately after the base backup's own
/// consistency checkpoint) and reports success, discarding the requested
/// target with no warning surfaced anywhere. A restore run would come back
/// `status: "completed"` having silently ignored what the caller actually
/// asked for. (Verified empirically against a real WAL-G backup: PostgreSQL
/// 18's log shows `starting point-in-time recovery to <target>` /
/// `consistent recovery state reached` / `recovery stopping before commit
/// of transaction N` — no FATAL, no error — for a target years before the
/// backup existed.)
///
/// A target in the FUTURE (past all archived WAL) is already handled safely
/// without this check: PostgreSQL itself FATALs with "recovery ended before
/// configured recovery target was reached" once it exhausts available WAL,
/// which — combined with `restart_policy=always` — crash-loops the
/// container until `wait_for_container_health`'s 90s timeout surfaces it as
/// a failed restore run. That path is a real, if slow (up to 90s) and
/// generically-worded, failure — not silent corruption — so it's
/// intentionally left alone here.
///
/// `backup_started_at` is `None` for orphan restores (backup discovered by
/// S3 scan, produced by another Temps instance) — there's no DB row and
/// thus no known start time to validate against, so we can't range-check
/// what we don't know; those still fall back on Postgres's own FATAL/crash
/// loop for a too-early target too (untested here, but the same "PG stops
/// at the earliest reachable point without erroring" behavior applies).
fn validate_pitr_recovery_target(
    target: &RecoveryTarget,
    backup_started_at: Option<chrono::DateTime<Utc>>,
) -> Result<(), RestoreError> {
    if let RecoveryTarget::Time { time } = target {
        if let Some(started_at) = backup_started_at {
            if *time < started_at {
                return Err(RestoreError::Validation {
                    message: format!(
                        "PITR recovery target {} is before this backup started ({}) — no WAL \
                         this backup covers can satisfy it. Choose a time at or after the \
                         backup start, or select an earlier backup.",
                        time.to_rfc3339(),
                        started_at.to_rfc3339(),
                    ),
                });
            }
        }
    }
    Ok(())
}

/// Reject a PITR request whose backup format cannot anchor a forward-roll.
///
/// The test is ENGINE-specific because the two PITR-capable engines record
/// their base backups differently:
///
///   Postgres — WAL-G bases are stored as `s3://…` URLs.
///   MariaDB  — physical (`mariadb-backup`) bases are stored either as a WAL-G
///              repository ending in `/walg` (what `MariadbPhysicalEngine`
///              writes today) or, in older buckets, as a bare S3 key ending in
///              `base.mbstream.gz`.
///
/// Applying the Postgres shape to MariaDB rejected every MariaDB PITR before
/// it could start — with a "requires WAL-G" message that made no sense for the
/// engine — even though `MariaDbService::restore_capabilities` advertises
/// `pitr: true`. MariaDB is classified with the engine's own predicate so this
/// guard, the plan preview, and the engine all agree on what a physical base is.
/// That predicate must be the layout-agnostic one: testing only the legacy
/// `base.mbstream.gz` object reintroduces the same class of bug, because a
/// MariaDB WAL-G repository is a physical base the engine can and does replay.
fn validate_pitr_backup_location(
    target_service_type: &str,
    backup_location: &str,
) -> Result<(), RestoreError> {
    if target_service_type.eq_ignore_ascii_case("mariadb") {
        if !temps_providers::externalsvc::mariadb::MariaDbService::is_physical_base_backup_location(
            backup_location,
        ) {
            return Err(RestoreError::Validation {
                message: format!(
                    "PITR requires a physical (mariadb-backup) base backup; location was '{}'",
                    backup_location
                ),
            });
        }
    } else if !backup_location.starts_with("s3://") {
        return Err(RestoreError::Validation {
            message: format!(
                "PITR requires a WAL-G backup (s3:// prefix); location was '{}'",
                backup_location
            ),
        });
    }
    Ok(())
}

/// The service that produced `backup`: its producer link, or for a
/// control-plane backup row the `service_id` its metadata records. `None`
/// when neither names one (an orphan from another Temps, a deleted producer).
async fn backup_origin_service_id(
    db: &DatabaseConnection,
    backup: &temps_entities::backups::Model,
) -> Option<i32> {
    temps_entities::external_service_backups::Entity::find()
        .filter(temps_entities::external_service_backups::Column::BackupId.eq(backup.id))
        .one(db)
        .await
        .ok()
        .flatten()
        .map(|b| b.service_id)
        .or_else(|| {
            serde_json::from_str::<serde_json::Value>(&backup.metadata)
                .ok()
                .and_then(|v| v.get("service_id").and_then(|id| id.as_i64()))
                .and_then(|id| i32::try_from(id).ok())
        })
}

/// The name of the service that produced `backup`, for a restore plan: the
/// producer the restore itself binds to ([`backup_origin_service_id`]), else
/// the name its metadata recorded. `None` when neither is known.
async fn backup_origin_service_name(
    db: &DatabaseConnection,
    backup: &temps_entities::backups::Model,
) -> Option<String> {
    if let Some(service_id) = backup_origin_service_id(db, backup).await {
        if let Ok(Some(service)) = temps_entities::external_services::Entity::find_by_id(service_id)
            .one(db)
            .await
        {
            return Some(service.name);
        }
    }
    serde_json::from_str::<serde_json::Value>(&backup.metadata)
        .ok()
        .and_then(|v| {
            v.get("service_name")
                .and_then(|s| s.as_str())
                .map(String::from)
        })
}

/// How a backup at `location` is restored into a `service_type` service
/// (lowercase): the strategy a restore plan reports, from the location alone,
/// the same way the engines dispatch on it.
///
/// MariaDB is classified FIRST and entirely on its own terms. Its logical
/// dump object is named `dump.sql.gz`, so the generic `.sql.gz` arm below
/// would label it `pg_dump_restore` and the plan would describe a
/// `pg_restore` that never runs. Its physical base is a WAL-G repository
/// (or, in older buckets, a `base.mbstream.gz` object), neither of which
/// matches a generic arm. MongoDB and the object stores are classified
/// before the generic arms too: a MongoDB object that is not a WAL-G
/// prefix is a `mongodump --archive` (the MongoDB engine's dump, also what a
/// worker's agent writes), and an object-store location that is not a
/// WAL-G prefix is a bucket-mirror prefix.
fn classify_restore_strategy(service_type: &str, location: &str) -> &'static str {
    match service_type {
        "mariadb" => {
            // Same predicate the MariaDB engine itself dispatches on, so the
            // preview cannot promise a restore shape the executor won't take.
            if temps_providers::externalsvc::mariadb::MariaDbService::is_physical_base_backup_location(
                location,
            ) {
                "mariadb_physical_restore"
            } else if location.ends_with(".sql.gz") {
                "mariadb_dump_restore"
            } else {
                "unsupported"
            }
        }
        _ if location.starts_with("s3://") => "walg_restore",
        "mongodb" if !location.trim().is_empty() => "mongodump_restore",
        "s3" | "rustfs" | "blob" | "minio" if !location.trim().is_empty() => "s3_mirror_restore",
        _ if location.ends_with(".sql.gz") || location.ends_with(".pgdump.gz") => "pg_dump_restore",
        "redis" | "kv"
            if temps_providers::externalsvc::redis::classify_redis_backup_location(location)
                == temps_providers::externalsvc::redis::RedisBackupLocationKind::RdbGzip =>
        {
            "redis_rdb_restore"
        }
        _ => "unsupported",
    }
}

/// The backup format a restore strategy reads, as a restore plan reports it.
/// The MariaDB values match the engine keys its backup engines register
/// under, so the plan lines up with `classify_backup_format` and the backup
/// list.
fn restore_format(strategy: &str) -> &'static str {
    match strategy {
        "walg_restore" => "walg",
        "pg_dump_restore" => "pg_dump",
        "mariadb_physical_restore" => "mariadb_physical",
        "mariadb_dump_restore" => "mariadb_dump",
        "mongodump_restore" => "mongodump",
        "redis_rdb_restore" => "redis_rdb",
        "s3_mirror_restore" => "s3_mirror",
        _ => "unknown",
    }
}

/// What restoring into a service on a worker node runs, for a restore plan.
struct WorkerRestorePlan {
    steps: Vec<String>,
    /// Why `start_restore` would refuse it.
    refusal: Option<String>,
}

/// The plan of a restore into `target` when it runs on a worker node: a job
/// on the node (PostgreSQL) or the node's agent (the other engines), both
/// reached through the agent. `None` for a target on the control plane.
fn worker_restore_plan(
    target: &temps_entities::external_services::Model,
    location: &str,
    mode: &RestoreRequestMode,
    container: &str,
    cross_service: bool,
) -> Option<WorkerRestorePlan> {
    let node = crate::engines::workload_job::worker_node(target)?;
    if worker_walg::takes_walg_path(target, location) {
        return Some(WorkerRestorePlan {
            steps: worker_walg::steps(node, location, mode, container, cross_service),
            refusal: worker_walg::walg_restore_supported(target, mode)
                .err()
                .map(|e| e.to_string()),
        });
    }
    if worker_mariadb::takes_mariadb_path(target) {
        let (steps, mut refusals) = worker_mariadb::steps(node, location, mode, container);
        refusals.extend(
            worker_mariadb::restore_supported(target, mode)
                .err()
                .map(|e| e.to_string()),
        );
        return Some(WorkerRestorePlan {
            steps,
            refusal: (!refusals.is_empty()).then(|| refusals.join(" ")),
        });
    }
    if crate::engines::workload_job::takes_job_path(target) {
        let mut steps = vec![
            format!(
                "Run a restore job on worker node {node}, through its agent, next to database \
                 container {container}"
            ),
            format!(
                "The job reads {location} from S3 and checks that it decompresses completely; \
                 nothing is applied if it does not"
            ),
        ];
        if cross_service {
            steps.push(
                "Give the job's login the password the dump restores, before applying it".into(),
            );
        }
        steps.push(format!(
            "Apply the dump in place into {container} with psql, stopping at the first error \
             (existing data is overwritten)"
        ));
        if cross_service {
            steps.push(
                "Update the service's stored credentials to the restored login's password".into(),
            );
        }
        return Some(WorkerRestorePlan {
            steps,
            refusal: job_restore_supported(target, location, mode)
                .err()
                .map(|e| e.to_string()),
        });
    }
    if worker_mongodb::takes_path(target) {
        let (steps, mut errors) = worker_mongodb::steps(
            node,
            classify_restore_strategy(&target.service_type, location),
            mode,
            container,
            location,
        );
        errors.extend(
            worker_mongodb::supported(target, mode)
                .err()
                .map(|e| e.to_string()),
        );
        return Some(WorkerRestorePlan {
            steps,
            refusal: (!errors.is_empty()).then(|| errors.join("; ")),
        });
    }
    if worker_redis::takes_redis_path(target) {
        return Some(WorkerRestorePlan {
            steps: worker_redis::steps(node, location, mode, container, cross_service),
            refusal: worker_redis::restore_supported(target, location, mode)
                .err()
                .map(|e| e.to_string()),
        });
    }
    if crate::engines::worker_agent::takes_agent_path(target) {
        use crate::engines::worker_agent::{worker_service_kind, WorkerServiceKind};
        use temps_providers::externalsvc::worker_data::DumpEngine;
        let applies = match worker_service_kind(&target.service_type) {
            Some(WorkerServiceKind::Storage) => format!(
                "The agent mirrors each bucket under {location} into {container} with \
                 `rc mirror --overwrite --remove`: missing objects are added, changed ones \
                 overwritten, and objects the backup does not have are deleted"
            ),
            // MongoDB, Redis and MariaDB take `worker_mongodb`, `worker_redis` and
            // `worker_mariadb` above.
            Some(WorkerServiceKind::Dump(DumpEngine::Mongodb)) | None => {
                format!(
                "The agent applies {location} in place with {}'s own restore (existing data is \
                 overwritten)",
                target.service_type
            )
            }
        };
        return Some(WorkerRestorePlan {
            steps: vec![
                format!(
                    "Restore on worker node {node}, through its agent, into container {container}"
                ),
                applies,
            ],
            refusal: agent_restore_supported(target, location, mode)
                .err()
                .map(|e| e.to_string()),
        });
    }
    None
}

/// The credentials a cross-service restore of `backup` leaves in `target`'s
/// data, which the target's stored config is patched to once the restore
/// succeeds: the same reconciliation, under the same per-engine gates
/// ([`credential_propagation_gates`]), as a restore on the control plane.
///
/// `None` when the backup is the target's own, when its data carries no
/// credentials, or when the origin is unknown or its config unreadable (the
/// restore then keeps the target's credentials, as on the control plane;
/// the restore plan warns about it).
async fn origin_credentials_to_reconcile(
    db: &DatabaseConnection,
    mgr: &ExternalServiceManager,
    backup: &temps_entities::backups::Model,
    target: &temps_entities::external_services::Model,
) -> Option<RestoredServiceCredentials> {
    let (preserves_source_credentials, _) =
        credential_propagation_gates(&target.service_type, &backup.s3_location);
    if !preserves_source_credentials {
        return None;
    }
    let origin_id = backup_origin_service_id(db, backup).await?;
    if origin_id == target.id {
        return None;
    }
    match mgr.get_service_config(origin_id).await {
        Ok(origin) => {
            RestoredServiceCredentials::from_origin(&target.service_type, &origin.parameters)
        }
        Err(e) => {
            warn!(
                "Failed to read origin service {} config; the restore of backup {} into service \
                 {} keeps the target's credentials, which may not match the restored data: {}",
                origin_id, backup.id, target.id, e
            );
            None
        }
    }
}

/// Patch `target`'s stored credentials to the ones its restored data now
/// expects. A failure is logged, never fatal: the data is restored, and the
/// operator can reset the password.
async fn reconcile_restored_credentials(
    db: &DatabaseConnection,
    enc: &Arc<temps_core::EncryptionService>,
    target_id: i32,
    credentials: Option<&RestoredServiceCredentials>,
) {
    let Some(credentials) = credentials else {
        return;
    };
    match patch_service_credentials(db, enc, target_id, credentials).await {
        Ok(()) => info!(
            "Updated target service {} config credentials to match restored data",
            target_id
        ),
        Err(e) => error!(
            "Restore on service {} succeeded but patching stored credentials failed; the \
             service's UI-shown credentials will be stale until someone updates them. Error: {}",
            target_id, e
        ),
    }
}

/// Which credential-reconciliation steps a restore needs, as
/// `(preserves_source_credentials, wants_pre_restore_merge)`.
///
/// `preserves_source_credentials` means the restored data carries the ORIGIN
/// service's auth catalog, so the target's stored password must be patched to
/// the origin's afterwards. `wants_pre_restore_merge` additionally hands the
/// origin's credentials to the engine up front, which is only safe for OFFLINE
/// restores (the data swap doesn't authenticate, and the steps after it must
/// use the credentials the restored data expects).
///
///   Postgres — both. The base backup contains `pg_authid`; the restore is
///              offline.
///   MongoDB  — patch only. `mongorestore` streams into a LIVE mongod that
///              still wants the TARGET's password until the restore lands.
///   MariaDB  — depends on the backup FORMAT, which the location encodes:
///              a physical base — WAL-G repository or legacy mbstream object
///              alike — replaces the whole datadir including the `mysql`
///              system schema (so both, like Postgres), while a
///              logical `mariadb_dump` explicitly EXCLUDES the `mysql` schema
///              (see engines/mariadb_dump.rs) and therefore carries no
///              credentials at all — merging there would authenticate the dump
///              load with the wrong password and then overwrite the target's
///              stored password with a credential the target never had,
///              locking the operator out via UI/CLI.
///   Redis / S3 / RustFS — neither; their data layer has no auth.
///
/// A MariaDB backup row with an empty `s3_location` (legacy rows, backfilled
/// from S3 later in the worker) classifies as non-physical and therefore skips
/// both steps: a cross-service PITR forward-roll that fails auth loudly is
/// strictly better than silently rewriting the target's stored credentials.
fn credential_propagation_gates(target_service_type: &str, backup_location: &str) -> (bool, bool) {
    match target_service_type.to_ascii_lowercase().as_str() {
        "postgres" => (true, true),
        "mongodb" => (true, false),
        "mariadb" => {
            let physical =
                temps_providers::externalsvc::mariadb::MariaDbService::is_physical_base_backup_location(
                    backup_location,
                );
            (physical, physical)
        }
        _ => (false, false),
    }
}

/// The origin's login an offline restore (PostgreSQL, a physical MariaDB
/// base) runs with, merged over the target's configuration: the restored
/// auth catalog (`pg_authid`, `mysql.user`) answers to it. `root_password`
/// is MariaDB's admin credential, the one the binary-log replay and every
/// post-restore admin exec use; PostgreSQL configs have no such key.
fn origin_login_merge(origin: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    ["password", "root_password", "username", "database"]
        .into_iter()
        .filter_map(|key| Some((key.to_string(), origin.get(key)?.clone())))
        .collect()
}

/// Worker: marks the run through phases, dispatches to the trait, and
/// persists target_service_id (when applicable) on success.
#[allow(clippy::too_many_arguments)]
async fn run_restore_worker(
    db: Arc<DatabaseConnection>,
    mgr: Arc<ExternalServiceManager>,
    enc: Arc<temps_core::EncryptionService>,
    plane: Arc<temps_workload_plane::PlaneSlot>,
    scoper: Arc<temps_providers::worker_s3_scope::WorkerCredentialScoper>,
    telemetry: Arc<dyn temps_core::telemetry::TelemetryReporter>,
    run_id: i32,
    mode: RestoreRequestMode,
    registration: super::restore_cancel::RegisteredRestore,
) -> Result<(), RestoreError> {
    let started = std::time::Instant::now();
    let mode_label = mode.as_str();
    let destructive = mode.is_destructive();
    // The restore runs in its own task so a panic inside an engine surfaces
    // here as a `JoinError` instead of unwinding past the terminal-state
    // write below and leaving the run `running` with no owner.
    let fence = super::restore_reconcile::ManagerRestoreFence::new(mgr.clone());
    let inner = tokio::spawn(run_restore_inner(
        db.clone(),
        mgr,
        enc,
        plane,
        scoper,
        run_id,
        mode,
        registration.token(),
    ));
    let result = match inner.await {
        // A cancellation that won a guarded phase move before anything was
        // written: nothing to clean up.
        Ok(Err(RestoreError::RestoreCancelled {
            restore_run_id,
            phase,
        })) => Err(RestoreError::Cancelled {
            restore_run_id,
            message: super::restore_cancel::cancelled_message(
                &phase,
                destructive,
                &super::restore_cancel::CancelCleanup::TargetUntouched,
            ),
            phase,
        }),
        // A cancelled download that could not be confirmed stopped: retry
        // the confirmation, and only release the service once it holds.
        Ok(Err(still_running @ RestoreError::StagedRestoreStillRunning { .. })) => {
            match confirm_staged_restore_stopped(&db, run_id, &fence).await {
                Ok(phase) => Err(RestoreError::Cancelled {
                    restore_run_id: run_id,
                    message: super::restore_cancel::cancelled_message(
                        &phase,
                        destructive,
                        &super::restore_cancel::CancelCleanup::TargetUntouched,
                    ),
                    phase,
                }),
                Err(e) => {
                    error!("{} Last confirmation attempt: {}", still_running, e);
                    telemetry.report(restore_outcome_event(
                        mode_label,
                        started.elapsed(),
                        Err(still_running.to_string()),
                    ));
                    return Err(still_running);
                }
            }
        }
        Ok(result) => result,
        Err(join_error) => {
            match settle_crashed_run(&db, run_id, &fence, join_error.to_string()).await {
                Ok(crashed) => Err(crashed),
                Err(keep_active) => {
                    // A helper may still be writing into the target, so the
                    // run must keep holding the active-restore constraint.
                    error!(
                        "Restore run {} worker crashed and the run was left active: {}",
                        run_id, keep_active
                    );
                    telemetry.report(restore_outcome_event(
                        mode_label,
                        started.elapsed(),
                        Err(keep_active.to_string()),
                    ));
                    return Err(keep_active);
                }
            }
        }
    };
    let elapsed = started.elapsed();

    // The worker is done with its cancellation token.
    drop(registration);
    let persisted = persist_terminal_state(&db, run_id, &result, TERMINAL_WRITE_RETRY).await;
    match (&result, &persisted) {
        (Ok(_), Ok(())) => info!("Restore run {} completed successfully", run_id),
        (Err(RestoreError::Cancelled { message, .. }), Ok(())) => {
            info!("Restore run {} cancelled: {}", run_id, message)
        }
        (Err(e), Ok(())) => error!("Restore run {} failed: {}", run_id, e),
        (_, Err(e)) => error!(
            "Restore run {} finished but its terminal state could not be recorded: {}. \
             It stays active until the next restart reconciles it as interrupted.",
            run_id, e
        ),
    }

    // A cancellation is the operator's decision, not a restore failure, so
    // it is not reported as one.
    if matches!(result, Err(RestoreError::Cancelled { .. })) && persisted.is_ok() {
        return Ok(());
    }

    // Reported only once the run's final state is persisted: a restore whose
    // completion could not be recorded is not a success.
    let outcome = match (&result, &persisted) {
        (Err(e), _) | (Ok(_), Err(e)) => Err(e.to_string()),
        (Ok(_), Ok(())) => Ok(()),
    };
    telemetry.report(restore_outcome_event(mode_label, elapsed, outcome));

    persisted?;
    result.map(|_| ())
}

/// Anonymous restore outcome: the restore mode, a duration band and, on
/// failure, a fixed code. The error message never leaves the instance.
fn restore_outcome_event(
    mode: &'static str,
    elapsed: std::time::Duration,
    outcome: Result<(), String>,
) -> temps_core::telemetry::TelemetryEvent {
    use temps_core::telemetry::{TelemetryEvent, TelemetryEventKind};
    let event = match outcome {
        Ok(()) => TelemetryEvent::new(TelemetryEventKind::RestoreSucceeded),
        Err(message) => TelemetryEvent::new(TelemetryEventKind::RestoreFailed)
            .with_failure_from_message(&message),
    };
    event.with("mode", mode).with_duration(elapsed)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the restore task's owned dependencies, one per argument"
)]
async fn run_restore_inner(
    db: Arc<DatabaseConnection>,
    mgr: Arc<ExternalServiceManager>,
    enc: Arc<temps_core::EncryptionService>,
    plane: Arc<temps_workload_plane::PlaneSlot>,
    scoper: Arc<temps_providers::worker_s3_scope::WorkerCredentialScoper>,
    run_id: i32,
    mode: RestoreRequestMode,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<Option<i32>, RestoreError> {
    let run = load_run_for_update(&db, run_id).await?;

    // Resolve backup identity. `source_backup_id == 0` is the sentinel
    // for orphan-backup restores, where everything we need lives in
    // `resume_token` (backup_location + s3_source_id + engine_hint).
    let resume: serde_json::Value = run
        .resume_token
        .clone()
        .unwrap_or_else(|| serde_json::json!({}));
    let token_location = resume
        .get("backup_location")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let token_s3_source_id = resume
        .get("s3_source_id")
        .and_then(|v| v.as_i64())
        .map(|v| v as i32);

    let mut backup_model: temps_entities::backups::Model = if run.source_backup_id > 0 {
        temps_entities::backups::Entity::find_by_id(run.source_backup_id)
            .one(db.as_ref())
            .await?
            .ok_or(RestoreError::BackupNotFound {
                backup_id: run.source_backup_id,
            })?
    } else {
        // Orphan: synthesize a `backups::Model` for the trait context.
        // The trait only reads `s3_location` + `s3_source_id` off it;
        // other fields are zeroed.
        let s3_source_id = token_s3_source_id.ok_or(RestoreError::Internal {
            reason: "orphan restore run missing s3_source_id in resume_token".into(),
        })?;
        temps_entities::backups::Model {
            id: 0,
            name: "orphan backup".to_string(),
            backup_id: "".to_string(),
            schedule_id: None,
            schedule_run_id: None,
            backup_type: "full".to_string(),
            state: "completed".to_string(),
            started_at: Utc::now(),
            finished_at: None,
            size_bytes: None,
            file_count: None,
            s3_source_id,
            s3_location: token_location.clone(),
            error_message: None,
            metadata: "{}".to_string(),
            checksum: None,
            compression_type: "gzip".to_string(),
            created_by: run.created_by,
            expires_at: None,
            tags: "[]".to_string(),
        }
    };

    let s3_source = temps_entities::s3_sources::Entity::find_by_id(backup_model.s3_source_id)
        .one(db.as_ref())
        .await?
        .ok_or(RestoreError::S3SourceNotFound {
            s3_source_id: backup_model.s3_source_id,
        })?;

    // `run.source_service_id` is semantically the TARGET service — that's
    // where the restored data lands, regardless of which (possibly
    // long-gone) service produced the backup.
    let target_service =
        temps_entities::external_services::Entity::find_by_id(run.source_service_id)
            .one(db.as_ref())
            .await?
            .ok_or(RestoreError::ServiceNotFound {
                service_id: run.source_service_id,
            })?;

    // ADR-099: a WAL-G backup is restored into a PostgreSQL on a worker
    // node by the control plane's own WAL-G restore, run by its agent.
    if worker_walg::takes_walg_path(&target_service, &backup_model.s3_location) {
        return worker_walg::run(
            &db,
            &enc,
            &scoper,
            &mgr,
            run_id,
            &backup_model,
            &target_service,
            &mode,
        )
        .await;
    }
    // ADR-099: a MariaDB on a worker node is restored by the control
    // plane's own MariaDB restore, run by its agent.
    if worker_mariadb::takes_mariadb_path(&target_service) {
        return worker_mariadb::run(
            WorkerRestoreDeps {
                db: &db,
                enc: &enc,
                scoper: &scoper,
                mgr: &mgr,
            },
            run_id,
            &backup_model,
            &target_service,
            &mode,
        )
        .await;
    }
    // ADR-055 Phase 9: a database on a worker node is restored by a job on
    // its node. Every other target takes the path below, unchanged.
    if crate::engines::workload_job::takes_job_path(&target_service) {
        return run_job_restore(
            &db,
            &enc,
            &scoper,
            &mgr,
            &plane,
            run_id,
            &backup_model,
            &target_service,
            &mode,
        )
        .await;
    }
    // ADR-099: a MongoDB on a worker node is restored by the control
    // plane's own MongoDB restore, run by its agent.
    if worker_mongodb::takes_path(&target_service) {
        return worker_mongodb::run(
            WorkerRestoreDeps {
                db: &db,
                enc: &enc,
                scoper: &scoper,
                mgr: &mgr,
            },
            run_id,
            &backup_model,
            &target_service,
            &mode,
        )
        .await;
    }
    // A Redis on a worker node is restored in place by the provider's own
    // restore, run on its node by the agent.
    if worker_redis::takes_redis_path(&target_service) {
        return worker_redis::run(
            &db,
            &scoper,
            &mgr,
            run_id,
            &backup_model,
            &target_service,
            &mode,
        )
        .await;
    }
    // The other services on a worker node are restored in place by the
    // node's agent, with the engine's own restore.
    if crate::engines::worker_agent::takes_agent_path(&target_service) {
        return run_agent_restore(
            &db,
            &enc,
            &scoper,
            &mgr,
            run_id,
            &backup_model,
            &target_service,
            &mode,
        )
        .await;
    }

    let service_type = ServiceType::from_str(&target_service.service_type).map_err(|e| {
        RestoreError::Validation {
            message: format!(
                "Invalid service type '{}': {}",
                target_service.service_type, e
            ),
        }
    })?;

    // Build the config the engine will use during restore.
    //
    // Starts from the target's config (container name, volume, port —
    // we're writing onto the target's container). Then overlays the
    // ORIGIN service's credential fields when the origin is still known
    // to this Temps instance, because:
    //   - Postgres: restored pg_authid carries the source's password
    //     hashes. Target's password in `config` would be wrong.
    //   - Redis: AOF / RDB include the source's `requirepass` if any.
    //   - Mongo: restored auth.* collections carry source hashes.
    //
    // When the origin service isn't in our DB (true DR from another
    // Temps), we can't recover its password — the user will log in with
    // whatever was in the original backup, which is surfaced as a
    // warning by the plan endpoint.
    let mut source_config = mgr
        .get_service_config(target_service.id)
        .await
        .map_err(|e| RestoreError::ExternalService {
            reason: e.to_string(),
        })?;

    let live_target_config = source_config.clone();

    let origin_service_id = backup_origin_service_id(db.as_ref(), &backup_model).await;

    // Whether the RESTORED DATA carries source-side credentials that the
    // target's stored config must be reconciled with.
    //
    // Postgres: YES. The WAL-G base backup includes `pg_authid`, which is
    //   the actual password hash catalog. Post-restore, the cluster only
    //   authenticates clients with the source's password. We must merge
    //   origin's password into the config we pass the engine, and patch
    //   the target's stored config after success so UI/env/CLI reflect the
    //   credentials that actually work.
    //
    // Redis: NO. RDB files are pure KV data. The `requirepass` setting is
    //   applied at container start (`redis-server --requirepass X`) and
    //   is preserved across an RDB load. A restore leaves auth untouched.
    //   Merging origin's password would *break* things — the target would
    //   keep authenticating with its original password but Temps's DB
    //   would now think it's the source's.
    //
    // MongoDB: YES, but with a timing twist. `mongorestore --archive --drop`
    //   streams into a LIVE mongod and thus must authenticate with the
    //   TARGET's currently-active password. Only AFTER the restore writes
    //   the source's `admin.system.users` does the effective password
    //   change to the source's. So we must NOT merge origin's password
    //   into the config passed to the engine (that breaks the fetch auth)
    //   — we only need to capture it for the post-restore config patch.
    //
    // MariaDB: YES for a physical base, and it needs the SAME pre-restore
    //   merge Postgres gets. `mariadb-backup --copy-back` replaces the entire
    //   datadir including the `mysql` system schema, so `mysql.user` — the
    //   password-hash table — becomes the source's the moment the swap lands.
    //   The swap itself is offline (stop container → helper rewrites the
    //   volume → start), so the merged credentials cannot break it. But the
    //   step AFTER it does depend on them: `restore_pitr` reuses the same
    //   `MariaDbConfig` (built from this very `source_config`) to authenticate
    //   `mariadb-binlog`'s replay against the freshly-restored server, which
    //   by then only accepts the ORIGIN's password. Without the merge, a
    //   cross-service MariaDB PITR restores the base and then fails auth on
    //   the forward-roll — data restored, recovery point silently lost.
    //
    //   NO for a logical `mariadb_dump` base. That dump explicitly excludes
    //   the `mysql` schema (`SCHEMA_NAME NOT IN (…, 'mysql', …)` in
    //   engines/mariadb_dump.rs), so restoring it leaves the target's own
    //   `mysql.user` — and therefore its password — completely untouched.
    //   Merging origin credentials there is actively harmful: the engine
    //   would authenticate the dump load with the WRONG password, and the
    //   post-restore `patch_service_credentials` would overwrite the target's
    //   stored password with the origin's even though nothing on the target
    //   changed, locking the operator out of the real credentials via the
    //   UI/CLI. So MariaDB is gated on the backup FORMAT, matching the
    //   plan preview (which excludes `mariadb_dump_restore` from the same
    //   classification) — the location string tells us the format without
    //   an extra S3 round-trip.
    //
    // S3/RustFS: NO. No auth in the data layer.
    //
    // Two separate gates. `engine_preserves_source_credentials` drives the
    // post-restore patch (Postgres, Mongo, physical MariaDB). The pre-restore
    // merge covers the OFFLINE restores (Postgres, physical MariaDB): the
    // engine's view of the "current password" during the data swap doesn't
    // matter, and the merged creds are what the restored data will actually
    // expect. Mongo's restore is online so merging too early breaks the
    // mongorestore auth.
    //
    // See `credential_propagation_gates` for the per-engine (and, for MariaDB,
    // per-FORMAT) reasoning; it is a free function so the matrix is unit-tested.
    let (engine_preserves_source_credentials, engine_wants_pre_restore_credential_merge) =
        credential_propagation_gates(&target_service.service_type, &backup_model.s3_location);

    // Redis keeps Temps' own bookkeeping (`_temps:*` in database 0) in the
    // data: another service's backup must not bring its database mappings
    // and owners into the target. The restore keeps the target's.
    if matches!(mode, RestoreRequestMode::InPlace)
        && worker_redis::is_redis(&target_service.service_type)
        && worker_redis::keeps_target_bookkeeping(origin_service_id, target_service.id)
    {
        temps_providers::externalsvc::redis::restore_swap::keep_target_bookkeeping(
            &mut source_config,
        );
    }

    let mut origin_credentials_for_post_restore_patch = None;
    if engine_preserves_source_credentials {
        if let Some(origin_id) = origin_service_id {
            if origin_id != target_service.id {
                match mgr.get_service_config(origin_id).await {
                    Ok(origin_cfg) => {
                        if engine_wants_pre_restore_credential_merge {
                            // Offline engines (Postgres, MariaDB): merge origin
                            // credentials into the config we pass to the engine.
                            // The restore replaces the auth catalog wholesale
                            // (pg_authid / mysql.user), so the merged creds are
                            // exactly what the restored server will expect.
                            if let Some(target_params) = source_config.parameters.as_object_mut() {
                                target_params.extend(origin_login_merge(&origin_cfg.parameters));
                            }
                            info!(
                                "Merged origin service ({}) credentials into restore config for target service {}",
                                origin_id, target_service.id
                            );
                        } else {
                            // Online restore (Mongo). Don't overwrite the
                            // target's live-auth password, but expose the
                            // origin's as `_alt_password` so the engine
                            // can use it as a fallback if a prior failed
                            // restore already flipped admin.system.users.
                            if let (Some(target_params), Some(origin_password)) = (
                                source_config.parameters.as_object_mut(),
                                origin_cfg
                                    .parameters
                                    .get("password")
                                    .and_then(|v| v.as_str()),
                            ) {
                                target_params.insert(
                                    "_alt_password".to_string(),
                                    serde_json::Value::String(origin_password.to_string()),
                                );
                            }
                            info!(
                                "Engine '{}' restore is online — attached origin password as _alt_password fallback for auth probe. Origin password captured for post-restore config patch.",
                                target_service.service_type
                            );
                        }
                        // Physical PostgreSQL backups carry the origin role and
                        // database as well as its password. Preserve the target's
                        // physical settings while reconciling that restored login.
                        origin_credentials_for_post_restore_patch =
                            RestoredServiceCredentials::from_origin(
                                &target_service.service_type,
                                &origin_cfg.parameters,
                            );
                    }
                    Err(e) => {
                        warn!(
                            "Failed to read origin service {} config; restore will use target credentials which may not match restored data: {}",
                            origin_id, e
                        );
                    }
                }
            }
        }
    } else {
        info!(
            "Backup '{}' for engine '{}' does not propagate source credentials; skipping origin-password merge for target service {}",
            backup_model.s3_location, target_service.service_type, target_service.id
        );
    }

    let instance = mgr
        .resolve_service_instance(&target_service.name, service_type)
        .await
        .map_err(|e| RestoreError::ExternalService {
            reason: format!("get service instance: {}", e),
        })?;

    // Decrypt S3 credentials once.
    let decrypted_access_key =
        enc.decrypt_string(&s3_source.access_key_id)
            .map_err(|e| RestoreError::Encryption {
                reason: format!("Failed to decrypt access key: {}", e),
            })?;
    let decrypted_secret_key =
        enc.decrypt_string(&s3_source.secret_key)
            .map_err(|e| RestoreError::Encryption {
                reason: format!("Failed to decrypt secret key: {}", e),
            })?;

    // `None` for every long-lived operator-configured credential, so nothing
    // about those restores changes.
    let decrypted_session_token =
        temps_entities::s3_sources::decrypt_session_token(enc.as_ref(), &s3_source).map_err(
            |e| RestoreError::Encryption {
                reason: format!("Failed to decrypt session token: {}", e),
            },
        )?;

    let s3_credentials = S3Credentials {
        access_key_id: decrypted_access_key.clone(),
        secret_key: decrypted_secret_key.clone(),
        session_token: decrypted_session_token.clone(),
        region: s3_source.region.clone(),
        endpoint: s3_source.endpoint.clone(),
        bucket_name: s3_source.bucket_name.clone(),
        bucket_path: s3_source.bucket_path.clone(),
        force_path_style: s3_source.force_path_style.unwrap_or(true),
    };

    // Build a plaintext copy of the source row. The original model carries
    // ENCRYPTED access/secret keys; any engine that reaches into `s3_source`
    // for mc-alias setup (s3/rustfs/blob) needs plaintext or it will pass
    // ciphertext to mc and get "not signed up" back. `backup_to_s3` already
    // decrypts before passing; the restore dispatch path did not — that was
    // the source of the in-place-restore auth failure. `session_token` is on
    // the same footing: leaving it encrypted here would sign a ciphertext
    // token and get a 403 back from the provider.
    let s3_source_plain = temps_entities::s3_sources::Model {
        access_key_id: decrypted_access_key.clone(),
        secret_key: decrypted_secret_key.clone(),
        session_token: decrypted_session_token,
        ..s3_source.clone()
    };

    let s3_client = build_s3_client(&s3_credentials);

    // Backfill an empty `s3_location` by probing the S3 source for a
    // backup under the origin service's conventional path. Fixes restore
    // for old `backups` rows written before the state/location update
    // bug was patched. We walk the same `external_services/<engine>/<svc>/`
    // prefix that `list_source_backups` uses and prefer a WAL-G prefix
    // when present (restores are much cheaper).
    if backup_model.s3_location.is_empty() {
        let origin_service_name = serde_json::from_str::<serde_json::Value>(&backup_model.metadata)
            .ok()
            .and_then(|v| {
                v.get("service_name")
                    .and_then(|s| s.as_str())
                    .map(String::from)
            });
        let engine = target_service.service_type.clone();

        if let Some(origin) = origin_service_name {
            let resolved = resolve_backup_location_from_s3(
                &s3_client,
                &s3_source,
                &engine,
                &origin,
                &backup_model.backup_id,
            )
            .await;
            match resolved {
                Ok(Some(loc)) => {
                    info!(
                        "Backfilled empty s3_location for backup {} -> {}",
                        backup_model.id, loc
                    );
                    backup_model.s3_location = loc;
                }
                Ok(None) => {
                    return Err(RestoreError::Validation {
                        message: format!(
                            "Backup {} has no s3_location and no matching backup was found in S3 under external_services/{}/{}/. Cannot restore.",
                            backup_model.id, engine, origin
                        ),
                    });
                }
                Err(e) => {
                    return Err(RestoreError::ExternalService {
                        reason: format!(
                            "Backup {} has no s3_location; S3 probe to find it failed: {}",
                            backup_model.id, e
                        ),
                    });
                }
            }
        } else {
            return Err(RestoreError::Validation {
                message: format!(
                    "Backup {} has no s3_location and no origin service_name in metadata to probe for one.",
                    backup_model.id
                ),
            });
        }
    }

    // Leave `prepare`. Every move out of a cancellable phase is conditional
    // on no cancellation having been recorded (see `restore_cancel`).
    let destructive = mode.is_destructive();
    let write_phase = match &mode {
        RestoreRequestMode::Pitr { .. } => "recover",
        _ => "restore",
    };
    let gate_target = match &mode {
        RestoreRequestMode::NewService { name, .. } => name.clone(),
        RestoreRequestMode::Pitr {
            to_new_service: true,
            new_service_name: Some(name),
            ..
        } => name.clone(),
        _ => target_service.name.clone(),
    };
    let gate = super::restore_cancel::WorkerGate::new(
        db.as_ref(),
        run_id,
        cancel.clone(),
        destructive,
        write_phase,
        gate_target,
    );
    let is_postgres_cluster =
        target_service.topology == "cluster" && target_service.service_type == "postgres";
    if destructive {
        if !is_postgres_cluster && instance.defers_target_writes(&backup_model.s3_location) {
            // The engine stages the backup first and passes the gate before
            // its first write, so the download stays cancellable.
            super::restore_cancel::enter_phase_unless_cancelled(db.as_ref(), run_id, "download")
                .await?;
        } else {
            // The engine writes as it reads: the last safe point is now.
            gate.enter_write_phase().await?;
        }
    } else {
        if let Some(new_name) = mode.new_service_name() {
            // Cancelling tears the new service down by its derived container
            // and volume names, so this run must own those names outright.
            super::restore_cancel::ensure_new_service_name_unclaimed(
                db.as_ref(),
                &super::restore_cancel::ManagerServiceTeardown::new(mgr.clone()),
                run_id,
                new_name,
                service_type,
            )
            .await?;
        }
        super::restore_cancel::enter_phase_unless_cancelled(db.as_ref(), run_id, "provision")
            .await?;
    }

    let ctx = RestoreContext {
        s3_client: &s3_client,
        s3_credentials: &s3_credentials,
        s3_source: &s3_source_plain,
        backup: &backup_model,
        backup_location: &backup_model.s3_location,
        source_service: &target_service,
        source_config: source_config.clone(),
        live_target_config,
        pool: db.as_ref(),
        gate: &gate,
    };

    let new_service_parameters = match mode {
        RestoreRequestMode::InPlace => {
            // Cluster topology routes through the manager: in-place
            // restore tears down every member, pre-seeds the primary's
            // pgdata from the WAL-G prefix, then rebuilds the cluster
            // around it. The trait-level restore_from_s3 doesn't have
            // access to service_members or the agent protocol so it
            // can't handle clusters — same carve-out as backup.
            let result = if is_postgres_cluster {
                match selected_walg_target_user_data(&backup_model) {
                    Ok(target_user_data) => mgr
                        .restore_postgres_cluster(
                            &target_service,
                            &backup_model.s3_location,
                            &s3_credentials,
                            target_user_data.as_deref(),
                        )
                        .await
                        .map_err(|e| RestoreError::ExternalService {
                            reason: format!("cluster in-place restore failed: {}", e),
                        }),
                    Err(e) => Err(e),
                }
            } else {
                instance
                    .restore_in_place(ctx)
                    .await
                    .map_err(|e| engine_error(run_id, e, "in-place restore failed"))
            };
            if let Err(e) = result {
                return Err(settle_destructive_failure(&db, run_id, &gate, e).await);
            }
            None
        }
        RestoreRequestMode::NewService {
            name,
            parameter_overrides,
        } => {
            // Everything this writes lands in a service the run is creating.
            // A cancellation lets the provision stop at its next check (the
            // gate) rather than dropping it mid-call: a dropped Docker request
            // can still create a container or volume after teardown looked.
            // Only once the engine has returned is the service torn down.
            let fence = super::restore_reconcile::ManagerRestoreFence::new(mgr.clone());
            let provisioned = await_new_service_provision(
                instance.restore_to_new_service(ctx, name.clone(), parameter_overrides),
                &cancel,
                &fence,
                &new_service_fence_target(&name, service_type),
                NEW_SERVICE_CANCEL_FENCE_INTERVAL,
            )
            .await;
            if cancel.is_cancelled() {
                return Err(cancel_new_service(&db, &mgr, run_id, &name, service_type).await);
            }
            match provisioned {
                Ok(result) => Some((name, result)),
                Err(e) => return Err(engine_error(run_id, e, "new-service restore failed")),
            }
        }
        RestoreRequestMode::Pitr {
            to_new_service,
            new_service_name,
            target,
        } => {
            if to_new_service {
                let name = new_service_name
                    .clone()
                    .ok_or_else(|| RestoreError::Internal {
                        reason: format!(
                            "PITR restore run {} to a new service has no new service name",
                            run_id
                        ),
                    })?;
                // As for a new-service restore: let the recovery return
                // before tearing down what it created.
                let fence = super::restore_reconcile::ManagerRestoreFence::new(mgr.clone());
                let recovered = await_new_service_provision(
                    instance.restore_pitr(ctx, target, true, new_service_name.clone()),
                    &cancel,
                    &fence,
                    &new_service_fence_target(&name, service_type),
                    NEW_SERVICE_CANCEL_FENCE_INTERVAL,
                )
                .await;
                if cancel.is_cancelled() {
                    return Err(cancel_new_service(&db, &mgr, run_id, &name, service_type).await);
                }
                match recovered {
                    Ok(Some(result)) => Some((name, result)),
                    Ok(None) => {
                        return Err(RestoreError::Internal {
                            reason: "PITR to_new_service did not return a new service result"
                                .into(),
                        })
                    }
                    Err(e) => return Err(engine_error(run_id, e, "PITR failed")),
                }
            } else {
                if let Err(e) = instance
                    .restore_pitr(ctx, target, false, None)
                    .await
                    .map_err(|e| engine_error(run_id, e, "PITR failed"))
                {
                    return Err(settle_destructive_failure(&db, run_id, &gate, e).await);
                }
                None
            }
        }
    };

    // The destructive step is done. Failing the run now because a progress
    // write failed would report a restore that happened as one that did not.
    if let Some((new_name, _)) = &new_service_parameters {
        // Registering the new service is the point of no return for a
        // new-service restore: a cancellation recorded before it still wins
        // and removes what was provisioned.
        match super::restore_cancel::enter_phase_unless_cancelled_retrying(
            db.as_ref(),
            run_id,
            "verify",
            &REGISTRATION_GATE_RETRY.config(),
        )
        .await
        {
            Ok(()) => {}
            Err(RestoreError::RestoreCancelled { .. }) => {
                return Err(cancel_new_service(&db, &mgr, run_id, new_name, service_type).await)
            }
            Err(e) => {
                // Without the guarded move the worker cannot tell whether a
                // cancellation was accepted meanwhile, so it must not
                // register the service. Remove what it provisioned instead.
                let cleanup = teardown_new_service(&db, &mgr, run_id, new_name, service_type).await;
                return Err(RestoreError::ExternalService {
                    reason: format!(
                        "{} The new service '{}' was not registered. {}",
                        e,
                        new_name,
                        super::restore_cancel::cleanup_note(false, &cleanup)
                    ),
                });
            }
        }
    } else if let Err(e) = update_phase(&db, run_id, "verify").await {
        warn!(
            "Restore run {} could not record its 'verify' phase; continuing: {}",
            run_id, e
        );
    }

    // If a new service was created, insert its row now and link it to
    // the run. The template is the TARGET service (we're provisioning a
    // sibling of it), not the backup's origin service.
    let target_service_id = if let Some((new_name, result)) = new_service_parameters {
        let new_id =
            persist_new_service(&db, &enc, &target_service, run_id, new_name, &result).await?;
        Some(new_id)
    } else {
        // In-place / PITR-in-place: the restored auth catalog carries the
        // origin's credentials. PostgreSQL also inherits its role/database,
        // MongoDB its root username.
        // Reconcile the encrypted login while retaining the target's endpoint
        // and physical ownership so UI/env vars/CLI match restored data.
        reconcile_restored_credentials(
            &db,
            &enc,
            target_service.id,
            origin_credentials_for_post_restore_patch.as_ref(),
        )
        .await;
        None
    };

    Ok(target_service_id)
}

/// How often a cancelled new-service restore re-stops its helpers while the
/// engine winds down.
const NEW_SERVICE_CANCEL_FENCE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

fn new_service_fence_target(
    name: &str,
    service_type: ServiceType,
) -> super::restore_reconcile::FenceTarget {
    super::restore_reconcile::FenceTarget {
        service_name: name.to_string(),
        service_type: service_type.to_string(),
        imported_container: None,
    }
}

/// Await an engine call that provisions a new service, staying responsive to
/// cancellation without abandoning it.
///
/// The call is never dropped mid-flight: a dropped Docker request can still
/// create a container or volume after teardown has looked. Not every engine
/// checks the restore gate, though (a Redis WAL-G helper can run for an
/// hour), so once `cancel` fires the new service's restore helpers and staged
/// downloads are stopped every `interval` until the engine returns, which
/// makes it fail fast instead of finishing a restore that will be torn down.
async fn await_new_service_provision<T>(
    provision: impl std::future::Future<Output = T>,
    cancel: &tokio_util::sync::CancellationToken,
    fence: &dyn super::restore_reconcile::RestoreHelperFence,
    target: &super::restore_reconcile::FenceTarget,
    interval: std::time::Duration,
) -> T {
    tokio::pin!(provision);
    tokio::select! {
        biased;
        output = &mut provision => return output,
        _ = cancel.cancelled() => {}
    }
    let mut ticks = tokio::time::interval(interval);
    loop {
        tokio::select! {
            biased;
            output = &mut provision => return output,
            _ = ticks.tick() => {
                if let Err(e) = fence.fence(target).await {
                    warn!(
                        "Cancelled restore into '{}' could not stop its restore helpers yet: {}",
                        target.service_name, e
                    );
                }
            }
        }
    }
}

/// Map an engine error to a [`RestoreError`], keeping the one outcome the
/// worker must act on rather than report: a staged download that could not be
/// confirmed stopped.
fn engine_error(
    run_id: i32,
    error: temps_providers::externalsvc::EngineError,
    context: &str,
) -> RestoreError {
    use temps_providers::externalsvc::EngineError;
    match error {
        EngineError::StagedRestoreStillRunning(still_running) => {
            RestoreError::StagedRestoreStillRunning {
                restore_run_id: run_id,
                target: still_running.target,
                reason: still_running.reason,
            }
        }
        other => RestoreError::ExternalService {
            reason: format!("{}: {}", context, other),
        },
    }
}

/// Outcome of a destructive engine call that returned an error.
///
/// If the run was cancelled and the engine never passed the gate, the error is
/// the engine stopping on request: the target was not written, so the run is
/// recorded as cancelled. Otherwise it is a real failure.
async fn settle_destructive_failure(
    db: &DatabaseConnection,
    run_id: i32,
    gate: &super::restore_cancel::WorkerGate<'_, DatabaseConnection>,
    error: RestoreError,
) -> RestoreError {
    if !gate.signalled()
        || gate.writes_started()
        || matches!(error, RestoreError::StagedRestoreStillRunning { .. })
    {
        return error;
    }
    let phase = load_run_for_update(db, run_id)
        .await
        .map(|run| run.phase)
        .unwrap_or_else(|_| "download".to_string());
    RestoreError::Cancelled {
        restore_run_id: run_id,
        message: super::restore_cancel::cancelled_message(
            &phase,
            true,
            &super::restore_cancel::CancelCleanup::TargetUntouched,
        ),
        phase,
    }
}

/// Fence any helper still writing into a half-created new service, then
/// remove its container and volume. Safe because the run checked that it owns
/// every name the service derives (see `ensure_new_service_name_unclaimed`).
async fn teardown_new_service(
    db: &DatabaseConnection,
    mgr: &Arc<ExternalServiceManager>,
    run_id: i32,
    new_name: &str,
    service_type: ServiceType,
) -> super::restore_cancel::CancelCleanup {
    use super::restore_reconcile::RestoreHelperFence;

    let fence = super::restore_reconcile::ManagerRestoreFence::new(mgr.clone());
    let fence_target = super::restore_reconcile::FenceTarget {
        service_name: new_name.to_string(),
        service_type: service_type.to_string(),
        imported_container: None,
    };
    if let Err(e) = fence.fence(&fence_target).await {
        warn!(
            "Restore run {} stopping: could not stop restore helpers for '{}': {}",
            run_id, new_name, e
        );
    }
    let manager = super::restore_cancel::ManagerServiceTeardown::new(mgr.clone());
    super::restore_cancel::teardown_cancelled_new_service(
        db,
        &manager,
        &manager,
        new_name,
        service_type,
    )
    .await
}

/// Stop a cancelled new-service restore: fence any helper still writing into
/// the half-created service, then remove its container and volume.
async fn cancel_new_service(
    db: &DatabaseConnection,
    mgr: &Arc<ExternalServiceManager>,
    run_id: i32,
    new_name: &str,
    service_type: ServiceType,
) -> RestoreError {
    let cleanup = teardown_new_service(db, mgr, run_id, new_name, service_type).await;
    let phase = load_run_for_update(db, run_id)
        .await
        .map(|run| run.phase)
        .unwrap_or_else(|_| "provision".to_string());
    RestoreError::Cancelled {
        restore_run_id: run_id,
        message: super::restore_cancel::cancelled_message(&phase, false, &cleanup),
        phase,
    }
}

/// Credentials inherited from a restored authentication catalog. PostgreSQL
/// physical backups also carry the origin's role and database. A MongoDB
/// dump carries the origin's users (`admin.system.users`, which
/// `mongorestore --drop` puts in place of the target's), so the target then
/// answers to the origin's root username as well as its password; its
/// database is the target's own. Other engines retain their existing
/// login-identity behavior.
struct RestoredServiceCredentials {
    /// `None` when the restore keeps the target's own password (a logical
    /// PostgreSQL restore by a job, [`Self::into_database_only`]).
    password: Option<String>,
    root_password: Option<String>,
    username: Option<String>,
    database: Option<String>,
}

impl RestoredServiceCredentials {
    fn from_origin(service_type: &str, parameters: &serde_json::Value) -> Option<Self> {
        let postgres = service_type == "postgres";
        let restores_username = postgres || service_type == "mongodb";
        Some(Self {
            password: Some(parameters.get("password")?.as_str()?.to_string()),
            root_password: parameters
                .get("root_password")
                .and_then(|v| v.as_str())
                .map(String::from),
            username: restores_username
                .then(|| {
                    parameters
                        .get("username")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                })
                .flatten(),
            database: postgres
                .then(|| {
                    parameters
                        .get("database")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                })
                .flatten(),
        })
    }

    /// What a logical PostgreSQL restore by a job leaves to reconcile: the
    /// database the origin's data was restored into. The job keeps the
    /// target's login and its password through the dump (the dump's own role
    /// passwords are those of backup time, which Temps does not know), so
    /// neither changes. `None` when the origin names no database.
    fn into_database_only(self) -> Option<Self> {
        Some(Self {
            password: None,
            root_password: None,
            username: None,
            database: Some(self.database?),
        })
    }

    fn apply(&self, parameters: &mut serde_json::Map<String, serde_json::Value>) {
        if let Some(password) = &self.password {
            parameters.insert(
                "password".to_string(),
                serde_json::Value::String(password.clone()),
            );
        }
        if parameters.contains_key("root_password") {
            if let Some(password) = &self.root_password {
                parameters.insert(
                    "root_password".to_string(),
                    serde_json::Value::String(password.clone()),
                );
            }
        }
        for (key, value) in [("username", &self.username), ("database", &self.database)] {
            if let Some(value) = value {
                parameters.insert(key.to_string(), serde_json::Value::String(value.clone()));
            }
        }
    }
}

/// Reconcile encrypted credentials after in-place restore without changing the
/// target's image, endpoint, resource limits, volume or imported container name.
async fn patch_service_credentials(
    db: &DatabaseConnection,
    enc: &Arc<temps_core::EncryptionService>,
    service_id: i32,
    credentials: &RestoredServiceCredentials,
) -> Result<(), RestoreError> {
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};

    let service = temps_entities::external_services::Entity::find_by_id(service_id)
        .one(db)
        .await?
        .ok_or(RestoreError::ServiceNotFound { service_id })?;

    let encrypted = service
        .config
        .clone()
        .ok_or_else(|| RestoreError::Internal {
            reason: format!("Service {} has no config to patch", service_id),
        })?;

    let decrypted = enc
        .decrypt_string(&encrypted)
        .map_err(|e| RestoreError::Encryption {
            reason: format!("Failed to decrypt service {} config: {}", service_id, e),
        })?;

    let mut params: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&decrypted).map_err(|e| RestoreError::Internal {
            reason: format!(
                "Failed to parse service {} config as JSON object: {}",
                service_id, e
            ),
        })?;

    credentials.apply(&mut params);

    let re_serialized = serde_json::to_string(&params).map_err(|e| RestoreError::Internal {
        reason: format!("Failed to re-serialize patched config: {}", e),
    })?;

    let re_encrypted =
        enc.encrypt_string(&re_serialized)
            .map_err(|e| RestoreError::Encryption {
                reason: format!("Failed to encrypt patched config: {}", e),
            })?;

    let mut active: temps_entities::external_services::ActiveModel = service.into();
    active.config = Set(Some(re_encrypted));
    active.update(db).await?;
    Ok(())
}

/// Insert a restore run while holding the source backup's row lock (so a
/// concurrent backup deletion cannot remove data underneath it) and, for a
/// destructive restore, the target service's row lock plus a check that no
/// other destructive restore is active on it. A second destructive restore
/// gets [`RestoreError::RestoreAlreadyActive`] naming the active run.
async fn insert_restore_run(
    db: &DatabaseConnection,
    run_active: temps_entities::restore_runs::ActiveModel,
    resolved_backup_id: Option<i32>,
    target_service_id: i32,
    destructive: bool,
) -> Result<temps_entities::restore_runs::Model, RestoreError> {
    let transaction = db.begin().await?;
    let inserted = lock_and_insert_restore_run(
        &transaction,
        run_active,
        resolved_backup_id,
        target_service_id,
        destructive,
    )
    .await;
    let error = match inserted {
        Ok(run) => {
            transaction.commit().await?;
            return Ok(run);
        }
        Err(error) => error,
    };
    // Roll back explicitly instead of on drop: a dropped transaction only
    // queues its ROLLBACK, so the backup and service row locks taken above
    // would stay held until the pool next touches that connection.
    if let Err(rollback_error) = transaction.rollback().await {
        warn!(
            "Rolling back the refused restore run for service {} failed: {}",
            target_service_id, rollback_error
        );
    }
    match error {
        RestoreError::Database(db_error) if is_active_restore_conflict(&db_error) => {
            // The index fired, so another in-place run committed between
            // our check and insert. If it already finished, surface the
            // original error rather than naming a run that is gone.
            Err(
                match find_active_destructive_run(db, target_service_id).await? {
                    Some(active) => RestoreError::RestoreAlreadyActive {
                        service_id: target_service_id,
                        restore_run_id: active.id,
                    },
                    None => RestoreError::Database(db_error),
                },
            )
        }
        other => Err(other),
    }
}

async fn reserve_restore_service_name(
    txn: &sea_orm::DatabaseTransaction,
    name: &str,
    owning_run: Option<i32>,
) -> Result<(), RestoreError> {
    use temps_providers::services::ServiceIdentityReservationError as E;
    temps_providers::services::reserve_service_identity(txn, name, None, owning_run)
        .await
        .map_err(|error| match error {
            E::Database(error) => RestoreError::Database(error),
            E::Registered { .. } | E::Restoring { .. } => RestoreError::ServiceNameConflict {
                name: name.to_string(),
                reason: error.to_string(),
            },
        })
}

/// Only a `completed` backup may be restored. `deleting` keeps its own error
/// so the console can say why; every other state (`pending`, `running`,
/// `failed`) has no complete snapshot behind its `s3_location`.
fn ensure_backup_restorable(backup: &temps_entities::backups::Model) -> Result<(), RestoreError> {
    match backup.state.as_str() {
        "completed" => Ok(()),
        "deleting" => Err(RestoreError::BackupDeleting {
            backup_id: backup.id,
        }),
        state => Err(RestoreError::BackupNotRestorable {
            backup_id: backup.id,
            state: state.to_string(),
        }),
    }
}

/// The checks and insert of [`insert_restore_run`], inside its transaction.
async fn lock_and_insert_restore_run(
    transaction: &sea_orm::DatabaseTransaction,
    run_active: temps_entities::restore_runs::ActiveModel,
    resolved_backup_id: Option<i32>,
    target_service_id: i32,
    destructive: bool,
) -> Result<temps_entities::restore_runs::Model, RestoreError> {
    // Admission durably reserves the clone's name under the same short
    // lock used by create/import/rename, before any Docker operation starts.
    if let Set(Some(name)) | sea_orm::ActiveValue::Unchanged(Some(name)) =
        &run_active.target_service_name
    {
        reserve_restore_service_name(transaction, name, None).await?;
    }
    if let Some(backup_id) = resolved_backup_id {
        let backup = temps_entities::backups::Entity::find_by_id(backup_id)
            .lock_exclusive()
            .one(transaction)
            .await?
            .ok_or(RestoreError::BackupNotFound { backup_id })?;
        ensure_backup_restorable(&backup)?;
    }
    // Deletion takes this same short source lock and refuses active runs,
    // so its cascade cannot remove a clone's durable name reservation.
    temps_entities::external_services::Entity::find_by_id(target_service_id)
        .lock_exclusive()
        .one(transaction)
        .await?
        .ok_or(RestoreError::ServiceNotFound {
            service_id: target_service_id,
        })?;
    if destructive {
        // Serialize destructive restores per target: the row lock makes
        // the check below race-free for PITR-in-place too, which the
        // partial unique index (in_place only) does not cover.
        if let Some(active) = find_active_destructive_run(transaction, target_service_id).await? {
            return Err(RestoreError::RestoreAlreadyActive {
                service_id: target_service_id,
                restore_run_id: active.id,
            });
        }
        // A data import writes into the same service. Imports take this same
        // row lock before checking for restores, so exactly one of the two
        // can start.
        if let Some(import) = temps_entities::service_data_imports::Entity::find()
            .filter(temps_entities::service_data_imports::Column::ServiceId.eq(target_service_id))
            .filter(temps_entities::service_data_imports::Column::Status.eq("running"))
            .one(transaction)
            .await?
        {
            return Err(RestoreError::DataImportActive {
                service_id: target_service_id,
                data_import_run_id: import.id,
            });
        }
    }
    Ok(run_active.insert(transaction).await?)
}

/// Decide how to record a run whose worker task panicked.
///
/// The panic may have left a restore helper container running, and marking
/// the run terminal releases the one-active-restore constraint, so the
/// helpers are fenced first. `Ok` carries the error to record as the run's
/// failure; `Err` means the helpers could not be fenced (or the run could not
/// be read) and the run must stay active until startup reconciliation, which
/// fences again before releasing it.
async fn settle_crashed_run(
    db: &DatabaseConnection,
    run_id: i32,
    fence: &dyn super::restore_reconcile::RestoreHelperFence,
    reason: String,
) -> Result<RestoreError, RestoreError> {
    let run = load_run_for_update(db, run_id).await?;
    let service = temps_entities::external_services::Entity::find_by_id(run.source_service_id)
        .one(db)
        .await?;
    let crashed = RestoreError::WorkerCrashed {
        restore_run_id: run_id,
        phase: run.phase.clone(),
        reason,
    };
    match super::restore_reconcile::fence_run_helpers(&run, service.as_ref(), fence).await {
        Ok(_) => Ok(crashed),
        Err(e) => Err(RestoreError::Internal {
            reason: format!(
                "{}. Its restore helpers could not be stopped ({}), so the run stays \
                 active until the next restart reconciles it.",
                crashed, e
            ),
        }),
    }
}

/// Confirm that a cancelled run's staged download has stopped, retrying for
/// about a minute (Docker may be briefly unavailable). Uses the same fence the
/// startup reconciliation does, which also runs the engine's
/// [`temps_providers::externalsvc::ExternalService::stop_staged_restore`].
/// Returns the run's phase once confirmed.
async fn confirm_staged_restore_stopped(
    db: &DatabaseConnection,
    run_id: i32,
    fence: &dyn super::restore_reconcile::RestoreHelperFence,
) -> Result<String, RestoreError> {
    let run = load_run_for_update(db, run_id).await?;
    let service = temps_entities::external_services::Entity::find_by_id(run.source_service_id)
        .one(db)
        .await?;
    STAGED_STOP_RETRY
        .config()
        .retry(|| async {
            super::restore_reconcile::fence_run_helpers(&run, service.as_ref(), fence)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|reason| RestoreError::Internal { reason })?;
    Ok(run.phase)
}

/// Retry policy for confirming a cancelled download stopped before the run
/// releases its service.
const STAGED_STOP_RETRY: TerminalWriteRetry = TerminalWriteRetry {
    attempts: 5,
    base_delay: std::time::Duration::from_secs(2),
    max_delay: std::time::Duration::from_secs(20),
};

/// Retry policy for the guarded move that precedes registering a new service.
/// Short: the run is holding a provisioned container while it waits.
const REGISTRATION_GATE_RETRY: TerminalWriteRetry = TerminalWriteRetry {
    attempts: 5,
    base_delay: std::time::Duration::from_millis(500),
    max_delay: std::time::Duration::from_secs(5),
};

/// Retry policy for the worker's final status write. A restore that ran to
/// an outcome must not be left `running` because of a database blip, so the
/// write is retried for roughly five minutes before giving up to the next
/// startup reconciliation.
const TERMINAL_WRITE_RETRY: TerminalWriteRetry = TerminalWriteRetry {
    attempts: 8,
    base_delay: std::time::Duration::from_secs(1),
    max_delay: std::time::Duration::from_secs(60),
};

#[derive(Debug, Clone, Copy)]
struct TerminalWriteRetry {
    attempts: u32,
    base_delay: std::time::Duration,
    max_delay: std::time::Duration,
}

impl TerminalWriteRetry {
    fn config(&self) -> temps_core::retry::RetryConfig {
        temps_core::retry::RetryConfig::new(self.attempts)
            .with_base_delay(self.base_delay)
            .with_max_delay(self.max_delay)
    }
}

/// Record a run's terminal outcome. Only an active (`pending`/`running`) row
/// is updated, so a run already reconciled as `interrupted` is never
/// overwritten with an outcome nobody observed end to end.
async fn persist_terminal_state(
    db: &DatabaseConnection,
    run_id: i32,
    result: &Result<Option<i32>, RestoreError>,
    retry: TerminalWriteRetry,
) -> Result<(), RestoreError> {
    use sea_orm::sea_query::Expr;
    use temps_entities::restore_runs::{Column, Entity};

    let now = Utc::now();
    let mut update = Entity::update_many()
        .col_expr(Column::FinishedAt, Expr::value(now))
        .col_expr(Column::UpdatedAt, Expr::value(now));
    update = match result {
        Ok(target_service_id) => update
            .col_expr(Column::Status, Expr::value("completed"))
            .col_expr(Column::Phase, Expr::value("completed"))
            .col_expr(Column::TargetServiceId, Expr::value(*target_service_id))
            .col_expr(Column::ErrorMessage, Expr::value(Option::<String>::None)),
        // A cancelled run keeps the phase it stopped in, like an interrupted
        // one, so the console can say where it stopped.
        Err(RestoreError::Cancelled { message, .. }) => update
            .col_expr(Column::Status, Expr::value("cancelled"))
            .col_expr(Column::ErrorMessage, Expr::value(Some(message.clone()))),
        Err(e) => update
            .col_expr(Column::Status, Expr::value("failed"))
            .col_expr(Column::Phase, Expr::value("failed"))
            .col_expr(Column::ErrorMessage, Expr::value(Some(e.to_string()))),
    };
    let update = update
        .filter(Column::Id.eq(run_id))
        .filter(Column::Status.is_in(ACTIVE_RESTORE_STATUSES));

    let mut delay = retry.base_delay;
    let mut attempt = 1;
    loop {
        match update.clone().exec(db).await {
            Ok(outcome) => {
                if outcome.rows_affected == 0 {
                    warn!(
                        "Restore run {} was no longer active when its outcome was recorded; \
                         leaving its stored status unchanged",
                        run_id
                    );
                }
                return Ok(());
            }
            Err(e) if attempt < retry.attempts => {
                warn!(
                    "Recording the outcome of restore run {} failed (attempt {}/{}): {}",
                    run_id, attempt, retry.attempts, e
                );
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(retry.max_delay);
                attempt += 1;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Statuses that mean a worker still owns the run.
pub(crate) const ACTIVE_RESTORE_STATUSES: [&str; 2] = ["pending", "running"];

/// The active destructive restore (in-place, or PITR in place) on `service_id`,
/// if any. At most one can exist.
async fn find_active_destructive_run<C: sea_orm::ConnectionTrait>(
    conn: &C,
    service_id: i32,
) -> Result<Option<temps_entities::restore_runs::Model>, RestoreError> {
    use sea_orm::Condition;
    use temps_entities::restore_runs::{Column, Entity};
    Ok(Entity::find()
        .filter(Column::SourceServiceId.eq(service_id))
        .filter(Column::Status.is_in(ACTIVE_RESTORE_STATUSES))
        .filter(
            Condition::any().add(Column::Mode.eq("in_place")).add(
                Condition::all()
                    .add(Column::Mode.eq("pitr"))
                    .add(Column::TargetServiceName.is_null()),
            ),
        )
        .order_by_desc(Column::Id)
        .one(conn)
        .await?)
}

/// Whether an insert failed on the one-active-in-place-restore index.
fn is_active_restore_conflict(error: &sea_orm::DbErr) -> bool {
    matches!(
        error.sql_err(),
        Some(sea_orm::SqlErr::UniqueConstraintViolation(message))
            if message.contains("idx_restore_runs_source_service_active")
    )
}

async fn load_run_for_update(
    db: &DatabaseConnection,
    run_id: i32,
) -> Result<temps_entities::restore_runs::Model, RestoreError> {
    temps_entities::restore_runs::Entity::find_by_id(run_id)
        .one(db)
        .await?
        .ok_or(RestoreError::RestoreRunNotFound {
            restore_run_id: run_id,
        })
}

/// Whether a restore of `backup_location` onto `target`, a database whose
/// backups and restores run as jobs on its worker node, is one a job can do:
/// an in-place apply of a `pg_dumpall` dump.
fn job_restore_supported(
    target: &temps_entities::external_services::Model,
    backup_location: &str,
    mode: &RestoreRequestMode,
) -> Result<(), RestoreError> {
    let node = crate::engines::workload_job::worker_node(target).unwrap_or_default();
    if !matches!(mode, RestoreRequestMode::InPlace) {
        return Err(RestoreError::Validation {
            message: format!(
                "Database {} ('{}') runs on worker node {node}, where restores run as a job that \
                 applies a dump in place; restore mode '{}' is not available there yet. Restore \
                 in place",
                target.id,
                target.name,
                mode.as_str()
            ),
        });
    }
    if !crate::engines::workload_job::restore_takes_job_path(target, backup_location) {
        return Err(RestoreError::Validation {
            message: format!(
                "Database {} ('{}') runs on worker node {node}, where a restore applies a \
                 pg_dumpall dump (`.sql.gz`) or a WAL-G backup (`s3://…`); backup \
                 '{backup_location}' is neither",
                target.id, target.name
            ),
        });
    }
    Ok(())
}

/// What a service on a worker node can restore: in place, from any backup in
/// the format its node applies, its own or another service's (confirmed as
/// a cross-service restore, like on the control plane). Restoring into a new
/// service, or to a point in time, is refused explicitly at admission.
fn worker_restore_capabilities() -> temps_providers::externalsvc::RestoreCapabilities {
    temps_providers::externalsvc::RestoreCapabilities {
        restore_in_place: true,
        restore_to_new_service: false,
        pitr: false,
        earliest_pitr_time: None,
        latest_pitr_time: None,
    }
}

/// Refuse what a restore by a worker's agent cannot do: anything but in
/// place, or a backup in another format than the worker backup writes.
fn agent_restore_supported(
    target: &temps_entities::external_services::Model,
    backup_location: &str,
    mode: &RestoreRequestMode,
) -> Result<(), RestoreError> {
    let node = crate::engines::workload_job::worker_node(target).unwrap_or_default();
    if !matches!(mode, RestoreRequestMode::InPlace) {
        return Err(RestoreError::Validation {
            message: format!(
                "Service {} ('{}') runs on worker node {node}, where restores run on the node and \
                 apply a backup in place; restore mode '{}' is not available there yet. Restore \
                 in place",
                target.id,
                target.name,
                mode.as_str()
            ),
        });
    }
    if !crate::engines::worker_agent::restore_location_matches(target, backup_location) {
        return Err(RestoreError::Validation {
            message: format!(
                "Service {} ('{}', {}) runs on worker node {node}, where restores apply the \
                 backups taken on the node; backup '{backup_location}' is in another format",
                target.id, target.name, target.service_type
            ),
        });
    }
    Ok(())
}

/// What a worker engine's restore takes from the restore service: the
/// database, the encryption service (the restored login is patched into
/// the target's stored configuration), the credential scoper (ADR-093) and
/// the manager that reaches the node's agent.
pub(super) struct WorkerRestoreDeps<'a> {
    pub db: &'a Arc<DatabaseConnection>,
    pub enc: &'a Arc<temps_core::EncryptionService>,
    pub scoper: &'a temps_providers::worker_s3_scope::WorkerCredentialScoper,
    pub mgr: &'a Arc<ExternalServiceManager>,
}

/// The S3 source a restore on worker `node_id` receives (ADR-093): a
/// read-only credential for `backup`'s own location (its WAL-G repository,
/// or its object's directory) and for `extra_prefixes` (a point-in-time
/// recovery's binary logs), never the source's own. A location outside the
/// source's bucket, with no directory of its own, or whose grant would span
/// more than one service's backups
/// ([`within_one_service`](crate::engines::worker_agent::within_one_service)),
/// is refused.
pub(super) async fn worker_read_destination(
    db: &Arc<DatabaseConnection>,
    scoper: &temps_providers::worker_s3_scope::WorkerCredentialScoper,
    run_id: i32,
    backup: &temps_entities::backups::Model,
    target_id: i32,
    node_id: i32,
    extra_prefixes: &[String],
) -> Result<temps_providers::remote_service_client::WorkerS3Destination, RestoreError> {
    use crate::engines::worker_agent;
    let s3_source = crate::engines::v2_common::load_s3_source(db.as_ref(), backup.s3_source_id)
        .await
        .map_err(|e| RestoreError::ExternalService {
            reason: format!("restore run {run_id}: {e}"),
        })?;
    let Some(read_prefix) =
        worker_agent::restore_read_prefix(&backup.s3_location, &s3_source.bucket_name)
    else {
        return Err(RestoreError::Validation {
            message: format!(
                "Backup {} is at '{}', which is not a backup location inside bucket '{}' of S3 \
                 source {}",
                backup.id, backup.s3_location, s3_source.bucket_name, backup.s3_source_id
            ),
        });
    };
    // However the row's location reads, the grant spans one service's
    // backups at most.
    if let Some(wide) = std::iter::once(&read_prefix)
        .chain(extra_prefixes)
        .find(|prefix| !worker_agent::within_one_service(prefix, &s3_source.bucket_path))
    {
        return Err(RestoreError::Validation {
            message: format!(
                "Backup {} at '{}' would need read access to '{wide}' in S3 source {}, which is \
                 not inside one service's backups under '{}'; a restore on worker node \
                 {node_id} is refused rather than given it",
                backup.id, backup.s3_location, backup.s3_source_id, s3_source.bucket_path
            ),
        });
    }
    let mut grant = temps_providers::worker_s3_scope::WorkerS3Grant::job(
        target_id,
        node_id,
        read_prefix,
        temps_core::ScopedS3Access::Read,
        "restore",
    );
    grant.prefixes.extend(extra_prefixes.iter().cloned());
    worker_agent::scoped_destination(scoper, db.as_ref(), backup.s3_source_id, grant, "restore")
        .await
        .map_err(|e| RestoreError::ExternalService {
            reason: format!("restore run {run_id}: {e}"),
        })
}

/// Restore `backup` in place into `target`, a MongoDB, Redis, MariaDB or
/// storage service on a worker node, through the node's agent, which runs the
/// engine's own restore there.
#[allow(
    clippy::too_many_arguments,
    reason = "the restore task's dependencies and the run's inputs"
)]
async fn run_agent_restore(
    db: &Arc<DatabaseConnection>,
    enc: &Arc<temps_core::EncryptionService>,
    scoper: &temps_providers::worker_s3_scope::WorkerCredentialScoper,
    mgr: &Arc<ExternalServiceManager>,
    run_id: i32,
    backup: &temps_entities::backups::Model,
    target: &temps_entities::external_services::Model,
    mode: &RestoreRequestMode,
) -> Result<Option<i32>, RestoreError> {
    use crate::engines::{worker_agent, workload_job};
    agent_restore_supported(target, &backup.s3_location, mode)?;
    let node_id = workload_job::worker_node(target).ok_or_else(|| RestoreError::Internal {
        reason: format!(
            "restore run {run_id}: service {} left its worker node",
            target.id
        ),
    })?;
    // A backup of another service is admitted as on the control plane (the
    // destructive restore was confirmed at `start_restore`); a dump that
    // carries its origin's users (MongoDB) leaves the target with the
    // origin's credentials, which its stored config is patched to below.
    let origin_credentials = origin_credentials_to_reconcile(db, mgr, backup, target).await;
    let s3_source = crate::engines::v2_common::load_s3_source(db.as_ref(), backup.s3_source_id)
        .await
        .map_err(|e| RestoreError::ExternalService {
            reason: format!("restore run {run_id}: {e}"),
        })?;
    let bucket_prefix = format!("s3://{}/", s3_source.bucket_name);
    let location = backup
        .s3_location
        .strip_prefix(&bucket_prefix)
        .unwrap_or(&backup.s3_location)
        .trim_start_matches('/')
        .to_string();
    // The worker can read this one backup and nothing else (ADR-093).
    let read_prefix = match worker_agent::worker_service_kind(&target.service_type) {
        Some(worker_agent::WorkerServiceKind::Storage) => {
            temps_providers::worker_s3_scope::as_prefix(&location)
        }
        _ => temps_providers::worker_s3_scope::key_directory(&location),
    };
    let mint = || async {
        worker_agent::scoped_destination(
            scoper,
            db.as_ref(),
            backup.s3_source_id,
            temps_providers::worker_s3_scope::WorkerS3Grant::job(
                target.id,
                node_id,
                read_prefix.clone(),
                temps_core::ScopedS3Access::Read,
                "restore",
            ),
            "restore",
        )
        .await
        .map_err(|e| RestoreError::ExternalService {
            reason: format!("restore run {run_id}: {e}"),
        })
    };
    let destination = mint().await?;

    // The last safe point: once the node starts writing, the run can no
    // longer be cancelled.
    super::restore_cancel::enter_phase_unless_cancelled(db.as_ref(), run_id, "restore").await?;
    info!(
        run_id,
        service_id = target.id,
        node_id,
        backup_id = backup.id,
        "restoring service {} in place on its node",
        target.id
    );
    // A node already running its maximum of backups and restores is waited
    // for, visibly and cancellably, instead of failing the run.
    // A try after a wait gets a credential minted after it.
    crate::node_busy::retry_while_node_busy_with_fresh_credential(
        &crate::node_busy::NodeBusyPolicy::DEFAULT,
        destination,
        mint,
        |destination| mgr.restore_worker_service(target.id, destination, location.clone()),
        |wait| wait_for_node_slot(db.as_ref(), run_id, "restore", wait),
    )
    .await?
    .map_err(|e| RestoreError::ExternalService {
        reason: format!("restore run {run_id} on node {node_id}: {e}"),
    })?;
    reconcile_restored_credentials(db, enc, target.id, origin_credentials.as_ref()).await;
    if let Err(e) = update_phase(db.as_ref(), run_id, "verify").await {
        warn!("Restore run {run_id}: recording the verify phase failed: {e}");
    }
    Ok(None)
}

/// Restore `backup` in place into `target`, a database on a worker node, by
/// a job on that node (ADR-055 Phase 9): `wal-g st cat | gunzip | psql`.
#[allow(
    clippy::too_many_arguments,
    reason = "the restore worker's shared handles and the run's own inputs, as run_restore_worker"
)]
async fn run_job_restore(
    db: &Arc<DatabaseConnection>,
    enc: &Arc<temps_core::EncryptionService>,
    scoper: &temps_providers::worker_s3_scope::WorkerCredentialScoper,
    mgr: &Arc<ExternalServiceManager>,
    plane: &temps_workload_plane::PlaneSlot,
    run_id: i32,
    backup: &temps_entities::backups::Model,
    target: &temps_entities::external_services::Model,
    mode: &RestoreRequestMode,
) -> Result<Option<i32>, RestoreError> {
    use crate::engines::workload_job;
    job_restore_supported(target, &backup.s3_location, mode)?;
    let node_id = workload_job::worker_node(target).ok_or_else(|| RestoreError::Internal {
        reason: format!(
            "restore run {run_id}: database {} left its worker node",
            target.id
        ),
    })?;
    // A backup of another database is admitted as on the control plane (the
    // destructive restore was confirmed at `start_restore`). Its dump carries
    // the origin's roles and their passwords; see below.
    let origin_credentials = origin_credentials_to_reconcile(db, mgr, backup, target).await;
    let plane = plane.get().ok_or_else(|| RestoreError::Internal {
        reason: format!(
            "restore run {run_id}: database {} is on worker node {node_id}, and restoring it \
             needs the workload plane, which this process does not run",
            target.id
        ),
    })?;
    let config_json = enc
        .decrypt_string(target.config.as_deref().unwrap_or("{}"))
        .map_err(|e| RestoreError::Encryption {
            reason: format!("decrypting the config of database {}: {e}", target.id),
        })?;
    let parameters: serde_json::Value =
        serde_json::from_str(&config_json).unwrap_or_else(|_| serde_json::json!({}));
    let (job_target, password) = workload_job::pg_target(target, node_id, &parameters);
    // The dump runs `ALTER ROLE` for every role it holds, with the password
    // each had when the backup was taken. The job keeps its login on the
    // target's own password through it (`restore_script`), so the target's
    // stored password stays the one that works, and only the database the
    // restored data lives in is reconciled below.
    let restored_database = origin_credentials.and_then(|origin| origin.into_database_only());
    let s3_source = crate::engines::v2_common::load_s3_source(db.as_ref(), backup.s3_source_id)
        .await
        .map_err(|e| RestoreError::ExternalService {
            reason: format!("restore run {run_id}: {e}"),
        })?;

    // `wal-g st cat` reads keys relative to the bucket (`WALG_S3_PREFIX`).
    let bucket_prefix = format!("s3://{}/", s3_source.bucket_name);
    let object_key = backup
        .s3_location
        .strip_prefix(&bucket_prefix)
        .unwrap_or(&backup.s3_location)
        .trim_start_matches('/')
        .to_string();
    if object_key.starts_with("s3://") {
        return Err(RestoreError::Validation {
            message: format!(
                "Backup {} is at '{}', outside bucket '{}' of S3 source {}",
                backup.id, backup.s3_location, s3_source.bucket_name, backup.s3_source_id
            ),
        });
    }
    // The job can read this one dump's directory and nothing else.
    let s3 = workload_job::s3_target(
        scoper,
        db.as_ref(),
        backup.s3_source_id,
        temps_providers::worker_s3_scope::WorkerS3Grant::job(
            target.id,
            node_id,
            temps_providers::worker_s3_scope::key_directory(&object_key),
            temps_core::ScopedS3Access::Read,
            "restore",
        ),
        "restore",
    )
    .await
    .map_err(|e| RestoreError::ExternalService {
        reason: format!("restore run {run_id}: {e}"),
    })?;

    // The last safe point: once the job starts writing, the run can no
    // longer be cancelled (`restore` is not a cancellable phase).
    super::restore_cancel::enter_phase_unless_cancelled(db.as_ref(), run_id, "restore").await?;
    info!(
        run_id,
        service_id = target.id,
        node_id,
        backup_id = backup.id,
        "restoring database {} in place by a job on its node",
        target.id
    );
    workload_job::run_database_job(
        &plane,
        &job_target,
        workload_job::JobPurpose::Restore {
            restore_run_id: run_id,
        },
        workload_job::restore_script(&job_target, &object_key),
        workload_job::restore_env(&password, &s3),
        // Past the safe point: nothing may interrupt the apply.
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .map_err(|e| RestoreError::ExternalService {
        reason: format!("restore run {run_id}: {e}"),
    })?;
    reconcile_restored_credentials(db, enc, target.id, restored_database.as_ref()).await;
    if let Err(e) = update_phase(db.as_ref(), run_id, "verify").await {
        warn!("Restore run {run_id}: recording the verify phase failed: {e}");
    }
    Ok(None)
}

/// The wait hook of a restore run whose worker node is busy
/// ([`crate::node_busy`]): the run shows `waiting_for_node` (cancellable,
/// nothing ran), checked every few seconds for a cancel while it sleeps, then
/// re-enters `resume_phase` unless it was cancelled.
pub(super) async fn wait_for_node_slot(
    db: &DatabaseConnection,
    run_id: i32,
    resume_phase: &str,
    wait: crate::node_busy::NodeWait,
) -> Result<(), RestoreError> {
    use super::restore_cancel::{enter_phase_unless_cancelled, WAITING_FOR_NODE_PHASE};
    const CANCEL_CHECK: std::time::Duration = std::time::Duration::from_secs(5);
    warn!(
        run_id,
        "restore run {run_id} waits for a slot: {}",
        wait.describe()
    );
    let deadline = tokio::time::Instant::now() + wait.delay;
    loop {
        enter_phase_unless_cancelled(db, run_id, WAITING_FOR_NODE_PHASE).await?;
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        tokio::time::sleep(CANCEL_CHECK.min(deadline - now)).await;
    }
    enter_phase_unless_cancelled(db, run_id, resume_phase).await
}

async fn update_phase(
    db: &DatabaseConnection,
    run_id: i32,
    phase: &str,
) -> Result<(), RestoreError> {
    let mut active: temps_entities::restore_runs::ActiveModel =
        load_run_for_update(db, run_id).await?.into();
    active.phase = Set(phase.to_string());
    active.update(db).await?;
    Ok(())
}

fn build_s3_client(creds: &S3Credentials) -> S3Client {
    let aws_creds = aws_sdk_s3::config::Credentials::new(
        creds.access_key_id.clone(),
        creds.secret_key.clone(),
        // `None` for the long-lived credentials operators configure; `Some`
        // only for a temporary one, which SigV4 rejects without its token.
        creds.session_token.clone(),
        None,
        "restore-service",
    );
    let builder = crate::engines::v2_common::s3_config_builder(
        &creds.region,
        creds.force_path_style,
        aws_creds,
        creds.endpoint.as_deref(),
    );
    S3Client::from_conf(builder.build())
}

/// Insert a new `external_services` row for a restored clone. The
/// per-engine `restore_to_new_service` / `restore_pitr` impl is responsible
/// for creating the Docker container + volume; here we only persist the DB
/// row with its runtime parameters (encrypted).
async fn persist_new_service(
    db: &DatabaseConnection,
    enc: &Arc<temps_core::EncryptionService>,
    source: &temps_entities::external_services::Model,
    run_id: i32,
    new_name: String,
    result: &temps_providers::externalsvc::NewServiceRestoreResult,
) -> Result<i32, RestoreError> {
    // Serialize the parameters HashMap as JSON and encrypt to match the
    // storage format used by ExternalServiceManager::create_service.
    let params_json =
        serde_json::to_string(&result.parameters).map_err(|e| RestoreError::Internal {
            reason: format!("Failed to serialize new service parameters: {}", e),
        })?;
    let encrypted = enc
        .encrypt_string(&params_json)
        .map_err(|e| RestoreError::Encryption {
            reason: format!("Failed to encrypt new service config: {}", e),
        })?;

    let slug = slugify(&new_name);

    let model = temps_entities::external_services::ActiveModel {
        name: Set(new_name.clone()),
        slug: Set(Some(slug)),
        service_type: Set(source.service_type.clone()),
        version: Set(source.version.clone()),
        status: Set("running".to_string()),
        config: Set(Some(encrypted)),
        node_id: Set(source.node_id),
        topology: Set(source.topology.clone()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        error_message: Set(None),
        ..Default::default()
    };

    let txn = db.begin().await?;
    reserve_restore_service_name(&txn, &new_name, Some(run_id)).await?;
    let run = temps_entities::restore_runs::Entity::find_by_id(run_id)
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or(RestoreError::RestoreRunNotFound {
            restore_run_id: run_id,
        })?;
    if run.target_service_name.as_deref() != Some(new_name.as_str())
        || run.source_service_id != source.id
        || !ACTIVE_RESTORE_STATUSES.contains(&run.status.as_str())
        || run.target_service_id.is_some()
        || run.cancel_requested_at.is_some()
    {
        return Err(RestoreError::ServiceNameConflict {
            name: new_name,
            reason: format!("restore run {run_id} no longer owns this name; its provisioned resources were left unchanged for inspection"),
        });
    }
    let inserted = model.insert(&txn).await?;
    let mut run: temps_entities::restore_runs::ActiveModel = run.into();
    run.target_service_id = Set(Some(inserted.id));
    run.update(&txn).await?;
    txn.commit().await?;
    info!(
        "Persisted restored service '{}' (id={}) from source '{}' (id={})",
        inserted.name, inserted.id, source.name, source.id
    );
    Ok(inserted.id)
}

fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_dash = false;
    for c in input.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// Engine-specific container-name prefix. Mirrors each engine's
/// `get_container_name()` implementation. Falls back to the service name
/// verbatim for engines we don't recognize so the plan still renders.
fn engine_container_name(engine_lower: &str, service_name: &str) -> String {
    match engine_lower {
        "postgres" => format!("postgres-{}", service_name),
        "redis" => format!("redis-{}", service_name),
        // Matches MongodbService::get_container_name().
        "mongodb" => format!("temps-mongodb-{}", service_name),
        // Matches MariaDbService::get_container_name() and the dispatch-side
        // PITR-tools probe in engines/dispatch.rs, both `mariadb-{name}`.
        "mariadb" => format!("mariadb-{}", service_name),
        "s3" => format!("rustfs-{}", service_name),
        "rustfs" => format!("rustfs-{}", service_name),
        "blob" | "kv" | "minio" => format!("{}-{}", engine_lower, service_name),
        _ => service_name.to_string(),
    }
}

/// Postgres restore plan. Same content as the original hard-coded branch;
/// moved here so the engine dispatcher stays readable.
fn build_postgres_steps(
    strategy: &str,
    mode: &RestoreRequestMode,
    container_name: &str,
    resolved_location: &str,
    steps: &mut Vec<String>,
    destructive: &mut bool,
    errors: &mut Vec<String>,
) {
    match strategy {
        "walg_restore" => match mode {
            RestoreRequestMode::InPlace => {
                *destructive = true;
                steps.push(format!(
                    "Fetch WAL-G base backup into {}'s shared volume (wal-g backup-fetch LATEST)",
                    container_name
                ));
                steps.push(
                    "Write /var/lib/postgresql/walg-restore.env read-only credentials onto the container"
                        .into(),
                );
                steps.push(
                    "Overwrite postgresql.auto.conf (recovery.signal, restore_command='wal-g wal-fetch', recovery_target='immediate', recovery_target_action='promote', archive_mode='off', archive_command='/bin/true')"
                        .into(),
                );
                steps.push(format!("Stop {}", container_name));
                steps.push("Swap PGDATA on the volume using an ephemeral helper container".into());
                steps.push(format!(
                    "Start {}. PostgreSQL enters recovery, replays WAL from S3 until it reaches consistency, then promotes to primary.",
                    container_name
                ));
                steps.push("Wait for container to report healthy".into());
            }
            RestoreRequestMode::NewService { name, .. } => {
                steps.push(format!(
                    "Allocate a new Postgres container 'postgres-{}' on a fresh volume",
                    name
                ));
                steps.push(
                    "Pull image and start the new container (same image + credentials as target)"
                        .into(),
                );
                steps.push(
                    "Run the WAL-G restore sequence against the new container (fetch base backup, write walg-restore.env, write recovery.signal, swap PGDATA, promote)"
                        .into(),
                );
                steps.push("Persist the new service in the database".into());
            }
            RestoreRequestMode::Pitr {
                to_new_service,
                new_service_name,
                target,
            } => {
                *destructive = !*to_new_service;
                if *to_new_service {
                    steps.push(format!(
                        "Provision new service 'postgres-{}' just like the new_service flow",
                        new_service_name.as_deref().unwrap_or("<unnamed>")
                    ));
                }
                steps.push(
                    "Fetch WAL-G base backup, write walg-restore.env, write recovery.signal".into(),
                );
                let target_desc = match target {
                    RecoveryTarget::Time { .. } => "timestamp",
                    RecoveryTarget::Xid { .. } => "xid",
                    RecoveryTarget::Lsn { .. } => "lsn",
                    RecoveryTarget::Name { .. } => "restore point name",
                };
                steps.push(format!("Set recovery target (type: {})", target_desc));
                steps.push(
                    "Start container. PostgreSQL replays WAL from S3 up to the target, then promotes."
                        .into(),
                );
            }
        },
        "pg_dump_restore" => match mode {
            RestoreRequestMode::InPlace => {
                *destructive = true;
                steps.push(format!(
                    "Download {} from S3 to a temp file on the host",
                    resolved_location
                ));
                steps.push(
                    "Decompress (gzip) and pipe into pg_restore via a sidecar container".into(),
                );
                steps.push(format!(
                    "pg_restore --clean --if-exists against {}'s database",
                    container_name
                ));
                steps.push("Clean up temp files".into());
            }
            RestoreRequestMode::NewService { name, .. } => {
                steps.push(format!(
                    "Provision new Postgres service 'postgres-{}'",
                    name
                ));
                steps.push("Download + pg_restore into the new service's database".into());
            }
            RestoreRequestMode::Pitr { .. } => {
                steps.push(
                    "PITR is not possible with a pg_dump backup — no WAL archive exists.".into(),
                );
            }
        },
        _ => {
            errors.push(
                "Backup format could not be classified as either WAL-G or pg_dump. Restore is not supported."
                    .into(),
            );
        }
    }
}

/// MariaDB restore plan.
///
/// Structurally the closest analog to Postgres: the PITR path is OFFLINE
/// (stop container → replace the datadir → start → forward-roll), not an
/// online stream like Mongo's. The two formats:
///
/// - `mariadb_physical_restore` — a gzipped `mariadb-backup --stream=mbstream`
///   base (`base.mbstream.gz`). Restore is the documented prepare/copy-back
///   dance run inside an ephemeral helper container that shares the service's
///   data volume (`volumes_from`), because the datadir must be replaced while
///   the server is down. PITR then replays archived binary logs on top.
/// - `mariadb_dump_restore` — a gzipped `mariadb-dump` of the user databases
///   (`dump.sql.gz`), applied by feeding it to the `mariadb` client inside the
///   running container. No binlog anchor, so no PITR.
///
/// **Credential side-effect (physical only):** `--copy-back` replaces the whole
/// datadir including the `mysql` system schema, so `mysql.user` — the
/// password-hash table — becomes the source's. The logical dump deliberately
/// skips the `mysql` schema and therefore leaves the target's credentials
/// alone.
fn build_mariadb_steps(
    strategy: &str,
    mode: &RestoreRequestMode,
    container_name: &str,
    resolved_location: &str,
    steps: &mut Vec<String>,
    destructive: &mut bool,
    errors: &mut Vec<String>,
) {
    // The physical restore sequence, shared by in-place, new-service, and the
    // base half of PITR. Kept in one place so the three previews can't drift
    // from each other (they all run `physical_restore_into_container`).
    let walg = temps_providers::externalsvc::mariadb::MariaDbService::is_walg_repository_location(
        resolved_location,
    );
    let physical_sequence = |target: &str, steps: &mut Vec<String>| {
        if walg {
            steps.push(format!(
                "Disable {target}'s restart policy and stop it so the datadir volume is free"
            ));
            steps.push(format!(
                "A helper sharing {target}'s volume runs `wal-g backup-fetch` from {resolved_location} into a staging directory on the volume and prepares it with `mariadb-backup --prepare`; nothing in the live datadir is touched yet"
            ));
            steps.push(
                "Swap the prepared base in, keeping the original datadir aside on the same volume"
                    .into(),
            );
            steps.push(format!(
                "Start {target} on the restored datadir, give it back its restart policy, and wait until it answers"
            ));
            steps.push(format!(
                "On success delete the kept original; if anything after the swap fails, put the original back and start {target} on it as it was"
            ));
            return;
        }
        steps.push(format!(
            "Download {} from S3 and gunzip it to a raw mbstream on the host",
            resolved_location
        ));
        steps.push(format!(
            "Disable {}'s restart policy and stop it so the datadir volume is free",
            target
        ));
        steps.push(format!(
            "Create an ephemeral helper container sharing {}'s volumes, upload the mbstream onto it, and start it",
            target
        ));
        steps.push(
            "Helper: `mbstream -x` into a staging dir, `mariadb-backup --prepare` (apply redo logs), wipe /var/lib/mysql, `mariadb-backup --copy-back`, chown to mysql"
                .into(),
        );
        steps.push("Remove the helper and re-enable the restart policy".into());
        steps.push(format!(
            "Start {} on the restored datadir and wait for it to report healthy",
            target
        ));
    };

    match strategy {
        "mariadb_physical_restore" => match mode {
            RestoreRequestMode::InPlace => {
                *destructive = true;
                physical_sequence(container_name, steps);
                steps.push(format!(
                    "The restored datadir carries the SOURCE's `mysql` system schema, so {} now authenticates with the source's root/user passwords",
                    container_name
                ));
                steps.push(
                    "Orchestrator patches the target service's stored config with the source's password so UI/env/CLI still work"
                        .into(),
                );
            }
            RestoreRequestMode::NewService { name, .. } => {
                steps.push(format!(
                    "Allocate a new MariaDB container 'mariadb-{}' on a fresh volume and port (image + credentials cloned from the target)",
                    name
                ));
                physical_sequence(&format!("mariadb-{}", name), steps);
                steps.push(
                    "Because the copy-back replays the source's `mysql` schema, the new service's accounts are the SOURCE's, not the freshly-generated ones"
                        .into(),
                );
                steps.push("Persist the new service in the database".into());
            }
            RestoreRequestMode::Pitr {
                to_new_service,
                new_service_name,
                target,
            } => {
                *destructive = !*to_new_service;
                let target_container = if *to_new_service {
                    let name = new_service_name.as_deref().unwrap_or("<unnamed>");
                    steps.push(format!(
                        "Provision new service 'mariadb-{}' just like the new_service flow",
                        name
                    ));
                    format!("mariadb-{}", name)
                } else {
                    container_name.to_string()
                };
                steps.push(
                    "Read the base's metadata.json companion for its binlog coordinates (binlog_file / binlog_position); reject the restore if the base was taken without binary logging"
                        .into(),
                );
                physical_sequence(&target_container, steps);
                steps.push(
                    "Read the binlog manifest.json under the SOURCE service's binlog/ prefix and download + gunzip every archived segment at or after the base's binlog_file"
                        .into(),
                );
                match target {
                    RecoveryTarget::Time { .. } => {
                        steps.push(
                            "Set the recovery target (type: timestamp) as `mariadb-binlog --stop-datetime`"
                                .into(),
                        );
                    }
                    RecoveryTarget::Lsn { .. } => {
                        steps.push(
                            "Set the recovery target (type: lsn, given as `binlog_file:position`) as `mariadb-binlog --stop-position`. The named file must be the LAST segment replayed, otherwise the position is ambiguous across segments and the restore is rejected."
                                .into(),
                        );
                    }
                    // `recovery_target_to_stop_flag` rejects both of these
                    // before touching any data, so the plan must surface them
                    // as errors rather than describing a step that can't run.
                    RecoveryTarget::Xid { .. } => {
                        errors.push(
                            "MariaDB PITR does not support an xid/GTID recovery target yet — use a timestamp, or an lsn given as `binlog_file:position`."
                                .into(),
                        );
                    }
                    RecoveryTarget::Name { .. } => {
                        errors.push(
                            "MariaDB has no named-restore-point equivalent — use a timestamp recovery target, or an lsn given as `binlog_file:position`."
                                .into(),
                        );
                    }
                }
                steps.push(format!(
                    "Upload the segments into {} and replay them in ONE `mariadb-binlog --disable-log-bin --start-position=<base position>` pass piped into the mariadb client, stopping at the target",
                    target_container
                ));
                steps.push(format!(
                    "Clean up the uploaded segments; {} is left running at the recovered point",
                    target_container
                ));
            }
        },
        "mariadb_dump_restore" => match mode {
            RestoreRequestMode::InPlace => {
                *destructive = true;
                steps.push(format!(
                    "Download {} from S3 and gunzip it to a .sql file on the host",
                    resolved_location
                ));
                steps.push(format!(
                    "Upload the .sql into the RUNNING {} at /tmp (the container is not stopped)",
                    container_name
                ));
                steps.push(
                    "Feed it to the `mariadb` client as root (`mariadb -uroot < /tmp/...`); the dump's `DROP TABLE`/`CREATE` statements replace the dumped databases".into(),
                );
                steps.push(
                    "The dump excludes the `mysql` system schema, so the target keeps its own accounts and passwords".into(),
                );
                steps.push("Remove the uploaded .sql from the container".into());
            }
            RestoreRequestMode::NewService { name, .. } => {
                steps.push(format!(
                    "Allocate a new MariaDB container 'mariadb-{}' on a fresh volume and port",
                    name
                ));
                steps.push(
                    "Download + gunzip the dump and apply it via the new container's mariadb client"
                        .into(),
                );
                steps.push(
                    "The new service keeps its freshly-generated credentials (the dump carries no `mysql` schema)"
                        .into(),
                );
                steps.push("Persist the new service in the database".into());
            }
            RestoreRequestMode::Pitr { .. } => {
                errors.push(
                    "PITR is not possible with a mariadb_dump backup — a logical dump records no binlog anchor to replay from. Choose a physical (mariadb-backup) base backup."
                        .into(),
                );
            }
        },
        _ => {
            errors.push(
                "Backup format could not be classified as either a physical mariadb-backup base (base.mbstream.gz) or a logical mariadb-dump (.sql.gz). Restore is not supported."
                    .into(),
            );
        }
    }
}

/// The in-place swap every Redis restore ends with, here or on a worker
/// (`restore_swap` in the Redis provider).
fn push_redis_swap_steps(container_name: &str, steps: &mut Vec<String>) {
    steps.push(
        "Move the current dump.rdb and appendonlydir aside on the same volume, move the staged RDB in, and write the Redis 7+ appendonlydir manifest that loads it as the base file"
            .into(),
    );
    steps.push(format!(
        "Start {} with its own restart policy and wait for it to answer. Redis keeps its EXISTING requirepass (auth is not part of the dump).",
        container_name
    ));
    steps.push(
        "Delete the kept original once the restored server answers; if any step fails, the original is moved back and started as it was"
            .into(),
    );
}

/// The step a Redis restore of another service's backup adds: Temps' own
/// `_temps:*` keys stay the target's.
fn redis_bookkeeping_step() -> String {
    "Keep the target's own Temps bookkeeping (`_temps:*` keys in database 0: database \
     mappings, owners, import claims) instead of the backup's: they are read before the \
     restore and written back after it"
        .into()
}

/// Redis restore plan. RDB dumps via WAL-G stream, or legacy tar. Redis
/// auth (`requirepass`) is NOT inside the data, so the container's
/// existing password is preserved across the restore — no credential
/// propagation needed.
fn build_redis_steps(
    strategy: &str,
    mode: &RestoreRequestMode,
    container_name: &str,
    resolved_location: &str,
    steps: &mut Vec<String>,
    destructive: &mut bool,
    errors: &mut Vec<String>,
) {
    match strategy {
        "walg_restore" => match mode {
            RestoreRequestMode::InPlace => {
                *destructive = true;
                steps.push(format!(
                    "Stop {} (Redis releases its volume lock)",
                    container_name
                ));
                steps.push(format!(
                    "Run `wal-g backup-fetch LATEST` inside a helper container to stage the RDB base file beside {}'s data; nothing is replaced until it is whole",
                    container_name
                ));
                push_redis_swap_steps(container_name, steps);
            }
            RestoreRequestMode::NewService { name, .. } => {
                steps.push(format!(
                    "Allocate a new Redis container 'redis-{}' on a fresh volume",
                    name
                ));
                steps.push("Run the same WAL-G fetch sequence against the new container".into());
                steps.push(
                    "New service is created with a fresh requirepass; restored data is loaded with that new password active"
                        .into(),
                );
                steps.push("Persist the new service in the database".into());
            }
            RestoreRequestMode::Pitr { .. } => {
                errors.push(
                    "Redis does not support point-in-time recovery — RDB snapshots are discrete; pick a specific backup instead."
                        .into(),
                );
            }
        },
        "redis_rdb_restore" => match mode {
            RestoreRequestMode::InPlace => {
                *destructive = true;
                steps.push(format!(
                    "Download and decompress {} into a Redis RDB snapshot",
                    resolved_location
                ));
                steps.push(format!(
                    "Stop {} and stage the RDB beside its data; nothing is replaced until it is whole",
                    container_name
                ));
                push_redis_swap_steps(container_name, steps);
            }
            RestoreRequestMode::NewService { name, .. } => {
                steps.push(format!(
                    "Allocate a new Redis container 'redis-{}' on a fresh volume",
                    name
                ));
                steps.push(format!(
                    "Download and decompress {} into the new service's data volume",
                    resolved_location
                ));
                steps
                    .push("Rebuild the Redis 7+ appendonlydir manifest and start the clone".into());
                steps
                    .push("Persist the restored Redis service after its healthcheck passes".into());
            }
            RestoreRequestMode::Pitr { .. } => {
                errors.push(
                    "Redis does not support point-in-time recovery — RDB snapshots are discrete; pick a specific backup instead."
                        .into(),
                );
            }
        },
        _ => {
            errors.push(format!(
                "Redis backup location '{}' is neither a WAL-G prefix nor a current .rdb.gz object.",
                resolved_location
            ));
        }
    }
}

/// MongoDB restore plan.
///
/// Temps stores Mongo backups as `mongodump --archive` streams wrapped in
/// a WAL-G envelope. Restore runs `wal-g backup-fetch LATEST` inside the
/// target container; WAL-G pipes the decompressed stream to
/// `mongorestore --archive --drop` via `WALG_STREAM_RESTORE_COMMAND`.
///
/// **Credential side-effect:** `mongorestore --drop` does NOT exclude the
/// `admin` database by default, so `admin.system.users` from the source
/// ends up in the target. The root user's password hash becomes the
/// source's. The orchestrator patches the target's stored config
/// afterwards so the Temps UI/env vars/CLI reflect the password that
/// actually works.
fn build_mongodb_steps(
    strategy: &str,
    mode: &RestoreRequestMode,
    container_name: &str,
    resolved_location: &str,
    steps: &mut Vec<String>,
    destructive: &mut bool,
    errors: &mut Vec<String>,
) -> Option<&'static str> {
    // A WAL-G backup is fetched from WALG_S3_PREFIX, not a single key; a
    // mongodump archive is one object, downloaded and replayed by a sidecar.
    let archive = strategy == "mongodump_restore";
    let fetch = |into: &str| {
        if archive {
            format!(
                "Download the mongodump archive {resolved_location} and run `mongorestore \
                 --archive --gzip --drop` against {into} from a one-shot sidecar"
            )
        } else {
            format!(
                "Run `wal-g backup-fetch LATEST` inside {into}; WAL-G decompresses the archived \
                 mongodump stream and pipes it to `mongorestore --archive --drop`"
            )
        }
    };
    match mode {
        RestoreRequestMode::InPlace => {
            *destructive = true;
            steps.push(fetch(container_name));
            steps.push(format!(
                "mongorestore replaces ALL databases on {}, including `admin.system.users` — target's root user takes on the source's password",
                container_name
            ));
            steps.push(
                "Orchestrator patches the target service's stored config with the source's password so UI/env/CLI still work"
                    .into(),
            );
        }
        RestoreRequestMode::NewService { name, .. } => {
            steps.push(format!(
                "Allocate a new MongoDB container 'temps-mongodb-{}' on a fresh volume",
                name
            ));
            steps.push("Start the new container and wait for mongod to initialize".into());
            steps.push(fetch("the new container"));
            steps.push(
                "Because mongorestore replays `admin.system.users`, the new service's root user is the SOURCE's credentials, not the freshly-generated ones"
                    .into(),
            );
            steps.push(
                "Persist the new service; verify its displayed credentials against the restored users before linking applications".into(),
            );
        }
        RestoreRequestMode::Pitr { .. } => {
            errors.push(
                "MongoDB point-in-time recovery is not yet supported by this orchestrator.".into(),
            );
        }
    }
    if matches!(mode, RestoreRequestMode::NewService { .. })
        && matches!(strategy, "walg_restore" | "mongodump_restore")
    {
        Some("MongoDB clone credentials: the archive replaces admin users with those from the backup. The new service's displayed password may differ from the restored password; verify the backup's credentials or reset the database password before linking applications. This clone path does not automatically reconcile stored credentials.")
    } else {
        None
    }
}

/// S3-compatible (RustFS / MinIO / Blob / KV) restore plan. Data is
/// objects in a bucket; no auth inside the data.
fn build_object_store_steps(
    strategy: &str,
    mode: &RestoreRequestMode,
    target_name: &str,
    resolved_location: &str,
    steps: &mut Vec<String>,
    destructive: &mut bool,
    errors: &mut Vec<String>,
) {
    match mode {
        RestoreRequestMode::InPlace => {
            *destructive = true;
            steps.push(format!("Enumerate objects under {}", resolved_location));
            steps.push(format!(
                "Mirror them into '{}' (mc mirror --remove semantics: adds missing, overwrites changed, deletes extras)",
                target_name
            ));
            steps.push("Verify object count + checksum against the backup index".into());
        }
        RestoreRequestMode::NewService { name, .. } => {
            steps.push(format!(
                "Allocate a new object-store service '{}' with a fresh bucket",
                name
            ));
            steps.push(
                "Mirror backup objects into the new bucket (new credentials, no overwrite risk)"
                    .into(),
            );
            steps.push("Persist the new service in the database".into());
        }
        RestoreRequestMode::Pitr { .. } => {
            errors.push(
                "Object-store point-in-time recovery would require versioned buckets — not yet wired up."
                    .into(),
            );
        }
    }
    let _ = strategy;
}

/// Probe an S3 source for a backup belonging to `<engine>/<origin_service>/`.
/// Prefers a WAL-G prefix (cheap continuous-archive restore) and falls
/// back to the newest dump-style object. Returns `Ok(None)` when nothing
/// plausible is found; `Err` only on unexpected S3 failures.
///
/// This is the repair path for `backups` rows whose `s3_location` was
/// never populated (pre-fix backup pipeline). The restore orchestrator
/// calls it right before dispatching to the engine, so a successful
/// repair turns a broken backup row into a restorable one without the
/// user noticing.
async fn resolve_backup_location_from_s3(
    s3_client: &aws_sdk_s3::Client,
    s3_source: &temps_entities::s3_sources::Model,
    engine: &str,
    origin_service_name: &str,
    backup_uuid: &str,
) -> Result<Option<String>, anyhow::Error> {
    let bucket = &s3_source.bucket_name;
    // Mirror the path convention backup_external_service writes to:
    //   external_services/<engine>/<svc>/...
    let path_prefix = s3_source.bucket_path.trim_matches('/');
    let service_prefix = if path_prefix.is_empty() {
        format!("external_services/{}/{}/", engine, origin_service_name)
    } else {
        format!(
            "{}/external_services/{}/{}/",
            path_prefix, engine, origin_service_name
        )
    };

    // 1) WAL-G: look for `<service_prefix>walg/basebackups_005/*_backup_stop_sentinel.json`.
    //    Presence means a complete WAL-G backup exists; the restore path
    //    is the `walg/` prefix, not the sentinel itself.
    let walg_prefix = format!("{}walg/", service_prefix);
    let basebackups = format!("{}basebackups_005/", walg_prefix);
    let resp = s3_client
        .list_objects_v2()
        .bucket(bucket)
        .prefix(&basebackups)
        .max_keys(10)
        .send()
        .await?;
    let has_walg = resp.contents().iter().any(|o| {
        o.key()
            .map(|k| k.ends_with("_backup_stop_sentinel.json"))
            .unwrap_or(false)
    });
    if has_walg {
        return Ok(Some(format!(
            "s3://{}/{}",
            bucket,
            walg_prefix.trim_end_matches('/')
        )));
    }

    // 2) pg_dump / rdb / mongodump / mariadb — every non-WAL-G engine writes
    //    its artifact under `<service_prefix>.../<backup_uuid>/<filename>`
    //    (see `v2_common::build_external_service_s3_key`, where `backup_uuid`
    //    is `backups.backup_id`). Scanning the whole service prefix and
    //    picking the newest matching extension is NOT safe here: a service
    //    can carry backups from more than one engine/format (e.g. a MariaDB
    //    physical base and a later logical dump), and "newest of any format"
    //    can silently return a different backup than the one the caller
    //    selected. Require the object's key to contain this exact backup's
    //    own uuid path segment, so the resolver can only ever return the
    //    artifact that this specific backup wrote.
    let uuid_segment = format!("/{}/", backup_uuid);
    let mut best: Option<(String, aws_sdk_s3::primitives::DateTime)> = None;
    let mut continuation: Option<String> = None;
    loop {
        let mut req = s3_client
            .list_objects_v2()
            .bucket(bucket)
            .prefix(&service_prefix);
        if let Some(ct) = continuation.clone() {
            req = req.continuation_token(ct);
        }
        let resp = req.send().await?;
        for obj in resp.contents() {
            let key = match obj.key() {
                Some(k) => k.to_string(),
                None => continue,
            };
            if key.contains("/walg/") {
                continue;
            }
            if !key.contains(&uuid_segment) {
                continue;
            }
            if !(key.ends_with(".sql.gz")
                || key.ends_with(".pgdump.gz")
                || key.ends_with(".rdb.gz")
                || key.ends_with(".bson.gz")
                || key.ends_with(".archive")
                || key.ends_with(".mbstream.gz"))
            {
                continue;
            }
            let lm = match obj.last_modified() {
                Some(d) => d,
                None => continue,
            };
            match &best {
                Some((_, best_lm)) if best_lm.secs() >= lm.secs() => {}
                _ => best = Some((key, *lm)),
            }
        }
        if resp.is_truncated().unwrap_or(false) {
            continuation = resp.next_continuation_token().map(|s| s.to_string());
            if continuation.is_none() {
                break;
            }
        } else {
            break;
        }
    }

    Ok(best.map(|(k, _)| k))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pitr(to_new_service: bool) -> RestoreRequestMode {
        RestoreRequestMode::Pitr {
            to_new_service,
            new_service_name: to_new_service.then(|| "clone".to_string()),
            target: RecoveryTarget::Name {
                name: "before-change".to_string(),
            },
        }
    }

    #[test]
    fn origin_binding_classifies_by_service_id() {
        assert!(!RestoreOriginBinding::new(7, vec![7]).cross_service);
        assert!(!RestoreOriginBinding::new(7, vec![3, 7]).cross_service);
        assert!(RestoreOriginBinding::new(7, vec![3]).cross_service);
        assert!(
            RestoreOriginBinding::new(7, vec![]).cross_service,
            "an unknown origin is never treated as the target itself"
        );
    }

    #[test]
    fn destructive_cross_service_restore_requires_confirmation() {
        let foreign = RestoreOriginBinding::new(7, vec![3]);
        let selector = BackupSelector::Id(42);
        for mode in [RestoreRequestMode::InPlace, pitr(false)] {
            let err = foreign
                .require_confirmation(7, &selector, &mode, false)
                .expect_err("unconfirmed destructive cross-service restore is refused");
            let message = err.to_string();
            assert!(
                matches!(
                    err,
                    RestoreError::CrossServiceRestoreNotConfirmed {
                        target_service_id: 7,
                        ..
                    }
                ),
                "{message}"
            );
            assert!(message.contains("backup 42") && message.contains("service(s) 3"));
            foreign
                .require_confirmation(7, &selector, &mode, true)
                .expect("explicit confirmation allows it");
        }
    }

    #[test]
    fn same_service_and_non_destructive_restores_need_no_confirmation() {
        let own = RestoreOriginBinding::new(7, vec![7]);
        let foreign = RestoreOriginBinding::new(7, vec![3]);
        let selector = BackupSelector::Id(42);
        own.require_confirmation(7, &selector, &RestoreRequestMode::InPlace, false)
            .expect("restoring a service's own backup in place is unchanged");
        let new_service = RestoreRequestMode::NewService {
            name: "clone".to_string(),
            parameter_overrides: serde_json::json!({}),
        };
        foreign
            .require_confirmation(7, &selector, &new_service, false)
            .expect("a new service has no data to overwrite");
        foreign
            .require_confirmation(7, &selector, &pitr(true), false)
            .expect("PITR into a new service has no data to overwrite");
    }

    #[test]
    fn raw_location_restore_in_place_requires_confirmation() {
        let unknown = RestoreOriginBinding::new(7, vec![]);
        let selector = BackupSelector::Location {
            location: "s3://bucket/path".to_string(),
            engine: "postgres".to_string(),
            s3_source_id: 5,
        };
        let err = unknown
            .require_confirmation(7, &selector, &RestoreRequestMode::InPlace, false)
            .expect_err("unknown origin must be confirmed");
        let message = err.to_string();
        assert!(message.contains("S3 source 5") && message.contains("origin service unknown"));
    }

    #[test]
    fn restore_outcome_reports_mode_duration_and_code_only() {
        let ok = restore_outcome_event("pitr", std::time::Duration::from_secs(400), Ok(()));
        assert_eq!(ok.event_type, "restore_succeeded");
        assert_eq!(ok.properties["mode"], "pitr");
        assert_eq!(ok.properties["duration_bucket"], "5-30m");
        assert!(!ok.properties.contains_key("failure_code"));

        let failed = restore_outcome_event(
            "new_service",
            std::time::Duration::from_secs(3),
            Err("download of s3://tenant-bucket/base.tar failed: No space left on device".into()),
        );
        assert_eq!(failed.event_type, "restore_failed");
        assert_eq!(failed.properties["failure_code"], "disk_exhausted");
        let serialized = serde_json::to_string(&failed).unwrap();
        assert!(!serialized.contains("tenant-bucket"));
    }

    #[test]
    fn redis_rdb_clone_plan_uses_current_volume_installer() {
        let mut steps = Vec::new();
        let mut destructive = false;
        let mut errors = Vec::new();

        build_redis_steps(
            "redis_rdb_restore",
            &RestoreRequestMode::NewService {
                name: "cache-clone".to_string(),
                parameter_overrides: serde_json::json!({}),
            },
            "redis-source",
            "external_services/redis/source/test-backup.rdb.gz",
            &mut steps,
            &mut destructive,
            &mut errors,
        );

        assert!(errors.is_empty());
        assert!(!destructive);
        assert!(steps.iter().any(|step| step.contains("redis-cache-clone")));
        assert!(steps.iter().any(|step| step.contains("appendonlydir")));
        assert!(!steps.iter().any(|step| step.contains("legacy tar")));
    }

    #[test]
    fn redis_unknown_backup_plan_is_rejected() {
        let mut steps = Vec::new();
        let mut destructive = false;
        let mut errors = Vec::new();

        build_redis_steps(
            "unsupported",
            &RestoreRequestMode::NewService {
                name: "cache-clone".to_string(),
                parameter_overrides: serde_json::json!({}),
            },
            "redis-source",
            "legacy-backup.tar",
            &mut steps,
            &mut destructive,
            &mut errors,
        );

        assert!(steps.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("neither a WAL-G prefix nor a current .rdb.gz"));
    }
    use bollard::Docker;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_core::EncryptionService;

    // ---- Pure unit tests ------------------------------------------------

    #[test]
    fn suggest_new_service_name_has_source_prefix_and_timestamp() {
        let s = RestoreService::suggest_new_service_name("my-pg");
        assert!(s.starts_with("my-pg-restore-"));
        // Ends with a 13-char timestamp segment like 20260422-1030
        let ts = s.trim_start_matches("my-pg-restore-");
        assert_eq!(ts.len(), 13, "unexpected timestamp length: {}", ts);
        assert!(ts.chars().nth(8) == Some('-'));
    }

    #[test]
    fn slugify_normalizes_mixed_input() {
        assert_eq!(slugify("My Restored DB!!"), "my-restored-db");
        assert_eq!(slugify("   leading   "), "leading");
        assert_eq!(slugify("UPPER_case"), "upper-case");
    }

    // ---- MariaDB restore plan -------------------------------------------

    #[test]
    fn postgres_restored_login_keeps_target_physical_configuration() {
        let origin = serde_json::json!({
            "password": "synthetic-origin-password",
            "username": "origin_role",
            "database": "origin_db",
            "container_name": "origin-container",
            "host": "origin-host",
            "port": "10001"
        });
        let mut target = serde_json::json!({
            "password": "synthetic-target-password",
            "username": "target_role",
            "database": "target_db",
            "container_name": "operator-owned-target",
            "host": "target-host",
            "port": "10002",
            "docker_image": "retained-image",
            "memory_limit_mb": 256
        })
        .as_object()
        .unwrap()
        .clone();
        let before = target.clone();
        RestoredServiceCredentials::from_origin("postgres", &origin)
            .unwrap()
            .apply(&mut target);
        for field in ["password", "username", "database"] {
            assert_ne!(before[field], origin[field]);
            assert_eq!(target[field], origin[field]);
        }
        for field in [
            "container_name",
            "host",
            "port",
            "docker_image",
            "memory_limit_mb",
        ] {
            assert_eq!(target[field], before[field]);
        }
    }

    #[tokio::test]
    async fn postgres_restored_login_is_encrypted_in_the_registered_target() {
        let enc = Arc::new(temps_core::EncryptionService::new_from_password(
            "synthetic-restore-test-key",
        ));
        let mut service = crashed_run_service();
        service.service_type = "postgres".to_string();
        service.config = Some(
            enc.encrypt_string(
                &serde_json::json!({
                    "password": "synthetic-target-password",
                    "username": "target_role",
                    "database": "target_db",
                    "container_name": "operator-owned-target",
                    "port": "10002"
                })
                .to_string(),
            )
            .unwrap(),
        );
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![service.clone()], vec![service.clone()]])
            .into_connection();
        let credentials = RestoredServiceCredentials::from_origin(
            "postgres",
            &serde_json::json!({
                "password": "synthetic-origin-password",
                "username": "origin_role",
                "database": "origin_db"
            }),
        )
        .unwrap();
        patch_service_credentials(&db, &enc, service.id, &credentials)
            .await
            .unwrap();
        let transactions = db.into_transaction_log();
        let update = transactions[1].statements().first().unwrap();
        // The only new bind is an encrypted config; no credential is stored
        // in plaintext, and the original imported physical identity survives.
        let ciphertext = update
            .values
            .as_ref()
            .unwrap()
            .0
            .iter()
            .find_map(|value| {
                if let sea_orm::Value::String(Some(value)) = value {
                    enc.decrypt_string(value).ok()
                } else {
                    None
                }
            })
            .unwrap();
        let parameters: serde_json::Value = serde_json::from_str(&ciphertext).unwrap();
        assert_eq!(
            parameters["password"].as_str(),
            credentials.password.as_deref()
        );
        assert_eq!(parameters["username"], "origin_role");
        assert_eq!(parameters["database"], "origin_db");
        assert_eq!(parameters["container_name"], "operator-owned-target");
        assert_eq!(parameters["port"], "10002");
    }

    #[test]
    fn non_postgres_restored_login_keeps_existing_identity_policy() {
        let origin = serde_json::json!({
            "password": "synthetic-origin-password",
            "root_password": "synthetic-origin-root-password",
            "username": "origin_role",
            "database": "origin_db"
        });
        let mut target = serde_json::json!({
            "password": "synthetic-target-password",
            "root_password": "synthetic-target-root-password",
            "username": "target_role",
            "database": "target_db"
        })
        .as_object()
        .unwrap()
        .clone();
        RestoredServiceCredentials::from_origin("mariadb", &origin)
            .unwrap()
            .apply(&mut target);
        assert_eq!(target["password"], origin["password"]);
        assert_eq!(target["root_password"], origin["root_password"]);
        assert_eq!(target["username"], "target_role");
        assert_eq!(target["database"], "target_db");
    }

    /// A cross-service MongoDB restore puts the origin's users in place of
    /// the target's, so the target's stored login takes the origin's root
    /// username with its password: the origin's password under the target's
    /// old username authenticates as nobody. The database stays the
    /// target's.
    #[test]
    fn a_mongodb_restore_takes_the_origins_username_with_its_password() {
        let origin = serde_json::json!({
            "password": "synthetic-origin-password",
            "username": "origin_admin",
            "database": "origin_db"
        });
        let mut target = serde_json::json!({
            "password": "synthetic-target-password",
            "username": "target_admin",
            "database": "target_db"
        })
        .as_object()
        .unwrap()
        .clone();
        RestoredServiceCredentials::from_origin("mongodb", &origin)
            .unwrap()
            .apply(&mut target);
        assert_eq!(target["password"], "synthetic-origin-password");
        assert_eq!(target["username"], "origin_admin");
        assert_eq!(target["database"], "target_db");

        // An origin whose config names no username leaves the target's.
        let mut target = serde_json::json!({ "username": "target_admin" })
            .as_object()
            .unwrap()
            .clone();
        RestoredServiceCredentials::from_origin(
            "mongodb",
            &serde_json::json!({ "password": "synthetic-origin-password" }),
        )
        .unwrap()
        .apply(&mut target);
        assert_eq!(target["username"], "target_admin");
    }

    /// A logical PostgreSQL restore by a job keeps the target's login and
    /// password (the dump's are those of backup time, which may no longer be
    /// the origin's): only the database the origin's data lives in is
    /// reconciled, so the stored password keeps working.
    #[test]
    fn a_job_restore_reconciles_only_the_restored_database() {
        let origin = serde_json::json!({
            "password": "synthetic-origin-current-password",
            "username": "origin_role",
            "database": "origin_db"
        });
        let mut target = serde_json::json!({
            "password": "synthetic-target-password",
            "username": "target_role",
            "database": "target_db"
        })
        .as_object()
        .unwrap()
        .clone();
        RestoredServiceCredentials::from_origin("postgres", &origin)
            .and_then(RestoredServiceCredentials::into_database_only)
            .expect("the origin names its database")
            .apply(&mut target);
        assert_eq!(target["password"], "synthetic-target-password");
        assert_eq!(target["username"], "target_role");
        assert_eq!(target["database"], "origin_db");

        assert!(RestoredServiceCredentials::from_origin(
            "postgres",
            &serde_json::json!({ "password": "p" })
        )
        .and_then(RestoredServiceCredentials::into_database_only)
        .is_none());
    }

    #[test]
    fn missing_origin_login_fields_do_not_overwrite_target_defaults() {
        assert!(
            RestoredServiceCredentials::from_origin("postgres", &serde_json::json!({})).is_none()
        );
        let mut target = serde_json::json!({
            "password": "synthetic-target-password",
            "username": "target_role",
            "database": "target_db"
        })
        .as_object()
        .unwrap()
        .clone();
        RestoredServiceCredentials::from_origin(
            "postgres",
            &serde_json::json!({
                "password": "synthetic-origin-password",
                "root_password": "synthetic-origin-root-password"
            }),
        )
        .unwrap()
        .apply(&mut target);
        assert_eq!(target["username"], "target_role");
        assert_eq!(target["database"], "target_db");
        assert!(!target.contains_key("root_password"));
    }

    #[test]
    fn engine_container_name_matches_mariadb_provider_convention() {
        // Must equal MariaDbService::get_container_name() and the container
        // dispatch.rs probes; a mismatch silently produces a plan naming a
        // container that doesn't exist.
        assert_eq!(engine_container_name("mariadb", "orders"), "mariadb-orders");
        assert_eq!(
            engine_container_name("mongodb", "orders"),
            "temps-mongodb-orders"
        );
    }

    fn mariadb_steps(
        strategy: &str,
        mode: &RestoreRequestMode,
        location: &str,
    ) -> (Vec<String>, bool, Vec<String>) {
        let mut steps = Vec::new();
        let mut destructive = false;
        let mut errors = Vec::new();
        build_mariadb_steps(
            strategy,
            mode,
            "mariadb-orders",
            location,
            &mut steps,
            &mut destructive,
            &mut errors,
        );
        (steps, destructive, errors)
    }

    #[test]
    fn mariadb_physical_in_place_is_destructive_and_describes_the_copy_back() {
        let (steps, destructive, errors) = mariadb_steps(
            "mariadb_physical_restore",
            &RestoreRequestMode::InPlace,
            "external_services/mariadb/orders/2026/05/01/uuid/base.mbstream.gz",
        );
        assert!(destructive, "replacing the datadir in place is destructive");
        assert!(errors.is_empty(), "unexpected errors: {:?}", errors);
        let joined = steps.join("\n");
        assert!(joined.contains("mbstream"));
        assert!(joined.contains("--prepare"));
        assert!(joined.contains("--copy-back"));
        assert!(
            joined.contains("mysql` system schema"),
            "the plan must warn that the source's credentials come with the datadir"
        );
    }

    #[test]
    fn mariadb_physical_new_service_is_not_destructive() {
        let (steps, destructive, errors) = mariadb_steps(
            "mariadb_physical_restore",
            &RestoreRequestMode::NewService {
                name: "orders-copy".into(),
                parameter_overrides: serde_json::Value::Null,
            },
            "external_services/mariadb/orders/2026/05/01/uuid/base.mbstream.gz",
        );
        assert!(!destructive);
        assert!(errors.is_empty(), "unexpected errors: {:?}", errors);
        assert!(steps.iter().any(|s| s.contains("mariadb-orders-copy")));
    }

    #[test]
    fn mariadb_pitr_in_place_is_destructive_but_to_new_service_is_not() {
        let target = RecoveryTarget::Time { time: Utc::now() };
        let location = "external_services/mariadb/orders/2026/05/01/uuid/base.mbstream.gz";

        let (steps, destructive, errors) = mariadb_steps(
            "mariadb_physical_restore",
            &RestoreRequestMode::Pitr {
                to_new_service: false,
                new_service_name: None,
                target: target.clone(),
            },
            location,
        );
        assert!(destructive);
        assert!(errors.is_empty(), "unexpected errors: {:?}", errors);
        assert!(steps.iter().any(|s| s.contains("--stop-datetime")));
        assert!(steps.iter().any(|s| s.contains("manifest.json")));

        let (steps, destructive, errors) = mariadb_steps(
            "mariadb_physical_restore",
            &RestoreRequestMode::Pitr {
                to_new_service: true,
                new_service_name: Some("orders-pitr".into()),
                target,
            },
            location,
        );
        assert!(!destructive, "provisioning a sibling touches no live data");
        assert!(errors.is_empty(), "unexpected errors: {:?}", errors);
        assert!(steps.iter().any(|s| s.contains("mariadb-orders-pitr")));
    }

    #[test]
    fn mariadb_pitr_rejects_targets_the_engine_cannot_honor() {
        // `recovery_target_to_stop_flag` fails fast on Xid and Name, so the
        // preview must surface them as errors, not as steps.
        for target in [
            RecoveryTarget::Xid { xid: "42".into() },
            RecoveryTarget::Name {
                name: "before-migration".into(),
            },
        ] {
            let (_steps, _destructive, errors) = mariadb_steps(
                "mariadb_physical_restore",
                &RestoreRequestMode::Pitr {
                    to_new_service: false,
                    new_service_name: None,
                    target,
                },
                "external_services/mariadb/orders/2026/05/01/uuid/base.mbstream.gz",
            );
            assert_eq!(errors.len(), 1, "expected exactly one error: {:?}", errors);
        }
    }

    #[test]
    fn pitr_location_guard_is_engine_aware() {
        let physical = "external_services/mariadb/orders/2026/05/01/uuid/base.mbstream.gz";
        let dump = "external_services/mariadb/orders/2026/05/01/uuid/dump.sql.gz";
        let walg = "s3://bucket/walg/basebackups_005/base_0000";

        // The regression: a MariaDB physical base is a BARE key, so the
        // Postgres-shaped `s3://` test rejected every MariaDB PITR at the door.
        validate_pitr_backup_location("mariadb", physical)
            .expect("physical MariaDB base must be accepted for PITR");
        validate_pitr_backup_location("MariaDB", physical)
            .expect("engine match is case-insensitive");

        let err = validate_pitr_backup_location("mariadb", dump)
            .expect_err("a logical dump cannot anchor a forward-roll");
        assert!(
            matches!(&err, RestoreError::Validation { message } if message.contains("physical (mariadb-backup)")),
            "MariaDB must get a MariaDB-shaped error, not a WAL-G one: {:?}",
            err
        );

        validate_pitr_backup_location("postgres", walg).expect("WAL-G base still accepted");
        let err = validate_pitr_backup_location("postgres", dump)
            .expect_err("pg_dump cannot anchor a forward-roll");
        assert!(
            matches!(&err, RestoreError::Validation { message } if message.contains("WAL-G")),
            "got {:?}",
            err
        );
    }

    /// Regression: `MariadbPhysicalEngine` writes a WAL-G repository, not a
    /// `base.mbstream.gz` object. Testing only the legacy layout rejected
    /// every PITR restore of every backup the current engine produces, with
    /// HTTP 400 at `startRestore` — before the engine (which handles the
    /// repository layout fine) was ever reached.
    #[test]
    fn pitr_location_guard_accepts_a_walg_repository_base() {
        let repository =
            "s3://temps-backups/prod/external_services/mariadb/orders-mariadb-pitr/walg";

        validate_pitr_backup_location("mariadb", repository)
            .expect("a MariaDB WAL-G repository base must be accepted for PITR");
        validate_pitr_backup_location("MariaDB", &format!("{repository}/"))
            .expect("a trailing slash must not change the classification");
    }

    /// A WAL-G repository restore streams the whole datadir back — including
    /// the `mysql` system schema — exactly like the legacy mbstream base, so
    /// it must take the same credential-propagation path. Leaving it on the
    /// logical-dump path would skip patching the target's stored password
    /// after a cross-service restore and lock the operator out via UI/CLI.
    #[test]
    fn credential_gates_treat_a_walg_repository_as_physical() {
        let repository =
            "s3://temps-backups/prod/external_services/mariadb/orders-mariadb-pitr/walg";
        assert_eq!(
            credential_propagation_gates("mariadb", repository),
            (true, true)
        );
        assert_eq!(
            credential_propagation_gates("mariadb", &format!("{repository}/")),
            (true, true)
        );
    }

    #[test]
    fn credential_gates_split_mariadb_by_backup_format() {
        let physical = "external_services/mariadb/orders/2026/05/01/uuid/base.mbstream.gz";
        let dump = "external_services/mariadb/orders/2026/05/01/uuid/dump.sql.gz";

        // Physical: datadir swap replaces `mysql.user`, so both the pre-restore
        // merge and the post-restore password patch are required.
        assert_eq!(
            credential_propagation_gates("mariadb", physical),
            (true, true)
        );
        assert_eq!(
            credential_propagation_gates("MariaDB", physical),
            (true, true)
        );

        // Logical dump: `mysql` schema is excluded from the dump, so the
        // target's credentials never change — merging would make the engine
        // authenticate with the origin's password and would then overwrite the
        // target's stored password with a credential it never had.
        assert_eq!(
            credential_propagation_gates("mariadb", dump),
            (false, false)
        );
        // Unknown/legacy empty location is treated as non-physical.
        assert_eq!(credential_propagation_gates("mariadb", ""), (false, false));

        // Other engines are unchanged by the MariaDB format split.
        assert_eq!(
            credential_propagation_gates("postgres", "s3://bucket/walg/base"),
            (true, true)
        );
        assert_eq!(
            credential_propagation_gates("postgres", "backups/x.sql.gz"),
            (true, true)
        );
        assert_eq!(
            credential_propagation_gates("mongodb", "backups/x.archive.gz"),
            (true, false)
        );
        assert_eq!(
            credential_propagation_gates("redis", "backups/dump.rdb"),
            (false, false)
        );
    }

    #[test]
    fn mongodb_clone_plan_warns_about_unreconciled_archive_credentials() {
        let mode = RestoreRequestMode::NewService {
            name: "clone".into(),
            parameter_overrides: serde_json::json!({}),
        };
        let mut steps = Vec::new();
        let mut destructive = false;
        let mut errors = Vec::new();
        let warning = build_mongodb_steps(
            "walg_restore",
            &mode,
            "mongodb-target",
            "s3://bucket/archive",
            &mut steps,
            &mut destructive,
            &mut errors,
        )
        .unwrap();
        assert!(!destructive);
        assert!(errors.is_empty());
        assert!(warning.contains("displayed password may differ"));
        assert!(warning.contains("does not automatically reconcile"));
        assert!(steps
            .iter()
            .any(|step| step.contains("verify its displayed credentials")));
    }

    #[test]
    fn mariadb_dump_restore_has_no_pitr_and_preserves_target_credentials() {
        let location = "external_services/mariadb/orders/2026/05/01/uuid/dump.sql.gz";

        let (steps, destructive, errors) = mariadb_steps(
            "mariadb_dump_restore",
            &RestoreRequestMode::InPlace,
            location,
        );
        assert!(destructive);
        assert!(errors.is_empty(), "unexpected errors: {:?}", errors);
        assert!(
            steps.iter().any(|s| s.contains("excludes the `mysql`")),
            "the plan must state that a logical dump does NOT carry credentials"
        );

        let (_steps, _destructive, errors) = mariadb_steps(
            "mariadb_dump_restore",
            &RestoreRequestMode::Pitr {
                to_new_service: false,
                new_service_name: None,
                target: RecoveryTarget::Time { time: Utc::now() },
            },
            location,
        );
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("no binlog anchor"), "{:?}", errors);
    }

    #[test]
    fn mariadb_unclassifiable_location_errors_rather_than_guessing() {
        let (_steps, _destructive, errors) = mariadb_steps(
            "unsupported",
            &RestoreRequestMode::InPlace,
            "external_services/mariadb/orders/some/random/key",
        );
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("base.mbstream.gz"), "{:?}", errors);
    }

    // ---- PITR out-of-range recovery target validation -------------------
    //
    // Regression coverage for the gap a live PITR restore against a real
    // WAL-G backup exposed: before this validation existed, a
    // `recovery_target_time` before the backup's own `started_at` was
    // silently accepted by the API (202), silently accepted by PostgreSQL
    // itself (no FATAL — recovery just stops at the earliest reachable
    // point), and the restore run came back `status: "completed"` having
    // silently discarded the caller's actual requested target. These tests
    // pin the fix: `validate_pitr_recovery_target` must reject that case
    // before the run is ever persisted or a container touched.

    #[test]
    fn pitr_target_before_backup_start_is_rejected() {
        let backup_started_at = chrono::DateTime::parse_from_rfc3339("2026-08-09T11:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let target_before_backup = chrono::DateTime::parse_from_rfc3339("2020-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let target = RecoveryTarget::Time {
            time: target_before_backup,
        };

        let err = validate_pitr_recovery_target(&target, Some(backup_started_at))
            .expect_err("a target years before the backup started must be rejected");
        match err {
            RestoreError::Validation { message } => {
                assert!(
                    message.contains("before this backup started"),
                    "got: {}",
                    message
                );
            }
            other => panic!("expected Validation error, got {:?}", other),
        }
    }

    #[test]
    fn pitr_target_at_or_after_backup_start_is_accepted() {
        let backup_started_at = chrono::DateTime::parse_from_rfc3339("2026-08-09T11:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        // Exactly at the backup start — the boundary case must NOT be
        // rejected (nothing before it is out of range, this is in range).
        let at_start = RecoveryTarget::Time {
            time: backup_started_at,
        };
        assert!(validate_pitr_recovery_target(&at_start, Some(backup_started_at)).is_ok());

        // A minute after the backup started — the ordinary in-range case.
        let after_start = RecoveryTarget::Time {
            time: backup_started_at + chrono::Duration::minutes(1),
        };
        assert!(validate_pitr_recovery_target(&after_start, Some(backup_started_at)).is_ok());
    }

    #[test]
    fn pitr_target_validation_skipped_when_backup_start_unknown() {
        // Orphan restores (backup discovered by S3 scan, no DB row) have no
        // known `started_at` to range-check against — must not fail closed
        // on missing metadata, just skip the check.
        let target = RecoveryTarget::Time {
            time: chrono::DateTime::parse_from_rfc3339("2020-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        assert!(validate_pitr_recovery_target(&target, None).is_ok());
    }

    #[test]
    fn pitr_target_validation_only_applies_to_time_targets() {
        // Xid/Lsn/Name targets have no comparable ordering against
        // `started_at` — the check must be a no-op for them, not panic or
        // spuriously reject.
        let backup_started_at = Utc::now();
        for target in [
            RecoveryTarget::Xid {
                xid: "12345".into(),
            },
            RecoveryTarget::Lsn {
                lsn: "0/3000000".into(),
            },
            RecoveryTarget::Name {
                name: "before-migration".into(),
            },
        ] {
            assert!(validate_pitr_recovery_target(&target, Some(backup_started_at)).is_ok());
        }
    }

    #[test]
    fn restore_request_mode_as_str() {
        assert_eq!(RestoreRequestMode::InPlace.as_str(), "in_place");
        assert_eq!(
            RestoreRequestMode::NewService {
                name: "x".into(),
                parameter_overrides: serde_json::json!({})
            }
            .as_str(),
            "new_service"
        );
        assert_eq!(
            RestoreRequestMode::Pitr {
                to_new_service: false,
                new_service_name: None,
                target: RecoveryTarget::Time {
                    time: chrono::Utc::now(),
                },
            }
            .as_str(),
            "pitr"
        );
    }

    #[test]
    fn restore_request_mode_flattened_json_round_trip() {
        // `#[serde(flatten)]` in the handler relies on this exact shape.
        let in_place = serde_json::to_value(RestoreRequestMode::InPlace).unwrap();
        assert_eq!(in_place["mode"], "in_place");

        let new_svc = serde_json::to_value(RestoreRequestMode::NewService {
            name: "pg-clone".into(),
            parameter_overrides: serde_json::json!({"port": "5500"}),
        })
        .unwrap();
        assert_eq!(new_svc["mode"], "new_service");
        assert_eq!(new_svc["name"], "pg-clone");
        assert_eq!(new_svc["parameter_overrides"]["port"], "5500");

        let parsed: RestoreRequestMode = serde_json::from_value(new_svc).unwrap();
        matches!(parsed, RestoreRequestMode::NewService { .. });
    }

    #[test]
    fn backup_engine_prefers_metadata_then_snapshot_then_current_service() {
        let some = |v: &str| Some(v.to_string());
        assert_eq!(
            pick_backup_engine(
                r#"{"service_type":"postgres"}"#,
                &[(some("redis"), some("redis"))]
            ),
            ResolvedBackupEngine::Known("postgres".into())
        );
        // Producer service deleted: the snapshot on the link still knows.
        assert_eq!(
            pick_backup_engine("{}", &[(some("mongodb"), None)]),
            ResolvedBackupEngine::Known("mongodb".into())
        );
        assert_eq!(
            pick_backup_engine("not json", &[(None, some("mariadb"))]),
            ResolvedBackupEngine::Known("mariadb".into())
        );
    }

    #[test]
    fn backup_engine_unknown_is_fail_closed_only_when_a_producer_exists() {
        assert_eq!(
            pick_backup_engine(r#"{"service_type":"  "}"#, &[(None, None)]),
            ResolvedBackupEngine::Unknown { has_producer: true }
        );
        assert_eq!(
            pick_backup_engine(r#"{"engine":"control_plane"}"#, &[]),
            ResolvedBackupEngine::Unknown {
                has_producer: false
            }
        );
    }

    #[test]
    fn engine_compatibility_allows_only_the_same_family() {
        assert!(engines_compatible("Postgres", "postgres"));
        assert!(engines_compatible("rustfs", "s3"));
        assert!(engines_compatible("minio", "blob"));
        assert!(!engines_compatible("postgres", "redis"));
        assert!(!engines_compatible("mongodb", "mariadb"));
        assert!(!engines_compatible("s3", "postgres"));
    }

    /// Fence that records calls and, on its first call, lets the stand-in
    /// engine finish (as stopping a real engine's helper would).
    struct ReleasingFence {
        calls: std::sync::atomic::AtomicUsize,
        release: tokio::sync::Notify,
    }

    #[async_trait::async_trait]
    impl super::super::restore_reconcile::RestoreHelperFence for ReleasingFence {
        async fn fence(
            &self,
            _target: &super::super::restore_reconcile::FenceTarget,
        ) -> Result<
            temps_providers::externalsvc::restore_helper::RestoreFenceReport,
            temps_providers::externalsvc::restore_helper::RestoreFenceError,
        > {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.release.notify_one();
            Ok(Default::default())
        }
    }

    /// REGRESSION (Greptile on #1295): an engine that never checks the gate
    /// (a Redis WAL-G helper can run for an hour) must still stop promptly
    /// once cancelled: its helpers are stopped while the call is awaited, and
    /// the call itself is never dropped.
    #[tokio::test]
    async fn cancelled_provision_stops_helpers_and_is_still_awaited() {
        let fence = ReleasingFence {
            calls: std::sync::atomic::AtomicUsize::new(0),
            release: tokio::sync::Notify::new(),
        };
        let cancel = tokio_util::sync::CancellationToken::new();
        let finished = std::sync::atomic::AtomicBool::new(false);
        // Stand-in engine: ignores cancellation, only returns once its
        // helper is stopped.
        let provision = async {
            fence.release.notified().await;
            finished.store(true, std::sync::atomic::Ordering::SeqCst);
            "helper stopped"
        };
        cancel.cancel();
        let output = await_new_service_provision(
            provision,
            &cancel,
            &fence,
            &new_service_fence_target("copy", ServiceType::Redis),
            std::time::Duration::from_millis(10),
        )
        .await;
        assert_eq!(output, "helper stopped");
        assert!(
            finished.load(std::sync::atomic::Ordering::SeqCst),
            "the call ran to completion"
        );
        assert!(fence.calls.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn uncancelled_provision_never_touches_helpers() {
        let fence = ReleasingFence {
            calls: std::sync::atomic::AtomicUsize::new(0),
            release: tokio::sync::Notify::new(),
        };
        let cancel = tokio_util::sync::CancellationToken::new();
        let output = await_new_service_provision(
            async { 42 },
            &cancel,
            &fence,
            &new_service_fence_target("copy", ServiceType::Redis),
            std::time::Duration::from_secs(3),
        )
        .await;
        assert_eq!(output, 42);
        assert_eq!(fence.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// REGRESSION (Greptile on #1295): a download that could not be
    /// confirmed stopped must reach the worker as its own outcome (which
    /// keeps the run active), never as a plain failure or a cancellation.
    #[test]
    fn engine_error_keeps_an_unconfirmed_stop_typed() {
        use temps_providers::externalsvc::{EngineError, StagedRestoreStillRunning};
        let still_running = EngineError::from(StagedRestoreStillRunning {
            target: "postgres-orders".into(),
            reason: "docker unreachable".into(),
        });
        match engine_error(7, still_running, "in-place restore failed") {
            RestoreError::StagedRestoreStillRunning {
                restore_run_id,
                target,
                reason,
            } => {
                assert_eq!(restore_run_id, 7);
                assert_eq!(target, "postgres-orders");
                assert_eq!(reason, "docker unreachable");
            }
            other => panic!("expected StagedRestoreStillRunning, got {other:?}"),
        }
        assert!(matches!(
            engine_error(
                7,
                EngineError::engine_failure("restore", "disk full"),
                "in-place restore failed",
            ),
            RestoreError::ExternalService { ref reason }
                if reason == "in-place restore failed: restore: disk full"
        ));
    }

    #[test]
    fn view_reports_cancellability_from_the_stored_run() {
        let now = chrono::Utc::now();
        let mut model = temps_entities::restore_runs::Model {
            id: 9,
            source_backup_id: 1,
            source_service_id: 2,
            target_service_id: None,
            target_service_name: None,
            mode: "in_place".into(),
            status: "running".into(),
            phase: "download".into(),
            recovery_target: None,
            parameter_overrides: serde_json::json!({}),
            resume_token: None,
            log_id: "abc".into(),
            error_message: None,
            attempt: 1,
            started_at: Some(now),
            finished_at: None,
            created_by: 1,
            created_at: now,
            updated_at: now,
            cancel_requested_at: None,
            cancel_requested_by: None,
        };
        let view: RestoreRunView = model.clone().into();
        assert!(view.cancellable);
        assert!(view.not_cancellable_reason.is_none());

        model.phase = "restore".into();
        let view: RestoreRunView = model.into();
        assert!(!view.cancellable);
        assert!(view
            .not_cancellable_reason
            .as_deref()
            .is_some_and(|r| r.contains("started writing data")));
    }

    #[test]
    fn view_from_model_formats_dates_as_iso8601() {
        let now = chrono::Utc::now();
        let model = temps_entities::restore_runs::Model {
            id: 42,
            source_backup_id: 1,
            source_service_id: 2,
            target_service_id: None,
            target_service_name: Some("clone".into()),
            mode: "new_service".into(),
            status: "running".into(),
            phase: "provision".into(),
            recovery_target: None,
            parameter_overrides: serde_json::json!({}),
            resume_token: None,
            log_id: "abc".into(),
            error_message: None,
            attempt: 1,
            started_at: Some(now),
            finished_at: None,
            created_by: 1,
            created_at: now,
            updated_at: now,
            cancel_requested_at: None,
            cancel_requested_by: None,
        };
        let v: RestoreRunView = model.into();
        assert_eq!(v.id, 42);
        assert_eq!(v.status, "running");
        assert!(v.created_at.ends_with("+00:00") || v.created_at.ends_with('Z'));
        assert!(v.started_at.unwrap().contains('T'));
    }

    fn tracked_backup(id: i32) -> temps_entities::backups::Model {
        temps_entities::backups::Model {
            id,
            name: "nightly".to_string(),
            backup_id: format!("uuid-{id}"),
            schedule_id: None,
            schedule_run_id: None,
            backup_type: "full".to_string(),
            state: "completed".to_string(),
            started_at: chrono::DateTime::parse_from_rfc3339("2026-10-01T02:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            finished_at: None,
            size_bytes: None,
            file_count: None,
            s3_source_id: 3,
            s3_location: format!("external_services/postgres/orders/{id}"),
            error_message: None,
            metadata: "{}".to_string(),
            checksum: None,
            compression_type: "gzip".to_string(),
            created_by: 1,
            expires_at: None,
            tags: "[]".to_string(),
        }
    }

    #[tokio::test]
    async fn run_views_name_their_source_backup_in_one_query() {
        let tracked = temps_entities::restore_runs::Model {
            id: 1,
            source_backup_id: 5,
            ..crashed_run_row()
        };
        let deleted = temps_entities::restore_runs::Model {
            id: 2,
            source_backup_id: 6,
            ..crashed_run_row()
        };
        let raw = temps_entities::restore_runs::Model {
            id: 3,
            source_backup_id: 0,
            resume_token: Some(serde_json::json!({
                "backup_location": "s3://bucket/base_000000010000000000000002",
                "engine_hint": "postgres",
                "s3_source_id": 4,
            })),
            ..crashed_run_row()
        };
        // One batched lookup for every tracked id; backup 6 no longer exists.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![tracked_backup(5)]])
            .into_connection();

        let views = views_with_source_backups(&db, vec![tracked, deleted, raw])
            .await
            .expect("views");

        assert_eq!(
            views[0].source_backup,
            RestoreRunSourceBackup {
                id: Some(5),
                backup_id: Some("uuid-5".to_string()),
                s3_source_id: Some(3),
                location: Some("external_services/postgres/orders/5".to_string()),
                taken_at: Some("2026-10-01T02:00:00Z".to_string()),
            }
        );
        assert_eq!(views[1].source_backup.id, Some(6));
        assert_eq!(views[1].source_backup.backup_id, None, "deleted backup");
        assert_eq!(views[2].source_backup.id, None);
        assert_eq!(views[2].source_backup.s3_source_id, Some(4));
        assert_eq!(
            views[2].source_backup.location.as_deref(),
            Some("s3://bucket/base_000000010000000000000002")
        );
        assert_eq!(db.into_transaction_log().len(), 1, "a single backups query");
    }

    #[tokio::test]
    async fn run_views_skip_the_backup_lookup_for_raw_location_restores() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let raw = temps_entities::restore_runs::Model {
            source_backup_id: 0,
            ..crashed_run_row()
        };
        let views = views_with_source_backups(&db, vec![raw])
            .await
            .expect("views");
        assert_eq!(views[0].source_backup, RestoreRunSourceBackup::default());
        assert!(db.into_transaction_log().is_empty());
    }

    fn busy_wait(delay: std::time::Duration) -> crate::node_busy::NodeWait {
        crate::node_busy::NodeWait {
            attempt: 1,
            delay,
            waited: std::time::Duration::ZERO,
            node: "worker-1".to_string(),
            detail: "this node already runs 2 backups or restores".to_string(),
        }
    }

    /// A run whose node is busy shows `waiting_for_node`, then goes back to
    /// the phase it was about to enter.
    #[tokio::test]
    async fn a_run_waiting_for_its_node_shows_it_then_resumes_its_phase() {
        let entered = || sea_orm::MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([entered(), entered()])
            .into_connection();
        wait_for_node_slot(&db, 7, "restore", busy_wait(std::time::Duration::ZERO))
            .await
            .expect("the wait ends in the restore phase");
        let phases: Vec<String> = db
            .into_transaction_log()
            .into_iter()
            .map(|statement| format!("{statement:?}"))
            .collect();
        assert_eq!(phases.len(), 2, "{phases:?}");
        assert!(phases[0].contains("waiting_for_node"), "{}", phases[0]);
        assert!(phases[1].contains("\"restore\""), "{}", phases[1]);
    }

    /// A cancel requested while the run waits for its node stops it there:
    /// nothing ran on the node.
    #[tokio::test]
    async fn a_run_waiting_for_its_node_stops_when_cancelled() {
        let now = Utc::now();
        let cancelled = temps_entities::restore_runs::Model {
            id: 7,
            source_backup_id: 3,
            source_service_id: 11,
            target_service_id: Some(11),
            target_service_name: None,
            mode: "in_place".to_string(),
            status: "running".to_string(),
            phase: "waiting_for_node".to_string(),
            recovery_target: None,
            parameter_overrides: serde_json::json!({}),
            resume_token: None,
            log_id: "log".to_string(),
            error_message: None,
            attempt: 1,
            started_at: Some(now),
            finished_at: None,
            created_by: 1,
            created_at: now,
            updated_at: now,
            cancel_requested_at: Some(now),
            cancel_requested_by: Some(1),
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([vec![cancelled]])
            .into_connection();
        let error = wait_for_node_slot(
            &db,
            7,
            "restore",
            busy_wait(std::time::Duration::from_secs(60)),
        )
        .await
        .expect_err("a cancelled run does not wait");
        assert!(
            matches!(&error, RestoreError::RestoreCancelled { restore_run_id: 7, phase } if phase == "waiting_for_node"),
            "{error:?}"
        );
    }

    // ---- MockDatabase-backed validation tests ---------------------------

    /// Construct a RestoreService wired to the provided MockDatabase. Uses a
    /// real EncryptionService + real ExternalServiceManager; the manager
    /// creates Docker handles lazily, so no container-side work happens as
    /// long as we don't actually call a method that touches Docker.
    fn build_restore_service(db: Arc<sea_orm::DatabaseConnection>) -> RestoreService {
        let key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let enc = Arc::new(EncryptionService::new(key).unwrap());
        // Docker connection is lazy for our code paths (no container ops in
        // the validation branches we're exercising).
        let docker = Arc::new(
            Docker::connect_with_local_defaults()
                .expect("Docker socket required to construct ExternalServiceManager in tests"),
        );
        let dns_registry = Arc::new(temps_providers::DnsRegistry::new(db.clone()));
        let mgr = Arc::new(temps_providers::ExternalServiceManager::new(
            db.clone(),
            enc.clone(),
            docker,
            dns_registry,
        ));
        RestoreService::new(db, mgr, enc)
    }

    fn make_producer_row(
        id: i32,
        backup_id: i32,
        service_id: i32,
    ) -> temps_entities::external_service_backups::Model {
        temps_entities::external_service_backups::Model {
            id,
            service_id,
            backup_id,
            backup_type: "full".to_string(),
            state: "completed".to_string(),
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            size_bytes: Some(1024),
            s3_location: format!("s3://bucket/{backup_id}/{service_id}"),
            error_message: None,
            metadata: serde_json::json!({}),
            checksum: None,
            compression_type: "gzip".to_string(),
            created_by: 1,
            expires_at: None,
            service_name_snapshot: Some(format!("service-{service_id}")),
            service_type_snapshot: Some("postgres".to_string()),
        }
    }

    #[tokio::test]
    async fn test_backup_source_services_for_backups_dedupes_and_preserves_ownerless() {
        // Arrange: backup 10 has two producer services (one duplicate), while
        // backup 20 is raw/control-plane and therefore ownerless.
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![
                    make_producer_row(1, 10, 8),
                    make_producer_row(2, 10, 3),
                    make_producer_row(3, 10, 8),
                ]])
                .into_connection(),
        );
        let service = build_restore_service(db);

        // Act.
        let mappings = service
            .backup_source_services_for_backups(&[20, 10, 20])
            .await
            .expect("producer mappings should resolve");

        // Assert.
        assert_eq!(
            mappings,
            vec![
                BackupProducerServices {
                    backup_id: 10,
                    service_ids: vec![3, 8],
                },
                BackupProducerServices {
                    backup_id: 20,
                    service_ids: vec![],
                },
            ]
        );
    }

    #[tokio::test]
    async fn test_backup_source_services_for_backups_database_error_is_typed() {
        // Arrange.
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors(vec![sea_orm::DbErr::Custom(
                    "restore producer lookup failed".to_string(),
                )])
                .into_connection(),
        );
        let service = build_restore_service(db);

        // Act.
        let error = service
            .backup_source_services_for_backups(&[10])
            .await
            .expect_err("producer query error must remain typed");

        // Assert.
        assert!(matches!(error, RestoreError::Database(_)));
        assert!(error.to_string().contains("restore producer lookup failed"));
    }

    #[tokio::test]
    async fn start_restore_returns_backup_not_found_when_id_missing() {
        // Under the new contract the URL's service id is the TARGET, and
        // the backup is resolved second. We mock the backup lookup as
        // empty (no row for id=999); the target service lookup comes
        // next — we never hit it because backup lookup fails first.
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results::<temps_entities::backups::Model, _, _>(vec![vec![]])
                .into_connection(),
        );
        if Docker::connect_with_local_defaults().is_err() {
            eprintln!("Docker not available, skipping");
            return;
        }
        let svc = build_restore_service(db);

        let err = svc
            .start_restore(
                1, /* target_service_id */
                BackupSelector::Id(999),
                RestoreRequestMode::InPlace,
                1,
            )
            .await
            .expect_err("expected BackupNotFound");
        assert!(
            matches!(err, RestoreError::BackupNotFound { backup_id: 999 }),
            "got {:?}",
            err
        );
    }

    #[test]
    fn only_completed_backups_are_restorable() {
        assert!(ensure_backup_restorable(&tracked_backup(7)).is_ok());
        assert!(matches!(
            ensure_backup_restorable(&temps_entities::backups::Model {
                state: "deleting".into(),
                ..tracked_backup(7)
            }),
            Err(RestoreError::BackupDeleting { backup_id: 7 })
        ));
        for state in ["pending", "running", "failed"] {
            let backup = temps_entities::backups::Model {
                state: state.into(),
                ..tracked_backup(7)
            };
            match ensure_backup_restorable(&backup) {
                Err(RestoreError::BackupNotRestorable {
                    backup_id: 7,
                    state: rejected,
                }) => assert_eq!(rejected, state),
                other => panic!("state '{state}' must not be restorable, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn start_restore_rejects_a_running_backup_before_touching_the_target() {
        // Only the backup lookup is mocked: reaching the target-service
        // lookup would exhaust the mock and fail with a different error.
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![temps_entities::backups::Model {
                    state: "running".into(),
                    ..tracked_backup(42)
                }]])
                .into_connection(),
        );
        if Docker::connect_with_local_defaults().is_err() {
            eprintln!("Docker not available, skipping");
            return;
        }
        let svc = build_restore_service(db);

        let err = svc
            .start_restore(1, BackupSelector::Id(42), RestoreRequestMode::InPlace, 1)
            .await
            .expect_err("a running backup must not be restorable");
        assert!(
            matches!(
                &err,
                RestoreError::BackupNotRestorable { backup_id: 42, state } if state == "running"
            ),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn admission_rejects_a_backup_that_failed_after_the_precheck() {
        // The pre-check in `start_restore` reads without a lock; a backup
        // can fail between it and admission, so admission re-checks under
        // the row lock and must refuse before inserting the run.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![temps_entities::backups::Model {
                state: "failed".into(),
                ..tracked_backup(42)
            }]])
            .into_connection();
        let txn = db.begin().await.unwrap();

        let err = lock_and_insert_restore_run(
            &txn,
            <temps_entities::restore_runs::ActiveModel as Default>::default(),
            Some(42),
            3,
            true,
        )
        .await
        .expect_err("a failed backup must not be admitted");
        assert!(
            matches!(
                &err,
                RestoreError::BackupNotRestorable { backup_id: 42, state } if state == "failed"
            ),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn start_restore_rejects_empty_orphan_location() {
        // Orphan restore with an empty location should fail validation
        // before any DB lookup happens.
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        if Docker::connect_with_local_defaults().is_err() {
            eprintln!("Docker not available, skipping");
            return;
        }
        let svc = build_restore_service(db);

        let err = svc
            .start_restore(
                1,
                BackupSelector::Location {
                    location: "   ".into(),
                    engine: "postgres".into(),
                    s3_source_id: 1,
                },
                RestoreRequestMode::InPlace,
                1,
            )
            .await
            .expect_err("expected Validation error");
        assert!(
            matches!(err, RestoreError::Validation { .. }),
            "got {:?}",
            err
        );
    }

    #[tokio::test]
    async fn get_restore_run_returns_not_found() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results::<temps_entities::restore_runs::Model, _, _>(vec![vec![]])
                .into_connection(),
        );
        if Docker::connect_with_local_defaults().is_err() {
            eprintln!("Docker not available, skipping");
            return;
        }
        let svc = build_restore_service(db);

        let err = svc
            .get_restore_run(404)
            .await
            .expect_err("expected not found");
        assert!(
            matches!(
                err,
                RestoreError::RestoreRunNotFound {
                    restore_run_id: 404
                }
            ),
            "got {:?}",
            err
        );
    }

    #[tokio::test]
    async fn list_restore_runs_returns_empty_when_no_rows() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results::<temps_entities::restore_runs::Model, _, _>(vec![vec![]])
                .into_connection(),
        );
        if Docker::connect_with_local_defaults().is_err() {
            eprintln!("Docker not available, skipping");
            return;
        }
        let svc = build_restore_service(db);

        let runs = svc.list_restore_runs_for_service(1).await.unwrap();
        assert!(runs.is_empty());
    }

    // ---- S3 client builder ---------------------------------------------

    #[test]
    fn build_s3_client_applies_region() {
        let creds = S3Credentials {
            access_key_id: "k".into(),
            secret_key: "s".into(),
            session_token: None,
            region: "eu-central-1".into(),
            endpoint: Some("http://localhost:9000".into()),
            bucket_name: "b".into(),
            bucket_path: "p".into(),
            force_path_style: true,
        };
        let client = build_s3_client(&creds);
        let conf = client.config();
        assert_eq!(conf.region().unwrap().as_ref(), "eu-central-1");
    }

    #[test]
    fn build_s3_client_wraps_bare_endpoint_with_http() {
        let creds = S3Credentials {
            access_key_id: "k".into(),
            secret_key: "s".into(),
            session_token: None,
            region: "us-east-1".into(),
            endpoint: Some("minio.example.com:9000".into()),
            bucket_name: "b".into(),
            bucket_path: "p".into(),
            force_path_style: true,
        };
        // Just verify it doesn't panic and produces a client. The
        // aws-sdk-s3 builder doesn't expose endpoint() on the final config
        // in a stable way across minor versions, so we check behavior via
        // construction success.
        let _client = build_s3_client(&creds);
    }

    // ---- Backup-location repair against a REAL RustFS (docker-tests) -----
    //
    // `resolve_backup_location_from_s3` is the repair path for `backups` rows
    // whose `s3_location` was never populated. It is private, so it can only be
    // exercised from in-crate tests — and it is pure S3 listing, so mocking the
    // S3 client would only test the mock. These tests boot a real RustFS,
    // seed real objects at the real key shapes the engines write, and call the
    // real function.

    #[cfg(feature = "docker-tests")]
    const LOCATION_TEST_S3_ACCESS_KEY: &str = crate::test_rustfs::RUSTFS_ACCESS_KEY;
    #[cfg(feature = "docker-tests")]
    const LOCATION_TEST_S3_SECRET_KEY: &str = crate::test_rustfs::RUSTFS_SECRET_KEY;

    /// RAII reaper for the RustFS container booted by the location-resolution
    /// tests. Mirrors `tests/mariadb_pitr_e2e.rs::ContainerGuard`; requires a
    /// multi-thread test runtime because it drives Docker from `Drop`.
    #[cfg(feature = "docker-tests")]
    struct S3ServerGuard {
        docker: Docker,
        id: String,
    }

    #[cfg(feature = "docker-tests")]
    impl Drop for S3ServerGuard {
        fn drop(&mut self) {
            let docker = self.docker.clone();
            let id = self.id.clone();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    tokio::task::block_in_place(|| {
                        handle.block_on(async {
                            let _ = docker
                                .remove_container(
                                    &id,
                                    Some(bollard::query_parameters::RemoveContainerOptions {
                                        force: true,
                                        v: true,
                                        ..Default::default()
                                    }),
                                )
                                .await;
                            eprintln!("Reaped RustFS container {id}");
                        });
                    });
                }
            }));
        }
    }

    /// Boot a RustFS container for the location-resolution tests, returning
    /// `(host_port, guard)`. Returns `None` (graceful skip) whenever Docker is
    /// unreachable or the image cannot be pulled — never panics on missing
    /// infrastructure.
    #[cfg(feature = "docker-tests")]
    async fn boot_location_test_s3() -> Option<(u16, S3ServerGuard)> {
        use futures::StreamExt;
        use std::collections::HashMap;

        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Docker unavailable (connect failed), skipping: {e}");
                return None;
            }
        };
        if let Err(e) = docker.ping().await {
            eprintln!("Docker socket unreachable (ping failed), skipping: {e}");
            return None;
        }

        let mut stream = docker.create_image(
            Some(bollard::query_parameters::CreateImageOptions {
                from_image: Some(crate::test_rustfs::RUSTFS_IMAGE.to_string()),
                tag: Some(crate::test_rustfs::RUSTFS_TAG.to_string()),
                ..Default::default()
            }),
            None,
            None,
        );
        while let Some(item) = stream.next().await {
            if let Err(e) = item {
                eprintln!("Could not pull RustFS image, skipping: {e}");
                return None;
            }
        }

        let port = {
            use std::net::TcpListener;
            (9400..9600).find(|&p| TcpListener::bind(("127.0.0.1", p)).is_ok())?
        };
        let name = format!("temps-test-restore-loc-rustfs-{}", uuid::Uuid::new_v4());

        let config = bollard::models::ContainerCreateBody {
            image: Some(format!(
                "{}:{}",
                crate::test_rustfs::RUSTFS_IMAGE,
                crate::test_rustfs::RUSTFS_TAG
            )),
            env: Some(
                crate::test_rustfs::rustfs_env()
                    .into_iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect(),
            ),
            host_config: Some(bollard::models::HostConfig {
                port_bindings: Some(HashMap::from([(
                    "9000/tcp".to_string(),
                    Some(vec![bollard::models::PortBinding {
                        host_ip: Some("127.0.0.1".to_string()),
                        host_port: Some(port.to_string()),
                    }]),
                )])),
                ..Default::default()
            }),
            ..Default::default()
        };

        let created = docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&name)
                        .build(),
                ),
                config,
            )
            .await
            .ok()?;
        let guard = S3ServerGuard {
            docker: docker.clone(),
            id: created.id.clone(),
        };
        docker
            .start_container(
                &created.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .ok()?;
        if let Err(e) = crate::test_rustfs::wait_for_rustfs_ready(port).await {
            eprintln!("{e}, skipping");
            return None;
        }
        Some((port, guard))
    }

    /// An `s3_sources::Model` pointing at the local RustFS with an empty
    /// `bucket_path`, matching the row shape `run_pitr_flow` inserts.
    #[cfg(feature = "docker-tests")]
    fn location_test_s3_source(port: u16, bucket: &str) -> temps_entities::s3_sources::Model {
        temps_entities::s3_sources::Model {
            id: 1,
            name: "loc-test-s3".to_string(),
            backing_service_id: None,
            bucket_name: bucket.to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some(format!("http://127.0.0.1:{port}")),
            bucket_path: String::new(),
            access_key_id: LOCATION_TEST_S3_ACCESS_KEY.to_string(),
            secret_key: LOCATION_TEST_S3_SECRET_KEY.to_string(),
            session_token: None,
            credentials_expire_at: None,
            force_path_style: Some(true),
            is_default: true,
            managed_by_cloud: false,
            lifecycle_reconcile_failed_at: None,
            lifecycle_reconcile_generation: 0,
            worker_credentials: "scoped".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// REGRESSION (Greptile findings on PR #878, two rounds):
    ///
    /// 1. A legacy MariaDB *physical* backup row with an empty `s3_location`
    ///    was undiscoverable, because the extension allowlist in
    ///    `resolve_backup_location_from_s3` did not include `.mbstream.gz`.
    ///    That made the backup permanently unrestorable through the repair
    ///    path — a data-safety bug, not a cosmetic one.
    /// 2. After (1) was fixed, the resolver still picked the *newest matching
    ///    object of any format* under the service prefix — so a service with
    ///    both a physical base and a later logical dump would silently
    ///    substitute the dump for a physical backup row, restoring the wrong
    ///    snapshot and format. The fix scopes every non-WAL-G lookup to the
    ///    calling backup's own `<backup_uuid>/` path segment (`backups.backup_id`,
    ///    the same uuid every engine already writes its artifact under —
    ///    see `v2_common::build_external_service_s3_key`), so the resolver can
    ///    only ever return that specific backup's own object.
    ///
    /// This seeds the exact key shape a physical base occupies in a real
    /// bucket and asserts the resolver now finds it; seeds a logical
    /// `dump.sql.gz` under a *different* service prefix and asserts that one
    /// still resolves (the extension fix is additive, not a swap); and seeds
    /// a physical base and a NEWER logical dump under the SAME service
    /// prefix with different backup uuids, asserting each backup's own uuid
    /// resolves to its own artifact rather than the newer one winning.
    #[cfg(feature = "docker-tests")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resolve_backup_location_from_s3_finds_mariadb_physical_and_logical_backups() {
        // ---- Arrange: real RustFS + real objects at real key shapes --------
        let Some((port, _s3_guard)) = boot_location_test_s3().await else {
            return;
        };
        let bucket = "restore-location-test";
        let s3_source = location_test_s3_source(port, bucket);
        let s3_client = build_s3_client(&S3Credentials {
            access_key_id: LOCATION_TEST_S3_ACCESS_KEY.to_string(),
            secret_key: LOCATION_TEST_S3_SECRET_KEY.to_string(),
            session_token: None,
            region: "us-east-1".to_string(),
            endpoint: s3_source.endpoint.clone(),
            bucket_name: bucket.to_string(),
            bucket_path: String::new(),
            force_path_style: true,
        });
        if let Err(e) = s3_client.create_bucket().bucket(bucket).send().await {
            eprintln!("Could not create RustFS bucket, skipping: {e}");
            return;
        }

        let physical_service = "orders-physical";
        let logical_service = "orders-logical";
        let physical_uuid = uuid::Uuid::new_v4().to_string();
        let logical_uuid = uuid::Uuid::new_v4().to_string();
        let physical_key = format!(
            "external_services/mariadb/{physical_service}/2026/01/01/{physical_uuid}/base.mbstream.gz"
        );
        let logical_key = format!(
            "external_services/mariadb/{logical_service}/2026/01/01/{logical_uuid}/dump.sql.gz"
        );
        // Engines always write a `metadata.json` companion next to the
        // artifact; seeding it proves the resolver picks the artifact and not
        // its sidecar.
        for key in [
            physical_key.clone(),
            logical_key.clone(),
            physical_key.replace("base.mbstream.gz", "metadata.json"),
            logical_key.replace("dump.sql.gz", "metadata.json"),
        ] {
            if let Err(e) = s3_client
                .put_object()
                .bucket(bucket)
                .key(&key)
                .body(aws_sdk_s3::primitives::ByteStream::from_static(b"seed"))
                .send()
                .await
            {
                eprintln!("Could not seed object {key}, skipping: {e}");
                return;
            }
        }

        // ---- Act + Assert: physical base is now discoverable --------------
        let physical = resolve_backup_location_from_s3(
            &s3_client,
            &s3_source,
            "mariadb",
            physical_service,
            &physical_uuid,
        )
        .await
        .expect("listing a reachable bucket must not error");
        eprintln!("resolved physical location = {physical:?}");
        let physical = physical.expect(
            "a .mbstream.gz physical base must be discoverable; before the \
             extension-allowlist fix this returned None and the backup was unrestorable",
        );
        assert!(
            physical.ends_with("base.mbstream.gz"),
            "resolver must return the physical base artifact, got {physical}"
        );
        assert_eq!(physical, physical_key, "resolver must return the exact key");

        // ---- Assert: the logical-dump path did not regress ----------------
        let logical = resolve_backup_location_from_s3(
            &s3_client,
            &s3_source,
            "mariadb",
            logical_service,
            &logical_uuid,
        )
        .await
        .expect("listing a reachable bucket must not error");
        eprintln!("resolved logical location = {logical:?}");
        let logical = logical.expect("a .sql.gz logical dump must stay discoverable");
        assert_eq!(
            logical, logical_key,
            "resolver must return the logical dump artifact unchanged"
        );

        // ---- Assert: an unknown service still resolves to None ------------
        let missing = resolve_backup_location_from_s3(
            &s3_client,
            &s3_source,
            "mariadb",
            "no-such-service",
            &physical_uuid,
        )
        .await
        .expect("listing an empty prefix must not error");
        assert!(
            missing.is_none(),
            "an empty service prefix must resolve to None, got {missing:?}"
        );

        // ---- REGRESSION (round 2): same service, two backups of different
        //      formats and different ages — the resolver must not let the
        //      newer one win when a specific backup's own uuid is given. ---
        let shared_service = "orders-mixed-formats";
        let older_physical_uuid = uuid::Uuid::new_v4().to_string();
        let newer_logical_uuid = uuid::Uuid::new_v4().to_string();
        let older_physical_key = format!(
            "external_services/mariadb/{shared_service}/2026/01/01/{older_physical_uuid}/base.mbstream.gz"
        );
        let newer_logical_key = format!(
            "external_services/mariadb/{shared_service}/2026/01/02/{newer_logical_uuid}/dump.sql.gz"
        );
        // Seed the OLDER physical object first, then the NEWER logical
        // object second, so a naive "pick whatever S3 reports as most
        // recently modified" implementation would pick the logical one.
        if let Err(e) = s3_client
            .put_object()
            .bucket(bucket)
            .key(&older_physical_key)
            .body(aws_sdk_s3::primitives::ByteStream::from_static(b"seed"))
            .send()
            .await
        {
            eprintln!("Could not seed object {older_physical_key}, skipping: {e}");
            return;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        if let Err(e) = s3_client
            .put_object()
            .bucket(bucket)
            .key(&newer_logical_key)
            .body(aws_sdk_s3::primitives::ByteStream::from_static(b"seed"))
            .send()
            .await
        {
            eprintln!("Could not seed object {newer_logical_key}, skipping: {e}");
            return;
        }

        let resolved_for_older = resolve_backup_location_from_s3(
            &s3_client,
            &s3_source,
            "mariadb",
            shared_service,
            &older_physical_uuid,
        )
        .await
        .expect("listing a reachable bucket must not error")
        .expect("the older physical backup's own artifact must still be discoverable by its uuid");
        eprintln!("resolved (older physical uuid) = {resolved_for_older}");
        assert_eq!(
            resolved_for_older, older_physical_key,
            "SECURITY/DATA-SAFETY: resolving the OLDER physical backup's own uuid must return \
             its own artifact, not the newer logical dump under the same service prefix — \
             substituting artifacts here means restoring the wrong snapshot in the wrong format"
        );

        let resolved_for_newer = resolve_backup_location_from_s3(
            &s3_client,
            &s3_source,
            "mariadb",
            shared_service,
            &newer_logical_uuid,
        )
        .await
        .expect("listing a reachable bucket must not error")
        .expect("the newer logical backup's own artifact must be discoverable by its uuid");
        assert_eq!(
            resolved_for_newer, newer_logical_key,
            "resolving the newer logical backup's own uuid must return its own artifact"
        );
    }

    // ---- Active-run guard and interrupted-run reconciliation (real Postgres)

    /// Fence double that checks, at the moment it is called, that the run it
    /// fences still holds the active-restore constraint — proving the
    /// constraint is released only after fencing.
    struct AssertingFence {
        db: Arc<sea_orm::DatabaseConnection>,
        run_id: i32,
        fail: bool,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl crate::services::RestoreHelperFence for AssertingFence {
        async fn fence(
            &self,
            target: &crate::services::FenceTarget,
        ) -> Result<
            temps_providers::externalsvc::restore_helper::RestoreFenceReport,
            temps_providers::externalsvc::restore_helper::RestoreFenceError,
        > {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let run = temps_entities::restore_runs::Entity::find_by_id(self.run_id)
                .one(self.db.as_ref())
                .await
                .ok()
                .flatten()
                .map(|run| run.status);
            assert_eq!(
                run.as_deref(),
                Some("running"),
                "the run must still be active while its helpers are fenced"
            );
            if self.fail {
                return Err(
                    temps_providers::externalsvc::restore_helper::RestoreFenceError::List {
                        target_container: target.service_name.clone(),
                        reason: "daemon unreachable".to_string(),
                    },
                );
            }
            Ok(
                temps_providers::externalsvc::restore_helper::RestoreFenceReport {
                    stopped: vec!["helper-1".to_string()],
                    removed: Vec::new(),
                },
            )
        }
    }

    fn new_run(
        service_id: i32,
        backup_id: i32,
        user_id: i32,
        mode: &str,
        target_service_name: Option<&str>,
    ) -> temps_entities::restore_runs::ActiveModel {
        temps_entities::restore_runs::ActiveModel {
            id: NotSet,
            source_backup_id: Set(backup_id),
            source_service_id: Set(service_id),
            target_service_id: Set(None),
            target_service_name: Set(target_service_name.map(String::from)),
            mode: Set(mode.to_string()),
            status: Set("running".to_string()),
            phase: Set("prepare".to_string()),
            recovery_target: Set(None),
            parameter_overrides: Set(serde_json::json!({})),
            resume_token: Set(None),
            log_id: Set(uuid::Uuid::new_v4().to_string()),
            error_message: Set(None),
            attempt: Set(1),
            started_at: Set(Some(Utc::now())),
            finished_at: Set(None),
            created_by: Set(user_id),
            created_at: NotSet,
            updated_at: NotSet,
            cancel_requested_at: Set(None),
            cancel_requested_by: Set(None),
        }
    }

    #[tokio::test]
    async fn interrupted_restore_releases_the_service_only_after_fencing() {
        if Docker::connect_with_local_defaults().is_err() {
            println!("Docker not available, skipping");
            return;
        }
        let test_db = match temps_test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("TestDatabase unavailable, skipping: {e}");
                return;
            }
        };
        let db = test_db.db.clone();

        let user = temps_entities::users::ActiveModel {
            name: Set("Restore Tester".to_string()),
            email: Set("restore-tester@example.com".to_string()),
            password_hash: Set(Some("hash".to_string())),
            email_verified: Set(true),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert user");
        let source = temps_entities::s3_sources::ActiveModel {
            name: Set("restore-source".to_string()),
            bucket_name: Set("restore-bucket".to_string()),
            bucket_path: Set("/".to_string()),
            access_key_id: Set(String::new()),
            secret_key: Set(String::new()),
            region: Set("us-east-1".to_string()),
            force_path_style: Set(Some(true)),
            is_default: Set(true),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert S3 source");
        let service = temps_entities::external_services::ActiveModel {
            name: Set("orders".to_string()),
            service_type: Set("postgres".to_string()),
            status: Set("running".to_string()),
            topology: Set("standalone".to_string()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert service");
        let backup = temps_entities::backups::ActiveModel {
            id: NotSet,
            name: Set("nightly".to_string()),
            backup_id: Set(uuid::Uuid::new_v4().to_string()),
            schedule_id: Set(None),
            schedule_run_id: Set(None),
            backup_type: Set("full".to_string()),
            state: Set("completed".to_string()),
            started_at: Set(Utc::now()),
            finished_at: Set(Some(Utc::now())),
            s3_source_id: Set(source.id),
            s3_location: Set("external_services/postgres/orders/1".to_string()),
            compression_type: Set("gzip".to_string()),
            created_by: Set(user.id),
            tags: Set("[]".to_string()),
            size_bytes: Set(None),
            file_count: Set(None),
            error_message: Set(None),
            expires_at: Set(None),
            checksum: Set(None),
            metadata: Set("{}".to_string()),
        }
        .insert(db.as_ref())
        .await
        .expect("insert backup");

        // A genuinely active in-place restore.
        let active = insert_restore_run(
            db.as_ref(),
            new_run(service.id, backup.id, user.id, "in_place", None),
            Some(backup.id),
            service.id,
            true,
        )
        .await
        .expect("first in-place restore is accepted");
        update_phase(db.as_ref(), active.id, "restore")
            .await
            .expect("record restore phase");

        // A second in-place restore, or a PITR in place, is a typed 409.
        for mode in ["in_place", "pitr"] {
            let err = insert_restore_run(
                db.as_ref(),
                new_run(service.id, backup.id, user.id, mode, None),
                Some(backup.id),
                service.id,
                true,
            )
            .await
            .expect_err("a second destructive restore must conflict");
            assert!(
                matches!(
                    err,
                    RestoreError::RestoreAlreadyActive { service_id, restore_run_id }
                        if service_id == service.id && restore_run_id == active.id
                ),
                "{mode}: got {err:?}"
            );
        }
        // The unique index still backs the check if it is ever bypassed.
        let raw = new_run(service.id, backup.id, user.id, "in_place", None)
            .insert(db.as_ref())
            .await
            .expect_err("index must reject a second active in-place run");
        assert!(is_active_restore_conflict(&raw), "{raw:?}");
        // A restore into a new service does not touch the target and is allowed.
        insert_restore_run(
            db.as_ref(),
            new_run(
                service.id,
                backup.id,
                user.id,
                "new_service",
                Some("orders-copy"),
            ),
            Some(backup.id),
            service.id,
            false,
        )
        .await
        .expect("new-service restore alongside an in-place one");

        // A destructive restore onto a service that is receiving imported
        // data is refused under the same lock imports take.
        let importing = temps_entities::external_services::ActiveModel {
            name: Set("catalog".to_string()),
            service_type: Set("postgres".to_string()),
            status: Set("running".to_string()),
            topology: Set("standalone".to_string()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert importing service");
        let import = temps_entities::service_data_imports::ActiveModel {
            service_id: Set(importing.id),
            service_type: Set("postgres".to_string()),
            target_database: Set("catalog_production".to_string()),
            source_display: Set("postgres://***:***@db.example.com:5432/catalog".to_string()),
            source_database: Set("catalog".to_string()),
            status: Set("running".to_string()),
            phase: Set("transferring".to_string()),
            timeout_seconds: Set(3600),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert running data import");
        let err = insert_restore_run(
            db.as_ref(),
            new_run(importing.id, backup.id, user.id, "in_place", None),
            Some(backup.id),
            importing.id,
            true,
        )
        .await
        .expect_err("a restore must not overlap a running import");
        assert!(
            matches!(
                err,
                RestoreError::DataImportActive { service_id, data_import_run_id }
                    if service_id == importing.id && data_import_run_id == import.id
            ),
            "got {err:?}"
        );

        // Fencing fails: the run stays active and keeps blocking.
        let failing = AssertingFence {
            db: db.clone(),
            run_id: active.id,
            fail: true,
            calls: Default::default(),
        };
        let report =
            crate::services::reconcile_interrupted_restores(db.as_ref(), &[active.id], &failing)
                .await
                .expect("reconcile");
        assert_eq!(report.deferred, vec![active.id]);
        assert!(insert_restore_run(
            db.as_ref(),
            new_run(service.id, backup.id, user.id, "in_place", None),
            Some(backup.id),
            service.id,
            true,
        )
        .await
        .is_err());

        // Fencing succeeds: the run becomes interrupted, keeping its phase.
        let fence = AssertingFence {
            db: db.clone(),
            run_id: active.id,
            fail: false,
            calls: Default::default(),
        };
        let report =
            crate::services::reconcile_interrupted_restores(db.as_ref(), &[active.id], &fence)
                .await
                .expect("reconcile");
        assert_eq!(report.interrupted, vec![active.id]);
        assert_eq!(fence.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let interrupted = load_run_for_update(db.as_ref(), active.id)
            .await
            .expect("reload run");
        assert_eq!(interrupted.status, "interrupted");
        assert_eq!(interrupted.phase, "restore");
        assert!(interrupted.finished_at.is_some());
        let message = interrupted.error_message.clone().unwrap_or_default();
        assert!(
            message.contains("interrupted when Temps restarted during the data restore"),
            "{message}"
        );
        assert!(message.contains("may be partially restored"), "{message}");

        // Reconciling again (another restart) changes nothing.
        let again =
            crate::services::reconcile_interrupted_restores(db.as_ref(), &[active.id], &fence)
                .await
                .expect("reconcile again");
        assert_eq!(again, crate::services::RestoreReconcileReport::default());
        assert_eq!(fence.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let unchanged = load_run_for_update(db.as_ref(), active.id)
            .await
            .expect("reload run");
        assert_eq!(unchanged.error_message, interrupted.error_message);

        // A late outcome from a surviving worker never overwrites it.
        persist_terminal_state(db.as_ref(), active.id, &Ok(None), TERMINAL_WRITE_RETRY)
            .await
            .expect("no-op write");
        assert_eq!(
            load_run_for_update(db.as_ref(), active.id)
                .await
                .expect("reload run")
                .status,
            "interrupted"
        );

        // The service accepts a new in-place restore again.
        let next = insert_restore_run(
            db.as_ref(),
            new_run(service.id, backup.id, user.id, "in_place", None),
            Some(backup.id),
            service.id,
            true,
        )
        .await
        .expect("restore after reconciliation is accepted");

        // A worker that crashes still records a terminal state.
        let crash = RestoreError::WorkerCrashed {
            restore_run_id: next.id,
            phase: "prepare".to_string(),
            reason: "task panicked".to_string(),
        };
        persist_terminal_state(db.as_ref(), next.id, &Err(crash), TERMINAL_WRITE_RETRY)
            .await
            .expect("terminal write");
        let failed = load_run_for_update(db.as_ref(), next.id)
            .await
            .expect("reload run");
        assert_eq!(failed.status, "failed");
        assert!(failed.finished_at.is_some());
        assert!(failed
            .error_message
            .unwrap_or_default()
            .contains("stopped unexpectedly during phase 'prepare'"));
    }

    #[tokio::test]
    async fn terminal_write_is_retried_then_reported() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_errors(vec![
                sea_orm::DbErr::Custom("connection reset".into()),
                sea_orm::DbErr::Custom("connection reset".into()),
            ])
            .append_exec_results(vec![sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let fast = TerminalWriteRetry {
            attempts: 3,
            base_delay: std::time::Duration::from_millis(1),
            max_delay: std::time::Duration::from_millis(2),
        };
        persist_terminal_state(&db, 9, &Ok(None), fast)
            .await
            .expect("third attempt succeeds");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_errors(vec![
                sea_orm::DbErr::Custom("down".into()),
                sea_orm::DbErr::Custom("down".into()),
            ])
            .into_connection();
        let two = TerminalWriteRetry {
            attempts: 2,
            ..fast
        };
        let err = persist_terminal_state(&db, 9, &Ok(None), two)
            .await
            .expect_err("gives up after the configured attempts");
        assert!(matches!(err, RestoreError::Database(_)));
    }

    /// Fence double for the crash path; `fail` simulates an unreachable daemon.
    struct CrashFence {
        fail: bool,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl crate::services::RestoreHelperFence for CrashFence {
        async fn fence(
            &self,
            target: &crate::services::FenceTarget,
        ) -> Result<
            temps_providers::externalsvc::restore_helper::RestoreFenceReport,
            temps_providers::externalsvc::restore_helper::RestoreFenceError,
        > {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail {
                return Err(
                    temps_providers::externalsvc::restore_helper::RestoreFenceError::List {
                        target_container: target.service_name.clone(),
                        reason: "daemon unreachable".to_string(),
                    },
                );
            }
            Ok(Default::default())
        }
    }

    #[tokio::test]
    async fn clone_name_admission_refuses_a_registered_service_before_inserting_run() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![crashed_run_service()]])
            .into_connection();
        let mut run: temps_entities::restore_runs::ActiveModel = crashed_run_row().into();
        run.target_service_name = Set(Some("cache".to_string()));
        let txn = db.begin().await.unwrap();
        let error = lock_and_insert_restore_run(&txn, run, None, 3, false)
            .await
            .unwrap_err();
        assert!(matches!(error, RestoreError::ServiceNameConflict { .. }));
        txn.rollback().await.unwrap();
        assert!(db.into_transaction_log()[0]
            .statements()
            .iter()
            .all(|s| !s.sql.contains("INSERT")));
    }

    #[tokio::test]
    async fn clone_registration_refuses_a_run_that_no_longer_owns_the_name() {
        let mut run = crashed_run_row();
        run.target_service_name = Some("expected".to_string());
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([Vec::<temps_entities::external_services::Model>::new()])
            .append_query_results([Vec::<temps_entities::restore_runs::Model>::new()])
            .append_query_results([vec![run]])
            .into_connection();
        let enc = Arc::new(temps_core::EncryptionService::new_from_password(
            "owned-registration-fixture",
        ));
        let result = temps_providers::externalsvc::NewServiceRestoreResult {
            parameters: Default::default(),
            connection_info: "owned-fixture".to_string(),
        };
        let error = persist_new_service(
            &db,
            &enc,
            &crashed_run_service(),
            11,
            "different".to_string(),
            &result,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, RestoreError::ServiceNameConflict { .. }));
        assert!(db.into_transaction_log()[0]
            .statements()
            .iter()
            .all(|s| !s.sql.contains("INSERT")));
    }

    fn crashed_run_row() -> temps_entities::restore_runs::Model {
        temps_entities::restore_runs::Model {
            id: 11,
            source_backup_id: 1,
            source_service_id: 3,
            target_service_id: None,
            target_service_name: None,
            mode: "in_place".to_string(),
            status: "running".to_string(),
            phase: "restore".to_string(),
            recovery_target: None,
            parameter_overrides: serde_json::json!({}),
            resume_token: None,
            log_id: "log".to_string(),
            error_message: None,
            attempt: 1,
            started_at: Some(Utc::now()),
            finished_at: None,
            created_by: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            cancel_requested_at: None,
            cancel_requested_by: None,
        }
    }

    fn worker_postgres() -> temps_entities::external_services::Model {
        temps_entities::external_services::Model {
            id: 11,
            name: "orders".to_string(),
            service_type: "postgres".to_string(),
            node_id: Some(4),
            workload_mode:
                temps_entities::external_service_workload_mode::ServiceWorkloadMode::Enforce,
            ..crashed_run_service()
        }
    }

    /// ADR-055 Phase 9: a database on a worker restores by a job, in place,
    /// from a `pg_dumpall` dump. Anything else is refused with the reason.
    #[test]
    fn a_worker_database_restores_only_in_place_from_a_dump() {
        let target = worker_postgres();
        let key = "external_services/postgres/orders/u/dump.sql.gz";
        assert!(job_restore_supported(&target, key, &RestoreRequestMode::InPlace).is_ok());

        let new_service = RestoreRequestMode::NewService {
            name: "copy".into(),
            parameter_overrides: serde_json::json!({}),
        };
        match job_restore_supported(&target, key, &new_service) {
            Err(RestoreError::Validation { message }) => {
                assert!(message.contains("worker node 4"), "{message}");
                assert!(message.contains("in place"), "{message}");
            }
            other => panic!("expected a validation error, got {other:?}"),
        }

        match job_restore_supported(
            &target,
            "s3://bucket/base_000001",
            &RestoreRequestMode::InPlace,
        ) {
            Err(RestoreError::Validation { message }) => {
                assert!(message.contains(".sql.gz"), "{message}");
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// A database on the control plane never takes the job path: its
    /// restore is unchanged. One on a worker restores by a job in every
    /// workload mode, since the local restore drives this host's Docker,
    /// where its container is not.
    #[test]
    fn a_control_plane_database_keeps_the_local_restore() {
        use crate::engines::workload_job::takes_job_path;
        let mut local = worker_postgres();
        local.node_id = None;
        assert!(!takes_job_path(&local));
        let mut legacy = worker_postgres();
        legacy.workload_mode = Default::default();
        assert!(takes_job_path(&legacy));
        assert!(takes_job_path(&worker_postgres()));
    }

    /// Storage services on a worker restore in place through the node's
    /// agent, from their own backup format; another mode is refused
    /// explicitly, naming the node, never silently. (MongoDB, Redis and
    /// MariaDB run the control plane's own restores on the node:
    /// `worker_mongodb`, `worker_redis`, `worker_mariadb`.)
    #[test]
    fn a_worker_service_restores_only_in_place_from_its_own_format() {
        let mariadb = temps_entities::external_services::Model {
            service_type: "mariadb".to_string(),
            node_id: Some(4),
            ..crashed_run_service()
        };
        assert!(!crate::engines::worker_agent::takes_agent_path(&mariadb));
        assert!(worker_mariadb::takes_mariadb_path(&mariadb));

        let cases = [("rustfs", "external_services/s3/cache/u")];
        for (service_type, key) in cases {
            let target = temps_entities::external_services::Model {
                service_type: service_type.to_string(),
                node_id: Some(4),
                ..crashed_run_service()
            };
            assert!(crate::engines::worker_agent::takes_agent_path(&target));
            assert!(
                agent_restore_supported(&target, key, &RestoreRequestMode::InPlace).is_ok(),
                "{service_type}"
            );
            let new_service = RestoreRequestMode::NewService {
                name: "copy".into(),
                parameter_overrides: serde_json::json!({}),
            };
            match agent_restore_supported(&target, key, &new_service) {
                Err(RestoreError::Validation { message }) => {
                    assert!(message.contains("worker node 4"), "{message}");
                    assert!(message.contains("in place"), "{message}");
                    assert!(message.contains("new_service"), "{message}");
                }
                other => panic!("{service_type}: expected a refusal, got {other:?}"),
            }
        }
        let mariadb = temps_entities::external_services::Model {
            service_type: "mariadb".to_string(),
            node_id: Some(4),
            ..crashed_run_service()
        };
        match agent_restore_supported(
            &mariadb,
            "external_services/mongodb/x/dump.archive",
            &RestoreRequestMode::InPlace,
        ) {
            Err(RestoreError::Validation { message }) => {
                assert!(message.contains("another format"), "{message}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// A service on a worker reports in-place restore only, without asking
    /// this host's Docker; the local services keep their engines' answer.
    #[test]
    fn worker_services_restore_in_place_only() {
        let capabilities = worker_restore_capabilities();
        assert!(capabilities.restore_in_place);
        assert!(!capabilities.restore_to_new_service);
        assert!(!capabilities.pitr);
        assert!(!crate::engines::worker_agent::takes_agent_path(
            &crashed_run_service()
        ));
    }

    /// A RestoreService on a control-plane-profile server: no Docker.
    fn control_plane_restore_service(
        rows: Vec<temps_entities::external_services::Model>,
    ) -> RestoreService {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results(rows.into_iter().map(|row| vec![row]))
                .into_connection(),
        );
        let key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let enc = Arc::new(EncryptionService::new(key).unwrap());
        let handle = Arc::new(temps_core::DockerHandle::disabled(
            temps_core::PROFILE_CONTROL_PLANE,
            temps_core::CONTROL_PLANE_DOCKER_REASON,
        ));
        let dns_registry = Arc::new(temps_providers::DnsRegistry::new(db.clone()));
        let mgr = Arc::new(temps_providers::ExternalServiceManager::new_with_handle(
            db.clone(),
            enc.clone(),
            handle,
            false,
            dns_registry,
        ));
        RestoreService::new(db, mgr, enc)
    }

    /// The restore page of a database on a worker node, read from a server
    /// that runs no containers: answered without an engine, for every worker
    /// engine; in place only, and to a point in time for PostgreSQL and
    /// MariaDB (their control-plane restores run on the node). Before, the
    /// engine lookup needed this server's Docker and the page failed with a
    /// generic 500.
    #[tokio::test]
    async fn a_worker_service_reports_its_restore_options_on_a_control_plane_profile() {
        for service_type in ["postgres", "mongodb", "redis", "mariadb", "s3"] {
            let mut service = crashed_run_service();
            service.id = 5;
            service.service_type = service_type.to_string();
            service.node_id = Some(2);
            let restore = control_plane_restore_service(vec![service]);
            let (capabilities, identity) = restore
                .get_capabilities_with_identity(5)
                .await
                .unwrap_or_else(|e| panic!("{service_type}: {e}"));
            assert_eq!(identity.id, 5);
            assert!(capabilities.restore_in_place, "{service_type}");
            assert!(!capabilities.restore_to_new_service, "{service_type}");
            assert_eq!(
                capabilities.pitr,
                matches!(service_type, "postgres" | "mariadb"),
                "{service_type}"
            );
        }
    }

    /// A service whose engine this server cannot build (a control-plane
    /// service with no Docker here) gets a 503 that names it and says why,
    /// never the generic 500.
    #[tokio::test]
    async fn restore_options_that_cannot_be_read_are_a_specific_problem() {
        let mut service = crashed_run_service();
        service.id = 5;
        service.config = Some(
            EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap()
            .encrypt_string(r#"{"port":"6379","password":"p"}"#)
            .unwrap(),
        );
        // The capability lookup, then the configuration load's own reads.
        let restore = control_plane_restore_service(vec![service; 4]);
        let error = restore
            .get_capabilities_with_identity(5)
            .await
            .expect_err("no engine can answer without Docker");
        assert!(
            matches!(
                &error,
                RestoreError::CapabilitiesUnavailable { service_id: 5, service_name, .. }
                    if service_name == "cache"
            ),
            "{error}"
        );
        let rendered = error.to_string();
        let problem = temps_core::problemdetails::Problem::from(error);
        assert_eq!(problem.status_code.as_u16(), 503);
        assert!(rendered.contains("service 5 ('cache')"), "{rendered}");
        assert!(rendered.contains("not available here"), "{rendered}");
    }

    /// The origin credentials a restore of backup 4, taken from service 5,
    /// leaves in worker service 7 of `service_type`, read through a manager
    /// on a server without Docker. `origin_config` is the origin's stored
    /// config (`None`: the origin row is gone).
    async fn worker_origin_credentials(
        service_type: &str,
        backup_location: &str,
        producer: i32,
        origin_config: Option<&str>,
    ) -> Option<RestoredServiceCredentials> {
        let key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let enc = Arc::new(EncryptionService::new(key).unwrap());
        let mut origin = crashed_run_service();
        origin.id = 5;
        origin.service_type = service_type.to_string();
        origin.node_id = Some(1);
        origin.config = origin_config.map(|config| enc.encrypt_string(config).unwrap());
        let mut db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![make_producer_row(1, 4, producer)]]);
        db = if origin_config.is_some() {
            db.append_query_results(vec![vec![origin.clone()], vec![origin.clone()]])
        } else {
            db.append_query_results(vec![Vec::<temps_entities::external_services::Model>::new()])
        };
        let db = Arc::new(db.into_connection());
        let handle = Arc::new(temps_core::DockerHandle::disabled(
            temps_core::PROFILE_CONTROL_PLANE,
            temps_core::CONTROL_PLANE_DOCKER_REASON,
        ));
        let dns_registry = Arc::new(temps_providers::DnsRegistry::new(db.clone()));
        let mgr = temps_providers::ExternalServiceManager::new_with_handle(
            db.clone(),
            enc,
            handle,
            false,
            dns_registry,
        );
        let mut target = crashed_run_service();
        target.id = 7;
        target.service_type = service_type.to_string();
        target.node_id = Some(1);
        let mut backup = tracked_backup(4);
        backup.s3_location = backup_location.to_string();
        origin_credentials_to_reconcile(db.as_ref(), &mgr, &backup, &target).await
    }

    /// A restore into a worker service of another service's backup is
    /// reconciled as on the control plane: the restored data's login
    /// (Postgres: role and database too; MongoDB: the admin password)
    /// becomes the target's stored credentials.
    #[tokio::test]
    async fn a_cross_service_restore_into_a_worker_takes_the_origins_login() {
        let postgres = worker_origin_credentials(
            "postgres",
            "external_services/postgres/a/1/dump.sql.gz",
            5,
            Some(r#"{"password":"origin-pw","username":"app","database":"shop"}"#),
        )
        .await
        .expect("a Postgres dump carries its origin's roles");
        assert_eq!(postgres.password.as_deref(), Some("origin-pw"));
        assert_eq!(postgres.username.as_deref(), Some("app"));
        assert_eq!(postgres.database.as_deref(), Some("shop"));

        let mongo = worker_origin_credentials(
            "mongodb",
            "mongodb/a/1/dump.archive.gz",
            5,
            Some(r#"{"password":"origin-pw","username":"admin"}"#),
        )
        .await
        .expect("a mongodump carries admin.system.users");
        assert_eq!(mongo.password.as_deref(), Some("origin-pw"));
        assert_eq!(
            mongo.username.as_deref(),
            Some("admin"),
            "the restored users answer to the origin's root username"
        );
        assert_eq!(mongo.database, None, "the database stays the target's");
    }

    /// What carries no credentials, or is the target's own, changes none;
    /// an origin that is gone keeps the target's, as on the control plane.
    #[tokio::test]
    async fn a_restore_without_foreign_credentials_keeps_the_targets() {
        let config = Some(r#"{"password":"origin-pw"}"#);
        // A logical MariaDB dump excludes the `mysql` schema; Redis and
        // storage have no auth in their data. Neither reads the origin.
        assert!(
            worker_origin_credentials("mariadb", "mariadb/a/1/dump.sql.gz", 5, config)
                .await
                .is_none()
        );
        assert!(
            worker_origin_credentials("redis", "redis/a/1/dump.rdb.gz", 5, config)
                .await
                .is_none()
        );
        // The target's own backup.
        assert!(worker_origin_credentials(
            "postgres",
            "external_services/postgres/a/1/dump.sql.gz",
            7,
            config
        )
        .await
        .is_none());
        // The origin was deleted.
        assert!(worker_origin_credentials(
            "postgres",
            "external_services/postgres/a/1/dump.sql.gz",
            5,
            None
        )
        .await
        .is_none());
    }

    /// A restore plan names the service that produced the backup from its
    /// producer link, the origin the restore itself binds to, even when the
    /// backup's metadata records no name. Before, the plan said "an unknown
    /// service" for a backup that was linked to a live one.
    #[tokio::test]
    async fn a_plan_names_the_backups_producer_from_its_link() {
        let mut origin = crashed_run_service();
        origin.id = 5;
        origin.name = "orders-old".to_string();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![make_producer_row(1, 4, 5)]])
            .append_query_results(vec![vec![origin]])
            .into_connection();
        let mut backup = tracked_backup(4);
        backup.metadata = "{}".to_string();
        assert_eq!(
            backup_origin_service_name(&db, &backup).await.as_deref(),
            Some("orders-old")
        );

        // No producer: the name the metadata recorded, if any.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![
                Vec::<temps_entities::external_service_backups::Model>::new(),
            ])
            .into_connection();
        backup.metadata = r#"{"service_name":"orders-gone"}"#.to_string();
        assert_eq!(
            backup_origin_service_name(&db, &backup).await.as_deref(),
            Some("orders-gone")
        );
    }

    /// The plan of a restore into a worker's service describes what runs
    /// there (a job or the agent on the node), never the control plane's
    /// host-side steps, and refuses what `start_restore` refuses.
    #[test]
    fn a_worker_restore_plan_describes_the_job_on_its_node() {
        let mut database = crashed_run_service();
        database.service_type = "postgres".to_string();
        database.name = "orders".to_string();
        database.node_id = Some(2);
        let plan = worker_restore_plan(
            &database,
            "external_services/postgres/orders-old/u/dump.sql.gz",
            &RestoreRequestMode::InPlace,
            "postgres-orders",
            true,
        )
        .expect("a worker database");
        let text = plan.steps.join("\n");
        assert!(
            text.contains("restore job on worker node 2, through its agent"),
            "{text}"
        );
        assert!(text.contains("password the dump restores"), "{text}");
        assert!(
            !text.contains("on the host") && !text.contains("sidecar"),
            "{text}"
        );
        assert!(plan.refusal.is_none(), "{:?}", plan.refusal);

        // A WAL-G backup is restored on the node by the control plane's
        // WAL-G restore, which the plan describes; into a new service it is
        // refused in the plan as at start.
        let walg = worker_restore_plan(
            &database,
            "s3://backups/external_services/postgres/orders/walg",
            &RestoreRequestMode::InPlace,
            "postgres-orders",
            false,
        )
        .expect("a worker database");
        assert!(walg.refusal.is_none(), "{:?}", walg.refusal);
        assert!(
            walg.steps.join("\n").contains("wal-g backup-fetch"),
            "{:?}",
            walg.steps
        );
        let walg_copy = worker_restore_plan(
            &database,
            "s3://backups/external_services/postgres/orders/walg",
            &RestoreRequestMode::NewService {
                name: "orders-copy".to_string(),
                parameter_overrides: serde_json::Value::Null,
            },
            "postgres-orders",
            false,
        )
        .expect("a worker database");
        assert!(walg_copy
            .refusal
            .is_some_and(|r| r.contains("worker node 2") && r.contains("new_service")));
        let copy = worker_restore_plan(
            &database,
            "external_services/postgres/orders/u/dump.sql.gz",
            &RestoreRequestMode::NewService {
                name: "orders-copy".to_string(),
                parameter_overrides: serde_json::Value::Null,
            },
            "postgres-orders",
            false,
        )
        .expect("a worker database");
        assert!(copy.refusal.is_some_and(|r| r.contains("new_service")));

        let mut cache = crashed_run_service();
        cache.node_id = Some(2);
        let plan = worker_restore_plan(
            &cache,
            "external_services/redis/cache/u/dump.rdb.gz",
            &RestoreRequestMode::InPlace,
            "redis-cache",
            false,
        )
        .expect("a worker cache");
        assert!(plan.steps[0].contains("worker node 2, through its agent"));
        assert!(
            plan.steps
                .iter()
                .any(|s| s.contains("original is moved back")),
            "a worker Redis restore is the control plane's: {:?}",
            plan.steps
        );
        assert!(plan.refusal.is_none());

        // A control-plane target keeps the control plane's steps.
        database.node_id = None;
        assert!(worker_restore_plan(
            &database,
            "external_services/postgres/orders/u/dump.sql.gz",
            &RestoreRequestMode::InPlace,
            "postgres-orders",
            false,
        )
        .is_none());
    }

    /// The restore plan names what each backup is and how it is applied,
    /// for the formats worker nodes and the control plane write: a
    /// mongodump archive and a bucket mirror are not "unsupported"/"unknown".
    #[test]
    fn a_plan_classifies_every_backup_format_the_engines_write() {
        for (engine, location, strategy, format) in [
            (
                "postgres",
                "s3://b/external_services/postgres/orders/walg",
                "walg_restore",
                "walg",
            ),
            (
                "postgres",
                "external_services/postgres/orders/u/dump.sql.gz",
                "pg_dump_restore",
                "pg_dump",
            ),
            (
                "mongodb",
                "external_services/mongodb/docs/2026/01/01/u/dump.archive",
                "mongodump_restore",
                "mongodump",
            ),
            (
                "mongodb",
                "s3://b/external_services/mongodb/docs/walg",
                "walg_restore",
                "walg",
            ),
            (
                "redis",
                "external_services/redis/cache/u/dump.rdb.gz",
                "redis_rdb_restore",
                "redis_rdb",
            ),
            (
                "mariadb",
                "external_services/mariadb/shop/u/dump.sql.gz",
                "mariadb_dump_restore",
                "mariadb_dump",
            ),
            (
                "s3",
                "external_services/s3/files/2026/01/01/u",
                "s3_mirror_restore",
                "s3_mirror",
            ),
            (
                "rustfs",
                "external_services/s3/files/2026/01/01/u",
                "s3_mirror_restore",
                "s3_mirror",
            ),
            (
                "blob",
                "external_services/blob/files/u",
                "s3_mirror_restore",
                "s3_mirror",
            ),
            ("redis", "legacy-backup.tar", "unsupported", "unknown"),
            ("mongodb", "", "unsupported", "unknown"),
        ] {
            let classified = classify_restore_strategy(engine, location);
            assert_eq!(classified, strategy, "{engine} {location}");
            assert_eq!(restore_format(classified), format, "{engine} {location}");
        }
    }

    /// A mongodump archive is planned as the archive restore the MongoDB
    /// engine runs, never as a WAL-G fetch.
    #[test]
    fn a_mongodump_archive_plan_describes_mongorestore_not_walg() {
        for mode in [
            RestoreRequestMode::InPlace,
            RestoreRequestMode::NewService {
                name: "docs-copy".to_string(),
                parameter_overrides: serde_json::Value::Null,
            },
        ] {
            let (mut steps, mut destructive, mut errors) = (Vec::new(), false, Vec::new());
            build_mongodb_steps(
                "mongodump_restore",
                &mode,
                "temps-mongodb-docs",
                "external_services/mongodb/docs/u/dump.archive",
                &mut steps,
                &mut destructive,
                &mut errors,
            );
            let text = steps.join("\n");
            assert!(errors.is_empty(), "{errors:?}");
            assert!(!text.contains("wal-g"), "{text}");
            assert!(text.contains("dump.archive"), "{text}");
            assert!(
                text.contains("mongorestore --archive --gzip --drop"),
                "{text}"
            );
        }
    }

    /// On a worker, the plan says what runs on the node for each engine:
    /// the control plane's own MongoDB restore and the exact bucket mirror.
    #[test]
    fn a_worker_restore_plan_names_the_engines_restore() {
        let mut docs = crashed_run_service();
        docs.service_type = "mongodb".to_string();
        docs.name = "docs".to_string();
        docs.node_id = Some(2);
        let plan = worker_restore_plan(
            &docs,
            "external_services/mongodb/docs/u/dump.archive",
            &RestoreRequestMode::InPlace,
            "temps-mongodb-docs",
            false,
        )
        .expect("a worker MongoDB");
        let text = plan.steps.join("\n");
        assert!(
            text.contains("mongorestore --archive --gzip --drop"),
            "{text}"
        );
        assert!(!text.contains("wal-g"), "{text}");
        assert!(text.contains("control plane's MongoDB restore"), "{text}");
        assert!(text.contains("worker node 2"), "{text}");
        assert!(plan.refusal.is_none(), "{:?}", plan.refusal);
        let refused = worker_restore_plan(
            &docs,
            "external_services/mongodb/docs/u/dump.archive",
            &RestoreRequestMode::NewService {
                name: "docs-copy".to_string(),
                parameter_overrides: serde_json::Value::Null,
            },
            "temps-mongodb-docs",
            false,
        )
        .expect("a worker MongoDB")
        .refusal
        .expect("a new service is refused on a worker");
        assert!(refused.contains("Restore in place"), "{refused}");

        let mut files = crashed_run_service();
        files.service_type = "s3".to_string();
        files.name = "files".to_string();
        files.node_id = Some(2);
        let plan = worker_restore_plan(
            &files,
            "external_services/s3/files/u",
            &RestoreRequestMode::InPlace,
            "files",
            false,
        )
        .expect("a worker storage service");
        let text = plan.steps.join("\n");
        assert!(text.contains("--overwrite --remove"), "{text}");
        assert!(text.contains("deleted"), "{text}");
    }

    fn crashed_run_service() -> temps_entities::external_services::Model {
        temps_entities::external_services::Model {
            id: 3,
            name: "cache".to_string(),
            service_type: "redis".to_string(),
            version: None,
            status: "running".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            slug: None,
            config: None,
            node_id: None,
            topology: "standalone".to_string(),
            error_message: None,
            health_status: None,
            last_health_check_at: None,
            last_health_error: None,
            consecutive_health_failures: 0,
            health_metadata: None,
            metrics_enabled: false,
            default_backup_provisioned: false,
            container_name: None,
            created_by_user_id: None,
            continuous_archive_s3_source_id: None,
            continuous_archive_pinned_at: None,
            workload_mode: Default::default(),
            log_ingest_lines_per_sec: None,
            log_ingest_kib_per_sec: None,
        }
    }

    #[tokio::test]
    async fn crashed_worker_is_recorded_only_after_its_helpers_are_fenced() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![crashed_run_row()]])
            .append_query_results(vec![vec![crashed_run_service()]])
            .into_connection();
        let fence = CrashFence {
            fail: false,
            calls: Default::default(),
        };

        let recorded = settle_crashed_run(&db, 11, &fence, "task panicked".into())
            .await
            .expect("fenced, so the crash can be recorded");

        assert_eq!(fence.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(
            matches!(
                &recorded,
                RestoreError::WorkerCrashed { restore_run_id: 11, phase, .. } if phase == "restore"
            ),
            "{recorded:?}"
        );
    }

    #[tokio::test]
    async fn crashed_worker_stays_active_when_its_helpers_cannot_be_fenced() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![crashed_run_row()]])
            .append_query_results(vec![vec![crashed_run_service()]])
            .into_connection();
        let fence = CrashFence {
            fail: true,
            calls: Default::default(),
        };

        let kept = settle_crashed_run(&db, 11, &fence, "task panicked".into())
            .await
            .expect_err("an unfenced helper keeps the run active");

        let message = kept.to_string();
        assert!(message.contains("stays active"), "{message}");
        assert!(message.contains("phase 'restore'"), "{message}");
    }

    // ---- Cancellation at the pre-write safe point
}
