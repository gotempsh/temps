//! HTTP handlers for the `pg_stat_statements` slow-query endpoint.
//!
//! # Route
//!
//! ```text
//! GET /external-services/{service_id}/pg-stat-statements/slow-queries?limit=N
//! ```
//!
//! Requires `ExternalServicesRead` permission. The caller must have access to
//! the service's parent project (same access control as every other
//! per-service endpoint in this plugin).

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use utoipa::{IntoParams, OpenApi, ToSchema};

use crate::handlers::types::AppState;
use crate::pg_stat_statements::{
    PgStatStatementsError, PgStatStatementsService, SlowQueryPage, SlowQueryRow, DEFAULT_PAGE_SIZE,
    MAX_PAGE_SIZE,
};

// ---------------------------------------------------------------------------
// Error → Problem
// ---------------------------------------------------------------------------

impl From<PgStatStatementsError> for Problem {
    fn from(error: PgStatStatementsError) -> Self {
        match error {
            PgStatStatementsError::NotAPostgresService { .. } => {
                problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
                    .with_title("Not a Postgres Service")
                    .with_detail(error.to_string())
            }
            PgStatStatementsError::ServiceNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Service Not Found")
                    .with_detail(error.to_string())
            }
            PgStatStatementsError::ExtensionNotAvailable { .. } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("Extension Not Available")
                    .with_detail(error.to_string())
            }
            PgStatStatementsError::ConnectionFailed { .. } => {
                problemdetails::new(StatusCode::BAD_GATEWAY)
                    .with_title("Connection Failed")
                    .with_detail(error.to_string())
            }
            PgStatStatementsError::ConfigurationError { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Configuration Error")
                    .with_detail(error.to_string())
            }
            PgStatStatementsError::QueryError { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Query Error")
                    .with_detail(error.to_string())
            }
            PgStatStatementsError::Validation { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Validation Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, IntoParams)]
pub struct SlowQueryParams {
    /// Page number (1-based). Defaults to 1.
    pub page: Option<u32>,
    /// Number of rows per page (1–100). Defaults to 20.
    pub page_size: Option<u32>,
}

/// Response envelope for the slow-queries list endpoint.
#[derive(Debug, Serialize, ToSchema)]
pub struct SlowQueriesResponse {
    /// Ordered list of query stats, slowest first by total_exec_time_ms.
    pub queries: Vec<SlowQueryRow>,
    /// Current page number (1-based).
    pub page: u32,
    /// Number of rows per page used for this request.
    pub page_size: u32,
    /// Total number of qualifying rows across all pages.
    pub total_count: u64,
}

// ---------------------------------------------------------------------------
// OpenAPI doc
// ---------------------------------------------------------------------------

#[derive(OpenApi)]
#[openapi(
    paths(get_slow_queries),
    components(schemas(SlowQueriesResponse, SlowQueryRow))
)]
pub struct PgStatStatementsApiDoc;

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

#[utoipa::path(
    tag = "External Services",
    get,
    path = "/external-services/{service_id}/pg-stat-statements/slow-queries",
    params(
        ("service_id" = i32, Path, description = "ID of the provisioned Postgres service"),
        SlowQueryParams,
    ),
    responses(
        (status = 200, description = "Paginated slow queries from pg_stat_statements", body = SlowQueriesResponse),
        (status = 400, description = "Invalid pagination parameters"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions (requires external_services:read)"),
        (status = 404, description = "Service not found"),
        (status = 422, description = "Service is not a Postgres service"),
        (status = 503, description = "pg_stat_statements extension not available (container restart required)"),
    ),
    security(("bearer_auth" = []))
)]
async fn get_slow_queries(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(service_id): Path<i32>,
    Query(params): Query<SlowQueryParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    let page = params.page.unwrap_or(1).max(1);
    let page_size = params
        .page_size
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE);

    let pg_stat_service = PgStatStatementsService::new(state.external_service_manager.clone());

    let (queries, total_count) = pg_stat_service
        .top_slow_queries(service_id, SlowQueryPage { page, page_size })
        .await
        .map_err(Problem::from)?;

    Ok(Json(SlowQueriesResponse {
        queries,
        page,
        page_size,
        total_count,
    }))
}

// ---------------------------------------------------------------------------
// Route registration
// ---------------------------------------------------------------------------

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new().route(
        "/external-services/{service_id}/pg-stat-statements/slow-queries",
        get(get_slow_queries),
    )
}
