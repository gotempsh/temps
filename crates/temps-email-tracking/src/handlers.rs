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

use crate::event_service::{
    EmailEventService, EmailEventStats, ListEmailEventsOptions, SnsProcessingOutcome,
};
use crate::hmac::verify_tracking_hmac;
use crate::sns::SnsVerifier;
use temps_email::{ProviderService, SuppressionService};

/// Shared state for tracking handlers
pub struct TrackingState {
    pub event_service: Arc<EmailEventService>,
    pub sns_verifier: Arc<SnsVerifier>,
    pub hmac_key: Vec<u8>,
    pub suppression_service: Arc<SuppressionService>,
    pub provider_service: Arc<ProviderService>,
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

    // Reject private/loopback/link-local/cloud-metadata targets — the URL
    // here comes from whatever HTML the sender submitted (rewritten +
    // HMAC-signed at send time, not filtered), so without this check a
    // sender could embed an internal URL and get our own domain to 302
    // recipients into it. Mirrors the same SSRF check temps-email's
    // should_track_link() applies at rewrite time instead of redirect time.
    if temps_core::url_validation::validate_external_url(&url).is_err() {
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

    let topic_arn = match state.sns_verifier.validate_topic(&sns_message) {
        Ok(topic) => topic,
        Err(error) => {
            warn!("SNS topic validation failed: {}", error);
            return StatusCode::FORBIDDEN.into_response();
        }
    };
    match state
        .provider_service
        .is_sns_topic_authorized(topic_arn)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            warn!("SNS topic is not configured on an active SES provider");
            return StatusCode::FORBIDDEN.into_response();
        }
        Err(error) => {
            error!("Failed to resolve SNS topic authorization: {}", error);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    // Verify signature
    if let Err(e) = state.sns_verifier.verify_signature(&sns_message).await {
        warn!("SNS signature verification failed: {}", e);
        return StatusCode::FORBIDDEN.into_response();
    }

    match sns_message.message_type.as_str() {
        "SubscriptionConfirmation" => {
            match state.sns_verifier.confirm_subscription(&sns_message).await {
                Ok(_) => {
                    // Surface pipeline health in the provider setup UI: a
                    // recorded confirmation is what distinguishes "working"
                    // from "subscribed before the topic was authorized and
                    // now stuck pending". Best-effort — the confirmation
                    // itself already succeeded against AWS.
                    if let Err(e) = state
                        .provider_service
                        .mark_sns_subscription_confirmed(topic_arn)
                        .await
                    {
                        error!("Failed to record SNS subscription confirmation: {}", e);
                    }
                    StatusCode::OK.into_response()
                }
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

            // Only a *permanent* bounce means the mailbox is really gone —
            // a transient/soft bounce (mailbox full, greylisting, etc.) is
            // expected to succeed on a later send and must not suppress it.
            let is_hard_bounce = ses_event
                .bounce
                .as_ref()
                .map(|b| b.bounce_type == "Permanent")
                .unwrap_or(false);
            let is_complaint = event_type == "complained";

            if recipients.is_empty() {
                warn!("SES {} event contained no recipients", event_type);
                return StatusCode::BAD_REQUEST.into_response();
            }

            let suppression_reason = if is_complaint {
                Some(temps_email::SuppressionReason::Complained)
            } else if is_hard_bounce {
                Some(temps_email::SuppressionReason::Bounced)
            } else {
                None
            };
            let processing_result = state
                .event_service
                .process_sns_event(
                    state.suppression_service.as_ref(),
                    topic_arn,
                    &sns_message.message_id,
                    &provider_message_id,
                    &event_type,
                    &recipients,
                    metadata,
                    suppression_reason,
                )
                .await;
            sns_processing_response(processing_result, &event_type, &provider_message_id)
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

fn sns_processing_response(
    result: Result<SnsProcessingOutcome, crate::errors::EmailTrackingError>,
    event_type: &str,
    provider_message_id: &str,
) -> Response {
    match result {
        Ok(SnsProcessingOutcome::Processed) => {
            debug!(
                "Durably processed SES {} event for message {}",
                event_type, provider_message_id
            );
            StatusCode::OK.into_response()
        }
        Ok(SnsProcessingOutcome::AlreadyProcessed) => StatusCode::OK.into_response(),
        Ok(SnsProcessingOutcome::Unmatched) => {
            warn!(
                "Retrying SES {} event for not-yet-correlated provider message {}",
                event_type, provider_message_id
            );
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
        Err(crate::errors::EmailTrackingError::RecipientMismatch { .. })
        | Err(crate::errors::EmailTrackingError::TopicMismatch { .. }) => {
            warn!("Ignoring terminal SNS correlation mismatch");
            StatusCode::OK.into_response()
        }
        Err(error) => {
            error!(
                "Failed to durably process SES {} event for {}: {}",
                event_type, provider_message_id, error
            );
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
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
        .route("/t/click/{email_id}/{hmac}/{*url}", get(click_handler))
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

#[cfg(test)]
mod route_tests {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    #[test]
    fn public_tracking_routes_use_valid_axum_patterns() {
        let _router = super::public_routes();
    }

    #[test]
    fn unmatched_sns_notification_returns_retryable_status() {
        let response = super::sns_processing_response(
            Ok(super::SnsProcessingOutcome::Unmatched),
            "bounced",
            "provider-message-id",
        )
        .into_response();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
