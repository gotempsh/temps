//! HTTP handlers for email tracking
//!
//! Public endpoints (no auth):
//! - GET /t/pixel/{email_id}/{hmac}.gif — tracking pixel
//! - GET /t/click/{email_id}/{hmac}/{url} — click redirect
//! - POST /t/webhook/ses — SES/SNS event webhook
//!
//! Authenticated endpoints:
//! - GET /emails/{email_id}/events — list events for an email
//! - GET /emails/events/stats — aggregate event stats

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use tracing::{debug, error, warn};
use utoipa::{OpenApi, ToSchema};
use uuid::Uuid;

use crate::event_service::{EmailEventService, EmailEventStats, ListEmailEventsOptions};
use crate::hmac::verify_tracking_hmac;
use crate::sns::SnsVerifier;

/// Shared state for tracking handlers
pub struct TrackingState {
    pub event_service: Arc<EmailEventService>,
    pub sns_verifier: Arc<SnsVerifier>,
    pub hmac_key: Vec<u8>,
}

/// OpenAPI documentation
#[derive(OpenApi)]
#[openapi(
    paths(
        list_all_email_events,
        list_email_events,
        get_email_event_stats,
    ),
    components(schemas(
        EmailEventResponse,
        EmailEventStatsResponse,
        PaginatedEmailEventsResponse,
    )),
    tags(
        (name = "email-tracking", description = "Email tracking and event endpoints")
    )
)]
pub struct EmailTrackingApiDoc;

// ============================================================
// PUBLIC ENDPOINTS (no auth)
// ============================================================

/// 1x1 transparent GIF (43 bytes)
const TRANSPARENT_GIF: &[u8] = &[
    0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0xff, 0xff, 0xff,
    0x00, 0x00, 0x00, 0x21, 0xf9, 0x04, 0x01, 0x00, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3b,
];

fn gif_response() -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/gif"),
            (header::CACHE_CONTROL, "no-store, no-cache, must-revalidate"),
            (header::PRAGMA, "no-cache"),
        ],
        Bytes::from_static(TRANSPARENT_GIF),
    )
        .into_response()
}

/// Extract client IP from X-Forwarded-For or connection info
fn extract_ip(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
}

/// GET /t/pixel/{email_id}/{hmac_with_ext}
///
/// Always returns a 1x1 transparent GIF. Records an "opened" event via fire-and-forget.
/// Never returns an error response — prevents email_id enumeration.
async fn pixel_handler(
    Path((email_id_str, hmac_with_ext)): Path<(String, String)>,
    State(state): State<Arc<TrackingState>>,
    headers: HeaderMap,
) -> Response {
    // Parse email_id — return GIF regardless of validity
    let email_id = match Uuid::parse_str(&email_id_str) {
        Ok(id) => id,
        Err(_) => return gif_response(),
    };

    // Validate HMAC — strip .gif extension
    let hmac_str = hmac_with_ext.strip_suffix(".gif").unwrap_or(&hmac_with_ext);
    if !verify_tracking_hmac(&state.hmac_key, &email_id_str, "open", hmac_str) {
        return gif_response();
    }

    // Fire-and-forget: record open event
    let event_service = state.event_service.clone();
    let ip = extract_ip(&headers);
    let ua = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    tokio::spawn(async move {
        if let Err(e) = event_service
            .record_event(email_id, "opened", None, None, None, ip, ua)
            .await
        {
            warn!("Failed to record open event for email {}: {}", email_id, e);
        }
    });

    gif_response()
}

/// GET /t/click/{email_id}/{hmac}/{encoded_url}
///
/// Verifies the HMAC, records a "clicked" event, and redirects to the original URL.
/// Invalid HMAC redirects to "/" (prevents email_id enumeration).
async fn click_handler(
    Path((email_id_str, hmac_str, encoded_url)): Path<(String, String, String)>,
    State(state): State<Arc<TrackingState>>,
    headers: HeaderMap,
) -> Response {
    let url = urlencoding::decode(&encoded_url)
        .unwrap_or_default()
        .to_string();

    let email_id = match Uuid::parse_str(&email_id_str) {
        Ok(id) => id,
        Err(_) => return Redirect::temporary("/").into_response(),
    };

    // Invalid HMAC → redirect to homepage (not 400, prevents enumeration)
    if !verify_tracking_hmac(&state.hmac_key, &email_id_str, &url, &hmac_str) {
        return Redirect::temporary("/").into_response();
    }

    // Validate URL scheme — only allow http/https
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Redirect::temporary("/").into_response();
    }

    // Fire-and-forget: record click event
    let event_service = state.event_service.clone();
    let ip = extract_ip(&headers);
    let ua = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let url_clone = url.clone();

    tokio::spawn(async move {
        let metadata = serde_json::json!({ "url": url_clone });
        if let Err(e) = event_service
            .record_event(email_id, "clicked", None, None, Some(metadata), ip, ua)
            .await
        {
            warn!("Failed to record click event for email {}: {}", email_id, e);
        }
    });

    Redirect::temporary(&url).into_response()
}

/// POST /t/webhook/ses
///
/// Receives SNS notifications from AWS SES for bounces, complaints, and deliveries.
async fn ses_webhook_handler(State(state): State<Arc<TrackingState>>, body: String) -> Response {
    // Parse SNS envelope
    let sns_message: crate::sns::SnsMessage = match serde_json::from_str(&body) {
        Ok(m) => m,
        Err(e) => {
            warn!("Invalid SNS message body: {}", e);
            return StatusCode::BAD_REQUEST.into_response();
        }
    };

    // Verify signature
    if let Err(e) = state.sns_verifier.verify_signature(&sns_message).await {
        warn!("SNS signature verification failed: {}", e);
        return StatusCode::FORBIDDEN.into_response();
    }

    match sns_message.message_type.as_str() {
        "SubscriptionConfirmation" => {
            match state.sns_verifier.confirm_subscription(&sns_message).await {
                Ok(_) => StatusCode::OK.into_response(),
                Err(e) => {
                    error!("Failed to confirm SNS subscription: {}", e);
                    StatusCode::INTERNAL_SERVER_ERROR.into_response()
                }
            }
        }
        "Notification" => {
            let ses_event = match SnsVerifier::parse_ses_event(&sns_message.message) {
                Ok(e) => e,
                Err(e) => {
                    warn!("Invalid SES event in SNS notification: {}", e);
                    return StatusCode::BAD_REQUEST.into_response();
                }
            };

            let provider_message_id = ses_event.mail.message_id.clone();
            let (event_type, metadata, recipients) = SnsVerifier::map_ses_event(&ses_event);

            // Record an event for each recipient
            let event_service = state.event_service.clone();
            let sns_msg_id = sns_message.message_id.clone();

            tokio::spawn(async move {
                for recipient in &recipients {
                    if let Err(e) = event_service
                        .record_event(
                            // Look up email by provider_message_id — for now, use a nil UUID
                            // since we need a DB lookup. The provider_message_id is stored
                            // for correlation.
                            Uuid::nil(),
                            &event_type,
                            Some(format!("{}:{}", sns_msg_id, recipient)),
                            Some(recipient.clone()),
                            metadata.clone(),
                            None,
                            None,
                        )
                        .await
                    {
                        warn!(
                            "Failed to record {} event for {}: {}",
                            event_type, provider_message_id, e
                        );
                    }
                }

                debug!(
                    "Processed SES {} event for message {}, {} recipients",
                    event_type,
                    provider_message_id,
                    recipients.len()
                );
            });

            StatusCode::OK.into_response()
        }
        "UnsubscribeConfirmation" => {
            warn!("Received SNS UnsubscribeConfirmation — ignored");
            StatusCode::OK.into_response()
        }
        other => {
            warn!("Unknown SNS message type: {}", other);
            StatusCode::OK.into_response()
        }
    }
}

// ============================================================
// AUTHENTICATED ENDPOINTS
// ============================================================

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EmailEventResponse {
    pub id: i64,
    pub email_id: String,
    pub event_type: String,
    pub provider_message_id: Option<String>,
    pub recipient: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub created_at: String,
}

impl From<temps_entities::email_events::Model> for EmailEventResponse {
    fn from(event: temps_entities::email_events::Model) -> Self {
        Self {
            id: event.id,
            email_id: event.email_id.to_string(),
            event_type: event.event_type,
            provider_message_id: event.provider_message_id,
            recipient: event.recipient,
            metadata: event.metadata,
            ip_address: event.ip_address,
            user_agent: event.user_agent,
            created_at: event.created_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EmailEventStatsResponse {
    pub delivered: u64,
    pub opened: u64,
    pub clicked: u64,
    pub bounced: u64,
    pub complained: u64,
    pub open_rate: Option<f64>,
    pub click_rate: Option<f64>,
    pub bounce_rate: Option<f64>,
}

impl From<EmailEventStats> for EmailEventStatsResponse {
    fn from(stats: EmailEventStats) -> Self {
        Self {
            delivered: stats.delivered,
            opened: stats.opened,
            clicked: stats.clicked,
            bounced: stats.bounced,
            complained: stats.complained,
            open_rate: stats.open_rate,
            click_rate: stats.click_rate,
            bounce_rate: stats.bounce_rate,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PaginatedEmailEventsResponse {
    pub events: Vec<EmailEventResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

#[derive(Debug, Deserialize)]
pub struct ListEmailEventsQuery {
    pub event_type: Option<String>,
    pub page: Option<u64>,
    pub page_size: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct ListAllEmailEventsQuery {
    pub email_id: Option<String>,
    pub event_type: Option<String>,
    pub page: Option<u64>,
    pub page_size: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct EmailEventStatsQuery {
    pub email_id: Option<String>,
}

/// GET /emails/events — list events across all emails with optional filters
#[utoipa::path(
    tag = "email-tracking",
    get,
    path = "/emails/events",
    params(
        ("email_id" = Option<String>, Query, description = "Filter by email ID"),
        ("event_type" = Option<String>, Query, description = "Filter by event type"),
        ("page" = Option<u64>, Query, description = "Page number (default: 1)"),
        ("page_size" = Option<u64>, Query, description = "Page size (default: 20, max: 100)"),
    ),
    responses(
        (status = 200, description = "Email events", body = PaginatedEmailEventsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
    ),
    security(("bearer_auth" = []))
)]
async fn list_all_email_events(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TrackingState>>,
    Query(query): Query<ListAllEmailEventsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailsRead);

    let email_id = query
        .email_id
        .as_deref()
        .map(Uuid::parse_str)
        .transpose()
        .map_err(|_| {
            temps_core::error_builder::bad_request()
                .title("Invalid Email ID")
                .detail("Invalid UUID in email_id query parameter")
                .build()
        })?;

    let page = query.page.unwrap_or(1);
    let page_size = std::cmp::min(query.page_size.unwrap_or(20), 100);

    let (events, total) = state
        .event_service
        .list_events(ListEmailEventsOptions {
            email_id,
            event_type: query.event_type,
            page: Some(page),
            page_size: Some(page_size),
        })
        .await
        .map_err(Problem::from)?;

    let response = PaginatedEmailEventsResponse {
        events: events.into_iter().map(EmailEventResponse::from).collect(),
        total,
        page,
        page_size,
    };

    Ok(Json(response))
}

/// GET /emails/{email_id}/events
#[utoipa::path(
    tag = "email-tracking",
    get,
    path = "/emails/{email_id}/events",
    params(
        ("email_id" = String, Path, description = "Email ID"),
        ("event_type" = Option<String>, Query, description = "Filter by event type"),
        ("page" = Option<u64>, Query, description = "Page number (default: 1)"),
        ("page_size" = Option<u64>, Query, description = "Page size (default: 20, max: 100)"),
    ),
    responses(
        (status = 200, description = "Email events", body = PaginatedEmailEventsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
    ),
    security(("bearer_auth" = []))
)]
async fn list_email_events(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TrackingState>>,
    Path(email_id_str): Path<String>,
    Query(query): Query<ListEmailEventsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailsRead);

    let email_id = Uuid::parse_str(&email_id_str).map_err(|_| {
        temps_core::error_builder::bad_request()
            .title("Invalid Email ID")
            .detail(format!("Invalid UUID: {}", email_id_str))
            .build()
    })?;

    let page = query.page.unwrap_or(1);
    let page_size = std::cmp::min(query.page_size.unwrap_or(20), 100);

    let (events, total) = state
        .event_service
        .list_events(ListEmailEventsOptions {
            email_id: Some(email_id),
            event_type: query.event_type,
            page: Some(page),
            page_size: Some(page_size),
        })
        .await
        .map_err(Problem::from)?;

    let response = PaginatedEmailEventsResponse {
        events: events.into_iter().map(EmailEventResponse::from).collect(),
        total,
        page,
        page_size,
    };

    Ok(Json(response))
}

/// GET /emails/events/stats
#[utoipa::path(
    tag = "email-tracking",
    get,
    path = "/emails/events/stats",
    params(
        ("email_id" = Option<String>, Query, description = "Filter by email ID"),
    ),
    responses(
        (status = 200, description = "Email event statistics", body = EmailEventStatsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
    ),
    security(("bearer_auth" = []))
)]
async fn get_email_event_stats(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TrackingState>>,
    Query(query): Query<EmailEventStatsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailsRead);

    let email_id = query
        .email_id
        .as_deref()
        .map(Uuid::parse_str)
        .transpose()
        .map_err(|_| {
            temps_core::error_builder::bad_request()
                .title("Invalid Email ID")
                .detail("Invalid UUID in email_id query parameter")
                .build()
        })?;

    let stats = state
        .event_service
        .get_stats(email_id)
        .await
        .map_err(Problem::from)?;

    Ok(Json(EmailEventStatsResponse::from(stats)))
}

// ============================================================
// ROUTE CONFIGURATION
// ============================================================

/// Public tracking routes (no auth required)
pub fn public_routes() -> Router<Arc<TrackingState>> {
    Router::new()
        .route("/t/pixel/{email_id}/{hmac}", get(pixel_handler))
        .route("/t/click/{email_id}/{hmac}/{url:.*}", get(click_handler))
        .route("/t/webhook/ses", post(ses_webhook_handler))
}

/// Authenticated API routes
pub fn api_routes() -> Router<Arc<TrackingState>> {
    Router::new()
        .route("/emails/events", get(list_all_email_events))
        .route("/emails/events/stats", get(get_email_event_stats))
        .route("/emails/{email_id}/events", get(list_email_events))
}

/// All routes merged
pub fn configure_routes() -> Router<Arc<TrackingState>> {
    Router::new().merge(public_routes()).merge(api_routes())
}
