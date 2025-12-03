//! Email sending handlers

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::{
    error_builder::{bad_request, internal_server_error, not_found},
    problemdetails::Problem,
    AuditContext, RequestMetadata,
};
use tracing::error;
use uuid::Uuid;

use super::audit::EmailSentAudit;
use super::types::{
    AppState, EmailResponse, EmailStatsResponse, ListEmailsQuery, PaginatedEmailsResponse,
    SendEmailRequestBody, SendEmailResponseBody,
};
use crate::services::{ListEmailsOptions, SendEmailRequest};

/// Configure email routes
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/emails", post(send_email).get(list_emails))
        .route("/emails/{id}", get(get_email))
        .route("/emails/stats", get(get_email_stats))
}

/// Send an email
#[utoipa::path(
    tag = "Emails",
    post,
    path = "/emails",
    request_body = SendEmailRequestBody,
    responses(
        (status = 201, description = "Email sent successfully", body = SendEmailResponseBody),
        (status = 400, description = "Invalid request or domain not verified"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn send_email(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    axum::Extension(metadata): axum::Extension<RequestMetadata>,
    Json(request): Json<SendEmailRequestBody>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailsSend);

    // Validate request
    if request.to.is_empty() {
        return Err(bad_request()
            .detail("At least one recipient is required")
            .build());
    }

    if request.html.is_none() && request.text.is_none() {
        return Err(bad_request()
            .detail("Either html or text body is required")
            .build());
    }

    let send_request = SendEmailRequest {
        domain_id: request.domain_id,
        project_id: request.project_id,
        from: request.from.clone(),
        from_name: request.from_name.clone(),
        to: request.to.clone(),
        cc: request.cc.clone(),
        bcc: request.bcc.clone(),
        reply_to: request.reply_to.clone(),
        subject: request.subject.clone(),
        html: request.html.clone(),
        text: request.text.clone(),
        headers: request.headers.clone(),
        tags: request.tags.clone(),
    };

    let result = state.email_service.send(send_request).await.map_err(|e| {
        error!("Failed to send email: {}", e);
        match &e {
            crate::errors::EmailError::DomainNotVerified(msg) => {
                bad_request().detail(msg.clone()).build()
            }
            crate::errors::EmailError::Validation(msg) => bad_request().detail(msg.clone()).build(),
            _ => internal_server_error()
                .detail(format!("Failed to send email: {}", e))
                .build(),
        }
    })?;

    // Create audit log
    let audit = EmailSentAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        email_id: result.id,
        from: request.from,
        to: request.to,
        subject: request.subject,
        domain_id: request.domain_id,
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    let response = SendEmailResponseBody {
        id: result.id.to_string(),
        status: result.status,
        provider_message_id: result.provider_message_id,
    };

    Ok((StatusCode::CREATED, Json(response)))
}

/// List emails with optional filtering
#[utoipa::path(
    tag = "Emails",
    get,
    path = "/emails",
    params(ListEmailsQuery),
    responses(
        (status = 200, description = "List of emails", body = PaginatedEmailsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_emails(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListEmailsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailsRead);

    let options = ListEmailsOptions {
        domain_id: query.domain_id,
        project_id: query.project_id,
        status: query.status,
        from_address: query.from_address,
        page: query.page,
        page_size: query.page_size,
    };

    let (emails, total) = state.email_service.list(options).await.map_err(|e| {
        error!("Failed to list emails: {}", e);
        internal_server_error()
            .detail("Failed to list emails")
            .build()
    })?;

    let data: Vec<EmailResponse> = emails
        .into_iter()
        .map(|e| EmailResponse {
            id: e.id.to_string(),
            domain_id: e.domain_id,
            project_id: e.project_id,
            from_address: e.from_address,
            from_name: e.from_name,
            to_addresses: parse_json_array(e.to_addresses),
            cc_addresses: e.cc_addresses.map(parse_json_array),
            bcc_addresses: e.bcc_addresses.map(parse_json_array),
            reply_to: e.reply_to,
            subject: e.subject,
            html_body: e.html_body,
            text_body: e.text_body,
            headers: e.headers.and_then(parse_json_map),
            tags: e.tags.map(parse_json_array),
            status: e.status,
            provider_message_id: e.provider_message_id,
            error_message: e.error_message,
            sent_at: e.sent_at.map(|dt| dt.to_rfc3339()),
            created_at: e.created_at.to_rfc3339(),
        })
        .collect();

    let response = PaginatedEmailsResponse {
        data,
        total,
        page: query.page.unwrap_or(1),
        page_size: query.page_size.unwrap_or(20),
    };

    Ok(Json(response))
}

/// Get an email by ID
#[utoipa::path(
    tag = "Emails",
    get,
    path = "/emails/{id}",
    responses(
        (status = 200, description = "Email details", body = EmailResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Email not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = String, Path, description = "Email ID (UUID)")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_email(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailsRead);

    let email_id = Uuid::parse_str(&id)
        .map_err(|_| bad_request().detail("Invalid email ID format").build())?;

    let email = state.email_service.get(email_id).await.map_err(|e| {
        error!("Failed to get email: {}", e);
        not_found().detail("Email not found").build()
    })?;

    let response = EmailResponse {
        id: email.id.to_string(),
        domain_id: email.domain_id,
        project_id: email.project_id,
        from_address: email.from_address,
        from_name: email.from_name,
        to_addresses: parse_json_array(email.to_addresses),
        cc_addresses: email.cc_addresses.map(parse_json_array),
        bcc_addresses: email.bcc_addresses.map(parse_json_array),
        reply_to: email.reply_to,
        subject: email.subject,
        html_body: email.html_body,
        text_body: email.text_body,
        headers: email.headers.and_then(parse_json_map),
        tags: email.tags.map(parse_json_array),
        status: email.status,
        provider_message_id: email.provider_message_id,
        error_message: email.error_message,
        sent_at: email.sent_at.map(|dt| dt.to_rfc3339()),
        created_at: email.created_at.to_rfc3339(),
    };

    Ok(Json(response))
}

/// Get email statistics
#[utoipa::path(
    tag = "Emails",
    get,
    path = "/emails/stats",
    params(
        ("domain_id" = Option<i32>, Query, description = "Optional domain ID to filter stats")
    ),
    responses(
        (status = 200, description = "Email statistics", body = EmailStatsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_email_stats(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Query(query): Query<StatsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailsRead);

    let stats = state
        .email_service
        .count_by_status(query.domain_id)
        .await
        .map_err(|e| {
            error!("Failed to get email stats: {}", e);
            internal_server_error()
                .detail("Failed to get email statistics")
                .build()
        })?;

    let response = EmailStatsResponse {
        total: stats.total,
        sent: stats.sent,
        failed: stats.failed,
        queued: stats.queued,
    };

    Ok(Json(response))
}

#[derive(Debug, serde::Deserialize)]
pub struct StatsQuery {
    pub domain_id: Option<i32>,
}

/// Parse a serde_json::Value array to Vec<String>
fn parse_json_array(value: serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Array(arr) => arr
            .into_iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => Vec::new(),
    }
}

/// Parse a serde_json::Value object to HashMap<String, String>
fn parse_json_map(value: serde_json::Value) -> Option<std::collections::HashMap<String, String>> {
    match value {
        serde_json::Value::Object(obj) => {
            let map: std::collections::HashMap<String, String> = obj
                .into_iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
                .collect();
            if map.is_empty() {
                None
            } else {
                Some(map)
            }
        }
        _ => None,
    }
}
