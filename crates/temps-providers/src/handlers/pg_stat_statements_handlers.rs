//! HTTP handlers for the `pg_stat_statements` slow-query endpoint.
//!
//! # Route
//!
//! ```text
//! GET /external-services/{service_id}/pg-stat-statements/slow-queries?page=N&page_size=N&sort_by=...&sort_order=...
//! ```
//!
//! Requires `ExternalServicesRead` permission. The caller must have access to
//! the service's parent project (same access control as every other
//! per-service endpoint in this plugin).

use std::sync::Arc;

use axum::Extension;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::{AuditContext, AuditOperation, RequestMetadata};
use tracing::error;
use utoipa::{IntoParams, OpenApi, ToSchema};

use crate::handlers::types::AppState;
use crate::pg_stat_statements::{
    PgStatStatementsError, PgStatStatementsService, SlowQueryPage, SlowQueryRow, SlowQuerySortKey,
    SortOrder, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE,
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
            PgStatStatementsError::ClusteredServiceNotSupported { .. } => {
                problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
                    .with_title("Clustered Service Not Supported")
                    .with_detail(error.to_string())
            }
            PgStatStatementsError::RestartFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Restart Failed")
                    .with_detail(error.to_string())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Audit
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct PgStatStatementsEnabledAudit {
    context: AuditContext,
    service_id: i32,
    service_name: String,
}

impl AuditOperation for PgStatStatementsEnabledAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_PG_STAT_STATEMENTS_ENABLED".to_string()
    }

    fn user_id(&self) -> i32 {
        self.context.user_id
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {}", e))
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
    /// Column to sort by: one of `calls`, `total_exec_time_ms`,
    /// `mean_exec_time_ms`, `rows`, `cache_hit_ratio`. Defaults to
    /// `mean_exec_time_ms`. Applied server-side so ordering stays
    /// consistent across pages.
    pub sort_by: Option<String>,
    /// Sort direction: `asc` or `desc`. Defaults to `desc`.
    pub sort_order: Option<String>,
}

/// Response envelope for the slow-queries list endpoint.
#[derive(Debug, Serialize, ToSchema)]
pub struct SlowQueriesResponse {
    /// Ordered list of query stats, slowest first by mean_exec_time_ms.
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

/// Response for the enable pg_stat_statements endpoint.
#[derive(Debug, Serialize, ToSchema)]
pub struct EnablePgStatStatementsResponse {
    /// Human-readable message confirming the action.
    pub message: String,
}

#[derive(OpenApi)]
#[openapi(
    paths(get_slow_queries, enable_pg_stat_statements),
    components(schemas(SlowQueriesResponse, SlowQueryRow, EnablePgStatStatementsResponse))
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
        (status = 400, description = "Invalid pagination or sort parameters"),
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

    let sort_by = match &params.sort_by {
        Some(raw) => SlowQuerySortKey::parse(raw)
            .map_err(|message| Problem::from(PgStatStatementsError::Validation { message }))?,
        None => SlowQuerySortKey::MeanExecTime,
    };
    let sort_order = match &params.sort_order {
        Some(raw) => SortOrder::parse(raw)
            .map_err(|message| Problem::from(PgStatStatementsError::Validation { message }))?,
        None => SortOrder::Desc,
    };

    let pg_stat_service = PgStatStatementsService::new(state.external_service_manager.clone());

    let (queries, total_count) = pg_stat_service
        .top_slow_queries(
            service_id,
            SlowQueryPage {
                page,
                page_size,
                sort_by,
                sort_order,
            },
        )
        .await
        .map_err(Problem::from)?;

    Ok(Json(SlowQueriesResponse {
        queries,
        page,
        page_size,
        total_count,
    }))
}

/// Enable `pg_stat_statements` on a standalone Postgres service.
///
/// Stops the container and restarts it so that the
/// `shared_preload_libraries=pg_stat_statements` CMD flag (baked into every
/// new standalone Postgres container) takes effect. The named data volume is
/// reused unchanged — no data is lost.
///
/// **Clustered (HA) services are rejected** with 422 — a blind single-container
/// restart bypasses controlled failover. For clustered services the response
/// body describes the manual rolling-restart steps.
///
/// Confirmation is the caller's responsibility (UI dialog / CLI `--yes` flag)
/// before invoking this endpoint.
#[utoipa::path(
    tag = "External Services",
    post,
    path = "/external-services/{service_id}/pg-stat-statements/enable",
    operation_id = "ExternalServiceEnablePgStatStatements",
    params(
        ("service_id" = i32, Path, description = "ID of the provisioned standalone Postgres service"),
    ),
    responses(
        (status = 200, description = "Container restarted; pg_stat_statements now active", body = EnablePgStatStatementsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions (requires external_services:write)"),
        (status = 404, description = "Service not found"),
        (status = 422, description = "Service is not standalone Postgres (cluster or wrong type)"),
        (status = 500, description = "Restart failed"),
    ),
    security(("bearer_auth" = []))
)]
async fn enable_pg_stat_statements(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(service_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    super::metrics_handlers::assert_service_owned_by_caller(service_id, &auth, &state).await?;

    let pg_stat_service = PgStatStatementsService::new(state.external_service_manager.clone());

    pg_stat_service
        .enable_pg_stat_statements(service_id)
        .await
        .map_err(Problem::from)?;

    // Fetch service name for audit log — best-effort, don't fail the response.
    let service_name = state
        .external_service_manager
        .get_service(service_id)
        .await
        .map(|s| s.name)
        .unwrap_or_else(|_| service_id.to_string());

    let audit = PgStatStatementsEnabledAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        service_id,
        service_name: service_name.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(service_id, error = %e, "Failed to create pg_stat_statements enable audit log");
    }

    Ok(Json(EnablePgStatStatementsResponse {
        message: format!(
            "Service {service_name} restarted successfully. \
             pg_stat_statements is now active — query data will appear after the next workload."
        ),
    }))
}

// ---------------------------------------------------------------------------
// Route registration
// ---------------------------------------------------------------------------

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/external-services/{service_id}/pg-stat-statements/slow-queries",
            get(get_slow_queries),
        )
        .route(
            "/external-services/{service_id}/pg-stat-statements/enable",
            post(enable_pg_stat_statements),
        )
}
