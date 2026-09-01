// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP handlers for the `pg_stat_statements` slow-query endpoint.
//!
//! # Route
//!
//! ```text
//! GET  /external-services/{service_id}/pg-stat-statements/slow-queries?page=N&page_size=N&sort_by=...&sort_order=...
//! POST /external-services/{service_id}/pg-stat-statements/reset
//! ```
//!
//! Reads require `ExternalServicesRead`; mutations require
//! `ExternalServicesWrite`. The caller must have access to the service's parent
//! project (same access control as every other per-service endpoint in this
//! plugin).

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
            PgStatStatementsError::ResetFailed { .. } => {
                problemdetails::new(StatusCode::BAD_GATEWAY)
                    .with_title("Statistics Reset Failed")
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
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {}", e))
    }
}

#[derive(Debug, Clone, Serialize)]
struct PgStatStatementsResetAudit {
    context: AuditContext,
    service_id: i32,
    service_name: String,
}

impl AuditOperation for PgStatStatementsResetAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_PG_STAT_STATEMENTS_RESET".to_string()
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

/// Response for the pg_stat_statements reset endpoint.
#[derive(Debug, Serialize, ToSchema)]
pub struct ResetPgStatStatementsResponse {
    /// Human-readable message confirming the destructive action.
    pub message: String,
}

/// Explicit confirmation required for the destructive statistics reset.
///
/// Requiring JSON makes the endpoint non-simple for browsers, preventing a
/// deployed same-site application from triggering it with a plain HTML form.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ResetPgStatStatementsRequest {
    /// Must be `true` to acknowledge the global, irreversible reset.
    pub confirm: bool,
}

fn validate_reset_confirmation(
    request: &ResetPgStatStatementsRequest,
) -> Result<(), PgStatStatementsError> {
    if !request.confirm {
        return Err(PgStatStatementsError::Validation {
            message: "confirm must be true to reset all pg_stat_statements statistics".to_owned(),
        });
    }
    Ok(())
}

#[derive(OpenApi)]
#[openapi(
    paths(get_slow_queries, enable_pg_stat_statements, reset_pg_stat_statements),
    components(schemas(
        SlowQueriesResponse,
        SlowQueryRow,
        EnablePgStatStatementsResponse,
        ResetPgStatStatementsRequest,
        ResetPgStatStatementsResponse
    ))
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
    super::metrics_handlers::assert_service_owned_by_caller(service_id, &auth, &state).await?;

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

/// Reset all statistics accumulated by `pg_stat_statements` for a Postgres
/// service. This affects every user, database, and normalized query tracked by
/// the target Postgres instance and cannot be undone.
#[utoipa::path(
    tag = "External Services",
    post,
    path = "/external-services/{service_id}/pg-stat-statements/reset",
    operation_id = "ExternalServiceResetPgStatStatements",
    request_body(
        content = ResetPgStatStatementsRequest,
        description = "Explicit confirmation of the global, irreversible reset",
        content_type = "application/json"
    ),
    params(
        ("service_id" = i32, Path, description = "ID of the provisioned Postgres service"),
    ),
    responses(
        (status = 200, description = "All accumulated pg_stat_statements statistics cleared", body = ResetPgStatStatementsResponse),
        (status = 400, description = "Missing or invalid reset confirmation"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions (requires external_services:write)"),
        (status = 404, description = "Service not found"),
        (status = 422, description = "Service is not Postgres"),
        (status = 502, description = "Target Postgres rejected or failed the reset operation"),
    ),
    security(("bearer_auth" = []))
)]
async fn reset_pg_stat_statements(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(service_id): Path<i32>,
    Json(request): Json<ResetPgStatStatementsRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    validate_reset_confirmation(&request).map_err(Problem::from)?;
    super::metrics_handlers::assert_service_owned_by_caller(service_id, &auth, &state).await?;

    let pg_stat_service = PgStatStatementsService::new(state.external_service_manager.clone());
    pg_stat_service
        .reset_pg_stat_statements(service_id)
        .await
        .map_err(Problem::from)?;

    let service_name = state
        .external_service_manager
        .get_service(service_id)
        .await
        .map(|s| s.name)
        .unwrap_or_else(|_| service_id.to_string());

    let audit = PgStatStatementsResetAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        service_id,
        service_name: service_name.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(service_id, error = %e, "Failed to create pg_stat_statements reset audit log");
    }

    Ok(Json(ResetPgStatStatementsResponse {
        message: format!(
            "All pg_stat_statements statistics for service {service_name} were reset successfully."
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
        .route(
            "/external-services/{service_id}/pg-stat-statements/reset",
            post(reset_pg_stat_statements),
        )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::MockDatabase;

    struct NoopAuditLogger;

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for NoopAuditLogger {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::AuditOperation,
        ) -> Result<(), temps_core::anyhow::Error> {
            Ok(())
        }
    }

    fn test_deployment_token_auth(project_id: i32) -> temps_auth::AuthContext {
        temps_auth::AuthContext::new_deployment_token(
            project_id,
            None,
            None,
            1,
            "test-token".to_string(),
            vec![temps_entities::deployment_tokens::DeploymentTokenPermission::FullAccess],
        )
    }

    fn test_request_metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "127.0.0.1".to_string(),
            user_agent: "pg-stat-statements-reset-test".to_string(),
            headers: axum::http::HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://localhost".to_string(),
            scheme: "http".to_string(),
            host: "localhost".to_string(),
            is_secure: false,
        }
    }

    fn test_state() -> Arc<AppState> {
        let db = Arc::new(MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection());
        let encryption_service = Arc::new(temps_core::EncryptionService::new_from_password(
            "pg-stat-statements-idor-test",
        ));
        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults()
                .expect("Docker client configuration should be available"),
        );
        let manager = Arc::new(crate::services::ExternalServiceManager::new(
            db.clone(),
            encryption_service,
            docker,
            Arc::new(temps_dns::DnsRegistry::new(db.clone())),
        ));
        Arc::new(AppState {
            external_service_manager: manager.clone(),
            audit_service: Arc::new(NoopAuditLogger),
            query_service: Arc::new(crate::QueryService::new(manager)),
            health_monitor: None,
            metrics_store: None,
            db: db.clone(),
            api_key_service: Arc::new(temps_auth::ApiKeyService::new(db)),
            config_service: None,
            telemetry: Arc::new(temps_core::NoopTelemetryReporter),
            project_access_checker: None,
        })
    }

    /// A deployment token can never satisfy `ExternalServicesRead` — the
    /// deployment-token permission bridge in `AuthContext::has_permission`
    /// only maps a small explicit whitelist (analytics/email/AI-gateway),
    /// deliberately excluding external-services access. `get_slow_queries`
    /// must reject it at the permission gate (403) regardless of the
    /// service/project it targets, and never reach
    /// `assert_service_owned_by_caller`'s DB-touching ownership check —
    /// this is why `state.db` here is a bare, empty `MockDatabase`: any
    /// unexpected query beyond the permission check would panic the mock.
    #[tokio::test]
    async fn get_slow_queries_rejects_deployment_token_at_permission_gate() {
        let state = test_state();

        let result = get_slow_queries(
            RequireAuth(test_deployment_token_auth(100)),
            State(state),
            Path(7),
            Query(SlowQueryParams {
                page: None,
                page_size: None,
                sort_by: None,
                sort_order: None,
            }),
        )
        .await;

        let problem = match result {
            Ok(_) => panic!("deployment tokens must never be granted ExternalServicesRead"),
            Err(problem) => problem,
        };
        assert_eq!(problem.into_response().status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn reset_pg_stat_statements_rejects_deployment_token_at_permission_gate() {
        let result = reset_pg_stat_statements(
            RequireAuth(test_deployment_token_auth(100)),
            State(test_state()),
            Extension(test_request_metadata()),
            Path(7),
            Json(ResetPgStatStatementsRequest { confirm: true }),
        )
        .await;

        let problem = match result {
            Ok(_) => panic!("deployment tokens must never reset pg_stat_statements"),
            Err(problem) => problem,
        };
        assert_eq!(problem.into_response().status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn reset_requires_explicit_true_confirmation() {
        let error = validate_reset_confirmation(&ResetPgStatStatementsRequest { confirm: false })
            .expect_err("false confirmation must be rejected");
        assert!(matches!(error, PgStatStatementsError::Validation { .. }));
        assert!(
            validate_reset_confirmation(&ResetPgStatStatementsRequest { confirm: true }).is_ok()
        );
    }
}
