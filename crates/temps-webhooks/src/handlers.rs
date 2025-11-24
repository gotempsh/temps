//! HTTP handlers for webhook management.

use crate::events::WebhookEventType;
use crate::service::{CreateWebhookRequest, UpdateWebhookRequest, WebhookService};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::error_builder::ErrorBuilder;
use temps_core::problemdetails::Problem;
use tracing::{error, info};
use utoipa::{OpenApi, ToSchema};

/// Shared state for webhook handlers
pub struct WebhookState {
    pub webhook_service: Arc<WebhookService>,
}

impl WebhookState {
    pub fn new(webhook_service: Arc<WebhookService>) -> Self {
        Self { webhook_service }
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
    pub response_body: Option<String>,
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
            response_body: delivery.response_body,
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
        ("project_id" = i32, Path, description = "Project ID")
    ),
    tag = "Webhooks",
    security(("bearer_auth" = []))
)]
async fn list_webhooks(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WebhookState>>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, WebhooksRead);

    match state.webhook_service.list_webhooks(project_id).await {
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
    Json(body): Json<CreateWebhookRequestBody>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, WebhooksCreate);

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
    Json(body): Json<UpdateWebhookRequestBody>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, WebhooksWrite);

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
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, WebhooksDelete);

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
    Path((_project_id, _webhook_id, delivery_id)): Path<(i32, i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, WebhooksWrite);

    match state.webhook_service.retry_delivery(delivery_id).await {
        Ok(result) => {
            info!(
                "Retried delivery {}, success: {}",
                delivery_id, result.success
            );
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
