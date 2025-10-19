use super::types::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use tracing::error;
use std::sync::Arc;
use temps_auth::permission_guard;
use temps_auth::RequireAuth;
use utoipa::OpenApi;

use super::types::{
    AuditLogResponse,
    ListAuditLogsQuery,
    AuditLogUserInfo,
    AuditLogIpInfo
};

#[derive(OpenApi)]
#[openapi(
    paths(list_audit_logs, get_audit_log),
    components(schemas(
        AuditLogResponse,
        ListAuditLogsQuery,
        AuditLogUserInfo,
        AuditLogIpInfo
    )),
    info(
        title = "Audit API",
        description = "API endpoints for managing and retrieving audit logs. \
        Provides detailed tracking of system events, user actions, and security-relevant operations.",
        version = "1.0.0"
    )
)]
pub struct AuditApiDoc;

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/audit/logs", get(list_audit_logs))
        .route("/audit/logs/{id}", get(get_audit_log))
}

/// List audit logs with optional filtering
#[utoipa::path(
    tag = "Audit Logs",
    get,
    path = "audit/logs",
    params(
        ("operation_type", Query, description = "Filter logs by operation type"),
        ("user_id", Query, description = "Filter logs by user ID"),
        ("from", Query, description = "Start timestamp (milliseconds since epoch)"),
        ("to", Query, description = "End timestamp (milliseconds since epoch)"),
        ("limit", Query, description = "Maximum number of logs to return"),
        ("offset", Query, description = "Number of logs to skip")
    ),
    responses(
        (status = 200, description = "List of audit logs", body = Vec<AuditLogResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(("api_key" = []))
)]
async fn list_audit_logs(
    State(app_state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Query(query): Query<ListAuditLogsQuery>,
) -> Result<impl IntoResponse, temps_core::problemdetails::Problem> {
    permission_guard!(auth, AuditRead);
    let from_date = query.from.map(Into::into);
    let to_date = query.to.map(Into::into);

    match app_state
        .audit_service
        .filter_audit_logs(
            query.operation_type.as_deref(),
            query.user_id,
            from_date,
            to_date,
            query.limit.unwrap_or(100),
            query.offset.unwrap_or(0),
        )
        .await
    {
        Ok(logs) => {
            let responses: Vec<AuditLogResponse> = logs.into_iter().map(Into::into).collect();
            Ok(Json(responses))
        }
        Err(e) => {
            error!("Failed to list audit logs: {}", e);
            Err(
                temps_core::error_builder::ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .type_("https://temps.sh/probs/audit-error")
                    .title("Audit Log Error")
                    .detail(format!("Failed to list audit logs: {}", e))
                    .build(),
            )
        }
    }
}

/// Get a specific audit log entry by ID
#[utoipa::path(
    tag = "Audit Logs",
    get,
    path = "audit/logs/{id}",
    responses(
        (status = 200, description = "Audit log details", body = AuditLogResponse),
        (status = 404, description = "Audit log not found"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(("api_key" = []))
)]
async fn get_audit_log(
    State(app_state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, temps_core::problemdetails::Problem> {
    permission_guard!(auth, AuditRead);
    match app_state.audit_service.get_log_by_id(id).await {
        Ok(Some(log_details)) => Ok(Json(AuditLogResponse::from(log_details))),
        Ok(None) => Err(
            temps_core::error_builder::ErrorBuilder::new(StatusCode::NOT_FOUND)
                .type_("https://temps.sh/probs/not-found")
                .title("Audit Log Not Found")
                .detail("Audit log not found")
                .build(),
        ),
        Err(e) => {
            error!("Failed to get audit log: {}", e);
            Err(
                temps_core::error_builder::ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .type_("https://temps.sh/probs/audit-error")
                    .title("Audit Log Error")
                    .detail(format!("Failed to get audit log: {}", e))
                    .build(),
            )
        }
    }
}
