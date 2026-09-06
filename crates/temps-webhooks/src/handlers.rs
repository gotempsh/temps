// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP handlers for webhook management.

use crate::events::WebhookEventType;
use crate::service::{CreateWebhookRequest, UpdateWebhookRequest, WebhookService};
use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{
    permission_check, permission_guard, project_access_guard, Permission, RequireAuth,
};
use temps_core::error_builder::ErrorBuilder;
use temps_core::problemdetails::Problem;
use temps_core::{AuditContext, AuditLogger, AuditOperation, RequestMetadata};
use tracing::{error, info};
use utoipa::{OpenApi, ToSchema};

/// Shared state for webhook handlers
pub struct WebhookState {
    pub webhook_service: Arc<WebhookService>,
    pub audit_service: Arc<dyn AuditLogger>,
    /// Optional checker for team-based project access (human sessions only).
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

impl WebhookState {
    pub fn new(webhook_service: Arc<WebhookService>, audit_service: Arc<dyn AuditLogger>) -> Self {
        Self {
            webhook_service,
            audit_service,
            project_access_checker: None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
struct WebhookAudit {
    context: AuditContext,
    webhook_id: i32,
    action: String,
}

impl AuditOperation for WebhookAudit {
    fn operation_type(&self) -> String {
        self.action.clone()
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

#[derive(OpenApi)]
#[openapi(
    paths(
        list_webhooks,
        get_webhook,
        create_webhook,
        update_webhook,
        delete_webhook,
        list_deliveries,
        get_delivery,
        retry_delivery,
        list_event_types,
    ),
    components(
        schemas(
            WebhookResponse,
            CreateWebhookRequestBody,
            UpdateWebhookRequestBody,
            WebhookDeliveryResponse,
            EventTypeResponse,
        )
    ),
    info(
        title = "Webhooks API",
        description = "API endpoints for managing webhooks and webhook deliveries",
        version = "1.0.0"
    ),
    tags(
        (name = "Webhooks", description = "Webhook management endpoints"),
        (name = "Webhook Deliveries", description = "Webhook delivery history and retry endpoints")
    )
)]
pub struct WebhooksApiDoc;

// ============================================================================
// Request/Response Types
// ============================================================================

#[derive(Debug, Serialize, ToSchema)]
pub struct WebhookResponse {
    pub id: i32,
    pub project_id: i32,
    pub url: String,
    pub events: Vec<String>,
    pub enabled: bool,
    pub has_secret: bool,
    #[schema(example = "2025-10-12T12:15:47.609192Z")]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[schema(example = "2025-10-12T12:15:47.609192Z")]
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<temps_entities::webhooks::Model> for WebhookResponse {
    fn from(webhook: temps_entities::webhooks::Model) -> Self {
        let events: Vec<String> = serde_json::from_str(&webhook.events).unwrap_or_default();
        Self {
            id: webhook.id,
            project_id: webhook.project_id,
            url: webhook.url,
            events,
            enabled: webhook.enabled,
            has_secret: webhook.secret.is_some(),
            created_at: webhook.created_at,
            updated_at: webhook.updated_at,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateWebhookRequestBody {
    /// Target URL for webhook delivery
    #[schema(example = "https://example.com/webhook")]
    pub url: String,
    /// Secret for HMAC signature verification (optional)
    pub secret: Option<String>,
    /// Event types to subscribe to
    #[schema(example = json!(["deployment.created", "deployment.succeeded"]))]
    pub events: Vec<String>,
    /// Whether the webhook is enabled
    #[schema(default = true)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateWebhookRequestBody {
    /// Target URL for webhook delivery
    pub url: Option<String>,
    /// Secret for HMAC signature verification
    pub secret: Option<String>,
    /// Event types to subscribe to
    pub events: Option<Vec<String>>,
    /// Whether the webhook is enabled
    pub enabled: Option<bool>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WebhookDeliveryResponse {
    pub id: i32,
    pub webhook_id: i32,
    pub event_type: String,
    pub event_id: String,
    /// JSON payload that was sent to the webhook endpoint
    #[schema(example = json!({"event_type": "deployment.succeeded", "data": {"deployment_id": 123}}))]
    pub payload: String,
    pub success: bool,
    pub status_code: Option<i32>,
    // SECURITY: response_body field removed to prevent data exfiltration via SSRF
    // This prevents attackers from reading responses from internal services
    pub error_message: Option<String>,
    pub attempt_number: i32,
    #[schema(example = "2025-10-12T12:15:47.609192Z")]
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub delivered_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<temps_entities::webhook_deliveries::Model> for WebhookDeliveryResponse {
    fn from(delivery: temps_entities::webhook_deliveries::Model) -> Self {
        Self {
            id: delivery.id,
            webhook_id: delivery.webhook_id,
            event_type: delivery.event_type,
            event_id: delivery.event_id,
            payload: delivery.payload,
            success: delivery.success,
            status_code: delivery.status_code,
            // SECURITY: response_body not included in API response
            error_message: delivery.error_message,
            attempt_number: delivery.attempt_number,
            created_at: delivery.created_at,
            delivered_at: delivery.delivered_at,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EventTypeResponse {
    pub event_type: String,
    pub description: String,
    pub category: String,
}

#[derive(Debug, Deserialize)]
pub struct ListDeliveriesQuery {
    pub limit: Option<u64>,
}

// ============================================================================
// Handlers
// ============================================================================

/// List all webhooks for a project
#[utoipa::path(
    get,
    path = "/projects/{project_id}/webhooks",
    responses(
        (status = 200, description = "List of webhooks", body = Vec<WebhookResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        temps_core::PaginationParams,
    ),
    tag = "Webhooks",
    security(("bearer_auth" = []))
)]
async fn list_webhooks(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WebhookState>>,
    Path(project_id): Path<i32>,
    Query(pagination): Query<temps_core::PaginationParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, WebhooksRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let (page, page_size) = pagination.normalize();

    match state
        .webhook_service
        .list_webhooks_paginated(project_id, page, page_size)
        .await
    {
        Ok(webhooks) => {
            let responses: Vec<WebhookResponse> = webhooks.into_iter().map(Into::into).collect();
            Ok(Json(responses))
        }
        Err(e) => {
            error!("Failed to list webhooks: {}", e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to list webhooks")
                .detail(e.to_string())
                .build())
        }
    }
}

/// Get a specific webhook
#[utoipa::path(
    get,
    path = "/projects/{project_id}/webhooks/{webhook_id}",
    responses(
        (status = 200, description = "Webhook details", body = WebhookResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Webhook not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("webhook_id" = i32, Path, description = "Webhook ID")
    ),
    tag = "Webhooks",
    security(("bearer_auth" = []))
)]
async fn get_webhook(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WebhookState>>,
    Path((project_id, webhook_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, WebhooksRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    match state.webhook_service.get_webhook(webhook_id).await {
        Ok(Some(webhook)) => {
            if webhook.project_id != project_id {
                return Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
                    .title("Webhook not found")
                    .detail("Webhook does not belong to this project")
                    .build());
            }
            Ok(Json(WebhookResponse::from(webhook)))
        }
        Ok(None) => Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
            .title("Webhook not found")
            .build()),
        Err(e) => {
            error!("Failed to get webhook: {}", e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get webhook")
                .detail(e.to_string())
                .build())
        }
    }
}

/// Backup webhook payloads carry the same metadata (S3 locations, sizes, raw
/// engine failure text) as the `BackupsRead`-gated local backup API. Without
/// this, `WebhooksCreate` alone -- a much broader, commonly-granted
/// permission -- would let a principal without any backup access route that
/// data to a URL of their choosing and read it back via the webhook's
/// delivery log.
fn subscribes_to_backup_events(events: &[WebhookEventType]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            WebhookEventType::BackupStarted
                | WebhookEventType::BackupCompleted
                | WebhookEventType::BackupFailed
        )
    })
}

/// Create a new webhook
#[utoipa::path(
    post,
    path = "/projects/{project_id}/webhooks",
    request_body = CreateWebhookRequestBody,
    responses(
        (status = 201, description = "Webhook created", body = WebhookResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    tag = "Webhooks",
    security(("bearer_auth" = []))
)]
async fn create_webhook(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WebhookState>>,
    Path(project_id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(body): Json<CreateWebhookRequestBody>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, WebhooksCreate);
    project_access_guard!(auth, project_id, state.project_access_checker);

    // Parse event types
    let events: Vec<WebhookEventType> = body
        .events
        .iter()
        .filter_map(|e| WebhookEventType::from_str(e))
        .collect();

    if events.is_empty() {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Invalid event types")
            .detail("At least one valid event type is required")
            .build());
    }
    if subscribes_to_backup_events(&events) {
        permission_check!(auth, Permission::BackupsRead);
    }

    let request = CreateWebhookRequest {
        project_id,
        url: body.url,
        secret: body.secret,
        events,
        enabled: body.enabled.unwrap_or(true),
    };

    match state.webhook_service.create_webhook(request).await {
        Ok(webhook) => {
            info!("Created webhook {} for project {}", webhook.id, project_id);

            let audit = WebhookAudit {
                context: AuditContext {
                    user_id: auth.user_id(),
                    ip_address: Some(metadata.ip_address.clone()),
                    user_agent: metadata.user_agent.clone(),
                },
                webhook_id: webhook.id,
                action: "WEBHOOK_CREATED".to_string(),
            };
            if let Err(e) = state.audit_service.create_audit_log(&audit).await {
                error!("Failed to create audit log: {}", e);
            }

            Ok((StatusCode::CREATED, Json(WebhookResponse::from(webhook))))
        }
        Err(e) => {
            error!("Failed to create webhook: {}", e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to create webhook")
                .detail(e.to_string())
                .build())
        }
    }
}

/// Update a webhook
#[utoipa::path(
    put,
    path = "/projects/{project_id}/webhooks/{webhook_id}",
    request_body = UpdateWebhookRequestBody,
    responses(
        (status = 200, description = "Webhook updated", body = WebhookResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Webhook not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("webhook_id" = i32, Path, description = "Webhook ID")
    ),
    tag = "Webhooks",
    security(("bearer_auth" = []))
)]
async fn update_webhook(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WebhookState>>,
    Path((project_id, webhook_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(body): Json<UpdateWebhookRequestBody>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, WebhooksWrite);
    project_access_guard!(auth, project_id, state.project_access_checker);

    // Verify webhook belongs to project
    if let Ok(Some(existing)) = state.webhook_service.get_webhook(webhook_id).await {
        if existing.project_id != project_id {
            return Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
                .title("Webhook not found")
                .detail("Webhook does not belong to this project")
                .build());
        }
    }

    // Parse event types if provided
    let events: Option<Vec<WebhookEventType>> = body.events.map(|e| {
        e.iter()
            .filter_map(|s| WebhookEventType::from_str(s))
            .collect()
    });
    if let Some(events) = &events {
        if subscribes_to_backup_events(events) {
            permission_check!(auth, Permission::BackupsRead);
        }
    }

    let request = UpdateWebhookRequest {
        url: body.url,
        secret: body.secret,
        events,
        enabled: body.enabled,
    };

    match state
        .webhook_service
        .update_webhook(webhook_id, request)
        .await
    {
        Ok(Some(webhook)) => {
            info!("Updated webhook {}", webhook_id);

            let audit = WebhookAudit {
                context: AuditContext {
                    user_id: auth.user_id(),
                    ip_address: Some(metadata.ip_address.clone()),
                    user_agent: metadata.user_agent.clone(),
                },
                webhook_id,
                action: "WEBHOOK_UPDATED".to_string(),
            };
            if let Err(e) = state.audit_service.create_audit_log(&audit).await {
                error!("Failed to create audit log: {}", e);
            }

            Ok(Json(WebhookResponse::from(webhook)))
        }
        Ok(None) => Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
            .title("Webhook not found")
            .build()),
        Err(e) => {
            error!("Failed to update webhook: {}", e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to update webhook")
                .detail(e.to_string())
                .build())
        }
    }
}

/// Delete a webhook
#[utoipa::path(
    delete,
    path = "/projects/{project_id}/webhooks/{webhook_id}",
    responses(
        (status = 204, description = "Webhook deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Webhook not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("webhook_id" = i32, Path, description = "Webhook ID")
    ),
    tag = "Webhooks",
    security(("bearer_auth" = []))
)]
async fn delete_webhook(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WebhookState>>,
    Path((project_id, webhook_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, WebhooksDelete);
    project_access_guard!(auth, project_id, state.project_access_checker);

    // Verify webhook belongs to project
    if let Ok(Some(existing)) = state.webhook_service.get_webhook(webhook_id).await {
        if existing.project_id != project_id {
            return Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
                .title("Webhook not found")
                .detail("Webhook does not belong to this project")
                .build());
        }
    }

    match state.webhook_service.delete_webhook(webhook_id).await {
        Ok(true) => {
            info!("Deleted webhook {}", webhook_id);

            let audit = WebhookAudit {
                context: AuditContext {
                    user_id: auth.user_id(),
                    ip_address: Some(metadata.ip_address.clone()),
                    user_agent: metadata.user_agent.clone(),
                },
                webhook_id,
                action: "WEBHOOK_DELETED".to_string(),
            };
            if let Err(e) = state.audit_service.create_audit_log(&audit).await {
                error!("Failed to create audit log: {}", e);
            }

            Ok(StatusCode::NO_CONTENT)
        }
        Ok(false) => Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
            .title("Webhook not found")
            .build()),
        Err(e) => {
            error!("Failed to delete webhook: {}", e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to delete webhook")
                .detail(e.to_string())
                .build())
        }
    }
}

/// List webhook deliveries
#[utoipa::path(
    get,
    path = "/projects/{project_id}/webhooks/{webhook_id}/deliveries",
    responses(
        (status = 200, description = "List of deliveries", body = Vec<WebhookDeliveryResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("webhook_id" = i32, Path, description = "Webhook ID"),
        ("limit" = Option<u64>, Query, description = "Number of deliveries to return (default: 50)")
    ),
    tag = "Webhook Deliveries",
    security(("bearer_auth" = []))
)]
async fn list_deliveries(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WebhookState>>,
    Path((project_id, webhook_id)): Path<(i32, i32)>,
    Query(query): Query<ListDeliveriesQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, WebhooksRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    // Verify webhook belongs to project
    if let Ok(Some(existing)) = state.webhook_service.get_webhook(webhook_id).await {
        if existing.project_id != project_id {
            return Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
                .title("Webhook not found")
                .detail("Webhook does not belong to this project")
                .build());
        }
    }

    let limit = query.limit.unwrap_or(50).min(100);

    match state
        .webhook_service
        .get_deliveries(webhook_id, limit)
        .await
    {
        Ok(deliveries) => {
            let responses: Vec<WebhookDeliveryResponse> =
                deliveries.into_iter().map(Into::into).collect();
            Ok(Json(responses))
        }
        Err(e) => {
            error!("Failed to list deliveries: {}", e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to list deliveries")
                .detail(e.to_string())
                .build())
        }
    }
}

/// Get a specific webhook delivery by ID
#[utoipa::path(
    get,
    path = "/projects/{project_id}/webhooks/{webhook_id}/deliveries/{delivery_id}",
    responses(
        (status = 200, description = "Delivery details including full payload", body = WebhookDeliveryResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Delivery not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("webhook_id" = i32, Path, description = "Webhook ID"),
        ("delivery_id" = i32, Path, description = "Delivery ID")
    ),
    tag = "Webhook Deliveries",
    security(("bearer_auth" = []))
)]
async fn get_delivery(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WebhookState>>,
    Path((project_id, webhook_id, delivery_id)): Path<(i32, i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, WebhooksRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    // Verify webhook belongs to project
    if let Ok(Some(existing)) = state.webhook_service.get_webhook(webhook_id).await {
        if existing.project_id != project_id {
            return Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
                .title("Webhook not found")
                .detail("Webhook does not belong to this project")
                .build());
        }
    }

    // Get the delivery
    match state.webhook_service.get_delivery(delivery_id).await {
        Ok(Some(delivery)) => {
            // Verify delivery belongs to the webhook
            if delivery.webhook_id != webhook_id {
                return Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
                    .title("Delivery not found")
                    .detail("Delivery does not belong to this webhook")
                    .build());
            }
            Ok(Json(WebhookDeliveryResponse::from(delivery)))
        }
        Ok(None) => Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
            .title("Delivery not found")
            .build()),
        Err(e) => {
            error!("Failed to get delivery: {}", e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get delivery")
                .detail(e.to_string())
                .build())
        }
    }
}

/// Retry a failed delivery
#[utoipa::path(
    post,
    path = "/projects/{project_id}/webhooks/{webhook_id}/deliveries/{delivery_id}/retry",
    responses(
        (status = 200, description = "Delivery retried", body = WebhookDeliveryResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "Delivery not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("webhook_id" = i32, Path, description = "Webhook ID"),
        ("delivery_id" = i32, Path, description = "Delivery ID")
    ),
    tag = "Webhook Deliveries",
    security(("bearer_auth" = []))
)]
async fn retry_delivery(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WebhookState>>,
    Path((project_id, webhook_id, delivery_id)): Path<(i32, i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, WebhooksWrite);
    project_access_guard!(auth, project_id, state.project_access_checker);

    match state
        .webhook_service
        .retry_delivery(project_id, webhook_id, delivery_id)
        .await
    {
        Ok(result) => {
            info!(
                "Retried delivery {}, success: {}",
                delivery_id, result.success
            );

            let audit = WebhookAudit {
                context: AuditContext {
                    user_id: auth.user_id(),
                    ip_address: Some(metadata.ip_address.clone()),
                    user_agent: metadata.user_agent.clone(),
                },
                webhook_id,
                action: "WEBHOOK_DELIVERY_RETRIED".to_string(),
            };
            if let Err(e) = state.audit_service.create_audit_log(&audit).await {
                error!("Failed to create audit log: {}", e);
            }

            Ok(Json(serde_json::json!({
                "success": result.success,
                "status_code": result.status_code,
                "error_message": result.error_message,
                "attempt_number": result.attempt_number,
            })))
        }
        Err(e @ crate::service::WebhookError::DeliveryNotInScope { .. }) => {
            Err(retry_delivery_problem(e))
        }
        Err(e) => {
            error!("Failed to retry delivery: {}", e);
            Err(retry_delivery_problem(e))
        }
    }
}

fn retry_delivery_problem(error: crate::service::WebhookError) -> Problem {
    match error {
        crate::service::WebhookError::DeliveryNotInScope { .. } => {
            ErrorBuilder::new(StatusCode::NOT_FOUND)
                .title("Delivery not found")
                .detail("Delivery does not exist in the requested project and webhook")
                .build()
        }
        other => ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
            .title("Failed to retry delivery")
            .detail(other.to_string())
            .build(),
    }
}

/// List available event types
#[utoipa::path(
    get,
    path = "/webhook-event-types",
    responses(
        (status = 200, description = "List of available event types", body = Vec<EventTypeResponse>),
    ),
    tag = "Webhooks",
)]
async fn list_event_types() -> impl IntoResponse {
    let event_types = vec![
        EventTypeResponse {
            event_type: "deployment.created".to_string(),
            description: "Triggered when a new deployment is initiated".to_string(),
            category: "Deployment".to_string(),
        },
        EventTypeResponse {
            event_type: "deployment.succeeded".to_string(),
            description: "Triggered when a deployment completes successfully".to_string(),
            category: "Deployment".to_string(),
        },
        EventTypeResponse {
            event_type: "deployment.failed".to_string(),
            description: "Triggered when a deployment fails".to_string(),
            category: "Deployment".to_string(),
        },
        EventTypeResponse {
            event_type: "deployment.cancelled".to_string(),
            description: "Triggered when a deployment is cancelled".to_string(),
            category: "Deployment".to_string(),
        },
        EventTypeResponse {
            event_type: "deployment.ready".to_string(),
            description: "Triggered when a deployment is ready to receive traffic".to_string(),
            category: "Deployment".to_string(),
        },
        EventTypeResponse {
            event_type: "project.created".to_string(),
            description: "Triggered when a new project is created".to_string(),
            category: "Project".to_string(),
        },
        EventTypeResponse {
            event_type: "project.deleted".to_string(),
            description: "Triggered when a project is deleted".to_string(),
            category: "Project".to_string(),
        },
        EventTypeResponse {
            event_type: "domain.created".to_string(),
            description: "Triggered when a new domain is added to a project".to_string(),
            category: "Domain".to_string(),
        },
        EventTypeResponse {
            event_type: "domain.provisioned".to_string(),
            description: "Triggered when SSL is provisioned for a domain".to_string(),
            category: "Domain".to_string(),
        },
    ];

    Json(event_types)
}

/// Configure webhook routes
pub fn configure_routes() -> Router<Arc<WebhookState>> {
    Router::new()
        // Event types (no auth required for listing available types)
        .route("/webhook-event-types", get(list_event_types))
        // Webhook CRUD
        .route(
            "/projects/{project_id}/webhooks",
            get(list_webhooks).post(create_webhook),
        )
        .route(
            "/projects/{project_id}/webhooks/{webhook_id}",
            get(get_webhook).put(update_webhook).delete(delete_webhook),
        )
        // Deliveries
        .route(
            "/projects/{project_id}/webhooks/{webhook_id}/deliveries",
            get(list_deliveries),
        )
        .route(
            "/projects/{project_id}/webhooks/{webhook_id}/deliveries/{delivery_id}",
            get(get_delivery),
        )
        .route(
            "/projects/{project_id}/webhooks/{webhook_id}/deliveries/{delivery_id}/retry",
            post(retry_delivery),
        )
}

/// Tests for `subscribes_to_backup_events` and the permission gate that wraps
/// it in `create_webhook` / `update_webhook`.
///
/// Handler-level integration tests (calling the full handler function with a
/// constructed `WebhookState` + `AuditLogger` + `RequestMetadata`) are not
/// included here because this crate has no existing harness for that pattern —
/// building one from scratch would be disproportionate relative to what is
/// already established. Instead, `backup_subscription_gate` below reproduces
/// the verbatim two-line gate from both handlers, which is sufficient to assert
/// the 403-vs-proceed branching behaviour.
#[cfg(test)]
mod backup_permission_tests {
    use super::subscribes_to_backup_events;
    use crate::events::WebhookEventType;
    use axum::http::StatusCode;
    use chrono::Utc;
    use temps_auth::{permission_check, AuthContext, Permission};
    use temps_core::problemdetails::Problem;
    use temps_entities::users;

    fn test_user() -> users::Model {
        let now = Utc::now();
        users::Model {
            id: 1,
            name: "Test User".to_string(),
            email: "test@example.com".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Build an `AuthContext` with explicit custom permissions and no predefined
    /// role. `AuthContext::has_permission` checks `custom_permissions` first,
    /// so only the supplied list is effective — no role-based fallback.
    fn auth_with_permissions(permissions: Vec<Permission>) -> AuthContext {
        AuthContext::new_api_key(
            test_user(),
            None,              // no predefined role
            Some(permissions), // custom permission set
            "test-key".to_string(),
            1,
        )
    }

    /// Mirrors the exact security gate from `create_webhook` and `update_webhook`:
    ///
    /// ```text
    /// if subscribes_to_backup_events(&events) {
    ///     permission_check!(auth, Permission::BackupsRead);
    /// }
    /// ```
    fn backup_subscription_gate(
        auth: &AuthContext,
        events: &[WebhookEventType],
    ) -> Result<(), Problem> {
        if subscribes_to_backup_events(events) {
            permission_check!(auth, Permission::BackupsRead);
        }
        Ok(())
    }

    // -------------------------------------------------------------------------
    // subscribes_to_backup_events unit tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_subscribes_to_backup_events_true_when_backup_started_present() {
        assert!(subscribes_to_backup_events(&[
            WebhookEventType::BackupStarted
        ]));
    }

    #[test]
    fn test_subscribes_to_backup_events_true_when_backup_completed_present() {
        assert!(subscribes_to_backup_events(&[
            WebhookEventType::BackupCompleted
        ]));
    }

    #[test]
    fn test_subscribes_to_backup_events_true_when_backup_failed_present() {
        assert!(subscribes_to_backup_events(&[
            WebhookEventType::BackupFailed
        ]));
    }

    #[test]
    fn test_subscribes_to_backup_events_true_when_backup_event_in_mixed_list() {
        let events = vec![
            WebhookEventType::DeploymentCreated,
            WebhookEventType::BackupCompleted,
            WebhookEventType::ProjectDeleted,
        ];
        assert!(subscribes_to_backup_events(&events));
    }

    #[test]
    fn test_subscribes_to_backup_events_false_for_deployment_events_only() {
        let events = vec![
            WebhookEventType::DeploymentCreated,
            WebhookEventType::DeploymentSucceeded,
            WebhookEventType::DeploymentFailed,
            WebhookEventType::DeploymentCancelled,
            WebhookEventType::DeploymentReady,
        ];
        assert!(!subscribes_to_backup_events(&events));
    }

    #[test]
    fn test_subscribes_to_backup_events_false_for_project_events() {
        let events = vec![
            WebhookEventType::ProjectCreated,
            WebhookEventType::ProjectDeleted,
        ];
        assert!(!subscribes_to_backup_events(&events));
    }

    #[test]
    fn test_subscribes_to_backup_events_false_for_domain_events() {
        let events = vec![
            WebhookEventType::DomainCreated,
            WebhookEventType::DomainProvisioned,
        ];
        assert!(!subscribes_to_backup_events(&events));
    }

    #[test]
    fn test_subscribes_to_backup_events_false_for_email_events() {
        let events = vec![
            WebhookEventType::EmailDelivered,
            WebhookEventType::EmailBounced,
            WebhookEventType::EmailComplained,
        ];
        assert!(!subscribes_to_backup_events(&events));
    }

    #[test]
    fn test_subscribes_to_backup_events_false_for_empty_list() {
        assert!(!subscribes_to_backup_events(&[]));
    }

    // -------------------------------------------------------------------------
    // Security gate tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_backup_gate_denied_when_webhooks_create_only_and_backup_started() {
        let auth = auth_with_permissions(vec![Permission::WebhooksCreate]);
        let result = backup_subscription_gate(&auth, &[WebhookEventType::BackupStarted]);
        let err = result.expect_err("WebhooksCreate alone must not clear the BackupsRead gate");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_backup_gate_denied_when_webhooks_create_only_and_backup_completed() {
        let auth = auth_with_permissions(vec![Permission::WebhooksCreate]);
        let result = backup_subscription_gate(&auth, &[WebhookEventType::BackupCompleted]);
        assert_eq!(
            result.expect_err("must be denied").status_code,
            StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn test_backup_gate_denied_when_webhooks_create_only_and_backup_failed() {
        let auth = auth_with_permissions(vec![Permission::WebhooksCreate]);
        let result = backup_subscription_gate(&auth, &[WebhookEventType::BackupFailed]);
        assert_eq!(
            result.expect_err("must be denied").status_code,
            StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn test_backup_gate_denied_when_single_backup_event_mixed_into_otherwise_innocent_list() {
        // One backup event hidden among deployment events must still trigger the gate.
        let auth = auth_with_permissions(vec![Permission::WebhooksCreate]);
        let events = vec![
            WebhookEventType::DeploymentSucceeded,
            WebhookEventType::BackupFailed,
        ];
        let result = backup_subscription_gate(&auth, &events);
        assert_eq!(
            result
                .expect_err("one backup event in the list must trigger the gate")
                .status_code,
            StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn test_backup_gate_allowed_when_webhooks_create_and_backups_read_both_present() {
        let auth = auth_with_permissions(vec![Permission::WebhooksCreate, Permission::BackupsRead]);
        let result = backup_subscription_gate(&auth, &[WebhookEventType::BackupCompleted]);
        assert!(
            result.is_ok(),
            "WebhooksCreate + BackupsRead must satisfy the backup-event gate"
        );
    }

    #[test]
    fn test_backup_gate_allowed_when_webhooks_create_only_and_no_backup_events_in_list() {
        // Non-backup events pass without any BackupsRead check — this is the
        // "create/update_webhook with deployment events succeeds for a
        // WebhooksCreate-only principal" case from the task requirements.
        let auth = auth_with_permissions(vec![Permission::WebhooksCreate]);
        let events = vec![
            WebhookEventType::DeploymentCreated,
            WebhookEventType::DeploymentSucceeded,
            WebhookEventType::ProjectDeleted,
        ];
        let result = backup_subscription_gate(&auth, &events);
        assert!(
            result.is_ok(),
            "WebhooksCreate alone is sufficient when no backup events are requested"
        );
    }
}

#[cfg(test)]
mod retry_delivery_tests {
    use super::retry_delivery_problem;
    use crate::service::WebhookError;
    use axum::http::StatusCode;

    #[test]
    fn every_out_of_scope_retry_has_an_identical_non_enumerating_404() {
        let errors = [
            WebhookError::DeliveryNotInScope {
                project_id: 1,
                webhook_id: 10,
                delivery_id: 20,
            },
            WebhookError::DeliveryNotInScope {
                project_id: 1,
                webhook_id: 99,
                delivery_id: 20,
            },
            WebhookError::DeliveryNotInScope {
                project_id: 1,
                webhook_id: 10,
                delivery_id: 999,
            },
        ];

        let problems = errors.map(retry_delivery_problem);
        let mut expected_body = problems[0].body.clone();
        expected_body.remove("timestamp");
        for problem in &problems {
            assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
            let mut body = problem.body.clone();
            body.remove("timestamp");
            assert_eq!(body, expected_body);
            let title_and_detail = format!(
                "{} {}",
                body.get("title").unwrap(),
                body.get("detail").unwrap()
            );
            assert!(!title_and_detail.contains("20"));
            assert!(!title_and_detail.contains("99"));
            assert!(!title_and_detail.contains("999"));
        }
    }
}
