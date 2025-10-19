//! Cron Jobs API Handlers
//!
//! API endpoints for managing scheduled tasks and cron jobs

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::error_builder::ErrorBuilder;
use temps_core::problemdetails::Problem;
use tracing::info;
use utoipa::{OpenApi, ToSchema};

use crate::handlers::types::AppState;

#[derive(OpenApi)]
#[openapi(
    paths(
        get_environment_crons,
        get_cron_by_id,
        get_cron_executions
    ),
    components(
        schemas(CronInfo, CronExecutionInfo, PaginationParams)
    ),
    info(
        title = "Cron Jobs API",
        description = "API endpoints for managing scheduled tasks and cron jobs. \
        Handles creation, monitoring, and execution history of scheduled operations.",
        version = "1.0.0"
    ),
    tags(
        (name = "Crons", description = "Cron jobs management API")
    )
)]
pub struct CronApiDoc;

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/environments/{env_id}/crons",
            get(get_environment_crons),
        )
        .route(
            "/projects/{project_id}/environments/{env_id}/crons/{cron_id}",
            get(get_cron_by_id),
        )
        .route(
            "/projects/{project_id}/environments/{env_id}/crons/{cron_id}/executions",
            get(get_cron_executions),
        )
}

#[derive(Serialize, ToSchema)]
pub struct CronInfo {
    id: i32,
    project_id: i32,
    environment_id: i32,
    path: String,
    schedule: String,
    next_run: Option<String>,
    created_at: String,
    updated_at: String,
    deleted_at: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct CronExecutionInfo {
    id: i32,
    cron_id: i32,
    executed_at: String,
    url: String,
    status_code: i32,
    headers: String,
    response_time_ms: i32,
    error_message: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct PaginationParams {
    #[serde(default = "default_page")]
    page: i64,
    #[serde(default = "default_per_page")]
    per_page: i64,
}

fn default_page() -> i64 {
    1
}

fn default_per_page() -> i64 {
    20
}

/// Convert database errors to Problem Details
impl From<crate::jobs::configure_crons::CronConfigError> for Problem {
    fn from(error: crate::jobs::configure_crons::CronConfigError) -> Self {
        match error {
            crate::jobs::configure_crons::CronConfigError::DatabaseError(msg) => {
                ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .type_("https://temps.sh/probs/cron-database-error")
                    .title("Database Error")
                    .detail(&msg)
                    .build()
            }
            crate::jobs::configure_crons::CronConfigError::InvalidSchedule(msg) => {
                ErrorBuilder::new(StatusCode::BAD_REQUEST)
                    .type_("https://temps.sh/probs/invalid-cron-schedule")
                    .title("Invalid Cron Schedule")
                    .detail(&msg)
                    .build()
            }
            crate::jobs::configure_crons::CronConfigError::ConfigError(msg) => {
                ErrorBuilder::new(StatusCode::BAD_REQUEST)
                    .type_("https://temps.sh/probs/cron-config-error")
                    .title("Configuration Error")
                    .detail(&msg)
                    .build()
            }
        }
    }
}

#[utoipa::path(
    get,
    path = "/projects/{project_id}/environments/{env_id}/crons",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("env_id" = i32, Path, description = "Environment ID")
    ),
    responses(
        (status = 200, description = "List of cron jobs", body = Vec<CronInfo>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Crons"
)]
async fn get_environment_crons(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path((project_id, env_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, CronsRead);

    info!(
        "Getting cron jobs for project {} environment {}",
        project_id, env_id
    );

    let crons = app_state
        .cron_service
        .get_environment_crons(project_id, env_id)
        .await?;

    let cron_infos: Vec<CronInfo> = crons
        .into_iter()
        .map(|cron| CronInfo {
            id: cron.id,
            project_id: cron.project_id,
            environment_id: cron.environment_id,
            path: cron.path,
            schedule: cron.schedule,
            next_run: cron.next_run.map(|dt| dt.to_rfc3339()),
            created_at: cron.created_at.to_rfc3339(),
            updated_at: cron.updated_at.to_rfc3339(),
            deleted_at: cron.deleted_at.map(|dt| dt.to_rfc3339()),
        })
        .collect();

    Ok(Json(cron_infos))
}

#[utoipa::path(
    get,
    path = "/projects/{project_id}/environments/{env_id}/crons/{cron_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("env_id" = i32, Path, description = "Environment ID"),
        ("cron_id" = i32, Path, description = "Cron Job ID")
    ),
    responses(
        (status = 200, description = "Cron job details", body = CronInfo),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Cron job not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Crons"
)]
async fn get_cron_by_id(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path((project_id, env_id, cron_id)): Path<(i32, i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, CronsRead);

    info!(
        "Getting cron job {} for project {} environment {}",
        cron_id, project_id, env_id
    );

    let cron = app_state
        .cron_service
        .get_cron_by_id(project_id, env_id, cron_id)
        .await?;

    let cron_info = CronInfo {
        id: cron.id,
        project_id: cron.project_id,
        environment_id: cron.environment_id,
        path: cron.path,
        schedule: cron.schedule,
        next_run: cron.next_run.map(|dt| dt.to_rfc3339()),
        created_at: cron.created_at.to_rfc3339(),
        updated_at: cron.updated_at.to_rfc3339(),
        deleted_at: cron.deleted_at.map(|dt| dt.to_rfc3339()),
    };

    Ok(Json(cron_info))
}

#[utoipa::path(
    get,
    path = "/projects/{project_id}/environments/{env_id}/crons/{cron_id}/executions",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("env_id" = i32, Path, description = "Environment ID"),
        ("cron_id" = i32, Path, description = "Cron Job ID"),
        ("page" = Option<i64>, Query, description = "Page number (default: 1)"),
        ("per_page" = Option<i64>, Query, description = "Items per page (default: 20)")
    ),
    responses(
        (status = 200, description = "List of cron job executions", body = Vec<CronExecutionInfo>),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Cron job not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Crons"
)]
async fn get_cron_executions(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path((project_id, env_id, cron_id)): Path<(i32, i32, i32)>,
    Query(pagination): Query<PaginationParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, CronsRead);

    info!(
        "Getting executions for cron job {} (page {}, per_page {})",
        cron_id, pagination.page, pagination.per_page
    );

    let executions = app_state
        .cron_service
        .get_cron_executions(project_id, env_id, cron_id, pagination.page, pagination.per_page)
        .await?;

    let execution_infos: Vec<CronExecutionInfo> = executions
        .into_iter()
        .map(|exec| CronExecutionInfo {
            id: exec.id,
            cron_id: exec.cron_id,
            executed_at: exec.executed_at.to_rfc3339(),
            url: exec.url,
            status_code: exec.status_code,
            headers: exec.headers,
            response_time_ms: exec.response_time_ms,
            error_message: exec.error_message,
        })
        .collect();

    Ok(Json(execution_infos))
}
