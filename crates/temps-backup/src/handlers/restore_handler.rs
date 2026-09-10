// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP routes for the generic restore framework.
//!
//! - `GET  /external-services/{id}/restore-capabilities`
//! - `GET  /external-services/{id}/restore-runs`
//! - `POST /external-services/{id}/restore`
//! - `GET  /restore-runs/{id}`

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

use crate::handlers::audit::{AuditContext, RestoreRunAudit};
use crate::handlers::types::BackupAppState;
use crate::services::{
    PlanSourceBackup, PlanTarget, RestoreError, RestorePlan, RestoreRequestMode, RestoreRunView,
    RestoreService,
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
            RestoreError::BackupDeleting { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Backup deletion in progress")
                .with_detail(error.to_string()),
            RestoreError::BackupHasNoService { .. }
            | RestoreError::Validation { .. }
            | RestoreError::UnsupportedMode { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Error")
                .with_detail(error.to_string()),
            internal_error @ (RestoreError::Database(_)
            | RestoreError::Encryption { .. }
            | RestoreError::ExternalService { .. }
            | RestoreError::Internal { .. }) => {
                error!(error = %internal_error, "restore request failed internally");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail("The restore operation could not be completed")
            }
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_restore_capabilities,
        list_restore_runs_for_service,
        start_restore,
        get_restore_run,
        plan_restore,
    ),
    components(
        schemas(
            StartRestoreRequest,
            RestoreRequestMode,
            RecoveryTarget,
            RestoreCapabilities,
            RestoreRunView,
            RestoreCapabilitiesResponse,
            RestorePlan,
            PlanTarget,
            PlanSourceBackup,
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

    let positive_backup_ids: Vec<i32> = runs
        .iter()
        .filter_map(|run| (run.source_backup_id > 0).then_some(run.source_backup_id))
        .collect();
    let producer_mappings = app_state
        .restore_service
        .backup_source_services_for_backups(&positive_backup_ids)
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
            let source_allowed = if run.source_backup_id > 0 {
                match producers_by_backup.get(&run.source_backup_id) {
                    Some(producer_service_ids) if !producer_service_ids.is_empty() => {
                        producer_service_ids
                            .iter()
                            .all(|service_id| accessible_services.contains(service_id))
                    }
                    Some(_) | None => caller_is_admin,
                }
            } else {
                caller_is_admin || !project_aware
            };
            let target_allowed = run.target_service_id.is_none_or(|target_service_id| {
                target_service_id == run.source_service_id
                    || accessible_services.contains(&target_service_id)
            });
            source_allowed && target_allowed
        })
        .collect())
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
    // longer existing) service, so we do NOT validate source/target
    // linkage here — cross-service restore is a supported disaster-recovery
    // path. We *do* require the caller to be entitled to the source as well,
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
        source_backup_id: request.backup_id.unwrap_or(0),
        mode: mode_str,
        target_service_name: target_name,
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

    if run.source_backup_id <= 0 {
        require_ownerless_restore_admin(&app_state, &auth, "read a raw-location restore run")?;
    }

    let mut related_service_ids = vec![run.source_service_id];
    if let Some(target_service_id) = run.target_service_id {
        if target_service_id != run.source_service_id {
            related_service_ids.push(target_service_id);
        }
    }
    if run.source_backup_id > 0 {
        let producer_service_ids = app_state
            .restore_service
            .backup_source_service_ids(run.source_backup_id)
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
        &app_state,
        &auth,
        &related_service_ids,
        Permission::BackupsRead,
        "restore run services",
        "read restore run",
    )
    .await?;

    Ok(Json(run))
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
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
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
    // Planning is read-only: only needs BackupsRead. Don't gate behind
    // ExternalServices* — users should be able to preview a restore before
    // deciding whether to escalate to someone with write perms.
    permission_guard!(auth, BackupsRead);

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
        Permission::BackupsRead,
        "target service",
        "restore plan",
    )
    .await?;
    require_restore_source_access(&app_state, &auth, &selector, Permission::BackupsRead).await?;

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
        .route("/external-services/{id}/restore", post(start_restore))
        .route("/restore-runs/{id}", get(get_restore_run))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::authz::deployment_token_may_access;
    use axum::http::StatusCode;

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

    fn status_for(err: RestoreError) -> StatusCode {
        let p: Problem = err.into();
        p.status_code
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
