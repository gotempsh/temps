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
use temps_auth::{permission_guard, project_access_guard, RequireAuth};
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
    let events = body.events.map(|e| {
        e.iter()
            .filter_map(|s| WebhookEventType::from_str(s))
            .collect()
    });

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

/// Why a delivery is not in scope for the requested webhook/project path.
#[derive(Debug, PartialEq, Eq)]
enum DeliveryScopeError {
    /// The webhook does not exist or belongs to a different project.
    WebhookNotInProject,
    /// The delivery does not exist or belongs to a different webhook.
    DeliveryNotOnWebhook,
}

impl DeliveryScopeError {
    fn detail(&self) -> &'static str {
        match self {
            Self::WebhookNotInProject => "Webhook does not belong to this project",
            Self::DeliveryNotOnWebhook => "Delivery does not belong to this webhook",
        }
    }
}

/// Verify the delivery→webhook→project ownership chain for delivery-scoped
/// endpoints. `webhook_project_id`/`delivery_webhook_id` are `None` when the
/// row was not found. Factored out so the IDOR guard can be unit-tested without
/// a full HTTP + service harness.
fn verify_delivery_scope(
    webhook_project_id: Option<i32>,
    delivery_webhook_id: Option<i32>,
    path_project_id: i32,
    path_webhook_id: i32,
) -> Result<(), DeliveryScopeError> {
    match webhook_project_id {
        Some(pid) if pid == path_project_id => {}
        _ => return Err(DeliveryScopeError::WebhookNotInProject),
    }
    match delivery_webhook_id {
        Some(wid) if wid == path_webhook_id => Ok(()),
        _ => Err(DeliveryScopeError::DeliveryNotOnWebhook),
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

    // Verify the delivery belongs to this webhook, and the webhook to this
    // project, before retrying. Without this, a caller with WebhooksWrite on any
    // one project could replay another tenant's delivery by delivery_id alone and
    // read back the response (security review finding #5). Mirrors get_delivery.
    let webhook_project_id = match state.webhook_service.get_webhook(webhook_id).await {
        Ok(existing) => existing.map(|w| w.project_id),
        Err(e) => {
            error!("Failed to load webhook: {}", e);
            return Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to retry delivery")
                .detail(e.to_string())
                .build());
        }
    };
    let delivery_webhook_id = match state.webhook_service.get_delivery(delivery_id).await {
        Ok(existing) => existing.map(|d| d.webhook_id),
        Err(e) => {
            error!("Failed to load delivery: {}", e);
            return Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to retry delivery")
                .detail(e.to_string())
                .build());
        }
    };
    if let Err(scope_err) = verify_delivery_scope(
        webhook_project_id,
        delivery_webhook_id,
        project_id,
        webhook_id,
    ) {
        return Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
            .title("Delivery not found")
            .detail(scope_err.detail())
            .build());
    }

    match state.webhook_service.retry_delivery(delivery_id).await {
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
        Err(e) => {
            error!("Failed to retry delivery: {}", e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to retry delivery")
                .detail(e.to_string())
                .build())
        }
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

#[cfg(test)]
mod idor_tests {
    //! Regression tests for the retry_delivery IDOR (security review finding
    //! #5). retry_delivery looked the delivery up by delivery_id alone, so a
    //! caller with WebhooksWrite on their own project could replay another
    //! tenant's delivery. verify_delivery_scope is the extracted ownership check.

    use super::{verify_delivery_scope, DeliveryScopeError};

    // path: /projects/1/webhooks/10/deliveries/... — caller owns project 1.
    const PATH_PROJECT: i32 = 1;
    const PATH_WEBHOOK: i32 = 10;

    #[test]
    fn in_scope_delivery_is_allowed() {
        // webhook 10 is in project 1, delivery is on webhook 10.
        assert_eq!(
            verify_delivery_scope(Some(1), Some(10), PATH_PROJECT, PATH_WEBHOOK),
            Ok(())
        );
    }

    #[test]
    fn cross_project_webhook_is_rejected() {
        // The exploit: webhook 10 actually belongs to another project (2).
        assert_eq!(
            verify_delivery_scope(Some(2), Some(10), PATH_PROJECT, PATH_WEBHOOK),
            Err(DeliveryScopeError::WebhookNotInProject)
        );
    }

    #[test]
    fn delivery_from_another_webhook_is_rejected() {
        // The core IDOR: delivery_id belongs to a different webhook (99).
        assert_eq!(
            verify_delivery_scope(Some(1), Some(99), PATH_PROJECT, PATH_WEBHOOK),
            Err(DeliveryScopeError::DeliveryNotOnWebhook)
        );
    }

    #[test]
    fn missing_webhook_or_delivery_is_rejected() {
        assert_eq!(
            verify_delivery_scope(None, Some(10), PATH_PROJECT, PATH_WEBHOOK),
            Err(DeliveryScopeError::WebhookNotInProject)
        );
        assert_eq!(
            verify_delivery_scope(Some(1), None, PATH_PROJECT, PATH_WEBHOOK),
            Err(DeliveryScopeError::DeliveryNotOnWebhook)
        );
    }
}
