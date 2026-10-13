// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP surface for importing data from an external database into a managed
//! service (see [`crate::data_import`]).
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET  | `/external-services/{id}/data-imports/availability` | can this service receive an import, and if not why |
//! | POST | `/external-services/{id}/data-imports` | start an import (202) |
//! | GET  | `/external-services/{id}/data-imports` | runs, newest first |
//! | GET  | `/external-services/{id}/data-imports/{run_id}` | one run |
//! | POST | `/external-services/{id}/data-imports/{run_id}/cancel` | stop a running import |
//!
//! The source connection string travels in the request body only. It is
//! never stored, logged, audited or returned: runs and audit records carry
//! the masked form.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::sensitive_action::require_sensitive_action;
use temps_auth::{deny_deployment_token, permission_guard, RequireAuth};
use temps_core::error_builder::ErrorBuilder;
use temps_core::problemdetails::Problem;
use temps_core::{AuditContext, AuditOperation, RequestMetadata, SensitiveAction};
use temps_entities::service_data_imports;
use tracing::error;
use utoipa::{IntoParams, OpenApi, ToSchema};

use super::types::AppState;
use crate::data_import::service::{DEFAULT_TIMEOUT_MINUTES, MAX_TIMEOUT_MINUTES};
use crate::data_import::{
    DataImportAvailability, DataImportError, DataImportRun, DataImportSpec, StartDataImport,
};

const PROBLEM_TYPE_BASE: &str = "https://temps.sh/probs/data-import";

impl From<DataImportError> for Problem {
    fn from(error: DataImportError) -> Self {
        let detail = error.to_string();
        let problem = |status: StatusCode, code: &str, title: &str| {
            ErrorBuilder::new(status)
                .type_(format!("{PROBLEM_TYPE_BASE}/{code}"))
                .title(title)
                .detail(detail.clone())
                .value("error_code", code)
        };
        match error {
            DataImportError::ServiceNotFound { .. } => problem(
                StatusCode::NOT_FOUND,
                "service-not-found",
                "Service Not Found",
            )
            .build(),
            DataImportError::RunNotFound { .. } => problem(
                StatusCode::NOT_FOUND,
                "run-not-found",
                "Data Import Not Found",
            )
            .build(),
            DataImportError::Unsupported { .. } => problem(
                StatusCode::UNPROCESSABLE_ENTITY,
                "unsupported",
                "Data Import Not Supported",
            )
            .build(),
            DataImportError::NotReady { .. } => problem(
                StatusCode::CONFLICT,
                "service-not-ready",
                "Service Not Ready",
            )
            .build(),
            DataImportError::InvalidSource { .. } => problem(
                StatusCode::BAD_REQUEST,
                "invalid-source",
                "Invalid Source Connection String",
            )
            .build(),
            DataImportError::InvalidTargetDatabase { .. } => problem(
                StatusCode::BAD_REQUEST,
                "invalid-target-database",
                "Invalid Target Database",
            )
            .build(),
            DataImportError::Validation { .. } => {
                problem(StatusCode::BAD_REQUEST, "validation", "Validation Error").build()
            }
            DataImportError::TargetNotEmpty { object_count, .. } => problem(
                StatusCode::CONFLICT,
                "target-not-empty",
                "Target Database Not Empty",
            )
            .value("object_count", object_count)
            .build(),
            DataImportError::AlreadyRunning { run_id, .. } => {
                let builder = problem(
                    StatusCode::CONFLICT,
                    "import-already-running",
                    "Import Already Running",
                );
                if run_id > 0 {
                    builder.value("active_run_id", run_id).build()
                } else {
                    builder.build()
                }
            }
            DataImportError::RestoreInProgress { restore_run_id, .. } => problem(
                StatusCode::CONFLICT,
                "restore-in-progress",
                "Restore In Progress",
            )
            .value("restore_run_id", restore_run_id)
            .build(),
            DataImportError::NotCancellable { .. } => problem(
                StatusCode::CONFLICT,
                "not-cancellable",
                "Data Import Not Cancellable",
            )
            .build(),
            DataImportError::Target { .. } => problem(
                StatusCode::BAD_GATEWAY,
                "target-unavailable",
                "Target Service Error",
            )
            .build(),
            DataImportError::Helper { .. } => problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "helper-failed",
                "Data Import Helper Failed",
            )
            .build(),
            DataImportError::DockerUnavailable(_) => problem(
                StatusCode::SERVICE_UNAVAILABLE,
                "docker-unavailable",
                "Docker Unavailable",
            )
            .build(),
            DataImportError::Database(_) => problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database-error",
                "Internal Server Error",
            )
            .build(),
        }
    }
}

/// Start importing an external database into a database of this service.
#[derive(Deserialize, ToSchema)]
pub struct StartDataImportRequest {
    /// Connection string of the database to copy, e.g.
    /// `postgres://user:password@db.example.com:5432/app?sslmode=require`.
    /// Must name the database. Never stored or returned.
    pub source_url: String,
    /// Database of this service that receives the data. Created when it does
    /// not exist; typically a project environment's database
    /// (`<project>_<environment>`).
    pub target_database: String,
    /// Drop and re-create `target_database` when it already holds data.
    #[serde(default)]
    pub replace: bool,
    /// Required when `replace` is true: must repeat `target_database`.
    #[serde(default)]
    pub confirm_target_database: Option<String>,
    /// Transfer time limit in minutes (default 60, maximum 1440).
    #[serde(default)]
    pub timeout_minutes: Option<u32>,
}

impl std::fmt::Debug for StartDataImportRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartDataImportRequest")
            .field("source_url", &"***")
            .field("target_database", &self.target_database)
            .field("replace", &self.replace)
            .field("timeout_minutes", &self.timeout_minutes)
            .finish()
    }
}

/// One data import run.
#[derive(Debug, Serialize, ToSchema)]
pub struct DataImportRunResponse {
    pub id: i32,
    pub service_id: i32,
    pub service_type: String,
    pub target_database: String,
    /// Source connection string with its credentials masked.
    pub source: String,
    pub source_database: String,
    pub replace_existing: bool,
    /// Whether a failed run leaves no imported data behind.
    pub atomic: bool,
    /// `running`, `succeeded`, `failed`, `cancelled` or `interrupted`.
    pub status: String,
    /// `preparing_target`, `transferring`, `verifying` or `finished`. Only a
    /// successful run reaches `finished`; otherwise this is where it stopped.
    pub phase: String,
    /// Why the run did not succeed, in one or two sentences.
    pub error_message: Option<String>,
    /// Last lines the transfer printed (dump/restore tool output), with every
    /// secret removed. Present for successful runs too.
    pub helper_output: Option<String>,
    /// Tables/collections in the target after a successful import.
    pub target_object_count: Option<i64>,
    pub target_size_bytes: Option<i64>,
    pub timeout_seconds: i32,
    pub created_by: Option<i32>,
    /// The user who started the run, when they still exist.
    pub started_by: Option<DataImportRunStarter>,
    pub cancel_requested: bool,
    #[schema(value_type = String, format = DateTime)]
    pub started_at: chrono::DateTime<chrono::Utc>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<service_data_imports::Model> for DataImportRunResponse {
    fn from(run: service_data_imports::Model) -> Self {
        Self {
            id: run.id,
            service_id: run.service_id,
            service_type: run.service_type,
            target_database: run.target_database,
            source: run.source_display,
            source_database: run.source_database,
            replace_existing: run.replace_existing,
            atomic: run.atomic_transfer,
            status: run.status,
            phase: run.phase,
            error_message: run.error_message,
            helper_output: run.helper_output,
            target_object_count: run.target_object_count,
            target_size_bytes: run.target_size_bytes,
            timeout_seconds: run.timeout_seconds,
            created_by: run.created_by,
            started_by: None,
            cancel_requested: run.cancel_requested_at.is_some(),
            started_at: run.started_at,
            finished_at: run.finished_at,
            created_at: run.created_at,
            updated_at: run.updated_at,
        }
    }
}

/// The user who started a data import.
#[derive(Debug, Serialize, ToSchema)]
pub struct DataImportRunStarter {
    pub user_id: i32,
    pub name: String,
    pub email: String,
}

impl From<DataImportRun> for DataImportRunResponse {
    fn from(run: DataImportRun) -> Self {
        let started_by = run.started_by.map(|starter| DataImportRunStarter {
            user_id: starter.user_id,
            name: starter.name,
            email: starter.email,
        });
        Self {
            started_by,
            ..Self::from(run.run)
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DataImportRunListResponse {
    pub items: Vec<DataImportRunResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct DataImportRunListQuery {
    /// Page number, 1-based (default 1).
    pub page: Option<u64>,
    /// Items per page (default 20, maximum 100).
    pub page_size: Option<u64>,
}

/// Whether a service can receive imported data. A service that cannot is
/// still described, with the reason, so the console can explain instead of
/// hiding the feature.
#[derive(Debug, Serialize, ToSchema)]
pub struct DataImportAvailabilityResponse {
    pub service_id: i32,
    pub service_type: String,
    /// The service's engine supports importing data.
    pub supported: bool,
    /// An import could start right now.
    pub available: bool,
    /// Why `supported` or `available` is false.
    pub reason: Option<String>,
    /// What the engine accepts, when it supports imports.
    pub spec: Option<DataImportSpec>,
    pub default_timeout_minutes: u32,
    pub max_timeout_minutes: u32,
}

impl From<DataImportAvailability> for DataImportAvailabilityResponse {
    fn from(availability: DataImportAvailability) -> Self {
        Self {
            service_id: availability.service_id,
            service_type: availability.service_type,
            supported: availability.supported,
            available: availability.available,
            reason: availability.reason,
            spec: availability.spec,
            default_timeout_minutes: DEFAULT_TIMEOUT_MINUTES,
            max_timeout_minutes: MAX_TIMEOUT_MINUTES,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DataImportStartedAudit {
    pub context: AuditContext,
    pub service_id: i32,
    pub run_id: i32,
    pub target_database: String,
    /// Masked source connection string.
    pub source: String,
    pub replace_existing: bool,
}

impl AuditOperation for DataImportStartedAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_DATA_IMPORT_STARTED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DataImportCancelledAudit {
    pub context: AuditContext,
    pub service_id: i32,
    pub run_id: i32,
    pub target_database: String,
}

impl AuditOperation for DataImportCancelledAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_DATA_IMPORT_CANCELLED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

/// Replacing a database must be confirmed by repeating its name.
fn check_replace_confirmation(request: &StartDataImportRequest) -> Result<(), DataImportError> {
    if !request.replace {
        return Ok(());
    }
    match request.confirm_target_database.as_deref() {
        Some(confirmation) if confirmation == request.target_database => Ok(()),
        _ => Err(DataImportError::Validation {
            message: format!(
                "replace drops database '{}' and everything in it; set confirm_target_database \
                 to the database name to confirm",
                request.target_database
            ),
        }),
    }
}

#[utoipa::path(
    get,
    path = "/external-services/{id}/data-imports/availability",
    tag = "External Services",
    params(("id" = i32, Path, description = "External service ID")),
    responses(
        (status = 200, description = "Whether the service can receive imported data", body = DataImportAvailabilityResponse),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Service not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn get_data_import_availability(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;
    let availability = app_state.data_import_service.availability(id).await?;
    Ok(Json(DataImportAvailabilityResponse::from(availability)))
}

#[utoipa::path(
    post,
    path = "/external-services/{id}/data-imports",
    tag = "External Services",
    params(("id" = i32, Path, description = "External service ID")),
    request_body = StartDataImportRequest,
    responses(
        (status = 202, description = "Import started; poll the run", body = DataImportRunResponse),
        (status = 400, description = "Invalid source connection string, target database, timeout or missing replace confirmation", body = temps_core::problemdetails::ProblemDetails),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions, or a deployment token", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Service not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 409, description = "The service is not running (`service-not-ready`), the target database holds data and replace was not requested (`target-not-empty`), an import into it is already running (`import-already-running`, with `active_run_id`), or the service is being restored (`restore-in-progress`)", body = temps_core::problemdetails::ProblemDetails),
        (status = 422, description = "The service's engine or topology cannot receive imports", body = temps_core::problemdetails::ProblemDetails),
        (status = 428, description = "Replacing a database requires recent MFA verification", body = temps_core::problemdetails::ProblemDetails),
        (status = 502, description = "The target service could not be inspected or prepared", body = temps_core::problemdetails::ProblemDetails),
        (status = 503, description = "This process has no Docker daemon", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn start_data_import(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<StartDataImportRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    deny_deployment_token!(auth);
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;
    check_replace_confirmation(&request)?;
    if request.replace {
        require_sensitive_action(
            app_state.sensitive_action_authorizer.as_ref(),
            &auth,
            SensitiveAction::ReplaceServiceDatabase { service_id: id },
        )
        .await?;
    }

    let run = app_state
        .data_import_service
        .start(StartDataImport {
            service_id: id,
            source_url: request.source_url,
            target_database: request.target_database,
            replace: request.replace,
            timeout_minutes: request.timeout_minutes,
            created_by: Some(auth.user_id()),
        })
        .await?;

    let audit = DataImportStartedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        service_id: id,
        run_id: run.id,
        target_database: run.target_database.clone(),
        source: run.source_display.clone(),
        replace_existing: run.replace_existing,
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!(
            service_id = id,
            run_id = run.id,
            "Failed to create audit log for data import: {}",
            e
        );
    }

    Ok((StatusCode::ACCEPTED, Json(DataImportRunResponse::from(run))))
}

#[utoipa::path(
    get,
    path = "/external-services/{id}/data-imports",
    tag = "External Services",
    params(("id" = i32, Path, description = "External service ID"), DataImportRunListQuery),
    responses(
        (status = 200, description = "Data import runs, newest first", body = DataImportRunListResponse),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Service not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn list_data_imports(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Query(query): Query<DataImportRunListQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;
    let page = query.page.unwrap_or(1).max(1);
    let page_size = query.page_size.unwrap_or(20).clamp(1, 100);
    let (runs, total) = app_state
        .data_import_service
        .list_runs(id, Some(page), Some(page_size))
        .await?;
    Ok(Json(DataImportRunListResponse {
        items: runs.into_iter().map(DataImportRunResponse::from).collect(),
        total,
        page,
        page_size,
    }))
}

#[utoipa::path(
    get,
    path = "/external-services/{id}/data-imports/{run_id}",
    tag = "External Services",
    params(
        ("id" = i32, Path, description = "External service ID"),
        ("run_id" = i32, Path, description = "Data import run ID")
    ),
    responses(
        (status = 200, description = "The data import run", body = DataImportRunResponse),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Service or run not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn get_data_import(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path((id, run_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;
    let run = app_state.data_import_service.get_run(id, run_id).await?;
    Ok(Json(DataImportRunResponse::from(run)))
}

#[utoipa::path(
    post,
    path = "/external-services/{id}/data-imports/{run_id}/cancel",
    tag = "External Services",
    params(
        ("id" = i32, Path, description = "External service ID"),
        ("run_id" = i32, Path, description = "Data import run ID")
    ),
    responses(
        (status = 202, description = "Cancellation requested; the run settles as `cancelled` shortly", body = DataImportRunResponse),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions, or a deployment token", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Service or run not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 409, description = "The run has finished, or its data has already been copied", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn cancel_data_import(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path((id, run_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    deny_deployment_token!(auth);
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;
    let run = app_state.data_import_service.cancel(id, run_id).await?;

    let audit = DataImportCancelledAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        service_id: id,
        run_id,
        target_database: run.run.target_database.clone(),
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!(
            service_id = id,
            run_id, "Failed to create audit log for data import cancel: {}", e
        );
    }

    Ok((StatusCode::ACCEPTED, Json(DataImportRunResponse::from(run))))
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/external-services/{id}/data-imports/availability",
            get(get_data_import_availability),
        )
        .route(
            "/external-services/{id}/data-imports",
            get(list_data_imports).post(start_data_import),
        )
        .route(
            "/external-services/{id}/data-imports/{run_id}",
            get(get_data_import),
        )
        .route(
            "/external-services/{id}/data-imports/{run_id}/cancel",
            post(cancel_data_import),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_data_import_availability,
        start_data_import,
        list_data_imports,
        get_data_import,
        cancel_data_import,
    ),
    components(schemas(
        StartDataImportRequest,
        DataImportRunResponse,
        DataImportRunListResponse,
        DataImportRunStarter,
        DataImportAvailabilityResponse,
        DataImportSpec,
    ))
)]
pub struct DataImportApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    fn request(replace: bool, confirm: Option<&str>) -> StartDataImportRequest {
        StartDataImportRequest {
            source_url: "postgres://u:hunter2@db.example.com/app".to_string(),
            target_database: "shop_production".to_string(),
            replace,
            confirm_target_database: confirm.map(str::to_string),
            timeout_minutes: None,
        }
    }

    #[test]
    fn replace_requires_repeating_the_database_name() {
        assert!(check_replace_confirmation(&request(false, None)).is_ok());
        assert!(check_replace_confirmation(&request(true, Some("shop_production"))).is_ok());
        for confirm in [None, Some("shop"), Some("SHOP_PRODUCTION")] {
            let error = check_replace_confirmation(&request(true, confirm)).expect_err("refused");
            assert!(matches!(error, DataImportError::Validation { .. }));
            assert!(error.to_string().contains("confirm_target_database"));
        }
    }

    #[test]
    fn request_debug_never_shows_the_source() {
        assert!(!format!("{:?}", request(false, None)).contains("hunter2"));
    }

    fn status_of(error: DataImportError) -> StatusCode {
        Problem::from(error).status_code
    }

    #[test]
    fn errors_map_to_their_status_codes() {
        let cases = [
            (
                DataImportError::ServiceNotFound { service_id: 1 },
                StatusCode::NOT_FOUND,
            ),
            (
                DataImportError::RunNotFound {
                    service_id: 1,
                    run_id: 2,
                },
                StatusCode::NOT_FOUND,
            ),
            (
                DataImportError::Unsupported {
                    service_id: 1,
                    service_type: "redis".to_string(),
                    reason: "not yet".to_string(),
                },
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                DataImportError::NotReady {
                    service_id: 1,
                    reason: "stopped".to_string(),
                },
                StatusCode::CONFLICT,
            ),
            (
                DataImportError::invalid_source("bad"),
                StatusCode::BAD_REQUEST,
            ),
            (
                DataImportError::invalid_target_database("x", "bad"),
                StatusCode::BAD_REQUEST,
            ),
            (
                DataImportError::Validation {
                    message: "bad".to_string(),
                },
                StatusCode::BAD_REQUEST,
            ),
            (
                DataImportError::TargetNotEmpty {
                    service_id: 1,
                    database: "app".to_string(),
                    object_count: 3,
                    object_noun: "table".to_string(),
                },
                StatusCode::CONFLICT,
            ),
            (
                DataImportError::AlreadyRunning {
                    service_id: 1,
                    database: "app".to_string(),
                    run_id: 9,
                },
                StatusCode::CONFLICT,
            ),
            (
                DataImportError::RestoreInProgress {
                    service_id: 1,
                    restore_run_id: 4,
                },
                StatusCode::CONFLICT,
            ),
            (
                DataImportError::NotCancellable {
                    run_id: 2,
                    status: "failed".to_string(),
                    reason: "it has already finished".to_string(),
                },
                StatusCode::CONFLICT,
            ),
            (
                DataImportError::target("orders", "connect", "refused"),
                StatusCode::BAD_GATEWAY,
            ),
            (
                DataImportError::Helper {
                    service_id: 1,
                    reason: "pull failed".to_string(),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                DataImportError::Database(sea_orm::DbErr::Custom("down".to_string())),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];
        for (error, expected) in cases {
            let label = error.to_string();
            assert_eq!(status_of(error), expected, "{label}");
        }
    }

    #[test]
    fn conflict_problems_carry_machine_readable_context() {
        let problem = Problem::from(DataImportError::AlreadyRunning {
            service_id: 1,
            database: "app".to_string(),
            run_id: 9,
        });
        let body = serde_json::to_value(&problem.body).expect("body");
        assert_eq!(body["error_code"], "import-already-running");
        assert_eq!(body["active_run_id"], 9);
    }

    #[test]
    fn run_response_exposes_only_the_masked_source() {
        let now = chrono::Utc::now();
        let response = DataImportRunResponse::from(service_data_imports::Model {
            id: 1,
            service_id: 2,
            service_type: "postgres".to_string(),
            target_database: "shop_production".to_string(),
            source_display: "postgres://***:***@db.example.com:5432/shop".to_string(),
            source_database: "shop".to_string(),
            replace_existing: false,
            atomic_transfer: true,
            status: "running".to_string(),
            phase: "transferring".to_string(),
            helper_container: Some("temps-data-import-1-abc".to_string()),
            error_message: None,
            helper_output: Some("pg_dump: warning: something harmless".to_string()),
            target_object_count: None,
            target_size_bytes: None,
            timeout_seconds: 3600,
            created_by: Some(1),
            cancel_requested_at: Some(now),
            started_at: now,
            finished_at: None,
            created_at: now,
            updated_at: now,
        });
        let json = serde_json::to_string(&response).expect("json");
        assert!(json.contains("postgres://***:***@db.example.com:5432/shop"));
        assert!(
            !json.contains("helper_container"),
            "internal container names stay internal"
        );
        assert!(response.cancel_requested);
        assert!(response.atomic);
        assert_eq!(
            response.helper_output.as_deref(),
            Some("pg_dump: warning: something harmless")
        );
        assert!(response.started_by.is_none());
    }

    #[test]
    fn run_response_names_who_started_the_run() {
        let now = chrono::Utc::now();
        let model = service_data_imports::Model {
            id: 3,
            service_id: 2,
            service_type: "redis".to_string(),
            target_database: "storefront_production".to_string(),
            source_display: "redis://***:***@cache.example.com:6379/0".to_string(),
            source_database: "0".to_string(),
            replace_existing: false,
            atomic_transfer: false,
            status: "succeeded".to_string(),
            phase: "finished".to_string(),
            helper_container: None,
            error_message: None,
            helper_output: None,
            target_object_count: Some(12),
            target_size_bytes: None,
            timeout_seconds: 3600,
            created_by: Some(7),
            cancel_requested_at: None,
            started_at: now,
            finished_at: Some(now),
            created_at: now,
            updated_at: now,
        };
        let response = DataImportRunResponse::from(DataImportRun {
            run: model,
            started_by: Some(crate::data_import::RunStarter {
                user_id: 7,
                name: "Ada".to_string(),
                email: "ada@example.com".to_string(),
            }),
        });
        let starter = response.started_by.expect("starter");
        assert_eq!((starter.user_id, starter.name.as_str()), (7, "Ada"));
        assert_eq!(response.target_object_count, Some(12));
    }
}
