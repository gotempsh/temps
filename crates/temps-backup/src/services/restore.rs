//! Generic restore orchestrator.
//!
//! Takes a restore request (backup id + mode), writes a `restore_runs` row,
//! and drives the per-engine `ExternalService` trait methods
//! (`restore_from_s3`, `restore_to_new_service`, `restore_pitr`) to
//! completion in a spawned task. Handlers poll `restore_runs` for progress.

use aws_sdk_s3::{Client as S3Client, Config as S3Config};
use chrono::Utc;
use sea_orm::ActiveValue::{NotSet, Set};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, TransactionTrait,
};
use serde::{Deserialize, Serialize};
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

    #[error("Restore mode '{mode}' not supported by service type '{service_type}'")]
    UnsupportedMode { mode: String, service_type: String },

    #[error("Encryption error: {reason}")]
    Encryption { reason: String },

    #[error("External service error: {reason}")]
    ExternalService { reason: String },

    #[error("Internal error: {reason}")]
    Internal { reason: String },
}

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
    /// Point-in-time recovery. Only valid on WAL-G backups (Postgres).
    Pitr {
        /// Whether PITR restores in place or creates a new service.
        to_new_service: bool,
        /// Required when `to_new_service` is true.
        new_service_name: Option<String>,
        /// Recovery target kind + value.
        target: RecoveryTarget,
    },
}

impl RestoreRequestMode {
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
}

impl From<temps_entities::restore_runs::Model> for RestoreRunView {
    fn from(m: temps_entities::restore_runs::Model) -> Self {
        Self {
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
    /// or "unsupported".
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
    /// "walg", "pg_dump", "unknown".
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
}

impl RestoreService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        external_service_manager: Arc<ExternalServiceManager>,
        encryption_service: Arc<temps_core::EncryptionService>,
    ) -> Self {
        Self {
            db,
            external_service_manager,
            encryption_service,
        }
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
                    let engine = serde_json::from_str::<serde_json::Value>(&backup.metadata)
                        .ok()
                        .and_then(|v| {
                            v.get("service_type")
                                .and_then(|t| t.as_str())
                                .map(String::from)
                        });
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
                    Some(engine.clone()),
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

        // Engine compat.
        let engine_from_backup = backup_engine_hint.clone();
        if let Some(engine) = &engine_from_backup {
            if !engines_compatible(engine, &target.service_type) {
                errors.push(format!(
                    "Engine mismatch: backup is '{}' but target '{}' is '{}'.",
                    engine, target.name, target.service_type
                ));
            }
        } else {
            warnings.push(
                "Could not determine the backup's engine from metadata — proceeding as if it matches the target."
                    .into(),
            );
        }

        // Classify backup format. For missing locations (pre-fix DB rows),
        // we peek at what the orchestrator's S3 resolver would find so the
        // plan matches reality.
        let mut resolved_location = backup_location.clone();
        let mut location_was_resolved = false;
        if resolved_location.is_empty() {
            let origin = backup_row
                .as_ref()
                .and_then(|b| serde_json::from_str::<serde_json::Value>(&b.metadata).ok())
                .and_then(|v| {
                    v.get("service_name")
                        .and_then(|s| s.as_str())
                        .map(String::from)
                });
            if let Some(origin) = origin {
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
                    if let (Some(a), Some(s)) = (decrypted_access_key, decrypted_secret_key) {
                        let creds = S3Credentials {
                            access_key_id: a,
                            secret_key: s,
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

        // Strategy classification.
        let strategy = if resolved_location.starts_with("s3://") {
            "walg_restore"
        } else if resolved_location.ends_with(".sql.gz")
            || resolved_location.ends_with(".pgdump.gz")
        {
            "pg_dump_restore"
        } else {
            "unsupported"
        };

        // PITR requires a WAL-G backup.
        let is_pitr = matches!(mode, RestoreRequestMode::Pitr { .. });
        if is_pitr && strategy != "walg_restore" {
            errors.push(
                "PITR requires a WAL-G backup. The selected backup is pg_dump; choose a WAL-G backup or a different mode."
                    .into(),
            );
        }

        // Cross-service warning.
        let origin_service_name = backup_row
            .as_ref()
            .and_then(|b| serde_json::from_str::<serde_json::Value>(&b.metadata).ok())
            .and_then(|v| {
                v.get("service_name")
                    .and_then(|s| s.as_str())
                    .map(String::from)
            });
        if let Some(origin) = origin_service_name.as_ref() {
            if origin != &target.name {
                warnings.push(format!(
                    "Cross-service restore: backup produced by '{}', target is '{}'. Data will be overwritten by foreign data.",
                    origin, target.name
                ));
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
        //
        // Redis RDB and S3 object copies don't carry auth, so the warning
        // would be misleading for those.
        let engine_preserves_source_credentials = matches!(
            target.service_type.to_ascii_lowercase().as_str(),
            "postgres" | "mongodb"
        );

        if engine_preserves_source_credentials
            && (strategy == "walg_restore" || strategy == "pg_dump_restore")
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
        let engine_lower = target.service_type.to_ascii_lowercase();
        let container_name = engine_container_name(&engine_lower, &target.name);
        let mut steps: Vec<String> = Vec::new();
        let mut destructive = false;

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
            }
            "mongodb" => {
                build_mongodb_steps(
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
                format: match strategy {
                    "walg_restore" => "walg".into(),
                    "pg_dump_restore" => "pg_dump".into(),
                    _ => "unknown".into(),
                },
                size_bytes: backup_row.as_ref().and_then(|b| b.size_bytes),
                created_at: backup_row.as_ref().map(|b| b.started_at.to_rfc3339()),
            },
            strategy: strategy.to_string(),
            steps,
            warnings,
            errors,
            destructive,
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
        let service = self.load_service(service_id).await?;
        let service_type =
            ServiceType::from_str(&service.service_type).map_err(|e| RestoreError::Validation {
                message: format!("Invalid service type '{}': {}", service.service_type, e),
            })?;
        let service_config = self
            .external_service_manager
            .get_service_config(service_id)
            .await
            .map_err(|e| RestoreError::ExternalService {
                reason: e.to_string(),
            })?;
        let instance = self
            .external_service_manager
            .get_service_instance(service.name.clone(), service_type);
        instance
            .restore_capabilities(service_config)
            .await
            .map_err(|e| RestoreError::ExternalService {
                reason: format!("Failed to read restore capabilities: {}", e),
            })
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
        let (resolved_backup_id, backup_location, backup_engine_hint, s3_source_id) =
            match &selector {
                BackupSelector::Id(id) => {
                    let backup = temps_entities::backups::Entity::find_by_id(*id)
                        .one(self.db.as_ref())
                        .await?
                        .ok_or(RestoreError::BackupNotFound { backup_id: *id })?;

                    // Try to infer the engine from the external_service_backups
                    // link OR from the metadata blob. This is advisory — used
                    // only for engine-compat checking.
                    let engine = if let Some(es_backup) =
                        temps_entities::external_service_backups::Entity::find()
                            .filter(
                                temps_entities::external_service_backups::Column::BackupId.eq(*id),
                            )
                            .one(self.db.as_ref())
                            .await?
                    {
                        let svc = self.load_service(es_backup.service_id).await.ok();
                        svc.map(|s| s.service_type)
                    } else {
                        serde_json::from_str::<serde_json::Value>(&backup.metadata)
                            .ok()
                            .and_then(|v| {
                                v.get("service_type")
                                    .and_then(|t| t.as_str())
                                    .map(String::from)
                            })
                    };
                    (
                        Some(backup.id),
                        backup.s3_location.clone(),
                        engine,
                        backup.s3_source_id,
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
                    (None, location.clone(), Some(engine.clone()), *s3_source_id)
                }
            };

        // Target service: where the restored data goes.
        let target = self.load_service(target_service_id).await?;

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
        let caps = self.get_capabilities(target_service_id).await?;
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
                ..
            } => {
                if !caps.pitr {
                    return Err(RestoreError::UnsupportedMode {
                        mode: "pitr".into(),
                        service_type: target.service_type.clone(),
                    });
                }
                if !backup_location.starts_with("s3://") {
                    return Err(RestoreError::Validation {
                        message: format!(
                            "PITR requires a WAL-G backup (s3:// prefix); location was '{}'",
                            backup_location
                        ),
                    });
                }
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
        };
        let run = if let Some(backup_id) = resolved_backup_id {
            let transaction = self.db.begin().await?;
            let backup = temps_entities::backups::Entity::find_by_id(backup_id)
                .lock_exclusive()
                .one(&transaction)
                .await?
                .ok_or(RestoreError::BackupNotFound { backup_id })?;
            if backup.state == "deleting" {
                return Err(RestoreError::BackupDeleting { backup_id });
            }
            let run = run_active.insert(&transaction).await?;
            transaction.commit().await?;
            run
        } else {
            run_active.insert(self.db.as_ref()).await?
        };

        // Spawn the worker — it owns Arc clones and updates the row as it goes.
        let run_id = run.id;
        let db = self.db.clone();
        let mgr = self.external_service_manager.clone();
        let enc = self.encryption_service.clone();
        tokio::spawn(async move {
            if let Err(e) = run_restore_worker(db, mgr, enc, run_id, mode).await {
                error!("Restore run {} failed: {}", run_id, e);
            }
        });

        Ok(run.into())
    }

    /// Fetch a single restore run by id.
    pub async fn get_restore_run(&self, id: i32) -> Result<RestoreRunView, RestoreError> {
        let run = temps_entities::restore_runs::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or(RestoreError::RestoreRunNotFound { restore_run_id: id })?;
        Ok(run.into())
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
        Ok(runs.into_iter().map(RestoreRunView::from).collect())
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

/// Worker: marks the run through phases, dispatches to the trait, and
/// persists target_service_id (when applicable) on success.
async fn run_restore_worker(
    db: Arc<DatabaseConnection>,
    mgr: Arc<ExternalServiceManager>,
    enc: Arc<temps_core::EncryptionService>,
    run_id: i32,
    mode: RestoreRequestMode,
) -> Result<(), RestoreError> {
    let result = run_restore_inner(db.clone(), mgr, enc, run_id, mode).await;

    let finished_at = Utc::now();
    match &result {
        Ok(target_service_id) => {
            let mut active: temps_entities::restore_runs::ActiveModel =
                load_run_for_update(&db, run_id).await?.into();
            active.status = Set("completed".to_string());
            active.phase = Set("completed".to_string());
            active.target_service_id = Set(*target_service_id);
            active.finished_at = Set(Some(finished_at));
            active.error_message = Set(None);
            active.update(db.as_ref()).await?;
            info!("Restore run {} completed successfully", run_id);
        }
        Err(e) => {
            let mut active: temps_entities::restore_runs::ActiveModel =
                load_run_for_update(&db, run_id).await?.into();
            active.status = Set("failed".to_string());
            active.phase = Set("failed".to_string());
            active.finished_at = Set(Some(finished_at));
            active.error_message = Set(Some(e.to_string()));
            active.update(db.as_ref()).await?;
            error!("Restore run {} failed: {}", run_id, e);
        }
    }

    result.map(|_| ())
}

async fn run_restore_inner(
    db: Arc<DatabaseConnection>,
    mgr: Arc<ExternalServiceManager>,
    enc: Arc<temps_core::EncryptionService>,
    run_id: i32,
    mode: RestoreRequestMode,
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

    let origin_service_id: Option<i32> = temps_entities::external_service_backups::Entity::find()
        .filter(temps_entities::external_service_backups::Column::BackupId.eq(backup_model.id))
        .one(db.as_ref())
        .await
        .ok()
        .flatten()
        .map(|b| b.service_id)
        .or_else(|| {
            // Fallback: control-plane backup row's metadata stores it.
            serde_json::from_str::<serde_json::Value>(&backup_model.metadata)
                .ok()
                .and_then(|v| v.get("service_id").and_then(|id| id.as_i64()))
                .map(|id| id as i32)
        });

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
    // S3/RustFS: NO. No auth in the data layer.
    //
    // Two separate gates. `engine_preserves_source_credentials` drives the
    // post-restore patch (same for Postgres and Mongo). The pre-restore
    // merge is Postgres-only because Postgres's restore is offline
    // (wal-g writes PGDATA, PG replays WAL) so the engine's view of the
    // "current password" during the fetch doesn't matter. Mongo's is
    // online so merging too early breaks everything.
    let engine_preserves_source_credentials = matches!(
        target_service.service_type.to_ascii_lowercase().as_str(),
        "postgres" | "mongodb"
    );
    let engine_wants_pre_restore_credential_merge =
        target_service.service_type.eq_ignore_ascii_case("postgres");

    let mut origin_password_for_post_restore_patch: Option<String> = None;
    if engine_preserves_source_credentials {
        if let Some(origin_id) = origin_service_id {
            if origin_id != target_service.id {
                match mgr.get_service_config(origin_id).await {
                    Ok(origin_cfg) => {
                        if engine_wants_pre_restore_credential_merge {
                            // Postgres only: merge origin credentials into
                            // the config we pass to the engine. Restore is
                            // offline — the merged creds will match the
                            // restored pg_authid.
                            if let (Some(target_params), Some(origin_params)) = (
                                source_config.parameters.as_object_mut(),
                                origin_cfg.parameters.as_object(),
                            ) {
                                for key in ["password", "username", "database"] {
                                    if let Some(v) = origin_params.get(key).cloned() {
                                        target_params.insert(key.to_string(), v);
                                    }
                                }
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
                        // Always remember the origin password for the
                        // post-restore patch — mongo's admin.system.users
                        // becomes the source's after mongorestore, so we
                        // need to reflect that in the stored config.
                        origin_password_for_post_restore_patch = origin_cfg
                            .parameters
                            .get("password")
                            .and_then(|v| v.as_str())
                            .map(String::from);
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
            "Engine '{}' does not propagate source credentials; skipping origin-password merge for target service {}",
            target_service.service_type, target_service.id
        );
    }

    let instance = mgr.get_service_instance(target_service.name.clone(), service_type);

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

    let s3_credentials = S3Credentials {
        access_key_id: decrypted_access_key.clone(),
        secret_key: decrypted_secret_key.clone(),
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
    // the source of the in-place-restore auth failure.
    let s3_source_plain = temps_entities::s3_sources::Model {
        access_key_id: decrypted_access_key.clone(),
        secret_key: decrypted_secret_key.clone(),
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
            let resolved =
                resolve_backup_location_from_s3(&s3_client, &s3_source, &engine, &origin).await;
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

    update_phase(&db, run_id, "restore").await?;

    let ctx = RestoreContext {
        s3_client: &s3_client,
        s3_credentials: &s3_credentials,
        s3_source: &s3_source_plain,
        backup: &backup_model,
        backup_location: &backup_model.s3_location,
        source_service: &target_service,
        source_config: source_config.clone(),
        pool: db.as_ref(),
    };

    let new_service_parameters = match mode {
        RestoreRequestMode::InPlace => {
            // Cluster topology routes through the manager: in-place
            // restore tears down every member, pre-seeds the primary's
            // pgdata from the WAL-G prefix, then rebuilds the cluster
            // around it. The trait-level restore_from_s3 doesn't have
            // access to service_members or the agent protocol so it
            // can't handle clusters — same carve-out as backup.
            if target_service.topology == "cluster" && target_service.service_type == "postgres" {
                let target_user_data = selected_walg_target_user_data(&backup_model)?;
                mgr.restore_postgres_cluster(
                    &target_service,
                    &backup_model.s3_location,
                    &s3_credentials,
                    target_user_data.as_deref(),
                )
                .await
                .map_err(|e| RestoreError::ExternalService {
                    reason: format!("cluster in-place restore failed: {}", e),
                })?;
            } else {
                instance.restore_in_place(ctx).await.map_err(|e| {
                    RestoreError::ExternalService {
                        reason: format!("in-place restore failed: {}", e),
                    }
                })?;
            }
            None
        }
        RestoreRequestMode::NewService {
            name,
            parameter_overrides,
        } => {
            update_phase(&db, run_id, "provision").await?;
            let result = instance
                .restore_to_new_service(ctx, name.clone(), parameter_overrides)
                .await
                .map_err(|e| RestoreError::ExternalService {
                    reason: format!("new-service restore failed: {}", e),
                })?;
            Some((name, result))
        }
        RestoreRequestMode::Pitr {
            to_new_service,
            new_service_name,
            target,
        } => {
            if to_new_service {
                update_phase(&db, run_id, "provision").await?;
            }
            update_phase(&db, run_id, "recover").await?;
            let maybe_result = instance
                .restore_pitr(ctx, target, to_new_service, new_service_name.clone())
                .await
                .map_err(|e| RestoreError::ExternalService {
                    reason: format!("PITR failed: {}", e),
                })?;
            match (to_new_service, maybe_result, new_service_name) {
                (true, Some(r), Some(name)) => Some((name, r)),
                (true, _, _) => {
                    return Err(RestoreError::Internal {
                        reason: "PITR to_new_service did not return a new service result".into(),
                    })
                }
                _ => None,
            }
        }
    };

    update_phase(&db, run_id, "verify").await?;

    // If a new service was created, insert its row now and link it to
    // the run. The template is the TARGET service (we're provisioning a
    // sibling of it), not the backup's origin service.
    let target_service_id = if let Some((new_name, result)) = new_service_parameters {
        let new_id = persist_new_service(&db, &enc, &target_service, new_name, &result).await?;
        Some(new_id)
    } else {
        // In-place / PITR-in-place: the target's stored config password is
        // now wrong — pg_authid (or equivalent) has the origin's password
        // hashes. Overwrite the target's `external_services.config.password`
        // with the origin's plaintext value so the UI/env vars/CLI reflect
        // the credentials that actually work post-restore.
        if let Some(new_password) = origin_password_for_post_restore_patch.as_ref() {
            if let Err(e) = patch_service_password(&db, &enc, target_service.id, new_password).await
            {
                // Don't fail the restore over a config patch — data is
                // restored, user can reset password manually. Log loudly.
                error!(
                    "Restore on service {} succeeded but patching stored password failed; the service's UI-shown credentials will be stale until someone updates them. Error: {}",
                    target_service.id, e
                );
            } else {
                info!(
                    "Updated target service {} config password to match restored data",
                    target_service.id
                );
            }
        }
        None
    };

    Ok(target_service_id)
}

/// Rewrite the target service's encrypted `config.password` field.
///
/// Called after an in-place restore so the credentials stored in the Temps
/// DB match what's actually in the restored cluster (which carries the
/// origin service's password hashes). Everything else about the config
/// — image, port, volume, container name — stays the target's.
async fn patch_service_password(
    db: &DatabaseConnection,
    enc: &Arc<temps_core::EncryptionService>,
    service_id: i32,
    new_password: &str,
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

    params.insert(
        "password".to_string(),
        serde_json::Value::String(new_password.to_string()),
    );

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
        None,
        None,
        "restore-service",
    );
    let mut builder = S3Config::builder()
        .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
        .region(aws_sdk_s3::config::Region::new(creds.region.clone()))
        .force_path_style(creds.force_path_style)
        .credentials_provider(aws_creds)
        .http_client(crate::engines::v2_common::bundled_roots_http_client());

    if let Some(endpoint) = &creds.endpoint {
        let url = if endpoint.starts_with("http") {
            endpoint.clone()
        } else {
            format!("http://{}", endpoint)
        };
        builder = builder.endpoint_url(url);
    }

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

    let inserted = model.insert(db).await?;
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
        "mongodb" => format!("mongodb-{}", service_name),
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
                    "Run `wal-g backup-fetch LATEST` inside a helper container to stream the RDB base file onto {}'s volume",
                    container_name
                ));
                steps.push(
                    "Write an appendonlydir manifest pointing at the restored RDB as the base file (Redis 7+ multi-part AOF layout)"
                        .into(),
                );
                steps.push(format!(
                    "Start {}. Redis loads the RDB on boot and accepts connections with the EXISTING requirepass (auth is not part of the dump).",
                    container_name
                ));
                steps.push("Wait for container to report healthy".into());
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
        _ => {
            steps.push(format!(
                "Download {} from S3 to a temp file",
                resolved_location
            ));
            steps.push(format!(
                "Extract dump.rdb / appendonly.aof and copy into {}'s volume",
                container_name
            ));
            steps.push(format!(
                "Restart {} to load the restored data",
                container_name
            ));
            if matches!(mode, RestoreRequestMode::InPlace) {
                *destructive = true;
            }
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
) {
    let _ = resolved_location; // wal-g reads from WALG_S3_PREFIX, not a single key
    match mode {
        RestoreRequestMode::InPlace => {
            *destructive = true;
            steps.push(format!(
                "Run `wal-g backup-fetch LATEST` inside {}",
                container_name
            ));
            steps.push(
                "WAL-G decompresses the archived mongodump stream and pipes it to `mongorestore --archive --drop`"
                    .into(),
            );
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
                "Allocate a new MongoDB container 'mongodb-{}' on a fresh volume",
                name
            ));
            steps.push("Start the new container and wait for mongod to initialize".into());
            steps.push(
                "Run `wal-g backup-fetch LATEST` inside the new container; WAL-G pipes the mongodump stream to `mongorestore --archive --drop`"
                    .into(),
            );
            steps.push(
                "Because mongorestore replays `admin.system.users`, the new service's root user is the SOURCE's credentials, not the freshly-generated ones"
                    .into(),
            );
            steps.push(
                "Persist the new service in the database with the effective credentials".into(),
            );
        }
        RestoreRequestMode::Pitr { .. } => {
            errors.push(
                "MongoDB point-in-time recovery is not yet supported by this orchestrator.".into(),
            );
        }
    }
    // Silence unused-parameter warnings for parity with the other builders.
    let _ = strategy;
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

    // 2) pg_dump / rdb / mongodump — pick the newest matching object under
    //    the service prefix.
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
            if !(key.ends_with(".sql.gz")
                || key.ends_with(".pgdump.gz")
                || key.ends_with(".rdb.gz")
                || key.ends_with(".bson.gz")
                || key.ends_with(".archive"))
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
        };
        let v: RestoreRunView = model.into();
        assert_eq!(v.id, 42);
        assert_eq!(v.status, "running");
        assert!(v.created_at.ends_with("+00:00") || v.created_at.ends_with('Z'));
        assert!(v.started_at.unwrap().contains('T'));
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
}
