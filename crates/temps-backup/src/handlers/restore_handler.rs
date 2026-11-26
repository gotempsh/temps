// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP routes for the generic restore framework.
//!
//! - `GET  /external-services/{id}/restore-capabilities`
//! - `GET  /external-services/{id}/restore-runs`
//! - `POST /external-services/{id}/restore`
//! - `GET  /restore-runs/{id}`
//! - `POST /restore-runs/{id}/cancel`

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use temps_auth::{permission_guard, require_sensitive_action, Permission, RequireAuth};
use temps_core::problemdetails::{self, Problem, ProblemDetails};
use temps_core::RequestMetadata;
use temps_core::SensitiveAction;
use tracing::{error, warn};
use utoipa::{OpenApi, ToSchema};

use crate::handlers::audit::{AuditContext, RestoreRunAudit, RestoreRunCancelledAudit};
use crate::handlers::types::BackupAppState;
use crate::services::{
    MissingLinkedDatabase, PlanSourceBackup, PlanTarget, RestoreError, RestorePlan,
    RestoreRequestMode, RestoreRunSourceBackup, RestoreRunView, RestoreService, RestoredDatabase,
    RestoredDatabaseKind, RestoredDatabaseUse, RestoredDatabasesReport, RestoredDatabasesResponse,
    RestoredRole,
};
use temps_providers::externalsvc::{RecoveryTarget, RestoreCapabilities};

impl From<RestoreError> for Problem {
    fn from(error: RestoreError) -> Self {
        match error {
            RestoreError::BackupNotFound { .. }
            | RestoreError::ServiceNotFound { .. }
            | RestoreError::S3SourceNotFound { .. }
            | RestoreError::RestoreRunNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Resource Not Found")
                .with_detail(error.to_string()),
            RestoreError::ServiceNameConflict { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Service Name Conflict")
                .with_detail(error.to_string()),
            RestoreError::BackupDeleting { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Backup deletion in progress")
                .with_detail(error.to_string()),
            RestoreError::BackupNotRestorable { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Backup Not Restorable")
                .with_detail(error.to_string()),
            RestoreError::CrossServiceRestoreNotConfirmed { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_type("https://temps.sh/probs/cross-service-restore-not-confirmed")
                    .with_title("Cross-Service Restore Not Confirmed")
                    .with_detail(error.to_string())
            }
            RestoreError::RestoreAlreadyActive { restore_run_id, .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_type("https://temps.sh/probs/restore-already-active")
                    .with_title("Restore Already In Progress")
                    .with_detail(error.to_string())
                    .with_value("active_restore_run_id", restore_run_id)
            }
            RestoreError::DataImportActive {
                data_import_run_id, ..
            } => problemdetails::new(StatusCode::CONFLICT)
                .with_type("https://temps.sh/probs/data-import-active")
                .with_title("Data Import In Progress")
                .with_detail(error.to_string())
                .with_value("active_data_import_run_id", data_import_run_id),
            RestoreError::RestoreNotActive { ref status, .. } => {
                let status = status.clone();
                problemdetails::new(StatusCode::CONFLICT)
                    .with_type("https://temps.sh/probs/restore-not-active")
                    .with_title("Restore Already Finished")
                    .with_detail(error.to_string())
                    .with_value("run_status", status)
            }
            RestoreError::RestoreNotCancellable { ref phase, .. } => {
                let phase = phase.clone();
                problemdetails::new(StatusCode::CONFLICT)
                    .with_type("https://temps.sh/probs/restore-not-cancellable")
                    .with_title("Restore Cannot Be Cancelled")
                    .with_detail(error.to_string())
                    .with_value("phase", phase)
            }
            // Worker outcomes; a request only sees them if it races the
            // worker, and then the run was cancelled.
            RestoreError::RestoreCancelled { .. } | RestoreError::Cancelled { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_type("https://temps.sh/probs/restore-cancelled")
                    .with_title("Restore Cancelled")
                    .with_detail(error.to_string())
            }
            RestoreError::BackupHasNoService { .. }
            | RestoreError::Validation { .. }
            | RestoreError::UnsupportedMode { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Error")
                .with_detail(error.to_string()),
            RestoreError::CapabilitiesUnavailable { .. } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_type("https://temps.sh/probs/restore-capabilities-unavailable")
                    .with_title("Restore Options Unavailable")
                    .with_detail(error.to_string())
            }
            RestoreError::RecordRunFailed {
                service_id,
                ref backup,
                s3_source_id,
                ref source,
            } => {
                error!(service_id, s3_source_id, error = %source, "failed to record restore run");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Could not record restore run")
                    .with_detail(format!("The database rejected the restore run for service {service_id} and backup {backup} on S3 source {s3_source_id}. Check server logs for the database error."))
            }
            RestoreError::Database(_) => {
                error!(error = %error, "restore database operation failed");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Restore database operation failed")
                    .with_detail(
                        "The database could not complete the restore operation; check server logs.",
                    )
            }
            RestoreError::SnapshotLookupFailed { ref backup_id, s3_source_id, ref source } => {
                error!(backup_id, s3_source_id, error = %source, "WAL-G snapshot lookup failed");
                problemdetails::new(StatusCode::BAD_GATEWAY)
                    .with_title("Could not locate the selected WAL-G backup")
                    .with_detail(error.to_string())
            }
            RestoreError::WalgRepositoryUnavailable { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("WAL-G repository unavailable")
                .with_detail(error.to_string()),
            RestoreError::RestoreSourceCredentials { .. } => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("S3 credentials unavailable")
                .with_detail(error.to_string()),
            RestoreError::WorkerCrashed { restore_run_id, ref phase, .. } => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Restore worker stopped")
                .with_detail(format!("Restore worker for run {restore_run_id} stopped during {phase}.")),
            RestoreError::StagedRestoreStillRunning { restore_run_id, .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Restore download still active")
                .with_detail(format!("The staged download for restore run {restore_run_id} could not be confirmed stopped.")),
            RestoreError::Encryption { .. } => restore_internal_problem("Could not read or protect restore settings", &error),
            RestoreError::ExternalService { .. } => restore_internal_problem("Database service restore failed", &error),
            RestoreError::Internal { .. } => restore_internal_problem("Restore worker state is inconsistent", &error),
        }
    }
}

fn restore_internal_problem(title: &'static str, error: &RestoreError) -> Problem {
    error!(error = %error, "restore request failed internally");
    problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
        .with_title(title)
        .with_detail(format!(
            "{title}. Check the restore run and server logs for details."
        ))
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_restore_capabilities,
        list_restore_runs_for_service,
        get_restored_databases,
        start_restore,
        get_restore_run,
        cancel_restore_run,
        plan_restore,
    ),
    components(
        schemas(
            StartRestoreRequest,
            RestoreRequestMode,
            RecoveryTarget,
            RestoreCapabilities,
            RestoreRunView,
            RestoreRunSourceBackup,
            RestoreCapabilitiesResponse,
            RestorePlan,
            PlanTarget,
            PlanSourceBackup,
            RestoredDatabasesResponse,
            RestoredDatabasesReport,
            RestoredDatabase,
            RestoredDatabaseKind,
            RestoredDatabaseUse,
            RestoredRole,
            MissingLinkedDatabase,
            temps_providers::DatabaseProvisioningMode,
        )
    ),
    info(
        title = "Restore API",
        description = "Endpoints for restoring external services from S3 backups — \
        supports in-place, clone-to-new-service, and point-in-time recovery modes.",
        version = "1.0.0"
    ),
    tags(
        (name = "Restore", description = "External service restore operations")
    )
)]
pub struct RestoreApiDoc;

#[derive(Deserialize, ToSchema, Clone)]
pub struct StartRestoreRequest {
    /// DB id of the backup to restore from. Either `backup_id` or
    /// `backup_location` MUST be provided. Use `backup_id` when restoring
    /// a backup this Temps instance recorded.
    #[serde(default)]
    pub backup_id: Option<i32>,
    /// Raw S3 URL / key of the backup — used when restoring a backup
    /// discovered by S3 scan (i.e., produced by another Temps instance).
    /// Requires `backup_engine` and `s3_source_id` to also be set.
    #[serde(default)]
    pub backup_location: Option<String>,
    /// Engine of the backup when specified by `backup_location`
    /// ("postgres", "redis", "mongodb", "s3"). Ignored when `backup_id`
    /// is used — we infer from the DB row.
    #[serde(default)]
    pub backup_engine: Option<String>,
    /// S3 source the `backup_location` lives in. Ignored when `backup_id`
    /// is used.
    #[serde(default)]
    pub s3_source_id: Option<i32>,
    /// Explicit confirmation for a destructive cross-service restore.
    ///
    /// Restoring in place (or PITR in place) onto a service that did not
    /// produce the backup — or from a raw backup location whose origin is
    /// unknown — overwrites the target with another service's data. Such a
    /// request is rejected with `409 Conflict` unless this is `true`. The
    /// confirmation is recorded in the audit log. Ignored for modes that
    /// provision a new service and for same-service restores.
    #[serde(default)]
    pub confirm_cross_service: bool,
    /// Requested restore mode. See `RestoreRequestMode`.
    #[serde(flatten)]
    pub mode: RestoreRequestMode,
}

#[derive(Serialize, ToSchema, Clone)]
pub struct RestoreCapabilitiesResponse {
    /// Trait-declared capabilities.
    #[serde(flatten)]
    pub capabilities: RestoreCapabilities,
    /// Suggested name for the new service when creating a clone. Safe to
    /// pre-fill into the UI dialog; the user can edit before submitting.
    pub suggested_new_service_name: String,
}

#[utoipa::path(
    tag = "Restore",
    get,
    path = "/external-services/{id}/restore-capabilities",
    params(("id" = i32, Path, description = "External service id")),
    responses(
        (status = 200, description = "Capabilities declared by the service", body = RestoreCapabilitiesResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Service not found", body = ProblemDetails),
        (status = 503, description = "The service's engine cannot be reached to read its restore options", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn get_restore_capabilities(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);
    require_service_access(
        &app_state,
        &auth,
        id,
        Permission::BackupsRead,
        "external service",
        "restore capability lookup",
    )
    .await?;

    let (caps, service) = app_state
        .restore_service
        .get_capabilities_with_identity(id)
        .await
        .map_err(Problem::from)?;

    let suggested = RestoreService::suggest_new_service_name(&service.name);

    Ok(Json(RestoreCapabilitiesResponse {
        capabilities: caps,
        suggested_new_service_name: suggested,
    }))
}

#[utoipa::path(
    tag = "Restore",
    get,
    path = "/external-services/{id}/restore-runs",
    params(("id" = i32, Path, description = "External service id")),
    responses(
        (status = 200, description = "Recent restore runs for the service", body = Vec<RestoreRunView>),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Service not found", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn list_restore_runs_for_service(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);
    require_service_access(
        &app_state,
        &auth,
        id,
        Permission::BackupsRead,
        "external service",
        "restore history lookup",
    )
    .await?;

    let runs = app_state
        .restore_service
        .list_restore_runs_for_service(id)
        .await
        .map_err(Problem::from)?;
    let runs = filter_accessible_restore_runs(&app_state, &auth, runs).await?;

    Ok(Json(runs))
}

/// What the latest restore of a PostgreSQL service brought back
///
/// The databases and login roles the restore recorded, each restored database
/// with the linked projects that read it, and each linked database the backup
/// does not have while restored data sits unused, with a restored database the
/// link could read instead.
#[utoipa::path(
    tag = "Restore",
    get,
    path = "/external-services/{id}/restored-databases",
    params(("id" = i32, Path, description = "External service id")),
    responses(
        (status = 200, description = "What the latest restore brought back; `report` is null when it recorded nothing", body = RestoredDatabasesResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Service not found", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn get_restored_databases(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);
    require_service_access(
        &app_state,
        &auth,
        id,
        Permission::BackupsRead,
        "external service",
        "restored database lookup",
    )
    .await?;
    let visible = visible_linked_projects(
        &app_state,
        &auth,
        id,
        Permission::BackupsRead,
        "restored database lookup",
    )
    .await?;

    let report = app_state
        .restore_service
        .restored_databases(id, visible.as_ref())
        .await
        .map_err(Problem::from)?;
    Ok(Json(RestoredDatabasesResponse { report }))
}

/// Filter restore history without disclosing runs whose backup producer or
/// completed clone target is outside the caller's project permission scope.
/// Database/checker failures abort the full request rather than returning a
/// potentially incomplete authorization result.
async fn filter_accessible_restore_runs(
    app_state: &BackupAppState,
    auth: &temps_auth::AuthContext,
    runs: Vec<RestoreRunView>,
) -> Result<Vec<RestoreRunView>, Problem> {
    if runs.is_empty() {
        return Ok(runs);
    }

    let tracked_backup_ids: Vec<i32> = runs.iter().filter_map(|run| run.source_backup_id).collect();
    let producer_mappings = app_state
        .restore_service
        .backup_source_services_for_backups(&tracked_backup_ids)
        .await
        .map_err(Problem::from)?;
    let producers_by_backup: BTreeMap<i32, Vec<i32>> = producer_mappings
        .into_iter()
        .map(|mapping| (mapping.backup_id, mapping.service_ids))
        .collect();

    let mut related_service_ids = BTreeSet::new();
    for producer_service_ids in producers_by_backup.values() {
        related_service_ids.extend(producer_service_ids.iter().copied());
    }
    for run in &runs {
        if let Some(target_service_id) = run.target_service_id {
            if target_service_id != run.source_service_id {
                related_service_ids.insert(target_service_id);
            }
        }
    }
    let requested_service_ids: Vec<i32> = related_service_ids.into_iter().collect();
    let accessible_services = accessible_service_ids(
        app_state,
        auth,
        &requested_service_ids,
        Permission::BackupsRead,
        "restore history",
        "list restore runs",
    )
    .await?;

    let caller_is_admin = auth.is_admin() || auth.has_role(&temps_auth::Role::PlatformAdmin);
    let project_aware = auth.project_id().is_some() || app_state.project_access_checker.is_some();

    Ok(runs
        .into_iter()
        .filter(|run| {
            let source_allowed = restore_source_visible(
                run.source_backup_id,
                &producers_by_backup,
                &accessible_services,
                caller_is_admin,
                project_aware,
            );
            let target_allowed = run.target_service_id.is_none_or(|target_service_id| {
                target_service_id == run.source_service_id
                    || accessible_services.contains(&target_service_id)
            });
            source_allowed && target_allowed
        })
        .collect())
}

fn restore_source_visible(
    backup_id: Option<i32>,
    producers: &BTreeMap<i32, Vec<i32>>,
    accessible_services: &BTreeSet<i32>,
    caller_is_admin: bool,
    project_aware: bool,
) -> bool {
    let Some(id) = backup_id else {
        return caller_is_admin || !project_aware;
    };
    match producers.get(&id) {
        Some(ids) if !ids.is_empty() => ids.iter().all(|id| accessible_services.contains(id)),
        _ => caller_is_admin,
    }
}

// ---------------------------------------------------------------------------
// Restore authorization
// ---------------------------------------------------------------------------
//
// A restore reads one service's data and writes it into another. Both halves
// are privileged: the source half discloses the backup's contents, the target
// half destroys whatever is already there. Cross-service restore is a
// deliberate disaster-recovery feature (see `services::restore`), so the fix
// for "any caller can restore any backup into any service" is authorization on
// *both* endpoints of the copy, not a block on the copy itself.
//
// `require_service_access` (and the `linked_project_ids` /
// `deployment_token_may_access` helpers it depends on) live in
// `crate::handlers::authz` — shared with `backup_handler` and
// `pg_upgrade_handler`, which have the identical "keyed by a bare
// `service_id`, no project scoping" gap.
use crate::handlers::authz::{
    accessible_service_ids, require_service_access, require_services_access,
    visible_linked_projects,
};

#[utoipa::path(
    tag = "Restore",
    post,
    path = "/external-services/{id}/restore",
    params(("id" = i32, Path, description = "External service id (source for the restore)")),
    request_body = StartRestoreRequest,
    responses(
        (status = 202, description = "Restore run started", body = RestoreRunView),
        (status = 400, description = "Validation error", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Backup or service not found", body = ProblemDetails),
        (status = 409, description = "Conflict: a destructive cross-service restore requires explicit confirmation (`cross-service-restore-not-confirmed`), another restore is already active on this service (`restore-already-active`, with `active_restore_run_id`), data is being imported into it (`data-import-active`, with `active_data_import_run_id`), the backup is being deleted or has not completed (pending, running or failed), or a requested new-service name is already owned by a service or another active restore", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn start_restore(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<StartRestoreRequest>,
) -> Result<impl IntoResponse, Problem> {
    // Writing to an external service via a backup — gate behind both.
    permission_guard!(auth, BackupsWrite);
    match &request.mode {
        RestoreRequestMode::InPlace => permission_guard!(auth, ExternalServicesWrite),
        RestoreRequestMode::NewService { .. } => permission_guard!(auth, ExternalServicesCreate),
        RestoreRequestMode::Pitr { to_new_service, .. } => {
            if *to_new_service {
                permission_guard!(auth, ExternalServicesCreate);
            } else {
                permission_guard!(auth, ExternalServicesWrite);
            }
        }
    }

    // Resolve which backup the caller is pointing at. The URL's `{id}`
    // is the TARGET service — where the data will land. The backup
    // itself may have been produced by a completely different (or no
    // longer existing) service — cross-service restore is a supported
    // disaster-recovery path, but a destructive one must be confirmed
    // explicitly (see the origin binding below). We also require the caller
    // to be entitled to the source as well,
    // otherwise a restore is an arbitrary read of another project's data into
    // a service the caller controls. The orchestrator enforces engine
    // compatibility.
    let selector = match (request.backup_id, request.backup_location.as_ref()) {
        (Some(backup_id), _) => crate::services::BackupSelector::Id(backup_id),
        (None, Some(loc)) => {
            let engine = request.backup_engine.clone().ok_or_else(|| {
                Problem::from(RestoreError::Validation {
                    message:
                        "backup_engine is required when using backup_location (orphan restore)"
                            .into(),
                })
            })?;
            let s3_source_id = request.s3_source_id.ok_or_else(|| {
                Problem::from(RestoreError::Validation {
                    message: "s3_source_id is required when using backup_location (orphan restore)"
                        .into(),
                })
            })?;
            crate::services::BackupSelector::Location {
                location: loc.clone(),
                engine,
                s3_source_id,
            }
        }
        (None, None) => {
            return Err(Problem::from(RestoreError::Validation {
                message: "Either backup_id or backup_location must be provided".into(),
            }));
        }
    };

    // Authorize both ends before step-up verification so an unauthorized
    // cross-project caller cannot use MFA responses to distinguish restore
    // targets or backup sources they cannot access.
    let target_permission = match &request.mode {
        RestoreRequestMode::InPlace => Permission::ExternalServicesWrite,
        RestoreRequestMode::NewService { .. } => Permission::ExternalServicesCreate,
        RestoreRequestMode::Pitr { to_new_service, .. } => {
            if *to_new_service {
                Permission::ExternalServicesCreate
            } else {
                Permission::ExternalServicesWrite
            }
        }
    };
    require_service_access(
        &app_state,
        &auth,
        id,
        target_permission,
        "target service",
        "restore",
    )
    .await?;
    require_restore_source_access(&app_state, &auth, &selector, Permission::BackupsWrite).await?;

    // Bind the restore to the backup's source service identity: a
    // destructive restore onto a service that did not produce the backup must
    // be confirmed explicitly. Checked after authorization (so the origin
    // linkage is only disclosed to callers entitled to both ends) and before
    // step-up verification (so the caller is not asked for MFA for a request
    // that will be refused anyway).
    let origin_binding = app_state
        .restore_service
        .resolve_origin_binding(id, &selector)
        .await
        .map_err(Problem::from)?;
    origin_binding
        .require_confirmation(id, &selector, &request.mode, request.confirm_cross_service)
        .map_err(Problem::from)?;

    // Destructive restore modes overwrite the live service — require step-up
    // verification only after project authorization, but before any service
    // mutation or audit-identity lookup. Non-destructive modes provision a
    // fresh service and do not destroy existing data.
    if matches!(
        &request.mode,
        RestoreRequestMode::InPlace
            | RestoreRequestMode::Pitr {
                to_new_service: false,
                ..
            }
    ) {
        require_sensitive_action(
            app_state.sensitive_action_authorizer.as_ref(),
            &auth,
            SensitiveAction::RestoreExternalService { service_id: id },
        )
        .await?;
    }

    let mode_str = match &request.mode {
        RestoreRequestMode::InPlace => "in_place".to_string(),
        RestoreRequestMode::NewService { .. } => "new_service".to_string(),
        RestoreRequestMode::Pitr { .. } => "pitr".to_string(),
    };

    let target_name = match &request.mode {
        RestoreRequestMode::InPlace => None,
        RestoreRequestMode::NewService { name, .. } => Some(name.clone()),
        RestoreRequestMode::Pitr {
            to_new_service: true,
            new_service_name,
            ..
        } => new_service_name.clone(),
        RestoreRequestMode::Pitr { .. } => None,
    };

    // Load audit identity through the service before mutation. The service is
    // already authorized above, and a lookup failure must not create a
    // restore run that cannot be attributed in the audit trail.
    let target_service = app_state
        .restore_service
        .get_service_identity(id)
        .await
        .map_err(Problem::from)?;

    let run = app_state
        .restore_service
        .start_restore(id, selector, request.mode.clone(), auth.user_id())
        .await
        .map_err(Problem::from)?;

    // Fire-and-forget anonymous telemetry for PITR restores.
    if matches!(request.mode, RestoreRequestMode::Pitr { .. }) {
        app_state
            .telemetry
            .report(temps_core::telemetry::TelemetryEvent::new(
                temps_core::telemetry::TelemetryEventKind::PitrRestoreTriggered,
            ));
    }

    // Audit — the URL id is the TARGET service.
    let audit = RestoreRunAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        restore_run_id: run.id,
        source_service_id: id,
        source_service_name: target_service.name,
        service_type: target_service.service_type,
        source_backup_id: request.backup_id,
        mode: mode_str,
        target_service_name: target_name,
        backup_origin_service_ids: origin_binding.origin_service_ids.clone(),
        cross_service: origin_binding.cross_service,
        cross_service_confirmed: origin_binding.cross_service && request.confirm_cross_service,
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for restore run {}: {}",
            run.id, e
        );
    }

    Ok((StatusCode::ACCEPTED, Json(run)))
}

#[utoipa::path(
    tag = "Restore",
    get,
    path = "/restore-runs/{id}",
    params(("id" = i32, Path, description = "Restore run id")),
    responses(
        (status = 200, description = "Restore run progress", body = RestoreRunView),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Restore run not found", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn get_restore_run(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);

    let run = app_state
        .restore_service
        .get_restore_run(id)
        .await
        .map_err(Problem::from)?;
    require_restore_run_read_access(&app_state, &auth, &run, "read a restore run").await?;

    Ok(Json(run))
}

/// Authorize reading `run`: the caller must reach its target service, any
/// service it created, and every service that produced its backup. A run
/// over a raw S3 location or an ownerless (control-plane) backup is
/// administrators-only. Shared by every endpoint that returns a run.
async fn require_restore_run_read_access(
    app_state: &BackupAppState,
    auth: &temps_auth::AuthContext,
    run: &RestoreRunView,
    operation: &str,
) -> Result<(), Problem> {
    if run.source_backup_id.is_none() {
        require_ownerless_restore_admin(
            app_state,
            auth,
            &format!("{operation} that reads a raw backup location"),
        )?;
    }

    let mut related_service_ids = vec![run.source_service_id];
    if let Some(target_service_id) = run.target_service_id {
        if target_service_id != run.source_service_id {
            related_service_ids.push(target_service_id);
        }
    }
    if let Some(backup_id) = run.source_backup_id {
        let producer_service_ids = app_state
            .restore_service
            .backup_source_service_ids(backup_id)
            .await
            .map_err(Problem::from)?;
        if producer_service_ids.is_empty()
            && !auth.is_admin()
            && !auth.has_role(&temps_auth::Role::PlatformAdmin)
        {
            return Err(problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Forbidden")
                .with_detail(
                    "This restore run uses a backup that is not owned by an external service and can only be read by an administrator",
                ));
        }
        related_service_ids.extend(producer_service_ids);
    }
    require_services_access(
        app_state,
        auth,
        &related_service_ids,
        Permission::BackupsRead,
        "restore run services",
        operation,
    )
    .await?;

    Ok(())
}

#[utoipa::path(
    tag = "Restore",
    post,
    path = "/restore-runs/{id}/cancel",
    params(("id" = i32, Path, description = "Restore run id")),
    responses(
        (status = 202, description = "Cancellation recorded (or already recorded); the run stops at its next check and ends as `cancelled`", body = RestoreRunView),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Restore run not found", body = ProblemDetails),
        (status = 409, description = "Conflict: the run already finished (`restore-not-active`, with `run_status`), or it can no longer be stopped safely (`restore-not-cancellable`, with `phase`): an in-place restore has started writing data, or a new service is being registered", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn cancel_restore_run(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsWrite);

    let run = app_state
        .restore_service
        .get_restore_run(id)
        .await
        .map_err(Problem::from)?;
    require_restore_run_read_access(&app_state, &auth, &run, "cancel a restore run").await?;

    // Stopping a restore takes the same entitlement as starting one onto the
    // same service in the same mode.
    let destructive =
        crate::services::run_is_destructive(&run.mode, run.target_service_name.as_deref());
    let target_permission = if destructive {
        permission_guard!(auth, ExternalServicesWrite);
        Permission::ExternalServicesWrite
    } else {
        permission_guard!(auth, ExternalServicesCreate);
        Permission::ExternalServicesCreate
    };
    require_service_access(
        &app_state,
        &auth,
        run.source_service_id,
        target_permission,
        "target service",
        "cancel restore",
    )
    .await?;

    let service = app_state
        .restore_service
        .get_service_identity(run.source_service_id)
        .await
        .map_err(Problem::from)?;

    let cancelled = app_state
        .restore_service
        .cancel_restore_run(id, auth.user_id())
        .await
        .map_err(Problem::from)?;

    let audit = RestoreRunCancelledAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        restore_run_id: id,
        service_id: service.id,
        service_name: service.name,
        service_type: service.service_type,
        source_backup_id: run.source_backup_id,
        mode: run.mode.clone(),
        phase: run.phase.clone(),
        target_service_name: run.target_service_name.clone(),
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for cancellation of restore run {}: {}",
            id, e
        );
    }

    Ok((StatusCode::ACCEPTED, Json(cancelled)))
}

/// Authorize the *source* of a restore: what the caller is about to read.
///
/// Shared by `start_restore` and `plan_restore` so the two can never drift —
/// the plan discloses the source's location, engine and size, which is the
/// same disclosure the restore itself makes, and it takes the identical
/// caller-controlled selector.
async fn require_restore_source_access(
    app_state: &BackupAppState,
    auth: &temps_auth::AuthContext,
    selector: &crate::services::BackupSelector,
    required_permission: Permission,
) -> Result<(), Problem> {
    match selector {
        crate::services::BackupSelector::Id(backup_id) => {
            // Every service that produced this backup must be reachable by the
            // caller — reading it anywhere else discloses its contents.
            let source_service_ids = app_state
                .restore_service
                .backup_source_service_ids(*backup_id)
                .await
                .map_err(Problem::from)?;

            // Fail closed on an empty set. A backup with no
            // `external_service_backups` rows is not "a backup nobody owns, so
            // anyone may read it" — it is the control-plane backup, Temps' own
            // Postgres dump: every user row, password hash, API-key hash and
            // provider token on the instance. Iterating an empty list would
            // authorize nothing at all and let any BackupsRead/Write holder
            // read or restore the platform database into a service they
            // control.
            if source_service_ids.is_empty()
                && !auth.is_admin()
                && !auth.has_role(&temps_auth::Role::PlatformAdmin)
            {
                warn!(
                    backup_id = *backup_id,
                    user_id = ?auth.user_id(),
                    "restore authz: denied a backup with no owning service to a non-admin"
                );
                return Err(problemdetails::new(StatusCode::FORBIDDEN)
                    .with_title("Forbidden")
                    .with_detail(
                        "This backup is not owned by an external service and can only be \
                         restored by an administrator",
                    ));
            }

            require_services_access(
                app_state,
                auth,
                &source_service_ids,
                required_permission,
                "source services",
                "restore",
            )
            .await?;
        }
        crate::services::BackupSelector::Location { .. } => {
            // Orphan restore reads a caller-supplied key out of a configured
            // S3 source, so there is no project-confined source service to
            // authorize against. Under Teams this is global instance data and
            // therefore administrators-only. Plain OSS keeps its historical
            // behavior because no project boundary is configured there.
            require_ownerless_restore_admin(app_state, auth, "restore from a raw backup location")?;
        }
    }

    Ok(())
}

fn require_ownerless_restore_admin(
    app_state: &BackupAppState,
    auth: &temps_auth::AuthContext,
    operation: &str,
) -> Result<(), Problem> {
    if auth.is_admin() || auth.has_role(&temps_auth::Role::PlatformAdmin) {
        return Ok(());
    }
    if auth.project_id().is_none() && app_state.project_access_checker.is_none() {
        return Ok(());
    }
    Err(problemdetails::new(StatusCode::FORBIDDEN)
        .with_title("Insufficient Permissions")
        .with_detail(format!(
            "Only an administrator can {operation}; project-scoped callers must reference an owned backup by id"
        )))
}

#[utoipa::path(
    tag = "Restore",
    post,
    path = "/external-services/{id}/restore-plan",
    params(("id" = i32, Path, description = "Target service id")),
    request_body = StartRestoreRequest,
    responses(
        (status = 200, description = "Preview of what the restore will do", body = RestorePlan),
        (status = 400, description = "Validation error", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions (requires backups:write)", body = ProblemDetails),
        (status = 404, description = "Backup or service not found", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn plan_restore(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
    Json(request): Json<StartRestoreRequest>,
) -> Result<impl IntoResponse, Problem> {
    // Planning mutates nothing, but it is a POST that drives the restore
    // orchestrator against a target service and an arbitrary backup source,
    // so it needs the backup write tier (BackupsWrite, the same baseline the
    // restore itself requires) rather than read. It is deliberately NOT gated
    // behind ExternalServices* -- a backup operator should be able to preview
    // a restore before escalating to someone who may execute it.
    permission_guard!(auth, BackupsWrite);

    let selector = match (request.backup_id, request.backup_location.as_ref()) {
        (Some(backup_id), _) => crate::services::BackupSelector::Id(backup_id),
        (None, Some(loc)) => {
            let engine = request.backup_engine.clone().ok_or_else(|| {
                Problem::from(RestoreError::Validation {
                    message: "backup_engine is required with backup_location".into(),
                })
            })?;
            let s3_source_id = request.s3_source_id.ok_or_else(|| {
                Problem::from(RestoreError::Validation {
                    message: "s3_source_id is required with backup_location".into(),
                })
            })?;
            crate::services::BackupSelector::Location {
                location: loc.clone(),
                engine,
                s3_source_id,
            }
        }
        (None, None) => {
            return Err(Problem::from(RestoreError::Validation {
                message: "Either backup_id or backup_location must be provided".into(),
            }));
        }
    };

    // Same disclosure as the restore itself — the plan names the source's
    // location, engine and size, and confirms target compatibility — so it
    // gets the same authorization on both ends.
    require_service_access(
        &app_state,
        &auth,
        id,
        Permission::BackupsWrite,
        "target service",
        "restore plan",
    )
    .await?;
    require_restore_source_access(&app_state, &auth, &selector, Permission::BackupsWrite).await?;

    let plan = app_state
        .restore_service
        .plan_restore(id, selector, request.mode.clone())
        .await
        .map_err(Problem::from)?;

    Ok(Json(plan))
}

pub fn configure_routes() -> Router<Arc<BackupAppState>> {
    Router::new()
        .route(
            "/external-services/{id}/restore-capabilities",
            get(get_restore_capabilities),
        )
        .route(
            "/external-services/{id}/restore-runs",
            get(list_restore_runs_for_service),
        )
        .route("/external-services/{id}/restore-plan", post(plan_restore))
        .route(
            "/external-services/{id}/restored-databases",
            get(get_restored_databases),
        )
        .route("/external-services/{id}/restore", post(start_restore))
        .route("/restore-runs/{id}", get(get_restore_run))
        .route("/restore-runs/{id}/cancel", post(cancel_restore_run))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::authz::deployment_token_may_access;
    use axum::http::StatusCode;

    #[test]
    fn restore_history_checks_nullable_source_and_all_producers() {
        let producers = BTreeMap::from([(7, vec![2, 3])]);
        let services = BTreeSet::from([2, 3]);
        assert!(!restore_source_visible(
            None, &producers, &services, false, true
        ));
        assert!(restore_source_visible(
            None, &producers, &services, true, true
        ));
        assert!(restore_source_visible(
            None, &producers, &services, false, false
        ));
        assert!(restore_source_visible(
            Some(7),
            &producers,
            &services,
            false,
            true
        ));
        assert!(!restore_source_visible(
            Some(7),
            &producers,
            &BTreeSet::from([2]),
            false,
            true
        ));
        assert!(!restore_source_visible(
            Some(8),
            &producers,
            &services,
            false,
            true
        ));
    }

    /// A deployment token is minted for one project; a restore that touches a
    /// service outside it must be refused even before any plugin-provided
    /// project-access checker is consulted.
    #[test]
    fn deployment_token_confined_to_its_own_project() {
        assert!(deployment_token_may_access(7, &[3, 7]));
        assert!(!deployment_token_may_access(7, &[3, 9]));
    }

    /// A service linked to no project is reachable by no deployment token —
    /// an empty link set must never read as "unrestricted".
    #[test]
    fn deployment_token_denied_for_unlinked_service() {
        assert!(!deployment_token_may_access(7, &[]));
    }

    mod restored_databases {
        use super::super::*;
        use crate::handlers::test_support::test_state;
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use sea_orm::{DatabaseBackend, MockDatabase};
        use temps_entities::{external_services, project_services, projects, restore_runs};
        use tower::ServiceExt;

        #[derive(Default)]
        struct NoAudit;

        #[async_trait::async_trait]
        impl temps_core::AuditLogger for NoAudit {
            async fn create_audit_log(
                &self,
                _operation: &dyn temps_core::AuditOperation,
            ) -> temps_core::AuditResult<()> {
                Ok(())
            }
        }

        fn admin() -> temps_auth::AuthContext {
            let now = chrono::Utc::now();
            temps_auth::AuthContext::new_session(
                temps_entities::users::Model {
                    id: 1,
                    name: "Restore Admin".to_string(),
                    email: "restore-admin@example.test".to_string(),
                    password_hash: None,
                    email_verified: true,
                    email_verification_token: None,
                    email_verification_expires: None,
                    password_reset_token: None,
                    password_reset_expires: None,
                    must_change_password: false,
                    deleted_at: None,
                    mfa_secret: None,
                    mfa_enabled: false,
                    mfa_recovery_codes: None,
                    oidc_subject: None,
                    oidc_provider_id: None,
                    created_at: now,
                    updated_at: now,
                },
                temps_auth::Role::Admin,
            )
        }

        fn service(service_type: &str) -> external_services::Model {
            let config =
                temps_core::EncryptionService::new_from_password("pg-upgrade-handler-tests")
                    .encrypt_string(r#"{"database":"app","username":"app","password":"pw"}"#)
                    .expect("encrypt");
            external_services::Model {
                id: 5,
                name: "orders".to_string(),
                service_type: service_type.to_string(),
                topology: "standalone".to_string(),
                status: "running".to_string(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                node_id: None,
                version: None,
                slug: None,
                config: Some(config),
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
                healthcheck_interval_secs: None,
                healthcheck_start_interval_secs: None,
                healthcheck_start_period_secs: None,
                healthcheck_timeout_secs: None,
                healthcheck_retries: None,
            }
        }

        fn completed_run() -> restore_runs::Model {
            restore_runs::Model {
                id: 21,
                source_backup_id: None,
                source_service_id: 5,
                target_service_id: None,
                target_service_name: None,
                mode: "in_place".into(),
                status: "completed".into(),
                phase: "completed".into(),
                recovery_target: None,
                parameter_overrides: serde_json::json!({}),
                resume_token: None,
                log_id: "log".into(),
                error_message: None,
                attempt: 1,
                started_at: None,
                finished_at: Some(chrono::Utc::now()),
                created_by: 1,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                cancel_requested_at: None,
                cancel_requested_by: None,
                restored_login_encrypted: None,
                restored_inventory: Some(serde_json::json!({
                    "databases": ["app", "postgres", "shop_prod"],
                    "roles": [{"name": "origin-admin", "superuser": true}],
                })),
            }
        }

        fn get(auth: Option<temps_auth::AuthContext>) -> Request<Body> {
            let mut request = Request::builder()
                .uri("/external-services/5/restored-databases")
                .body(Body::empty())
                .expect("request");
            if let Some(auth) = auth {
                request.extensions_mut().insert(auth);
            }
            request
        }

        async fn send(db: MockDatabase, request: Request<Body>) -> (StatusCode, serde_json::Value) {
            let state = test_state(
                std::sync::Arc::new(db.into_connection()),
                std::sync::Arc::new(NoAudit),
                None,
            );
            let response = configure_routes()
                .with_state(state)
                .oneshot(request)
                .await
                .expect("response");
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
                .await
                .expect("body");
            (status, serde_json::from_slice(&bytes).unwrap_or_default())
        }

        #[tokio::test]
        async fn without_a_token_it_is_401() {
            let (status, _) = send(MockDatabase::new(DatabaseBackend::Postgres), get(None)).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }

        #[tokio::test]
        async fn the_latest_restore_lists_its_databases_and_users() {
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![service("postgres")]])
                .append_query_results([vec![completed_run()]])
                .append_query_results([vec![service("postgres")]])
                .append_query_results([
                    Vec::<(project_services::Model, Option<projects::Model>)>::new(),
                ]);
            let (status, body) = send(db, get(Some(admin()))).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let report = &body["report"];
            assert_eq!(report["restore_run_id"], 21);
            let kinds: Vec<_> = report["databases"]
                .as_array()
                .expect("databases")
                .iter()
                .map(|d| {
                    (
                        d["name"].as_str().unwrap_or_default(),
                        d["kind"].as_str().unwrap_or_default(),
                    )
                })
                .collect();
            assert_eq!(
                kinds,
                vec![
                    ("app", "service"),
                    ("postgres", "system"),
                    ("shop_prod", "unused")
                ]
            );
            assert_eq!(report["roles"][0]["name"], "origin-admin");
            assert_eq!(report["roles"][0]["superuser"], true);
            assert_eq!(report["missing"], serde_json::json!([]));
        }

        #[tokio::test]
        async fn another_engine_has_no_report() {
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![service("redis")]]);
            let (status, body) = send(db, get(Some(admin()))).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body, serde_json::json!({ "report": null }));
        }
    }

    fn status_for(err: RestoreError) -> StatusCode {
        let p: Problem = err.into();
        p.status_code
    }

    /// Cancelling a run that already finished is a conflict that names the
    /// run's final status, which the console renders.
    #[test]
    fn cancelling_a_finished_run_maps_to_restore_not_active() {
        let problem: Problem = RestoreError::RestoreNotActive {
            restore_run_id: 42,
            status: "completed".to_string(),
        }
        .into();
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        assert_eq!(
            problem.body.get("type").and_then(|t| t.as_str()),
            Some("https://temps.sh/probs/restore-not-active")
        );
        assert_eq!(
            problem.body.get("run_status").and_then(|s| s.as_str()),
            Some("completed")
        );
    }

    #[test]
    fn unconfirmed_cross_service_restore_maps_to_409() {
        let problem: Problem = RestoreError::CrossServiceRestoreNotConfirmed {
            target_service_id: 7,
            backup: "backup 42".to_string(),
            origin: "produced by service(s) 3".to_string(),
        }
        .into();
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        let detail = problem
            .body
            .get("detail")
            .and_then(|d| d.as_str())
            .unwrap_or_default()
            .to_string();
        assert!(detail.contains("confirm_cross_service"), "{detail}");
    }

    /// A restore past its last safe point answers 409 with a type the
    /// console recognises and the phase it is in.
    #[test]
    fn not_cancellable_restore_maps_to_409_with_phase() {
        let problem: Problem = RestoreError::RestoreNotCancellable {
            restore_run_id: 42,
            phase: "restore".to_string(),
            reason: "The restore has started writing data to the service".to_string(),
        }
        .into();
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        assert_eq!(
            problem.body.get("type").and_then(|t| t.as_str()),
            Some("https://temps.sh/probs/restore-not-cancellable")
        );
        assert_eq!(
            problem.body.get("phase").and_then(|p| p.as_str()),
            Some("restore")
        );
        let detail = problem
            .body
            .get("detail")
            .and_then(|d| d.as_str())
            .unwrap_or_default()
            .to_string();
        assert!(
            detail.contains("Restore run 42") && detail.contains("started writing data"),
            "{detail}"
        );
    }

    #[test]
    fn cancelled_restore_outcome_maps_to_409_not_500() {
        let problem: Problem = RestoreError::Cancelled {
            restore_run_id: 42,
            phase: "download".to_string(),
            message: "Restore cancelled during phase 'download'.".to_string(),
        }
        .into();
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        assert_eq!(
            problem.body.get("type").and_then(|t| t.as_str()),
            Some("https://temps.sh/probs/restore-cancelled")
        );
    }

    /// A restore onto a service that is receiving imported data is a
    /// conflict naming the import, so the client can open it.
    #[test]
    fn active_data_import_maps_to_409_naming_the_import() {
        let problem: Problem = RestoreError::DataImportActive {
            service_id: 7,
            data_import_run_id: 13,
        }
        .into();
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        assert_eq!(
            problem.body.get("type").and_then(|t| t.as_str()),
            Some("https://temps.sh/probs/data-import-active")
        );
        assert_eq!(
            problem
                .body
                .get("active_data_import_run_id")
                .and_then(|id| id.as_i64()),
            Some(13)
        );
    }

    /// A second restore onto a service that already has one active is a
    /// conflict the client can act on (attach to the running restore), not
    /// the opaque 500 the unique index used to produce.
    #[test]
    fn active_restore_conflict_maps_to_409_naming_the_run() {
        let problem: Problem = RestoreError::RestoreAlreadyActive {
            service_id: 7,
            restore_run_id: 42,
        }
        .into();
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        assert_eq!(
            problem.body.get("type").and_then(|t| t.as_str()),
            Some("https://temps.sh/probs/restore-already-active")
        );
        assert_eq!(
            problem
                .body
                .get("active_restore_run_id")
                .and_then(|id| id.as_i64()),
            Some(42)
        );
        let detail = problem
            .body
            .get("detail")
            .and_then(|d| d.as_str())
            .unwrap_or_default();
        assert!(detail.contains("run 42"), "{detail}");
        assert!(detail.contains("Service 7"), "{detail}");
    }

    #[test]
    fn record_run_failure_names_the_target_without_exposing_database_details() {
        let problem: Problem = RestoreError::RecordRunFailed {
            service_id: 7,
            backup: "service-a/backup.sql.gz".into(),
            s3_source_id: 4,
            source: sea_orm::DbErr::Custom("private database detail".into()),
        }
        .into();
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        let detail = problem
            .body
            .get("detail")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        assert!(detail.contains("service 7"));
        assert!(!detail.contains("private database detail"));
    }

    #[test]
    fn snapshot_lookup_failure_identifies_backup_and_source_without_exposing_provider_details() {
        let problem: Problem = RestoreError::SnapshotLookupFailed {
            backup_id: "backup-a".into(),
            s3_source_id: 4,
            source: crate::execution::engine_v2::BackupError::Failed {
                reason: "provider diagnostic".into(),
            },
        }
        .into();
        assert_eq!(problem.status_code, StatusCode::BAD_GATEWAY);
        let message = problem.body["detail"].as_str().unwrap_or_default();
        assert!(message.contains("backup-a") && message.contains("S3 source 4"));
        assert!(!message.contains("provider diagnostic"));
    }

    #[test]
    fn source_settings_and_repository_problems_identify_the_restore() {
        for (error, status, detail) in [
            (
                RestoreError::RestoreSourceCredentials {
                    field: "access key",
                    s3_source_id: 4,
                    run_id: 3,
                    service_id: 7,
                },
                StatusCode::INTERNAL_SERVER_ERROR,
                "access key",
            ),
            (
                RestoreError::WalgRepositoryUnavailable {
                    location: "service-a/walg".into(),
                    s3_source_id: 4,
                    run_id: 3,
                    service_id: 7,
                },
                StatusCode::BAD_REQUEST,
                "service-a/walg",
            ),
        ] {
            let problem = Problem::from(error);
            assert_eq!(problem.status_code, status);
            let message = problem.body["detail"].as_str().unwrap_or_default();
            for expected in ["S3 source 4", "restore run 3", "service 7", detail] {
                assert!(message.contains(expected), "{message}");
            }
        }
    }

    #[test]
    fn worker_crash_maps_to_500_without_leaking_details() {
        let problem: Problem = RestoreError::WorkerCrashed {
            restore_run_id: 3,
            phase: "restore".into(),
            reason: "task panicked".into(),
        }
        .into();
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn confirm_cross_service_defaults_to_false_and_parses() {
        let req: StartRestoreRequest =
            serde_json::from_str(r#"{"backup_id": 42, "mode": "in_place"}"#).unwrap();
        assert!(!req.confirm_cross_service);
        let req: StartRestoreRequest = serde_json::from_str(
            r#"{"backup_id": 42, "mode": "in_place", "confirm_cross_service": true}"#,
        )
        .unwrap();
        assert!(req.confirm_cross_service);
        assert!(matches!(req.mode, RestoreRequestMode::InPlace));
    }

    #[test]
    fn not_found_variants_map_to_404() {
        assert_eq!(
            status_for(RestoreError::BackupNotFound { backup_id: 1 }),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status_for(RestoreError::ServiceNotFound { service_id: 1 }),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status_for(RestoreError::S3SourceNotFound { s3_source_id: 1 }),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status_for(RestoreError::RestoreRunNotFound { restore_run_id: 1 }),
            StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn validation_variants_map_to_400() {
        assert_eq!(
            status_for(RestoreError::BackupHasNoService { backup_id: 1 }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status_for(RestoreError::Validation {
                message: "bad".into()
            }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status_for(RestoreError::UnsupportedMode {
                mode: "pitr".into(),
                service_type: "redis".into(),
            }),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn deleting_backup_maps_to_conflict() {
        assert_eq!(
            status_for(RestoreError::BackupDeleting { backup_id: 1 }),
            StatusCode::CONFLICT
        );
    }

    #[test]
    fn non_completed_backup_maps_to_conflict() {
        assert_eq!(
            status_for(RestoreError::BackupNotRestorable {
                backup_id: 1,
                state: "running".into(),
            }),
            StatusCode::CONFLICT
        );
    }

    #[test]
    fn internal_variants_map_to_500() {
        assert_eq!(
            status_for(RestoreError::Database(sea_orm::DbErr::Custom("x".into()))),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            status_for(RestoreError::Encryption { reason: "x".into() }),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            status_for(RestoreError::ExternalService { reason: "x".into() }),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            status_for(RestoreError::Internal { reason: "x".into() }),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn problem_detail_includes_error_message() {
        let err = RestoreError::BackupNotFound { backup_id: 42 };
        let problem: Problem = err.into();
        // Detail should carry the contextual id so users see which backup.
        let body = serde_json::to_value(&problem.body).unwrap();
        assert!(
            body["detail"].as_str().unwrap_or("").contains("42"),
            "expected backup id in detail, got: {}",
            body
        );
    }

    #[test]
    fn start_restore_request_deserializes_in_place() {
        let json = r#"{"backup_id": 7, "mode": "in_place"}"#;
        let req: StartRestoreRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.backup_id, Some(7));
        assert!(matches!(req.mode, RestoreRequestMode::InPlace));
    }

    #[test]
    fn start_restore_request_deserializes_orphan_location() {
        let json = r#"{
            "backup_location": "s3://bucket/external_services/postgres/svc/walg",
            "backup_engine": "postgres",
            "s3_source_id": 2,
            "mode": "in_place"
        }"#;
        let req: StartRestoreRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.backup_id, None);
        assert_eq!(
            req.backup_location.as_deref(),
            Some("s3://bucket/external_services/postgres/svc/walg")
        );
        assert_eq!(req.backup_engine.as_deref(), Some("postgres"));
        assert_eq!(req.s3_source_id, Some(2));
    }

    #[test]
    fn start_restore_request_deserializes_new_service() {
        let json = r#"{
            "backup_id": 7,
            "mode": "new_service",
            "name": "pg-clone",
            "parameter_overrides": {"port": "5500"}
        }"#;
        let req: StartRestoreRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.backup_id, Some(7));
        match req.mode {
            RestoreRequestMode::NewService {
                name,
                parameter_overrides,
            } => {
                assert_eq!(name, "pg-clone");
                assert_eq!(parameter_overrides["port"], "5500");
            }
            other => panic!("expected NewService, got {:?}", other),
        }
    }

    #[test]
    fn start_restore_request_deserializes_pitr_in_place() {
        let json = r#"{
            "backup_id": 7,
            "mode": "pitr",
            "to_new_service": false,
            "target": {"kind": "time", "time": "2026-04-22T10:00:00Z"}
        }"#;
        let req: StartRestoreRequest = serde_json::from_str(json).unwrap();
        match req.mode {
            RestoreRequestMode::Pitr {
                to_new_service,
                new_service_name,
                target,
            } => {
                assert!(!to_new_service);
                assert!(new_service_name.is_none());
                assert!(matches!(target, RecoveryTarget::Time { .. }));
            }
            other => panic!("expected Pitr, got {:?}", other),
        }
    }
}
