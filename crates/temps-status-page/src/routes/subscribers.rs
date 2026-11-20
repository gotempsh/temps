// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Authenticated management of a status page's email subscriptions
//! (ADR 108 D2).
//!
//! Authorization follows the page routes: the instance-wide permission first,
//! then the permission on every project the page draws from. Reading the
//! settings and counts needs `status_page:read`; anything that shows,
//! exports or deletes subscriber addresses, or changes the settings, needs
//! `status_page:write`, because addresses are personal data. Every write and
//! every export is audit-logged, without the addresses.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{delete, get},
    Json, Router,
};
use serde::Deserialize;
use temps_authz::{permission_guard, project_permission_guard, project_scope_guard, RequireAuth};
use temps_core::error_builder::{bad_request, internal_server_error, not_found, ErrorBuilder};
use temps_core::problemdetails::Problem;
use temps_core::{AuditLogger, RequestMetadata};
use utoipa::OpenApi;

use super::audit::{StatusPageSubscriberAction, StatusPageSubscriberAudit};
use crate::services::subscribers::{
    StatusPageSubscriberService, SubscriberListResponse, SubscriberResponse, SubscriberState,
    SubscriptionCapability, SubscriptionError, SubscriptionSettingsResponse,
    UpdateSubscriptionSettingsRequest,
};
use crate::services::PublicStatusPageService;

pub struct SubscriberRoutesState {
    pub subscribers: Arc<StatusPageSubscriberService>,
    pub pages: Arc<PublicStatusPageService>,
    pub audit: Arc<dyn AuditLogger>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

impl From<SubscriptionError> for Problem {
    fn from(error: SubscriptionError) -> Self {
        match error {
            SubscriptionError::StatusPageNotFound { .. } => not_found()
                .title("Status Page Not Found")
                .detail(error.to_string())
                .build(),
            SubscriptionError::SubscriberNotFound { .. } => not_found()
                .title("Subscriber Not Found")
                .detail(error.to_string())
                .build(),
            SubscriptionError::InvalidSender { .. } => bad_request()
                .title("Invalid Sender Address")
                .detail(error.to_string())
                .build(),
            SubscriptionError::NotReady { .. } => ErrorBuilder::new(StatusCode::CONFLICT)
                .title("Subscriptions Not Ready")
                .detail(error.to_string())
                .build(),
            SubscriptionError::Database(_) => internal_server_error()
                .title("Database Error")
                .detail(error.to_string())
                .build(),
            SubscriptionError::TokenGeneration { .. } => internal_server_error()
                .title("Token Generation Failed")
                .detail(error.to_string())
                .build(),
            SubscriptionError::EmailSetup { .. } => internal_server_error()
                .title("Email Setup Unreadable")
                .detail(error.to_string())
                .build(),
            SubscriptionError::Send { .. } => internal_server_error()
                .title("Email Send Failed")
                .detail(error.to_string())
                .build(),
            SubscriptionError::Settings { .. } => internal_server_error()
                .title("Settings Unreadable")
                .detail(error.to_string())
                .build(),
        }
    }
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct SubscriberListQuery {
    /// Page number, starting at 1 (default 1).
    pub page: Option<u64>,
    /// Items per page (default 20, max 100).
    pub page_size: Option<u64>,
}

async fn record(state: &SubscriberRoutesState, audit: StatusPageSubscriberAudit) {
    if let Err(error) = state.audit.create_audit_log(&audit).await {
        tracing::error!(
            status_page_id = audit.status_page_id,
            action = ?audit.action,
            error = %error,
            "failed to persist status page subscription audit"
        );
    }
}

fn audit(
    auth: &temps_authz::AuthContext,
    metadata: &RequestMetadata,
    action: StatusPageSubscriberAction,
    project_ids: Vec<i32>,
    status_page_id: i32,
) -> StatusPageSubscriberAudit {
    StatusPageSubscriberAudit {
        actor_user_id: auth.user_id_opt(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
        action,
        project_ids,
        status_page_id,
        enabled: None,
        sender: None,
        subscriber_id: None,
        exported_count: None,
    }
}

/// Email subscription settings of a status page
///
/// Whether visitors can subscribe, the sender, whether mail and links can go
/// out (with what is missing and where to fix it) and subscriber counts.
#[utoipa::path(
    get,
    path = "/status-pages/{status_page_id}/subscriptions",
    operation_id = "get_status_page_subscriptions",
    params(("status_page_id" = i32, Path, description = "Status page ID")),
    responses(
        (status = 200, description = "Subscription settings", body = SubscriptionSettingsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Status page not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn get_subscription_settings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SubscriberRoutesState>>,
    Path(status_page_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    page_access_guard!(auth, state, StatusPageRead, status_page_id);
    Ok(Json(state.subscribers.settings(status_page_id).await?))
}

/// Change the email subscription settings of a status page
///
/// Switching subscriptions on needs a sender at a verified email domain with
/// an active provider, and an address for the links (a public URL or a
/// verified custom domain); otherwise 409 says what is missing.
#[utoipa::path(
    put,
    path = "/status-pages/{status_page_id}/subscriptions",
    operation_id = "update_status_page_subscriptions",
    params(("status_page_id" = i32, Path, description = "Status page ID")),
    request_body = UpdateSubscriptionSettingsRequest,
    responses(
        (status = 200, description = "Settings saved", body = SubscriptionSettingsResponse),
        (status = 400, description = "Invalid sender address"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Status page not found"),
        (status = 409, description = "Subscriptions cannot be switched on yet"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn update_subscription_settings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SubscriberRoutesState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(status_page_id): Path<i32>,
    Json(request): Json<UpdateSubscriptionSettingsRequest>,
) -> Result<impl IntoResponse, Problem> {
    let project_ids = page_access_guard!(auth, state, StatusPageWrite, status_page_id);
    // A credential confined to a project may only pick a sender its projects
    // were granted; an unscoped one picks any verified domain.
    let scoped = auth.project_id().is_some();
    let settings = state
        .subscribers
        .update_settings(
            status_page_id,
            request,
            scoped.then_some(project_ids.as_slice()),
        )
        .await?;
    let mut entry = audit(
        &auth,
        &metadata,
        StatusPageSubscriberAction::SettingsUpdated,
        project_ids,
        status_page_id,
    );
    entry.enabled = Some(settings.enabled);
    entry.sender = settings.sender.clone();
    record(&state, entry).await;
    Ok(Json(settings))
}

/// Subscribers of a status page
///
/// Newest first. Addresses are personal data: only callers who may change the
/// page see them.
#[utoipa::path(
    get,
    path = "/status-pages/{status_page_id}/subscribers",
    operation_id = "list_status_page_subscribers",
    params(("status_page_id" = i32, Path, description = "Status page ID"), SubscriberListQuery),
    responses(
        (status = 200, description = "Subscribers, newest first", body = SubscriberListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Status page not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn list_subscribers(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SubscriberRoutesState>>,
    Path(status_page_id): Path<i32>,
    Query(query): Query<SubscriberListQuery>,
) -> Result<impl IntoResponse, Problem> {
    page_access_guard!(auth, state, StatusPageWrite, status_page_id);
    Ok(Json(
        state
            .subscribers
            .list(
                status_page_id,
                query.page.unwrap_or(1),
                query.page_size.unwrap_or(20),
            )
            .await?,
    ))
}

/// Delete a subscriber
///
/// Removes the address from the page. The subscriber gets no more mail.
#[utoipa::path(
    delete,
    path = "/status-pages/{status_page_id}/subscribers/{subscriber_id}",
    operation_id = "delete_status_page_subscriber",
    params(
        ("status_page_id" = i32, Path, description = "Status page ID"),
        ("subscriber_id" = i32, Path, description = "Subscriber ID"),
    ),
    responses(
        (status = 204, description = "Subscriber deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Status page or subscriber not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn delete_subscriber(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SubscriberRoutesState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((status_page_id, subscriber_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    let project_ids = page_access_guard!(auth, state, StatusPageWrite, status_page_id);
    state
        .subscribers
        .delete(status_page_id, subscriber_id)
        .await?;
    let mut entry = audit(
        &auth,
        &metadata,
        StatusPageSubscriberAction::SubscriberDeleted,
        project_ids,
        status_page_id,
    );
    entry.subscriber_id = Some(subscriber_id);
    record(&state, entry).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Export the subscribers of a status page as CSV
///
/// `email,state,subscribed_at,confirmed_at`, oldest first. Audit-logged.
#[utoipa::path(
    get,
    path = "/status-pages/{status_page_id}/subscribers/export",
    operation_id = "export_status_page_subscribers",
    params(("status_page_id" = i32, Path, description = "Status page ID")),
    responses(
        (status = 200, description = "Subscribers as CSV", content_type = "text/csv", body = String),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Status page not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn export_subscribers(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SubscriberRoutesState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(status_page_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    let project_ids = page_access_guard!(auth, state, StatusPageWrite, status_page_id);
    let (csv, count) = state.subscribers.export_csv(status_page_id).await?;
    let mut entry = audit(
        &auth,
        &metadata,
        StatusPageSubscriberAction::SubscribersExported,
        project_ids,
        status_page_id,
    );
    entry.exported_count = Some(count);
    record(&state, entry).await;
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"status-page-{status_page_id}-subscribers.csv\""),
            ),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        csv,
    ))
}

pub fn create_subscriber_router(state: Arc<SubscriberRoutesState>) -> Router {
    Router::new()
        .route(
            "/status-pages/{status_page_id}/subscriptions",
            get(get_subscription_settings).put(update_subscription_settings),
        )
        .route(
            "/status-pages/{status_page_id}/subscribers",
            get(list_subscribers),
        )
        .route(
            "/status-pages/{status_page_id}/subscribers/export",
            get(export_subscribers),
        )
        .route(
            "/status-pages/{status_page_id}/subscribers/{subscriber_id}",
            delete(delete_subscriber),
        )
        .with_state(state)
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_subscription_settings,
        update_subscription_settings,
        list_subscribers,
        delete_subscriber,
        export_subscribers,
        super::public_subscriptions::subscribe,
        super::public_subscriptions::confirm_form,
        super::public_subscriptions::confirm,
        super::public_subscriptions::unsubscribe_form,
        super::public_subscriptions::unsubscribe,
    ),
    components(schemas(
        SubscriptionSettingsResponse,
        UpdateSubscriptionSettingsRequest,
        SubscriptionCapability,
        SubscriberListResponse,
        SubscriberResponse,
        SubscriberState,
    ))
)]
pub struct SubscriberApiDoc;
