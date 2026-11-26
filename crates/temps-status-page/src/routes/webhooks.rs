// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Authenticated management of a status page's webhooks (ADR 108 D2).
//!
//! Listing needs `status_page:read`; creating, changing and deleting need
//! `status_page:write` on every project of the page. Every write is
//! audit-logged without the secret, which is returned once, on creation,
//! with `Cache-Control: no-store`.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use temps_authz::{permission_guard, project_permission_guard, project_scope_guard, RequireAuth};
use temps_core::error_builder::{bad_request, internal_server_error, not_found, ErrorBuilder};
use temps_core::problemdetails::Problem;
use temps_core::{AuditLogger, RequestMetadata};
use utoipa::OpenApi;

use super::audit::{StatusPageWebhookAction, StatusPageWebhookAudit};
use crate::services::webhooks::payload::{WebhookEvent, WebhookPage, WebhookPayload};
use crate::services::webhooks::{
    CreateStatusPageWebhookRequest, StatusPageWebhookCreatedResponse,
    StatusPageWebhookListResponse, StatusPageWebhookResponse, StatusPageWebhookService,
    UpdateStatusPageWebhookRequest, WebhookError,
};
use crate::services::PublicStatusPageService;

pub struct WebhookRoutesState {
    pub webhooks: Arc<StatusPageWebhookService>,
    pub pages: Arc<PublicStatusPageService>,
    pub audit: Arc<dyn AuditLogger>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

impl From<WebhookError> for Problem {
    fn from(error: WebhookError) -> Self {
        match error {
            WebhookError::StatusPageNotFound { .. } => not_found()
                .title("Status Page Not Found")
                .detail(error.to_string())
                .build(),
            WebhookError::NotFound { .. } => not_found()
                .title("Webhook Not Found")
                .detail(error.to_string())
                .build(),
            WebhookError::Validation { .. } => bad_request()
                .title("Invalid Webhook")
                .detail(error.to_string())
                .build(),
            WebhookError::TooMany { .. } => ErrorBuilder::new(StatusCode::CONFLICT)
                .title("Too Many Webhooks")
                .detail(error.to_string())
                .build(),
            WebhookError::SecretGeneration { .. } => internal_server_error()
                .title("Secret Generation Failed")
                .detail(error.to_string())
                .build(),
            WebhookError::Secret { .. } => internal_server_error()
                .title("Webhook Secret Unreadable")
                .detail(error.to_string())
                .build(),
            WebhookError::Database(_) => internal_server_error()
                .title("Database Error")
                .detail(error.to_string())
                .build(),
        }
    }
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct WebhookListQuery {
    /// Page number, starting at 1 (default 1).
    pub page: Option<u64>,
    /// Items per page (default 20, max 100).
    pub page_size: Option<u64>,
}

#[allow(clippy::too_many_arguments, reason = "one audit entry's fields")]
async fn record(
    state: &WebhookRoutesState,
    auth: &temps_authz::AuthContext,
    metadata: &RequestMetadata,
    action: StatusPageWebhookAction,
    project_ids: Vec<i32>,
    status_page_id: i32,
    webhook_id: i32,
    url: Option<String>,
    enabled: Option<bool>,
) {
    let entry = StatusPageWebhookAudit {
        actor_user_id: auth.user_id_opt(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
        action,
        project_ids,
        status_page_id,
        webhook_id,
        url,
        enabled,
    };
    if let Err(error) = state.audit.create_audit_log(&entry).await {
        tracing::error!(
            status_page_id,
            webhook_id,
            action = ?action,
            error = %error,
            "failed to persist status page webhook audit"
        );
    }
}

/// Webhooks of a status page
///
/// Newest first, each with the outcome of its last delivery.
#[utoipa::path(
    get,
    path = "/status-pages/{status_page_id}/webhooks",
    operation_id = "list_status_page_webhooks",
    params(("status_page_id" = i32, Path, description = "Status page ID"), WebhookListQuery),
    responses(
        (status = 200, description = "Webhooks, newest first", body = StatusPageWebhookListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Status page not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn list_webhooks(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WebhookRoutesState>>,
    Path(status_page_id): Path<i32>,
    Query(query): Query<WebhookListQuery>,
) -> Result<impl IntoResponse, Problem> {
    page_access_guard!(auth, state, StatusPageRead, status_page_id);
    Ok(Json(
        state
            .webhooks
            .list(
                status_page_id,
                query.page.unwrap_or(1),
                query.page_size.unwrap_or(20),
            )
            .await?,
    ))
}

/// One webhook of a status page
///
/// The same shape as in the list, with the outcome of its last delivery.
/// The signing secret is never returned again.
#[utoipa::path(
    get,
    path = "/status-pages/{status_page_id}/webhooks/{webhook_id}",
    operation_id = "get_status_page_webhook",
    params(
        ("status_page_id" = i32, Path, description = "Status page ID"),
        ("webhook_id" = i32, Path, description = "Webhook ID"),
    ),
    responses(
        (status = 200, description = "The webhook", body = StatusPageWebhookResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Status page or webhook not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn get_webhook(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WebhookRoutesState>>,
    Path((status_page_id, webhook_id)): Path<(i32, i32)>,
) -> Result<Json<StatusPageWebhookResponse>, Problem> {
    page_access_guard!(auth, state, StatusPageRead, status_page_id);
    Ok(Json(state.webhooks.get(status_page_id, webhook_id).await?))
}

/// Register a webhook on a status page
///
/// Each new public event of the page (an incident update or resolution, a
/// scheduled maintenance window) is posted to the URL as JSON, signed in
/// `X-Temps-Signature` with the secret returned here, once. The URL must be
/// `https://` and resolve to a public address. `enabled` defaults to `true`.
#[utoipa::path(
    post,
    path = "/status-pages/{status_page_id}/webhooks",
    operation_id = "create_status_page_webhook",
    params(("status_page_id" = i32, Path, description = "Status page ID")),
    request_body = CreateStatusPageWebhookRequest,
    responses(
        (status = 201, description = "Webhook registered; the secret is shown only now", body = StatusPageWebhookCreatedResponse),
        (status = 400, description = "Invalid URL or non-public address"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Status page not found"),
        (status = 409, description = "The page already has the most webhooks allowed"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn create_webhook(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WebhookRoutesState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(status_page_id): Path<i32>,
    Json(request): Json<CreateStatusPageWebhookRequest>,
) -> Result<impl IntoResponse, Problem> {
    let project_ids = page_access_guard!(auth, state, StatusPageWrite, status_page_id);
    let created = state.webhooks.create(status_page_id, request).await?;
    record(
        &state,
        &auth,
        &metadata,
        StatusPageWebhookAction::Created,
        project_ids,
        status_page_id,
        created.webhook.id,
        Some(created.webhook.url.clone()),
        Some(created.webhook.enabled),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(created),
    ))
}

/// Change a webhook's URL or switch it on or off
#[utoipa::path(
    patch,
    path = "/status-pages/{status_page_id}/webhooks/{webhook_id}",
    operation_id = "update_status_page_webhook",
    params(
        ("status_page_id" = i32, Path, description = "Status page ID"),
        ("webhook_id" = i32, Path, description = "Webhook ID"),
    ),
    request_body = UpdateStatusPageWebhookRequest,
    responses(
        (status = 200, description = "Webhook updated", body = StatusPageWebhookResponse),
        (status = 400, description = "Invalid URL or non-public address"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Status page or webhook not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn update_webhook(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WebhookRoutesState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((status_page_id, webhook_id)): Path<(i32, i32)>,
    Json(request): Json<UpdateStatusPageWebhookRequest>,
) -> Result<impl IntoResponse, Problem> {
    let project_ids = page_access_guard!(auth, state, StatusPageWrite, status_page_id);
    let webhook = state
        .webhooks
        .update(status_page_id, webhook_id, request)
        .await?;
    record(
        &state,
        &auth,
        &metadata,
        StatusPageWebhookAction::Updated,
        project_ids,
        status_page_id,
        webhook_id,
        Some(webhook.url.clone()),
        Some(webhook.enabled),
    )
    .await;
    Ok(Json(webhook))
}

/// Delete a webhook
///
/// Events still waiting for it are dropped.
#[utoipa::path(
    delete,
    path = "/status-pages/{status_page_id}/webhooks/{webhook_id}",
    operation_id = "delete_status_page_webhook",
    params(
        ("status_page_id" = i32, Path, description = "Status page ID"),
        ("webhook_id" = i32, Path, description = "Webhook ID"),
    ),
    responses(
        (status = 204, description = "Webhook deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Status page or webhook not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn delete_webhook(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WebhookRoutesState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((status_page_id, webhook_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    let project_ids = page_access_guard!(auth, state, StatusPageWrite, status_page_id);
    state.webhooks.delete(status_page_id, webhook_id).await?;
    record(
        &state,
        &auth,
        &metadata,
        StatusPageWebhookAction::Deleted,
        project_ids,
        status_page_id,
        webhook_id,
        None,
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

pub fn create_webhook_router(state: Arc<WebhookRoutesState>) -> Router {
    Router::new()
        .route(
            "/status-pages/{status_page_id}/webhooks",
            get(list_webhooks).post(create_webhook),
        )
        .route(
            "/status-pages/{status_page_id}/webhooks/{webhook_id}",
            get(get_webhook)
                .patch(update_webhook)
                .delete(delete_webhook),
        )
        .with_state(state)
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_webhooks,
        get_webhook,
        create_webhook,
        update_webhook,
        delete_webhook
    ),
    components(schemas(
        StatusPageWebhookResponse,
        StatusPageWebhookListResponse,
        StatusPageWebhookCreatedResponse,
        CreateStatusPageWebhookRequest,
        UpdateStatusPageWebhookRequest,
        WebhookPayload,
        WebhookPage,
        WebhookEvent,
    ))
)]
pub struct WebhookApiDoc;
