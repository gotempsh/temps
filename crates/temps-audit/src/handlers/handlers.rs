use super::types::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Extension, Json, Router,
};
use std::sync::Arc;
use temps_auth::permission_guard;
use temps_auth::RequireAuth;
use temps_core::{AuditContext, RequestMetadata};
use tracing::error;
use utoipa::OpenApi;

use super::types::{
    AuditLogIpInfo, AuditLogResponse, AuditLogUserInfo, ListAuditLogsQuery, ScrubAuditDataRequest,
    ScrubAuditDataResponse,
};
use crate::audit::AuditDataScrubbedAudit;
use crate::services::AuditScrubError;

#[derive(OpenApi)]
#[openapi(
    paths(list_audit_logs, get_audit_log, scrub_audit_logs),
    components(schemas(
        AuditLogResponse,
        ListAuditLogsQuery,
        AuditLogUserInfo,
        AuditLogIpInfo,
        ScrubAuditDataRequest,
        ScrubAuditDataResponse
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
        .route("/audit/logs/scrub", post(scrub_audit_logs))
}

/// List audit logs with optional filtering
#[utoipa::path(
    tag = "Audit Logs",
    get,
    path = "audit/logs",
    params(ListAuditLogsQuery),
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

impl From<AuditScrubError> for temps_core::problemdetails::Problem {
    fn from(error: AuditScrubError) -> Self {
        match error {
            AuditScrubError::Validation { .. } => {
                temps_core::error_builder::ErrorBuilder::new(StatusCode::BAD_REQUEST)
                    .type_("https://temps.sh/probs/validation-error")
                    .title("Validation Error")
                    .detail(error.to_string())
                    .build()
            }
            AuditScrubError::Database(_) => {
                temps_core::error_builder::ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .type_("https://temps.sh/probs/audit-error")
                    .title("Audit Scrub Error")
                    .detail(error.to_string())
                    .build()
            }
        }
    }
}

/// Redact a deleted user's identifier values from audit log payloads.
///
/// Replaces every payload string value exactly matching one of the provided
/// identifiers (case-insensitive) with `[REDACTED]`, across all audit rows —
/// both rows the user authored and rows about them. The structural record
/// (operation type, timestamps, non-matching context) is preserved. Intended
/// for data-erasure requests after an account deletion; the scrub itself is
/// recorded as an `AUDIT_DATA_SCRUBBED` audit entry that stores only which
/// identifier kinds were provided, never their values.
///
/// Note: plugins that fingerprint audit payload content will report scrubbed
/// rows as modified — an in-place redaction is exactly that, and the
/// `AUDIT_DATA_SCRUBBED` entry documents why.
#[utoipa::path(
    tag = "Audit Logs",
    post,
    path = "audit/logs/scrub",
    request_body = ScrubAuditDataRequest,
    responses(
        (status = 200, description = "Scrub completed", body = ScrubAuditDataResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("api_key" = []))
)]
async fn scrub_audit_logs(
    State(app_state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<ScrubAuditDataRequest>,
) -> Result<impl IntoResponse, temps_core::problemdetails::Problem> {
    permission_guard!(auth, AuditWrite);

    let mut identifier_fields = Vec::new();
    let mut identifiers = Vec::new();
    for (field, value) in [
        ("email", &request.email),
        ("username", &request.username),
        ("name", &request.name),
    ] {
        if let Some(v) = value {
            identifier_fields.push(field.to_string());
            identifiers.push(v.clone());
        }
    }

    let outcome = app_state
        .audit_service
        .scrub_pii_values(identifiers)
        .await?;

    // Record the scrub itself — counts and which identifier kinds were
    // provided, never the values being erased.
    let audit = AuditDataScrubbedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        identifier_fields,
        rows_scanned: outcome.rows_scanned,
        rows_scrubbed: outcome.rows_scrubbed,
    };
    if let Err(e) = app_state.audit_service.create_audit_log_typed(&audit).await {
        error!("Failed to create audit log for audit data scrub: {}", e);
    }

    Ok(Json(ScrubAuditDataResponse {
        rows_scanned: outcome.rows_scanned,
        rows_scrubbed: outcome.rows_scrubbed,
    }))
}
